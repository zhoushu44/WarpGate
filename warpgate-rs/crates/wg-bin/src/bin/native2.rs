//! Multi-instance liveness harness: do native CONNECT-IP tunnels stay up?
//!
//! ```text
//! WG_STACK_TRACE=1 ./native2 <config.json> <instances-dir> <count> <hold-secs> [target:port]
//! ```
//!
//! Attaches `count` instances through the production path in one process (the
//! way the pool does), then probes every instance once per interval and prints
//! a liveness line. A tunnel that stops carrying bytes shows up as `FAIL` on
//! every subsequent probe while `attach_masque` succeeded — the "500 tunnels
//! die after a while" symptom.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wg_config::Config;
use wg_pool::Manager;

#[tokio::main]
async fn main() {
    let cfg_path = std::env::args().nth(1).unwrap_or_else(|| "/root/wg-repro/config.json".into());
    let base = std::env::args().nth(2).unwrap_or_else(|| "/root/wg-repro/instances".into());
    let count: i64 = std::env::args().nth(3).unwrap_or_else(|| "8".into()).parse().unwrap();
    let hold: u64 = std::env::args().nth(4).unwrap_or_else(|| "180".into()).parse().unwrap();
    let target = std::env::args().nth(5).unwrap_or_else(|| "example.com:80".into());

    let cfg = Arc::new(Config::load(&cfg_path).expect("config"));
    let base_path = std::path::PathBuf::from(&base);
    let mgr = Manager::new(cfg.clone(), base_path.clone());

    println!("[harness] attaching {count} instances, holding {hold}s, target {target}");

    // `REPRO_CREATE=1` registers fresh accounts instead of reusing existing
    // ones. Never reuse a production account here: a second CONNECT-IP session
    // for the same identity can make the edge drop the live one.
    let create = std::env::var("REPRO_CREATE").as_deref() == Ok("1");

    let mut dials = Vec::new();
    for i in 1..=count {
        let dir = wg_store::dir(&base_path, i);
        if create && wg_store::UsqueConfig::read(&dir).ok().flatten().is_none() {
            if let Err(e) = mgr.create(i).await {
                println!("[harness] warp-{i}: create failed: {e}");
                continue;
            }
        }
        let Some(ucfg) = wg_store::UsqueConfig::read(&dir).ok().flatten() else {
            println!("[harness] warp-{i}: no usque.json, skipped");
            continue;
        };
        if let Err(e) = mgr.start_with_config(i, ucfg, false).await {
            println!("[harness] warp-{i}: start failed: {e}");
            continue;
        }
        let t0 = std::time::Instant::now();
        match mgr.attach_masque(i).await {
            Ok(n) => {
                let dial = mgr.get(i).await.and_then(|x| x.dial_now());
                println!(
                    "[harness] warp-{i}: attach ok ({n} listeners) in {:?}, dial={}",
                    t0.elapsed(),
                    dial.is_some()
                );
                if let Some(d) = dial {
                    dials.push((i, d));
                }
            }
            Err(e) => println!("[harness] warp-{i}: attach FAILED: {e}"),
        }
    }
    println!("[harness] attached {}/{}", dials.len(), count);

    let start = std::time::Instant::now();
    let mut round = 0u64;
    let mut last_fails: Vec<i64> = Vec::new();
    while start.elapsed() < Duration::from_secs(hold) {
        tokio::time::sleep(Duration::from_secs(10)).await;
        round += 1;
        let mut ok = 0usize;
        let mut fails = Vec::new();
        for (i, dial) in &dials {
            match probe(dial, &target, 8).await {
                Ok(n) if n > 0 => ok += 1,
                Ok(_) => fails.push(*i),
                Err(_) => fails.push(*i),
            }
        }
        println!(
            "[harness] t={:>3}s round={round} ok={ok}/{} fail={:?} {}",
            start.elapsed().as_secs(),
            dials.len(),
            fails,
            wg_tunnel::connectip::trace::summary()
        );
        if fails == last_fails && !fails.is_empty() && ok == 0 {
            println!("[harness] ALL PROBES FAILING, stopping early");
            break;
        }
        last_fails = fails;
    }
    let _ = mgr;
    println!("[harness] done");
}

async fn probe(dial: &wg_tunnel::DialFn, target: &str, timeout_s: u64) -> std::io::Result<usize> {
    let host = target.rsplit_once(':').map(|(h, _)| h).unwrap_or(target);
    let body = format!("GET / HTTP/1.0\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let fut = async {
        let mut s = dial(target.to_string()).await?;
        s.write_all(body.as_bytes()).await?;
        let mut buf = Vec::new();
        let _ = s.read_to_end(&mut buf).await;
        Ok::<usize, std::io::Error>(buf.len())
    };
    match tokio::time::timeout(Duration::from_secs(timeout_s), fut).await {
        Ok(r) => r,
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "probe")),
    }
}
