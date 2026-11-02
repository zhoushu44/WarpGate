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
    let use_engine = cfg.masque_http2 || std::env::var("WARP_FORCE_ENGINE").is_ok();
    if use_engine {
        if !std::path::Path::new(&engine_binary).exists() {
            eprintln!(
                "启动失败: {} 不存在。masque_http2 需要 usque 二进制（或用 WARP_USQUE_BIN 指定路径）",
                engine_binary
            );
            return ExitCode::FAILURE;
        }
        tracing::info!(target: "BOOT", "数据面: usque 外部引擎 ({})，http2={}", engine_binary, cfg.masque_http2);
    } else {
        tracing::info!(target: "BOOT", "数据面: Rust 原生 MASQUE（QUIC/UDP）");
    }

    let base = cfg.instances_dir_resolved(args.instances.as_deref().unwrap_or(""));
    if let Err(e) = std::fs::create_dir_all(&base) {
        eprintln!("创建实例目录失败: {e}");
        return ExitCode::FAILURE;
    }

    let mgr = Arc::new(Manager::new(cfg.clone(), base));

    let cancel = CancellationToken::new();

    // Warm-start existing accounts, then top up to resident_count.
    tracing::info!(target: "BOOT", "开始恢复/创建实例（并发上限 {}）...", cfg.max_concurrent_create);
    let boot_start = std::time::Instant::now();
    mgr.boot(&cancel).await;

    // Attach the data plane to every running instance. A failure here marks the
    // instance unhealthy rather than aborting the boot, so a partially-working
    // pool is still usable.
    if use_engine {
        let mut ok = 0;
        let mut failed = 0;
        for inst in mgr.all().await {
            match mgr.attach_engine(inst.index, &engine_binary, cfg.masque_http2).await {
                Ok(_) => ok += 1,
                Err(e) => {
                    failed += 1;
                    tracing::warn!(target: "BOOT", "warp-{} 数据面启动失败: {}", inst.index, e);
                }
            }
        }
        tracing::info!(target: "BOOT", "数据面就绪 {}/{}（失败 {}）", ok, ok + failed, failed);
    } else {
        for inst in mgr.all().await {
            if let Err(e) = mgr.attach_masque(inst.index).await {
                tracing::warn!(target: "BOOT", "warp-{} MASQUE 失败: {}", inst.index, e);
            }
        }
    }

    tracing::info!(
        target: "BOOT",
        "实例就绪 {}/{}，耗时 {:?}",
        mgr.healthy_count().await,
        cfg.resident_count,
        boot_start.elapsed()
    );

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
