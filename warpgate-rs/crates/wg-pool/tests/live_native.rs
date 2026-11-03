//! Live check: does the native HTTP/2 CONNECT-IP path actually carry traffic?
//!
//! Ignored by default — needs egress to Cloudflare and creates a real account.

use std::time::Duration;

#[tokio::test]
#[ignore]
async fn native_h2_tunnel_carries_traffic() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = wg_config::Config::default();
    cfg.resident_count = 1;
    cfg.base_port = 48010;
    cfg.whitelist_port_offset = 1000;
    cfg.ip_whitelist = Vec::new();
    cfg.rate_limit_enabled = false;

    let mgr = std::sync::Arc::new(wg_pool::Manager::new(
        std::sync::Arc::new(cfg),
        tmp.path().to_path_buf(),
    ));

    let ucfg = match tokio::time::timeout(Duration::from_secs(60), mgr.register_for_test(0)).await {
        Ok(Ok(u)) => u,
        Ok(Err(e)) => panic!("registration failed: {e}"),
        Err(_) => panic!("registration timed out"),
    };
    println!("registered: id={} ipv6={}", ucfg.id, ucfg.ipv6);

    // The native path: TLS + HTTP/2 + CONNECT-IP + smoltcp.
    let native = match tokio::time::timeout(
        Duration::from_secs(60),
        wg_tunnel::connectip::NativeTunnel::connect(
            &ucfg.endpoint_v4,
            &ucfg.endpoint_pub_key,
            &ucfg.private_key,
            &ucfg.ipv6,
            &ucfg.ipv4,
            true,
        ),
    )
    .await
    {
        Ok(Ok(n)) => std::sync::Arc::new(n),
        Ok(Err(e)) => panic!("native tunnel failed: {e}"),
        Err(_) => panic!("native tunnel timed out"),
    };
    println!("tunnel up");

    // Drive a real HTTP request through it.
    let dial = native.dial_fn();
    let stream = tokio::time::timeout(Duration::from_secs(30), dial("api.ipify.org:80".into()))
        .await
        .expect("dial must not hang")
        .expect("dial must succeed");
    println!("tcp established through stack");

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut s = stream;
    s.write_all(b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n")
        .await
        .expect("write");
    let mut buf = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut buf))
        .await
        .expect("read must not hang")
        .expect("read");
    let text = String::from_utf8_lossy(&buf);
    println!("response ({} bytes): {}", buf.len(), &text[..text.len().min(300)]);
    assert!(!buf.is_empty(), "must get a response");
    assert!(
        text.contains("HTTP/1."),
        "must look like HTTP, got: {text:.200}"
    );
}
