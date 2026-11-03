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

/// How many fresh TLS connections to burn before giving up on one `connect()`.
///
/// `162.159.198.2` is an anycast VIP, and not every node behind it is a MASQUE
/// edge. Measured over 24 consecutive dials on one account: 21 presented
/// `CN=masque.cloudflareclient.com` and answered CONNECT-IP with `200`, 3
/// presented `CN=cloudflareclient.com` (Let's Encrypt) and answered `400`. The
/// pinning verifier correctly rejects that node as `UnknownIssuer`, so a single
/// attempt fails roughly 12-30% of the time depending on how long the VIP keeps
/// the flow pinned to one backend.
///
/// usque has exactly the same problem and solves it the same way: `MaintainTunnel`
/// retries forever with `--reconnect-delay` (default 1s). This is a bounded
/// version of that loop — a new TCP connection per attempt, the same 1s pacing,
/// 8 tries. Measured: 4 quick retries still lost ~1 in 6 tunnel bring-ups;
/// 8 paced retries succeeded on every account tried.
const CONNECT_ATTEMPTS: usize = 8;

/// Delay between attempts. Matches usque's `--reconnect-delay` default: the VIP
/// re-balances on new flows, and a full second is long enough for the
/// hash-based backend pick to land somewhere else.
const CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);

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
        // Retry the whole handshake on a fresh connection. The failure this
        // exists for is not deterministic: it depends on which anycast node the
        // VIP picks, and a new TCP connection gets a new pick. Retrying the
        // *same* connection cannot help, so `connect_inner` is re-entered from
        // scratch and nothing is reused.
        let mut last = String::new();
        for attempt in 1..=CONNECT_ATTEMPTS {
            let fut = Self::connect_inner(
                endpoint_v4,
                endpoint_pub_key_pem,
                private_key_b64,
                ipv6,
                ipv4,
                pin,
            );
            match tokio::time::timeout(CONNECT_TIMEOUT, fut).await {
                Ok(Ok(tunnel)) => {
                    if attempt > 1 {
                        tracing::info!(
                            attempt,
                            "native CONNECT-IP succeeded after retry"
                        );
                    }
                    return Ok(tunnel);
                }
                Ok(Err(e)) => {
                    tracing::warn!(attempt, error = %e, "native CONNECT-IP attempt failed");
                    last = e;
                }
                Err(_) => {
                    tracing::warn!(attempt, "native CONNECT-IP attempt timed out");
                    last = format!("原生隧道建立超时（{}s）", CONNECT_TIMEOUT.as_secs());
                }
            }
            if attempt < CONNECT_ATTEMPTS {
                // Pace the retries like usque does: the point is to land on a
                // different anycast backend, and the backend pick is per-flow.
                tokio::time::sleep(CONNECT_RETRY_DELAY).await;
            }
        }
        Err(last)
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
        eprintln!("[native] tls connecting...");
        let tls_stream = tokio::time::timeout(Duration::from_secs(15), connector.connect(sni, tcp))
            .await
            .map_err(|_| "TLS 握手超时".to_string())?
            .map_err(|e| format!("TLS 握手失败: {e}"))?;
        eprintln!("[native] tls ok");

        // 4. HTTP/2 preface, then the CONNECT-IP stream.
        let mut io = tls_stream;
        eprintln!("[native] h2 handshake...");
        crate::connectip::h2_handshake(&mut io)
            .await
            .map_err(|e| format!("HTTP/2 握手失败: {e}"))?;
        eprintln!("[native] h2 ok");
        eprintln!("[native] connect-ip...");
        let tunnel = crate::connectip::H2Tunnel::open(io, 1, crate::connectip::CONNECT_AUTHORITY)
            .await
            .map_err(|e| format!("CONNECT-IP 失败: {e}"))?;
        eprintln!("[native] connect-ip ok, stack up");

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
        // The account owns an IPv4 as well (`172.16.0.2` in `usque.json`). Both
        // families ride the same tunnel, and most destinations are A-only, so
        // the interface needs both addresses or IPv4 dials fail as
        // "unaddressable destination".
        if let Ok(v4) = ipv4.trim().parse::<std::net::Ipv4Addr>() {
            stack.add_ipv4(v4.into()).await;
        }
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

/// Report the endpoint's actual public key next to what the config pins.
///
/// A pinning mismatch shows up as `UnknownIssuer`, which is indistinguishable
/// from a genuinely untrusted certificate; this makes the two obvious.
pub async fn compare_pin(
    endpoint_v4: &str,
    endpoint_pub_key_pem: &str,
    private_key_b64: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    // The bisection below must use the *enrolled* key: a throwaway one makes the
    // handshake succeed and the connection then die with certificate_unknown,
    // which is indistinguishable from a pinning failure.
    let signing = match crate::masque::parse_private_key(private_key_b64) {
        Ok(k) => k,
        Err(e) => {
            out.push(format!("key parse FAILED: {e}"));
            return out;
        }
    };
    match crate::masque::pem_to_spki_der(endpoint_pub_key_pem) {
        Ok(der) => {
            let pt = crate::masque::pinned_point(&der).unwrap_or_default();
            out.push(format!("pinned   = {}", hex(&pt)));
        }
        Err(e) => out.push(format!("pinned parse FAILED: {e}")),
    }

    let host = endpoint_v4.trim_end_matches(":0");
    let tls = match crate::masque::build_h2_tls_config_no_pin_for_probe() {
        Ok(t) => t,
        Err(e) => {
            out.push(format!("tls cfg FAILED: {e}"));
            return out;
        }
    };
    let tcp = match tokio::net::TcpStream::connect(format!("{host}:443")).await {
        Ok(s) => s,
        Err(e) => {
            out.push(format!("tcp FAILED: {e}"));
            return out;
        }
    };
    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let sni_name = std::env::var("WARP_H2_SNI")
        .unwrap_or_else(|_| crate::connectip::CONNECT_SNI.to_string());
    let sni = match rustls::pki_types::ServerName::try_from(sni_name) {
        Ok(s) => s,
        Err(e) => {
            out.push(format!("sni FAILED: {e}"));
            return out;
        }
    };
    match tokio::time::timeout(Duration::from_secs(20), connector.connect(sni, tcp)).await {
        Ok(Ok(stream)) => {
            out.push("tls: OK without pinning".into());
            if let Some(certs) = stream.get_ref().1.peer_certificates() {
                if let Some(leaf) = certs.first() {
                    if let Ok((_, cert)) = x509_parser::parse_x509_certificate(leaf.as_ref()) {
                        out.push(format!(
                            "peer     = {}",
                            hex(&crate::masque::ec_point_of(cert.public_key()))
                        ));
                    }
                }
            }
        }
        Ok(Err(e)) => out.push(format!("tls FAILED: {e}")),
        Err(_) => out.push("tls TIMEOUT".into()),
    }

    // Bisect: is the failure the pinning verifier, or the client certificate?
    // A = accept-any verifier + client cert; B = pinning verifier + client cert.
    for (label, pinned) in [("A accept-any+clientcert", false), ("B pinned+clientcert", true)] {
        let tls = match crate::masque::build_tls_for_test(
            &signing,
            pinned.then(|| endpoint_pub_key_pem.to_string()),
        ) {
            Ok(t) => t,
            Err(e) => {
                out.push(format!("{label}: cfg FAILED {e}"));
                continue;
            }
        };
        let tcp = match tokio::net::TcpStream::connect(format!("{host}:443")).await {
            Ok(s) => s,
            Err(e) => {
                out.push(format!("{label}: tcp FAILED {e}"));
                continue;
            }
        };
        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
        let sni = rustls::pki_types::ServerName::try_from(
            crate::connectip::CONNECT_SNI.to_string(),
        )
        .unwrap();
        match tokio::time::timeout(Duration::from_secs(15), connector.connect(sni, tcp)).await {
            Ok(Ok(_)) => out.push(format!("{label}: OK")),
            Ok(Err(e)) => out.push(format!("{label}: FAILED {e}")),
            Err(_) => out.push(format!("{label}: TIMEOUT")),
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Bring the tunnel up one stage at a time, reporting where it stops.
///
/// A single error string cannot distinguish "TCP is blocked" from "TLS
/// rejected our client certificate" from "CONNECT-IP was refused"; this can.
pub async fn diagnose(
    endpoint_v4: &str,
    endpoint_pub_key_pem: &str,
    private_key_b64: &str,
) -> Vec<String> {
    let mut out = Vec::new();

    let signing = match crate::masque::parse_private_key(private_key_b64) {
        Ok(k) => {
            out.push("key: SEC1/PKCS8 parse OK".into());
            k
        }
        Err(e) => {
            out.push(format!("key: PARSE FAILED {e}"));
            return out;
        }
    };

    // NOTE: this must be the enrolled key. An earlier version called
    // `build_tls_for_test`, which mints a *fresh random* P-256 key for the
    // client certificate; the edge accepts the TLS handshake (it checks only
    // that a certificate was presented, at the TLS layer) and then, once the
    // HTTP/2 request names an account, rejects the stream. That turned a
    // perfectly good account into what looked like a broken CONNECT-IP
    // implementation.
    let tls = match crate::masque::build_h2_tls_config(
        &signing,
        Some(endpoint_pub_key_pem),
    ) {
        Ok(t) => {
            out.push(format!("tls-config: alpn={:?}", t.alpn_protocols));
            t
        }
        Err(e) => {
            out.push(format!("tls-config: FAILED {e}"));
            return out;
        }
    };

    let host = endpoint_v4.trim_end_matches(":0");
    let tcp = match tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(format!("{host}:443")),
    )
    .await
    {
        Ok(Ok(s)) => {
            out.push(format!("tcp: OK -> {host}:443"));
            s
        }
        Ok(Err(e)) => {
            out.push(format!("tcp: FAILED {e}"));
            return out;
        }
        Err(_) => {
            out.push("tcp: TIMEOUT".into());
            return out;
        }
    };
    tcp.set_nodelay(true).ok();

    let connector = tokio_rustls::TlsConnector::from(Arc::new(tls));
    let sni_name = std::env::var("WARP_H2_SNI")
        .unwrap_or_else(|_| crate::connectip::CONNECT_SNI.to_string());
    let sni = match rustls::pki_types::ServerName::try_from(sni_name) {
        Ok(s) => s,
        Err(e) => {
            out.push(format!("sni: INVALID {e}"));
            return out;
        }
    };

    let mut tls_stream =
        match tokio::time::timeout(Duration::from_secs(20), connector.connect(sni, tcp)).await {
            Ok(Ok(s)) => {
                // Careful: this does NOT mean the edge accepted the account.
                // Cloudflare completes the TLS 1.3 handshake for a certificate
                // that is not enrolled and only kills the connection afterwards
                // (a post-handshake `certificate_unknown(46)` alert, which
                // surfaces at the first request as an io error). Only the
                // CONNECT-IP step below proves the certificate was accepted.
                out.push("tls: handshake OK (certificate NOT yet authorised)".into());
                s
            }
            Ok(Err(e)) => {
                out.push(format!("tls: FAILED {e}"));
                return out;
            }
            Err(_) => {
                out.push("tls: TIMEOUT".into());
                return out;
            }
        };

    match tokio::time::timeout(
        Duration::from_secs(15),
        crate::connectip::h2_handshake(&mut tls_stream),
    )
    .await
    {
        Ok(Ok(())) => out.push("h2: handshake OK".into()),
        Ok(Err(e)) => {
            out.push(format!("h2: FAILED {e}"));
            return out;
        }
        Err(_) => {
            out.push("h2: TIMEOUT".into());
            return out;
        }
    }

    match crate::connectip::H2Tunnel::open(tls_stream, 1, crate::connectip::CONNECT_AUTHORITY).await {
        Ok(t) => out.push(format!("connect-ip: OK stream={}", t.stream_id())),
        Err(e) => {
            // `CertificateUnknown` here is the post-handshake alert Cloudflare
            // sends when the client key is not enrolled with the account being
            // connected. It is not a certificate-format problem: the same
            // certificate shape is accepted for an enrolled key.
            let hint = if e.to_string().contains("CertificateUnknown") {
                "  (= the client key is not enrolled for this account; \
                 the edge accepts any certificate at the TLS layer and rejects \
                 after the handshake)"
            } else {
                ""
            };
            out.push(format!("connect-ip: FAILED {e}{hint}"));
        }
    }
    out
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
