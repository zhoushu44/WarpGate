package socks

import (
	"context"
	"io"
	"net"
	"strings"
	"testing"
	"time"
)

// stubDial 假出站：返回一个内存管道连接。
func stubDial(respond string) DialFunc {
	return func(ctx context.Context, network, target string) (net.Conn, error) {
		a, b := net.Pipe()
		go func() {
			// 先写响应（net.Pipe 是同步的，服务端中继读 b 时解除阻塞），再丢弃后续输入
			_, _ = a.Write([]byte(respond))
			_, _ = io.Copy(io.Discard, a)
		}()
		return b, nil
	}
}

func startServer(t *testing.T, srv *Server) string {
	t.Helper()
	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	srv.Listener = ln
	go func() { _ = srv.Serve() }()
	t.Cleanup(func() { _ = srv.Close() })
	return ln.Addr().String()
}

// handshake 完成 SOCKS5 方法协商，返回后续原始连接。
func handshake(t *testing.T, addr string, methods []byte) net.Conn {
	t.Helper()
	conn, err := net.DialTimeout("tcp", addr, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { _ = conn.Close() })
	greet := append([]byte{0x05, byte(len(methods))}, methods...)
	if _, err := conn.Write(greet); err != nil {
		t.Fatal(err)
	}
	rep := make([]byte, 2)
	if _, err := io.ReadFull(conn, rep); err != nil {
		t.Fatal(err)
	}
	return conn
}

func sendConnect(t *testing.T, conn net.Conn, target string) []byte {
	t.Helper()
	host, portS, _ := net.SplitHostPort(target)
	port := 0
	for i := 0; i < len(portS); i++ {
		port = port*10 + int(portS[i]-'0')
	}
	req := []byte{0x05, 0x01, 0x00, atypDomain, byte(len(host))}
	req = append(req, []byte(host)...)
	req = append(req, byte(port>>8), byte(port&0xff))
	if _, err := conn.Write(req); err != nil {
		t.Fatal(err)
	}
	rep := make([]byte, 10)
	if _, err := io.ReadFull(conn, rep); err != nil {
		t.Fatal(err)
	}
	return rep
}

func TestNoAuthConnect(t *testing.T) {
	addr := startServer(t, &Server{Dial: stubDial("hello")})
	conn := handshake(t, addr, []byte{methodNoAuth})
	rep := sendConnect(t, conn, "example.com:80")
	if rep[1] != 0x00 {
		t.Fatalf("CONNECT 应成功，REP=0x%02x", rep[1])
	}
	buf := make([]byte, 5)
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	n, _ := io.ReadFull(conn, buf)
	if string(buf[:n]) != "hello" {
		t.Fatalf("应读到上游响应, got %q", string(buf[:n]))
	}
}

func TestAuthSuccessAndFailure(t *testing.T) {
	addr := startServer(t, &Server{RequireAuth: true, Username: "u1", Password: "p1", Dial: stubDial("")})

	// 正确账密
	conn := handshake(t, addr, []byte{methodUserPass})
	_, _ = conn.Write([]byte{0x01, 2, 'u', '1', 2, 'p', '1'})
	rep := make([]byte, 2)
	if _, err := io.ReadFull(conn, rep); err != nil || rep[1] != 0x00 {
		t.Fatalf("正确账密应通过: %v %v", rep, err)
	}

	// 错误账密
	conn2 := handshake(t, addr, []byte{methodUserPass})
	_, _ = conn2.Write([]byte{0x01, 2, 'u', '1', 2, 'x', 'x'})
	rep2 := make([]byte, 2)
	if _, err := io.ReadFull(conn2, rep2); err != nil || rep2[1] != 0x01 {
		t.Fatalf("错误账密应拒绝: %v %v", rep2, err)
	}
}

func TestAuthRequiredButClientNoAuth(t *testing.T) {
	addr := startServer(t, &Server{RequireAuth: true, Username: "u", Password: "p", Dial: stubDial("")})
	// 服务端应回 0xFF（无可接受方法）
	conn, err := net.DialTimeout("tcp", addr, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_, _ = conn.Write([]byte{0x05, 1, methodNoAuth})
	buf := make([]byte, 2)
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	if _, err := io.ReadFull(conn, buf); err != nil {
		t.Fatal(err)
	}
	if buf[1] != methodNoAccept {
		t.Fatalf("仅提供 NO-AUTH 应被拒 0xFF, got 0x%02x", buf[1])
	}
}

func TestUDPCommandRejected(t *testing.T) {
	addr := startServer(t, &Server{Dial: stubDial("")})
	conn := handshake(t, addr, []byte{methodNoAuth})
	// UDP ASSOCIATE
	_, _ = conn.Write([]byte{0x05, 0x03, 0x00, atypIPv4, 0, 0, 0, 0, 0, 0})
	rep := make([]byte, 10)
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	if _, err := io.ReadFull(conn, rep); err != nil {
		t.Fatal(err)
	}
	if rep[1] != cmdNotSupport {
		t.Fatalf("UDP 应返回 0x07, got 0x%02x", rep[1])
	}
}

func TestWhitelistDeny(t *testing.T) {
	_, ipnet, _ := net.ParseCIDR("10.0.0.0/8")
	addr := startServer(t, &Server{Whitelist: []*net.IPNet{ipnet}, Dial: stubDial("")})
	// 测试来源是 127.0.0.1，不在 10/8 → 握手阶段即被断开
	conn, err := net.DialTimeout("tcp", addr, 2*time.Second)
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	_, _ = conn.Write([]byte{0x05, 1, methodNoAuth})
	buf := make([]byte, 2)
	_ = conn.SetReadDeadline(time.Now().Add(2 * time.Second))
	_, readErr := io.ReadFull(conn, buf)
	if readErr == nil {
		t.Fatalf("非白名单来源应被直接断开")
	}
}

func TestParseWhitelist(t *testing.T) {
	wl, err := ParseWhitelist([]string{"1.2.3.4", "10.0.0.0/8", "::1"})
	if err != nil {
		t.Fatal(err)
	}
	if len(wl) != 3 {
		t.Fatalf("应解析出 3 条, got %d", len(wl))
	}
	if !ipAllowed(net.ParseIP("1.2.3.4"), wl) || !ipAllowed(net.ParseIP("10.9.9.9"), wl) || !ipAllowed(net.ParseIP("::1"), wl) {
		t.Fatalf("白名单命中判断错误")
	}
	if ipAllowed(net.ParseIP("8.8.8.8"), wl) {
		t.Fatalf("8.8.8.8 不应在白名单内")
	}
	if _, err := ParseWhitelist([]string{"bad_entry!"}); err == nil {
		t.Fatalf("非法条目应报错")
	}
}

func TestReadTargetIPv6(t *testing.T) {
	raw := []byte{atypIPv6}
	raw = append(raw, net.ParseIP("2606:4700::1111").To16()...)
	raw = append(raw, 0x01, 0xbb) // 端口 443
	got, err := readTarget(strings.NewReader(string(raw[1:])), atypIPv6)
	if err != nil {
		t.Fatal(err)
	}
	if got != "[2606:4700::1111]:443" {
		t.Fatalf("v6 目标解析错误: %q", got)
	}
}
