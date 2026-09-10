// Package apiserver 提供 Node 版兼容的 HTTP 管理面（A 级端点）+ 静态页托管。
package apiserver

import (
	"context"
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	"warpgate/internal/appconfig"
	"warpgate/internal/logx"
	"warpgate/internal/pool"
	"warpgate/internal/socks"
	"warpgate/internal/store"
)

// ---- Node 版响应契约 ----

const (
	codeOK            = "00000"
	codeUnauthorized  = "A0001"
	codeRateLimited   = "A0002"
	codeNoProxy       = "P0001"
	codeInvalidInput  = "I0001"
	codeNotFound      = "I0002"
	codeInstanceError = "R0001"
	codeIPCheckFailed = "R0002"
	codeInternal      = "99999"
)

type apiServer struct {
	cfg    *appconfig.Config
	mgr    *pool.Manager
	secret []byte

	jobsMu sync.Mutex
	jobs   map[string]*job

	loginMu    sync.Mutex
	loginFails map[string]*failRec
}

type job struct {
	mu      sync.Mutex
	ID      string      `json:"id"`
	Type    string      `json:"type"`
	Status  string      `json:"status"` // running/completed/failed
	Logs    []string    `json:"logs"`
	Results []jobResult `json:"results"`
}

type jobResult struct {
	Success bool   `json:"success"`
	Name    string `json:"name"`
	Error   string `json:"error,omitempty"`
}

type failRec struct {
	fail        int
	lockedUntil time.Time
}

// New 构造并返回 http.Handler。
func New(cfg *appconfig.Config, mgr *pool.Manager) http.Handler {
	s := &apiServer{cfg: cfg, mgr: mgr, jobs: map[string]*job{}, loginFails: map[string]*failRec{}}
	s.secret = make([]byte, 32)
	_, _ = rand.Read(s.secret)
	mux := http.NewServeMux()
	mux.HandleFunc("/api/login", s.handleLogin)

	protected := http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if !s.auth(w, r) {
			return
		}
		s.route(w, r)
	})
	mux.Handle("/api/", protected)

	pub := http.Dir(publicDir())
	fs := http.FileServer(pub)
	mux.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/docs" {
			http.ServeFile(w, r, filepath.Join(publicDir(), "docs.html"))
			return
		}
		fs.ServeHTTP(w, r)
	})
	return mux
}

func publicDir() string {
	if v := os.Getenv("WARP_PUBLIC_DIR"); v != "" {
		return v
	}
	return "./public"
}

// ---- 鉴权（语义同 Node 版：白名单/本机免 Key、登录 token、X-API-Key 直连）----

func (s *apiServer) clientIP(r *http.Request) string {
	host, _, err := net.SplitHostPort(r.RemoteAddr)
	if err != nil {
		return r.RemoteAddr
	}
	return host
}

func (s *apiServer) isWhitelisted(r *http.Request) bool {
	ip := net.ParseIP(s.clientIP(r))
	if ip == nil {
		return false
	}
	if ip.IsLoopback() {
		return true
	}
	wl, err := socks.ParseWhitelist(s.cfg.IPWhitelist)
	if err != nil {
		return false
	}
	for _, n := range wl {
		if n.Contains(ip) {
			return true
		}
	}
	return false
}

func (s *apiServer) auth(w http.ResponseWriter, r *http.Request) bool {
	if s.isWhitelisted(r) {
		return true
	}
	// X-API-Key 直连
	if k := r.Header.Get("X-API-Key"); k != "" && k == s.cfg.APIKey {
		return true
	}
	// 登录 token（管理类接口）
	if tok := s.tokenFromReq(r); tok != "" && s.verifyToken(tok) {
		return true
	}
	writeError(w, codeUnauthorized, "来源IP不在白名单或未登录", http.StatusUnauthorized)
	return false
}

func (s *apiServer) tokenFromReq(r *http.Request) string {
	if h := r.Header.Get("Authorization"); strings.HasPrefix(h, "Bearer ") {
		return strings.TrimPrefix(h, "Bearer ")
	}
	return r.Header.Get("X-Auth-Token")
}

func (s *apiServer) signToken() string {
	payload := base64.RawURLEncoding.EncodeToString([]byte(fmt.Sprintf(`{"u":"admin","iat":%d}`, time.Now().UnixMilli())))
	mac := hmac.New(sha256.New, s.secret)
	mac.Write([]byte(payload))
	return payload + "." + base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
}

func (s *apiServer) verifyToken(token string) bool {
	parts := strings.Split(token, ".")
	if len(parts) != 2 {
		return false
	}
	mac := hmac.New(sha256.New, s.secret)
	mac.Write([]byte(parts[0]))
	expect := base64.RawURLEncoding.EncodeToString(mac.Sum(nil))
	if !hmac.Equal([]byte(expect), []byte(parts[1])) {
		return false
	}
	raw, err := base64.RawURLEncoding.DecodeString(parts[0])
	if err != nil {
		return false
	}
	var p struct {
		Iat int64 `json:"iat"`
	}
	if json.Unmarshal(raw, &p) != nil {
		return false
	}
	return time.Since(time.UnixMilli(p.Iat)) < 24*time.Hour
}

// ---- 登录（带失败锁定，同 Node 版 5 次/60s）----

func (s *apiServer) handleLogin(w http.ResponseWriter, r *http.Request) {
	ip := s.clientIP(r)
	s.loginMu.Lock()
	rec := s.loginFails[ip]
	now := time.Now()
	if rec != nil && now.Before(rec.lockedUntil) {
		wait := int(rec.lockedUntil.Sub(now).Seconds())
		s.loginMu.Unlock()
		writeError(w, codeRateLimited, fmt.Sprintf("登录失败次数过多，请 %d 秒后重试", wait), http.StatusTooManyRequests)
		return
	}
	s.loginMu.Unlock()

	var body struct {
		Key string `json:"key"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.Key != s.cfg.APIKey {
		s.loginMu.Lock()
		if s.loginFails[ip] == nil {
			s.loginFails[ip] = &failRec{}
		}
		s.loginFails[ip].fail++
		if s.loginFails[ip].fail >= 5 {
			s.loginFails[ip].lockedUntil = now.Add(60 * time.Second)
			s.loginFails[ip].fail = 0
		}
		s.loginMu.Unlock()
		logx.Warnf("AUTH", "管理台登录失败 来源:%s", ip)
		writeError(w, codeUnauthorized, "API Key 无效", http.StatusUnauthorized)
		return
	}
	s.loginMu.Lock()
	delete(s.loginFails, ip)
	s.loginMu.Unlock()
	writeOK(w, map[string]any{
		"token":        s.signToken(),
		"expires_in":   86400,
		"api_key_hint": mask(s.cfg.APIKey),
	})
}

func mask(s string) string {
	if len(s) <= 2 {
		return "**"
	}
	return s[:2] + "****"
}

// ---- 路由 ----

func (s *apiServer) route(w http.ResponseWriter, r *http.Request) {
	path := r.URL.Path
	switch {
	case r.Method == http.MethodGet && path == "/api/config":
		s.handleGetConfig(w, r)
	case r.Method == http.MethodGet && path == "/api/stats":
		s.handleStats(w, r)
	case r.Method == http.MethodGet && path == "/api/containers":
		s.handleContainers(w, r)
	case r.Method == http.MethodPost && path == "/api/create":
		s.handleCreate(w, r)
	case r.Method == http.MethodPost && path == "/api/delete":
		s.handleDelete(w, r)
	case r.Method == http.MethodPost && path == "/api/rebuild":
		s.handleRebuild(w, r)
	case r.Method == http.MethodPost && path == "/api/start":
		s.handleStart(w, r)
	case r.Method == http.MethodPost && path == "/api/stop":
		s.handleStop(w, r)
	case r.Method == http.MethodGet && path == "/api/check":
		s.handleCheck(w, r)
	case r.Method == http.MethodGet && strings.HasPrefix(path, "/api/job/"):
		s.handleJob(w, r)
	case r.Method == http.MethodGet && path == "/api/whitelist":
		writeOK(w, map[string]any{"whitelist": s.cfg.IPWhitelist})
	case r.Method == http.MethodPost && path == "/api/whitelist":
		s.handleWhitelist(w, r)
	case r.Method == http.MethodPost && path == "/api/creds/set":
		s.handleCredsSet(w, r)
	case r.Method == http.MethodGet && path == "/api/proxies":
		s.handleProxies(w, r)
	case r.Method == http.MethodGet && path == "/api/pool/acquire":
		s.handleAcquire(w, r)
	case r.Method == http.MethodPost && path == "/api/pool/release":
		s.handleRelease(w, r)
	case r.Method == http.MethodGet && path == "/api/pool/status":
		s.handlePoolStatus(w, r)
	case r.Method == http.MethodPost && path == "/api/pool/rotate":
		s.handleRotate(w, r)
	case r.Method == http.MethodGet && path == "/api/usage":
		writeOK(w, map[string]any{"total": 0, "per_instance": map[string]int{}})
	default:
		writeError(w, codeNotFound, "接口不存在", http.StatusNotFound)
	}
}

// ---- 处理器 ----

func (s *apiServer) handleGetConfig(w http.ResponseWriter, _ *http.Request) {
	c := *s.cfg
	c.APIKey = mask(c.APIKey)
	c.ProxyPassword = mask(c.ProxyPassword)
	writeOK(w, c)
}

func (s *apiServer) handleStats(w http.ResponseWriter, _ *http.Request) {
	writeOK(w, map[string]any{
		"resident_count":  s.cfg.ResidentCount,
		"instance_count":  s.mgr.Count(),
		"healthy_count":   s.mgr.HealthyCount(),
		"sticky_sessions": s.mgr.StickyActive(),
		"mode":            "embedded",
		"transport":       "masque",
	})
}

func (s *apiServer) handleContainers(w http.ResponseWriter, _ *http.Request) {
	out := []map[string]any{}
	for _, inst := range s.mgr.All() {
		port := s.cfg.BasePort + inst.Index
		out = append(out, map[string]any{
			"name":             store.Name(inst.Index),
			"index":            inst.Index,
			"port":             port,
			"running":          inst.Status == pool.StatusRunning,
			"pid":              0,
			"enginePid":        0,
			"engineRunning":    inst.Status == pool.StatusRunning,
			"configExists":     true,
			"ip":               inst.LastExitIP,
			"endpoint":         nil,
			"ipv6":             inst.Meta.IPv6,
			"dedicated":        inst.Meta.Dedicated,
			"dedicated_remark": inst.Meta.Remark,
			"status":           inst.Status,
			"cf_403_count":     inst.CF403Count,
			"cf_check_count":   inst.CFCheckCnt,
		})
	}
	writeOK(w, out)
}

func (s *apiServer) handleCreate(w http.ResponseWriter, r *http.Request) {
	var body struct {
		InstanceCount *int `json:"instance_count"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.InstanceCount != nil {
		n := clamp(*body.InstanceCount, 1, s.cfg.MaxInstances)
		if n != s.cfg.ResidentCount {
			s.cfg.ResidentCount = n
			saveConfig(s.cfg)
		}
	}
	target := s.cfg.ResidentCount
	j := s.newJob("create")
	writeOK(w, map[string]any{"jobId": j.ID})
	go func() {
		healthy := 0
		for i := 0; i < target; i++ {
			j.addLog(fmt.Sprintf("创建实例 warp-%d...", i))
			if _, ok := s.mgr.Get(i); ok {
				j.addLog(fmt.Sprintf("warp-%d 已存在，跳过", i))
				continue
			}
			if err := s.mgr.Create(r.Context(), i); err != nil {
				j.addLog(fmt.Sprintf("warp-%d 创建失败: %v", i, err))
				j.addResult(jobResult{Success: false, Name: store.Name(i), Error: err.Error()})
				continue
			}
			j.addResult(jobResult{Success: true, Name: store.Name(i)})
			healthy++
		}
		j.finish(fmt.Sprintf("创建完成，健康实例 %d/%d", healthy, target))
	}()
}

func (s *apiServer) handleDelete(w http.ResponseWriter, r *http.Request) {
	idx, err := s.indexFromBody(r)
	if err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	if err := s.mgr.DeleteInstance(idx); err != nil {
		writeError(w, codeInstanceError, err.Error(), http.StatusInternalServerError)
		return
	}
	writeOK(w, map[string]any{"deleted": store.Name(idx)})
}

func (s *apiServer) handleRebuild(w http.ResponseWriter, r *http.Request) {
	idx, err := s.indexFromBody(r)
	if err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	j := s.newJob("rebuild")
	writeOK(w, map[string]any{"jobId": j.ID})
	go func() {
		j.addLog(fmt.Sprintf("重建 warp-%d（重新注册账号）...", idx))
		if err := s.mgr.Rebuild(r.Context(), idx); err != nil {
			j.addResult(jobResult{Success: false, Name: store.Name(idx), Error: err.Error()})
			j.fail()
			return
		}
		j.addResult(jobResult{Success: true, Name: store.Name(idx)})
		j.finish("重建完成")
	}()
}

func (s *apiServer) handleStart(w http.ResponseWriter, r *http.Request) {
	idx, err := s.indexFromBody(r)
	if err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	if err := s.mgr.StartInstance(r.Context(), idx); err != nil {
		writeError(w, codeInstanceError, err.Error(), http.StatusInternalServerError)
		return
	}
	writeOK(w, map[string]any{"started": store.Name(idx)})
}

func (s *apiServer) handleStop(w http.ResponseWriter, r *http.Request) {
	idx, err := s.indexFromBody(r)
	if err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	if err := s.mgr.StopInstance(idx); err != nil {
		writeError(w, codeInstanceError, err.Error(), http.StatusInternalServerError)
		return
	}
	writeOK(w, map[string]any{"stopped": store.Name(idx)})
}

func (s *apiServer) handleCheck(w http.ResponseWriter, r *http.Request) {
	qi := r.URL.Query().Get("index")
	if qi == "" {
		writeError(w, codeInvalidInput, "缺少 index 参数", http.StatusOK)
		return
	}
	idx, err := strconv.Atoi(qi)
	if err != nil {
		writeError(w, codeInvalidInput, "index 非法", http.StatusOK)
		return
	}
	inst, ok := s.mgr.Get(idx)
	if !ok {
		writeError(w, codeNotFound, "实例不存在", http.StatusOK)
		return
	}
	res := s.mgr.ProbeOnce(inst)
	if res.HTTPCode == 0 {
		writeError(w, codeIPCheckFailed, res.Error, http.StatusOK)
		return
	}
	writeOK(w, map[string]any{
		"index":      idx,
		"http_code":  res.HTTPCode,
		"latency_ms": res.LatencyMS,
		"ipv6":       inst.Meta.IPv6,
	})
}

func (s *apiServer) handleJob(w http.ResponseWriter, r *http.Request) {
	id := strings.TrimPrefix(r.URL.Path, "/api/job/")
	s.jobsMu.Lock()
	j, ok := s.jobs[id]
	s.jobsMu.Unlock()
	if !ok {
		writeError(w, codeNotFound, "任务不存在", http.StatusOK)
		return
	}
	j.mu.Lock()
	defer j.mu.Unlock()
	writeOK(w, j)
}

func (s *apiServer) handleWhitelist(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Whitelist []string `json:"whitelist"`
		IP        string   `json:"ip"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.IP != "" && len(body.Whitelist) == 0 {
		body.Whitelist = append(s.cfg.IPWhitelist, body.IP)
	}
	if len(body.Whitelist) == 0 {
		writeError(w, codeInvalidInput, "缺少 whitelist/ip", http.StatusOK)
		return
	}
	if err := s.mgr.SetWhitelist(body.Whitelist); err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	s.cfg.IPWhitelist = body.Whitelist
	saveConfig(s.cfg)
	writeOK(w, map[string]any{"whitelist": s.cfg.IPWhitelist})
}

func (s *apiServer) handleCredsSet(w http.ResponseWriter, r *http.Request) {
	var body struct {
		Username string `json:"username"`
		Password string `json:"password"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil || body.Username == "" || body.Password == "" {
		writeError(w, codeInvalidInput, "需要 username/password", http.StatusOK)
		return
	}
	s.cfg.ProxyUsername = body.Username
	s.cfg.ProxyPassword = body.Password
	saveConfig(s.cfg)
	writeOK(w, map[string]any{"updated": true, "username": body.Username})
}

func (s *apiServer) handleProxies(w http.ResponseWriter, _ *http.Request) {
	ip := s.mgr.PublicIP(context.Background())
	out := []map[string]any{}
	for _, inst := range s.mgr.All() {
		if inst.Status != pool.StatusRunning {
			continue
		}
		out = append(out, map[string]any{
			"proxy":     fmt.Sprintf("%s:%d", ip, s.cfg.BasePort+inst.Index),
			"index":     inst.Index,
			"ipv6":      inst.Meta.IPv6,
			"exit_ipv6": inst.Meta.IPv6,
		})
	}
	writeOK(w, out)
}

func (s *apiServer) handleAcquire(w http.ResponseWriter, r *http.Request) {
	q := r.URL.Query()
	sticky := q.Get("sticky") == "1" || strings.EqualFold(q.Get("sticky"), "true")
	sessionID := q.Get("session_id")
	inst, err := s.mgr.Acquire(sticky, sessionID)
	if err != nil {
		writeError(w, codeNoProxy, "无可用代理实例", http.StatusServiceUnavailable)
		return
	}
	ip := s.mgr.PublicIP(r.Context())
	data := map[string]any{
		"proxy":     fmt.Sprintf("%s:%d", ip, s.cfg.BasePort+inst.Index),
		"index":     inst.Index,
		"exit_ipv6": inst.Meta.IPv6,
		"ipv6":      inst.Meta.IPv6,
	}
	if s.cfg.ProxyAuthEnabled {
		data["username"] = s.cfg.ProxyUsername
		data["password"] = s.cfg.ProxyPassword
	}
	if sticky {
		data["session_id"] = sessionID
		data["expires_in"] = s.cfg.StickyMinutes * 60
	}
	writeOK(w, data)
}

func (s *apiServer) handleRelease(w http.ResponseWriter, r *http.Request) {
	var body struct {
		SessionID string `json:"session_id"`
	}
	_ = json.NewDecoder(r.Body).Decode(&body)
	if body.SessionID == "" {
		body.SessionID = r.URL.Query().Get("session_id")
	}
	if body.SessionID == "" {
		writeError(w, codeInvalidInput, "缺少 session_id", http.StatusOK)
		return
	}
	if !s.mgr.Release(body.SessionID) {
		writeError(w, codeNotFound, "会话不存在或已过期", http.StatusOK)
		return
	}
	writeOK(w, map[string]any{"released": true})
}

func (s *apiServer) handlePoolStatus(w http.ResponseWriter, _ *http.Request) {
	total := s.mgr.Count()
	writeOK(w, map[string]any{
		"total":           total,
		"healthy":         s.mgr.HealthyCount(),
		"sticky_sessions": s.mgr.StickyActive(),
		"min_available":   s.cfg.MinAvailable,
	})
}

func (s *apiServer) handleRotate(w http.ResponseWriter, r *http.Request) {
	idx, err := s.indexFromBody(r)
	if err != nil {
		writeError(w, codeInvalidInput, err.Error(), http.StatusOK)
		return
	}
	j := s.newJob("rotate")
	writeOK(w, map[string]any{"jobId": j.ID})
	go func() {
		if err := s.mgr.Rotate(r.Context(), idx); err != nil {
			j.addResult(jobResult{Success: false, Name: store.Name(idx), Error: err.Error()})
			j.fail()
			return
		}
		j.addResult(jobResult{Success: true, Name: store.Name(idx)})
		j.finish("换 IP 完成")
	}()
}

// indexFromBody 支持 {"index":N} 或 {"name":"warp-N"}。
func (s *apiServer) indexFromBody(r *http.Request) (int, error) {
	var body struct {
		Index *int   `json:"index"`
		Name  string `json:"name"`
	}
	if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
		return 0, fmt.Errorf("请求体需为 JSON")
	}
	if body.Index != nil {
		return *body.Index, nil
	}
	if body.Name == "" {
		return 0, fmt.Errorf("缺少 index/name")
	}
	if !strings.HasPrefix(body.Name, "warp-") {
		return 0, fmt.Errorf("实例名 %q 非法", body.Name)
	}
	return strconv.Atoi(strings.TrimPrefix(body.Name, "warp-"))
}

// ---- job ----

func (s *apiServer) newJob(typ string) *job {
	id := fmt.Sprintf("job-%d", time.Now().UnixNano())
	j := &job{ID: id, Type: typ, Status: "running", Logs: []string{}, Results: []jobResult{}}
	s.jobsMu.Lock()
	s.jobs[id] = j
	// 保留最近 200 个
	if len(s.jobs) > 200 {
		for k := range s.jobs {
			delete(s.jobs, k)
			break
		}
	}
	s.jobsMu.Unlock()
	return j
}

func (j *job) addLog(msg string) {
	j.mu.Lock()
	j.Logs = append(j.Logs, time.Now().Format("15:04:05")+" "+msg)
	j.mu.Unlock()
}

func (j *job) addResult(res jobResult) {
	j.mu.Lock()
	j.Results = append(j.Results, res)
	j.mu.Unlock()
}

func (j *job) finish(msg string) {
	j.mu.Lock()
	j.Status = "completed"
	j.Logs = append(j.Logs, time.Now().Format("15:04:05")+" "+msg)
	j.mu.Unlock()
}

func (j *job) fail() {
	j.mu.Lock()
	j.Status = "failed"
	j.mu.Unlock()
}

// ---- 序列化助手 ----

func writeOK(w http.ResponseWriter, data any) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	_ = json.NewEncoder(w).Encode(map[string]any{codeFieldName: codeOK, msgFieldName: "", dataField: data})
}

func writeError(w http.ResponseWriter, code, msg string, httpStatus int) {
	w.Header().Set("Content-Type", "application/json; charset=utf-8")
	w.WriteHeader(httpStatus)
	_ = json.NewEncoder(w).Encode(map[string]any{codeFieldName: code, msgFieldName: msg, dataField: nil})
}

const (
	codeFieldName = "code"
	msgFieldName  = "msg"
	dataField     = "data"
)

func clamp(v, lo, hi int) int {
	if v < lo {
		return lo
	}
	if v > hi {
		return hi
	}
	return v
}

func saveConfig(cfg *appconfig.Config) {
	// 配置热更新落盘（保持键序由 JSON marshal 决定；Node 版为无序对象，等价）
	raw, err := json.MarshalIndent(cfg, "", "  ")
	if err != nil {
		return
	}
	path := os.Getenv("WARP_CONFIG_PATH")
	if path == "" {
		path = "config.json"
	}
	_ = os.WriteFile(path, raw, 0o644)
}
