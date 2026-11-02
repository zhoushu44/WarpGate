//! Shared wire-contract types for warpgate-rs.
//!
//! Everything here mirrors a type from the Go implementation
//! (`warpgate-go/internal/...`) or the Node original (`server.js`), so a Go
//! client and a Rust server agree on JSON field-for-field.
//!
//! Two things are load-bearing:
//!
//! 1. **The API envelope.** `{code, msg, data}` with `"00000"` for success.
//!    Business errors keep HTTP 200; only auth (401), rate limit (429) and
//!    internal errors (500) use a real HTTP status.
//! 2. **Instance status strings.** They are written into `meta.json` and read
//!    back by the Node version, so they are `const &str`, not an enum.

use serde::{Deserialize, Serialize};

/// Instance lifecycle states, mirroring the Go `const` block.
pub mod status {
    pub const CREATING: &str = "creating";
    pub const RUNNING: &str = "running";
    pub const UNHEALTHY: &str = "unhealthy";
    pub const STOPPED: &str = "stopped";
    pub const ERROR: &str = "error";
}

/// Default claim lease for a sticky session is NOT here — the pool owns it.

/// The business error codes from `server.js` `ERROR_CODES`.
///
/// These are strings, not numbers, and they appear verbatim in every response.
pub mod codes {
    pub const OK: &str = "00000";
    pub const UNAUTHORIZED: &str = "A0001";
    pub const RATE_LIMITED: &str = "A0002";
    pub const NO_AVAILABLE_PROXY: &str = "P0001";
    pub const SESSION_NOT_FOUND: &str = "S0001";
    pub const INVALID_INPUT: &str = "I0001";
    pub const NOT_FOUND: &str = "I0002";
    pub const DEDICATED_BOUND: &str = "D0001";
    pub const INSTANCE_ERROR: &str = "R0001";
    pub const IP_CHECK_FAILED: &str = "R0002";
    pub const INTERNAL_ERROR: &str = "99999";
}

/// Errors the pool and API can produce.
///
/// `http_status()` mirrors the Go handlers: 404 / 409 / 400 / 500.
#[derive(Debug, thiserror::Error)]
pub enum WgError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("store: {0}")]
    Store(String),
}

impl WgError {
    pub fn http_status(&self) -> u16 {
        match self {
            WgError::NotFound(_) => 404,
            WgError::Conflict(_) => 409,
            WgError::Invalid(_) => 400,
            WgError::Store(_) => 500,
        }
    }

    /// The business code for this error.
    pub fn code(&self) -> &'static str {
        match self {
            WgError::NotFound(_) => codes::NOT_FOUND,
            WgError::Conflict(_) => codes::DEDICATED_BOUND,
            WgError::Invalid(_) => codes::INVALID_INPUT,
            WgError::Store(_) => codes::INTERNAL_ERROR,
        }
    }
}

pub type Result<T> = std::result::Result<T, WgError>;

/// A successful API response: `{"code":"00000","msg":"","data":...}`.
///
/// Field ORDER matters: the Node version emits `code, msg, data` and clients
/// (and the golden fixtures) compare the serialized string, so the struct is
/// declared in that order.
#[derive(Debug, Clone, Serialize)]
pub struct Envelope<T: Serialize> {
    pub code: &'static str,
    pub msg: String,
    pub data: T,
}

impl<T: Serialize> Envelope<T> {
    pub fn ok(data: T) -> Self {
        Self {
            code: codes::OK,
            msg: String::new(),
            data,
        }
    }
}

/// A failed API response: `{"code":"Exxxx","msg":"...","data":null}`.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub code: String,
    pub msg: String,
    pub data: Option<()>,
}

impl ErrorEnvelope {
    pub fn new(code: impl Into<String>, msg: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            msg: msg.into(),
            data: None,
        }
    }

    pub fn from_error(e: &WgError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

/// One instance on the board, as `/api/containers` reports it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct InstanceInfo {
    pub name: String,
    pub index: i64,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default)]
    pub dedicated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dedicated_remark: Option<String>,
    /// `ip6` or `ip4` — which exit this instance currently uses.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_mode: Option<String>,
}

/// The `data` payload of a successful `/api/pool/acquire`.
#[derive(Debug, Clone, Serialize)]
pub struct AcquiredProxy {
    /// `socks5://<public_ip>:<port>`.
    pub proxy: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipv6: Option<String>,
    /// Minutes this sticky session is bound for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minutes: Option<u32>,
}

/// Board counters for `/api/stats` and `/api/pool/status`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PoolStats {
    pub total_instances: i64,
    pub available: i64,
    pub unhealthy: i64,
    pub unique_ips: i64,
    #[serde(default)]
    pub in_use: i64,
    #[serde(default)]
    pub dedicated: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_shape_is_exactly_the_node_contract() {
        let env = Envelope::ok(serde_json::json!({"a": 1}));

        // Serialize the STRUCT directly. Going through `serde_json::Value`
        // first would reorder the keys (its Map is a BTreeMap), which is not
        // what the HTTP layer does.
        let s = serde_json::to_string(&env).unwrap();
        assert_eq!(s, r#"{"code":"00000","msg":"","data":{"a":1}}"#);

        // And the same field values when read back as a Value.
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["code"], "00000");
        assert_eq!(v["msg"], "");
        assert_eq!(v["data"]["a"], 1);
    }

    #[test]
    fn error_envelope_has_null_data() {
        let v = serde_json::to_value(ErrorEnvelope::new(codes::UNAUTHORIZED, "bad key")).unwrap();
        assert_eq!(v["code"], "A0001");
        assert_eq!(v["msg"], "bad key");
        assert!(v["data"].is_null());
    }

    #[test]
    fn status_strings_match_the_go_constants() {
        assert_eq!(status::CREATING, "creating");
        assert_eq!(status::RUNNING, "running");
        assert_eq!(status::UNHEALTHY, "unhealthy");
        assert_eq!(status::STOPPED, "stopped");
        assert_eq!(status::ERROR, "error");
    }

    #[test]
    fn error_codes_are_strings() {
        assert_eq!(codes::OK, "00000");
        assert_eq!(codes::NO_AVAILABLE_PROXY, "P0001");
        assert_eq!(codes::INTERNAL_ERROR, "99999");
    }

    #[test]
    fn error_status_and_code_map_together() {
        let e = WgError::NotFound("warp-9".into());
        assert_eq!(e.http_status(), 404);
        assert_eq!(e.code(), codes::NOT_FOUND);

        let e = WgError::Conflict("already bound".into());
        assert_eq!(e.http_status(), 409);
        assert_eq!(e.code(), codes::DEDICATED_BOUND);
    }

    #[test]
    fn instance_info_omits_none_fields() {
        let it = InstanceInfo {
            name: "warp-0".into(),
            index: 0,
            status: status::RUNNING.into(),
            ..Default::default()
        };
        let v = serde_json::to_value(&it).unwrap();
        assert!(v.get("ip").is_none(), "None must be omitted");
        assert_eq!(v["status"], "running");
    }

    #[test]
    fn pool_stats_uses_camel_case() {
        let s = PoolStats {
            total_instances: 10,
            available: 8,
            unhealthy: 1,
            unique_ips: 8,
            ..Default::default()
        };
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["totalInstances"], 10);
        assert_eq!(v["uniqueIps"], 8);
    }
}

/// Fill `buf` with cryptographically secure random bytes.
///
/// A thin wrapper so callers need not depend on `getrandom` directly; on
/// glibc 2.17 (CentOS 7) the syscall is unavailable and the crate falls back
/// to `/dev/urandom`, which is why this must not be hand-declared.
pub fn random_bytes(buf: &mut [u8]) -> std::result::Result<(), getrandom::Error> {
    getrandom::getrandom(buf)
}