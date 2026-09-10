package apiserver

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"encoding/base64"
	"encoding/json"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/pool"
	"warpgate/internal/store"
	"warpgate/internal/wgtunnel"
)

// TestAPIEndToEnd 离线验证管理面 API 兼容契约：
// 登录 / stats / containers / acquire(RR+粘性) / release / check / whitelist。
// 隧道拨号注入本地 echo，不依赖外网。
func TestAPIEndToEnd(t *testing.T) {
	t.Setenv("WARP_CONFIG_PATH", t.TempDir()+"/config.json") // 热更新落盘隔离
	cfg := appconfig.Default()
	cfg.ResidentCount = 2
	cfg.BasePort = 38210
	cfg.WhitelistOffset = 1000
	cfg.InstancesDir = t.TempDir()
	cfg.APIKey = "testkey"
	cfg.ProxyUsername = "u"
	cfg.ProxyPassword = "p"

	// 本地 echo 上游
	echoLn, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	defer echoLn.Close()
	go func() {
		for {
			c, err := echoLn.Accept()
			if err != nil {
				return
			}
			go func(c net.Conn) {
				_, _ = io.Copy(c, c)
				_ = c.Close()
			}(c)
		}
	}()

	m := pool.New(cfg)
	for i := 0; i < 2; i++ {
		dir := store.Dir(cfg.InstancesDir, i)
		if err := fakeUsqueConf(dir, i); err != nil {
			t.Fatal(err)
		}
	}
	m.Boot(context.Background())
	if m.HealthyCount() != 2 {
		t.Fatalf("应有两个健康实例, got %d", m.HealthyCount())
	}
	for _, inst := range m.All() {
		inst.Tunnel.SetDialOverride(func(ctx context.Context, network, target string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "tcp", echoLn.Addr().String())
		})
	}

	ts := httptest.NewServer(New(cfg, m))
	defer ts.Close()

	call := func(method, path string, body any, wantCode string) map[string]any {
		t.Helper()
		var rd io.Reader
		if body != nil {
			raw, _ := json.Marshal(body)
			rd = stringsReader(string(raw))
		}
		req, err := http.NewRequest(method, ts.URL+path, rd)
		if err != nil {
			t.Fatal(err)
		}
		req.Header.Set("X-API-Key", "testkey")
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		var out map[string]any
		if err := json.NewDecoder(resp.Body).Decode(&out); err != nil {
			t.Fatalf("%s %s 响应非 JSON: %v", method, path, err)
		}
		if out["code"] != wantCode {
			t.Fatalf("%s %s -> code=%v want=%v (msg=%v)", method, path, out["code"], wantCode, out["msg"])
		}
		return out
	}

	// 登录：正确/错误
	call(http.MethodPost, "/api/login", map[string]string{"key": "testkey"}, "00000")
	resp, err := http.Post(ts.URL+"/api/login", "application/json", stringsReader(`{"key":"wrong"}`))
	if err != nil {
		t.Fatal(err)
	}
	resp.Body.Close()
	if resp.StatusCode != 401 {
		t.Fatalf("错误 Key 应 401, got %d", resp.StatusCode)
	}

	// stats / containers
	stats := call(http.MethodGet, "/api/stats", nil, "00000")["data"].(map[string]any)
	if stats["instance_count"].(float64) != 2 || stats["mode"] != "embedded" || stats["transport"] != "masque" {
		t.Fatalf("stats 不符: %v", stats)
	}
	containers := call(http.MethodGet, "/api/containers", nil, "00000")["data"].([]any)
	if len(containers) != 2 {
		t.Fatalf("containers 应有 2 项")
	}
	c0 := containers[0].(map[string]any)
	if c0["name"] != "warp-0" || c0["port"].(float64) != float64(cfg.BasePort) {
		t.Fatalf("containers[0] 字段不符: %v", c0)
	}

	// acquire：RR 轮转
	addresses := map[string]bool{}
	for i := 0; i < 2; i++ {
		d := call(http.MethodGet, "/api/pool/acquire", nil, "00000")["data"].(map[string]any)
		addresses[d["proxy"].(string)] = true
		if d["exit_ipv6"] == "" {
			t.Fatalf("acquire 应带 exit_ipv6: %v", d)
		}
	}
	if len(addresses) != 2 {
		t.Fatalf("RR 应轮转不同端口: %v", addresses)
	}

	// acquire 粘性
	call(http.MethodGet, "/api/pool/acquire?sticky=1&session_id=s1", nil, "00000")
	st := call(http.MethodGet, "/api/pool/status", nil, "00000")["data"].(map[string]any)
	if st["sticky_sessions"].(float64) != 1 {
		t.Fatalf("粘性会话应计数: %v", st)
	}
	call(http.MethodPost, "/api/pool/release", map[string]string{"session_id": "s1"}, "00000")
	call(http.MethodPost, "/api/pool/release", map[string]string{"session_id": "s1"}, "I0002") // 重复释放

	// stop 一个实例 → pool/status.healthy=1
	idx := 1.0
	call(http.MethodPost, "/api/stop", map[string]any{"index": idx}, "00000")
	st = call(http.MethodGet, "/api/pool/status", nil, "00000")["data"].(map[string]any)
	if st["healthy"].(float64) != 1 {
		t.Fatalf("停止后 healthy 应为 1: %v", st)
	}
	// 再 stop 报错
	call(http.MethodPost, "/api/stop", map[string]any{"index": idx}, "R0001")
	// start 恢复
	call(http.MethodPost, "/api/start", map[string]any{"index": idx}, "00000")
	st = call(http.MethodGet, "/api/pool/status", nil, "00000")["data"].(map[string]any)
	if st["healthy"].(float64) != 2 {
		t.Fatalf("start 后 healthy 应为 2: %v", st)
	}

	// 白名单更新（写回 config 隔离目录）
	call(http.MethodPost, "/api/whitelist", map[string]any{"ip": "10.0.0.1"}, "00000")
}

func fakeUsqueConf(dir string, i int) error {
	key, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		return err
	}
	sec1, err := x509.MarshalECPrivateKey(key)
	if err != nil {
		return err
	}
	pkix, err := x509.MarshalPKIXPublicKey(&key.PublicKey)
	if err != nil {
		return err
	}
	peerPub := pem.EncodeToMemory(&pem.Block{Type: "PUBLIC KEY", Bytes: pkix})
	uc := &wgtunnel.UsqueConfig{
		PrivateKey:     base64.StdEncoding.EncodeToString(sec1),
		EndpointV4:     "162.159.193.10",
		EndpointH2V4:   "162.159.198.2",
		EndpointPubKey: string(peerPub),
		ID:             fmt.Sprintf("fake-%d-%d", i, time.Now().UnixNano()),
		AccessToken:    "fake-token",
		IPv4:           "172.16.0.2/32",
		IPv6:           "2606:4700:110:8c9f::1/128",
	}
	if err := uc.Save(store.UsqueConfPath(dir)); err != nil {
		return err
	}
	return store.WriteMeta(dir, &store.Meta{Index: i, Name: fmt.Sprintf("warp-%d", i), Transport: "masque", IPv6: uc.IPv6, Status: "stopped"})
}

func stringsReader(s string) io.Reader { return strings.NewReader(s) }
