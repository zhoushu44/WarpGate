//! `meta.json` — the per-instance state file.
//!
//! Mirrors Go `store.Meta` (store.go L13-26). Field-for-field, including the
//! `omitempty` choices: `endpoint` and `dedicated_remark` are omitted when
//! absent, and `engine` too, because the Node version reads these keys
//! dynamically and an unexpected `null` where it expects a string breaks it.

use serde::{Deserialize, Serialize};

use crate::StoreError;

/// Accept `null` for a string field, mapping it to the empty string.
///
/// The Node version writes `"ip": null` and `"endpoint": null` when it does not
/// know the value yet, so a plain `String` field rejects the whole file.
fn null_to_empty<'de, D>(d: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(d)?.unwrap_or_default())
}

/// The `meta.json` payload.
///
/// Two on-disk dialects exist and both must be readable:
///
/// * **Go / this crate**: snake_case, with `index` / `name` / `ipv6` / `status`.
/// * **Node** (`server.js`): camelCase — `createdAt` instead of `created_at`,
///   `ip` instead of `ipv6`, and no `index` / `name` / `status` at all.
///
/// `serde(alias)` accepts the Node spelling on read while we keep writing the
/// Go spelling, so a directory produced by either binary loads correctly and
/// this binary's output stays interchangeable with the Go one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    #[serde(default)]
    pub index: i64,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub name: String,
    /// The embedded version has no child process, so this is always 0.
    #[serde(default)]
    pub pid: i64,
    #[serde(default)]
    pub engine_pid: i64,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// Node calls this `ip`, and writes it as `null` when unknown.
    #[serde(default, deserialize_with = "null_to_empty", alias = "ip")]
    pub ipv6: String,
    #[serde(default, deserialize_with = "null_to_empty")]
    pub status: String,
    /// Milliseconds since the epoch. Node calls this `createdAt`.
    #[serde(default, alias = "createdAt")]
    pub created_at: i64,
    #[serde(default)]
    pub dedicated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedicated_remark: Option<String>,
    /// When the pin was set, in milliseconds since the epoch. `0` means never.
    ///
    /// Omitted when unset so `meta.json` stays byte-compatible with the Go and
    /// Node versions, which do not know this key.
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub dedicated_at: i64,
    /// `"embedded"` for this implementation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
}

/// `skip_serializing_if` helper: treat 0 as "never set".
fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

impl Meta {
    /// Read `meta.json` from `dir`. A missing file yields `None`.
    pub fn read(dir: &std::path::Path) -> Result<Option<Meta>, StoreError> {
        let path = dir.join("meta.json");
        let raw = match std::fs::read_to_string(&path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::Io(e.to_string())),
        };
        let m: Meta = serde_json::from_str(&raw).map_err(|source| StoreError::Parse {
            path: path.display().to_string(),
            source,
        })?;
        Ok(Some(m))
    }

    /// Write `meta.json` with two-space indentation, matching Go
    /// `json.MarshalIndent(m, "", "  ")` and Node `JSON.stringify(x, null, 2)`.
    pub fn write(&self, dir: &std::path::Path) -> Result<(), StoreError> {
        std::fs::create_dir_all(dir).map_err(|e| StoreError::Io(e.to_string()))?;
        let data = serde_json::to_string_pretty(self).map_err(|e| StoreError::Io(e.to_string()))?;
        std::fs::write(dir.join("meta.json"), data).map_err(|e| StoreError::Io(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Meta` written by Rust must be readable by the Go version.
    ///
    /// The assertion is on the serialized STRING, not on the parsed value,
    /// because "byte-compatible" is the actual requirement.
    #[test]
    fn serialization_matches_go_shape() {
        let m = Meta {
            index: 3,
            name: "warp-3".into(),
            pid: 0,
            engine_pid: 0,
            transport: "masque".into(),
            endpoint: None,
            ipv6: "2606:4700:110:8c9f::1".into(),
            status: "running".into(),
            created_at: 1_700_000_000_000,
            dedicated: false,
            dedicated_remark: None,
            dedicated_at: 0,
            engine: Some("embedded".into()),
        };
        let s = serde_json::to_string_pretty(&m).unwrap();
        // Two-space indent, keys in declaration order.
        assert_eq!(
            s,
            r#"{
  "index": 3,
  "name": "warp-3",
  "pid": 0,
  "engine_pid": 0,
  "transport": "masque",
  "ipv6": "2606:4700:110:8c9f::1",
  "status": "running",
  "created_at": 1700000000000,
  "dedicated": false,
  "engine": "embedded"
}"#
        );
    }

    #[test]
    fn absent_optionals_are_omitted_not_null() {
        let m = Meta {
            index: 0,
            name: "warp-0".into(),
            transport: "masque".into(),
            ipv6: "::1".into(),
            status: "stopped".into(),
            created_at: 1,
            ..Default::default()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(!s.contains("endpoint"), "absent endpoint must be omitted");
        assert!(!s.contains("null"), "no nulls anywhere: {s}");
    }

    #[test]
    fn round_trip_preserves_everything() {
        let m = Meta {
            index: 9,
            name: "warp-9".into(),
            pid: 0,
            engine_pid: 0,
            transport: "masque".into(),
            endpoint: Some("162.159.198.3".into()),
            ipv6: "2606:4700::9".into(),
            status: "unhealthy".into(),
            created_at: 42,
            dedicated: true,
            dedicated_remark: Some("kept".into()),
            dedicated_at: 0,
            engine: Some("embedded".into()),
        };
        let tmp = tempfile::tempdir().unwrap();
        m.write(tmp.path()).unwrap();
        let back = Meta::read(tmp.path()).unwrap().unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn missing_file_yields_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(Meta::read(tmp.path()).unwrap().is_none());
    }

    /// The Node version reads keys dynamically, so extra keys must not break us.
    #[test]
    fn unknown_keys_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meta.json"),
            r#"{"index":1,"name":"warp-1","pid":0,"engine_pid":0,"transport":"masque","ipv6":"::1","status":"running","created_at":5,"dedicated":false,"some_future_key":123}"#,
        )
        .unwrap();
        let m = Meta::read(tmp.path()).unwrap().unwrap();
        assert_eq!(m.index, 1);
        assert_eq!(m.created_at, 5);
    }

    /// A dedicated flag written by an older binary must still be read.
    #[test]
    fn dedicated_flag_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meta.json"),
            r#"{"index":2,"name":"warp-2","transport":"masque","ipv6":"::2","status":"running","created_at":1,"dedicated":true,"dedicated_remark":"vip"}"#,
        )
        .unwrap();
        let m = Meta::read(tmp.path()).unwrap().unwrap();
        assert!(m.dedicated);
        assert_eq!(m.dedicated_remark.as_deref(), Some("vip"));
    }

    /// The REAL Node meta.json, captured from a live deployment.
    ///
    /// Note it is camelCase, has no `index`/`name`/`status`, and uses `ip`
    /// rather than `ipv6`. Reading it must not fail and must not lose the
    /// fields it does have.
    #[test]
    fn reads_the_real_node_dialect() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meta.json"),
            r#"{
  "createdAt": 1793452299660,
  "engine_pid": 13209,
  "transport": "masque",
  "pid": 13210,
  "port": 10010,
  "ip": null,
  "endpoint": null
}"#,
        )
        .unwrap();
        let m = Meta::read(tmp.path()).unwrap().unwrap();
        assert_eq!(m.transport, "masque", "transport must survive");
        assert_eq!(m.created_at, 1793452299660, "createdAt must map to created_at");
        assert_eq!(m.pid, 13210);
        assert_eq!(m.engine_pid, 13209);
        // Fields Node does not write default to empty rather than failing.
        assert_eq!(m.index, 0);
        assert_eq!(m.name, "");
        assert_eq!(m.ipv6, "");
        assert_eq!(m.status, "");
    }

    /// A Node `ip` that is a real string (not null) must land in `ipv6`.
    #[test]
    fn node_ip_maps_to_ipv6() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meta.json"),
            r#"{"createdAt":1,"transport":"masque","ip":"2606:4700:110::1","port":10010}"#,
        )
        .unwrap();
        let m = Meta::read(tmp.path()).unwrap().unwrap();
        assert_eq!(m.ipv6, "2606:4700:110::1");
    }

    /// Writing still uses the Go spelling, so the Go binary can take over.
    #[test]
    fn writing_uses_the_go_dialect() {
        let m = Meta {
            index: 3,
            name: "warp-3".into(),
            transport: "masque".into(),
            ipv6: "::3".into(),
            status: "running".into(),
            created_at: 42,
            ..Default::default()
        };
        let s = serde_json::to_string(&m).unwrap();
        assert!(s.contains("\"created_at\":42"), "Go spelling on write: {s}");
        assert!(s.contains("\"ipv6\":\"::3\""), "Go spelling on write: {s}");
        assert!(!s.contains("createdAt"), "must not emit the Node spelling");
    }

    /// The pool derives index/name/status when the file lacks them, so a Node
    /// directory is still schedulable.
    #[test]
    fn fill_from_dir_name() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("meta.json"),
            r#"{"createdAt":1,"transport":"masque","port":10010}"#,
        )
        .unwrap();
        let mut m = Meta::read(tmp.path()).unwrap().unwrap();

        // The pool does this after reading a Node-style file.
        let dir_name = tmp
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        if m.index == 0 {
            if let Some(n) = dir_name.strip_prefix("warp-") {
                m.index = n.parse().unwrap_or(0);
            }
        }
        if m.name.is_empty() {
            m.name = format!("warp-{}", m.index);
        }
        if m.status.is_empty() {
            m.status = "running".into();
        }
        assert_eq!(m.name, "warp-0");
        assert_eq!(m.status, "running");
    }
}
