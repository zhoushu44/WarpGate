//! CONNECT-IP probe binary.
//!
//! Dials the real Cloudflare endpoint over TCP 443 + HTTP/2 and reports what
//! came back. This is the gate for the Rust-native data plane: if the
//! hand-rolled HTTP/2 client gets a 200, the remaining work is a userspace
//! network stack; anything else means the frame construction needs more work.
//!
//! ```text
//! warpgate-probe --config <usque.json>
//! ```

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(name = "warpgate-probe", about = "Probe the Cloudflare MASQUE endpoint")]
struct Args {
    /// Path to an enrolled `usque.json`.
    #[arg(long)]
    config: PathBuf,
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let args = Args::parse();
    let raw = match std::fs::read_to_string(&args.config) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("读取 {} 失败: {e}", args.config.display());
            return std::process::ExitCode::FAILURE;
        }
    };
    let cfg: wg_store::UsqueConfig = match serde_json::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("解析 usque.json 失败: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    println!("== 账号 ==");
    println!("  id       : {}", cfg.id);
    println!("  ipv6     : {}", cfg.ipv6);
    println!("  endpoint : {} (h2: {})", cfg.endpoint_v4, cfg.endpoint_h2_v4);
    println!();

    println!("== 探测 HTTP/2 CONNECT-IP ==");
    println!("  SNI  : {}", wg_tunnel::connectip::CONNECT_SNI);
    println!("  URI  : {}", wg_tunnel::connectip::CONNECT_URI);

    // HTTP/2 uses the `_h2` endpoint field.
    let endpoint = if cfg.endpoint_h2_v4.trim().is_empty() {
        cfg.endpoint_v4.clone()
    } else {
        cfg.endpoint_h2_v4.clone()
    };
    println!("  dial : {}:443", endpoint);
    println!();

    match wg_tunnel::connectip::probe_connect_ip(&endpoint, &cfg.private_key).await {
        Ok(r) => {
            println!("== 结果 ==");
            println!("  HTTP status        : {}", r.http_status);
            println!(
                "  capsule-protocol   : {}",
                r.capsule_protocol_header.as_deref().unwrap_or("(absent)")
            );
            println!(
                "  cf-connect-proto   : {}",
                r.cf_connect_proto_echoed.as_deref().unwrap_or("(absent)")
            );
            println!();
            match r.http_status {
                200 => println!("VERDICT: CONNECT-IP over HTTP/2 is ACCEPTED."),
                401 | 403 => println!("VERDICT: REJECTED — key not enrolled or wrong cert."),
                s => println!("VERDICT: unexpected status {s}."),
            }
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            println!("== 失败 ==");
            println!("  {e}");
            std::process::ExitCode::FAILURE
        }
    }
}
