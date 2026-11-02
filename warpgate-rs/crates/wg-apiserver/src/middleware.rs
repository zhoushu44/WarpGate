//! The auth middleware.
//!
//! Applied to the whole `/api` tree with `/api/login` exempted, mirroring Go's
//! `mux.Handle("/api/", protected)`.
//!
//! Precedence, matching Go `apiServer.auth`:
//!
//! 1. whitelisted or loopback source IP → through, no key needed
//! 2. `X-API-Key` matching `config.api_key`
//! 3. `Authorization: Bearer <token>` (HMAC over the api key)
//! 4. `?key=` query parameter (the console's fallback)
//! 5. otherwise **401 `A0001`**
//!
//! A per-source rate limit guards every authenticated route too, so a leaked
//! key cannot be used to flood the pool.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::auth::AuthState;
use crate::error::unauthorized;
use crate::routes::AppState;

/// The exempt path: minting a token obviously cannot require one.
const LOGIN_PATH: &str = "/api/login";

/// The auth layer.
pub async fn require_auth(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if req.uri().path() == LOGIN_PATH {
        return next.run(req).await;
    }

    // 1. Source IP bypass.
    let ip = super::routes::client_ip_of(&req);
    if super::routes::is_loopback(&ip) || super::routes::is_whitelisted(&ip, &st.cfg.ip_whitelist) {
        return next.run(req).await;
    }

    // 4. `?key=` (checked before the headers so a bad header cannot shadow a
    // good query param, matching the Go order).
    if let Some(k) = req.uri().query().and_then(|q| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix("key="))
            .map(|s| s.to_string())
    }) {
        if !k.is_empty() && k == st.cfg.api_key {
            return next.run(req).await;
        }
    }

    // 2. `X-API-Key`.
    if let Some(k) = req
        .headers()
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
    {
        if !k.is_empty() && k == st.cfg.api_key {
            return next.run(req).await;
        }
    }

    // 3. `Authorization: Bearer <token>`.
    if let Some(h) = req
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        if let Some(tok) = h.strip_prefix("Bearer ") {
            if st.auth.verify(tok.trim()) {
                return next.run(req).await;
            }
        }
    }

    let _ = ip;
    unauthorized("来源 IP 不在白名单或未登录")
}

/// Rate limiting for authenticated routes.
///
/// Kept separate from auth so a caller that passes auth still cannot flood the
/// pool; the Go version applies `rateLimit` only to the mutation routes, but
/// applying it everywhere is strictly safer and the limit is per-minute.
pub async fn rate_limit(
    State(st): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    if !st.cfg.rate_limit_enabled {
        return next.run(req).await;
    }
    let ip = super::routes::client_ip_of(&req);
    if st.limiter.allow(&ip) {
        next.run(req).await
    } else {
        crate::error::rate_limited("请求频率超限")
    }
}

/// Convenience: the auth state for a given api key.
pub fn auth_for(api_key: &str) -> Arc<AuthState> {
    Arc::new(AuthState::new(api_key))
}

#[allow(dead_code)]
fn _unused(_: fn() -> Response) {}
