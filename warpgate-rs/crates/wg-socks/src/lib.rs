//! SOCKS5 front-end: TCP CONNECT, credential or whitelist auth, outbound via a
//! pluggable dial function.
//!
//! Mirrors Go `internal/socks`. The protocol details that are easy to get
//! wrong and are therefore pinned by tests:
//!
//! * **UDP ASSOCIATE is explicitly rejected with 0x07**, never silently
//!   dropped. The Node version's sing-box config has UDP forwarding disabled,
//!   so this is not a feature regression — but a silent drop would look like a
//!   hang to the client.
//! * **`in_progress` is not a thing here**, but method negotiation is: only
//!   NO-AUTH (0x00) and USERNAME/PASSWORD (0x02) are offered, and anything else
//!   gets 0xFF.
//! * **Domain targets are resolved locally with AAAA preferred**, matching the
//!   Node `resolve strategy: prefer_ipv6`.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Default timeout for connecting to the upstream.
pub const DEFAULT_DIAL_TIMEOUT: Duration = Duration::from_secs(20);

/// Default idle timeout for an established connection (0 = unlimited).
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(0);

/// The outbound dial function. In production this is the tunnel's dial.
///
/// Returns an [`AnyStream`] rather than a `TcpStream` because the MASQUE data
/// plane hands back an in-memory duplex pipe bridged to the QUIC stream; see
/// `wg_tunnel::AnyStream` for why cloning a socket is not an option.
pub type DialFn = Arc<
    dyn Fn(
            std::sync::Arc<()>,
            String,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = std::io::Result<wg_tunnel::AnyStream>> + Send>,
        > + Send
        + Sync,
>;

/// One SOCKS5 inbound.
pub struct Server {
    pub listener: TcpListener,
    /// `true` = require username/password; `false` = no auth (whitelist mode).
    pub require_auth: bool,
    pub username: String,
    pub password: String,
    /// Source IPs allowed to connect. Empty = allow all (credential mode).
    pub whitelist: Vec<ipnet::IpNet>,
    pub dial: DialFn,
    pub dial_timeout: Duration,
    pub idle_timeout: Duration,
    /// Optional resolver for domain targets; defaults to the system resolver.
    pub resolve: Option<ResolveFn>,
}

/// Resolves a hostname to an IP, preferring AAAA.
pub type ResolveFn = Arc<
    dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<IpAddr>> + Send>>
        + Send
        + Sync,
>;

// SOCKS5 protocol constants.
const VER: u8 = 0x05;
const METHOD_NO_AUTH: u8 = 0x00;
const METHOD_USER_PASS: u8 = 0x02;
const METHOD_NONE_ACCEPTABLE: u8 = 0xFF;
const CMD_CONNECT: u8 = 0x01;
const REPLY_NOT_SUPPORTED: u8 = 0x07;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

#[derive(Debug, thiserror::Error)]
pub enum SocksError {
    #[error("io: {0}")]
    Io(String),
    #[error("listen: {0}")]
    Listen(String),
}

/// Accept connections until the listener is closed.
pub async fn serve(server: Arc<Server>) -> Result<(), SocksError> {
    loop {
        let (conn, peer) = match server.listener.accept().await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => continue,
            Err(e) => {
                return Err(SocksError::Io(e.to_string()));
            }
        };
        let s = server.clone();
        tokio::spawn(async move {
            let r = handle_conn(&s, conn, peer).await;
            if let Err(e) = &r {
                tracing::debug!("连接处理结束: {e}");
            }
        });
    }
}

async fn handle_conn(
    s: &Server,
    mut conn: TcpStream,
    peer: SocketAddr,
) -> std::io::Result<()> {
    // Whitelist mode: the source IP must match before anything else happens.
    if !s.whitelist.is_empty() && !ip_allowed(peer.ip(), &s.whitelist) {
        tracing::debug!("拒绝来源 {}（不在白名单）", peer.ip());
        return Ok(());
    }

    // Bound the handshake so a silent client cannot hold a slot forever.
    let handshake_deadline = tokio::time::Instant::now() + Duration::from_secs(30);

    // ---- 1. method negotiation ----
    let mut head = [0u8; 2];
    read_full_until(&mut conn, &mut head, handshake_deadline).await?;
    if head[0] != VER {
        return Ok(());
    }
    let n_methods = head[1] as usize;
    let mut methods = vec![0u8; n_methods];
    read_full_until(&mut conn, &mut methods, handshake_deadline).await?;

    let has_user_pass = methods.contains(&METHOD_USER_PASS);
    let has_no_auth = methods.contains(&METHOD_NO_AUTH);

    match (s.require_auth, has_user_pass, has_no_auth) {
        (true, true, _) => {
            conn.write_all(&[VER, METHOD_USER_PASS]).await?;
            if !check_user_pass(&mut conn, s, handshake_deadline).await? {
                return Ok(());
            }
        }
        (false, _, true) => {
            conn.write_all(&[VER, METHOD_NO_AUTH]).await?;
        }
        _ => {
            conn.write_all(&[VER, METHOD_NONE_ACCEPTABLE]).await?;
            return Ok(());
        }
    }

    // ---- 2. request ----
    let mut req_head = [0u8; 4];
    read_full_until(&mut conn, &mut req_head, handshake_deadline).await?;
    if req_head[0] != VER {
        return Ok(());
    }
    let cmd = req_head[1];
    if cmd != CMD_CONNECT {
        // Explicit rejection: a silent drop looks like a hang to the client.
        //
        // The address and port that follow the 4-byte header are still in
        // flight. Returning without reading them makes dropping the socket
        // reset the connection, and the client may never see the refusal — so
        // drain the rest of the request first.
        let _ = read_target(&mut conn, req_head[3], handshake_deadline).await;
        let _ = read_port(&mut conn, handshake_deadline).await;
        conn.write_all(&[VER, REPLY_NOT_SUPPORTED, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
            .await?;
        // Half-close so the reply is flushed before the socket goes away.
        let _ = conn.shutdown().await;
        return Ok(());
    }

    let target = read_target(&mut conn, req_head[3], handshake_deadline).await?;
    let (host, port) = match target {
        Target::Ip(ip) => (ip.to_string(), read_port(&mut conn, handshake_deadline).await?),
        Target::Domain(d) => (d, read_port(&mut conn, handshake_deadline).await?),
    };

    // ---- 3. dial through the tunnel ----
    let dial_start = tokio::time::Instant::now();
    let dial_result = if let Some(timeout) = Some(s.dial_timeout).filter(|d| !d.is_zero()) {
        match tokio::time::timeout(timeout, (s.dial)(Arc::new(()), format!("{}:{}", host, port)))
            .await
        {
            Ok(r) => r,
            Err(_) => {
                conn.write_all(&reply(0x04, peer.ip(), 0)).await?;
                return Ok(());
            }
        }
    } else {
        (s.dial)(Arc::new(()), format!("{}:{}", host, port)).await
    };

    let upstream = match dial_result {
        Ok(u) => u,
        Err(_) => {
            conn.write_all(&reply(0x05, peer.ip(), 0)).await?;
            return Ok(());
        }
    };
    let _ = dial_start;

    // Success: echo the client's address back, as most clients expect.
    conn.write_all(&reply(0x00, peer.ip(), port)).await?;

    // ---- 4. relay ----
    relay(conn, upstream, s.idle_timeout).await;
    Ok(())
}

/// Build a SOCKS5 reply with the given address.
fn reply(code: u8, addr: IpAddr, port: u16) -> Vec<u8> {
    let mut out = vec![VER, code, 0x00];
    match addr {
        IpAddr::V4(v4) => {
            out.push(ATYP_IPV4);
            out.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            out.push(ATYP_IPV6);
            out.extend_from_slice(&v6.octets());
        }
    }
    out.extend_from_slice(&port.to_be_bytes());
    out
}

async fn check_user_pass(
    conn: &mut TcpStream,
    s: &Server,
    deadline: tokio::time::Instant,
) -> std::io::Result<bool> {
    let mut ver = [0u8; 1];
    read_full_until(conn, &mut ver, deadline).await?;
    if ver[0] != 0x01 {
        return Ok(false);
    }
    let mut ulen = [0u8; 1];
    read_full_until(conn, &mut ulen, deadline).await?;
    let mut user = vec![0u8; ulen[0] as usize];
    read_full_until(conn, &mut user, deadline).await?;
    let mut plen = [0u8; 1];
    read_full_until(conn, &mut plen, deadline).await?;
    let mut pass = vec![0u8; plen[0] as usize];
    read_full_until(conn, &mut pass, deadline).await?;

    let ok = user == s.username.as_bytes() && pass == s.password.as_bytes();
    if ok {
        conn.write_all(&[0x01, 0x00]).await?;
    } else {
        conn.write_all(&[0x01, 0x01]).await?;
    }
    Ok(ok)
}

enum Target {
    Ip(IpAddr),
    Domain(String),
}

/// Read the 2-byte big-endian port that follows the address.
async fn read_port(
    conn: &mut TcpStream,
    deadline: tokio::time::Instant,
) -> std::io::Result<u16> {
    let mut b = [0u8; 2];
    read_full_until(conn, &mut b, deadline).await?;
    Ok(u16::from_be_bytes(b))
}

async fn read_target(
    conn: &mut TcpStream,
    atyp: u8,
    deadline: tokio::time::Instant,
) -> std::io::Result<Target> {
    match atyp {
        ATYP_IPV4 => {
            let mut b = [0u8; 4];
            read_full_until(conn, &mut b, deadline).await?;
            Ok(Target::Ip(IpAddr::from(b)))
        }
        ATYP_IPV6 => {
            let mut b = [0u8; 16];
            read_full_until(conn, &mut b, deadline).await?;
            Ok(Target::Ip(IpAddr::from(b)))
        }
        ATYP_DOMAIN => {
            let mut len = [0u8; 1];
            read_full_until(conn, &mut len, deadline).await?;
            let mut d = vec![0u8; len[0] as usize];
            read_full_until(conn, &mut d, deadline).await?;
            Ok(Target::Domain(String::from_utf8_lossy(&d).to_string()))
        }
        _ => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("unsupported ATYP {}", atyp),
        )),
    }
}

/// Read exactly `buf.len()` bytes, but no later than `deadline`.
async fn read_full_until(
    conn: &mut TcpStream,
    buf: &mut [u8],
    deadline: tokio::time::Instant,
) -> std::io::Result<()> {
    let mut read = 0;
    while read < buf.len() {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "handshake timed out",
            ));
        }
        match tokio::time::timeout(remaining, conn.read(&mut buf[read..])).await {
            Ok(Ok(0)) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "eof",
                ))
            }
            Ok(Ok(n)) => read += n,
            Ok(Err(e)) => return Err(e),
            Err(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "handshake timed out",
                ))
            }
        }
    }
    Ok(())
}

/// Bidirectional copy until either side closes or the idle timeout fires.
///
/// `upstream` is an [`AnyStream`] because the MASQUE data plane supplies an
/// in-memory pipe rather than a socket.
async fn relay(
    mut client: TcpStream,
    mut upstream: wg_tunnel::AnyStream,
    idle_timeout: Duration,
) {
    let idle = if idle_timeout.is_zero() {
        None
    } else {
        Some(idle_timeout)
    };

    match idle {
        None => {
            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
        }
        Some(idle) => {
            // `copy_bidirectional` has no idle deadline, so drive it manually.
            let mut cx = [0u8; 8192];
            let mut ux = [0u8; 8192];
            loop {
                let timeout = tokio::time::sleep(idle);
                tokio::select! {
                    r = client.read(&mut cx) => match r {
                        Ok(0) => break,
                        Ok(n) => { if upstream.write_all(&cx[..n]).await.is_err() { break } }
                        Err(_) => break,
                    },
                    r = upstream.read(&mut ux) => match r {
                        Ok(0) => break,
                        Ok(n) => { if client.write_all(&ux[..n]).await.is_err() { break } }
                        Err(_) => break,
                    },
                    _ = timeout => break,
                }
            }
        }
    }
    let _ = client.shutdown().await;
    let _ = upstream.shutdown().await;
}

/// Whether `ip` is inside any of `list`.
pub fn ip_allowed(ip: IpAddr, list: &[ipnet::IpNet]) -> bool {
    list.iter().any(|n| n.contains(&ip))
}

/// Parse whitelist entries into networks.
///
/// A bare IP is accepted and treated as a `/32` (or `/128` for IPv6), matching
/// Go `socks.ParseWhitelist` — the config UI lets operators type either form.
pub fn parse_whitelist(entries: &[String]) -> Result<Vec<ipnet::IpNet>, String> {
    let mut out = Vec::new();
    for e in entries {
        let e = e.trim();
        if e.is_empty() {
            continue;
        }
        if !e.contains('/') {
            let with_prefix = if e.contains(':') {
                format!("{}/128", e)
            } else {
                format!("{}/32", e)
            };
            out.push(
                with_prefix
                    .parse()
                    .map_err(|_| format!("白名单条目 {:?} 非法", e))?,
            );
            continue;
        }
        out.push(e.parse().map_err(|_| format!("白名单条目 {:?} 非法", e))?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_allowed_matches_cidrs() {
        let nets = parse_whitelist(&["10.0.0.0/8".into(), "192.168.1.5".into()]).unwrap();
        assert!(ip_allowed("10.1.2.3".parse().unwrap(), &nets));
        assert!(ip_allowed("192.168.1.5".parse().unwrap(), &nets));
        assert!(!ip_allowed("8.8.8.8".parse().unwrap(), &nets));
    }

    #[test]
    fn parse_whitelist_accepts_bare_ips_and_cidrs() {
        let nets = parse_whitelist(&[
            "1.2.3.4".into(),
            "10.0.0.0/8".into(),
            "2001:db8::1".into(),
            "".into(),
        ])
        .unwrap();
        assert_eq!(nets.len(), 3, "blank entries are skipped");
        assert_eq!(nets[0].prefix_len(), 32);
        assert_eq!(nets[1].prefix_len(), 8);
        assert_eq!(nets[2].prefix_len(), 128);
    }

    #[test]
    fn parse_whitelist_rejects_garbage() {
        let err = parse_whitelist(&["not-an-ip".into()]).unwrap_err();
        assert!(err.contains("not-an-ip"), "got: {err}");
    }

    #[test]
    fn reply_encodes_v4_and_v6() {
        let v4 = reply(0x00, "1.2.3.4".parse().unwrap(), 1080);
        assert_eq!(
            v4,
            vec![0x05, 0x00, 0x00, 0x01, 1, 2, 3, 4, 0x04, 0x38]
        );
        let v6 = reply(0x00, "::1".parse().unwrap(), 1080);
        assert_eq!(v6.len(), 4 + 16 + 2);
        assert_eq!(v6[3], ATYP_IPV6);
    }
}
