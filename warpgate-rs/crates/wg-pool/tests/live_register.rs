//! End-to-end registration check against the real Cloudflare API.
//!
//! Run with `cargo test -p wg-pool --test live_register -- --ignored --nocapture`.
//! Ignored by default: it needs egress to Cloudflare and creates a real account.

use std::time::Duration;

#[tokio::test]
#[ignore]
async fn registers_a_real_account_and_enrolls_a_masque_key() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = wg_config::Config::default();
    cfg.resident_count = 1;
    cfg.base_port = 47010;
    cfg.whitelist_port_offset = 1000;
    cfg.ip_whitelist = Vec::new();
    cfg.rate_limit_enabled = false;

    let mgr = std::sync::Arc::new(wg_pool::Manager::new(
        std::sync::Arc::new(cfg),
        tmp.path().to_path_buf(),
    ));

    // Straight to the registration path, no tunnel yet.
    let out = tokio::time::timeout(Duration::from_secs(60), mgr.register_for_test(0)).await;

    match out {
        Ok(Ok(u)) => {
            println!("id           = {}", u.id);
            println!("endpoint_v4  = {}", u.endpoint_v4);
            println!("ipv4         = {}", u.ipv4);
            println!("ipv6         = {}", u.ipv6);
            println!("private_key  = {}...", &u.private_key[..20.min(u.private_key.len())]);
            println!("pub_key_len  = {}", u.endpoint_pub_key.len());
            assert!(!u.id.is_empty(), "account id must not be empty");
            assert!(u.ipv6.contains(':'), "must get an IPv6 address, got {}", u.ipv6);
            assert!(!u.endpoint_v4.is_empty(), "must get an endpoint");
            assert!(
                u.endpoint_pub_key.contains("BEGIN PUBLIC KEY"),
                "must get a PEM pinned key"
            );
        }
        Ok(Err(e)) => panic!("registration failed: {e}"),
        Err(_) => panic!("registration timed out after 60s"),
    }
}
