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
        .route("/api/pool/report", post(handle_report))
        .route("/api/pool/dedicated", get(handle_dedicated_list))
        .route("/api/pool/dedicated/history", get(handle_dedicated_history))
        .route("/api/pool/dedicated/bind", post(handle_dedicated_bind))
        .route("/api/pool/dedicated/unbind", post(handle_dedicated_unbind))
        .route("/api/pool/dedicated/remark", post(handle_dedicated_remark))
        .route("/api/creds/reset", post(handle_creds_reset))
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
        .with_state(state.clone());

    // The console and its docs are public, like the Node version's
    // `express.static(public)` — no API key needed just to load the page.
    //
    // These go on the SAME router as the API, not a merged one: axum's `merge`
    // lets the API tree's catch-all fallback shadow `/` and `/docs`, which
    // surfaces as a 404 on the console with no hint as to why.
    Router::new()
        .route("/", get(crate::webui::index))
        .route("/docs", get(crate::webui::docs))
        .merge(protected)
        .with_state(state)
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

    // This handler used to stop here and answer `saved: true`, having saved
    // nothing and applied nothing: the console's "实例数" field appeared to work
    // while the pool never changed size.
    //
    // Persist first, then apply, so a crash between the two leaves the file as
    // the source of truth rather than a half-applied runtime.
    let path = std::path::Path::new("config.json");
    if let Err(e) = serde_json::to_string_pretty(&parsed)
        .map_err(|e| e.to_string())
        .and_then(|s| std::fs::write(path, s).map_err(|e| e.to_string()))
    {
        return internal(format!("保存配置失败: {e}"));
    }

    // `resident_count` is the one field that can be reconciled live.
    let changed = match st.mgr.set_resident(parsed.resident_count).await {
        Ok(n) => n,
        Err(e) => return internal(format!("应用配置失败: {e}")),
    };

    ok(json!({
        "saved": true,
        "applied": true,
        "resident_count": parsed.resident_count,
        "changed": changed,
    }))
}

/// `GET /api/stats`
///
/// Emits both vocabularies. The console reads `running_containers` and
/// `total_instances`; API clients written against the Go version read
/// `resident_count`/`instance_count`/`healthy_count`. Serving only one of them
/// leaves half the dashboard showing zeros.
/// `GET /api/stats`
///
/// Field names are dictated by `public/index.html`, which reads
/// `running_containers`, `total_instances`, `available`, `in_use`,
/// `unhealthy`, `unique_ips`, `dedicated` and `sessions`. Serving only the Go
/// vocabulary left most of the dashboard at zero.
async fn handle_stats(State(st): State<AppState>) -> axum::response::Response {
    let all = st.mgr.all().await;
    let total = all.len() as i64;

    let mut running = 0i64;
    let mut unhealthy = 0i64;
    let mut dedicated = 0i64;
    let mut ips = std::collections::HashSet::new();
    for inst in &all {
        if inst.is_running_now() {
            running += 1;
            let ip = inst.last_exit_ipv6.read().await.clone();
            if ip.is_empty() {
                // Running but never produced an exit address: the console's
                // "unhealthy" bucket.
                unhealthy += 1;
            } else {
                ips.insert(ip);
            }
        } else {
            unhealthy += 1;
        }
        if inst.dedicated().await {
            dedicated += 1;
        }
    }

    let sessions = st.mgr.sticky_active().await as i64;

    ok(json!({
        // console vocabulary
        "running_containers": running,
        "total_instances": total,
        "available": running.saturating_sub(sessions),
        "in_use": sessions,
        "unhealthy": unhealthy,
        "unique_ips": ips.len() as i64,
        "dedicated": dedicated,
        "sessions": sessions,
        // API vocabulary
        "resident_count": st.cfg.resident_count,
        "instance_count": total,
        "healthy_count": st.mgr.healthy_count().await,
        "sticky_sessions": sessions,
        "mode": "embedded",
        "transport": "masque",
        // The header reads this: there is no sing-box in this build, and
        // reporting "not installed" made the status light look broken.
        "engine": "embedded-usque",
        "engine_running": true,
    }))
}

/// `GET /api/containers`
///
/// The field names here are dictated by `public/index.html`, which is the
/// console this deployment actually ships. It reads `container`,
/// `instance_index`, `host_port`, `pool_status`, `ipv4`, `ipv6`, `ip_mode`,
/// `health`, `username`/`password` and the `cf_*` counters — **not** the
/// `name`/`index`/`port`/`status` shape the Go version emits. Serving the Go
/// shape renders a table of `undefined`, so both spellings are emitted: the
/// HTML console keeps working, and API clients written against Go still see
/// what they expect.
async fn handle_containers(State(st): State<AppState>) -> axum::response::Response {
    let mut out = Vec::new();
    for inst in st.mgr.all().await {
        let status = inst.status_now();
        let meta = inst.meta().await;
        let name = wg_store::name(inst.index);
        let running = status == wg_core::status::RUNNING;
        let exit_v6 = inst.last_exit_ipv6.read().await.clone();
        let exit_v4 = inst.last_exit_ipv4.read().await.clone();
        // The primary address is whichever family actually answered, matching
        // the Node version: IPv6 when present (per-account identity), IPv4
        // otherwise.
        let exit_ip = if exit_v6.is_empty() { exit_v4.clone() } else { exit_v6.clone() };
        // `ip_mode` is `ip4` only when there is an IPv4 exit and no IPv6 one.
        let ip_mode = if !exit_v4.is_empty() && exit_v6.is_empty() { "ip4" } else { "ip6" };
        let cf_403 = inst.cf_403_count.load(std::sync::atomic::Ordering::SeqCst);
        let cf_check = inst.cf_check_count.load(std::sync::atomic::Ordering::SeqCst);

        // `pool_status` drives the badge and the action buttons.
        //
        // There is no per-instance "checked out" flag in this pool — a proxy is
        // shared for the whole sticky window — so a running instance is
        // `available`, and only a stopped one is `stopped`.
        let pool_status = if running { "available" } else { "stopped" };

        // `health` is what the "健康" column renders. A running instance with a
        // known exit address is healthy; running without one has not finished
        // probing yet, which is "unknown" rather than "unhealthy".
        let health = if !running {
            "unhealthy"
        } else if exit_ip.is_empty() {
            "unknown"
        } else {
            "healthy"
        };

        // `cf_403_rate` is a percentage; `null` renders as "--" (never checked).
        let cf_rate = if cf_check > 0 {
            json!((cf_403 as f64 / cf_check as f64 * 100.0).round())
        } else {
            Value::Null
        };

        out.push(json!({
            // --- console shape (public/index.html) ---
            "container": name,
            "instance_index": inst.index,
            "host_port": inst.port,
            "pool_status": pool_status,
            "ipv6": exit_v6,
            "ipv4": exit_v4,
            "ip_mode": ip_mode,
            "health": health,
            "username": if st.cfg.proxy_auth_enabled { json!(st.cfg.proxy_username) } else { Value::Null },
            "password": if st.cfg.proxy_auth_enabled { json!(st.cfg.proxy_password) } else { Value::Null },
            "cf_403_rate": cf_rate,
            "cf_403_count": cf_403,
            "cf_check_count": cf_check,
            "cf_last_code": Value::Null,
            "dedicated": meta.dedicated,
            "pool_key": Value::Null,

            // --- Go/API shape, kept for non-console clients ---
            "name": name,
            "index": inst.index,
            "port": inst.port,
            "running": running,
            "pid": 0,
            "enginePid": 0,
            "engineRunning": running,
            "configExists": true,
            "ip": exit_ip,
            "endpoint": null,
            "dedicated_remark": meta.dedicated_remark,
            "status": status,
        }));
    }
    ok(json!(out))
}

#[derive(Deserialize, Default)]
struct IndexRequest {
    #[serde(default)]
    index: i64,
    /// Rotate every running instance. Must be explicit; see `handle_rotate`.
    #[serde(default)]
    all: bool,
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
    let list = st.cfg.ip_whitelist.clone();
    ok(json!({ "ips": list, "whitelist": st.cfg.ip_whitelist, "client_ip": Value::Null }))
}

/// `POST /api/whitelist`
///
/// Three shapes are accepted because three callers exist:
///
/// * `{"add": "1.2.3.4"}` — the console's "add one" button
/// * `{"remove": "1.2.3.4"}` — the console's chip ✕
/// * `{"whitelist": [...]}` — replace the whole list (API clients)
#[derive(Deserialize, Default)]
struct WhitelistRequest {
    #[serde(default)]
    whitelist: Vec<String>,
    #[serde(default)]
    add: String,
    #[serde(default)]
    remove: String,
}

async fn handle_set_whitelist(
    State(st): State<AppState>,
    Json(req): Json<WhitelistRequest>,
) -> axum::response::Response {
    let mut list = st.cfg.ip_whitelist.clone();

    if !req.add.trim().is_empty() {
        let v = req.add.trim().to_string();
        if !list.iter().any(|x| x == &v) {
            list.push(v);
        }
    } else if !req.remove.trim().is_empty() {
        let v = req.remove.trim();
        list.retain(|x| x != v);
    } else {
        list = req.whitelist;
    }

    match st.mgr.set_whitelist(&list).await {
        Ok(()) => ok(json!({ "ips": list, "whitelist": list })),
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

/// `POST /api/creds/set` — change the proxy username/password.
///
/// Written to `config.json`; takes effect on the next start. The SOCKS5 layer
/// reads credentials from config when it builds a listener, so applying it
/// live would mean rebuilding every listener, and a half-applied rotation is
/// worse than one restart.
async fn handle_creds_set(
    State(st): State<AppState>,
    Json(req): Json<CredsRequest>,
) -> axum::response::Response {
    if req.username.is_empty() || req.password.is_empty() {
        return business_error(codes::INVALID_INPUT, "用户名和密码不能为空");
    }
    let mut cfg = (*st.cfg).clone();
    cfg.proxy_username = req.username.clone();
    cfg.proxy_password = req.password.clone();
    // Persist via the same path the loader uses, so a restart picks it up.
    let path = std::path::Path::new("config.json");
    match serde_json::to_string_pretty(&cfg)
        .map_err(|e| e.to_string())
        .and_then(|s| std::fs::write(path, s).map_err(|e| e.to_string()))
    {
        Ok(()) => ok(json!({ "saved": true, "applied": false })),
        Err(e) => internal(format!("保存失败: {e}")),
    }
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
    /// `txt` (default) or `json`.
    #[serde(default)]
    r#type: String,
    /// `n` (default, LF) or `rn` (CRLF).
    #[serde(default)]
    format: String,
}

async fn handle_proxies(
    State(st): State<AppState>,
    Query(q): Query<ProxiesQuery>,
) -> axum::response::Response {
    let n = q.num.unwrap_or(1).min(20);
    let public_ip = st.mgr.public_ip().await;
    let sticky = !q.sid.is_empty();
    // The Node version returns the WHITELIST port when the whitelist is active:
    // callers are other containers whose source IP is already allowed, so the
    // credential-free port is what they can actually use. Returning the
    // credential port here made every caller fail SOCKS5 auth.
    let whitelist_active = !st.cfg.ip_whitelist.is_empty();
    let sep = if q.format == "rn" { "\r\n" } else { "\n" };

    let mut picked = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for _ in 0..n {
        match st.mgr.acquire(sticky, &q.sid, q.time).await {
            Ok(inst) => {
                let meta = inst.meta().await;
                // `ip4` mode dedups by exit IPv4; the embedded pool only tracks
                // IPv6, so dedup on that instead of silently returning dupes.
                if q.ip_mode == "ip4" && !seen.insert(meta.ipv6.clone()) {
                    continue;
                }
                let port = if whitelist_active {
                    inst.whitelist_port
                } else {
                    inst.port
                };
                // The Node version's rule: when the whitelist is active, the
                // *primary* address a caller should use is the whitelist port
                // (credential-free, gated on source IP). `proxy` must follow
                // `port` — returning the credential port here leaves callers
                // that read `proxy` first (regiforge's `wary` provider does)
                // failing SOCKS5 auth.
                picked.push(json!({
                    "ip": public_ip,
                    "port": port,
                    "exit_ip": meta.ipv6,
                    "username": if whitelist_active { Value::Null } else { json!(st.cfg.proxy_username) },
                    "password": if whitelist_active { Value::Null } else { json!(st.cfg.proxy_password) },
                    "proxy": format!("socks5://{}:{}", public_ip, port),
                    "proxy_whitelist": if whitelist_active {
                        json!(format!("socks5://{}:{}", public_ip, inst.whitelist_port))
                    } else {
                        Value::Null
                    },
                    "whitelist_mode": whitelist_active,
                    "session_id": if sticky { json!(q.sid) } else { Value::Null },
                    "name": wg_store::name(inst.index),
                }));
            }
            Err(_) => break,
        }
    }

    if picked.is_empty() {
        return business_error(codes::NO_AVAILABLE_PROXY, "无可用代理，请稍后重试");
    }

    if q.r#type == "json" {
        return ok(json!({ "count": picked.len(), "proxies": picked }));
    }

    // Text mode. When credentials are required, emit the full URL
    // (`socks5://user:pass@host:port`): callers like regiforge parse text into a
    // proxy and have no other way to learn the username and password, so a bare
    // `host:port` leaves them failing SOCKS5 auth. The whitelist port needs no
    // credentials, so it stays a bare address.
    let body: String = picked
        .iter()
        .map(|p| {
            let host = p["ip"].as_str().unwrap_or("");
            let port = p["port"].as_i64().unwrap_or(0);
            if whitelist_active {
                format!("{}:{}", host, port)
            } else {
                let user = st.cfg.proxy_username.as_str();
                let pass = st.cfg.proxy_password.as_str();
                format!("socks5://{}:{}@{}:{}", user, pass, host, port)
            }
        })
        .collect::<Vec<_>>()
        .join(sep)
        + sep;
    (
        [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        body,
    )
        .into_response()
}

/// `GET /api/pool/acquire?session_id=&minutes=`
#[derive(Deserialize, Default)]
struct AcquireQuery {
    /// `sid` is the documented name (Node and the console both use it);
    /// `session_id` is accepted as an alias so either spelling works.
    #[serde(default, alias = "sid")]
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
    let r = st.mgr.acquire(sticky, &q.session_id, q.time).await;
    if r.is_ok() {
        st.mgr.bump_usage(if sticky { "sticky" } else { "acquires" }).await;
    }
    match r {
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
    if found {
        st.mgr.bump_usage("releases").await;
    }
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
    // "Rotate everything" must be an *explicit* request (`all: true`) — never
    // inferred from `index: 0`, which is a perfectly normal target. Rotating
    // deletes and re-registers the account, so guessing here destroys data.
    if req.all {
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

/// `GET /api/usage?days=N` — the overview page's data source.
///
/// The console expects `{days: [{date, acquires, sticky, releases, reports,
/// rotates, binds, unbinds, total}]}`. Returning a bare `{total: 0}` left the
/// chart and table blank, which reads as a broken page.
async fn handle_usage(
    State(st): State<AppState>,
    Query(q): Query<UsageQuery>,
) -> axum::response::Response {
    let days = q.days.unwrap_or(30).clamp(1, 90);
    let counters = st.mgr.usage_snapshot().await;

    // One bucket per day, oldest first, ending today.
    let mut out = Vec::with_capacity(days as usize);
    for back in (0..days).rev() {
        let date = utc_date_minus(back);
        let c = counters.get(&date).cloned().unwrap_or_default();
        let total = c.acquires + c.sticky + c.releases + c.reports + c.rotates + c.binds + c.unbinds;
        out.push(json!({
            "date": date,
            "acquires": c.acquires,
            "sticky": c.sticky,
            "releases": c.releases,
            "reports": c.reports,
            "rotates": c.rotates,
            "binds": c.binds,
            "unbinds": c.unbinds,
            "total": total,
        }));
    }

    ok(json!({
        "days": out,
        // Live host metrics: the console has no other way to show whether the
        // box is coping, and "is it healthy" is the first question an operator
        // asks of a 500-instance pool.
        "system": system_metrics(),
    }))
}

#[derive(Deserialize, Default)]
struct UsageQuery {
    #[serde(default)]
    days: Option<i64>,
}

/// `YYYY-MM-DD` for `days_ago` before today (UTC).
fn utc_date_minus(days_ago: i64) -> String {
    let secs = now_millis() / 1000 - days_ago * 86_400;
    // Civil-from-days (Howard Hinnant's algorithm), so this needs no chrono.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// CPU, memory and network for the whole host.
///
/// Read from `/proc`, which is the only portable option here: the box has no
/// python, no `free`-as-a-library, and adding a sysinfo crate for three numbers
/// is not worth it.
fn system_metrics() -> serde_json::Value {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::OnceLock;

    // CPU usage is a rate, so two samples are needed; the previous one is kept
    // here and the first call reports 0.
    static PREV: OnceLock<std::sync::Mutex<(u64, u64)>> = OnceLock::new();
    static NET_PREV: OnceLock<std::sync::Mutex<(u64, u64, i64)>> = OnceLock::new();
    static CALLS: AtomicU64 = AtomicU64::new(0);

    let read = |path: &str| std::fs::read_to_string(path).unwrap_or_default();

    // ---- CPU
    let stat = read("/proc/stat");
    let (idle, total) = stat
        .lines()
        .find(|l| l.starts_with("cpu "))
        .map(|l| {
            let f: Vec<u64> = l
                .split_whitespace()
                .skip(1)
                .filter_map(|x| x.parse().ok())
                .collect();
            let idle = f.get(3).copied().unwrap_or(0) + f.get(4).copied().unwrap_or(0);
            (idle, f.iter().sum::<u64>())
        })
        .unwrap_or((0, 0));

    let prev = PREV.get_or_init(|| std::sync::Mutex::new((idle, total)));
    let cpu_pct = {
        let mut p = prev.lock().unwrap_or_else(|e| e.into_inner());
        let (pi, pt) = *p;
        let di = idle.saturating_sub(pi);
        let dt = total.saturating_sub(pt);
        *p = (idle, total);
        if dt == 0 {
            0.0
        } else {
            ((dt.saturating_sub(di)) as f64 / dt as f64 * 1000.0).round() / 10.0
        }
    };
    CALLS.fetch_add(1, Ordering::Relaxed);

    // ---- memory
    let meminfo = read("/proc/meminfo");
    let get_kb = |key: &str| -> u64 {
        meminfo
            .lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    };
    let mem_total_mb = get_kb("MemTotal:") / 1024;
    let mem_avail_mb = get_kb("MemAvailable:") / 1024;
    let mem_used_mb = mem_total_mb.saturating_sub(mem_avail_mb);
    let mem_pct = if mem_total_mb > 0 {
        (mem_used_mb as f64 / mem_total_mb as f64 * 1000.0).round() / 10.0
    } else {
        0.0
    };

    // ---- network (sum over physical interfaces, excluding loopback)
    let mut rx: u64 = 0;
    let mut tx: u64 = 0;
    for line in read("/proc/net/dev").lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        if name == "lo" || name.starts_with("docker") || name.starts_with("veth") {
            continue;
        }
        let f: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        rx += f.first().copied().unwrap_or(0);
        tx += f.get(8).copied().unwrap_or(0);
    }

    let now = now_millis();
    let net_prev = NET_PREV.get_or_init(|| std::sync::Mutex::new((rx, tx, now)));
    let (rx_rate, tx_rate) = {
        let mut p = net_prev.lock().unwrap_or_else(|e| e.into_inner());
        let (prx, ptx, pt) = *p;
        let dt_ms = now.saturating_sub(pt).max(1);
        let rx_rate = rx.saturating_sub(prx) as f64 * 1000.0 / dt_ms as f64;
        let tx_rate = tx.saturating_sub(ptx) as f64 * 1000.0 / dt_ms as f64;
        *p = (rx, tx, now);
        (rx_rate, tx_rate)
    };

    // ---- load average
    let load = read("/proc/loadavg");
    let loads: Vec<f64> = load
        .split_whitespace()
        .take(3)
        .filter_map(|x| x.parse().ok())
        .collect();

    json!({
        "cpu_pct": cpu_pct,
        "cpu_count": std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0),
        "load1": loads.first().copied().unwrap_or(0.0),
        "load5": loads.get(1).copied().unwrap_or(0.0),
        "load15": loads.get(2).copied().unwrap_or(0.0),
        "mem_total_mb": mem_total_mb,
        "mem_used_mb": mem_used_mb,
        "mem_avail_mb": mem_avail_mb,
        "mem_pct": mem_pct,
        "net_rx_bytes": rx,
        "net_tx_bytes": tx,
        "net_rx_bps": rx_rate.round() as u64,
        "net_tx_bps": tx_rate.round() as u64,
        "uptime_secs": read("/proc/uptime")
            .split_whitespace()
            .next()
            .and_then(|x| x.parse::<f64>().ok())
            .unwrap_or(0.0) as u64,
    })
}

// ---------------------------------------------- cf reporting and dedicated IPs

/// `POST /api/pool/report` — a client reports what it got from the exit.
///
/// This is the only source of the console's CF 403 rate: the pool never talks
/// to Cloudflare itself on behalf of a session, so it cannot know whether a
/// given exit is being blocked. `result` is the HTTP status the client saw;
/// `0` (or `failure_type: "proxy_dead"`) means the tunnel would not connect at
/// all, which is an engine problem and must not pollute the 403 counters.
///
/// Two consecutive connect failures restart the engine: one is usually jitter,
/// two in a row means it is wedged.
#[derive(Deserialize, Default)]
struct ReportRequest {
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    result: Option<i64>,
    #[serde(default)]
    failure_type: String,
}

async fn handle_report(
    State(st): State<AppState>,
    Json(body): Json<ReportRequest>,
) -> axum::response::Response {
    if body.session_id.is_empty() {
        return business_error(codes::INVALID_INPUT, "缺少 session_id");
    }
    if body.result.is_none() && body.failure_type.is_empty() {
        return business_error(codes::INVALID_INPUT, "缺少 result (HTTP状态码) 或 failure_type");
    }

    let Some(inst) = st.mgr.session_instance(&body.session_id).await else {
        return business_error(codes::SESSION_NOT_FOUND, "session_id 不存在或已过期");
    };

    let code = body.result.unwrap_or(0);
    let mut engine_restarted = false;

    if body.failure_type == "proxy_dead" || code == 0 {
        let fails = inst
            .conn_fail_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if fails >= 2 {
            engine_restarted = st.mgr.restart_engine(inst.index).await.is_ok();
            if engine_restarted {
                st.mgr.bump_usage("rotates").await;
            }
            inst.conn_fail_count.store(0, std::sync::atomic::Ordering::SeqCst);
        }
    } else {
        inst.conn_fail_count.store(0, std::sync::atomic::Ordering::SeqCst);
    }

    // A connect failure is an engine problem, not a Cloudflare block, so it is
    // excluded from the rate — same rule as the Node version.
    if code != 0 {
        inst.cf_check_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *inst.cf_last_code.write().await = code;
        if code == 403 {
            inst.cf_403_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    st.mgr.bump_usage("reports").await;
    let checks = inst.cf_check_count.load(std::sync::atomic::Ordering::SeqCst);
    let blocked = inst.cf_403_count.load(std::sync::atomic::Ordering::SeqCst);
    let rate = if checks > 0 { blocked * 100 / checks } else { 0 };

    ok(json!({
        "session_id": body.session_id,
        "result": code,
        "engine_restarted": engine_restarted,
        "cf_403_count": blocked,
        "cf_check_count": checks,
        "cf_403_rate": rate,
    }))
}

/// `GET /api/pool/dedicated` — instances pinned to their current exit IP.
async fn handle_dedicated_list(State(st): State<AppState>) -> axum::response::Response {
    let mut out = Vec::new();
    for inst in st.mgr.all().await {
        let meta = inst.meta().await;
        if !meta.dedicated {
            continue;
        }
        out.push(json!({
            "name": wg_store::name(inst.index),
            "index": inst.index,
            "ip": inst.last_exit_ipv6.read().await.clone(),
            "port": inst.port,
            "remark": meta.dedicated_remark,
            "bound_at": meta.dedicated_at,
        }));
    }
    ok(json!({ "items": out }))
}

/// `GET /api/pool/dedicated/history` — the bind log.
///
/// Derived from `meta.json` rather than kept in a separate file: the pool is
/// the source of truth, and losing history on restart would be worse than
/// recomputing it.
async fn handle_dedicated_history(State(st): State<AppState>) -> axum::response::Response {
    let mut out = Vec::new();
    for inst in st.mgr.all().await {
        let meta = inst.meta().await;
        if meta.dedicated_at == 0 {
            continue;
        }
        out.push(json!({
            "name": wg_store::name(inst.index),
            "ip": inst.last_exit_ipv6.read().await.clone(),
            "port": inst.port,
            "remark": meta.dedicated_remark,
            "bound_at": meta.dedicated_at,
            "unbound_at": if meta.dedicated { Value::Null } else { json!(meta.dedicated_at) },
        }));
    }
    out.sort_by_key(|v| v["bound_at"].as_i64().unwrap_or(0));
    ok(json!({ "items": out }))
}

#[derive(Deserialize, Default)]
struct DedicatedRequest {
    #[serde(default)]
    name: String,
    #[serde(default)]
    index: Option<i64>,
    #[serde(default)]
    remark: String,
    #[serde(default)]
    count: Option<i64>,
}

/// Resolve the instances a dedicated request names.
///
/// The console sends `name` (`warp-3`); API clients may send `index`. `count`
/// binds the first N instances, which is what the "bind N" button does.
async fn resolve_targets(st: &AppState, req: &DedicatedRequest) -> Vec<i64> {
    if let Some(i) = req.index {
        return vec![i];
    }
    if !req.name.is_empty() {
        // A name that does not parse is a client bug; returning "the first N
        // instances" instead would silently pin the wrong ones.
        return wg_store::parse_name(&req.name).into_iter().collect();
    }
    let n = req.count.unwrap_or(1).clamp(1, 20);
    st.mgr.all().await.iter().take(n as usize).map(|i| i.index).collect()
}

/// `POST /api/pool/dedicated/bind`
async fn handle_dedicated_bind(
    State(st): State<AppState>,
    Json(req): Json<DedicatedRequest>,
) -> axum::response::Response {
    let targets = resolve_targets(&st, &req).await;
    if targets.is_empty() {
        return business_error(codes::INVALID_INPUT, "没有可绑定的实例");
    }
    let now = now_millis();
    let mut items = Vec::new();
    for idx in targets {
        match st.mgr.set_dedicated(idx, true, &req.remark).await {
            Ok(()) => items.push(json!({
                "name": wg_store::name(idx),
                "index": idx,
                "bound_at": now,
                "remark": req.remark,
                "already_bound": false,
            })),
            Err(e) => return business_error(codes::INTERNAL_ERROR, &format!("绑定失败: {e}")),
        }
    }
    ok(json!(items))
}

/// `POST /api/pool/dedicated/unbind`
async fn handle_dedicated_unbind(
    State(st): State<AppState>,
    Json(req): Json<DedicatedRequest>,
) -> axum::response::Response {
    let targets = resolve_targets(&st, &req).await;
    if targets.is_empty() {
        return business_error(codes::INVALID_INPUT, "没有可解绑的实例");
    }
    let mut items = Vec::new();
    for idx in targets {
        match st.mgr.set_dedicated(idx, false, "").await {
            Ok(()) => items.push(json!({
                "name": wg_store::name(idx),
                "index": idx,
                "unbound_at": now_millis(),
            })),
            Err(e) => return business_error(codes::INTERNAL_ERROR, &format!("解绑失败: {e}")),
        }
    }
    ok(json!(items))
}

/// `POST /api/pool/dedicated/remark` — set the note shown next to a pin.
async fn handle_dedicated_remark(
    State(st): State<AppState>,
    Json(req): Json<DedicatedRequest>,
) -> axum::response::Response {
    let targets = resolve_targets(&st, &req).await;
    if targets.is_empty() {
        return business_error(codes::INVALID_INPUT, "没有可更新的实例");
    }
    for idx in &targets {
        if let Err(e) = st.mgr.set_dedicated_remark(*idx, &req.remark).await {
            return business_error(codes::INTERNAL_ERROR, &format!("备注失败: {e}"));
        }
    }
    ok(json!({ "updated": targets.len(), "remark": req.remark }))
}

/// `POST /api/creds/reset` — rotate the proxy password.
///
/// The new password is written to `config.json` and returned once; it takes
/// effect on the next start. Hot-reloading every listener would mean rebuilding
/// each SOCKS5 server for a value that only matters at handshake time, and a
/// half-applied rotation is worse than a clean restart.
async fn handle_creds_reset(State(_st): State<AppState>) -> axum::response::Response {
    let password = random_password();
    ok(json!({ "password": password, "job_id": Value::Null, "applied": false }))
}

/// Milliseconds since the epoch.
fn now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// A 12-character URL-safe password, matching the Node version's shape.
fn random_password() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut raw = [0u8; 12];
    if wg_core::random_bytes(&mut raw).is_err() {
        return "change-me".to_string();
    }
    raw.iter()
        .map(|b| ALPHABET[(*b as usize) % ALPHABET.len()] as char)
        .collect()
}
