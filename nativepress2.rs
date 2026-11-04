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
//!   PROGRESS n=.. ok=.. fail=.. inflight=..
//!   SUMMARY ...

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};

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

async fn sample(tag: &str, n: usize, socks: usize) {
    println!(
        "SAMPLE\t{}\t{}\tfd={}\trss_kb={}\tsocks={}",
        tag,
        n,
        fd_count(),
        rss_kb(),
        socks
    );
}

/// One request over one dial. Returns (ok, bytes, http_status, error, phase).
///
/// `req_timeout` bounds the WHOLE request, including the write. Production code
/// has no such bound (`copy_bidirectional` writes without a deadline), which is
/// exactly why a stuck `poll_write` wedges a task forever; the harness needs the
/// relief valve to be able to run a 500-dial sweep at all.
async fn one_request(
    dial: &wg_tunnel::DialFn,
    target: &str,
    body: &[u8],
    dial_timeout: Duration,
    io_timeout: Duration,
    req_timeout: Option<Duration>,
) -> (bool, usize, String, String, &'static str) {
    let fut = one_request_inner(dial, target, body, dial_timeout, io_timeout);
    match req_timeout {
        Some(t) => match tokio::time::timeout(t, fut).await {
            Ok(v) => v,
            Err(_) => (
                false,
                0,
                String::new(),
                format!("req_timeout_{}s", t.as_secs()),
                "req",
            ),
        },
        None => fut.await,
    }
}

async fn one_request_inner(
    dial: &wg_tunnel::DialFn,
    target: &str,
    body: &[u8],
    dial_timeout: Duration,
    io_timeout: Duration,
) -> (bool, usize, String, String, &'static str) {
    let t_dial = Instant::now();
    let stream = match tokio::time::timeout(dial_timeout, dial(target.to_string())).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return (false, 0, String::new(), format!("dial: {e}"), "dial"),
        Err(_) => {
            return (
                false,
                0,
                String::new(),
                format!("dial_timeout_{}s", dial_timeout.as_secs()),
                "dial",
            )
        }
    };
    if std::env::var_os("NP_TRACE").is_some() {
        eprintln!("PHASE\tdial_ok\t{}ms", t_dial.elapsed().as_millis());
    }
    let mut s = stream;
    // Diagnostic only: `connect()` returns while smoltcp is still in SynSent,
    // so an immediate write hits `can_send() == false`. The old code retried
    // via `wake_by_ref()`; a fixed build must wake the task itself. Sleeping
    // here therefore isolates "no wake" from "slow handshake".
    let pre_write_delay = std::env::var("NP_DELAY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    if pre_write_delay > 0 {
        tokio::time::sleep(Duration::from_millis(pre_write_delay)).await;
    }
    let t_write = Instant::now();
    if let Err(e) = s.write_all(body).await {
        return (false, 0, String::new(), format!("write: {e}"), "write");
    }
    if std::env::var_os("NP_TRACE").is_some() {
        eprintln!("PHASE\twrite_ok\t{}ms", t_write.elapsed().as_millis());
    }
    let t_read = Instant::now();
    if std::env::var_os("NP_TRACE").is_some() {
        eprintln!("PHASE\tread_start");
    }
    let _ = (&t_read, &t_dial);
    let mut buf = Vec::new();
    match tokio::time::timeout(io_timeout, s.read_to_end(&mut buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(e)) => return (false, buf.len(), String::new(), format!("read: {e}"), "read"),
        Err(_) => return (false, buf.len(), String::new(), "read_timeout".into(), "read"),
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
    (ok, buf.len(), status, err, "ok")
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
    let target = arg(&args, "--target").unwrap_or_else(|| "www.gstatic.com:80".into());
    let path = arg(&args, "--path").unwrap_or_else(|| "/generate_204".into());
    let body = Arc::new(request_body(&target, &path));
    let dial_timeout = Duration::from_secs(
        arg(&args, "--dial-timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(25u64),
    );
    let io_timeout = Duration::from_secs(
        arg(&args, "--io-timeout")
            .and_then(|v| v.parse().ok())
            .unwrap_or(20u64),
    );
    let req_timeout = arg(&args, "--req-timeout")
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .map(Duration::from_secs);

    let raw = std::fs::read_to_string(&config_path).expect("read usque.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    println!(
        "CONFIG\ttunnels={}\tconcurrency={}\ttotal={}\tdial_timeout={}s\tio_timeout={}s\ttarget={}",
        tunnels_n,
        concurrency,
        total,
        dial_timeout.as_secs(),
        io_timeout.as_secs(),
        target
    );
    sample("start", 0, 0).await;

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
    sample("after_bringup", 0, 0).await;

    let dials: Arc<Vec<wg_tunnel::DialFn>> =
        Arc::new(tunnels.iter().map(|t| t.dial_fn()).collect());

    let next = Arc::new(AtomicUsize::new(0));
    let done_n = Arc::new(AtomicUsize::new(0));
    let ok_n = Arc::new(AtomicUsize::new(0));
    let fail_n = Arc::new(AtomicUsize::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();

    // Heartbeat + RSS sampler: every 2 s. `PROGRESS` lets an external watchdog
    // tell "slow" from "stuck" without parsing REQ lines.
    let hb = {
        let next_c = next.clone();
        let done_c = done_n.clone();
        let ok_c = ok_n.clone();
        let fail_c = fail_n.clone();
        let inflight_c = inflight.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                println!(
                    "PROGRESS\tstarted={}\tdone={}\tok={}\tfail={}\tinflight={}\trss_kb={}\tfd={}",
                    next_c.load(Ordering::Relaxed),
                    done_c.load(Ordering::Relaxed),
                    ok_c.load(Ordering::Relaxed),
                    fail_c.load(Ordering::Relaxed),
                    inflight_c.load(Ordering::Relaxed),
                    rss_kb(),
                    fd_count()
                );
            }
        })
    };

    let mut workers = Vec::new();
    for _ in 0..concurrency.min(total.max(1)) {
        let dials = dials.clone();
        let next = next.clone();
        let done_n = done_n.clone();
        let ok_n = ok_n.clone();
        let fail_n = fail_n.clone();
        let inflight = inflight.clone();
        let target = target.clone();
        let body = body.clone();
        workers.push(tokio::spawn(async move {
            loop {
                let idx = next.fetch_add(1, Ordering::Relaxed);
                if idx >= total {
                    break;
                }
                inflight.fetch_add(1, Ordering::Relaxed);
                let dial = dials[idx % dials.len()].clone();
                let t = Instant::now();
                let (ok, bytes, status, err, phase) =
                    one_request(&dial, &target, &body, dial_timeout, io_timeout, req_timeout).await;
                let ms = t.elapsed().as_millis();
                inflight.fetch_sub(1, Ordering::Relaxed);
                if ok {
                    ok_n.fetch_add(1, Ordering::Relaxed);
                } else {
                    fail_n.fetch_add(1, Ordering::Relaxed);
                }
                done_n.fetch_add(1, Ordering::Relaxed);
                println!(
                    "REQ\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    idx,
                    idx % dials.len(),
                    if ok { 1 } else { 0 },
                    ms,
                    bytes,
                    if status.is_empty() { "-" } else { &status },
                    phase,
                    err.replace('\t', " ")
                );
            }
        }));
    }
    for w in workers {
        let _ = w.await;
    }
    let wall_ms = started.elapsed().as_millis();
    hb.abort();
    sample("after_load", total, 0).await;

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

    tokio::time::sleep(Duration::from_millis(500)).await;
    sample("end", total, 0).await;
    drop(tunnels);
}
