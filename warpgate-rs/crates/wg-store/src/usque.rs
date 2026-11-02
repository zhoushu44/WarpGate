//! `usque.json` — the MASQUE account configuration.
//!
//! Mirrors Go `wgtunnel.UsqueConfig` (usqueconf.go L20-31), which in turn
//! matches the `usque` CLI's own file format field-for-field. That is the whole
//! point: an account created by the Go or Node version must be usable here, and
//! vice versa.

use serde::{Deserialize, Serialize};

use crate::StoreError;

/// The `usque.json` payload.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsqueConfig {
    /// base64(SEC1 EC private key DER).
    pub private_key: String,
    /// UDP endpoint, no port suffix.
    pub endpoint_v4: String,
    /// UDP endpoint, no brackets and no port.
    pub endpoint_v6: String,
    /// TCP 443 fallback endpoint.
    pub endpoint_h2_v4: String,
    #[serde(default)]
    pub endpoint_h2_v6: String,
    /// PEM(PKIX) endpoint public key.
    pub endpoint_pub_key: String,
    pub id: String,
    pub access_token: String,
    pub ipv4: String,
    pub ipv6: String,
}

impl UsqueConfig {
    /// Read `usque.json`. A missing file yields `None`.
    pub fn read(dir: &std::path::Path) -> Result<Option<UsqueConfig>, StoreError> {
        Self::read_path(&dir.join("usque.json"))
    }

    /// Read a specific `usque.json` path.
    pub fn read_path(path: &std::path::Path) -> Result<Option<UsqueConfig>, StoreError> {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Io(e.to_string())),
        };
        let c: UsqueConfig = serde_json::from_str(&raw).map_err(|source| StoreError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        if c.private_key.is_empty() || c.endpoint_pub_key.is_empty() {
            return Err(StoreError::MissingField {
                path: path.display().to_string(),
                field: "private_key/endpoint_pub_key",
            });
        }
        Ok(Some(c))
    }

    /// Write `usque.json` with two-space indentation and 0600 permissions.
    ///
    /// It holds a private key, so the mode is not cosmetic.
    pub fn save(&self, path: &std::path::Path) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Io(e.to_string()))?;
        }
        let data = serde_json::to_string_pretty(self).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::write(path, data).map_err(|e| StoreError::Io(e.to_string()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// The QUIC endpoint for this account.
    ///
    /// Accepts the three storage formats the CLI produces: a bare IP, `[v6]`,
    /// and `ip:port` / `[v6]:port`. Mirrors Go `SelectUDPEndpoint`.
    pub fn udp_endpoint(&self, use_ipv6: bool, connect_port: i64) -> Result<String, StoreError> {
        let host = if use_ipv6 {
            &self.endpoint_v6
        } else {
            &self.endpoint_v4
        };
        if host.is_empty() {
            return Err(StoreError::MissingField {
                path: "usque.json".into(),
                field: if use_ipv6 { "endpoint_v6" } else { "endpoint_v4" },
            });
        }
        // Already carries a port?
        if host.parse::<std::net::SocketAddr>().is_ok() {
            return Ok(host.clone());
        }
        let cleaned = host.trim_matches(|c| c == '[' || c == ']');
        if cleaned.parse::<std::net::IpAddr>().is_err() {
            return Err(StoreError::MissingField {
                path: "usque.json".into(),
                field: "endpoint (not a valid address)",
            });
        }
        Ok(format!("{}:{}", cleaned, connect_port))
    }

    /// Whether the private key parses as base64 DER (a cheap sanity check
    /// before handing it to the tunnel layer).
    pub fn private_key_is_plausible(&self) -> bool {
        use base64_decode_shim as b64;
        b64::decode(&self.private_key).map(|d| !d.is_empty()).unwrap_or(false)
    }
}

/// A tiny base64 decoder so this crate stays dependency-free.
///
/// `usque.json` stores keys as standard base64, and the only thing we do with
/// them here is check they decode — pulling in a crate for that is not worth
/// the dependency.
mod base64_decode_shim {
    pub fn decode(s: &str) -> Result<Vec<u8>, ()> {
        let table = |c: u8| -> Option<u8> {
            match c {
                b'A'..=b'Z' => Some(c - b'A'),
                b'a'..=b'z' => Some(c - b'a' + 26),
                b'0'..=b'9' => Some(c - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        };
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0u32;
        for &c in s.as_bytes() {
            if c == b'=' || c == b'\n' || c == b'\r' {
                continue;
            }
            let v = table(c).ok_or(())?;
            buf = (buf << 6) | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> UsqueConfig {
        UsqueConfig {
            private_key: "MHcCAQEEIB9x7vVZ0o9YlQ1r2sT8k3mN4pQ5rS6tU7vW8x9y0z1A2oAoGCCqGSM49AwEHoUQDQgAE".into(),
            endpoint_v4: "162.159.198.3".into(),
            endpoint_v6: "[2606:4700:d0::a29f:c003]".into(),
            endpoint_h2_v4: "162.159.198.2".into(),
            endpoint_h2_v6: String::new(),
            endpoint_pub_key: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----\n".into(),
            id: "fake-id".into(),
            access_token: "fake-token".into(),
            ipv4: "172.16.0.2/32".into(),
            ipv6: "2606:4700:110:8c9f::1/128".into(),
        }
    }

    #[test]
    fn serialization_matches_the_cli_shape() {
        let c = sample();
        let s = serde_json::to_string_pretty(&c).unwrap();
        // snake_case keys, two-space indent, declaration order.
        assert!(s.contains("\"private_key\":"), "{s}");
        assert!(s.contains("\"endpoint_v4\": \"162.159.198.3\""));
        assert!(s.contains("\"endpoint_h2_v4\": \"162.159.198.2\""));
        assert!(s.contains("\"access_token\": \"fake-token\""));
    }

    #[test]
    fn round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("usque.json");
        sample().save(&p).unwrap();
        let back = UsqueConfig::read_path(&p).unwrap().unwrap();
        assert_eq!(back, sample());
    }

    #[test]
    fn missing_file_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(UsqueConfig::read(tmp.path()).unwrap().is_none());
    }

    /// A structurally valid file that is missing the key material is rejected
    /// by the explicit check, not by serde.
    #[test]
    fn missing_key_material_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("usque.json"),
            r#"{"private_key":"","endpoint_v4":"1.2.3.4","endpoint_v6":"","endpoint_h2_v4":"","endpoint_pub_key":"k","id":"x","access_token":"t","ipv4":"","ipv6":""}"#,
        )
        .unwrap();
        let err = UsqueConfig::read(tmp.path()).unwrap_err();
        assert!(
            matches!(
                err,
                StoreError::MissingField { field, .. } if field.contains("private_key")
            ),
            "got: {err}"
        );
    }

    /// A file missing a required *structural* field fails at parse time, which
    /// is the same outcome (an error) with a clearer message.
    #[test]
    fn structurally_incomplete_file_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("usque.json"), r#"{"private_key":"abc"}"#).unwrap();
        assert!(UsqueConfig::read(tmp.path()).is_err());
    }

    #[test]
    fn udp_endpoint_handles_all_storage_formats() {
        let mut c = sample();
        let port = 443;

        // bare IPv4
        c.endpoint_v4 = "162.159.198.3".into();
        assert_eq!(c.udp_endpoint(false, port).unwrap(), "162.159.198.3:443");

        // bracketed IPv6 with no port: the brackets are stripped, then the
        // connect port is appended.
        c.endpoint_v6 = "[2606:4700:d0::a29f:c003]".into();
        assert_eq!(
            c.udp_endpoint(true, port).unwrap(),
            "2606:4700:d0::a29f:c003:443"
        );

        // already carries a port
        c.endpoint_v4 = "162.159.198.3:8443".into();
        assert_eq!(c.udp_endpoint(false, port).unwrap(), "162.159.198.3:8443");

        // a non-default connect port
        c.endpoint_v4 = "162.159.198.3".into();
        assert_eq!(c.udp_endpoint(false, 8443).unwrap(), "162.159.198.3:8443");
    }

    #[test]
    fn udp_endpoint_rejects_garbage() {
        let mut c = sample();
        c.endpoint_v4 = "not an ip".into();
        assert!(c.udp_endpoint(false, 443).is_err());
    }

    #[test]
    fn udp_endpoint_reports_which_field_is_missing() {
        let mut c = sample();
        c.endpoint_v6 = String::new();
        let err = c.udp_endpoint(true, 443).unwrap_err();
        assert!(err.to_string().contains("endpoint_v6"), "got: {err}");
    }

    #[test]
    fn private_key_sanity_check() {
        assert!(sample().private_key_is_plausible());
        let mut bad = sample();
        bad.private_key = "!!!not base64!!!".into();
        assert!(!bad.private_key_is_plausible());
    }

    /// The Go version writes 0600 because the file holds a private key.
    #[test]
    #[cfg(unix)]
    fn saved_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("usque.json");
        sample().save(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "got {:o}", mode & 0o777);
    }
}
