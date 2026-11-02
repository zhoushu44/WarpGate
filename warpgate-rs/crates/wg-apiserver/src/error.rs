//! Error → HTTP mapping, mirroring Go `writeError` / `writeStoreError`.
//!
//! The envelope is `{code, msg, data}`. Business errors keep HTTP 200; only
//! auth (401), rate limit (429), not-found (404) and internal (500) use a real
//! status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use wg_core::codes;

/// A 200 with `{code, msg, data}`.
pub fn ok<T: serde::Serialize>(data: T) -> Response {
    Json(json!({
        "code": codes::OK,
        "msg": "",
        "data": data,
    }))
    .into_response()
}

/// A business error: HTTP 200 with a non-zero code.
pub fn business_error(code: &'static str, msg: impl Into<String>) -> Response {
    Json(json!({
        "code": code,
        "msg": msg.into(),
        "data": null,
    }))
    .into_response()
}

/// A 401 `A0001`.
pub fn unauthorized(msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "code": codes::UNAUTHORIZED,
            "msg": msg,
            "data": null,
        })),
    )
        .into_response()
}

/// A 404 `I0002`.
pub fn not_found(msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "code": codes::NOT_FOUND,
            "msg": msg,
            "data": null,
        })),
    )
        .into_response()
}

/// A 429 `A0002`.
pub fn rate_limited(msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "code": codes::RATE_LIMITED,
            "msg": msg,
            "data": null,
        })),
    )
        .into_response()
}

/// A 500 `99999`.
pub fn internal(msg: impl Into<String>) -> Response {
    let msg = msg.into();
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "code": codes::INTERNAL_ERROR,
            "msg": msg,
            "data": null,
        })),
    )
        .into_response()
}

/// A 400 `I0001`.
pub fn bad_request(msg: impl Into<String>) -> Response {
    business_error(codes::INVALID_INPUT, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body_of(r: Response) -> String {
        let bytes = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(axum::body::to_bytes(r.into_body(), usize::MAX))
            .unwrap();
        String::from_utf8_lossy(&bytes).to_string()
    }

    #[test]
    fn ok_envelope_shape() {
        let s = body_of(ok(serde_json::json!({"a": 1})));
        assert_eq!(s, r#"{"code":"00000","data":{"a":1},"msg":""}"#);
    }

    #[test]
    fn business_error_keeps_http_200() {
        let r = business_error(codes::NO_AVAILABLE_PROXY, "pool empty");
        assert_eq!(r.status(), StatusCode::OK);
        let s = body_of(r);
        assert!(s.contains(r#""code":"P0001""#), "{s}");
        assert!(s.contains(r#""data":null"#), "{s}");
    }

    #[test]
    fn auth_and_limit_use_real_status_codes() {
        assert_eq!(unauthorized("x").status(), StatusCode::UNAUTHORIZED);
        assert_eq!(rate_limited("x").status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(not_found("x").status(), StatusCode::NOT_FOUND);
        assert_eq!(internal("x").status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(bad_request("x").status(), StatusCode::OK);
    }

    #[test]
    fn codes_are_strings_not_numbers() {
        let s = body_of(business_error(codes::SESSION_NOT_FOUND, "gone"));
        assert!(s.contains(r#""code":"S0001""#), "{s}");
    }
}
