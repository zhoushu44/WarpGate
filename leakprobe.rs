//! Focused socket-leak probe with per-phase progress so a hang can be located.
//!
//! Runs N sequential requests through one long-lived stack, sampling the
//! smoltcp `SocketSet` size every `--step` requests. Prints the phase before
//! each blocking await, so if it hangs we know whether it was the dial, the
//! write, or the read.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn rss_kb() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1).map(|v| v.parse().ok()))
                .flatten()
        })
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).cloned().unwrap_or_else(|| "usque.json".into());
    let total: usize = arg(&args, "--total")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let step: usize = arg(&args, "--step")
        .and_then(|v| v.parse().ok())
        .unwrap_or(50);
    let host = arg(&args, "--host").unwrap_or_else(|| "api.ipify.org".into());
    let phase = args.iter().any(|a| a == "--phase");
    let concurrency: usize = arg(&args, "--concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);

    let raw = std::fs::read_to_string(&path).expect("read usque.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    let t = wg_tunnel::connectip::NativeTunnel::connect(
        &get("endpoint_v4"),
        &get("endpoint_pub_key"),
        &get("private_key"),
        &get("ipv6"),
        &get("ipv4"),
        true,
    )
    .await
    .expect("tunnel");
    let t = Arc::new(t);
    let dial = t.dial_fn();
    let body = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    println!("SAMPLE\t0\tsocks={}\trss_kb={}", t.socket_count().await, rss_kb());

    let ok_n = Arc::new(AtomicUsize::new(0));
    let next = Arc::new(AtomicUsize::new(0));
    let done_n = Arc::new(AtomicUsize::new(0));
    let mut handles = Vec::new();
    for _ in 0..concurrency.max(1) {
        let t = t.clone();
        let dial = dial.clone();
        let body = body.clone();
        let host = host.clone();
        let ok_n = ok_n.clone();
        let next = next.clone();
        let done_n = done_n.clone();
        handles.push(tokio::spawn(async move {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed) + 1;
                if i > total {
                    break;
                }
                if phase {
                    println!("PHASE\t{i}\tbefore_dial");
                }
                let mut s = match tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    dial(format!("{host}:80")),
                )
                .await
                {
                    Ok(Ok(s)) => s,
                    Ok(Err(e)) => {
                        println!("REQ\t{i}\t0\tdial_err: {e}");
                        continue;
                    }
                    Err(_) => {
                        println!("REQ\t{i}\t0\tdial_timeout");
                        continue;
                    }
                };
                let _ = s.write_all(body.as_bytes()).await;
                let mut buf = Vec::new();
                let _ = tokio::time::timeout(
                    std::time::Duration::from_secs(15),
                    s.read_to_end(&mut buf),
                )
                .await;
                drop(s);
                if std::str::from_utf8(&buf)
                    .map(|t| t.starts_with("HTTP/1.1 200"))
                    .unwrap_or(false)
                {
                    ok_n.fetch_add(1, Ordering::Relaxed);
                }
                let d = done_n.fetch_add(1, Ordering::Relaxed) + 1;
                if d % step == 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    println!(
                        "SAMPLE\t{}\tsocks={}\trss_kb={}\tok={}",
                        d,
                        t.socket_count().await,
                        rss_kb(),
                        ok_n.load(Ordering::Relaxed)
                    );
                }
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    println!(
        "FINAL\tsocks={}\trss_kb={}\tok={}",
        t.socket_count().await,
        rss_kb(),
        ok_n.load(Ordering::Relaxed)
    );
}
