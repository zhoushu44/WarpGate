package pool

import (
	"context"
	"fmt"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/store"
	"warpgate/internal/wgtunnel"
)

func newTestManager(t *testing.T) *Manager {
	t.Helper()
	cfg := appconfig.Default()
	cfg.ResidentCount = 3
	cfg.BasePort = 20000 // 测试用高位端口，避免冲突
	cfg.WhitelistOffset = 100
	m := New(cfg)
	m.base = t.TempDir() // 实例目录隔离
	return m
}

func fakeInstance(idx int) *Instance {
	return &Instance{
		Index:  idx,
		Dir:    "fake",
		Meta:   &store.Meta{Index: idx, Name: store.Name(idx), IPv6: "2606:4700::1"},
		Status: StatusRunning,
	}
}

func TestRoundRobinSkipsUnhealthy(t *testing.T) {
	m := newTestManager(t)
	m.mu.Lock()
	for i := 0; i < 3; i++ {
		inst := fakeInstance(i)
		if i == 1 {
			inst.Status = StatusUnhealthy
		}
		m.inst[i] = inst
	}
	m.mu.Unlock()

	seen := map[int]bool{}
	for i := 0; i < 10; i++ {
		inst, err := m.Acquire(false, "")
		if err != nil {
			t.Fatal(err)
		}
		if inst.Index == 1 {
			t.Fatalf("不应调度到 unhealthy 实例")
		}
		seen[inst.Index] = true
	}
	if !seen[0] || !seen[2] {
		t.Fatalf("轮转应覆盖 0 和 2: %v", seen)
	}
}

func TestAcquireNoHealthy(t *testing.T) {
	m := newTestManager(t)
	if _, err := m.Acquire(false, ""); err == nil {
		t.Fatalf("无实例应报错")
	}
}

func TestStickySession(t *testing.T) {
	m := newTestManager(t)
	m.mu.Lock()
	for i := 0; i < 3; i++ {
		m.inst[i] = fakeInstance(i)
	}
	m.mu.Unlock()

	a, err := m.Acquire(true, "sess-1")
	if err != nil {
		t.Fatal(err)
	}
	for i := 0; i < 5; i++ {
		b, err := m.Acquire(true, "sess-1")
		if err != nil {
			t.Fatal(err)
		}
		if b.Index != a.Index {
			t.Fatalf("粘性会话应固定实例: %d != %d", b.Index, a.Index)
		}
	}
	if m.StickyActive() != 1 {
		t.Fatalf("活跃粘性会话应为 1")
	}
	if !m.Release("sess-1") {
		t.Fatalf("释放应成功")
	}
	if m.Release("sess-1") {
		t.Fatalf("重复释放应失败")
	}
}

func TestStickyExpiry(t *testing.T) {
	m := newTestManager(t)
	m.mu.Lock()
	m.inst[0] = fakeInstance(0)
	m.sticky["old"] = &StickyEntry{Index: 0, ExpiresAt: time.Now().Add(-time.Minute)}
	m.mu.Unlock()
	if m.StickyActive() != 0 {
		t.Fatalf("过期会话不应计数")
	}
}

func TestShouldRotate(t *testing.T) {
	cases := []struct {
		name                      string
		autoRotate                bool
		cf403, cfCheck, thr, minC int
		want                      bool
	}{
		{"未开自动换IP", false, 5, 5, 50, 3, false},
		{"次数不足", true, 2, 2, 50, 3, false},
		{"达标 60%>=50%", true, 3, 5, 50, 3, true},
		{"未达标 40%<50%", true, 2, 5, 50, 3, false},
		{"刚好阈值 50%", true, 5, 10, 50, 3, true},
	}
	for _, c := range cases {
		if got := shouldRotate(c.autoRotate, c.cf403, c.cfCheck, c.thr, c.minC); got != c.want {
			t.Fatalf("%s: got %v want %v", c.name, got, c.want)
		}
	}
}

func TestApplyProbe403TriggersRotate(t *testing.T) {
	m := newTestManager(t)
	cfg := m.cfg
	cfg.AutoRotateOnHigh403 = true
	cfg.CF403Threshold = 50
	cfg.CFMinCheckCount = 3

	rotated := make(chan int, 1)
	m.rotateFn = func(ctx context.Context, m *Manager, idx int) error {
		rotated <- idx
		return nil
	}
	inst := fakeInstance(0)
	inst.Tunnel = newSafeTunnel()
	m.mu.Lock()
	m.inst[0] = inst
	m.mu.Unlock()

	ctx := context.Background()
	for i := 0; i < 3; i++ {
		m.applyProbe(ctx, inst, ProbeResult{Index: 0, HTTPCode: 403, Healthy: true})
	}
	select {
	case idx := <-rotated:
		if idx != 0 {
			t.Fatalf("应换 IP 实例 0, got %d", idx)
		}
	case <-time.After(2 * time.Second):
		t.Fatalf("403 达阈值应触发 rotate")
	}
	inst.mu.Lock()
	_, checkCnt := inst.CF403Count, inst.CFCheckCnt
	inst.mu.Unlock()
	if checkCnt != 0 {
		t.Fatalf("触发 rotate 后统计应清零, got %d", checkCnt)
	}
}

// TestConcurrentCreateRespectsMaxConcurrent 验证并发调用 Create 时，
// 同时进行中的注册数不超过 max_concurrent_create（信号量上限）。
func TestConcurrentCreateRespectsMaxConcurrent(t *testing.T) {
	m := newTestManager(t)
	m.cfg.MaxConcurrent = 3
	m.sem = make(chan struct{}, 3) // 与 New 一致：容量 = max_concurrent_create

	var inflight, peak int32
	m.registerFn = func(name, jwt string) (*wgtunnel.UsqueConfig, error) {
		cur := atomic.AddInt32(&inflight, 1)
		for {
			old := atomic.LoadInt32(&peak)
			if cur <= old || atomic.CompareAndSwapInt32(&peak, old, cur) {
				break
			}
		}
		time.Sleep(150 * time.Millisecond) // 拉长注册窗口，制造重叠
		atomic.AddInt32(&inflight, -1)
		return nil, fmt.Errorf("test: 注册后中断（不进入启动流程）")
	}

	const n = 8
	var wg sync.WaitGroup
	for i := 0; i < n; i++ {
		wg.Add(1)
		go func(idx int) {
			defer wg.Done()
			_ = m.Create(context.Background(), idx)
		}(i)
	}
	wg.Wait()

	if got := atomic.LoadInt32(&peak); got > 3 {
		t.Fatalf("同时注册数超过 max_concurrent_create=3, got %d", got)
	} else if got != 3 {
		t.Fatalf("并发未真正生效（峰值为 %d，期望 3）", got)
	}
}

// newSafeTunnel 返回未启动的真实 Tunnel（Stop 等方法可安全调用）。
func newSafeTunnel() *wgtunnel.Tunnel {
	return wgtunnel.New(&wgtunnel.UsqueConfig{ID: "test"}, wgtunnel.Options{})
}

func TestApplyProbeTransportFailure(t *testing.T) {
	m := newTestManager(t)
	m.cfg.AutoRebuild = false // 不真的重建
	inst := fakeInstance(0)
	m.mu.Lock()
	m.inst[0] = inst
	m.mu.Unlock()
	ctx := context.Background()
	m.applyProbe(ctx, inst, ProbeResult{HTTPCode: 0, Error: "dial fail"})
	m.applyProbe(ctx, inst, ProbeResult{HTTPCode: 0, Error: "dial fail"})
	if inst.Status != StatusUnhealthy {
		t.Fatalf("连续失败 2 次应标记 unhealthy, got %s", inst.Status)
	}
}

func TestSameIPv6Block(t *testing.T) {
	if !sameIPv6Block("2606:4700::1", "2606:4700::1") {
		t.Fatal("相同地址应判定一致")
	}
	if sameIPv6Block("2606:4700::1", "2a09:bacx::1") {
		t.Fatal("不同地址不应一致")
	}
}
