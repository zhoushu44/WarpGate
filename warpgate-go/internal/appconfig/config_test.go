package appconfig

import (
	"os"
	"path/filepath"
	"testing"
)

func TestDefault(t *testing.T) {
	c := Default()
	if c.BasePort != 10010 || c.WhitelistOffset != 1000 || c.ResidentCount != 5 {
		t.Fatalf("默认值与 Node 版不一致: %+v", c)
	}
	if c.WarpTransport != "masque" {
		t.Fatalf("默认 transport 应为 masque")
	}
}

func TestLoadNodeConfig(t *testing.T) {
	// 模拟仓库根目录的真实 config.json（MASQUE 生产配置）
	sample := `{
	  "port": 4433, "max_containers": 1, "resident_count": 20, "sticky_minutes": 10,
	  "auto_rebuild_on_unhealthy": true, "health_check_interval": 600,
	  "bind_address": "0.0.0.0", "mode": "resident", "base_port": 10010,
	  "max_instances": 200, "min_available": 5, "max_concurrent_create": 3,
	  "ip_check_url": "https://api.ipify.org", "ip_check_url_v6": "https://api6.ipify.org",
	  "openai_check_url": "https://www.cloudflare.com", "ip_check_retries": 10,
	  "ip_check_interval": 3000, "max_rebuild_attempts": 3, "cleanup_on_exit": false,
	  "rate_limit_enabled": true, "rate_limit_per_minute": 60,
	  "auto_rotate_on_high_403": true, "cf_403_threshold": 50, "cf_min_check_count": 3,
	  "ip_risk_check": true, "ip_risk_threshold": 70, "ip_risk_ttl": 21600,
	  "ip_risk_check_url": "https://scamalytics.com/ip/", "warp_transport": "masque",
	  "log_level": "info", "public_ip": "auto-detect", "session_cleanup_interval": 30,
	  "team_token": "", "proxy_auth_enabled": true, "proxy_username": "warpuser",
	  "proxy_password": "test-pass-123", "ip_whitelist": [], "whitelist_port_offset": 1000,
	  "api_key": "zs1236547", "masque_http2": true
	}`
	path := filepath.Join(t.TempDir(), "config.json")
	if err := os.WriteFile(path, []byte(sample), 0o644); err != nil {
		t.Fatal(err)
	}
	c, err := Load(path)
	if err != nil {
		t.Fatalf("Node 版配置应可加载: %v", err)
	}
	if c.ResidentCount != 20 || c.ProxyUsername != "warpuser" || c.APIKey != "zs1236547" {
		t.Fatalf("字段解析错误: %+v", c)
	}
	// 默认值填充：Node 版 config.json 未写 dns_servers
	if len(c.DNSServers) == 0 {
		t.Fatalf("dns_servers 默认值未填充")
	}
}

func TestValidateRejectsWireGuard(t *testing.T) {
	c := Default()
	c.WarpTransport = "wireguard"
	if err := c.Validate(); err == nil {
		t.Fatalf("v1 应拒绝 wireguard 模式")
	}
}

func TestValidatePortRange(t *testing.T) {
	c := Default()
	c.BasePort = 65530
	c.MaxInstances = 200
	c.WhitelistOffset = 1000
	if err := c.Validate(); err == nil {
		t.Fatalf("端口越界应报错")
	}
}
