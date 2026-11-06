//! Config loading, mirroring Go `internal/appconfig/config.go`.
//!
//! The field names are the `config.json` keys verbatim — the Node version reads
//! and writes this file, so a renamed key silently breaks a live deployment.
//! `serde` is configured to match Go's `json` tags exactly.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The full configuration, one field per `config.json` key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    pub port: i64,
    pub max_containers: i64,
    pub resident_count: i64,
    pub sticky_minutes: i64,
    pub auto_rebuild_on_unhealthy: bool,
    pub health_check_interval: i64,
    /// Instances probed per health-loop tick. Keeps a 500-instance sweep from
    /// opening every tunnel at the same moment.
    #[serde(default = "default_health_probe_batch")]
    pub health_probe_batch: i64,
    /// Recycle an engine once it holds this many established connections.
    ///
    /// `usque --always-reconnect` leaks one socket per redial and Cloudflare
    /// drops the tunnel roughly every 5 minutes, so the count climbs ~10/hour.
    /// A healthy engine needs exactly one connection (HTTP/2 multiplexes every
    /// SOCKS request over it), so 3 catches a leak almost immediately while
    /// still leaving room for a transient redial. 0 disables the check.
    #[serde(default = "default_health_max_conns")]
    pub health_max_conns: i64,
    pub bind_address: String,
    pub mode: String,
    pub base_port: i64,
    pub max_instances: i64,
    pub min_available: i64,
    pub max_concurrent_create: i64,

    pub ip_check_url: String,
    pub ip_check_url_v6: String,
    pub openai_check_url: String,
    pub ip_check_retries: i64,
    pub ip_check_interval: i64,
    pub max_rebuild_attempts: i64,
    pub cleanup_on_exit: bool,
    pub rate_limit_enabled: bool,
    pub rate_limit_per_minute: i64,

    pub auto_rotate_on_high_403: bool,
    pub cf_403_threshold: i64,
    pub cf_min_check_count: i64,
    pub ip_risk_check: bool,
    pub ip_risk_threshold: i64,
    pub ip_risk_ttl: i64,
    pub ip_risk_check_url: String,
    pub warp_transport: String,
    pub log_level: String,
    pub public_ip: String,
    pub session_cleanup_interval: i64,
    pub team_token: String,

    pub proxy_auth_enabled: bool,
    pub proxy_username: String,
    pub proxy_password: String,
    /// CIDR or bare IP entries.
    ///
    /// The Node version stores objects (`{ip, remark, region, added_at}`) so the
    /// console can show provenance; the Go version and this one accept plain
    /// strings too. `whitelist_entries` normalises both shapes.
    #[serde(default, deserialize_with = "de_ip_whitelist")]
    pub ip_whitelist: Vec<String>,
    pub whitelist_port_offset: i64,
    pub api_key: String,
    pub masque_http2: bool,
    pub masque_endpoint_v4: String,
    pub masque_endpoint_h2_v4: String,
    pub masque_endpoint_candidates: Vec<String>,

    /// v1 keys the Go version added on top of the Node defaults.
    #[serde(default)]
    pub instances_dir: String,
    #[serde(default = "default_keepalive")]
    pub tunnel_keepalive_seconds: i64,
    #[serde(default = "default_reconnect_max")]
    pub tunnel_reconnect_max_seconds: i64,
    #[serde(default = "default_dns")]
    pub dns_servers: Vec<String>,
    #[serde(default)]
    pub socks_tcp_timeout_seconds: i64,
    #[serde(default = "default_connect_port")]
    pub connect_port: i64,
}

fn default_keepalive() -> i64 {
    30
}
fn default_reconnect_max() -> i64 {
    60
}
fn default_dns() -> Vec<String> {
    vec!["1.1.1.1".into(), "8.8.8.8".into()]
}

/// Accept `ip_whitelist` in either on-disk shape.
///
/// * Node: `[{"ip": "1.2.3.4", "remark": "", "region": "..."}]`
/// * Go / this crate: `["1.2.3.4", "10.0.0.0/8"]`
///
/// Both reduce to the address list, which is all the SOCKS5 layer needs.
fn de_ip_whitelist<'de, D>(d: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Entry {
        Plain(String),
        Object { ip: String },
    }

    let raw = Vec::<Entry>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .map(|e| match e {
            Entry::Plain(s) => s,
            Entry::Object { ip } => ip,
        })
        .filter(|s| !s.trim().is_empty())
        .collect())
}
fn default_connect_port() -> i64 {
    443
}

impl Default for Config {
    /// The shipped defaults, matching Go `Default()`.
    fn default() -> Self {
        Self {
            port: 4433,
            max_containers: 1,
            resident_count: 5,
            sticky_minutes: 10,
            auto_rebuild_on_unhealthy: true,
            health_check_interval: 600,
            health_probe_batch: 8,
            health_max_conns: 3,
            bind_address: "0.0.0.0".into(),
            mode: "resident".into(),
            base_port: 10010,
            max_instances: 200,
            min_available: 5,
            max_concurrent_create: 3,

            ip_check_url: "https://api.ipify.org".into(),
            ip_check_url_v6: "https://api6.ipify.org".into(),
            openai_check_url: "https://www.cloudflare.com".into(),
            ip_check_retries: 10,
            ip_check_interval: 3000,
            max_rebuild_attempts: 3,
            cleanup_on_exit: false,
            rate_limit_enabled: true,
            rate_limit_per_minute: 60,

            auto_rotate_on_high_403: true,
            cf_403_threshold: 50,
            cf_min_check_count: 3,
            ip_risk_check: true,
            ip_risk_threshold: 70,
            ip_risk_ttl: 21600,
            ip_risk_check_url: "https://scamalytics.com/ip/".into(),
            warp_transport: "masque".into(),
            log_level: "info".into(),
            public_ip: "auto-detect".into(),
            session_cleanup_interval: 30,
            team_token: String::new(),

            proxy_auth_enabled: true,
            proxy_username: "warpuser".into(),
            proxy_password: String::new(),
            ip_whitelist: Vec::new(),
            whitelist_port_offset: 1000,
            api_key: String::new(),
            masque_http2: true,
            masque_endpoint_v4: String::new(),
            masque_endpoint_h2_v4: String::new(),
            masque_endpoint_candidates: Vec::new(),

            instances_dir: String::new(),
            tunnel_keepalive_seconds: 30,
            tunnel_reconnect_max_seconds: 60,
            dns_servers: default_dns(),
            socks_tcp_timeout_seconds: 0,
            connect_port: 443,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("读取配置失败: {0}")]
    Read(String),
    #[error("解析配置失败: {0}")]
    Parse(String),
    #[error("{0}")]
    Invalid(String),
}

impl Config {
    /// Load `path`, filling in defaults for anything absent.
    pub fn load(path: &str) -> Result<Self, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|e| ConfigError::Read(e.to_string()))?;
        let cfg: Config =
            serde_json::from_str(&raw).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// The v1 hard constraints, mirroring Go `Validate()`.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // The embedded data plane only speaks MASQUE; WireGuard embedding is a
        // later phase.
        if self.warp_transport != "masque" {
            return Err(ConfigError::Invalid(format!(
                "warp_transport={:?}: 嵌入版仅支持 masque",
                self.warp_transport
            )));
        }
        if self.resident_count < 0 || self.resident_count > self.max_instances {
            return Err(ConfigError::Invalid(format!(
                "resident_count={} 超出范围 [0,{}]",
                self.resident_count, self.max_instances
            )));
        }
        if self.base_port <= 0 || self.whitelist_port_offset < 0 {
            return Err(ConfigError::Invalid(
                "base_port/whitelist_port_offset 非法".into(),
            ));
        }
        if self.max_instances > 0
            && self.base_port + self.max_instances + self.whitelist_port_offset > 65535
        {
            return Err(ConfigError::Invalid(format!(
                "base_port({})+max_instances({})+offset({}) 超出端口范围",
                self.base_port, self.max_instances, self.whitelist_port_offset
            )));
        }
        Ok(())
    }

    /// Resolve the instances directory: flag > env > config > `./instances`.
    pub fn instances_dir_resolved(&self, flag_val: &str) -> PathBuf {
        if !flag_val.is_empty() {
            return PathBuf::from(flag_val);
        }
        if let Ok(env) = std::env::var("WARP_INSTANCES_DIR") {
            if !env.is_empty() {
                return PathBuf::from(env);
            }
        }
        if !self.instances_dir.is_empty() {
            return PathBuf::from(&self.instances_dir);
        }
        PathBuf::from("./instances")
    }

    /// Whether this deployment uses the MASQUE engine.
    pub fn is_masque(&self) -> bool {
        self.warp_transport == "masque"
    }

    /// The SOCKS5 port for instance `i` in credential mode.
    pub fn account_port(&self, i: i64) -> i64 {
        self.base_port + i
    }

    /// The SOCKS5 port for instance `i` in whitelist mode.
    pub fn whitelist_port(&self, i: i64) -> i64 {
        self.base_port + self.whitelist_port_offset + i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ip_whitelist_accepts_both_on_disk_shapes() {
        // Merge each shape into a full config so only the whitelist differs.
        let base = serde_json::to_value(Config::default()).unwrap();

        let mut plain_json = base.clone();
        plain_json["ip_whitelist"] = serde_json::json!(["1.2.3.4", "10.0.0.0/8"]);
        let plain: Config = serde_json::from_value(plain_json).expect("plain list");
        assert_eq!(plain.ip_whitelist, vec!["1.2.3.4", "10.0.0.0/8"]);

        // Node: objects carrying provenance the console shows.
        let mut node_json = base;
        node_json["ip_whitelist"] = serde_json::json!([
            {"ip": "1.2.3.4", "remark": "", "region": "中国 浙江"},
            {"ip": "172.17.0.0/16"}
        ]);
        let node: Config = serde_json::from_value(node_json).expect("node object list");
        assert_eq!(node.ip_whitelist, vec!["1.2.3.4", "172.17.0.0/16"]);
    }

    #[test]
    fn ip_whitelist_skips_blank_entries() {
        let mut json = serde_json::to_value(Config::default()).unwrap();
        json["ip_whitelist"] = serde_json::json!(["", "1.2.3.4", "  "]);
        let c: Config = serde_json::from_value(json).expect("must parse");
        assert_eq!(c.ip_whitelist, vec!["1.2.3.4"]);
    }

    #[test]
    fn defaults_match_the_go_version() {
        let c = Config::default();
        assert_eq!(c.port, 4433);
        assert_eq!(c.base_port, 10010);
        assert_eq!(c.max_instances, 200);
        assert_eq!(c.whitelist_port_offset, 1000);
        assert_eq!(c.warp_transport, "masque");
        assert_eq!(c.sticky_minutes, 10);
        assert_eq!(c.tunnel_keepalive_seconds, 30);
        assert_eq!(c.connect_port, 443);
        assert_eq!(c.dns_servers, vec!["1.1.1.1", "8.8.8.8"]);
    }

    /// The real production `config.json` must parse.
    #[test]
    fn parses_the_repository_config() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../config.json"
        );
        let Ok(cfg) = Config::load(path) else {
            // Not every checkout has it next to the crate; skip rather than fail.
            return;
        };
        assert_eq!(cfg.resident_count, 150);
        assert_eq!(cfg.base_port, 10010);
        assert!(cfg.is_masque());
        assert_eq!(cfg.masque_endpoint_candidates.len(), 8);
    }

    #[test]
    fn rejects_non_masque_transport() {
        let mut c = Config::default();
        c.warp_transport = "wireguard".into();
        let err = c.validate().unwrap_err();
        assert!(err.to_string().contains("masque"), "got: {err}");
    }

    #[test]
    fn rejects_resident_count_above_max() {
        let mut c = Config::default();
        c.resident_count = 500;
        c.max_instances = 200;
        assert!(c.validate().is_err());
    }

    #[test]
    fn rejects_port_overflow() {
        let mut c = Config::default();
        // 65000 + 200 + 1000 = 66200 > 65535.
        c.base_port = 65_000;
        c.max_instances = 200;
        c.whitelist_port_offset = 1000;
        assert!(c.validate().is_err());
    }

    #[test]
    fn accepts_a_config_that_fits() {
        let mut c = Config::default();
        c.base_port = 10_010;
        c.max_instances = 300;
        c.whitelist_port_offset = 1000;
        assert!(c.validate().is_ok());
    }

    #[test]
    fn port_arithmetic() {
        let c = Config::default();
        assert_eq!(c.account_port(0), 10010);
        assert_eq!(c.account_port(7), 10017);
        assert_eq!(c.whitelist_port(0), 11010);
        assert_eq!(c.whitelist_port(7), 11017);
    }

    #[test]
    fn instances_dir_priority() {
        let c = Config::default();
        // flag wins
        assert_eq!(c.instances_dir_resolved("/tmp/x"), PathBuf::from("/tmp/x"));
        // config wins over the default
        let mut c2 = Config::default();
        c2.instances_dir = "/data/instances".into();
        assert_eq!(c2.instances_dir_resolved(""), PathBuf::from("/data/instances"));
    }

    #[test]
    fn json_round_trip_preserves_every_key() {
        let c = Config::default();
        let json = serde_json::to_string_pretty(&c).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(c, back, "every key must survive a round trip");
    }
}

fn default_health_probe_batch() -> i64 {
    8
}

fn default_health_max_conns() -> i64 {
    3
}
