//! End-to-end: register, start, and pull the real exit address through the
//! native MASQUE path. Ignored by default — needs UDP 443 to Cloudflare.

use std::time::Duration;

#[tokio::test]
#[ignore]
async fn registered_account_carries_traffic() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = wg_config::Config::default();
    cfg.resident_count = 1;
    cfg.base_port = 47110;
    cfg.whitelist_port_offset = 1000;
    cfg.ip_whitelist = Vec::new();
    cfg.rate_limit_enabled = false;
    cfg.ip_check_url_v6 = "https://api6.ipify.org".into();

    let mgr = std::sync::Arc::new(wg_pool::Manager::new(
        std::sync::Arc::new(cfg),
        tmp.path().to_path_buf(),
    ));

    let ucfg = mgr
        .register_for_test(0)
        .await
        .expect("registration must succeed");
    println!("registered {} with exit {}", ucfg.id, ucfg.ipv6);

    // Native MASQUE tunnel, no usque process.
    mgr.attach_masque(0)
        .await
        .expect("native MASQUE tunnel must start");

    // Let the QUIC handshake settle, then ask the echo service who we are.
    let exit = tokio::time::timeout(Duration::from_secs(45), mgr.probe_exit_for_test(0))
        .await
        .expect("probe must not hang")
        .expect("probe must run");

    println!("observed exit = {:?}", exit);
    assert!(
        exit.0.contains(':') || exit.1.contains('.'),
        "must observe an exit address, got {:?}",
        exit
    );
}
