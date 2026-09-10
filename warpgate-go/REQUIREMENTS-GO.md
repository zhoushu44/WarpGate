# WarpGate-Go（嵌入式单进程版）需求文档

> 版本：v1.0（2026-09-10）
> 目标：将 WarpGate 从「Node.js 控制面 + 每实例 2 个引擎进程」重构为「Go 单进程：控制面与 MASQUE 数据面同进程」，在**不切回 WireGuard、保持 MASQUE 模式**的前提下，消除多实例的进程开销。

---

## 1. 背景与动机

### 1.1 现状架构（Node.js 版）

```
客户端 ── SOCKS5 ──> sing-box 进程 (实例 i, 端口 10010+i)      ← Go
                          │ 链式出站
                          ▼
                      usque 进程 (实例 i, 127.0.0.1:引擎端口)   ← Go
                          │ MASQUE CONNECT-IP (QUIC)
                          ▼
                      Cloudflare WARP 边缘（独立 IPv6 出口）
```

- 每实例 = `sing-box` + `usque` 两个进程；Node.js 只做编排（API / 生命周期 / 健康检查）。
- 单实例内存 ~30-40MB（MASQUE），其中 **Go runtime 基线与进程开销占一半以上**，隧道本身的状态只有几 MB。
- 数据面 100% 是 Go 代码：sing-box（Go）+ usque（Go，MIT，内部 quic-go + gvisor + connect-ip-go 魔改版）。

### 1.2 目标架构（本需求）

```
客户端 ── SOCKS5 ──> warpgate-go 单进程
                       ├─ SOCKS5 前端（账密模式 10010+i / 白名单模式 11010+i）
                       ├─ 实例 i 的隧道 = 进程内 usque MASQUE 连接（api.L4Proxy）
                       │    └─ MASQUE CONNECT-IP (QUIC/UDP 443) → Cloudflare WARP
                       └─ 控制面（HTTP API / 生命周期 / 健康检查 / 粘性会话）
```

- 每实例从「2 进程」变为「1 组进程内对象」，边际内存 2~5MB/实例。
- 20 实例总内存目标 <150MB（现 ~750MB）；3000 实例 <10GB（现不可行）。

### 1.3 可行性依据（已验证）

| 事实 | 来源 |
|---|---|
| usque 为 Go 编写，MIT 许可，`api/` 包为公开嵌入面 | usque 仓库 README/LICENSE |
| `api` 包（Register / EnrollKey / PrepareTlsConfig / ConnectTunnel / NewL4Proxy）**不依赖包级全局变量**，全局仅存在于 `cmd/` CLI 层 | 源码 grep 验证 |
| `api.L4Proxy.DialContext(ctx, "host:port")` 返回经 MASQUE 隧道的 `net.Conn`，可直接作 SOCKS5 后端 | `api/l4proxy.go:97` |
| usque 依赖 quic-go（QUIC/UDP），`L4Proxy` 仅支持 HTTP/3 UDP 端点 | `cmd/l4_helpers.go` |
| 现有 sing-box 配置生成中未启用 UDP 转发 → TCP-only 不是功能回退 | server.js grep 验证 |
| Go 的 `internal/` 包禁止外部导入 → usque 的证书生成 / DNS 解析器需在本项目内重新实现（各 ~40 行） | Go 语言规则 |

---

## 2. 术语

- **实例（Instance）**：一个独立注册的 WARP 账号 + 一条进程内 MASQUE 隧道 + 一个 SOCKS5 入站端口。等价于旧版的 `warp-<i>`。
- **隧道（Tunnel）**：进程内维持的一条 MASQUE CONNECT-IP 会话，封装在 `api.L4Proxy` 中。
- **暖创建 / 冷创建**：冷创建含 Cloudflare 注册（秒级~60s）；暖创建仅用已有 `usque.json` 拉起隧道（2~5s）。
- **粘性会话（Sticky）**：`session_id` 在 `sticky_minutes` 内固定绑定同一实例。

---

## 3. 总体需求（FR）

### FR-1 实例生命周期

| 编号 | 需求 | 优先级 |
|---|---|---|
| FR-1.1 | **创建**：冷创建 = 注册匿名 WARP 账号（`api.Register` + `api.EnrollKey`，`acceptTos=true`）→ 写 `usque.json`（与 usque CLI 格式逐字段兼容）→ 拉起隧道 → 出口 IPv6 唯一性检测（`ip_check_url_v6`，最多 `ip_check_retries` 次 × `ip_check_interval`）→ 写 `meta.json` | P0 |
| FR-1.2 | **重建**：删除实例目录（换号）→ 按索引重新创建；`max_rebuild_attempts` 限制 | P0 |
| FR-1.3 | **停止/启动**：停止 = 关闭隧道与监听（账号保留，目录不动）；启动 = 暖创建 | P0 |
| FR-1.4 | **删除**：关隧道 → 删实例目录 | P0 |
| FR-1.5 | **换 IP（rotate）**：删除账号重新注册，同一索引换新出口 IPv6 | P0 |
| FR-1.6 | **崩溃自愈**：隧道断开/QUIC 会话死亡 → 指数退避重连（1s→2s→4s…上限 60s）；连续失败超阈值标记实例 unhealthy → 若 `auto_rebuild_on_unhealthy` 则自动重建 | P0 |
| FR-1.7 | **并发创建限流**：同时最多 `max_concurrent_create` 个创建任务 | P0 |
| FR-1.8 | 实例数上限 `max_instances`；常驻数量 `resident_count`（超出部分按旧版规则回收，dedicated 除外为 v2） | P1 |

### FR-2 数据面（SOCKS5）

| 编号 | 需求 | 优先级 |
|---|---|---|
| FR-2.1 | 每实例入站监听：账密模式 `base_port+i` 恒开；白名单模式 `base_port+whitelist_port_offset+i` **仅当 `ip_whitelist` 非空时开放**（免密直连 + 源 CIDR 过滤，与 Node 版 sing-box 双规则语义一致；空列表 = 端口不开放）。均为进程内 listener | P0 |
| FR-2.2 | SOCKS5 TCP CONNECT；账密模式校验 `proxy_username/proxy_password`；白名单模式无认证但来源 IP 必须命中 `ip_whitelist` CIDR | P0 |
| FR-2.3 | 域名目标**本地解析**（内置 DNS，AAAA 优先——对齐 Node 版 sing-box `resolve strategy: prefer_ipv6`），无 AAAA 回落 A | P0 |
| FR-2.4 | 出站走对应实例隧道 `L4Proxy.DialContext`；UDP ASSOCIATE 返回 `command not supported`（明确拒绝而非静默丢弃） | P0 |
| FR-2.5 | 连接关闭正确回收；隧道级并发流上限沿用 usque 默认（`MaxIncomingStreams=100`） | P0 |
| FR-2.6 | 白名单模式客户端无认证直连；账密模式支持 `NO AUTH` 仅当 `proxy_auth_enabled=false` | P0 |

### FR-3 池化与调度

| 编号 | 需求 | 优先级 |
|---|---|---|
| FR-3.1 | `/api/pool/acquire`：Round-Robin 返回下一个健康实例的代理地址（与旧版响应字段兼容：`proxy`/`exit_ipv6`/`index`…） | P0 |
| FR-3.2 | 粘性会话：`?sticky=1&session_id=X` 固定绑定；`sticky_minutes` 过期；后台每 `session_cleanup_interval` 秒清理 | P0 |
| FR-3.3 | `/api/pool/release`：释放粘性占用 | P0 |
| FR-3.4 | 仅调度 `healthy` 状态实例；无可用实例返回明确错误码 | P0 |
| FR-3.5 | dedicated（固定 IP 绑定）/ remark：v2 | P2 |

### FR-4 健康检查与自动换 IP

| 编号 | 需求 | 优先级 |
|---|---|---|
| FR-4.1 | 每 `health_check_interval` 秒，经每条隧道对 `openai_check_url`（默认 cloudflare.com）发起 HTTP 探测 | P0 |
| FR-4.2 | 探测统计：HTTP 状态码计数，`cf_403_count/cf_check_count` 比率 ≥ `cf_403_threshold%` 且次数 ≥ `cf_min_check_count` → 触发 rotate | P0 |
| FR-4.3 | 探测失败（连接层）→ 实例标记 unhealthy → FR-1.6 自愈路径 | P0 |
| FR-4.4 | 每实例出站 IPv6 风控值检测（scamalytics，`ip_risk_threshold`，缓存 `ip_risk_ttl`）：**v2**（v1 仅记录探测返回的出口 IP） | P2 |

### FR-5 管理 API（兼容性分级）

路径与响应结构以 Node 版为准，分级：

| 级别 | 含义 | 端点 |
|---|---|---|
| **A 完全兼容** | 同路径、同请求、同响应结构 | `/api/login`、`/api/config`(GET)、`/api/stats`、`/api/containers`（实例列表）、`/api/pool/acquire`、`/api/pool/release`、`/api/pool/status`、`/api/check`、`/api/create`、`/api/delete`、`/api/rebuild`、`/api/start`、`/api/stop`、`/api/whitelist`(GET/POST)、`/api/creds/set` |
| **B 简化兼容** | 路径兼容但语义简化：v1 同步执行并返回 `job_id` 形状响应（Node 版为异步 job），`/api/job/:id` 立即返回完成状态 | `/api/create` 等写操作的 job 语义 |
| **C v2 交付** | `/api/pool/dedicated/*`、`/api/pool/report`、`/api/pool/rotate`（独立于健康检查的手动换 IP 保留为 A 级）、`/api/usage`、SSH 远程管理 | — |

- 鉴权：`X-API-Key` 头或 `?key=` 参数，等于 `api_key`；登录锁定（连续 5 次失败锁 60s）与 rate limit（`rate_limit_per_minute`）为 P1。
- 静态页：继续托管 `public/`（`/` 与 `/docs`）。

### FR-6 配置兼容

- `config.json` 键与 Node 版完全同名同义；v1 新增键见 §6。
- **`warp_transport` 仅支持 `masque`**；值为 `wireguard` 时启动报错并提示（WG 嵌入为 v2，经 sing-box 库）。
- `masque_http2=true`（TCP 443 兜底）**v1 不支持**：L4Proxy 仅 QUIC/UDP；配置存在但忽略，日志 warn 提示（v2 经 ConnectTunnel+netstack 实装）。
- `team_token`：v1 支持传入作为注册 JWT（团队模式）。

### FR-7 数据与部署兼容

| 编号 | 需求 |
|---|---|
| FR-7.1 | 实例目录布局不变：`instances/warp-<i>/{usque.json, meta.json, …}`；`meta.json` 字段（`index/name/pid→0/ipv6/endpoint/created_at/status`）保持可被 Node 版读写 → **两版本可原地互换** |
| FR-7.2 | 二进制单文件部署；Docker 镜像仅含 `warpgate` + `curl`（scratch/distroless，目标 <40MB） |
| FR-7.3 | `--config` 参数指定 config.json 路径；`INSTANCES_DIR` 解析逻辑与 Node 版一致 |

---

## 4. 非功能需求（NFR）

| 编号 | 指标 | 目标 |
|---|---|---|
| NFR-1 | 内存（20 实例 MASQUE） | RSS < 150MB（Node 版 ~750MB） |
| NFR-2 | 内存边际 | ≤ 5MB/实例（空闲）；≤ 8MB/实例（10 并发流） |
| NFR-3 | CPU（空闲） | < 1%（20 实例，4 核） |
| NFR-4 | 启动时间 | 二进制 <1s；20 实例暖启动 <30s（并发受 `max_concurrent_create` 限制，冷注册另计） |
| NFR-5 | 稳定性 | 单隧道 panic 不影响进程（每隧道独立 goroutine + recover）；运行 7 天无内存增长超 20% |
| NFR-6 | 平台 | linux/amd64（生产）、windows/amd64（开发自测）；静态编译 |
| NFR-7 | 日志 | 分级（debug/info/warn/error），`log_level=info` 时不逐连接记录；引擎事件（连接/断开/重连）为 info |

---

## 5. 约束、风险与对策

| 风险 | 对策 |
|---|---|
| usque `api/` 作者声明 API 不稳定 | go.mod 锁定具体 commit；升级需回归 §8 测试 |
| 失去进程隔离（旧版一隧道崩只死一个进程） | 每隧道独立 goroutine 树 + recover；隧道状态机重建；管理 API 可单独 stop/start 实例 |
| L4Proxy 仅 QUIC/UDP，`masque_http2` TCP 兜底缺失 | v1 明示降级；被 UDP 风控的环境保留 Node 版作为回退部署 |
| Cloudflare 注册风控（同 IP 高频注册 CAPTCHA） | `max_concurrent_create` 限流；失败退避；注册失败不重试超过 `max_rebuild_attempts` |
| 3000 实例 = 6000 个 listener/fd | 保留兼容端口方案仅到建议规模 500/机；超规模启用 v2 单入口路由模式 |

---

## 6. 配置新增键（v1）

| 键 | 默认 | 说明 |
|---|---|---|
| `socks_tcp_timeout` | `0`（不限） | 每连接 TCP 空闲超时（秒） |
| `tunnel_keepalive` | `30s` | MASQUE QUIC keepalive |
| `tunnel_reconnect_max_backoff` | `60s` | 重连退避上限 |
| `dns_servers` | `1.1.1.1,8.8.8.8` | 本地解析用的 DNS（仅隧道外解析目标域名） |

其余键沿用 Node 版语义（`port`/`base_port`/`whitelist_port_offset`/`resident_count`/`sticky_minutes`/`health_check_interval`/`cf_403_threshold`/`cf_min_check_count`/`auto_rebuild_on_unhealthy`/`proxy_*`/`ip_whitelist`/`api_key`/`log_level`/`team_token`/`ip_check_*`/`max_instances`/`max_concurrent_create`/`max_rebuild_attempts`/`session_cleanup_interval`/`ip_check_url_v6`/`openai_check_url`/`rate_limit_*`）。

---

## 7. 分期计划

- **v1（本需求交付）**：§3 全部 P0 + FR-1.6/1.7 + A 级 API + 兼容数据格式；测试按 §8。
- **v2**：WG 嵌入（sing-box 库）、`masque_http2`、UDP、dedicated、ip_risk、单入口凭据路由、rate limit 完整版、多机 agent。

---

## 8. 验收标准与测试计划

### 8.1 单元测试（离线，CI 可跑）

| 用例 | 断言 |
|---|---|
| 配置解析 | 默认值填充、非法值报错、Node 版 config.json 样例可加载 |
| usque.json 解析/生成 | 与 usque CLI 生成的 JSON 双向兼容（字段名/格式） |
| 端点选择 | QUIC 模式选 v4 UDP；H2 请求返回明确错误；v6 开关 |
| meta.json 读写 | Node 版样例可读；写出的文件 Node 版字段齐全 |
| SOCKS5 握手 | NO-AUTH/账密正确/账密错误/不支持命令（BIND/UDP）/白名单拒绝 |
| Round-Robin | 连续 acquire 顺序轮转、跳过 unhealthy、粘性命中 |
| 粘性过期 | sticky_minutes 过期后重新分配 |
| 403 统计 | 达到阈值触发 rotate、未达 min_check_count 不触发 |
| 重连退避 | 断开后按 1/2/4…s 重连，恢复后 healthy |

### 8.2 集成测试（需外网，本地/CI 手动触发）

1. **注册冒烟**：创建 1 实例 → Cloudflare 注册成功、`usque.json` 落盘、拿到独立 IPv6。
2. **隧道冒烟**：经该实例 SOCKS5 访问 `https://api6.ipify.org` 返回的 IPv6 == `meta.json.ipv6` 且 == 服务器其它出口不同。
3. **隔离验证**：同机开 2 实例，分别 acquire 连续取代理，出口 IPv6 互不相同且逐次轮转。
4. **自愈验证**：kill 隧道底层 QUIC（模拟断网 10s）→ 隧道自动重连 → 健康检查恢复 healthy。
5. **兼容验证**：用 Node 版生成的 `instances/` 目录直接启动 Go 版 → 全部实例暖启动成功，Node 版可再次接管。
6. **规模冒烟**（可选，8G+ 机器）：40 实例暖启动后 RSS < 300MB。

### 8.3 验收清单

- [ ] `go build` 在 linux/amd64 与 windows/amd64 通过，单二进制
- [ ] `go test ./...` 全绿
- [ ] 8.2 的 1/2/3/5 通过（4 视环境）
- [ ] 20 实例 RSS < 150MB（生产环境实测）
- [ ] Node 版 API 客户端不改代码即可调用 A 级端点
