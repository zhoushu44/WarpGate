package wgtunnel

import (
	"path/filepath"
	"testing"
)

func TestUsqueConfigRoundTrip(t *testing.T) {
	// 与 usque CLI 保存格式一致的字段
	src := &UsqueConfig{
		PrivateKey:     "MHcCAQEEIIabcdef",
		EndpointV4:     "162.159.193.10",
		EndpointV6:     "2606:4700:110:8884:8b4e:dead:beef:face",
		EndpointH2V4:   "162.159.198.2",
		EndpointH2V6:   "",
		EndpointPubKey: "-----BEGIN PUBLIC KEY-----\nMFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE\n-----END PUBLIC KEY-----",
		ID:             "device-id-1",
		AccessToken:    "token-1",
		IPv4:           "172.16.0.2/32",
		IPv6:           "2606:4700:110:88bd:face:0:abcd:1234/128",
	}
	path := filepath.Join(t.TempDir(), "usque.json")
	if err := src.Save(path); err != nil {
		t.Fatal(err)
	}
	got, err := LoadUsqueConfig(path)
	if err != nil {
		t.Fatal(err)
	}
	if got.ID != src.ID || got.IPv6 != src.IPv6 || got.EndpointV4 != src.EndpointV4 {
		t.Fatalf("往返不一致: %+v vs %+v", got, src)
	}
}

func TestSelectUDPEndpoint(t *testing.T) {
	c := &UsqueConfig{EndpointV4: "162.159.193.10", EndpointV6: "2606:4700:110:8884::1"}
	ep, err := c.SelectUDPEndpoint(false, 443)
	if err != nil || ep.Port != 443 || ep.IP.String() != "162.159.193.10" {
		t.Fatalf("v4 端点错误: %v %v", ep, err)
	}
	ep, err = c.SelectUDPEndpoint(true, 443)
	if err != nil || ep.IP.String() != "2606:4700:110:8884::1" {
		t.Fatalf("v6 端点错误: %v %v", ep, err)
	}
	// [v6]:port 形式（手工编辑容错）
	c.EndpointV4 = ""
	c.EndpointV6 = "[2606:4700::1]:443"
	if _, err := c.SelectUDPEndpoint(true, 443); err != nil {
		t.Fatalf("[v6]:port 形式应解析: %v", err)
	}
	c.EndpointV6 = "2606:4700:110:8884::1"
	c.EndpointV4 = "1.2.3.4:500"
	ep, err = c.SelectUDPEndpoint(false, 443)
	if err != nil || ep.Port != 500 {
		t.Fatalf("ip:port 形式应解析端口: %v %v", ep, err)
	}
	// 缺失端点
	empty := &UsqueConfig{}
	if _, err := empty.SelectUDPEndpoint(false, 443); err == nil {
		t.Fatalf("缺少端点应报错")
	}
}

func TestTrimHelpers(t *testing.T) {
	if got := stringsTrimSuffix("162.159.193.10:0", ":0"); got != "162.159.193.10" {
		t.Fatalf("v4 trim 错误: %q", got)
	}
	if got := stringsTrimBracketsPort("[2606:4700:110::1]:0"); got != "2606:4700:110::1" {
		t.Fatalf("v6 trim 错误: %q", got)
	}
}

func TestLoadMissingFile(t *testing.T) {
	if _, err := LoadUsqueConfig(filepath.Join(t.TempDir(), "nope.json")); err == nil {
		t.Fatalf("缺失文件应报错")
	}
}
