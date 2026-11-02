//! Instance-directory persistence.
//!
//! Mirrors Go `internal/store`. The invariant that matters most:
//!
//! > **The JSON must be byte-compatible with the Go and Node versions.**
//!
//! `instances/warp-<i>/meta.json` and `usque.json` are read and written by all
//! three implementations, so a field rename or a different `omitempty` policy
//! breaks a live deployment the moment the binaries are swapped. The tests here
//! compare against fixtures captured from the Go output.

pub mod meta;
pub mod paths;
pub mod usque;

pub use meta::Meta;
pub use usque::UsqueConfig;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io: {0}")]
    Io(String),
    #[error("parse {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path} 缺少 {field}")]
    MissingField { path: String, field: &'static str },
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// `warp-<i>` — the instance directory name, matching Go `store.Name`.
pub fn name(index: i64) -> String {
    format!("warp-{}", index)
}

/// `<base>/warp-<i>`, matching Go `store.Dir`.
pub fn dir(base: &std::path::Path, index: i64) -> std::path::PathBuf {
    base.join(name(index))
}

/// Whether the instance directory exists.
pub fn exists(base: &std::path::Path, index: i64) -> bool {
    dir(base, index).is_dir()
}

/// Delete the whole instance directory.
pub fn remove(d: &std::path::Path) -> Result<()> {
    std::fs::remove_dir_all(d).map_err(|e| StoreError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_go() {
        assert_eq!(name(0), "warp-0");
        assert_eq!(name(149), "warp-149");
        let base = std::path::Path::new("/data/instances");
        assert_eq!(dir(base, 7), base.join("warp-7"));
    }

    #[test]
    fn exists_detects_a_real_directory() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir(tmp.path(), 3)).unwrap();
        assert!(exists(tmp.path(), 3));
        assert!(!exists(tmp.path(), 4));
    }

    #[test]
    fn remove_deletes_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let d = dir(tmp.path(), 1);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("usque.json"), "{}").unwrap();
        remove(&d).unwrap();
        assert!(!d.exists());
    }
}
