//! Minimal one-instance reproduction: does `attach_masque(0)` + SOCKS5 return
//! any bytes?
//!
//! Run:
//! ```text
//! WG_STACK_TRACE=1 ./native1 <config.json> <instances-dir> [index] [target:port]
//! ```
//!
//! It deliberately goes through the *production* path — `Manager::new` →
//! `start_with_config` → `attach_masque` → `attach_dial` → SOCKS5 listener —
//! and then exercises the same tunnel three ways:
//!
//! ```text
//!   A. mgr dial directly, write first, read to EOF   (what nativecheck does)
//!   B. mgr dial directly, read first                (what copy_bidirectional does)
//!   C. full SOCKS5 CONNECT + GET through the listener
//! ```
//!
//! A works and B/C fail => the bug is not in the tunnel or HTTP/2 layer; it is
//! where `StackStream` reports readiness immediately after `connect()`.
//!
//! Every stage prints its result and the per-layer counters from
//! `wg_tunnel::connectip::trace`, so one run shows which layer stopped moving.

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
    let target = std::env::args()
        .nth(4)
        .unwrap_or_else(|| "example.com:80".into());

    step("config", &format!("load {cfg_path}, instances {base}, index {idx}, target {target}"));
    let cfg = Arc::new(Config::load(&cfg_path).expect("config"));
    let base_path = std::path::PathBuf::from(&base);
    let mgr = Manager::new(cfg.clone(), base_path.clone());

    // ---- 1. the instance must already have an account ----
    let dir = wg_store::dir(&base_path, idx);
    let Some(ucfg) = wg_store::UsqueConfig::read(&dir).ok().flatten() else {
        fail(&format!("no usque.json in {} (use a warm instance dir)", dir.display()));
        return;
    };
    step(
        "instance",
        &format!(
            "warp-{idx} endpoint={} ipv6={} ipv4={}",
            ucfg.endpoint_v4, ucfg.ipv6, ucfg.ipv4
        ),
    );

    let inst = match mgr.start_with_config(idx, ucfg, false).await {
        Ok(i) => {
            step("start_with_config", "ok");
            i
        }
        Err(e) => {
            fail(&format!("start_with_config: {e}"));
            return;
        }
    };

    // ---- 2. the native CONNECT-IP tunnel ----
    let t0 = std::time::Instant::now();
    let listeners = match mgr.attach_masque(idx).await {
        Ok(n) => n,
        Err(e) => {
            fail(&format!("attach_masque({idx}): {e}"));
            return;
        }
    };
    step(
        "attach_masque",
        &format!("{listeners} listener(s) in {:?}", t0.elapsed()),
    );

    let port = inst.port as u16;
    step("socks", &format!("port {port} (credential mode)"));

    let dial = match inst.dial_now() {
        Some(d) => d,
        None => {
            fail("attach_masque installed no dial");
            return;
        }
    };

    // ---- A: write first, then read (nativecheck's shape) ----
    let body = http_get(&target);
    match tokio::time::timeout(Duration::from_secs(25), dial(target.clone())).await {
        Ok(Ok(mut s)) => {
            step("A dial", "ok");
            let w = s.write_all(&body).await;
            step("A write", &format!("{w:?} ({} bytes)", body.len()));
            let mut buf = Vec::new();
            let r = tokio::time::timeout(Duration::from_secs(20), s.read_to_end(&mut buf)).await;
            step(
                "A read",
                &format!(
                    "{:?} {} bytes first={:?}",
                    r.map(|x| x.map(|_| ())),
                    buf.len(),
                    buf.split(|b| *b == b'\n')
                        .next()
                        .map(|l| String::from_utf8_lossy(l).to_string())
                        .unwrap_or_default()
                ),
            );
        }
        Ok(Err(e)) => step("A dial", &format!("FAILED {e}")),
        Err(_) => step("A dial", "TIMEOUT"),
    }
    step("counters after A", &wg_tunnel::connectip::trace::summary());

    // ---- B: read first, exactly what copy_bidirectional polls ----
    match tokio::time::timeout(Duration::from_secs(25), dial(target.clone())).await {
        Ok(Ok(mut s)) => {
            let mut buf = [0u8; 64];
            let r = tokio::time::timeout(Duration::from_secs(10), s.read(&mut buf)).await;
            step(
                "B first read",
                &match r {
                    Ok(Ok(0)) => "EOF (0 bytes) <-- the bug".to_string(),
                    Ok(Ok(n)) => format!("{n} bytes"),
                    Ok(Err(e)) => format!("error {e}"),
                    Err(_) => "timeout".to_string(),
                },
            );
            let w = tokio::time::timeout(Duration::from_secs(10), s.write_all(&body)).await;
            step("B write after EOF", &format!("{w:?}"));
            let mut rest = Vec::new();
            let r = tokio::time::timeout(Duration::from_secs(10), s.read_to_end(&mut rest)).await;
            step(
                "B remaining",
                &format!("{:?} {} bytes", r.map(|x| x.map(|_| ())), rest.len()),
            );
        }
        Ok(Err(e)) => step("B dial", &format!("FAILED {e}")),
        Err(_) => step("B dial", "TIMEOUT"),
    }

    // ---- C: real SOCKS5 client against the listener ----
    match socks_get(
        port,
        &cfg.proxy_username,
        &cfg.proxy_password,
        &target,
        &body,
    )
    .await
    {
        Ok(n) => step("C socks", &format!("{n} bytes")),
        Err(e) => step("C socks", &format!("error: {e}")),
    }

    step("counters final", &wg_tunnel::connectip::trace::summary());
    step("done", "exiting (dropping mgr closes the tunnel)");
    drop(mgr);
}

fn step(stage: &str, msg: &str) {
    println!("[{stage}] {msg}");
}

fn fail(msg: &str) {
    println!("[FATAL] {msg}");
}

fn http_get(target: &str) -> Vec<u8> {
    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    format!("GET / HTTP/1.0\r\nHost: {host}\r\nConnection: Close\r\n\r\n").into_bytes()
}

/// Minimal SOCKS5 CONNECT + HTTP request, mirroring `/root/socks_test.py`.
async fn socks_get(
    port: u16,
    user: &str,
    pass: &str,
    target: &str,
    body: &[u8],
) -> std::io::Result<usize> {
    let mut s = tokio::net::TcpStream::connect(("127.0.0.1", port)).await?;
    s.set_nodelay(true).ok();
    s.write_all(&[0x05, 0x01, 0x02]).await?;
    let mut m = [0u8; 2];
    s.read_exact(&mut m).await?;
    let mut auth = vec![0x01, user.len() as u8];
    auth.extend_from_slice(user.as_bytes());
    auth.push(pass.len() as u8);
    auth.extend_from_slice(pass.as_bytes());
    s.write_all(&auth).await?;
    let mut ar = [0u8; 2];
    s.read_exact(&mut ar).await?;
    if ar[1] != 0 {
        return Err(std::io::Error::other("auth refused"));
    }
    // ATYP=domain: the SOCKS layer resolves it (same as production).
    let (host, port_s) = target.rsplit_once(':').unwrap_or((target, "80"));
    let port_n: u16 = port_s.parse().unwrap_or(80);
    let mut req = vec![0x05, 0x01, 0x00, 0x03, host.len() as u8];
    req.extend_from_slice(host.as_bytes());
    req.extend_from_slice(&port_n.to_be_bytes());
    s.write_all(&req).await?;
    let mut rep = [0u8; 10];
    s.read_exact(&mut rep).await?;
    if rep[1] != 0 {
        return Err(std::io::Error::other(format!("REP={}", rep[1])));
    }
    s.write_all(body).await?;
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
