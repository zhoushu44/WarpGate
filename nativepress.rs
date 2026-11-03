//! Concurrency / leak stress harness for the native CONNECT-IP data plane.
//!
//! `nativecheck` is one-shot: tunnel up, one request, exit. That cannot
//! exercise the two things under test here:
//!
//!  * a lock-order inversion between `connect()` (sockets -> iface) and
//!    `pump_once()` (iface -> sockets). Reproducing it needs a dial racing the
//!    2 ms pump driver inside one process.
//!  * a socket that is never removed from the smoltcp `SocketSet`. Process exit
//!    would hide it, so all dials must happen in one long-lived process.
//!
//! Output is tab-separated so bash/awk can post-process it (no python on host):
//!   REQ    idx tunnel ok ms bytes status error
//!   SAMPLE tag n fd=.. rss_kb=.. socks=..
//!   SUMMARY ...

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const IO_TIMEOUT: Duration = Duration::from_secs(20);

/// Build the request line for a target such as `www.gstatic.com:80`.
fn request_body(target: &str, path: &str) -> Vec<u8> {
    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").into_bytes()
}

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

fn fd_count() -> u64 {
    std::fs::read_dir("/proc/self/fd")
        .map(|d| d.count() as u64)
        .unwrap_or(0)
}

async fn sample(tag: &str, n: usize, tunnels: &[Arc<wg_tunnel::connectip::NativeTunnel>]) {
    let mut socks = 0usize;
    for t in tunnels {
        socks += t.socket_count().await;
    }
    println!(
        "SAMPLE\t{}\t{}\tfd={}\trss_kb={}\tsocks={}",
        tag,
        n,
        fd_count(),
        rss_kb(),
        socks
    );
}

/// One request over one dial. Returns (ok, bytes, http_status, error).
async fn one_request(
    dial: &wg_tunnel::DialFn,
    target: &str,
    body: &[u8],
    dial_timeout: Duration,
) -> (bool, usize, String, String) {
    let t = Instant::now();
    let stream = match tokio::time::timeout(dial_timeout, dial(target.to_string())).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return (false, 0, String::new(), format!("dial: {e}")),
        Err(_) => {
            return (
                false,
                0,
                String::new(),
                format!("dial_timeout_{}s", dial_timeout.as_secs()),
            )
        }
    };
    let mut s = stream;
    if let Err(e) = s.write_all(body).await {
        return (false, 0, String::new(), format!("write: {e}"));
    }
    let mut buf = Vec::new();
    match tokio::time::timeout(IO_TIMEOUT, s.read_to_end(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return (false, buf.len(), String::new(), format!("read: {e}")),
        Err(_) => return (false, buf.len(), String::new(), "read_timeout".into()),
    }
    let text = String::from_utf8_lossy(&buf);
    let status = text
        .lines()
        .next()
        .unwrap_or("")
        .split_whitespace()
        .nth(1)
        .unwrap_or("")
        .to_string();
    // 200 for a normal page, 204 for the connectivity-check endpoints, which
    // do not rate-limit the way api.ipify.org does.
    let ok = status == "200" || status == "204";
    let err = if ok {
        String::new()
    } else if buf.is_empty() {
        "empty_response".to_string()
    } else if status.is_empty() {
        "malformed".to_string()
    } else {
        format!("http_{status}")
    };
    let _ = t;
    (ok, buf.len(), status, err)
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let config_path = args.get(1).cloned().unwrap_or_else(|| "usque.json".into());
    let tunnels_n: usize = arg(&args, "--tunnels")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let concurrency: usize = arg(&args, "--concurrency")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    let total: usize = arg(&args, "--total")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let target = arg(&args, "--target").unwrap_or_else(|| "api.ipify.org:80".into());
    let path = arg(&args, "--path").unwrap_or_else(|| "/".into());
    let body = Arc::new(request_body(&target, &path));
    let dial_timeout = Duration::from_secs(
        arg(&args, "--dial-timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(25u64),
    );

    let raw = std::fs::read_to_string(&config_path).expect("read usque.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    println!(
        "CONFIG\ttunnels={}\tconcurrency={}\ttotal={}\tdial_timeout={}s\ttarget={}",
        tunnels_n,
        concurrency,
        total,
        dial_timeout.as_secs(),
        target
    );
    sample("start", 0, &[]).await;

    let t0 = Instant::now();
    let mut tunnels: Vec<Arc<wg_tunnel::connectip::NativeTunnel>> = Vec::new();
    for i in 0..tunnels_n {
        match wg_tunnel::connectip::NativeTunnel::connect(
            &get("endpoint_v4"),
            &get("endpoint_pub_key"),
            &get("private_key"),
            &get("ipv6"),
            &get("ipv4"),
            true,
        )
        .await
        {
            Ok(t) => tunnels.push(Arc::new(t)),
            Err(e) => {
                println!("TUNNEL\t{}\tFAIL\t{}", i, e.replace('\t', " "));
                println!(
                    "SUMMARY\ttunnels_wanted={}\ttunnels_up={}\tbringup_ms={}\tFATAL",
                    tunnels_n,
                    tunnels.len(),
                    t0.elapsed().as_millis()
                );
                std::process::exit(2);
            }
        }
        println!("TUNNEL\t{}\tOK", i);
    }
    let bringup_ms = t0.elapsed().as_millis();
    sample("after_bringup", 0, &tunnels).await;

    let dials: Arc<Vec<wg_tunnel::DialFn>> =
        Arc::new(tunnels.iter().map(|t| t.dial_fn()).collect());

    let next = Arc::new(AtomicUsize::new(0));
    let ok_n = Arc::new(AtomicUsize::new(0));
    let fail_n = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();

    // Periodic samples while load runs, so a steady socket climb is visible
    // even if the run is interrupted.
    let want_samples = total >= 100;
    let sampler = if want_samples {
        let tunnels = tunnels.clone();
        let next_c = next.clone();
        Some(tokio::spawn(async move {
            let mut last = 0usize;
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let done = next_c.load(Ordering::Relaxed);
                // Every 50 requests, plus a final tick once the run ends.
                if done >= total || done == last || done % 50 != 0 {
                    if done >= total && done == last {
                        break;
                    }
                    if done >= total {
                        sample("running", done, &tunnels).await;
                        break;
                    }
                    continue;
                }
                last = done;
                sample("running", done, &tunnels).await;
            }
        }))
    } else {
        None
    };

    let mut workers = Vec::new();
    for _ in 0..concurrency.min(total.max(1)) {
        let dials = dials.clone();
        let next = next.clone();
        let ok_n = ok_n.clone();
        let fail_n = fail_n.clone();
        let target = target.clone();
        let body = body.clone();
        workers.push(tokio::spawn(async move {
            loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= total {
                    break;
                }
                let dial = dials[idx % dials.len()].clone();
                let t = Instant::now();
                let (ok, bytes, status, err) =
                    one_request(&dial, &target, &body, dial_timeout).await;
                let ms = t.elapsed().as_millis();
                if ok {
                    ok_n.fetch_add(1, Ordering::Relaxed);
                } else {
                    fail_n.fetch_add(1, Ordering::Relaxed);
                }
                println!(
                    "REQ\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    idx,
                    idx % dials.len(),
                    if ok { 1 } else { 0 },
                    ms,
                    bytes,
                    if status.is_empty() { "-" } else { &status },
                    err.replace('\t', " ")
                );
            }
        }));
    }
    for w in workers {
        let _ = w.await;
    }
    let wall_ms = started.elapsed().as_millis();
    if let Some(s) = sampler {
        s.abort();
    }
    sample("after_load", total, &tunnels).await;

    println!(
        "SUMMARY\ttunnels={}\tconcurrency={}\ttotal={}\tok={}\tfail={}\twall_ms={}\tbringup_ms={}",
        tunnels.len(),
        concurrency,
        total,
        ok_n.load(Ordering::Relaxed),
        fail_n.load(Ordering::Relaxed),
        wall_ms,
        bringup_ms
    );

    // Settle, then sample again: a socket that was closed properly should be
    // gone from the set by now.
    tokio::time::sleep(Duration::from_millis(500)).await;
    sample("end", total, &tunnels).await;
    drop(tunnels);
}
