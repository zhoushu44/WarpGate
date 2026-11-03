//! The native data plane: CONNECT-IP over HTTP/2 plus a userspace TCP stack.
//!
//! This is the replacement for the `usque socks --http2` subprocess. Given an
//! enrolled account it produces a `DialFn` that the SOCKS5 layer can use
//! exactly like the engine-backed one, so nothing above has to know which
//! transport is in play.
//!
//! ```text
//! connect()
//!   ├── TCP 443 + TLS (client cert from the enrolled P-256 key)
//!   ├── HTTP/2 preface + SETTINGS
//!   ├── CONNECT-IP on stream 1   → raw IP packets
//!   └── UserspaceStack (smoltcp) → DialFn
//! ```
//!
//! Why HTTP/2 and not QUIC: `usque --http2` is what the Node version actually
//! runs, because UDP 443 to Cloudflare is blocked on the deployment host.
//! `ss` on a live pool shows ~400 TCP connections to `:443` and zero UDP.

use std::sync::Arc;
use std::time::Duration;

use smoltcp::wire::IpAddress;

use crate::connectip::stack::{StackPump, UserspaceStack};

/// Budget for bringing the tunnel up. Longer than a SOCKS dial, shorter than a
/// user's patience.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// A live native tunnel.
///
/// Holds the stack and the pump together so dropping this tears both down;
/// leaking either would leave the QUIC/HTTP2 stream and the smoltcp sockets
/// alive after an instance is stopped.
pub struct NativeTunnel {
    stack: Arc<UserspaceStack>,
    _pump: StackPump,
}

impl NativeTunnel {
    /// Bring up a tunnel for an enrolled account.
    ///
    /// `ipv6` is the address Cloudflare assigned the account — the tunnel only
    /// routes for that identity, so it is what the stack binds. `ipv4` is only a
    /// fallback for accounts that got no v6 (rare; the v6 is the whole point).
    pub async fn connect(
        endpoint_v4: &str,
        endpoint_pub_key_pem: &str,
        private_key_b64: &str,
        ipv6: &str,
        ipv4: &str,
        pin: bool,
    ) -> Result<Self, String> {
        let fut = Self::connect_inner(
            endpoint_v4,
            endpoint_pub_key_pem,
            private_key_b64,
            ipv6,
            ipv4,
            pin,
        );
        tokio::time::timeout(CONNECT_TIMEOUT, fut)
            .await
            .map_err(|_| format!("原生隧道建立超时（{}s）", CONNECT_TIMEOUT.as_secs()))?
    }

    async fn connect_inner(
        endpoint_v4: &str,
        endpoint_pub_key_pem: &str,
        private_key_b64: &str,
        ipv6: &str,
        ipv4: &str,
        pin: bool,
    ) -> Result<Self, String> {
        // 1. Client identity: the P-256 key enrolled with Cloudflare.
        let signing = crate::masque::parse_private_key(private_key_b64)?;

        // 2. TLS with ALPN h2, pinning the endpoint's key.
        let tls = crate::masque::build_h2_tls_config(
            &signing,
            if pin { Some(endpoint_pub_key_pem) } else { None },
        )?;

        // 3. TCP + TLS. SNI is the CONNECT-IP one, not the endpoint hostname.
        let host = endpoint_v4.trim_end_matches(":0");
        let tcp = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::net::TcpStream::connect(format!("{}:443", host)),
        )
        .await
        .map_err(|_| "TCP 连接超时".to_string())?
        .map_err(|e| format!("TCP 连接失败: {e}"))?;
        tcp.set_nodelay(true).ok();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
        let sni = rustls::pki_types::ServerName::try_from(crate::connectip::CONNECT_SNI.to_string())
            .map_err(|e| format!("SNI 无效: {e}"))?;
        let tls_stream = tokio::time::timeout(Duration::from_secs(15), connector.connect(sni, tcp))
            .await
            .map_err(|_| "TLS 握手超时".to_string())?
            .map_err(|e| format!("TLS 握手失败: {e}"))?;

        // 4. HTTP/2 preface, then the CONNECT-IP stream.
        let mut io = tls_stream;
        crate::connectip::h2_handshake(&mut io)
            .await
            .map_err(|e| format!("HTTP/2 握手失败: {e}"))?;
        let tunnel = crate::connectip::H2Tunnel::open(io, 1, "cloudflareaccess.com")
            .await
            .map_err(|e| format!("CONNECT-IP 失败: {e}"))?;

        // 5. The userspace stack, bound to the address the account owns.
        let local = if !ipv6.trim().is_empty() {
            ipv6.trim()
                .split('/')
                .next()
                .unwrap_or(ipv6)
                .parse::<std::net::Ipv6Addr>()
                .map(|a| IpAddress::Ipv6(a.into()))
                .map_err(|e| format!("IPv6 地址非法 {ipv6}: {e}"))?
        } else {
            ipv4.trim()
                .parse::<std::net::Ipv4Addr>()
                .map(|a| IpAddress::Ipv4(a.into()))
                .map_err(|e| format!("IPv4 地址非法 {ipv4}: {e}"))?
        };
        let stack = Arc::new(UserspaceStack::new(local));
        let pump = stack.attach(tunnel);

        Ok(Self {
            stack,
            _pump: pump,
        })
    }

    /// The dial function for the SOCKS5 layer.
    pub fn dial_fn(self: &Arc<Self>) -> crate::DialFn {
        let stack = self.stack.clone();
        Arc::new(move |target: String| {
            let stack = stack.clone();
            Box::pin(async move {
                let (host, port) = split_target(&target)?;
                stack.connect(host, port).await
            })
        })
    }
}

/// `host:port` → `(host, port)`, tolerating `[v6]:port`.
fn split_target(target: &str) -> std::io::Result<(&str, u16)> {
    if let Some(rest) = target.strip_prefix('[') {
        let (host, tail) = rest
            .split_once(']')
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad v6"))?;
        let port = tail
            .strip_prefix(':')
            .and_then(|p| p.parse().ok())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad port"))?;
        return Ok((host, port));
    }
    let (host, port) = target
        .rsplit_once(':')
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad target"))?;
    let port = port
        .parse()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad port"))?;
    Ok((host, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_host_port() {
        assert_eq!(split_target("example.com:443").unwrap(), ("example.com", 443));
        assert_eq!(split_target("1.2.3.4:80").unwrap(), ("1.2.3.4", 80));
    }

    #[test]
    fn splits_bracketed_v6() {
        assert_eq!(
            split_target("[2606:4700::1]:443").unwrap(),
            ("2606:4700::1", 443)
        );
    }

    #[test]
    fn rejects_garbage() {
        // A target must have a port; the host is validated later, by `resolve`.
        assert!(split_target("nope").is_err());
        assert!(split_target("host:abc").is_err());
        // Bracketed hosts parse structurally even if the host is nonsense —
        // rejecting that is `resolve`'s job, not the splitter's.
        assert_eq!(split_target("[bad]:1").unwrap(), ("bad", 1));
    }
}
