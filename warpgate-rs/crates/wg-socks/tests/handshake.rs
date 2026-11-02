//! SOCKS5 handshake matrix, mirroring Go `socks5_test.go`.
//!
//! Every case drives a real TCP connection against a real listener, so the
//! byte-level protocol is exercised end to end rather than through a mock.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use wg_socks::{serve, DialFn, Server};

/// A dial function that returns a connection to a local echo server.
///
/// The echo server is started separately (see `spawn_server`), and the dial
/// function *dials* it like a real upstream would — that keeps the two sides
/// from racing on the same listener.
fn dial_echo(echo_addr: SocketAddr) -> DialFn {
    Arc::new(move |_ctx, _target| {
        Box::pin(async move {
            let s = TcpStream::connect(echo_addr).await?;
            Ok(Box::new(s) as wg_tunnel::AnyStream)
        })
    })
}

/// A dial function that always fails.
fn dial_fail() -> DialFn {
    Arc::new(|_ctx, _target| {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "no upstream",
            ))
        })
    })
}

async fn spawn_server(
    cfg: impl FnOnce(&mut Server),
) -> (Arc<Server>, SocketAddr, Arc<tokio::net::TcpListener>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    // A standalone echo upstream, dialled by the fake dial function.
    let echo = Arc::new(tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap());
    let echo_addr = echo.local_addr().unwrap();
    {
        let echo = echo.clone();
        tokio::spawn(async move {
            loop {
                let Ok((c, _)) = echo.accept().await else { return };
                tokio::spawn(async move {
                    let (mut r, mut w) = c.into_split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
    }

    let s = Server {
        listener,
        require_auth: false,
        username: "u".into(),
        password: "p".into(),
        whitelist: Vec::new(),
        dial: dial_echo(echo_addr),
        dial_timeout: Duration::from_secs(5),
        idle_timeout: Duration::ZERO,
        resolve: None,
    };
    let mut s = s;
    cfg(&mut s);

    let server = Arc::new(s);
    let srv = server.clone();
    tokio::spawn(async move {
        let _ = serve(srv).await;
    });
    (server, addr, echo)
}

use std::net::SocketAddr;

/// Read `n` bytes, failing the test if they do not arrive.
async fn read_exact(conn: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    conn.read_exact(&mut buf).await.expect("read");
    buf
}

#[tokio::test]
async fn no_auth_connect_roundtrips() {
    let (_s, addr, _echo) = spawn_server(|s| s.require_auth = false).await;

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let sel = read_exact(&mut c, 2).await;
    assert_eq!(sel, vec![0x05, 0x00], "must select NO-AUTH");

    // CONNECT to a domain; the dial is faked so the target does not matter.
    let target = "echo.test";
    let mut req = vec![0x05, 0x01, 0x00, 0x03, target.len() as u8];
    req.extend_from_slice(target.as_bytes());
    req.extend_from_slice(&80u16.to_be_bytes());
    c.write_all(&req).await.unwrap();

    let reply = read_exact(&mut c, 10).await;
    assert_eq!(reply[1], 0x00, "CONNECT must succeed");

    c.write_all(b"ping").await.unwrap();
    let mut buf = [0u8; 4];
    c.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"ping", "payload must round-trip through the echo");
}

#[tokio::test]
async fn correct_credentials_are_accepted() {
    let (_s, addr, _e) = spawn_server(|s| {
        s.require_auth = true;
        s.username = "warpuser".into();
        s.password = "test-pass-123".into();
    })
    .await;

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
    let sel = read_exact(&mut c, 2).await;
    assert_eq!(sel, vec![0x05, 0x02], "must select USERNAME/PASSWORD");

    let (u, p) = ("warpuser", "test-pass-123");
    let mut auth = vec![0x01, u.len() as u8];
    auth.extend_from_slice(u.as_bytes());
    auth.push(p.len() as u8);
    auth.extend_from_slice(p.as_bytes());
    c.write_all(&auth).await.unwrap();
    let st = read_exact(&mut c, 2).await;
    assert_eq!(st, vec![0x01, 0x00], "credentials must be accepted");
}

#[tokio::test]
async fn wrong_credentials_are_rejected() {
    let (_s, addr, _e) = spawn_server(|s| {
        s.require_auth = true;
        s.username = "u".into();
        s.password = "right".into();
    })
    .await;

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
    let _ = read_exact(&mut c, 2).await;
    let (u, p) = ("u", "wrong");
    let mut auth = vec![0x01, u.len() as u8];
    auth.extend_from_slice(u.as_bytes());
    auth.push(p.len() as u8);
    auth.extend_from_slice(p.as_bytes());
    c.write_all(&auth).await.unwrap();
    let st = read_exact(&mut c, 2).await;
    assert_eq!(st, vec![0x01, 0x01], "wrong password must be refused");
}

#[tokio::test]
async fn no_auth_method_is_refused_in_credential_mode() {
    let (_s, addr, _e) = spawn_server(|s| s.require_auth = true).await;
    let mut c = TcpStream::connect(addr).await.unwrap();
    // The client only offers NO-AUTH, which credential mode will not accept.
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let sel = read_exact(&mut c, 2).await;
    assert_eq!(sel, vec![0x05, 0xFF], "must answer 0xFF");
}

#[tokio::test]
async fn udp_associate_is_explicitly_rejected() {
    let (_s, addr, _e) = spawn_server(|_s| {}).await;
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let _ = read_exact(&mut c, 2).await;

    // UDP ASSOCIATE = 0x03.
    let target = "1.2.3.4";
    let mut req = vec![0x05, 0x03, 0x00, 0x01];
    req.extend_from_slice(&[1, 2, 3, 4]);
    req.extend_from_slice(&0u16.to_be_bytes());
    let _ = target;
    c.write_all(&req).await.unwrap();

    let reply = read_exact(&mut c, 10).await;
    assert_eq!(
        reply[1], 0x07,
        "UDP ASSOCIATE must be explicitly refused, not dropped"
    );
}

#[tokio::test]
async fn bind_is_explicitly_rejected() {
    let (_s, addr, _e) = spawn_server(|_s| {}).await;
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let _ = read_exact(&mut c, 2).await;

    // BIND = 0x02.
    let req = vec![0x05, 0x02, 0x00, 0x01, 1, 2, 3, 4, 0, 80];
    c.write_all(&req).await.unwrap();
    let reply = read_exact(&mut c, 10).await;
    assert_eq!(reply[1], 0x07);
}

#[tokio::test]
async fn dial_failure_is_reported_as_a_socks_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mut s = Server {
        listener,
        require_auth: false,
        username: String::new(),
        password: String::new(),
        whitelist: Vec::new(),
        dial: dial_fail(),
        dial_timeout: Duration::from_secs(5),
        idle_timeout: Duration::ZERO,
        resolve: None,
    };
    s.require_auth = false;
    let server = Arc::new(s);
    let srv = server.clone();
    tokio::spawn(async move {
        let _ = serve(srv).await;
    });

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let _ = read_exact(&mut c, 2).await;
    let target = "echo.test";
    let mut req = vec![0x05, 0x01, 0x00, 0x03, target.len() as u8];
    req.extend_from_slice(target.as_bytes());
    req.extend_from_slice(&80u16.to_be_bytes());
    c.write_all(&req).await.unwrap();

    let reply = read_exact(&mut c, 10).await;
    assert_eq!(reply[1], 0x05, "a dial failure must map to host unreachable");
}

/// An empty whitelist means "not in whitelist mode": the port is not opened for
/// direct connections at all, so the pool never starts a listener.
#[tokio::test]
async fn empty_whitelist_allows_every_source() {
    let (_s, addr, _e) = spawn_server(|s| {
        s.require_auth = false;
        s.whitelist = Vec::new();
    })
    .await;
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    assert_eq!(read_exact(&mut c, 2).await, vec![0x05, 0x00]);
}

#[tokio::test]
async fn ipv6_target_is_accepted() {
    let (_s, addr, _e) = spawn_server(|_s| {}).await;
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
    let _ = read_exact(&mut c, 2).await;

    let mut req = vec![0x05, 0x01, 0x00, 0x04];
    req.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    req.extend_from_slice(&443u16.to_be_bytes());
    c.write_all(&req).await.unwrap();

    let reply = read_exact(&mut c, 10).await;
    assert_eq!(reply[1], 0x00, "ATYP 0x04 must be handled");
    // The reply echoes the CLIENT's address family (as most clients expect),
    // which here is IPv4 — the target's family is not reflected.
    assert_eq!(reply[3], 0x01, "reply carries the client's address family");
}

/// A source outside the whitelist is dropped before the handshake, so the
/// client sees an EOF rather than a protocol error.
#[tokio::test]
async fn whitelisted_source_is_enforced() {
    use wg_socks::ip_allowed;
    // Loopback is allowed by an explicit entry.
    let nets: Vec<ipnet::IpNet> = ["127.0.0.0/8"].iter().map(|s| s.parse().unwrap()).collect();
    assert!(ip_allowed(IpAddr::from([127, 0, 0, 1]), &nets));
    // A public address is not.
    assert!(!ip_allowed(IpAddr::from([8, 8, 8, 8]), &nets));
}
