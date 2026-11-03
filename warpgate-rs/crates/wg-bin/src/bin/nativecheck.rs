//! Diagnostic: bring up the native path stage by stage and print where it stops.
//!
//! Reads an `usque.json` so a known-good account can be used, isolating
//! transport failures from registration ones.

use std::time::Duration;

#[tokio::main]
async fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| "usque.json".into());
    let raw = match std::fs::read_to_string(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot read {path}: {e}");
            std::process::exit(1);
        }
    };
    let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();

    println!("id          = {}", get("id"));
    println!("endpoint_v4 = {}", get("endpoint_v4"));
    println!("ipv6        = {}", get("ipv6"));

    println!("--- pin comparison ---");
    for s in wg_tunnel::connectip::native::compare_pin(
        &get("endpoint_v4"),
        &get("endpoint_pub_key"),
        &get("private_key"),
    )
    .await
    {
        println!("  {s}");
    }
    println!("--- stages ---");

    let stages = wg_tunnel::connectip::native::diagnose(
        &get("endpoint_v4"),
        &get("endpoint_pub_key"),
        &get("private_key"),
    )
    .await;
    for s in stages {
        println!("  {s}");
    }

    // Full tunnel + one HTTP request, if the stages got that far.
    let native = tokio::time::timeout(
        Duration::from_secs(40),
        wg_tunnel::connectip::NativeTunnel::connect(
            &get("endpoint_v4"),
            &get("endpoint_pub_key"),
            &get("private_key"),
            &get("ipv6"),
            &get("ipv4"),
            true,
        ),
    )
    .await;
    match native {
        Ok(Ok(n)) => {
            println!("tunnel: UP");
            let dial = std::sync::Arc::new(n).dial_fn();
            match tokio::time::timeout(Duration::from_secs(20), dial("api.ipify.org:80".into())).await {
                Ok(Ok(mut s)) => {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let _ = s
                        .write_all(b"GET / HTTP/1.1\r\nHost: api.ipify.org\r\nConnection: close\r\n\r\n")
                        .await;
                    let mut buf = Vec::new();
                    let _ = tokio::time::timeout(Duration::from_secs(15), s.read_to_end(&mut buf)).await;
                    println!("http: {} bytes", buf.len());
                    let t = String::from_utf8_lossy(&buf);
                    println!("body: {}", &t[..t.len().min(200)]);
                }
                Ok(Err(e)) => println!("dial failed: {e}"),
                Err(_) => println!("dial timeout"),
            }
        }
        Ok(Err(e)) => println!("tunnel failed: {e}"),
        Err(_) => println!("tunnel timeout"),
    }
}
