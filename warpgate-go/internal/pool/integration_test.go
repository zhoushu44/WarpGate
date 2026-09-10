//go:build integration

// 集成冒烟测试：真实 Cloudflare 注册 + 真实 MASQUE 隧道 + SOCKS5 端到端。
// 运行：go test -tags integration -run TestIntegration -v ./internal/pool/ -timeout 300s
package pool

import (
	"context"
	"crypto/tls"
	"fmt"
	"io"
	"net"
	"os"
	"testing"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/store"
	"warpgate/internal/wgtunnel"
)

func intCfg(t *testing.T) *appconfig.Config {
	t.Helper()
	cfg := appconfig.Default()
	cfg.ResidentCount = 1
	cfg.BasePort = 39110
	cfg.WhitelistOffset = 1000
	cfg.IPCheckRetries = 5
	cfg.HealthInterval = 600
	cfg.MaxConcurrent = 1
	cfg.InstancesDir = t.TempDir()
	return cfg
}

func TestIntegrationCreateAndTunnel(t *testing.T) {
	if os.Getenv("WARP_IT") == "" {
		t.Skip("设置 WARP_IT=1 运行集成测试（需要可直连 Cloudflare 的网络）")
	}
	cfg := intCfg(t)
	m := New(cfg)
	ctx := context.Background()

	// ① 冷创建：真实注册
	start := time.Now()
	if err := m.Create(ctx, 0); err != nil {
		t.Fatalf("创建实例失败: %v", err)
	}
	t.Logf("① 实例创建耗时 %s", time.Since(start).Round(time.Millisecond))

	inst, ok := m.Get(0)
	if !ok || inst.Status != StatusRunning {
		t.Fatalf("实例应为 running, got %v", inst)
	}
	if inst.Meta.IPv6 == "" {
		t.Fatalf("meta.json 应记录出口 IPv6")
	}
	t.Logf("② 账号分配 IPv6: %s", inst.Meta.IPv6)

	// usque.json 与 meta.json 落盘检查
	dir := store.Dir(cfg.InstancesDir, 0)
	if _, err := wgtunnel.LoadUsqueConfig(store.UsqueConfPath(dir)); err != nil {
		t.Fatalf("usque.json 应有效: %v", err)
	}
	meta, err := store.ReadMeta(dir)
	if err != nil || meta.IPv6 == "" {
		t.Fatalf("meta.json 应有效: %v %v", meta, err)
	}

	// ③ 经 SOCKS5（账密模式）取出口 IPv6
	gotIP, err := fetchViaSOCKS(t, cfg, 0, "https://api6.ipify.org")
	if err != nil {
		t.Fatalf("经隧道取出口 IP 失败: %v", err)
	}
	t.Logf("③ SOCKS5 出口 IPv6: %s", gotIP)
	if !containsColon(gotIP) {
		t.Fatalf("出口应为 IPv6: %q", gotIP)
	}
	if inst.Meta.IPv6 != "" && !sameIPv6Block(gotIP, inst.Meta.IPv6) {
		t.Logf("警告：出口 %s 与账号分配 %s 不同段（记录但不失败，CF 可能分配段内其他地址）", gotIP, inst.Meta.IPv6)
	}

	// ④ 管理台 HTTP 探测（健康检查路径）
	res := m.ProbeOnce(inst)
	if res.HTTPCode == 0 {
		t.Fatalf("健康探测失败: %s", res.Error)
	}
	t.Logf("④ 健康探测 HTTP:%d %dms", res.HTTPCode, res.LatencyMS)

	// ⑤ 清理
	if err := m.DeleteInstance(0); err != nil {
		t.Fatalf("删除失败: %v", err)
	}
	if _, err := os.Stat(dir); !os.IsNotExist(err) {
		t.Fatalf("实例目录应已删除")
	}
	t.Logf("⑤ 实例已删除")
}

func TestIntegrationTwoInstancesDistinctIPv6(t *testing.T) {
	if os.Getenv("WARP_IT") == "" {
		t.Skip("设置 WARP_IT=1 运行集成测试")
	}
	cfg := intCfg(t)
	cfg.BasePort = 39210
	m := New(cfg)
	ctx := context.Background()
	defer func() {
		_ = m.DeleteInstance(0)
		_ = m.DeleteInstance(1)
	}()

	for i := 0; i < 2; i++ {
		if err := m.Create(ctx, i); err != nil {
			t.Fatalf("实例 %d 创建失败: %v", i, err)
		}
	}
	ips := map[string]bool{}
	for i := 0; i < 2; i++ {
		ip, err := fetchViaSOCKS(t, cfg, i, "https://api6.ipify.org")
		if err != nil {
			t.Fatalf("实例 %d 取出口失败: %v", i, err)
		}
		t.Logf("实例 %d 出口: %s", i, ip)
		ips[ip] = true
	}
	if len(ips) != 2 {
		t.Fatalf("两实例出口 IPv6 应互不相同: %v", ips)
	}
}

// fetchViaSOCKS 走实例 i 的账密 SOCKS5 端口请求 url，返回响应体。
func fetchViaSOCKS(t *testing.T, cfg *appconfig.Config, idx int, url string) (string, error) {
	t.Helper()
	addr := fmt.Sprintf("127.0.0.1:%d", cfg.BasePort+idx)
	var d net.Dialer
	up, err := d.DialContext(context.Background(), "tcp", addr)
	if err != nil {
		return "", err
	}
	defer up.Close()
	_ = up.SetDeadline(time.Now().Add(30 * time.Second))

	// SOCKS5 握手（账密）
	if _, err := up.Write([]byte{0x05, 0x01, 0x02}); err != nil {
		return "", err
	}
	rep := make([]byte, 2)
	if _, err := io.ReadFull(up, rep); err != nil {
		return "", err
	}
	if rep[1] != 0x02 {
		return "", fmt.Errorf("服务端未选账密方法: 0x%02x", rep[1])
	}
	un := cfg.ProxyUsername
	pw := cfg.ProxyPassword
	_, _ = up.Write([]byte{0x01, byte(len(un))})
	up.Write([]byte(un))
	up.Write([]byte{byte(len(pw))})
	up.Write([]byte(pw))
	rep2 := make([]byte, 2)
	if _, err := io.ReadFull(up, rep2); err != nil {
		return "", err
	}
	if rep2[1] != 0x00 {
		return "", fmt.Errorf("账密被拒: 0x%02x", rep2[1])
	}

	// CONNECT api6.ipify.org:443（域名目标，走服务端本地解析）
	host := "api6.ipify.org"
	req := []byte{0x05, 0x01, 0x00, 0x03, byte(len(host))}
	req = append(req, host...)
	req = append(req, 0x01, 0xbb)
	if _, err := up.Write(req); err != nil {
		return "", err
	}
	rep3 := make([]byte, 10)
	if _, err := io.ReadFull(up, rep3); err != nil {
		return "", err
	}
	if rep3[1] != 0x00 {
		return "", fmt.Errorf("CONNECT 被拒: 0x%02x", rep3[1])
	}

	// TLS + GET
	_ = up.SetDeadline(time.Time{})
	tlsConn := tls.Client(up, &tls.Config{ServerName: "api6.ipify.org"})
	get := "GET / HTTP/1.1\r\nHost: api6.ipify.org\r\nUser-Agent: curl/8.0\r\nConnection: close\r\n\r\n"
	if _, err := tlsConn.Write([]byte(get)); err != nil {
		return "", err
	}
	body, err := io.ReadAll(tlsConn)
	if err != nil && len(body) == 0 {
		return "", err
	}
	// 提取 body（跳过 HTTP 头）
	s := string(body)
	if i := indexSubstring(s, "\r\n\r\n"); i >= 0 {
		s = s[i+4:]
	}
	return trimSpaceAll(s), nil
}

func containsColon(s string) bool {
	for i := 0; i < len(s); i++ {
		if s[i] == ':' {
			return true
		}
	}
	return false
}

func indexSubstring(s, sub string) int {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}

func trimSpaceAll(s string) string {
	start, end := 0, len(s)
	for start < end && (s[start] == ' ' || s[start] == '\n' || s[start] == '\r' || s[start] == '\t') {
		start++
	}
	for end > start && (s[end-1] == ' ' || s[end-1] == '\n' || s[end-1] == '\r' || s[end-1] == '\t') {
		end--
	}
	return s[start:end]
}
