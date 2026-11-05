//! `warpgate` — the binary.
//!
//! Mirrors Go `warpgate-go/main.go`: load the config, warm-start the pool,
//! start the loops, serve the API, and shut down cleanly on a signal.

use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use tokio_util::sync::CancellationToken;
use wg_config::Config;
use wg_pool::Manager;

/// WarpGate: an embedded single-process WARP proxy pool.
#[derive(Parser, Debug)]
#[command(name = "warpgate", version, about)]
struct Args {
    /// Path to `config.json`.
    #[arg(long, default_value = "config.json")]
    config: String,

    /// Instances directory. Overrides `WARP_INSTANCES_DIR` and the config key.
    #[arg(long)]
    instances: Option<String>,
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .compact()
        .init();

    let args = Args::parse();

    let cfg = match Config::load(&args.config) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            eprintln!("启动失败: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        target: "BOOT",
        "实例目录: {}，常驻: {}，端口: {}~{}",
        cfg.instances_dir_resolved(args.instances.as_deref().unwrap_or(""))
            .display(),
        cfg.resident_count,
        cfg.base_port,
        cfg.base_port + cfg.max_instances - 1
    );

    // `masque_http2` means the tunnel runs over TCP 443 instead of QUIC/UDP.
    // The Rust-native QUIC client cannot do that yet, so this build falls back
    // to the `usque` binary — the same data plane the Node version uses on
    // networks that block UDP 443.
    let engine_binary = std::env::var("WARP_USQUE_BIN")
        .unwrap_or_else(|_| "/usr/local/bin/usque".to_string());

    // `WARP_NATIVE` selects the data plane explicitly:
    //
    //   h2   — native CONNECT-IP over TCP 443 (needs no child process)
    //   quic — native QUIC/UDP 443
    //   *    — the usque subprocess (the historical behaviour)
    //
    // It takes precedence over `masque_http2`: that flag describes the
    // *transport shape* (TCP vs UDP), and both shapes now have native
    // implementations, so the flag alone can no longer imply "use usque".
    // `WARP_NATIVE` selects the data plane explicitly:
    //
    //   h2    -- native CONNECT-IP over TCP 443 (no child process)
    //   quic  -- the usque subprocess, which on this host is the only working
    //            path: UDP 443 to Cloudflare is blocked, so a native QUIC
    //            client can never complete a handshake here.
    //   unset -- historical default (`masque_http2` / WARP_FORCE_ENGINE)
    let native_mode = std::env::var("WARP_NATIVE").unwrap_or_default();
    let native_h2 = native_mode == "h2";
    let use_engine = if native_h2 {
        false
    } else if native_mode == "quic" {
        // Deliberately the engine, not the native QUIC dialer: this flag names
        // the transport shape the *engine* should use, and UDP is unusable here.
        true
    } else {
        cfg.masque_http2 || std::env::var("WARP_FORCE_ENGINE").is_ok()
    };
    if use_engine {
        if !std::path::Path::new(&engine_binary).exists() {
            eprintln!(
                "启动失败: {} 不存在。masque_http2 需要 usque 二进制（或用 WARP_USQUE_BIN 指定路径）",
                engine_binary
            );
            return ExitCode::FAILURE;
        }
        tracing::info!(target: "BOOT", "数据面: usque 外部引擎 ({})，http2={}", engine_binary, cfg.masque_http2);
        // Kill engines left behind by a previous run.
        //
        // systemd stops the main process, but usque runs with
        // --always-reconnect and keeps reconnecting on its own. A restarted
        // warpgate then finds the ports taken and starts a second copy, so each
        // instance ends up with two engines. Seen in production: 500 orphans
        // had accumulated 24,543 TCP connections to Cloudflare and the whole
        // pool stopped forwarding.
        reap_orphan_engines(&engine_binary);
    } else {
        if native_h2 {
            tracing::info!(target: "BOOT", "数据面: Rust 原生 CONNECT-IP（HTTP/2 over TCP 443）");
        } else {
            tracing::info!(target: "BOOT", "数据面: Rust 原生 MASQUE（QUIC/UDP）");
        }
    }

    let base = cfg.instances_dir_resolved(args.instances.as_deref().unwrap_or(""));
    if let Err(e) = std::fs::create_dir_all(&base) {
        eprintln!("创建实例目录失败: {e}");
        return ExitCode::FAILURE;
    }

    let mgr = Arc::new(Manager::new(cfg.clone(), base));

    let cancel = CancellationToken::new();

    // Instance bring-up runs in the background on purpose.
    //
    // 500 instances take minutes to attach (each usque needs its port to come
    // up), and doing that before `axum::serve` means the console is unreachable
    // for the entire time — it looks like the deployment failed. Binding first
    // and filling the pool afterwards keeps the UI usable throughout; the
    // counters simply climb while it works.
    let boot_mgr = mgr.clone();
    let boot_cancel = cancel.clone();
    let boot_engine = engine_binary.clone();
    tokio::spawn(async move {
        tracing::info!(target: "BOOT", "开始恢复/创建实例（并发上限 {}）...", boot_mgr.config().max_concurrent_create);
        let boot_start = std::time::Instant::now();
        boot_mgr.boot(&boot_cancel).await;

        // Attach the data plane to every running instance. A failure here marks
        // the instance unhealthy rather than aborting, so a partially-working
        // pool is still usable.
        if use_engine {
            let mut ok = 0;
            let mut failed = 0;
            for inst in boot_mgr.all().await {
                if boot_cancel.is_cancelled() {
                    return;
                }
                match boot_mgr
                    .attach_engine(inst.index, &boot_engine, boot_mgr.config().masque_http2)
                    .await
                {
                    Ok(_) => ok += 1,
                    Err(e) => {
                        failed += 1;
                        tracing::warn!(target: "BOOT", "warp-{} 数据面启动失败: {}", inst.index, e);
                    }
                }
            }
            tracing::info!(target: "BOOT", "数据面就绪 {}/{}（失败 {}）", ok, ok + failed, failed);
        } else {
            for inst in boot_mgr.all().await {
                if boot_cancel.is_cancelled() {
                    return;
                }
                if let Err(e) = boot_mgr.attach_masque(inst.index).await {
                    tracing::warn!(target: "BOOT", "warp-{} MASQUE 失败: {}", inst.index, e);
                }
            }
        }

        tracing::info!(
            target: "BOOT",
            "实例就绪 {}/{}，耗时 {:?}",
            boot_mgr.healthy_count().await,
            boot_mgr.config().resident_count,
            boot_start.elapsed()
        );
    });

    mgr.start_loops(cancel.clone());

    // The API.
    let auth = Arc::new(wg_apiserver::AuthState::new(&cfg.api_key));
    let limiter = Arc::new(wg_apiserver::LoginLimiter::new(
        cfg.rate_limit_per_minute.max(1) as u32,
    ));
    let state = wg_apiserver::AppState {
        cfg: cfg.clone(),
        mgr: mgr.clone(),
        auth,
        limiter,
    };
    let app = wg_apiserver::router(state);
    let bind = format!("{}:{}", cfg.bind_address, cfg.port);
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("监听 {} 失败: {e}", bind);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(target: "API", "管理台监听 http://{}", bind);

    tokio::select! {
        r = axum::serve(listener, app) => {
            if let Err(e) = r {
                eprintln!("HTTP 服务错误: {e}");
                cancel.cancel();
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!(target: "BOOT", "收到退出信号，关闭中...");
        }
    }

    cancel.cancel();
    tracing::info!(
        target: "BOOT",
        "已退出（实例账号目录保留，重启后暖恢复）"
    );
    ExitCode::SUCCESS
}
/// Terminate engine processes that no warpgate is supervising.
///
/// Only processes reparented to init (PPid 1) are touched: those are left over
/// from a previous run. Engines belonging to a live warpgate still have a
/// parent, so two concurrent instances of this binary do not fight.
fn reap_orphan_engines(engine_binary: &str) {
    let name = std::path::Path::new(engine_binary)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("usque");

    let Ok(rd) = std::fs::read_dir("/proc") else {
        return;
    };

    let mut killed = 0usize;
    for entry in rd.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }

        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let cmd = String::from_utf8_lossy(&cmdline).replace('\0', " ");
        if !cmd.contains(name) || cmd.contains("warpgate") {
            continue;
        }

        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            continue;
        };
        let ppid = status
            .lines()
            .find_map(|l| l.strip_prefix("PPid:"))
            .and_then(|s| s.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if ppid != 1 {
            continue;
        }

        // SIGKILL: usque ignores SIGTERM while it is reconnecting.
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
        }
        // Not reachable in practice: the engine only runs on Linux.
        #[cfg(not(unix))]
        let _ = pid;
        killed += 1;
    }

    if killed > 0 {
        tracing::warn!(killed, "清理了上一轮遗留的引擎进程");
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
}