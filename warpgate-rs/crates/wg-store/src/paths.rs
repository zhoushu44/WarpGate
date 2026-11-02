//! Path helpers for the instance directory.
//!
//! Mirrors the Go accessors (`engineConfigPath`, `engineLogPath`) so the Rust
//! and Go binaries lay out `instances/warp-<i>/` identically.

use std::path::{Path, PathBuf};

/// `<dir>/usque.json` — the MASQUE account config.
pub fn usque_conf_path(dir: &Path) -> PathBuf {
    dir.join("usque.json")
}

/// `<dir>/meta.json` — the instance state.
pub fn meta_path(dir: &Path) -> PathBuf {
    dir.join("meta.json")
}

/// `<dir>/engine.log` — the engine's log file.
pub fn engine_log_path(dir: &Path) -> PathBuf {
    dir.join("engine.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_match_the_go_layout() {
        let d = Path::new("/data/instances/warp-0");
        assert_eq!(usque_conf_path(d), d.join("usque.json"));
        assert_eq!(meta_path(d), d.join("meta.json"));
        assert_eq!(engine_log_path(d), d.join("engine.log"));
    }
}
