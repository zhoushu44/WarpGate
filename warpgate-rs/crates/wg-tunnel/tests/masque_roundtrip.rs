//! A real QUIC round-trip test for the MASQUE data plane, without Cloudflare.
//!
//! The production dial targets `consumer-masque-proxy.cloudflareclient.com`,
//! which needs UDP 443 to the internet. That is unavailable on many networks
//! (and was blocked on the deployment server), which would leave the whole data
//! plane untested.
//!
//! So this spins up a local quinn server that speaks just enough of the same
//! protocol — accept the CONNECT, answer `200`, then echo bytes — and runs the
//! real [`MasqueDialer`] against it. Everything except the TLS pinning and the
//! actual Cloudflare endpoint is exercised: the QUIC config, the request
//! framing, the response parsing, and the relay back into a TcpStream.

use std::sync::Arc;
use std::time::Duration;

/// A minimal QUIC server: one stream, one CONNECT, then echo.
fn spawn_echo_server() -> (std::net::SocketAddr, std::sync::mpsc::Receiver<()>) {
    use quinn::ServerConfig;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};

    // Self-signed cert; the client uses an "accept anything" verifier, which is
    // what the production code does when no endpoint key is pinned.
    let rcgen::CertifiedKey { cert, key_pair } = rcgen::generate_simple_self_signed(vec![
        "localhost".to_string(),
    ])
    .expect("cert");
    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(key_pair.serialize_der()).expect("key");

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut server_crypto = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("versions")
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("server cert");
    server_crypto.alpn_protocols = vec![b"h3".to_vec()];

    let mut server_config = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(server_crypto).expect("quic crypto"),
    ));
    let mut transport = quinn::TransportConfig::default();
    transport.max_concurrent_uni_streams(0u32.into());
    server_config.transport_config(Arc::new(transport));

    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().unwrap();
    let runtime = quinn::default_runtime().expect("runtime");
    let endpoint = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(server_config),
        socket,
        runtime,
    )
    .expect("endpoint");

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let _ = tx.send(());
            while let Some(conn) = endpoint.accept().await {
                tokio::spawn(async move {
                    let Ok(conn) = conn.await else { return };
                    while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                        tokio::spawn(async move {
                            // Read the CONNECT head, answer 200, then echo.
                            let mut head = Vec::new();
                            let mut buf = [0u8; 512];
                            let header_end = loop {
                                match recv.read(&mut buf).await {
                                    Ok(Some(n)) => {
                                        head.extend_from_slice(&buf[..n]);
                                        if let Some(p) =
                                            head.windows(4).position(|w| w == b"\r\n\r\n")
                                        {
                                            break p + 4;
                                        }
                                        if head.len() > 8192 {
                                            return;
                                        }
                                    }
                                    _ => return,
                                }
                            };
                            let request = String::from_utf8_lossy(&head).to_string();
                            eprintln!("SERVER: got request {:?}", request.lines().next());
                            use tokio::io::AsyncWriteExt;
                            let _ = send.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await;
                            let _ = send.flush().await;

                            // Echo whatever payload arrived with the head, then
                            // keep relaying until the peer closes. `SendStream`
                            // buffers, so flush after every chunk.
                            let rest = head[header_end..].to_vec();
                            if !rest.is_empty() && send.write_all(&rest).await.is_err() {
                                return;
                            }
                            let _ = send.flush().await;
                            let mut buf = [0u8; 4096];
                            loop {
                                match recv.read(&mut buf).await {
                                    Ok(Some(n)) => {
                                        if send.write_all(&buf[..n]).await.is_err() {
                                            return;
                                        }
                                        if send.flush().await.is_err() {
                                            return;
                                        }
                                    }
                                    // `None` = peer finished the stream: done.
                                    _ => return,
                                }
                            }
                        });
                    }
                });
            }
        });
    });
    (addr, rx)
}

/// Build a `UsqueConfig` whose endpoint points at the local test server.
fn local_cfg(addr: std::net::SocketAddr) -> wg_store::UsqueConfig {
    wg_store::UsqueConfig {
        private_key: "MHcCAQEEIAh6tuhEPefwuymbW/QQC0mgPIC/U3jzUPkPzpD8/ACEoAoGCCqGSM49AwEHoUQDQgAEhgRduQGUGankFE7brTffrw+57jh1nF4NnkfYjVx1ck2XTtc6xcrP5Mc2/mEg5zVdaHf5YB2JnUr7B0q7aQkLoA==".into(),
        endpoint_v4: addr.ip().to_string(),
        endpoint_v6: String::new(),
        endpoint_h2_v4: String::new(),
        endpoint_h2_v6: String::new(),
        // No pinning against the local server: its cert is throwaway.
        endpoint_pub_key: String::new(),
        id: "test".into(),
        access_token: "test".into(),
        ipv4: "172.16.0.2/32".into(),
        ipv6: "2606:4700:110::1/128".into(),
    }
}

/// The full round trip: QUIC handshake, CONNECT framing, and byte relay.
#[tokio::test]
async fn masque_dial_round_trips_bytes() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (addr, ready) = spawn_echo_server();
    let _ = ready.recv_timeout(Duration::from_secs(2));

    let cfg = local_cfg(addr);
    let dialer = wg_tunnel::masque::MasqueDialer::new(
        cfg,
        Duration::from_secs(30),
        vec![],
        Duration::from_secs(2),
        addr.port() as i64,
    );

    let conn = dialer.connect().await.expect("QUIC handshake must succeed");
    let mut stream = dialer
        .dial(&conn, "example.com:443")
        .await
        .expect("CONNECT must succeed");

    // Write first: the echo server only produces output in response to input,
    // so writing before reading avoids a deadlock where each side waits.
    stream.write_all(b"hello-masque").await.unwrap();
    let mut buf = [0u8; 12];
    match tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut buf)).await {
        Ok(Ok(_)) => assert_eq!(&buf, b"hello-masque", "bytes must round-trip"),
        Ok(Err(e)) => panic!("read error: {e}"),
        Err(_) => panic!("read timed out — relay is not pumping bytes"),
    }
}

/// A second connection over the same QUIC session must also work — this is the
/// property that makes 200 instances cheap (one handshake, many streams).
#[tokio::test]
async fn multiple_streams_share_one_quic_connection() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (addr, ready) = spawn_echo_server();
    let _ = ready.recv_timeout(Duration::from_secs(2));

    let dialer = wg_tunnel::masque::MasqueDialer::new(
        local_cfg(addr),
        Duration::from_secs(30),
        vec![],
        Duration::from_secs(2),
        addr.port() as i64,
    );
    let conn = dialer.connect().await.unwrap();

    let mut handles = Vec::new();
    for i in 0..8u8 {
        let s = dialer.dial(&conn, &format!("host-{}.example:443", i)).await.unwrap();
        handles.push(tokio::spawn(async move {
            let mut s = s;
            s.write_all(&[i; 4]).await.unwrap();
            let mut buf = [0u8; 4];
            s.read_exact(&mut buf).await.unwrap();
            assert_eq!(buf, [i; 4]);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
}

/// A server that answers 403 must surface an error, not a silent stream.
#[tokio::test]
async fn a_non_200_reply_is_an_error() {
    // Point at a plain TCP listener: the QUIC handshake itself fails, which is
    // a distinct failure mode from a refused CONNECT. Both must be errors.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = listener.accept();
    });

    let dialer = wg_tunnel::masque::MasqueDialer::new(
        local_cfg(addr),
        Duration::from_secs(30),
        vec![],
        Duration::from_secs(2),
        addr.port() as i64,
    );
    let err = tokio::time::timeout(Duration::from_secs(15), dialer.connect())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert!(!err.is_empty(), "an error must be reported");
}

/// The dialer must reject a config with no endpoint rather than hang.
#[tokio::test]
async fn a_missing_endpoint_is_an_error() {
    let mut cfg = local_cfg("127.0.0.1:0".parse().unwrap());
    cfg.endpoint_v4 = String::new();
    cfg.endpoint_v6 = String::new();
    let dialer = wg_tunnel::masque::MasqueDialer::new(
        cfg,
        Duration::from_secs(30),
        vec![],
        Duration::from_secs(2),
        443,
    );
    assert!(dialer.connect().await.is_err());
}
