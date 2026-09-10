// Package pool 实例生命周期与调度：
// 创建/重建/停止/删除/换 IP、Round-Robin 提取、粘性会话、健康检查与 403 统计。
package pool

import (
	"context"
	"errors"
	"fmt"
	"net"
	"net/http"
	"net/netip"
	"os"
	"sort"
	"strconv"
	"strings"
	"sync"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/logx"
	"warpgate/internal/socks"
	"warpgate/internal/store"
	"warpgate/internal/wgtunnel"
)

// 实例状态。
const (
	StatusCreating  = "creating"
	StatusRunning   = "running"
	StatusUnhealthy = "unhealthy"
	StatusStopped   = "stopped"
	StatusError     = "error"
)

// Instance 单个实例（进程内对象组）。
type Instance struct {
	Index  int
	Dir    string
	Meta   *store.Meta
	Tunnel *wgtunnel.Tunnel
	Status string

	CF403Count  int
	CFCheckCnt  int
	FailStreak  int
	LastCode    int
	LastLatency int64 // 毫秒
	LastExitIP  string

	lnAcc, lnWl net.Listener

	mu sync.Mutex
}

// StickyEntry 粘性会话绑定。
type StickyEntry struct {
	Index     int
	ExpiresAt time.Time
}

// Manager 全局管理器。
type Manager struct {
	cfg  *appconfig.Config
	base string

	mu      sync.RWMutex
	inst    map[int]*Instance
	rr      int
	sticky  map[string]*StickyEntry
	sem     chan struct{}
	booting bool

	whitelistMu sync.RWMutex
	whitelist   []*net.IPNet

	publicIP   string
	publicIPMu sync.RWMutex

	// rotateFn 可注入换 IP 动作（默认 (*Manager).Rotate；测试用）
	rotateFn func(ctx context.Context, m *Manager, idx int) error
}

func (m *Manager) rotate(ctx context.Context, idx int) error {
	if m.rotateFn != nil {
		return m.rotateFn(ctx, m, idx)
	}
	return m.Rotate(ctx, idx)
}

func New(cfg *appconfig.Config) *Manager {
	m := &Manager{
		cfg:    cfg,
		base:   cfg.InstancesDirResolved(""),
		inst:   map[int]*Instance{},
		sticky: map[string]*StickyEntry{},
		sem:    make(chan struct{}, max(cfg.MaxConcurrent, 1)),
	}
	wl, err := socks.ParseWhitelist(cfg.IPWhitelist)
	if err == nil {
		m.whitelist = wl
	}
	return m
}

func max(a, b int) int {
	if a > b {
		return a
	}
	return b
}

func (m *Manager) tunnelOpts() wgtunnel.Options {
	addrs := make([]netip.Addr, 0, len(m.cfg.DNSServers))
	for _, s := range m.cfg.DNSServers {
		if a, err := netip.ParseAddr(s); err == nil {
			addrs = append(addrs, a)
		}
	}
	return wgtunnel.Options{
		Keepalive:       time.Duration(m.cfg.KeepaliveSec()) * time.Second,
		ConnectPort:     m.cfg.ConnectPort,
		UseIPv6Endpoint: false,
		DNSServers:      addrs,
		DNSTimeout:      2 * time.Second,
	}
}

// ---------- 生命周期 ----------

// Boot 扫描实例目录并暖启动；缺口按 resident_count 冷创建补齐。
func (m *Manager) Boot(ctx context.Context) {
	var idxs []int
	entries, _ := os.ReadDir(m.base)
	for _, e := range entries {
		if !e.IsDir() || !strings.HasPrefix(e.Name(), "warp-") {
			continue
		}
		if i, err := strconv.Atoi(strings.TrimPrefix(e.Name(), "warp-")); err == nil {
			idxs = append(idxs, i)
		}
	}
	sort.Ints(idxs)

	// 暖启动已存在的实例
	for _, i := range idxs {
		if _, err := m.startExisting(i); err != nil {
			logx.Warnf("POOL", "实例 warp-%d 暖启动失败: %v", i, err)
		}
	}

	// 补齐到 resident_count
	need := m.cfg.ResidentCount
	for i := 0; i < need; i++ {
		m.mu.RLock()
		_, ok := m.inst[i]
		m.mu.RUnlock()
		if !ok {
			if err := m.Create(ctx, i); err != nil {
				logx.Errorf("POOL", "实例 warp-%d 创建失败: %v", i, err)
			}
		}
	}
}

// startExisting 用已有 usque.json 暖启动（不注册）。
func (m *Manager) startExisting(idx int) (*Instance, error) {
	dir := store.Dir(m.base, idx)
	ucfg, err := wgtunnel.LoadUsqueConfig(store.UsqueConfPath(dir))
	if err != nil {
		return nil, err
	}
	return m.startWithConfig(idx, ucfg, false)
}

// Create 创建实例：目录无账号则冷注册，否则暖启动。
func (m *Manager) Create(ctx context.Context, idx int) error {
	if idx >= m.cfg.MaxInstances {
		return fmt.Errorf("超过 max_instances=%d", m.cfg.MaxInstances)
	}
	m.sem <- struct{}{}
	defer func() { <-m.sem }()

	dir := store.Dir(m.base, idx)
	var ucfg *wgtunnel.UsqueConfig
	if _, err := wgtunnel.LoadUsqueConfig(store.UsqueConfPath(dir)); err == nil {
		// 已有账号：暖创建
		ucfg, _ = wgtunnel.LoadUsqueConfig(store.UsqueConfPath(dir))
	} else {
		// 冷创建：注册新账号
		name := fmt.Sprintf("warpgate-%d", idx)
		var err error
		ucfg, err = wgtunnel.RegisterAccount(name, m.cfg.TeamToken)
		if err != nil {
			return err
		}
		if err := os.MkdirAll(dir, 0o755); err != nil {
			return err
		}
		if err := ucfg.Save(store.UsqueConfPath(dir)); err != nil {
			return err
		}
		logx.Infof("ENGINE", "warp-%d 已注册全新 MASQUE 账号 (ipv6=%s)", idx, ucfg.IPv6)
	}
	_, err := m.startWithConfig(idx, ucfg, true)
	return err
}

// startWithConfig 拉起隧道 + 出口校验 + SOCKS 监听 + meta 落盘。
func (m *Manager) startWithConfig(idx int, ucfg *wgtunnel.UsqueConfig, verifyIP bool) (*Instance, error) {
	dir := store.Dir(m.base, idx)

	t := wgtunnel.New(ucfg, m.tunnelOpts())
	if err := t.Start(context.Background()); err != nil {
		return nil, err
	}

	inst := &Instance{Index: idx, Dir: dir, Tunnel: t, Status: StatusCreating}

	// 出口 IPv6 校验（新注册时执行；暖启动信任落盘值）
	if verifyIP {
		ipv6, err := m.probeExitIPv6(t)
		if err != nil {
			t.Stop()
			return nil, fmt.Errorf("出口 IP 检测失败: %w", err)
		}
		if ipv6 != "" && ucfg.IPv6 != "" && !sameIPv6Block(ipv6, ucfg.IPv6) {
			logx.Warnf("IP_CHECK", "warp-%d 出口 %s 与账号分配 %s 不一致（记录实际值）", idx, ipv6, ucfg.IPv6)
		}
		inst.LastExitIP = ipv6
	}

	if err := m.startSocks(inst); err != nil {
		t.Stop()
		return nil, err
	}

	meta, _ := store.ReadMeta(dir)
	if meta == nil {
		meta = &store.Meta{Index: idx, Name: store.Name(idx), CreatedAt: time.Now().UnixMilli()}
	}
	meta.PID = 0
	meta.EnginePID = 0
	meta.Transport = "masque"
	meta.Engine = "embedded"
	meta.IPv6 = ucfg.IPv6
	meta.Endpoint = nil
	meta.Status = StatusRunning
	if inst.LastExitIP != "" {
		meta.IPv6 = inst.LastExitIP
	}
	inst.Meta = meta
	inst.Status = StatusRunning
	_ = store.WriteMeta(dir, meta)

	m.mu.Lock()
	m.inst[idx] = inst
	m.mu.Unlock()
	logx.Infof("POOL", "warp-%d 启动完成（端口:%d/%d ipv6:%s）",
		idx, m.cfg.BasePort+idx, m.cfg.BasePort+m.cfg.WhitelistOffset+idx, meta.IPv6)
	return inst, nil
}

// startSocks 开账密/白名单两个入站监听。
func (m *Manager) startSocks(inst *Instance) error {
	dial := func(ctx context.Context, network, target string) (net.Conn, error) {
		return inst.Tunnel.DialContext(ctx, network, target)
	}
	accAddr := net.JoinHostPort(m.cfg.BindAddress, strconv.Itoa(m.cfg.BasePort+inst.Index))
	lnAcc, err := net.Listen("tcp", accAddr)
	if err != nil {
		return fmt.Errorf("监听 %s 失败: %w", accAddr, err)
	}
	srvAcc := &socks.Server{
		Listener:    lnAcc,
		RequireAuth: m.cfg.ProxyAuthEnabled,
		Username:    m.cfg.ProxyUsername,
		Password:    m.cfg.ProxyPassword,
		Dial:        dial,
		IdleTimeout: time.Duration(m.cfg.SocksTCPTimeoutSec) * time.Second,
	}
	go func() { _ = srvAcc.Serve() }()

	// 白名单端口：仅当 ip_whitelist 非空时开放（与 Node 版 sing-box 规则一致：免密直连 + 源 CIDR 过滤）
	m.whitelistMu.RLock()
	wl := m.whitelist
	m.whitelistMu.RUnlock()
	if len(wl) > 0 {
		if err := m.startWlListener(inst, wl); err != nil {
			lnAcc.Close()
			return err
		}
	}

	inst.lnAcc = lnAcc
	return nil
}

// startWlListener 开白名单入站（免认证 + 源 CIDR 白名单）。
func (m *Manager) startWlListener(inst *Instance, wl []*net.IPNet) error {
	wlAddr := net.JoinHostPort(m.cfg.BindAddress, strconv.Itoa(m.cfg.BasePort+m.cfg.WhitelistOffset+inst.Index))
	lnWl, err := net.Listen("tcp", wlAddr)
	if err != nil {
		return fmt.Errorf("监听 %s 失败: %w", wlAddr, err)
	}
	dial := func(ctx context.Context, network, target string) (net.Conn, error) {
		return inst.Tunnel.DialContext(ctx, network, target)
	}
	srvWl := &socks.Server{
		Listener:    lnWl,
		RequireAuth: false,
		Whitelist:   wl,
		Dial:        dial,
		IdleTimeout: time.Duration(m.cfg.SocksTCPTimeoutSec) * time.Second,
	}
	go func() { _ = srvWl.Serve() }()
	inst.lnWl = lnWl
	return nil
}

func (m *Manager) stopWlListener(inst *Instance) {
	if inst.lnWl != nil {
		_ = inst.lnWl.Close()
		inst.lnWl = nil
	}
}

func (m *Manager) stopSocks(inst *Instance) {
	if inst.lnAcc != nil {
		_ = inst.lnAcc.Close()
		inst.lnAcc = nil
	}
	if inst.lnWl != nil {
		_ = inst.lnWl.Close()
		inst.lnWl = nil
	}
}

// StopInstance 停止实例（账号保留）。
func (m *Manager) StopInstance(idx int) error {
	m.mu.Lock()
	inst, ok := m.inst[idx]
	if ok {
		delete(m.inst, idx)
	}
	m.mu.Unlock()
	if !ok {
		return fmt.Errorf("实例 warp-%d 不存在", idx)
	}
	m.stopSocks(inst)
	if inst.Tunnel != nil {
		inst.Tunnel.Stop()
	}
	inst.Status = StatusStopped
	if inst.Meta != nil {
		inst.Meta.Status = StatusStopped
		_ = store.WriteMeta(inst.Dir, inst.Meta)
	}
	logx.Infof("POOL", "warp-%d 已停止", idx)
	return nil
}

// StartInstance 暖启动实例。
func (m *Manager) StartInstance(ctx context.Context, idx int) error {
	m.mu.RLock()
	_, ok := m.inst[idx]
	m.mu.RUnlock()
	if ok {
		return fmt.Errorf("实例 warp-%d 已在运行", idx)
	}
	if !store.Exists(m.base, idx) {
		return fmt.Errorf("实例 warp-%d 目录不存在", idx)
	}
	_, err := m.startExisting(idx)
	return err
}

// DeleteInstance 删除实例（目录一并删除）。
func (m *Manager) DeleteInstance(idx int) error {
	if err := m.StopInstance(idx); err != nil {
		// 目录存在但未运行也允许删除
		if !store.Exists(m.base, idx) {
			return err
		}
	}
	m.mu.Lock()
	delete(m.inst, idx)
	m.mu.Unlock()
	if err := store.Remove(store.Dir(m.base, idx)); err != nil {
		return err
	}
	logx.Infof("POOL", "warp-%d 已删除", idx)
	return nil
}

// Rebuild 重建实例：删目录 → 重新注册（同索引换新 IP）。
func (m *Manager) Rebuild(ctx context.Context, idx int) error {
	_ = m.StopInstance(idx)
	if err := store.Remove(store.Dir(m.base, idx)); err != nil {
		return err
	}
	m.mu.Lock()
	delete(m.inst, idx)
	m.mu.Unlock()
	return m.Create(ctx, idx)
}

// Rotate 等价于 Rebuild（换号即换 IP），保留独立入口以兼容 API。
func (m *Manager) Rotate(ctx context.Context, idx int) error { return m.Rebuild(ctx, idx) }

// ---------- 调度 ----------

// Acquire Round-Robin 提取一个健康实例；sticky=true 时按 sessionID 固定。
func (m *Manager) Acquire(sticky bool, sessionID string) (*Instance, error) {
	m.mu.Lock()
	defer m.mu.Unlock()

	if sticky && sessionID != "" {
		if s, ok := m.sticky[sessionID]; ok && time.Now().Before(s.ExpiresAt) {
			if inst, ok := m.inst[s.Index]; ok && inst.Status == StatusRunning {
				return inst, nil
			}
		}
	}

	var healthy []*Instance
	idxs := make([]int, 0, len(m.inst))
	for i := range m.inst {
		idxs = append(idxs, i)
	}
	sort.Ints(idxs)
	for _, i := range idxs {
		if m.inst[i].Status == StatusRunning {
			healthy = append(healthy, m.inst[i])
		}
	}
	if len(healthy) == 0 {
		return nil, errors.New("无可用代理")
	}

	inst := healthy[m.rr%len(healthy)]
	m.rr++

	if sticky && sessionID != "" {
		m.sticky[sessionID] = &StickyEntry{
			Index:     inst.Index,
			ExpiresAt: time.Now().Add(time.Duration(m.cfg.StickyMinutes) * time.Minute),
		}
	}
	return inst, nil
}

// Release 释放粘性会话。
func (m *Manager) Release(sessionID string) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	if _, ok := m.sticky[sessionID]; ok {
		delete(m.sticky, sessionID)
		return true
	}
	return false
}

// StickyActive 粘性会话数。
func (m *Manager) StickyActive() int {
	m.mu.RLock()
	defer m.mu.RUnlock()
	n := 0
	for _, s := range m.sticky {
		if time.Now().Before(s.ExpiresAt) {
			n++
		}
	}
	return n
}

// cleanupLoop 过期粘性会话清理（session_cleanup_interval）。
func (m *Manager) cleanupLoop(ctx context.Context) {
	interval := time.Duration(m.cfg.SessionCleanup) * time.Second
	if interval <= 0 {
		interval = 30 * time.Second
	}
	tk := time.NewTicker(interval)
	defer tk.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tk.C:
			m.mu.Lock()
			now := time.Now()
			for k, s := range m.sticky {
				if now.After(s.ExpiresAt) {
					delete(m.sticky, k)
				}
			}
			m.mu.Unlock()
		}
	}
}

// ---------- 健康检查 ----------

// ProbeResult 单次探测结果。
type ProbeResult struct {
	Index     int    `json:"index"`
	HTTPCode  int    `json:"http_code"`
	LatencyMS int64  `json:"latency_ms"`
	Healthy   bool   `json:"healthy"`
	Error     string `json:"error,omitempty"`
}

// probe 经隧道对 openai_check_url 发 GET（语义同 Node 版 CF 检测）。
func (m *Manager) probe(inst *Instance) ProbeResult {
	res := ProbeResult{Index: inst.Index}
	client := &http.Client{
		Timeout: 15 * time.Second,
		Transport: &http.Transport{
			DialContext:       inst.Tunnel.DialContext,
			DisableKeepAlives: true,
		},
	}
	start := time.Now()
	resp, err := client.Get(m.cfg.OpenAICheckURL)
	res.LatencyMS = time.Since(start).Milliseconds()
	if err != nil {
		res.Error = err.Error()
		return res
	}
	defer resp.Body.Close()
	res.HTTPCode = resp.StatusCode
	res.Healthy = resp.StatusCode < 500
	return res
}

// ProbeOnce 立即探测一次（API /api/check 用）。
func (m *Manager) ProbeOnce(inst *Instance) ProbeResult { return m.probe(inst) }

// probeExitIPv6 经隧道取出口 IPv6（带重试，语义同 Node 版 IP 检测）。
func (m *Manager) probeExitIPv6(t *wgtunnel.Tunnel) (string, error) {
	client := &http.Client{
		Timeout: 15 * time.Second,
		Transport: &http.Transport{
			DialContext:       t.DialContext,
			DisableKeepAlives: true,
		},
	}
	var lastErr error
	for i := 0; i < max(m.cfg.IPCheckRetries, 1); i++ {
		resp, err := client.Get(m.cfg.IPCheckURLV6)
		if err == nil {
			body := make([]byte, 64)
			n, _ := resp.Body.Read(body)
			resp.Body.Close()
			ip := strings.TrimSpace(string(body[:n]))
			if strings.Contains(ip, ":") {
				return ip, nil
			}
			lastErr = fmt.Errorf("响应不是 IPv6: %q", ip)
		} else {
			lastErr = err
		}
		time.Sleep(time.Duration(m.cfg.IPCheckIntervalSec() * float64(time.Second)))
	}
	return "", lastErr
}

// HealthLoop 周期健康检查 + 403 统计 + 自动换 IP。
func (m *Manager) HealthLoop(ctx context.Context) {
	interval := time.Duration(m.cfg.HealthInterval) * time.Second
	if interval <= 0 {
		interval = 600 * time.Second
	}
	tk := time.NewTicker(interval)
	defer tk.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-tk.C:
			m.checkAll(ctx)
		}
	}
}

func (m *Manager) checkAll(ctx context.Context) {
	m.mu.RLock()
	instances := make([]*Instance, 0, len(m.inst))
	for _, i := range m.inst {
		instances = append(instances, i)
	}
	m.mu.RUnlock()

	for _, inst := range instances {
		r := m.probe(inst)
		m.applyProbe(ctx, inst, r)
	}
}

// applyProbe 更新统计并按需触发 rotate/rebuild（导出便于测试）。
func (m *Manager) applyProbe(ctx context.Context, inst *Instance, r ProbeResult) {
	inst.mu.Lock()
	inst.LastCode = r.HTTPCode
	inst.LastLatency = r.LatencyMS
	if r.HTTPCode == 0 {
		inst.FailStreak++
		failStreak := inst.FailStreak
		inst.mu.Unlock()

		// 连接层失败
		if failStreak >= 2 {
			inst.Status = StatusUnhealthy
			logx.Warnf("HEALTH", "warp-%d 连续 %d 次探测失败: %s", inst.Index, failStreak, r.Error)
			if m.cfg.AutoRebuild {
				go func() {
					if err := m.Rebuild(ctx, inst.Index); err != nil {
						logx.Errorf("POOL", "warp-%d 自动重建失败: %v", inst.Index, err)
					}
				}()
			}
		}
		return
	}

	inst.FailStreak = 0
	inst.CFCheckCnt++
	if r.HTTPCode == 403 {
		inst.CF403Count++
	}
	rate := 0
	if inst.CFCheckCnt > 0 {
		rate = inst.CF403Count * 100 / inst.CFCheckCnt
	}
	logx.Infof("CF_USAGE", "warp-%d 探测 HTTP:%d %dms，403统计 %d/%d=%d%%",
		inst.Index, r.HTTPCode, r.LatencyMS, inst.CF403Count, inst.CFCheckCnt, rate)
	needRotate := shouldRotate(m.cfg.AutoRotateOnHigh403, inst.CF403Count, inst.CFCheckCnt, m.cfg.CF403Threshold, m.cfg.CFMinCheckCount)
	if needRotate {
		inst.CF403Count, inst.CFCheckCnt = 0, 0
	}
	if r.HTTPCode < 500 {
		inst.Status = StatusRunning
	}
	inst.mu.Unlock()

	if needRotate {
		logx.Warnf("CF_USAGE", "warp-%d 403 率 %d%% 达阈值，自动换 IP", inst.Index, rate)
		go func() {
			if err := m.rotate(ctx, inst.Index); err != nil {
				logx.Errorf("POOL", "warp-%d 自动换 IP 失败: %v", inst.Index, err)
			}
		}()
	}
}

// ---------- 查询 ----------

// Get 实例查询。
func (m *Manager) Get(idx int) (*Instance, bool) {
	m.mu.RLock()
	defer m.mu.RUnlock()
	i, ok := m.inst[idx]
	return i, ok
}

// Count 运行中实例数。
func (m *Manager) Count() int {
	m.mu.RLock()
	defer m.mu.RUnlock()
	return len(m.inst)
}

// HealthyCount 健康实例数。
func (m *Manager) HealthyCount() int {
	m.mu.RLock()
	defer m.mu.RUnlock()
	n := 0
	for _, i := range m.inst {
		if i.Status == StatusRunning {
			n++
		}
	}
	return n
}

// All 所有实例（按 index 排序）。
func (m *Manager) All() []*Instance {
	m.mu.RLock()
	defer m.mu.RUnlock()
	out := make([]*Instance, 0, len(m.inst))
	idxs := make([]int, 0, len(m.inst))
	for i := range m.inst {
		idxs = append(idxs, i)
	}
	sort.Ints(idxs)
	for _, i := range idxs {
		out = append(out, m.inst[i])
	}
	return out
}

// PublicIP 返回对外公告 IP（auto-detect 时开机探测一次）。
func (m *Manager) PublicIP(ctx context.Context) string {
	m.publicIPMu.RLock()
	ip := m.publicIP
	m.publicIPMu.RUnlock()
	if ip != "" {
		return ip
	}
	if m.cfg.PublicIP != "" && m.cfg.PublicIP != "auto-detect" {
		return m.cfg.PublicIP
	}
	client := &http.Client{Timeout: 8 * time.Second}
	resp, err := client.Get(m.cfg.IPCheckURL)
	if err == nil {
		body := make([]byte, 64)
		n, _ := resp.Body.Read(body)
		resp.Body.Close()
		if s := strings.TrimSpace(string(body[:n])); s != "" {
			m.publicIPMu.Lock()
			m.publicIP = s
			m.publicIPMu.Unlock()
			return s
		}
	}
	return "127.0.0.1"
}

// SetWhitelist 更新白名单并热更新运行中实例的白名单入站（空列表 = 关闭白名单端口）。
func (m *Manager) SetWhitelist(entries []string) error {
	wl, err := socks.ParseWhitelist(entries)
	if err != nil {
		return err
	}
	m.whitelistMu.Lock()
	m.whitelist = wl
	m.whitelistMu.Unlock()

	m.mu.RLock()
	defer m.mu.RUnlock()
	for _, inst := range m.inst {
		m.stopWlListener(inst)
		if len(wl) > 0 {
			if err := m.startWlListener(inst, wl); err != nil {
				logx.Warnf("POOL", "warp-%d 白名单端口重开失败: %v", inst.Index, err)
			}
		}
	}
	return nil
}

// StartLoops 启动粘性清理与健康检查循环。
func (m *Manager) StartLoops(ctx context.Context) {
	go m.cleanupLoop(ctx)
	go m.HealthLoop(ctx)
}

// shouldRotate 403 率是否达到自动换 IP 阈值（纯函数，便于测试）。
func shouldRotate(autoRotate bool, cf403, cfCheck, thresholdPct, minCheck int) bool {
	if !autoRotate || cfCheck < minCheck || cfCheck <= 0 {
		return false
	}
	return cf403*100/cfCheck >= thresholdPct
}

// sameIPv6Block 宽松比较两个 IPv6（前 4 段一致即认为同源）。
func sameIPv6Block(a, b string) bool {
	pa, err1 := netip.ParseAddr(strings.TrimSpace(a))
	pb, err2 := netip.ParseAddr(strings.TrimSpace(b))
	if err1 != nil || err2 != nil {
		return a == b
	}
	return pa.Is4() == pb.Is4() && pa.String() == pb.String()
}
