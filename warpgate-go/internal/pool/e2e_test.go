package pool

import (
	"context"
	"crypto/ecdsa"
	"crypto/elliptic"
	"crypto/rand"
	"crypto/x509"
	"encoding/base64"
	"encoding/pem"
	"fmt"
	"io"
	"net"
	"testing"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/store"
	"warpgate/internal/wgtunnel"
)

// TestControlPlaneEndToEnd 离线验证全链路：
// 实例暖启动 → SOCKS5 账密/白名单入站 → 调度 acquire → HTTP API。
// 隧道拨号注入本地 echo 服务，替代真实 MASQUE（需外网的路径由 integration 测试覆盖）。
func TestControlPlaneEndToEnd(t *testing.T) {
	cfg := appconfig.Default()
	cfg.ResidentCount = 2
	cfg.BasePort = 38110
	cfg.WhitelistOffset = 1000
	cfg.InstancesDir = t.TempDir()
	cfg.ProxyAuthEnabled = true
	cfg.ProxyUsername = "u"
	cfg.ProxyPassword = "p"

	// 本地 echo 上游（替代隧道对端）
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
				io.Copy(c, c)
				c.Close()
			}(c)
		}
	}()

	m := New(cfg)
	// 预置两个"已有账号"实例目录（暖启动，不联网注册）
	for i := 0; i < 2; i++ {
		dir := store.Dir(cfg.InstancesDir, i)
		if err := writeFakeUsqueConf(dir); err != nil {
			t.Fatal(err)
		}
	}

	// Boot：暖启动两实例（Tunnel.Start 离线安全；拨号由钩子接管）
	m.Boot(context.Background())
	if m.HealthyCount() != 2 {
		t.Fatalf("应有两个健康实例, got %d", m.HealthyCount())
	}

	// 拨号钩子：全部指向本地 echo
	for _, inst := range m.All() {
		inst.Tunnel.SetDialOverride(func(ctx context.Context, network, target string) (net.Conn, error) {
			var d net.Dialer
			return d.DialContext(ctx, "tcp", echoLn.Addr().String())
		})
	}

	// ① SOCKS5 账密入站 → echo 数据往返
	for i := 0; i < 2; i++ {
		rep := socks5EchoRoundtrip(t, "127.0.0.1", cfg.BasePort+i, "u", "p", fmt.Sprintf("ping-%d", i))
		if rep != fmt.Sprintf("ping-%d", i) {
			t.Fatalf("实例 %d echo 不符: %q", i, rep)
		}
	}

	// ② 白名单入站：白名单外来源被拒（测试来源 127.0.0.1 不在白名单）
	if got := socks5TryWhitelist(t, "127.0.0.1", cfg.BasePort+cfg.WhitelistOffset); got {
		t.Fatalf("空白名单应拒绝所有来源")
	}

	// ③ 调度：RR 轮转 + 粘性
	a1, _ := m.Acquire(false, "")
	a2, _ := m.Acquire(false, "")
	if a1.Index == a2.Index {
		t.Fatalf("RR 应轮转: %d %d", a1.Index, a2.Index)
	}
	s1, _ := m.Acquire(true, "sess")
	s2, _ := m.Acquire(true, "sess")
	if s1.Index != s2.Index {
		t.Fatalf("粘性应固定: %d %d", s1.Index, s2.Index)
	}
	// HTTP API 的兼容性由 internal/apiserver 的测试覆盖（避免 import 环）
}

// writeFakeUsqueConf 生成可被 wgtunnel 解析的 usque.json（真实 EC 密钥材料，离线安全）。
func writeFakeUsqueConf(dir string) error {
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
		ID:             "fake-" + fmt.Sprint(time.Now().UnixNano()),
		AccessToken:    "fake-token",
		IPv4:           "172.16.0.2/32",
		IPv6:           "2606:4700:110:8c9f::1/128",
	}
	if err := uc.Save(store.UsqueConfPath(dir)); err != nil {
		return err
	}
	return store.WriteMeta(dir, &store.Meta{Index: 0, Name: "warp-0", Transport: "masque", IPv6: uc.IPv6, Status: "stopped"})
}

// socks5EchoRoundtrip 完整 SOCKS5 客户端握手 + 双向 echo。
func socks5EchoRoundtrip(t *testing.T, host string, port int, user, pass, payload string) string {
	t.Helper()
	conn, err := net.DialTimeout("tcp", fmt.Sprintf("%s:%d", host, port), 3*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(5 * time.Second))

	if _, err := conn.Write([]byte{0x05, 0x01, 0x02}); err != nil {
		t.Fatal(err)
	}
	rep := make([]byte, 2)
	if _, err := io.ReadFull(conn, rep); err != nil || rep[1] != 0x02 {
		t.Fatalf("应选账密方法: %v %v", rep, err)
	}
	_, _ = conn.Write([]byte{0x01, byte(len(user))})
	conn.Write([]byte(user))
	conn.Write([]byte{byte(len(pass))})
	conn.Write([]byte(pass))
	st := make([]byte, 2)
	if _, err := io.ReadFull(conn, st); err != nil || st[1] != 0x00 {
		t.Fatalf("账密应通过: %v %v", st, err)
	}
	// CONNECT 目标任意（拨号被钩子接管）
	target := "echo.test:80"
	req := []byte{0x05, 0x01, 0x00, 0x03, byte(len(target))}
	req = append(req, target...)
	req = append(req, 0x00, 0x50)
	if _, err := conn.Write(req); err != nil {
		t.Fatal(err)
	}
	rep3 := make([]byte, 10)
	if _, err := io.ReadFull(conn, rep3); err != nil || rep3[1] != 0x00 {
		t.Fatalf("CONNECT 应成功: %v", rep3)
	}
	// echo 往返
	if _, err := conn.Write([]byte(payload)); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, len(payload))
	if _, err := io.ReadFull(conn, buf); err != nil {
		t.Fatal(err)
	}
	return string(buf)
}

// socks5TryWhitelist 尝试白名单端口无认证握手，返回是否被接受。
func socks5TryWhitelist(t *testing.T, host string, port int) bool {
	t.Helper()
	conn, err := net.DialTimeout("tcp", fmt.Sprintf("%s:%d", host, port), 3*time.Second)
	if err != nil {
		return false
	}
	defer conn.Close()
	_ = conn.SetDeadline(time.Now().Add(2 * time.Second))
	if _, err := conn.Write([]byte{0x05, 0x01, 0x00}); err != nil {
		return false
	}
	rep := make([]byte, 2)
	if _, err := io.ReadFull(conn, rep); err != nil {
		return false // 被直接断开 = 拒绝
	}
	return true
}
