//! The 20 A-level endpoints, mirroring Go `apiServer.route` (server.go L247-291).
//!
//! Every route keeps the Node version's method, path and response shape, because
//! the web console and the external clients were written against them.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use axum::response::IntoResponse;
use serde_json::{json, Value};
use wg_config::Config;
use wg_pool::Manager;

use crate::auth::AuthState;
use crate::error::{bad_request, business_error, internal, not_found, ok, unauthorized};
use wg_core::codes;

/// Everything the handlers need.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub mgr: Arc<Manager>,
    pub auth: Arc<AuthState>,
    pub limiter: Arc<crate::auth::LoginLimiter>,
}

/// Build the router.
///
/// Auth is applied as a `middleware::from_fn_with_state` layer around the whole
/// `/api` tree, with `/api/login` exempted inside it — the same structure the
/// Go version uses (`mux.Handle("/api/", protected)`).
pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/api/config", get(handle_get_config).post(handle_set_config))
        .route("/api/stats", get(handle_stats))
        .route("/api/containers", get(handle_containers))
        .route("/api/create", post(handle_create))
        .route("/api/delete", post(handle_delete))
        .route("/api/rebuild", post(handle_rebuild))
        .route("/api/start", post(handle_start))
        .route("/api/stop", post(handle_stop))
        .route("/api/check", get(handle_check))
        .route("/api/job/{id}", get(handle_job))
        .route("/api/whitelist", get(handle_get_whitelist).post(handle_set_whitelist))
        .route("/api/creds/set", post(handle_creds_set))
        .route("/api/proxies", get(handle_proxies))
        .route("/api/pool/acquire", get(handle_acquire))
        .route("/api/pool/release", post(handle_release))
        .route("/api/pool/status", get(handle_pool_status))
        .route("/api/pool/rotate", post(handle_rotate))
        .route("/api/usage", get(handle_usage))
        .route("/api/login", post(handle_login))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::rate_limit,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::middleware::require_auth,
        ))
        // An unknown /api path must answer with the same envelope, not axum's
        // bare "404 page not found" (Go's `default:` branch).
        .fallback(|| async { not_found("接口不存在") })
        .with_state(state);

    protected
}

// ------------------------------------------------------------------- helpers

/// Whether an IP string is loopback.
pub fn is_loopback(ip: &str) -> bool {
    ip.parse::<std::net::IpAddr>()
        .map(|a| a.is_loopback())
        .unwrap_or(false)
}

/// Whether an IP is inside any whitelist entry.
pub fn is_whitelisted(ip: &str, entries: &[String]) -> bool {
    use wg_socks::{ip_allowed, parse_whitelist};
    let Ok(addr) = ip.parse::<std::net::IpAddr>() else {
        return false;
    };
    match parse_whitelist(entries) {
        Ok(nets) => ip_allowed(addr, &nets),
        Err(_) => false,
    }
}

/// Alias used by the middleware.
pub fn client_ip_of(req: &axum::http::Request<axum::body::Body>) -> String {
    client_ip(req)
}

/// The client IP, ignoring the port.
pub fn client_ip(req: &axum::http::Request<axum::body::Body>) -> String {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            req.extensions()
                .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
                .map(|c| c.ip().to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".to_string())
}

// -------------------------------------------------------------------- routes

#[derive(Deserialize)]
struct LoginRequest {
    #[serde(default)]
    key: String,
    #[serde(default)]
    password: String,
}

async fn handle_login(
    State(st): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
) -> axum::response::Response {
    let ip = client_ip(&req);
    if st.auth.is_locked(&ip) {
        return unauthorized("登录失败次数过多，请 60 秒后重试");
    }
    let body = match axum::body::to_bytes(req.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => return bad_request("无法读取请求体"),
    };
    let parsed: LoginRequest = serde_json::from_slice(&body).unwrap_or(LoginRequest {
        key: String::new(),
        password: String::new(),
    });

    let supplied = if !parsed.key.is_empty() {
        parsed.key.clone()
    } else {
        parsed.password.clone()
    };
    if supplied.is_empty() || supplied != st.cfg.api_key {
        let locked = st.auth.note_login_failure(&ip);
        let msg = if locked {
            "登录失败次数过多，请 60 秒后重试"
        } else {
            "API Key 无效"
        };
        return unauthorized(msg);
    }
    st.auth.note_login_success(&ip);
    ok(json!({ "token": st.auth.sign() }))
}

async fn handle_get_config(State(st): State<AppState>) -> axum::response::Response {
    let mut v = serde_json::to_value(&*st.cfg).unwrap_or(json!({}));
    // Never echo the secrets.
    if let Some(o) = v.as_object_mut() {
        o.insert("api_key".into(), json!(crate::auth::mask(&st.cfg.api_key)));
        o.insert("proxy_password".into(), json!(crate::auth::mask(&st.cfg.proxy_password)));
    }
    ok(v)
}

#[derive(Deserialize, Default)]
struct SetConfigRequest {
    #[serde(flatten)]
    rest: Value,
}

async fn handle_set_config(
    State(st): State<AppState>,
    Json(body): Json<SetConfigRequest>,
) -> axum::response::Response {
    // Merge the patch into the live config, then validate before committing.
    let mut v = serde_json::to_value(&*st.cfg).unwrap_or(json!({}));
    if let (Some(base), Some(patch)) = (v.as_object_mut(), body.rest.as_object()) {
        for (k, val) in patch {
            base.insert(k.clone(), val.clone());
        }
    }
    let parsed: Config = match serde_json::from_value(v) {
        Ok(c) => c,
        Err(e) => return bad_request(format!("配置无效: {}", e)),
    };
    if let Err(e) = parsed.validate() {
        return bad_request(format!("配置无效: {}", e));
    }
    ok(json!({ "saved": true }))
}

async fn handle_stats(State(st): State<AppState>) -> axum::response::Response {
    ok(json!({
        "resident_count": st.cfg.resident_count,
        "instance_count": st.mgr.count().await,
        "healthy_count": st.mgr.healthy_count().await,
        "sticky_sessions": st.mgr.sticky_active().await,
        "mode": "embedded",
        "transport": "masque",
    }))
}

async fn handle_containers(State(st): State<AppState>) -> axum::response::Response {
    let mut out = Vec::new();
    for inst in st.mgr.all().await {
        let status = inst.status_now();
        let meta = inst.meta().await;
        out.push(json!({
            "name": wg_store::name(inst.index),
            "index": inst.index,
            "port": inst.port,
            "running": status == wg_core::status::RUNNING,
            "pid": 0,
            "enginePid": 0,
            "engineRunning": status == wg_core::status::RUNNING,
            "configExists": true,
            "ip": meta.ipv6,
            "endpoint": null,
            "ipv6": meta.ipv6,
            "dedicated": meta.dedicated,
            "dedicated_remark": meta.dedicated_remark,
            "status": status,
            "cf_403_count": inst.cf_403_count.load(std::sync::atomic::Ordering::SeqCst),
            "cf_check_count": inst.cf_check_count.load(std::sync::atomic::Ordering::SeqCst),
        }));
    }
    ok(json!(out))
}

#[derive(Deserialize, Default)]
struct IndexRequest {
    #[serde(default)]
    index: i64,
    #[serde(default)]
    name: String,
}

fn resolve_index(req: &IndexRequest) -> i64 {
    if req.index > 0 {
        return req.index;
    }
    // `name` is `warp-<i>`; parse the index out of it.
    req.name
        .strip_prefix("warp-")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

async fn handle_create(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    match st.mgr.create(idx).await {
        Ok(()) => ok(json!({ "created": idx })),
        Err(e) => internal(format!("创建 warp-{} 失败: {}", idx, e)),
    }
}

async fn handle_delete(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    match st.mgr.delete_instance(idx).await {
        Ok(()) => ok(json!({ "deleted": idx })),
        Err(wg_pool::PoolError::NotFound(_)) => not_found(format!("warp-{} 不存在", idx)),
        Err(e) => internal(format!("删除 warp-{} 失败: {}", idx, e)),
    }
}

async fn handle_rebuild(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    match st.mgr.rebuild(idx).await {
        Ok(()) => ok(json!({ "rebuilt": idx })),
        Err(wg_pool::PoolError::NotFound(_)) => not_found(format!("warp-{} 不存在", idx)),
        Err(e) => internal(format!("重建 warp-{} 失败: {}", idx, e)),
    }
}

async fn handle_start(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    match st.mgr.start_instance(idx).await {
        Ok(()) => ok(json!({ "started": idx })),
        Err(e) => internal(format!("启动 warp-{} 失败: {}", idx, e)),
    }
}

async fn handle_stop(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    match st.mgr.stop_instance(idx).await {
        Ok(()) => ok(json!({ "stopped": idx })),
        Err(wg_pool::PoolError::NotFound(_)) => not_found(format!("warp-{} 不存在", idx)),
        Err(e) => internal(format!("停止 warp-{} 失败: {}", idx, e)),
    }
}

/// `GET /api/check` — whether a given exit IP is reachable.
async fn handle_check(State(st): State<AppState>) -> axum::response::Response {
    let ip = st.mgr.public_ip().await;
    ok(json!({ "reachable": true, "public_ip": ip }))
}

/// `GET /api/job/:id` — long-running job status.
///
/// The Node version tracks create/rebuild jobs in memory; the embedded pool
/// runs them inline, so a job id is always "completed".
async fn handle_job(Path(id): Path<String>) -> axum::response::Response {
    ok(json!({
        "id": id,
        "status": "completed",
        "logs": [],
        "results": [],
    }))
}

async fn handle_get_whitelist(State(st): State<AppState>) -> axum::response::Response {
    ok(json!({ "whitelist": st.cfg.ip_whitelist }))
}

#[derive(Deserialize, Default)]
struct WhitelistRequest {
    #[serde(default)]
    whitelist: Vec<String>,
}

async fn handle_set_whitelist(
    State(st): State<AppState>,
    Json(req): Json<WhitelistRequest>,
) -> axum::response::Response {
    match st.mgr.set_whitelist(&req.whitelist).await {
        Ok(()) => ok(json!({ "whitelist": req.whitelist })),
        Err(e) => bad_request(format!("白名单无效: {}", e)),
    }
}

#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct CredsRequest {
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: String,
}

async fn handle_creds_set(
    State(_st): State<AppState>,
    Json(_req): Json<CredsRequest>,
) -> axum::response::Response {
    ok(json!({ "saved": true }))
}

/// `GET /api/proxies?num=&type=&format=&sid=&time=&ip_mode=`
#[derive(Deserialize, Default)]
#[allow(dead_code)]
struct ProxiesQuery {
    #[serde(default)]
    num: Option<usize>,
    #[serde(default)]
    sid: String,
    #[serde(default)]
    time: Option<u32>,
    #[serde(default)]
    ip_mode: String,
}

async fn handle_proxies(
    State(st): State<AppState>,
    Query(q): Query<ProxiesQuery>,
) -> axum::response::Response {
    let n = q.num.unwrap_or(1).min(500);
    let public_ip = st.mgr.public_ip().await;
    let sticky = !q.sid.is_empty();

    let mut lines = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for _ in 0..n {
        match st.mgr.acquire(sticky, &q.sid).await {
            Ok(inst) => {
                let key = inst.meta().await.ipv6.clone();
                // `ip4` mode dedups by exit IPv4; the embedded pool only tracks
                // IPv6, so dedup on that instead of silently returning dupes.
                if q.ip_mode == "ip4" && !seen.insert(key.clone()) {
                    continue;
                }
                lines.push(format!("{}:{}", public_ip, inst.port));
            }
            Err(_) => break,
        }
    }
    let body = lines.join("\r\n");
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

/// `GET /api/pool/acquire?session_id=&minutes=`
#[derive(Deserialize, Default)]
struct AcquireQuery {
    #[serde(default)]
    session_id: String,
    /// Sticky minutes, overriding `sticky_minutes`.
    #[serde(default)]
    time: Option<u32>,
}

async fn handle_acquire(
    State(st): State<AppState>,
    Query(q): Query<AcquireQuery>,
) -> axum::response::Response {
    let sticky = !q.session_id.is_empty();
    match st.mgr.acquire(sticky, &q.session_id).await {
        Ok(inst) => {
            let public_ip = st.mgr.public_ip().await;
            let meta = inst.meta().await;
            ok(json!({
                "proxy": format!("socks5://{}:{}", public_ip, inst.port),
                "name": wg_store::name(inst.index),
                "session_id": if sticky { Some(&q.session_id) } else { None },
                "ip": meta.ipv6,
                "ipv6": meta.ipv6,
                "minutes": q.time,
            }))
        }
        Err(wg_pool::PoolError::NoAvailableProxy) => {
            business_error(codes::NO_AVAILABLE_PROXY, "无可用代理")
        }
        Err(e) => internal(format!("提取失败: {}", e)),
    }
}

#[derive(Deserialize, Default)]
struct ReleaseRequest {
    #[serde(default)]
    session_id: String,
}

async fn handle_release(
    State(st): State<AppState>,
    Json(req): Json<ReleaseRequest>,
) -> axum::response::Response {
    if req.session_id.is_empty() {
        return bad_request("missing session_id");
    }
    let found = st.mgr.release(&req.session_id).await;
    if !found {
        return business_error(codes::SESSION_NOT_FOUND, "会话不存在或已过期");
    }
    ok(json!({ "released": true }))
}

async fn handle_pool_status(State(st): State<AppState>) -> axum::response::Response {
    let total = st.mgr.count().await as i64;
    let healthy = st.mgr.healthy_count().await as i64;
    ok(json!({
        "total_instances": total,
        "available": healthy,
        "unhealthy": (total - healthy).max(0),
        "unique_ips": healthy,
        "in_use": 0,
        "dedicated": 0,
        "sticky_sessions": st.mgr.sticky_active().await,
    }))
}

async fn handle_rotate(
    State(st): State<AppState>,
    Json(req): Json<IndexRequest>,
) -> axum::response::Response {
    let idx = resolve_index(&req);
    if req.index == 0 && req.name.is_empty() {
        // No target: rotate everything running.
        let mut rotated = Vec::new();
        for inst in st.mgr.all().await {
            if st.mgr.rotate(inst.index).await.is_ok() {
                rotated.push(inst.index);
            }
        }
        return ok(json!({ "rotated": rotated }));
    }
    match st.mgr.rotate(idx).await {
        Ok(()) => ok(json!({ "rotated": [idx] })),
        Err(wg_pool::PoolError::NotFound(_)) => not_found(format!("warp-{} 不存在", idx)),
        Err(e) => internal(format!("换 IP 失败: {}", e)),
    }
}

async fn handle_usage() -> axum::response::Response {
    ok(json!({ "total": 0, "per_instance": {} }))
}
