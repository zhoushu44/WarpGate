//! Grow the pool at a rate Cloudflare accepts.
//!
//! `POST /reg` is rate limited per source IP: six concurrent requests get 429
//! (`error code: 1015`) immediately, while one request every few seconds
//! succeeds indefinitely. This drives registration serially with a spacing
//! that stays under the limit, and retries a rejected attempt after a longer
//! pause rather than giving up.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "/root/warpgate-rs/instances".into());
    let target_total: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(500);
    let spacing_ms: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(8000);

    let base = std::path::PathBuf::from(base);
    std::fs::create_dir_all(&base).ok();

    let count_existing = || {
        std::fs::read_dir(&base)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.file_name().to_string_lossy().starts_with("warp-")
                    && e.path().join("usque.json").exists()
            })
            .count() as i64
    };

    println!("target={target_total} spacing={spacing_ms}ms");
    let ok = AtomicI64::new(0);
    let failed = AtomicI64::new(0);

    let cfg = wg_config::Config::default();
    let mgr = std::sync::Arc::new(wg_pool::Manager::new(
        std::sync::Arc::new(cfg),
        base.clone(),
    ));

    while count_existing() < target_total {
        let have = count_existing();
        println!("--- have {have} / {target_total} ---");

        // Lowest unused index.
        let mut idx = 0i64;
        while base.join(format!("warp-{idx}")).join("usque.json").exists() {
            idx += 1;
        }

        match tokio::time::timeout(Duration::from_secs(40), mgr.register_for_test(idx)).await {
            Ok(Ok(u)) => {
                let n = ok.fetch_add(1, Ordering::SeqCst) + 1;
                println!("[{n}] warp-{idx} ok ipv6={}", u.ipv6);
                tokio::time::sleep(Duration::from_millis(spacing_ms)).await;
            }
            Ok(Err(e)) => {
                let n = failed.fetch_add(1, Ordering::SeqCst) + 1;
                let msg = e.to_string();
                println!("[{n}] warp-{idx} FAILED: {msg}");
                // A rejection is the rate limiter: back off much harder than
                // the normal spacing instead of hammering it.
                if msg.contains("429") || msg.contains("1015") {
                    println!("    rate limited, sleeping 90s");
                    tokio::time::sleep(Duration::from_secs(90)).await;
                } else {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
            Err(_) => {
                let n = failed.fetch_add(1, Ordering::SeqCst) + 1;
                println!("[{n}] warp-{idx} TIMEOUT");
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        }
    }

    println!(
        "DONE have={} ok={} failed={}",
        count_existing(),
        ok.load(Ordering::SeqCst),
        failed.load(Ordering::SeqCst)
    );
}
