//! End-to-end API tests over the real HTTP stack.
//!
//! The critical assertions are the auth ones: an unauthenticated request must
//! get 401 `A0001`, and a leaked key must not be usable from outside the
//! whitelist. Those were the gap in the first draft of `routes.rs` — the auth
//! helper existed but was never wired up.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use wg_apiserver::{router, AppState};
use wg_config::Config;
use wg_pool::Manager;

async fn app_with(api_key: &str, whitelist: Vec<String>) -> (axum::Router, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.api_key = api_key.to_string();
    cfg.ip_whitelist = whitelist;
    cfg.rate_limit_enabled = false; // isolate auth behaviour

    let mgr = Arc::new(Manager::new(Arc::new(cfg.clone()), tmp.path().to_path_buf()));
    let state = AppState {
        cfg: Arc::new(cfg),
        mgr,
        auth: Arc::new(wg_apiserver::AuthState::new(api_key)),
        limiter: Arc::new(wg_apiserver::LoginLimiter::new(60)),
    };
    (router(state), tmp)
}

async fn call(app: &axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body: Value = if text.trim().is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, body)
}

/// A request that appears to come from a PUBLIC address, so the loopback
/// bypass does not apply. `x-forwarded-for` is honoured by `client_ip`.
fn from_public(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-forwarded-for", "203.0.113.9")
        .body(Body::empty())
        .unwrap()
}

/// The same, with an api key header.
fn from_public_with_key(path: &str, key: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-forwarded-for", "203.0.113.9")
        .header("x-api-key", key)
        .body(Body::empty())
        .unwrap()
}

fn get(path: &str) -> Request<Body> {
    Request::builder().uri(path).body(Body::empty()).unwrap()
}

fn get_with_key(path: &str, key: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header("x-api-key", key)
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, body: Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

// ------------------------------------------------------------------ auth

/// The most important test here: without a key, everything is 401.
#[tokio::test]
async fn unauthenticated_requests_are_401() {
    let (app, _t) = app_with("secret", vec![]).await;
    for path in [
        "/api/stats",
        "/api/containers",
        "/api/config",
        "/api/pool/status",
        "/api/pool/acquire",
        "/api/proxies",
        "/api/usage",
        "/api/check",
    ] {
        let (status, body) = call(&app, from_public(path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must require auth");
        assert_eq!(body["code"], "A0001", "{path}");
    }
}

#[tokio::test]
async fn a_wrong_key_is_401() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, _) = call(&app, from_public_with_key("/api/stats", "wrong")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_right_key_passes() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, get_with_key("/api/stats", "secret")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["code"], "00000");
}

/// A whitelisted source bypasses the key entirely — this is how the console
/// runs on the server itself.
#[tokio::test]
async fn loopback_bypasses_the_key() {
    // The test client connects from 127.0.0.1, so the loopback rule applies.
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, _) = call(&app, get("/api/stats")).await;
    assert_eq!(status, StatusCode::OK, "loopback must bypass auth");
}

#[tokio::test]
async fn whitelisted_ip_bypasses_the_key() {
    // Explicitly whitelist loopback so the rule is exercised, not assumed.
    let (app, _t) = app_with("secret", vec!["127.0.0.1".into()]).await;
    let (status, _) = call(&app, get("/api/stats")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn login_mints_a_usable_token() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, post_json("/api/login", json!({"key": "secret"}))).await;
    assert_eq!(status, StatusCode::OK);
    let token = body["data"]["token"].as_str().unwrap().to_string();
    assert!(!token.is_empty());

    let req = Request::builder()
        .uri("/api/stats")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let (status, _) = call(&app, req).await;
    assert_eq!(status, StatusCode::OK, "the minted token must work");
}

#[tokio::test]
async fn a_token_from_another_key_is_rejected() {
    let other = wg_apiserver::AuthState::new("different-key");
    let token = other.sign();
    let (app, _t) = app_with("secret", vec![]).await;
    let req = Request::builder()
        .uri("/api/stats")
        .header("x-forwarded-for", "203.0.113.9")
        .header("authorization", format!("Bearer {}", token))
        .body(Body::empty())
        .unwrap();
    let (status, _) = call(&app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn login_with_a_wrong_key_is_401() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, post_json("/api/login", json!({"key": "nope"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "A0001");
}

#[tokio::test]
async fn five_failed_logins_lock_the_source() {
    let (app, _t) = app_with("secret", vec![]).await;
    for _ in 0..5 {
        let _ = call(&app, post_json("/api/login", json!({"key": "bad"}))).await;
    }
    // Even the CORRECT key is now refused for this source.
    let (status, body) = call(&app, post_json("/api/login", json!({"key": "secret"}))).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        body["msg"].as_str().unwrap_or("").contains("60 秒"),
        "must explain the lockout: {body}"
    );
}

// ------------------------------------------------------------- endpoints

#[tokio::test]
async fn stats_reports_the_pool_shape() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, get_with_key("/api/stats", "secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["mode"], "embedded");
    assert_eq!(body["data"]["transport"], "masque");
    assert!(body["data"]["instance_count"].is_number());
}

#[tokio::test]
async fn containers_is_an_empty_array_not_null() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (_, body) = call(&app, get_with_key("/api/containers", "secret")).await;
    assert!(
        body["data"].is_array(),
        "an empty pool must serialise as []: {body}"
    );
}

#[tokio::test]
async fn config_masks_the_secrets() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (_, body) = call(&app, get_with_key("/api/config", "secret")).await;
    let key = body["data"]["api_key"].as_str().unwrap();
    assert_ne!(key, "secret", "the api key must be masked");
    assert!(key.contains("****"), "got: {key}");
}

#[tokio::test]
async fn pool_status_counts_instances() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, get_with_key("/api/pool/status", "secret")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["total_instances"], 0);
    assert_eq!(body["data"]["available"], 0);
}

#[tokio::test]
async fn acquire_with_no_instances_is_a_business_error() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, get_with_key("/api/pool/acquire", "secret")).await;
    // Business errors keep HTTP 200 and carry the code.
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "P0001", "got: {body}");
}

#[tokio::test]
async fn release_an_unknown_session_is_s0001() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/pool/release")
            .header("x-api-key", "secret")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&json!({"session_id":"nope"})).unwrap()))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "S0001", "got: {body}");
}

#[tokio::test]
async fn unknown_api_path_is_404() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(&app, get_with_key("/api/nope", "secret")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "I0002");
}

#[tokio::test]
async fn proxies_with_an_empty_pool_is_a_business_error() {
    // The Node version answers `P0001` rather than an empty body, so a caller
    // can tell "no proxies right now" from "the API broke".
    let (app, _t) = app_with("secret", vec![]).await;
    let resp = app
        .oneshot(get_with_key("/api/proxies?num=3", "secret"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["code"], "P0001");
}

/// With a live instance the text endpoint must emit a dialable URL that
/// carries the credentials — callers have no other channel to learn them.
#[tokio::test]
async fn proxies_text_carries_credentials_when_auth_is_on() {
    use wg_config::Config;
    use wg_pool::Manager;
    use wg_store::UsqueConfig;

    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.api_key = "secret".into();
    cfg.ip_whitelist = Vec::new(); // credential mode
    cfg.proxy_auth_enabled = true;
    cfg.proxy_username = "warpuser".into();
    cfg.proxy_password = "s3cret".into();
    cfg.rate_limit_enabled = false;
    cfg.base_port = 40010;

    let mgr = Arc::new(Manager::new(Arc::new(cfg.clone()), tmp.path().to_path_buf()));
    let dir = wg_store::dir(tmp.path(), 0);
    std::fs::create_dir_all(&dir).unwrap();
    UsqueConfig {
        private_key: "MHcCAQEEIAh6tuhEPefwuymbW/QQC0mgPIC/U3jzUPkPzpD8/ACEoAoGCCqGSM49AwEHoUQDQgAEhgRduQGUGankFE7brTffrw+57jh1nF4NnkfYjVx1ck2XTtc6xcrP5Mc2/mEg5zVdaHf5YB2JnUr7B0q7aQkLoA==".into(),
        endpoint_v4: "162.159.198.2".into(),
        endpoint_v6: String::new(),
        endpoint_h2_v4: "162.159.198.2".into(),
        endpoint_h2_v6: String::new(),
        endpoint_pub_key: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEIaU7MToJm9NKp8YfGxR6r+/h4mcG\n7SxI8tsW8OR1A5tv/zCzVbCRRh2t87/kxnP6lAy0lkr7qYwu+ox+k3dr6w==\n-----END PUBLIC KEY-----\n".into(),
        id: "t".into(),
        access_token: "t".into(),
        ipv4: "172.16.0.2/32".into(),
        ipv6: "2606:4700:110::1/128".into(),
    }
    .save(&wg_store::paths::usque_conf_path(&dir))
    .unwrap();
    mgr.start_with_config(0, UsqueConfig::read(&dir).unwrap().unwrap(), false)
        .await
        .unwrap();

    let state = AppState {
        cfg: Arc::new(cfg),
        mgr,
        auth: Arc::new(wg_apiserver::AuthState::new("secret")),
        limiter: Arc::new(wg_apiserver::LoginLimiter::new(60)),
    };
    let app = router(state);

    let resp = app
        .oneshot(get_with_key("/api/proxies?num=1", "secret"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "text/plain; charset=utf-8"
    );
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        text.starts_with("socks5://warpuser:s3cret@"),
        "credentials must be in the URL, got: {text}"
    );
    assert!(text.contains(":40010"), "must point at the instance port: {text}");
}

#[tokio::test]
async fn job_status_is_completed() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (_, body) = call(&app, get_with_key("/api/job/abc123", "secret")).await;
    assert_eq!(body["data"]["status"], "completed");
}

#[tokio::test]
async fn whitelist_roundtrip() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (_, body) = call(&app, get_with_key("/api/whitelist", "secret")).await;
    assert!(body["data"]["whitelist"].is_array());

    let (status, body) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/whitelist")
            .header("x-api-key", "secret")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({"whitelist": ["1.2.3.4"]})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["whitelist"][0], "1.2.3.4");
}

#[tokio::test]
async fn a_bad_whitelist_entry_is_a_400() {
    let (app, _t) = app_with("secret", vec![]).await;
    let (status, body) = call(
        &app,
        Request::builder()
            .method("POST")
            .uri("/api/whitelist")
            .header("x-api-key", "secret")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&json!({"whitelist": ["not-an-ip"]})).unwrap(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "business error keeps 200");
    assert_eq!(body["code"], "I0001");
}

// ------------------------------------------------------------- rate limit

#[tokio::test]
async fn rate_limit_engages_when_enabled() {
    let tmp = tempfile::tempdir().unwrap();
    let mut cfg = Config::default();
    cfg.api_key = "secret".into();
    cfg.rate_limit_enabled = true;
    cfg.rate_limit_per_minute = 3;

    let mgr = Arc::new(Manager::new(Arc::new(cfg.clone()), tmp.path().to_path_buf()));
    let state = AppState {
        cfg: Arc::new(cfg),
        mgr,
        auth: Arc::new(wg_apiserver::AuthState::new("secret")),
        limiter: Arc::new(wg_apiserver::LoginLimiter::new(3)),
    };
    let app = router(state);

    let mut saw_limit = false;
    for _ in 0..6 {
        let (status, _) = call(&app, get_with_key("/api/stats", "secret")).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            saw_limit = true;
            break;
        }
    }
    assert!(saw_limit, "the 4th request in a minute must be limited");
}
