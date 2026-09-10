// Package appconfig 加载与 Node 版 WarpGate 完全兼容的 config.json。
package appconfig

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
)

// Config 与 Node 版 server.js 的 config.json 键一一对应。
type Config struct {
	Port           int    `json:"port"`
	MaxContainers  int    `json:"max_containers"`
	ResidentCount  int    `json:"resident_count"`
	StickyMinutes  int    `json:"sticky_minutes"`
	AutoRebuild    bool   `json:"auto_rebuild_on_unhealthy"`
	HealthInterval int    `json:"health_check_interval"` // 秒
	BindAddress    string `json:"bind_address"`
	Mode           string `json:"mode"`
	BasePort       int    `json:"base_port"`
	MaxInstances   int    `json:"max_instances"`
	MinAvailable   int    `json:"min_available"`
	MaxConcurrent  int    `json:"max_concurrent_create"`

	IPCheckURL       string `json:"ip_check_url"`
	IPCheckURLV6     string `json:"ip_check_url_v6"`
	OpenAICheckURL   string `json:"openai_check_url"`
	IPCheckRetries   int    `json:"ip_check_retries"`
	IPCheckInterval  int    `json:"ip_check_interval"` // 毫秒
	MaxRebuildTries  int    `json:"max_rebuild_attempts"`
	CleanupOnExit    bool   `json:"cleanup_on_exit"`
	RateLimitEnabled bool   `json:"rate_limit_enabled"`
	RateLimitPerMin  int    `json:"rate_limit_per_minute"`

	AutoRotateOnHigh403 bool   `json:"auto_rotate_on_high_403"`
	CF403Threshold      int    `json:"cf_403_threshold"`   // 百分比
	CFMinCheckCount     int    `json:"cf_min_check_count"` // 最小统计次数
	IPRiskCheck         bool   `json:"ip_risk_check"`      // v2 实装，v1 仅透传
	IPRiskThreshold     int    `json:"ip_risk_threshold"`  // 0-100
	IPRiskTTL           int    `json:"ip_risk_ttl"`        // 秒
	IPRiskCheckURL      string `json:"ip_risk_check_url"`  // v2 实装
	WarpTransport       string `json:"warp_transport"`
	LogLevel            string `json:"log_level"`
	PublicIP            string `json:"public_ip"`
	SessionCleanup      int    `json:"session_cleanup_interval"` // 秒
	TeamToken           string `json:"team_token"`

	ProxyAuthEnabled bool     `json:"proxy_auth_enabled"`
	ProxyUsername    string   `json:"proxy_username"`
	ProxyPassword    string   `json:"proxy_password"`
	IPWhitelist      []string `json:"ip_whitelist"`
	WhitelistOffset  int      `json:"whitelist_port_offset"`
	APIKey           string   `json:"api_key"`
	MasqueHTTP2      bool     `json:"masque_http2"` // v1 不支持 H2 端点，仅透传并在启动时告警

	// v1 新增键
	InstancesDir       string   `json:"instances_dir"`            // 空则取 env WARP_INSTANCES_DIR，再退 ./instances
	TunnelKeepaliveSec int      `json:"tunnel_keepalive_seconds"` // MASQUE QUIC keepalive
	ReconnectMaxSec    int      `json:"tunnel_reconnect_max_seconds"`
	DNSServers         []string `json:"dns_servers"`               // 本地解析目标域名用
	SocksTCPTimeoutSec int      `json:"socks_tcp_timeout_seconds"` // 0 = 不限
	ConnectPort        int      `json:"connect_port"`              // MASQUE 连接端口，默认 443
}

// Default 返回与 Node 版 server.js 默认值一致的配置。
func Default() *Config {
	return &Config{
		Port:           4433,
		MaxContainers:  1,
		ResidentCount:  5,
		StickyMinutes:  10,
		AutoRebuild:    true,
		HealthInterval: 600,
		BindAddress:    "0.0.0.0",
		Mode:           "resident",
		BasePort:       10010,
		MaxInstances:   200,
		MinAvailable:   5,
		MaxConcurrent:  3,

		IPCheckURL:      "https://api.ipify.org",
		IPCheckURLV6:    "https://api6.ipify.org",
		OpenAICheckURL:  "https://www.cloudflare.com",
		IPCheckRetries:  10,
		IPCheckInterval: 3000,
		MaxRebuildTries: 3,

		RateLimitEnabled: true,
		RateLimitPerMin:  60,

		AutoRotateOnHigh403: true,
		CF403Threshold:      50,
		CFMinCheckCount:     3,
		IPRiskCheck:         true,
		IPRiskThreshold:     70,
		IPRiskTTL:           21600,
		IPRiskCheckURL:      "https://scamalytics.com/ip/",
		WarpTransport:       "masque",
		LogLevel:            "info",
		PublicIP:            "auto-detect",
		SessionCleanup:      30,

		ProxyAuthEnabled: true,
		WhitelistOffset:  1000,
		MasqueHTTP2:      true,

		TunnelKeepaliveSec: 30,
		ReconnectMaxSec:    60,
		DNSServers:         []string{"1.1.1.1", "8.8.8.8"},
		ConnectPort:        443,
	}
}

// Load 读取 config.json 并填充默认值。
func Load(path string) (*Config, error) {
	cfg := Default()
	raw, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("读取配置失败: %w", err)
	}
	if err := json.Unmarshal(raw, cfg); err != nil {
		return nil, fmt.Errorf("解析配置失败: %w", err)
	}
	if err := cfg.Validate(); err != nil {
		return nil, err
	}
	return cfg, nil
}

// Validate 校验 v1 的硬性约束。
func (c *Config) Validate() error {
	// v1 仅支持 MASQUE（数据面为进程内 usque L4Proxy）；WG 嵌入是 v2 范围
	if c.WarpTransport != "masque" {
		return fmt.Errorf("warp_transport=%q: 嵌入版 v1 仅支持 masque（wireguard 嵌入在 v2 计划中）", c.WarpTransport)
	}
	if c.ResidentCount < 0 || c.ResidentCount > c.MaxInstances {
		return fmt.Errorf("resident_count=%d 超出范围 [0,%d]", c.ResidentCount, c.MaxInstances)
	}
	if c.BasePort <= 0 || c.WhitelistOffset < 0 {
		return fmt.Errorf("base_port/whitelist_port_offset 非法")
	}
	if c.MaxInstances > 0 && c.BasePort+c.MaxInstances+c.WhitelistOffset > 65535 {
		return fmt.Errorf("base_port(%d)+max_instances(%d)+offset(%d) 超出端口范围", c.BasePort, c.MaxInstances, c.WhitelistOffset)
	}
	return nil
}

// InstancesDirResolved 解析实例目录：flag/env/config/默认值。
func (c *Config) InstancesDirResolved(flagVal string) string {
	if flagVal != "" {
		return flagVal
	}
	if env := os.Getenv("WARP_INSTANCES_DIR"); env != "" {
		return env
	}
	if c.InstancesDir != "" {
		return c.InstancesDir
	}
	return "./instances"
}

// Keepalive / DNSTimeout 等便捷取值。
func (c *Config) KeepaliveSec() int {
	if c.TunnelKeepaliveSec <= 0 {
		return 30
	}
	return c.TunnelKeepaliveSec
}

func (c *Config) IPCheckIntervalSec() float64 {
	return float64(max(c.IPCheckInterval, 500)) / 1000.0
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

// ParseBool 宽松布尔解析（供 API 层使用）。
func ParseBool(s string) bool {
	b, _ := strconv.ParseBool(s)
	return b
}
