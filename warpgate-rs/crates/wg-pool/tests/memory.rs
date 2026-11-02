//! Memory benchmark: how much does each instance actually cost?
//!
//! The Node version spends ~10 GB for 200 instances, and roughly 8 GB of that
//! is 401 Go runtime baselines (one `sing-box` + one `usque` per instance) —
//! the tunnel state itself is only a few hundred MB. So the interesting number
//! is not the binary size but the **marginal cost per instance**.
//!
//! This benchmark builds N instances with a fake dial function (no network, no
//! Cloudflare) and reports the process RSS before and after, so the per-instance
//! figure can be read off directly.
//!
//! Run with:
//!
//! ```text
//! cargo test --release -p wg-pool --test memory -- --nocapture --ignored
//! ```

use std::sync::Arc;

use wg_config::Config;
use wg_pool::Manager;
use wg_store::UsqueConfig;

/// A dial that always fails: the tunnel exists, the data plane does not.
fn stub_dial() -> wg_tunnel::DialFn {
    Arc::new(|_target| {
        Box::pin(async {
            Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "stub",
            ))
        })
    })
}

fn rss_mb() -> f64 {
    #[cfg(unix)]
    {
        if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
            for line in s.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: f64 = rest.trim().trim_end_matches(" kB").parse().unwrap_or(0.0);
                    return kb / 1024.0;
                }
            }
        }
        0.0
    }
    #[cfg(windows)]
    {
        use std::mem::MaybeUninit;
        use windows_sys::Win32::System::ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
        };
        use windows_sys::Win32::System::Threading::GetCurrentProcess;

        // SAFETY: zeroed PMC struct, then filled by the API.
        let mut pmc: PROCESS_MEMORY_COUNTERS = unsafe { MaybeUninit::zeroed().assume_init() };
        pmc.cb = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let ok = unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                &mut pmc,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
        };
        if ok == 0 {
            return 0.0;
        }
        // WorkingSetSize is the RSS equivalent on Windows.
        pmc.WorkingSetSize as f64 / (1024.0 * 1024.0)
    }
    #[cfg(not(any(unix, windows)))]
    {
        0.0
    }
}

fn fake_usque(i: i64) -> UsqueConfig {
    UsqueConfig {
        private_key: "MHcCAQEEIB9x7vVZ0o9YlQ1r2sT8k3mN4pQ5rS6tU7vW8x9y0z1A2oAoGCCqGSM49AwEHoUQDQgAE".into(),
        endpoint_v4: "162.159.198.3".into(),
        endpoint_v6: "[2606:4700:d0::a29f:c003]".into(),
        endpoint_h2_v4: "162.159.198.2".into(),
        endpoint_h2_v6: String::new(),
        endpoint_pub_key: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n".into(),
        id: format!("id-{}", i),
        access_token: "tok".into(),
        ipv4: "172.16.0.2/32".into(),
        ipv6: format!("2606:4700:110:8c9f::{}/128", i + 1),
    }
}

/// Build N instances and report the marginal cost of each.
///
/// Marked `ignore` so it only runs when explicitly asked for: it allocates a
/// real directory tree and is only meaningful on the machine being measured.
#[tokio::test]
#[ignore = "memory benchmark; run explicitly"]
async fn marginal_cost_per_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.resident_count = 200;
    cfg.max_instances = 300;
    cfg.max_concurrent_create = 16;

    let mgr = Arc::new(Manager::new(Arc::new(cfg), tmp.path().to_path_buf()));

    let baseline = rss_mb();
    let t0 = std::time::Instant::now();

    for i in 0..200 {
        let dir = wg_store::dir(tmp.path(), i);
        std::fs::create_dir_all(&dir).unwrap();
        fake_usque(i)
            .save(&wg_store::paths::usque_conf_path(&dir))
            .unwrap();
        mgr.start_with_config(i, fake_usque(i), false).await.unwrap();
    }

    let after_meta = rss_mb();
    println!();
    println!("=== warpgate-rs memory benchmark ===");
    println!("instances              : {}", mgr.count().await);
    println!("RSS baseline           : {:.2} MB", baseline);
    println!("RSS after metadata only: {:.2} MB  (delta {:.2} MB, {:.4} MB/inst)",
        after_meta, after_meta - baseline, (after_meta - baseline) / 200.0);
    println!("build wall time        : {:?}", t0.elapsed());
    println!();

    // A metadata-only instance is not the real cost: each one also owns a SOCKS5
    // listener (a tokio task + a socket + buffers), which is exactly what the
    // Node version pays a whole process for. Open those too.
    let dial = stub_dial();
    let mut opened_total = 0usize;
    for i in 0..200 {
        if let Ok(n) = mgr.attach_dial(i, dial.clone()).await {
            opened_total += n;
        }
    }
    let after_listeners = rss_mb();
    println!("SOCKS5 listeners      : {}", opened_total);
    println!("RSS after listeners   : {:.2} MB  (delta {:.2} MB, {:.4} MB/inst)",
        after_listeners, after_listeners - baseline, (after_listeners - baseline) / 200.0);
    println!();

    println!("reference (Node)      : ~10000 MB for 200 instances (~50 MB each)");
    println!("reference (Go)        :   ~1000 MB for 200 instances (~5 MB each)");
    println!();

    // The real ceiling: 200 instances must not approach the Node version even
    // with a live listener each. 5 MB/instance is the Go figure; we hold
    // ourselves to well under it.
    let per = (after_listeners - baseline) / 200.0;
    assert!(
        per < 2.0,
        "每实例 {:.4} MB 超过 2 MB 预算——内存回退了",
        per
    );
}
