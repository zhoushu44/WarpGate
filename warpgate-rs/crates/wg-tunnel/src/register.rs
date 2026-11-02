//! Cloudflare WARP account registration.
//!
//! Mirrors usque's `api.Register` + `api.EnrollKey`
//! (`api/cloudflare.go`, `internal/consts.go`).
//!
//! Two HTTP calls produce a fully usable account:
//!
//! 1. **POST /v0a4471/reg** with a throwaway WireGuard public key — this only
//!    mimics the Android app's registration shape and creates the account.
//! 2. **PATCH /v0a4471/reg/<id>** with the real MASQUE (secp256r1) public key,
//!    which is what the tunnel actually authenticates with.
//!
//! The response's `config.peers[0].public_key` is the endpoint's ECDSA public
//! key, which we pin in TLS — that is the only thing preventing a MITM from
//! impersonating the WARP edge.

use serde::{Deserialize, Serialize};

/// API base and version (usque `internal/consts.go`).
pub const API_URL: &str = "https://api.cloudflareclient.com";
pub const API_VERSION: &str = "v0a4471";

/// Headers that make the request look like the Android client.
///
/// These are not cosmetic: Cloudflare rejects registrations without them.
pub fn default_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    h.insert("User-Agent", HeaderValue::from_static("WARP for Android"));
    h.insert("CF-Client-Version", HeaderValue::from_static("a-6.35-4471"));
    h.insert("Content-Type", HeaderValue::from_static("application/json; charset=UTF-8"));
    h.insert("Connection", HeaderValue::from_static("Keep-Alive"));
    h
}

/// Cloudflare's timestamp format: RFC 3339 with millisecond precision.
fn cf_now() -> String {
    // usque's TimeAsCfString: "2006-01-02T15:04:05.000Z07:00"
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let days = secs / 86400;
    let rem = secs % 86400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days (Howard Hinnant's algorithm), avoids a chrono dependency.
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mo <= 2 { y + 1 } else { y };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, m, s, millis
    )
}

/// The registration payload (`models.Registration`).
#[derive(Debug, Serialize)]
pub struct Registration {
    pub key: String,
    #[serde(rename = "install_id")]
    pub install_id: String,
    #[serde(rename = "fcm_token")]
    pub fcm_token: String,
    pub tos: String,
    pub model: String,
    pub serial: String,
    #[serde(rename = "os_version")]
    pub os_version: String,
    #[serde(rename = "type")]
    pub key_type: String,
    #[serde(rename = "tun_type")]
    pub tun_type: String,
    pub locale: String,
}

/// The account returned by registration.
#[derive(Debug, Deserialize)]
pub struct AccountData {
    pub id: String,
    pub token: String,
    pub config: AccountConfig,
}

#[derive(Debug, Deserialize, Default)]
pub struct AccountConfig {
    #[serde(default)]
    pub peers: Vec<Peer>,
    #[serde(default)]
    pub interface: InterfaceConfig,
}

#[derive(Debug, Deserialize)]
pub struct Peer {
    pub endpoint: Endpoint,
    pub public_key: String,
}

#[derive(Debug, Deserialize)]
pub struct Endpoint {
    pub v4: String,
    pub v6: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct InterfaceConfig {
    #[serde(default)]
    pub addresses: Addresses,
}

#[derive(Debug, Deserialize, Default)]
pub struct Addresses {
    #[serde(default)]
    pub v4: String,
    #[serde(default)]
    pub v6: String,
}

/// The PATCH payload that enrolls the MASQUE device key.
#[derive(Debug, Serialize)]
pub struct EnrollKeyRequest {
    pub key: String,
    #[serde(rename = "key_type")]
    pub key_type: String,
    #[serde(rename = "tun_type")]
    pub tun_type: String,
}

#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    #[error("http: {0}")]
    Http(String),
    #[error("Cloudflare 拒绝: {0}")]
    Rejected(String),
    #[error("响应缺少 {0}")]
    Missing(&'static str),
}

/// A random 32-byte base64 key, shaped like a WireGuard public key.
///
/// usque notes the key is never stored or used — it only exists so the request
/// looks like the Android app's. A random value is sufficient.
fn random_wg_pubkey() -> String {
    use base64::Engine;
    let mut buf = [0u8; 32];
    getrandom(&mut buf);
    base64::engine::general_purpose::STANDARD.encode(buf)
}

/// A random 16-char hex serial, like an Android device serial.
fn random_android_serial() -> String {
    let mut buf = [0u8; 8];
    getrandom(&mut buf);
    let mut s = String::with_capacity(16);
    for b in buf {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

/// Fill a buffer from the OS CSPRNG.
///
/// Uses the `getrandom` crate rather than declaring the libc symbol by hand:
/// a bare `extern "C" { fn getrandom(...) }` links on modern glibc but fails on
/// CentOS 7 (glibc 2.17), where the symbol does not exist. The crate falls back
/// to the syscall directly.
fn getrandom(buf: &mut [u8]) {
    // These are throwaway values (a WireGuard key that is never used, and a
    // device serial), so a failure only degrades realism, not correctness.
    let _ = getrandom::getrandom(buf);
}

/// Register a fresh anonymous account.
///
/// `jwt` is the Teams token; empty for anonymous.
pub async fn register(model: &str, locale: &str, jwt: &str) -> Result<AccountData, RegisterError> {
    let payload = Registration {
        key: random_wg_pubkey(),
        install_id: String::new(),
        fcm_token: String::new(),
        tos: cf_now(),
        model: if model.is_empty() { "PC".to_string() } else { model.to_string() },
        serial: random_android_serial(),
        os_version: String::new(),
        key_type: "curve25519".into(),
        tun_type: "wireguard".into(),
        locale: if locale.is_empty() { "en_US".to_string() } else { locale.to_string() },
    };

    let url = format!("{}/{}/reg", API_URL, API_VERSION);
    let mut req = reqwest::Client::new()
        .post(&url)
        .headers(default_headers())
        .json(&payload);

    if !jwt.is_empty() {
        req = req.header("CF-Access-Jwt-Assertion", jwt);
    }

    let resp = req.send().await.map_err(|e| RegisterError::Http(e.to_string()))?;
    let status = resp.status();
    let body = resp.text().await.map_err(|e| RegisterError::Http(e.to_string()))?;

    if !status.is_success() {
        return Err(RegisterError::Rejected(format!("{} {}", status.as_u16(), first_line(&body))));
    }

    let account: AccountData =
        serde_json::from_str(&body).map_err(|e| RegisterError::Http(format!("解析账号失败: {e}")))?;
    if account.id.is_empty() {
        return Err(RegisterError::Missing("id"));
    }
    Ok(account)
}

/// Enroll the MASQUE device key and return the updated account.
pub async fn enroll_key(
    account_id: &str,
    account_token: &str,
    masque_pubkey_b64: &str,
    _device_name: &str,
) -> Result<AccountData, RegisterError> {
    let payload = EnrollKeyRequest {
        key: masque_pubkey_b64.to_string(),
        key_type: "secp256r1".into(),
        tun_type: "masque".into(),
    };

    let url = format!("{}/{}/reg/{}", API_URL, API_VERSION, account_id);
    let resp = reqwest::Client::new()
        .patch(&url)
        .headers(default_headers())
        .header("Authorization", format!("Bearer {}", account_token))
        .json(&payload)
        .send()
        .await
        .map_err(|e| RegisterError::Http(e.to_string()))?;

    let status = resp.status();
    let body = resp.text().await.map_err(|e| RegisterError::Http(e.to_string()))?;
    if !status.is_success() {
        return Err(RegisterError::Rejected(format!("{} {}", status.as_u16(), first_line(&body))));
    }

    let updated: AccountData =
        serde_json::from_str(&body).map_err(|e| RegisterError::Http(format!("解析登记响应失败: {e}")))?;
    if updated.config.peers.is_empty() {
        return Err(RegisterError::Missing("config.peers"));
    }
    Ok(updated)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_timestamp_is_rfc3339_millis() {
        let t = cf_now();
        // 2026-11-01T09:00:00.000Z — 24 chars, ends with Z
        assert_eq!(t.len(), 24, "got {t}");
        assert!(t.ends_with('Z'), "got {t}");
        assert!(t.contains('T'), "got {t}");
        assert!(t.as_bytes()[19] == b'.', "millis separator, got {t}");
    }

    #[test]
    fn random_wg_pubkey_is_44_chars_base64() {
        use base64::Engine;
        let k = random_wg_pubkey();
        assert_eq!(k.len(), 44, "base64 of 32 bytes, got {k}");
        let decoded = base64::engine::general_purpose::STANDARD.decode(&k).unwrap();
        assert_eq!(decoded.len(), 32);
    }

    #[test]
    fn random_serial_is_16_hex() {
        let s = random_android_serial();
        assert_eq!(s.len(), 16, "got {s}");
        assert!(s.chars().all(|c| c.is_ascii_hexdigit()), "got {s}");
    }

    #[test]
    fn keys_differ_between_calls() {
        assert_ne!(random_wg_pubkey(), random_wg_pubkey());
        assert_ne!(random_android_serial(), random_android_serial());
    }

    #[test]
    fn registration_payload_shape() {
        let p = Registration {
            key: "k".into(),
            install_id: String::new(),
            fcm_token: String::new(),
            tos: "2026-01-01T00:00:00.000Z".into(),
            model: "PC".into(),
            serial: "abc".into(),
            os_version: String::new(),
            key_type: "curve25519".into(),
            tun_type: "wireguard".into(),
            locale: "en_US".into(),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["type"], "curve25519");
        assert_eq!(v["tun_type"], "wireguard");
        assert_eq!(v["model"], "PC");
    }

    #[test]
    fn enroll_payload_shape() {
        let p = EnrollKeyRequest {
            key: "pub".to_string(),
            key_type: "secp256r1".into(),
            tun_type: "masque".into(),
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["key_type"], "secp256r1");
        assert_eq!(v["tun_type"], "masque");
    }

    #[test]
    fn default_headers_match_usque() {
        let h = default_headers();
        assert_eq!(h.get("User-Agent").unwrap(), "WARP for Android");
        assert_eq!(h.get("CF-Client-Version").unwrap(), "a-6.35-4471");
        assert!(h.get("Content-Type").unwrap().to_str().unwrap().starts_with("application/json"));
    }
}
