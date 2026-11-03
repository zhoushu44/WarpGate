//! Register N WARP accounts and write their `usque.json` into an instance dir.
//!
//! Accounts are anonymous — `POST /reg` needs no email — so this is the bulk
//! path for growing the pool. Indexes continue from what is already on disk.

use std::time::Duration;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let base = args.get(1).cloned().unwrap_or_else(|| "/root/warpgate-rs/instances".into());
    let want: i64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(400);
    let concurrency: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);

    let base = std::path::PathBuf::from(base);
    std::fs::create_dir_all(&base).ok();

    // Find the highest index already present.
    let mut start = 0i64;
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        if let Some(rest) = n.strip_prefix("warp-") {
            if let Ok(i) = rest.parse::<i64>() {
                start = start.max(i + 1);
            }
        }
    }
    println!("base={:?} start_index={} want={} concurrency={}", base, start, want, concurrency);

    let cfg = wg_config::Config::default();
    let mgr = std::sync::Arc::new(wg_pool::Manager::new(
        std::sync::Arc::new(cfg),
        base.clone(),
    ));

    let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(concurrency));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let failed = std::sync::Arc::new(std::sync::atomic::AtomicI64::new(0));
    let mut handles = Vec::new();

    for k in 0..want {
        let idx = start + k;
        let mgr = mgr.clone();
        let sem = sem.clone();
        let done = done.clone();
        let failed = failed.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.ok();
            // Each attempt gets its own timeout: a hung registration must not
            // stall the batch.
            match tokio::time::timeout(Duration::from_secs(40), mgr.register_for_test(idx)).await {
                Ok(Ok(u)) => {
                    let n = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    println!("[{n}] warp-{idx} ok  ipv6={}", u.ipv6);
                }
                Ok(Err(e)) => {
                    let n = failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    println!("[{n}] warp-{idx} FAILED: {e}");
                }
                Err(_) => {
                    let n = failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    println!("[{n}] warp-{idx} TIMEOUT");
                }
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
    println!(
        "done: ok={} failed={}",
        done.load(std::sync::atomic::Ordering::SeqCst),
        failed.load(std::sync::atomic::Ordering::SeqCst)
    );
}
