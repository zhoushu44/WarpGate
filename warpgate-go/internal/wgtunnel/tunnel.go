package wgtunnel

import (
	"context"
	"fmt"
	"net"
	"net/netip"
	"sync/atomic"
	"time"

	"github.com/Diniboy1123/usque/api"
	quic "github.com/quic-go/quic-go"
)

// Status 隧道状态。
const (
	StatusStarting  = "starting"
	StatusRunning   = "running"
	StatusReconnect = "reconnecting"
	StatusStopped   = "stopped"
)

// Options 隧道参数。
type Options struct {
	Keepalive         time.Duration
	ConnectPort       int          // MASQUE 端口，默认 443
	UseIPv6Endpoint   bool         // 用 v6 端点建隧道
	DNSServers        []netip.Addr // 本地解析 DNS
	DNSTimeout        time.Duration
	InitialPacketSize uint16 // 0 = 自动 PMTU
}

// Tunnel 进程内一条 MASQUE 隧道。底层 api.L4Proxy 自带懒重连：
// QUIC 会话死亡后下一次 DialContext 会自动重建 HTTP/3 连接，
// 因此本层只负责构建、状态标记与显式重启。
type Tunnel struct {
	ucfg    *UsqueConfig
	opts    Options
	proxy   atomic.Pointer[api.L4Proxy]
	status  atomic.Value // string
	conns   atomic.Int64
	startAt atomic.Value // time.Time

	// dialOverride 测试钩子：非空时替代真实 MASQUE 拨号（离线测试控制面用）。
	dialOverride func(ctx context.Context, network, target string) (net.Conn, error)
}

// SetDialOverride 注入测试拨号函数（生产代码禁止调用）。
func (t *Tunnel) SetDialOverride(fn func(ctx context.Context, network, target string) (net.Conn, error)) {
	t.dialOverride = fn
}

func New(ucfg *UsqueConfig, opts Options) *Tunnel {
	if opts.Keepalive <= 0 {
		opts.Keepalive = 30 * time.Second
	}
	if opts.ConnectPort <= 0 {
		opts.ConnectPort = 443
	}
	if opts.DNSTimeout <= 0 {
		opts.DNSTimeout = 2 * time.Second
	}
	if len(opts.DNSServers) == 0 {
		opts.DNSServers = []netip.Addr{netip.MustParseAddr("1.1.1.1"), netip.MustParseAddr("8.8.8.8")}
	}
	t := &Tunnel{ucfg: ucfg, opts: opts}
	t.status.Store(StatusStarting)
	return t
}

// quicConfig 复刻 usque cmd/l4_helpers.go 的 L4 QUIC 参数。
func quicConfig(keepalive time.Duration, initialPacketSize uint16) *quic.Config {
	cfg := &quic.Config{
		EnableDatagrams:                false,
		KeepAlivePeriod:                keepalive,
		InitialConnectionReceiveWindow: 10_000_000,
		MaxConnectionReceiveWindow:     10_000_000,
		InitialStreamReceiveWindow:     1_000_000,
		MaxStreamReceiveWindow:         1_000_000,
		MaxIncomingStreams:             100,
		MaxIncomingUniStreams:          100,
	}
	if initialPacketSize > 0 {
		cfg.InitialPacketSize = initialPacketSize
		cfg.DisablePathMTUDiscovery = true
	}
	return cfg
}

// Start 构建 TLS/QUIC 参数并创建 L4Proxy（懒连接，首个 Dial 时真正握手）。
func (t *Tunnel) Start(_ context.Context) error {
	priv, err := t.ucfg.ECPrivateKey()
	if err != nil {
		return err
	}
	peerPub, err := t.ucfg.EndpointPubKeyParsed()
	if err != nil {
		return err
	}
	certDER, err := selfSignedCertDER(priv)
	if err != nil {
		return fmt.Errorf("生成自签证书失败: %w", err)
	}
	tlsCfg, err := api.PrepareTlsConfig(priv, peerPub, certDER, l4ConnectSNI, false)
	if err != nil {
		return fmt.Errorf("准备 TLS 失败: %w", err)
	}
	endpoint, err := t.ucfg.SelectUDPEndpoint(t.opts.UseIPv6Endpoint, t.opts.ConnectPort)
	if err != nil {
		return err
	}

	proxy, err := api.NewL4Proxy(api.L4ProxyConfig{
		TLSConfig:      tlsCfg,
		QUICConfig:     quicConfig(t.opts.Keepalive, t.opts.InitialPacketSize),
		Endpoint:       endpoint,
		DNSResolver:    &dnsResolver{addrs: t.opts.DNSServers, timeout: t.opts.DNSTimeout},
		ResolveLocally: true,
		ConnectTimeout: 10 * time.Second,
	})
	if err != nil {
		return fmt.Errorf("创建 L4Proxy 失败: %w", err)
	}
	t.proxy.Store(proxy)
	t.startAt.Store(time.Now())
	t.status.Store(StatusRunning)
	return nil
}

// Restart 显式重建隧道（Stop + Start）。
func (t *Tunnel) Restart(ctx context.Context) error {
	t.Stop()
	t.status.Store(StatusReconnect)
	return t.Start(ctx)
}

// Stop 放弃当前 L4Proxy（其底层 QUIC 连接随空闲超时回收）。
func (t *Tunnel) Stop() {
	t.proxy.Store(nil)
	t.status.Store(StatusStopped)
}

// DialContext 经隧道向 target 发起 TCP 连接。
// 签名与 http.Transport.DialContext / socks.DialFunc 一致（network 恒为 "tcp"，忽略）。
func (t *Tunnel) DialContext(ctx context.Context, network, target string) (net.Conn, error) {
	if t.dialOverride != nil {
		return t.dialOverride(ctx, network, target)
	}
	p := t.proxy.Load()
	if p == nil {
		return nil, fmt.Errorf("隧道未启动")
	}
	t.conns.Add(1)
	defer t.conns.Add(-1)
	return p.DialContext(ctx, target)
}

// Status 返回当前状态字符串。
func (t *Tunnel) Status() string {
	s, _ := t.status.Load().(string)
	if s == "" {
		return StatusStopped
	}
	return s
}

// StartedAt 隧道启动时间。
func (t *Tunnel) StartedAt() time.Time {
	v, ok := t.startAt.Load().(time.Time)
	if !ok {
		return time.Time{}
	}
	return v
}

// dnsResolver 实现 api.DNSResolver：
// 解析目标域名优先返回 AAAA（本系统的卖点是每实例唯一 IPv6 出口），
// 无 AAAA 时回落 A 记录。
type dnsResolver struct {
	addrs   []netip.Addr
	timeout time.Duration
}

func (r *dnsResolver) Resolve(ctx context.Context, name string) (net.IP, error) {
	dialAddr := r.addrs[0].String()
	res := &net.Resolver{
		PreferGo: true,
		Dial: func(ctx context.Context, network, _ string) (net.Conn, error) {
			d := net.Dialer{Timeout: r.timeout}
			return d.DialContext(ctx, "udp", net.JoinHostPort(dialAddr, "53"))
		},
	}
	cctx, cancel := context.WithTimeout(ctx, r.timeout)
	defer cancel()

	ips, err := res.LookupNetIP(cctx, "ip", name)
	if err != nil {
		return nil, fmt.Errorf("解析 %s 失败: %w", name, err)
	}
	var v4 net.IP
	for _, a := range ips {
		if a.Is6() && !a.Is4In6() {
			return net.IP(a.AsSlice()), nil // 优先 IPv6
		}
		if v4 == nil && a.Is4() {
			v4 = net.IP(a.AsSlice())
		}
	}
	if v4 != nil {
		return v4, nil
	}
	return nil, fmt.Errorf("域名 %s 无可用记录", name)
}

var _ api.DNSResolver = (*dnsResolver)(nil)
