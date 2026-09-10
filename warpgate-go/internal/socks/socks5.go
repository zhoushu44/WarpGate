// Package socks 实现进程内 SOCKS5 前端（TCP CONNECT）：
// 支持账密认证模式与来源 IP 白名单模式，出站经隧道 DialFunc。
package socks

import (
	"context"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"strings"
	"sync"
	"time"
)

// DialFunc 出站拨号（通常为 tunnel.DialContext）。
type DialFunc func(ctx context.Context, network, target string) (net.Conn, error)

// Server 单个 SOCKS5 入站。
type Server struct {
	Listener    net.Listener
	RequireAuth bool // true：必须账密；false：无认证（配合 Whitelist 做白名单直连）
	Username    string
	Password    string
	Whitelist   []*net.IPNet // 非空时校验来源 IP；用于白名单模式
	Dial        DialFunc
	DialTimeout time.Duration // 连接上游超时，默认 20s
	IdleTimeout time.Duration // 双向空闲超时，0 = 不限
	Logger      Logger

	mu       sync.Mutex
	closing  bool
	activeWG sync.WaitGroup
}

// Logger 最小日志接口。
type Logger interface {
	Printf(format string, args ...any)
}

type nopLogger struct{}

func (nopLogger) Printf(string, ...any) {}

// Serve 阻塞接受连接，直到 Listener 关闭。
func (s *Server) Serve() error {
	if s.Logger == nil {
		s.Logger = nopLogger{}
	}
	if s.DialTimeout <= 0 {
		s.DialTimeout = 20 * time.Second
	}
	for {
		conn, err := s.Listener.Accept()
		if err != nil {
			s.mu.Lock()
			closing := s.closing
			s.mu.Unlock()
			if closing || errors.Is(err, net.ErrClosed) {
				return nil
			}
			return err
		}
		s.activeWG.Add(1)
		go func() {
			defer s.activeWG.Done()
			s.handleConn(conn)
		}()
	}
}

// Close 关闭监听并等待在途连接结束。
func (s *Server) Close() error {
	s.mu.Lock()
	if s.closing {
		s.mu.Unlock()
		return nil
	}
	s.closing = true
	s.mu.Unlock()
	err := s.Listener.Close()
	s.activeWG.Wait()
	return err
}

// ---- SOCKS5 协议 ----

const (
	socksVer5      = 0x05
	methodNoAuth   = 0x00
	methodUserPass = 0x02
	methodNoAccept = 0xFF

	cmdConnect    = 0x01
	cmdNotSupport = 0x07

	atypIPv4   = 0x01
	atypDomain = 0x03
	atypIPv6   = 0x04
)

func (s *Server) handleConn(conn net.Conn) {
	defer conn.Close()

	if len(s.Whitelist) > 0 {
		ra, ok := conn.RemoteAddr().(*net.TCPAddr)
		if !ok || !ipAllowed(ra.IP, s.Whitelist) {
			s.Logger.Printf("拒绝来源 %s（不在白名单）", ra)
			return
		}
	}

	_ = conn.SetDeadline(time.Now().Add(30 * time.Second))

	// 1. 方法协商
	buf := make([]byte, 2)
	if _, err := io.ReadFull(conn, buf); err != nil {
		return
	}
	if buf[0] != socksVer5 {
		return
	}
	n := int(buf[1])
	methods := make([]byte, n)
	if _, err := io.ReadFull(conn, methods); err != nil {
		return
	}

	hasUserPass := false
	for _, m := range methods {
		if m == methodUserPass {
			hasUserPass = true
			break
		}
	}
	hasNoAuth := false
	for _, m := range methods {
		if m == methodNoAuth {
			hasNoAuth = true
			break
		}
	}

	switch {
	case s.RequireAuth && hasUserPass:
		_, _ = conn.Write([]byte{socksVer5, methodUserPass})
		if !s.authUserPass(conn) {
			return
		}
	case !s.RequireAuth && hasNoAuth:
		_, _ = conn.Write([]byte{socksVer5, methodNoAuth})
	default:
		_, _ = conn.Write([]byte{socksVer5, methodNoAccept})
		return
	}

	// 2. 请求
	header := make([]byte, 4)
	if _, err := io.ReadFull(conn, header); err != nil {
		return
	}
	if header[0] != socksVer5 {
		return
	}
	cmd := header[1]
	if cmd != cmdConnect {
		_, _ = conn.Write([]byte{socksVer5, cmdNotSupport, 0x00, atypIPv4, 0, 0, 0, 0, 0, 0})
		return
	}
	target, err := readTarget(conn, header[3])
	if err != nil {
		return
	}

	// 3. 出站
	_ = conn.SetDeadline(time.Time{})
	ctx, cancel := context.WithTimeout(context.Background(), s.DialTimeout)
	up, err := s.dialSafe(ctx, target)
	cancel()
	if err != nil {
		s.Logger.Printf("连接 %s 失败: %v", target, err)
		_, _ = conn.Write([]byte{socksVer5, 0x01, 0x00, atypIPv4, 0, 0, 0, 0, 0, 0})
		return
	}
	defer up.Close()

	if _, err := conn.Write([]byte{socksVer5, 0x00, 0x00, atypIPv4, 0, 0, 0, 0, 0, 0}); err != nil {
		return
	}

	if s.IdleTimeout > 0 {
		_ = conn.SetDeadline(time.Now().Add(s.IdleTimeout))
		_ = up.SetDeadline(time.Now().Add(s.IdleTimeout))
	}
	relay(conn, up)
}

func (s *Server) dialSafe(ctx context.Context, target string) (net.Conn, error) {
	if strings.Contains(target, ":") && !strings.HasPrefix(target, "[") {
		// 已是 IPv6 字面量 host:port
		if h, p, err := net.SplitHostPort(target); err == nil && net.ParseIP(h) != nil {
			_ = p
		}
	}
	return s.Dial(ctx, "tcp", target)
}

// authUserPass RFC 1929 子协商，成功返回 true（失败时已回写状态）。
func (s *Server) authUserPass(conn net.Conn) bool {
	hdr := make([]byte, 2)
	if _, err := io.ReadFull(conn, hdr); err != nil {
		return false
	}
	user := make([]byte, hdr[1])
	if _, err := io.ReadFull(conn, user); err != nil {
		return false
	}
	plen := make([]byte, 1)
	if _, err := io.ReadFull(conn, plen); err != nil {
		return false
	}
	pass := make([]byte, plen[0])
	if _, err := io.ReadFull(conn, pass); err != nil {
		return false
	}
	ok := string(user) == s.Username && string(pass) == s.Password
	status := byte(0x00)
	if !ok {
		status = 0x01
	}
	_, _ = conn.Write([]byte{0x01, status})
	return ok
}

// readTarget 解析 ATYP + 地址 + 端口，返回 host:port。
func readTarget(r io.Reader, atyp byte) (string, error) {
	switch atyp {
	case atypIPv4:
		b := make([]byte, 6)
		if _, err := io.ReadFull(r, b); err != nil {
			return "", err
		}
		return fmt.Sprintf("%d.%d.%d.%d:%d", b[0], b[1], b[2], b[3], binary.BigEndian.Uint16(b[4:6])), nil
	case atypIPv6:
		b := make([]byte, 18)
		if _, err := io.ReadFull(r, b); err != nil {
			return "", err
		}
		ip := net.IP(b[:16])
		return net.JoinHostPort(ip.String(), fmt.Sprintf("%d", binary.BigEndian.Uint16(b[16:18]))), nil
	case atypDomain:
		lb := make([]byte, 1)
		if _, err := io.ReadFull(r, lb); err != nil {
			return "", err
		}
		host := make([]byte, lb[0])
		if _, err := io.ReadFull(r, host); err != nil {
			return "", err
		}
		pb := make([]byte, 2)
		if _, err := io.ReadFull(r, pb); err != nil {
			return "", err
		}
		return net.JoinHostPort(string(host), fmt.Sprintf("%d", binary.BigEndian.Uint16(pb))), nil
	default:
		return "", fmt.Errorf("不支持的 ATYP 0x%02x", atyp)
	}
}

// relay 双向拷贝，任一方向结束即关闭两侧。
func relay(a, b net.Conn) {
	done := make(chan struct{}, 2)
	copyFn := func(dst, src net.Conn) {
		_, _ = io.Copy(dst, src)
		_ = dst.SetDeadline(time.Now())
		_ = src.SetDeadline(time.Now())
		done <- struct{}{}
	}
	go copyFn(a, b)
	go copyFn(b, a)
	<-done
}

// ipAllowed 判断 IP 是否命中白名单（条目支持单 IP 或 CIDR）。
func ipAllowed(ip net.IP, list []*net.IPNet) bool {
	for _, n := range list {
		if n.Contains(ip) {
			return true
		}
	}
	return false
}

// ParseWhitelist 把 ["1.2.3.4", "10.0.0.0/8"] 解析为 IPNet 列表。
func ParseWhitelist(entries []string) ([]*net.IPNet, error) {
	var out []*net.IPNet
	for _, e := range entries {
		e = strings.TrimSpace(e)
		if e == "" {
			continue
		}
		if !strings.Contains(e, "/") {
			e += "/32"
			if strings.Contains(e, ":") { // IPv6 单地址
				e = strings.TrimSuffix(e, "/32") + "/128"
			}
		}
		_, ipnet, err := net.ParseCIDR(e)
		if err != nil {
			return nil, fmt.Errorf("白名单条目 %q 非法: %w", e, err)
		}
		out = append(out, ipnet)
	}
	return out, nil
}
