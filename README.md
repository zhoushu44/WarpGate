# WARP 代理池管理系统

基于 **sing-box + Cloudflare WARP** 的多出口 IP SOCKS5 代理池。系统在单台 Linux 服务器上直接运行多个相互隔离的 WARP 实例（进程化架构，不依赖 Docker），每个实例独立注册 Cloudflare WARP 账号，拥有**独立的 IPv6 出口 IP**，并共享边缘节点的 **IPv4 NAT 池**。

- 单机支持 1～200 个 WARP 实例（当前生产环境 20 个）
- **20 个唯一 IPv6 出口**（每个账号独立分配，`2a09:bacx::/29` 段）
- **4 个 IPv4 出口**（由 Cloudflare 边缘节点 NAT 池决定，动态轮换）
- **多对端（Endpoint）优选**：5 个边缘接入点轮询分配，进一步扩展出口 IP 组合（账号数 × 端点数的组合）
- API 按 Round-Robin 顺序提取代理，连续调用返回不同出口 IP
- 支持独占的粘性会话（同一会话固定同一出口 IP）
- 健康检查、IP 唯一性检测、403 率统计与自动换 IP
- Web 管理页面支持创建、重建、停止、删除、换 IP、改配置

---

## 目录

- [架构](#架构)
- [技术原理](#技术原理)
  - [出口 IP 分配机制](#出口-ip-的分配机制核心)
  - [多对端（Endpoint）优选](#多对端endpoint优选与出口扩展)
  - [账号注册机制](#账号注册机制)
  - [为什么 WARP 出口都是数据中心 IP](#为什么-warp-出口都是数据中心-ip)
  - [检测端点的坑](#检测端点的坑重要)
- [环境要求](#环境要求服务器端)
- [安装与启动](#安装与启动服务器端)
- [实例管理](#实例管理)
- [API 参考](#api-参考)
- [在业务程序中使用](#在业务程序中使用)
- [403 统计与自动换 IP](#403-统计与自动换-ip)
- [WARP Teams 无限流量](#warp-teams-无限流量账户可选)
- [主要配置](#主要配置)
- [当前生产环境实测数据](#当前生产环境实测数据192612116)
- [多服务器扩展](#多服务器扩展增加-ipv4-出口)
- [故障排查（FAQ）](#故障排查faq)
- [测试](#测试)
- [安全提示](#安全提示)

---

## 架构

| 版本 | 实例运行方式 | 注册方式 | IPv4 出口 | IPv6 出口 |
|---|---|---|---|---|
| 旧版（已废弃） | Docker 容器 | wgcf | 共享 1 个 | 共享 |
| 当前版 | sing-box 进程 | **registerViaAPI（直连 CF API）优先 + wgcf 回退** | **4 个（NAT 池轮换）** | **每实例 1 个（20 个唯一）** |

### 目录结构

**服务器端：**

```text
/root/warp/
├── server.js                 # 主服务（Node.js，端口 4433）
├── config.json               # 运行配置
├── logs/server.log           # 运行日志
└── package.json

/root/warp-instances/
└── warp-{idx}/               # 每个实例一个目录（idx = 0..N-1）
    ├── singbox.json          # sing-box 配置（WireGuard 在 endpoints 数组）
    ├── wgcf-account.toml     # WARP 账号凭据（API 注册成功也会生成等效文件）
    ├── wgcf-profile.conf     # WireGuard 配置文件
    ├── meta.json             # 元数据（pid / ipv6 / endpoint）
    └── pid                   # 进程号文件
```

**本地开发（Windows）：**

```text
server.js                  # 与服务器同步的主服务
deploy-v3.ps1              # 部署脚本（SFTP 上传 + 重启 systemd 服务）
run-verify-endpoints.ps1   # 端点/IP 分配验证脚本
test-*.sh / diag-*.ps1     # 测试与诊断脚本
ref-warp-yg/               # 参考项目 yonggekkk/warp-yg 源码（对比研究）
```

---

## 技术原理

### 出口 IP 的分配机制（核心）

```
你的服务器 (YOUR_SERVER_IP)
   │  就近接入
   ▼
Cloudflare 边缘节点 (colo=LAX 洛杉矶)
   ├── IPv6 出口：每个 WARP 注册账号分配一个唯一地址
   │     → 20 个账号 = 20 个不同 IPv6 (2a09:bac1::/29 段)
   └── IPv4 出口：边缘节点的共享 NAT 池
         → LAX 节点池 = 104.28.195.186 / .187 / 104.28.227.186 / .187
         → 所有账号从这 4 个 IP 中动态轮换
```

**关键结论：**

1. **IPv6 数量 = 账号数量**。每次注册生成独立账号和唯一 IPv6，这是实现大量不同出口 IP 的途径。
2. **IPv4 数量由边缘节点决定，与账号数无关**。同一台服务器永远连接最近的边缘节点（LAX），其 NAT 池固定为 4 个 IPv4。注册 200 个账号 IPv4 出口仍只有这 4 个。
3. **想增加 IPv4 出口**，唯一办法是增加**不同地区/不同机房**的服务器（每台连接不同边缘节点，各自贡献 4 个 IPv4）。
4. **IPv4 是动态轮换的**：同一实例连续请求，出口可能在不同 IPv4 间切换；业务如需固定 IPv4，需使用粘性会话并接受其限制。
5. **注册方式不影响 IPv4 出口（已实测验证）**：wgcf CLI、直接调用 CF 注册 API（不同 type/UA/参数）注册的账号，连接同一 endpoint 时 IPv4 出口均来自同一个 4 IP NAT 池（`104.28.195.186/187`、`104.28.227.186/187`）。所有 WARP 注册工具（wgcf / warp-go / 本项目 API）底层调用的是同一个 Cloudflare 注册接口，区别仅在是否携带 license/team_token。

### 多对端（Endpoint）优选与出口扩展

WARP 对端（endpoint）就是 sing-box WireGuard 配置中 `peers[].address` 指向的 Cloudflare 边缘接入点。**同一个 WARP 账号连接不同的 endpoint，得到的出口 IP 不同**（IPv4、IPv6 均有差异），因此：

```
出口 IP 组合数 ≈ 账号数 × 可用 endpoint 数
```

系统内置 5 个实测可用的 endpoint：

| endpoint 地址 | 端口 | 说明 |
|---|---:|---|
| `188.114.96.1` | 2408 | 洛杉矶节点 |
| `188.114.97.1` | 2408 | 洛杉矶节点 |
| `188.114.96.2` | 2408 | 洛杉矶节点 |
| `188.114.96.3` | 2408 | 洛杉矶节点 |
| `162.159.192.1` | 2408 | 备用节点 |

**工作机制：**

1. **启动探针**：服务启动时 `probeEndpoints()` 对 5 个候选端点做 UDP 连通性探测，剔除不可用项，生成 `availableEndpoints` 列表。
2. **新建实例分配**：`getEndpointForInstance(idx) = availableEndpoints[idx % 列表长度]`。相邻实例分配不同 endpoint，出口 IP 更分散。20 实例 × 5 端点 = 每端点恰好 4 个实例。
3. **换 IP 时端点轮换**：`nextEndpointForInstance(idx) = availableEndpoints[(idx+1) % 长度]`。自动换 IP 时轮换到下一个 endpoint 再注册新账号，避免同一 endpoint 换 IP 后出口仍相同。
4. **元数据记录**：每个实例的 `meta.json` 记录 `endpoint` 字段，`/api/containers` 返回每实例的 endpoint 与 ipv6，端点分配可观测。

> 注意：部分 endpoint（如 `162.159.193.1`）虽能完成 UDP 握手，但实际无法承载流量（出口 IP 检测超时），已被从候选列表剔除。若某个实例频繁出现"隧道已建立但无出口 IP"，大概率是该 endpoint 或该账号的瞬时故障，重建实例即可恢复。

### 账号注册机制

系统按 **registerViaAPI → registerViaWgcf** 顺序注册账号，API 失败自动回退 wgcf：

| 方式 | 实现 | 请求体/命令 | 支持 Teams | 备注 |
|---|---|---|:---:|---|
| **registerViaAPI**（首选） | 直接调用 CF 注册 API | `POST https://api.cloudflareclient.com/v0a2158/reg`，`type=Android`、`model=warp-manager`、UA `okhttp/3.12.1`，携带 X25519 公钥 | ✅（body 加 `team_token`） | 内置 429 退避重试（5 次，间隔 5s/10s/15s/20s 递增） |
| **registerViaWgcf**（回退） | wgcf CLI | `echo \| wgcf register --accept-tos` | ❌ | wgcf 2.2.27 |
| warp-go（参考项目） | warp-go CLI | `warp-go --register ...` | ✅（`--team-config`） | **已过时**：默认携带 referral code，CF 拒绝（错误码 1070） |

关键点：

- 所有方式底层都是同一个 CF 注册接口，注册成功返回账号 ID、私钥、唯一 IPv6 内部地址。
- **429 限流**：Cloudflare API 批量注册（约 16 个并发）会触发 429，系统已内置退避重试；`max_rebuild_attempts` 控制单实例重建重试次数（默认 3）。
- **Teams 无限流量必须走 registerViaAPI**：wgcf CLI 不支持 `team_token` 字段，只有直连 API 才能在注册时携带 team_token（见下文 [WARP Teams](#warp-teams-无限流量账户可选)）。

### 为什么 WARP 出口都是"数据中心 IP"

- IPv4 出口属于 Cloudflare（AS13335），WHOIS 标记为 "Cloudflare, Inc."
- IPv6 出口属于 Cloudflare WARP 专用段 `2a09:bac1::/29`
- 部分网站（如 **OpenAI / ChatGPT**）会拦截所有数据中心 IP，无论 WARP、VPS 原生 IP 还是其他机房 IP，均返回 HTTP 403
- 实测：WARP 的 4 个 IPv4 与 20 个 IPv6、服务器原生 IP，访问 `chatgpt.com`、`auth.openai.com`、`api.openai.com` **全部 403**；访问 Google（支持 IPv6）则全部正常
- 因此本系统适用于大多数站点（Google、普通网站、爬虫轮换 IP 等），但**不适用于 OpenAI 账号注册**。OpenAI 注册需要住宅代理（真实家庭宽带 IP）。

### 检测端点的坑（重要）

- `api.ipify.org`、`ifconfig.co`、`api-ipv4.ip.sb` 等端点本身**托管在 Cloudflare 后面**，WARP 流量访问它们时，Cloudflare 内部看到统一出口，会**误报所有实例 IPv4 相同**
- 必须使用**非 Cloudflare 托管**的端点（如 `ip-api.com`、`checkip.amazonaws.com`）才能看到真实的 4 个 IPv4 NAT 池
- 系统默认 `ip_check_url` 使用 `api.ipify.org`（IPv4）、`api64.ipify.org`（IPv6 优先），管理页面展示的是 IPv6 出口（每实例唯一）

---

## 环境要求（服务器端）

- Linux（Debian/Ubuntu 等），内核无需额外模块（用户态 WireGuard）
- Node.js 18+
- 服务器需能访问 `engage.cloudflareclient.com`（UDP 2408 端口）
- 部署产物：
  - `/usr/local/bin/sing-box`（用户态 WireGuard 代理，从官方镜像提取，无需 Docker）
  - `/usr/local/bin/wgcf`（WARP 账号注册工具，回退方案）
  - `/root/warp-instances/warp-{idx}/`（每个实例的配置目录）

---

## 安装与启动（服务器端）

上传项目到服务器后：

```bash
npm install
```

启动服务：

```bash
npm start
```

或使用 systemd 守护：

```ini
[Unit]
Description=WARP Proxy Pool Manager
After=network.target

[Service]
WorkingDirectory=/root/warp
ExecStart=/usr/bin/node server.js
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

打开管理页面：

```text
http://<服务器IP>:4433
```

默认创建 20 个实例，端口 `10010～10029`。每个实例创建流程：

1. 探测可用 endpoint（仅启动时）
2. `registerViaAPI` 注册新账号（失败回退 wgcf），按实例索引分配 endpoint
3. 生成 sing-box 配置（WireGuard 置于 `endpoints` 数组，写入所选 endpoint）
4. 启动 sing-box 进程（`detached: true`）
5. 等待 8 秒，通过 SOCKS5 端口检测 IPv6 出口（最多重试 10 次）
6. 保存 `meta.json`（pid / ipv6 / endpoint），加入代理池

---

## 实例管理

- 每个实例是一个独立的 `sing-box run` 进程，由 Node.js 通过 `child_process` 管理
- 实例目录：`/root/warp-instances/warp-{idx}/`，含 `singbox.json`（代理配置）、`meta.json`（元数据）、`pid`（进程号）
- **换 IP** = 停止进程 → 重新注册 WARP 账号（`registerWarpAccount`）→ **轮换到下一个 endpoint** → 生成新配置 → 启动新进程 → 检测新 IP（最多 `max_rebuild_attempts` 次）
- **服务器重启后**，Node.js 启动时扫描 `/root/warp-instances/` 自动恢复所有实例
- 管理页面提供：创建、重建（重新注册账号）、停止、启动、删除、强制换 IP（`/api/pool/rotate`）

---

## API 参考

### 代理提取

**普通提取**（Round-Robin，不占用实例）：

```http
GET /api/pool/acquire
```

响应示例：

```json
{
  "success": true,
  "data": {
    "session_id": null,
    "proxy": "socks5://127.0.0.1:10010",
    "ip": "2a09:bac1:76e0:3d0::48c:5f",
    "expires_at": null,
    "sticky_minutes": null,
    "sticky": false
  }
}
```

**粘性提取**（同一 session_id 锁定同一实例，默认 10 分钟）：

```http
GET /api/pool/acquire?session_id=account-001
```

规则：首次分配实例并立即 `in_use`（其他提取不能获取）→ 相同 session_id 复用同一代理并刷新过期时间 → 超时未用自动释放。

**主动释放粘性会话：**

```http
POST /api/pool/release
Content-Type: application/json

{"session_id": "account-001"}
```

**上报业务方真实状态码**（用于 403 统计，SOCKS5 无法读取 HTTPS 真实状态码）：

```http
POST /api/pool/report
Content-Type: application/json

{"session_id": "account-001", "result": 403}
```

**强制换 IP**（指定实例重新注册账号 + 轮换 endpoint）：

```http
POST /api/pool/rotate
Content-Type: application/json

{"name": "warp-0"}
```

**代理池状态：**

```http
GET /api/pool/status
```

### 实例管理 API

| 接口 | 方法 | 参数 | 说明 |
|---|---|---:|---|
| `/api/containers` | GET | - | 全部实例状态（含 ip / endpoint / ipv6 / health / pool_status） |
| `/api/check` | GET | - | 检查服务与关键依赖是否就绪 |
| `/api/stats` | GET | - | 实例统计 |
| `/api/create` | POST | `{"count": N}` | 创建 N 个新实例 |
| `/api/delete` | POST | `{"name": "warp-0"}` | 删除实例（停止进程 + 删除目录） |
| `/api/rebuild` | POST | `{"name": "warp-0"}` 或 `{"names": ["warp-0","warp-1"]}` | 重建实例（重新注册账号 + 重新分配 endpoint） |
| `/api/start` | POST | `{"name": "warp-0"}` | 启动实例 |
| `/api/stop` | POST | `{"name": "warp-0"}` | 停止实例 |
| `/api/job/:id` | GET | - | 查询重建/创建任务进度（status / current / total / logs） |
| `/api/config` | GET | - | 查看配置（team_token 脱敏，仅显示前 6 位） |
| `/api/config` | POST | `{"team_token": "..."}` 等 | 修改配置（白名单字段） |

---

## 在业务程序中使用

提取到的地址是 SOCKS5 代理，例如：

```text
socks5://127.0.0.1:10010
```

请求链路：

```text
业务程序 → SOCKS5端口 → sing-box (WireGuard) → Cloudflare边缘 → WARP出口IP → 目标网站
```

**IPv4 / IPv6 出口选择：** sing-box 配置默认 IPv6 优先（`prefer_ipv6`），目标网站支持 IPv6 时走独立 IPv6 出口（每实例唯一）；仅支持 IPv4 时走共享 NAT 池（4 个 IPv4 轮换）。如需强制 IPv4，可修改 `singbox.json` 中 DNS `strategy` 为 `ipv4_only`。

如果服务部署在其他机器，需要将 `bind_address`、`public_ip` 和防火墙端口配置为实际环境，并确保 SOCKS5 端口能够被业务机器访问。

---

## 403 统计与自动换 IP

系统有两类统计来源：

- 定时健康巡检通过实例访问检测目标（默认 `https://chatgpt.com`）。
- 每次普通或粘性提取后，在后台通过该实例主动探测目标站。

默认规则：

```text
检测次数 >= 3
403率 >= 50%
```

达到阈值后：

- 普通实例处于空闲状态，会立即开始自动换 IP（重新注册 WARP 账号 + 轮换 endpoint）。
- 粘性实例正在使用时，会先标记为待换 IP；会话释放或过期后再执行换 IP，避免中途断开业务。
- 换 IP 成功后清空该实例的 403 统计。

**自动重建（健康检查兜底）**：`auto_rebuild_on_unhealthy=true` 时，健康巡检发现实例异常（如 IP 检测失败、403 率过高）且处于 available 状态，自动触发重建。

注意：SOCKS5 无法读取客户端 HTTPS 请求返回的真实 HTTP 状态码。因此"使用时统计"是提取后的后台主动探测。业务程序如需按真实响应统计，可调用 `/api/pool/report` 上报接口。

---

## WARP Teams 无限流量账户（可选）

免费 WARP 账号流量有配额上限（约 1GB 后限速）。配置 **WARP Teams（Cloudflare Zero Trust）** 团队 Token 后，新注册的账号为 **Teams 账户，无流量限制**：

```bash
# 1. 通过配置 API 设置 team_token
curl -X POST http://127.0.0.1:4433/api/config \
  -H "Content-Type: application/json" \
  -d '{"team_token": "你的Teams团队Token"}'

# 2. 设置后，重新注册的账号即为 Teams 账户
curl -X POST http://127.0.0.1:4433/api/rebuild \
  -H "Content-Type: application/json" \
  -d '{"name":"warp-0"}'
```

- Token 获取：登录 Cloudflare Zero Trust 控制台 → **Networks → WARP Client** → 创建设备配置文件，复制其中的 Team Token
- **必须走 registerViaAPI**：注册请求体携带 `team_token` 字段，直接调用 CF 注册 API（`POST https://api.cloudflareclient.com/v0a2158/reg`）；wgcf CLI 不支持 team_token
- 批量注册会触发 Cloudflare API **429 限流**（约 16 个并发注册时出现），系统内置退避重试（5 次，间隔 5s/10s/15s/20s 递增），重建任务失败会自动跳过并重试
- `GET /api/config` 与页面仅显示 Token 前 6 位，不会泄露完整值

---

## 主要配置

配置保存在 `config.json`，也可在管理页面修改常用参数。

| 配置项 | 默认值 | 说明 |
|---|---:|---|
| `port` | 4433 | Web 和 API 端口 |
| `bind_address` | 0.0.0.0 | 服务监听地址 |
| `mode` | resident | 运行模式 |
| `resident_count` | 5 | 常驻实例数（auto 模式） |
| `max_instances` | 200 | 最大实例数 |
| `base_port` | 10010 | SOCKS5 起始端口（端口 = base_port + 实例序号，200 实例为 10010～10209） |
| `min_available` | 5 | 最小可用实例数 |
| `max_concurrent_create` | 3 | 并发创建实例数上限 |
| `sticky_minutes` | 10 | 粘性会话超时时间（分钟） |
| `health_check_interval` | 600 | 健康巡检间隔（秒） |
| `auto_rebuild_on_unhealthy` | true | 健康巡检发现异常时自动重建 |
| `max_rebuild_attempts` | 3 | 单实例重建/换 IP 最大尝试次数 |
| `openai_check_url` | https://chatgpt.com | CF/403 探测目标 |
| `ip_check_url` | https://api.ipify.org | IPv4 出口检测端点 |
| `ip_check_url_v6` | https://api64.ipify.org | IPv6 出口检测端点 |
| `ip_check_retries` | 10 | IP 检测重试次数 |
| `ip_check_interval` | 3000 | IP 检测重试间隔（毫秒） |
| `auto_rotate_on_high_403` | true | 403 率过高时自动换 IP |
| `cf_403_threshold` | 50 | 自动换 IP 的 403 率阈值（%） |
| `cf_min_check_count` | 3 | 判断阈值前的最小检测次数 |
| `session_cleanup_interval` | 30 | 过期会话清理间隔（秒） |
| `cleanup_on_exit` | false | 退出时是否清理实例 |
| `rate_limit_enabled` | true | 提取 API 限流开关 |
| `rate_limit_per_minute` | 60 | 提取 API 每分钟限流次数 |
| `log_level` | info | 日志级别 |
| `public_ip` | auto-detect | 公网 IP（自动检测） |
| `api_key` | 随机生成 | API 密钥（存储于 config.json） |
| `team_token` | 空 | WARP Teams (Zero Trust) 团队 Token（见上文） |

---

## 当前生产环境实测数据（YOUR_SERVER_IP）

### 出口 IP 汇总

| 类型 | 数量 | 具体地址 |
|---|---|---|
| IPv6 出口 | 20 个（每实例唯一） | `2a09:bac1:76e0:3d0::48c:5f` 等 |
| IPv4 出口 | 4 个（NAT 池轮换） | `104.28.195.186` `104.28.195.187` `104.28.227.186` `104.28.227.187` |
| 服务器原生 IP | 1 个 | `YOUR_SERVER_IP`（LAX 机房） |

### Endpoint 分配实测（20 实例 × 5 端点）

| endpoint | 实例数 | 实例 |
|---|---:|---|
| `188.114.96.1` | 4 | warp-0, 5, 10, 15 |
| `188.114.97.1` | 4 | warp-1, 6, 11, 16 |
| `188.114.96.2` | 4 | warp-2, 7, 12, 17 |
| `188.114.96.3` | 4 | warp-3, 8, 13, 18 |
| `162.159.192.1` | 4 | warp-4, 9, 14, 19 |

### 注册方式对比实测

| 注册方式 | 结果 | IPv4 出口池 |
|---|---|---|
| wgcf CLI | 成功（免费账号） | `104.28.227.186` 等（同一 4 IP 池） |
| CF API 直连（Android/warp-manager） | 成功（免费账号） | 同上池 |
| CF API 直连（iOS/1.1.1.1 UA） | 成功（免费账号） | 同上池 |
| warp-go 1.0.8 | **失败**（referral code 被拒，错误码 1070） | - |

→ 三种注册方式连同一 endpoint，IPv4 出口全部来自同一个 4 IP NAT 池，证明 **IPv4 出口与注册脚本无关**。

### 目标网站可达性实测

| 目标 | 通过 WARP | 服务器原生 IP | 说明 |
|---|---|---:|---:|---|
| Google | 200 | 200 | ✅ 正常 |
| ip-api.com / checkip.amazonaws.com | 200 | 200 | ✅ 可获取真实 IPv4 |
| chatgpt.com | **403** | **403** | ❌ OpenAI 拦截数据中心 IP |
| auth.openai.com | **403** | **403** | ❌ |
| api.openai.com | **403** | **403** | ❌（v1/models 为 401 仅缺 key）|
| sentinel.openai.com | 200 | 200 | ✅ SDK 可访问 |

---

## 多服务器扩展（增加 IPv4 出口）

**单台服务器最多 4 个 IPv4**（由边缘节点决定）。如需更多 IPv4，部署多台不同地区/不同机房的服务器，每台运行本系统：

| 方案 | IPv4 出口数 | IPv6 出口数 | 月成本（约） |
|---|---|---:|---:|---:|
| 当前单机（LAX） | 4 | 20 | 服务器本身 |
| +1 台纽约 VPS | 8 | 40 | +$3-6 |
| +3 台不同地区 VPS | 16 | 80 | +$9-18 |
| Cloudflare 官方 Dedicated IP | 2 起 | — | $6000+/年（Enterprise） |

> Cloudflare 官方 Dedicated Egress IP（$6000+/年）性价比极低：仍为数据中心 IP，且 OpenAI 可能同样拦截。

---

## 故障排查（FAQ）

| 现象 | 原因 | 解决 |
|---|---|---|
| 实例 `endpoint=none` | 旧版本创建的实例，meta.json 没有 endpoint 字段 | 通过 `/api/rebuild` 重建该实例 |
| 实例 `ip=none` | 隧道已建立但出口 IP 检测失败（endpoint 或账号瞬时故障） | 等待健康巡检自动重建，或手动 `/api/pool/rotate` |
| 批量重建部分失败 | Cloudflare API **429 限流** | 系统已内置退避重试；重跑重建任务即可 |
| 所有实例检测到相同 IPv4 | 使用了 CF 托管的检测端点（`api.ipify.org`） | 用 `ip-api.com` / `checkip.amazonaws.com` 验证 |
| warp-go 注册失败（错误 1070） | warp-go 携带 referral code，CF 已拒绝 | 本项目不使用 warp-go，改用 registerViaAPI |
| 实例握手成功但无流量 | 该 endpoint 无法承载流量（如 `162.159.193.1`） | 已从候选列表剔除；重建实例换 endpoint |
| OpenAI 全部 403 | 数据中心 IP 被 OpenAI 拦截 | 需住宅代理，本系统不适用 OpenAI 注册 |
| `sing-box: outbound detour not found` | 配置缺少 `direct-out` outbound | 使用系统 `generateSingboxConfig` 生成的完整配置 |

---

## 测试

Mock 测试（不依赖真实 WARP）：

```bash
node test/test-mock-pool.js
```

测试覆盖端口分配、IP 唯一性、Round-Robin、粘性会话、并发分配、会话过期、403 统计和自动换 IP 阈值。

语法检查：

```bash
node --check server.js
```

服务器端实测验证：

```bash
# 端点/IP 分配验证（本地）
powershell -File run-verify-endpoints.ps1
```

---

## 安全提示

当前 API 未启用鉴权（`apiKeyAuth` 中间件已预留但默认放行），服务默认监听 `0.0.0.0`。暴露到公网时，应通过防火墙仅放行管理端口，或在反向代理层增加访问控制（API Key），避免任何人创建实例、提取代理或修改配置。SOCKS5 代理端口（10010 起）也应限制访问来源，防止被滥用为公共代理。
