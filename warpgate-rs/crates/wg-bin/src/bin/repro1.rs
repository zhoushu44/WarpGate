//! Minimal one-instance reproduction of "CONNECT-IP tunnel up, SOCKS returns 0 bytes".
//!
//! Brings up exactly one instance through the *production* path
//! (`Manager::create` → `attach_masque` → `attach_dial` → SOCKS listener) and
//! then exercises the same tunnel three ways:
//!
//! ```text
//!   A. dial + write_all + read_to_end      (what nativecheck does)
//!   B. dial + read *first*                 (what copy_bidirectional does)
//!   C. full SOCKS5 CONNECT + GET           (what a real client does)
//! ```
//!
//! If A works and B/C do not, the bug is not in the tunnel/HTTP2 layer: it is
//! in how `StackStream` reports readiness right after `connect()` returns.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wg_config::Config;
use wg_pool::Manager;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let cfg_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/root/wg-repro/config.json".into());
    let base = std::env::args()
        .nth(2)
        .unwrap_or_else(|| "/root/wg-repro/instances".into());
    let idx: i64 = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "1".into())
        .parse()
        .expect("index");

    let cfg = Arc::new(Config::load(&cfg_path).expect("config"));
    let base_path = std::path::PathBuf::from(&base);
    let mgr = Manager::new(cfg.clone(), base_path.clone());

    // Fresh account on first run; warm-start afterwards.
    let dir = wg_store::dir(&base_path, idx);
    let mut have = wg_store::UsqueConfig::read(&dir).ok().flatten();
    if have.is_none() {
        println!("[repro] registering a fresh account for warp-{idx} ...");
        match mgr.create(idx).await {
            Ok(()) => println!("[repro] created"),
            Err(e) => {
                println!("[repro] create failed: {e}");
                std::process::exit(1);
            }
        }
        have = wg_store::UsqueConfig::read(&dir).ok().flatten();
    }

    // `register_account` stores the *WARP* endpoint (`162.159.192.7`), which
    // answers :443 with a certificate this client does not pin. The MASQUE
    // edge is a different VIP; allow overriding it so the repro tests the data
    // plane rather than account provisioning.
    if let Ok(ep) = std::env::var("REPRO_H2_ENDPOINT") {
        if let Some(mut u) = have.clone() {
            u.endpoint_v4 = ep.clone();
            u.endpoint_h2_v4 = ep;
            let _ = u.save(&wg_store::paths::usque_conf_path(&dir));
            println!("[repro] endpoint overridden to {}", u.endpoint_v4);
            have = Some(u);
        }
    }

    if let Some(ucfg) = have {
        println!("[repro] starting warp-{idx} from its usque.json");
        if let Err(e) = mgr.start_with_config(idx, ucfg, false).await {
            println!("[repro] start failed: {e}");
            std::process::exit(1);
        }
    }

    println!("[repro] attach_masque ...");
    let t0 = std::time::Instant::now();
    let listeners = match mgr.attach_masque(idx).await {
        Ok(n) => n,
        Err(e) => {
            println!("[repro] attach_masque failed: {e}");
            std::process::exit(1);
        }
    };
    println!("[repro] attach_masque ok: {listeners} listener(s), {:?}", t0.elapsed());

    let inst = mgr.get(idx).await.expect("instance");
    let port = inst.port as u16;
    println!("[repro] socks port = {port}");

    // ---- A: nativecheck-style (write first, then read) ----
    let dial = inst.dial_now().expect("dial");
    match tokio::time::timeout(Duration::from_secs(25), dial("example.com:80".into())).await {
        Ok(Ok(mut s)) => {
            let w = s
                .write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\nConnection: Close\r\n\r\n")
                .await;
            println!("[A write-first] write = {:?}", w.map(|_| "ok"));
            let mut buf = Vec::new();
            let r = tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut buf)).await;
            println!(
                "[A write-first] read = {:?}, {} bytes, first line = {:?}",
                r.map(|x| x.map(|_| ())),
                buf.len(),
                buf.split(|b| *b == b'\n').next().map(|l| String::from_utf8_lossy(l).to_string())
            );
        }
        Ok(Err(e)) => println!("[A write-first] dial failed: {e}"),
        Err(_) => println!("[A write-first] dial timeout"),
    }

    // ---- B: read-first, exactly what copy_bidirectional polls ----
    match tokio::time::timeout(Duration::from_secs(25), dial("example.com:80".into())).await {
        Ok(Ok(mut s)) => {
            let mut buf = [0u8; 64];
            let r = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf)).await;
            match r {
                Ok(Ok(0)) => println!("[B read-first] FIRST READ = EOF (0 bytes) <-- bug"),
                Ok(Ok(n)) => println!("[B read-first] read {n} bytes"),
                Ok(Err(e)) => println!("[B read-first] error {e}"),
                Err(_) => println!("[B read-first] read timed out"),
            }
            // The write that a real client would then make.
            let w = tokio::time::timeout(
                Duration::from_secs(10),
                s.write_all(b"GET / HTTP/1.0\r\nHost: example.com\r\n\r\n"),
            )
            .await;
            println!("[B read-first] write after EOF = {:?}", w);
            let mut rest = Vec::new();
            let r = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut rest)).await;
            println!("[B read-first] remaining bytes = {:?} {:?}", r.map(|x| x.map(|_| ())), rest.len());
        }
        Ok(Err(e)) => println!("[B read-first] dial failed: {e}"),
        Err(_) => println!("[B read-first] dial timeout"),
    }

    // ---- C: real SOCKS5 client ----
    match socks_get(port, "example.com").await {
        Ok(n) => println!("[C socks] {n} bytes"),
        Err(e) => println!("[C socks] error: {e}"),
    }

    println!("[repro] done, exiting");
    let _ = &mgr;
}

/// Minimal SOCKS5 CONNECT + HTTP GET, mirroring the Python test script.
async fn socks_get(port: u16, host: &str) -> std::io::Result<usize> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    s.set_nodelay(true).ok();
    s.write_all(&[0x05, 0x01, 0x02]).await?;
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await?;
    let (u, p) = ("warpuser", "test-pass-123");
    let mut auth = vec![0x01, u.len() as u8];
    auth.extend_from_slice(u.as_bytes());
    auth.push(p.len() as u8);
    auth.extend_from_slice(p.as_bytes());
    s.write_all(&auth).await?;
    let mut ar = [0u8; 2];
    s.read_exact(&mut ar).await?;
    if ar[1] != 0 {
        return Err(std::io::Error::other("auth refused"));
    }
    // Resolve locally the way the SOCKS layer does, then ask for IPv4.
    let ip: std::net::Ipv4Addr = match host {
        "example.com" => "93.184.215.14".parse().unwrap(),
        _ => "127.0.0.1".parse().unwrap(),
    };
    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    req.extend_from_slice(&ip.octets());
    req.extend_from_slice(&80u16.to_be_bytes());
    s.write_all(&req).await?;
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await?;
    if rep[1] != 0 {
        return Err(std::io::Error::other(format!("REP={}", rep[1])));
    }
    s.write_all(
        format!("GET / HTTP/1.0\r\nHost: {host}\r\nConnection: Close\r\n\r\n").as_bytes(),
    )
    .await?;
    let mut total = 0usize;
    let mut buf = vec![0u8; 8192];
    loop {
        match tokio::time::timeout(Duration::from_secs(15), s.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(n)) => total += n,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    Ok(total)
}
