# WARP 代理池管理系统

基于 **sing-box + Cloudflare WARP** 的多出口 IP SOCKS5 代理池。系统在单台 Linux 服务器上直接运行多个相互隔离的 WARP 实例（进程化架构，不依赖 Docker），每个实例独立注册 Cloudflare WARP 账号，拥有**独立的 IPv6 出口 IP**，并共享边缘节点的 **IPv4 NAT 池**。

- 单机支持 1～200 个 WARP 实例（当前生产环境 20 个）
- **20 个唯一 IPv6 出口**（每个账号独立分配，`2a09:bacx::/29` 段）
- **4 个 IPv4 出口**（由 Cloudflare 边缘节点 NAT 池决定，动态轮换）
- **多对端（Endpoint）优选**：5 个边缘接入点轮询分配，进一步扩展出口 IP 组合（账号数 × 端点数的组合）
- API 按 Round-Robin 顺序提取代理，连续调用返回不同出口 IP
- 支持独占的粘性会话（同一会话固定同一出口 IP）
- 健康检查、IP 唯一性检测、403 率统计、出口 IP 风控值检测（scamalytics）与自动换 IP
- Web 管理页面支持创建、重建、停止、删除、换 IP、改配置

---

## 系统用途

**一台服务器 → 多个不同出口 IP 的自动维护代理池**：利用 WARP"每注册账号分配独立 IPv6 出口"的特性，把单台 VPS 扩展为 1～200 个唯一出口 IP，供业务程序按需提取，避免为每个任务单独购买 IP。

### 适用场景

- **爬虫/采集**：轮换出口 IP，降低目标站对单 IP 频率的限制与封禁
- **多账号业务**：每次会话独立 IP；粘性会话可固定 IP（适合登录态任务）
- **IPv6 测试/业务**：每实例独立 IPv6 出口；IPv4 走 4 IP 共享 NAT 池
- **API 调用**：OpenAI 兼容协议（`api.openai.com`）、Telegram 等对数据中心 IP 友好的 API

### 不适用

- OpenAI **网页/注册**（chatgpt.com / auth.openai.com / platform.openai.com 被 Cloudflare 质询拦截 403，需住宅代理）
- 需要住宅 IP 的风控严格场景（部分电商、社媒注册）

### 使用流程

```text
GET /api/pool/acquire   → 返回 SOCKS5 端口 + 会话 ID（每次轮换出口 IP）
→ 业务程序走 SOCKS5 访问目标
POST /api/pool/release  → 归还实例（可用 /api/pool/report 上报真实 HTTP 码）
```

---

## 目录

- [系统用途](#系统用途)
- [架构](#架构)
  - [服务器端模块架构](#服务器端模块架构)
- [技术原理](#技术原理)
  - [WARP 是什么](#warp-是什么)
  - [WireGuard 隧道建立全过程](#wireguard-隧道建立全过程)
  - [三种接入账户形态对比](#三种接入账户形态对比)
  - [出口 IP 分配机制（核心）](#出口-ip-的分配机制核心)
  - [Docker 多容器方案实测](#docker-多容器方案实测官方-warp-cli)
  - [多对端（Endpoint）优选](#多对端endpoint优选与出口扩展)
  - [账号注册机制与注册 API 解析](#账号注册机制与注册-api-解析)
  - [sing-box 配置详解](#sing-box-配置详解)
  - [MASQUE 与 WireGuard 协议对比](#masque-与-wireguard-协议对比)
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
- [OpenAI 403 的替代方案](#openai-403-的替代方案)
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
deploy-v3.ps1              # 部署脚本（SFTP 上传 + 重启 systemd 服务，进程化方式）
Dockerfile                 # Docker 镜像定义（内置 sing-box / wgcf 二进制）
docker-compose.yml         # 容器编排（host 网络 + 挂载卷 + 自启，从 Docker Hub 拉镜像）
.github/workflows/         # GitHub Actions：push 到 main/master 自动构建并推送镜像（2.0 / 3.0 / latest）
test-ipv6-sites.sh         # IPv6 目标网站可达性测试脚本
test/                      # API / 代理池模拟测试
```

### 服务器端模块架构

`server.js` 单进程承载全部功能，按职责划分为以下模块：

```text
server.js（Node.js，端口 4433）
│
├── 配置管理      loadConfig / saveConfig / GET·POST /api/config（白名单字段）
├── 密钥生成      generateWireGuardKeypair（Node crypto 生成 X25519 密钥对）
├── WARP 账号注册  registerWarpAccount（registerViaAPI 优先 → wgcf 回退）
│                 registerViaAPI：直连 CF 注册 API，内置 429 退避重试
│                 registerViaWgcf：wgcf CLI register + generate
├── 端点优选      probeEndpoints / getEndpointForInstance / nextEndpointForInstance
├── 配置生成      generateSingboxConfig（WireGuard 置于 endpoints 数组）
├── 实例管理      createInstance / rebuildInstance / start/stop/delete
├── 进程管理      spawn sing-box run（detached）+ pid 记录 + SIGTERM 停止
├── 健康巡检      定时探测出口 IP、403 率统计、异常自动重建
├── 代理池        acquire / release（Round-Robin + 粘性会话）
├── API 路由      Express REST（/api/pool/*、/api/containers、/api/job/:id 等）
└── Web 管理页面   静态页面（状态总览、实例操作、配置修改）
```

设计要点：

- **两种部署方式**：进程化（每个实例是独立的 `sing-box run` 进程，用户态 WireGuard，内核无需 `wireguard` 模块，内存约 15MB/实例）或 **Docker 容器**（`node:20-slim` + host 网络，镜像内置 sing-box/wgcf 二进制，挂载实例目录，`restart: unless-stopped` 自启）。两者底层完全一致，Docker 化的价值在于：环境可移植（依赖打进镜像，新机器免装 Node/sing-box/wgcf）、服务自启免 systemd 配置、日志轮转统一管理。
- **进程隔离**：每个实例独立目录（`/root/warp-instances/warp-{idx}/`）、独立 SOCKS5 端口（`base_port + idx`）、独立 WARP 账号与密钥，互不影响。
- **可观测**：`meta.json` 记录 pid / ipv6 / endpoint；`registration.json` 记录注册响应原文；`singbox.log` 记录进程日志。
- **崩溃恢复**：服务启动时扫描实例目录自动重启所有进程；`auto_rebuild_on_unhealthy` 在健康巡检发现异常时自动重建。

---

## 技术原理

### WARP 是什么

Cloudflare WARP（对应客户端 App 即 1.1.1.1）是 Cloudflare 提供的免费 VPN 服务，底层使用 **WireGuard** 协议（近两年官方客户端逐步切换 **MASQUE/QUIC**）。它的本质是把你的流量加密后送入 Cloudflare 遍布全球的边缘节点（colo），由 Cloudflare 的网络代你访问目标网站——因此：

- **出口 IP 是 Cloudflare 的**（AS13335），不是你的服务器 IP
- **WARP 是"共享出口"架构**：出口 IP 由 Cloudflare 根据地区动态分配，大量用户共用
- **它是免费且无需付费的**：注册一个 Cloudflare 账号即可使用（本系统正是用程序批量注册 WARP 账号）

WARP 的接入形态分三种（详见 [三种接入账户形态对比](#三种接入账户形态对比)）：

| 形态 | 适用 | 流量配额 | 出口路由 |
|---|---|---|---|
| 免费 WARP | 个人 | 约 1GB 高速流量后限速 | 普通路由 |
| WARP+ | 付费订阅（license key） | 更高配额 | Argo 优化路由 |
| Zero Trust / Teams | 企业组织 | 无限 | 企业级骨干网 |

### WireGuard 隧道建立全过程

一个实例从注册到可用的完整链路（本系统 `createInstance` → `registerWarpAccount` → `startSingbox` 的实际流程）：

```text
[1] 生成密钥对
    本地 Node crypto 生成 X25519 密钥对（WireGuard 非对称加密基础）
        └─ 公钥 pubB64  → 用于注册
        └─ 私钥 privB64 → 本地保存，用于解密隧道流量

[2] 调用 Cloudflare 注册 API
    POST https://api.cloudflareclient.com/v0a2158/reg（携带公钥）
        └─ 返回：账号 ID、服务器端私钥、唯一内部地址
           v4 = 172.16.0.2            （WARP 内网固定地址）
           v6 = 2606:4700:110:xxxx    （WARP 内部段，每账号唯一）

[3] 生成 sing-box 配置
    WireGuard 隧道 interface 地址 = 上述内部地址（v4 + v6）
    peer 对端 = Cloudflare 边缘节点（endpoint:2408）+ 服务器端公钥
        └─ 写入 /root/warp-instances/warp-{idx}/singbox.json

[4] 启动隧道
    sing-box run（用户态 WireGuard，无需内核模块）
    与 endpoint 建立加密 UDP 连接（KeepAlive 30s 保活）

[5] 流量进入隧道
    业务程序 → SOCKS5 端口(10010+) → sing-box → WARP 隧道
        └─ 到达 Cloudflare 边缘节点后做"双栈 NAT"出公网：
           IPv4 出口 → 节点共享 NAT 池（4 个固定 IP，动态轮换）
           IPv6 出口 → 账号专属公网地址（2a09:bacx::/29 段）

[6] 出口验证
    系统通过 SOCKS5 端口探测目标站，得到真实出口地址
        └─ meta.json 记录 ip（公网出口地址，非内部地址）
```

**关键认知：隧道内部地址 ≠ 公网出口地址。** 注册返回的 `2606:4700:110:xxxx` 只是 WARP 的"内网"地址，公网出口由边缘节点做 NAT 转换而来。IPv6 场景下两者一一对应（每账号唯一），IPv4 场景下被折算进共享 NAT 池。

### 三种接入账户形态对比

| 维度 | 免费 WARP | WARP+ | Zero Trust / Teams |
|---|---|---|---|
| 获取方式 | 注册即得 | 订阅/邀请刷流量（license key） | 企业组织配置（team token） |
| 流量配额 | 约 1GB 高速后限速 | 更高 | **无限** |
| 出口路由 | 普通 | Argo 优化（更快更稳） | 企业级骨干网 |
| 出口 IPv4 | 节点 NAT 池 | 节点 NAT 池（实测同池） | **与免费段不重叠**（XTLS 分析） |
| 出口 IPv6 | `2a09:bacx::/29` 每账号唯一 | 同段每账号唯一 | 企业段（不同） |
| 注册方式 | 注册 API / wgcf | 注册 + license 字段 | 注册 + `team_token` 字段 |
| 本项目支持 | ✅ 默认 | ⚠️ 未内置（可自行扩展） | ✅ 可选（见 [WARP Teams](#warp-teams-无限流量账户可选)） |

> 政策提示：Cloudflare 正逐步收紧免费通道（2026 年起 wgcf/warp-go 的 referral 被拒、部分社区脚本已移除 WARP+ 与 Teams 支持），企业级接入（Zero Trust）仍是扩展出口段的可行方向，但需实测确认当前可用性。

### 出口 IP 的分配机制（核心）

```
你的服务器 (192.6.121.16)
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

#### IPv4 NAT 池与 IPv6 地址的分配原理

WARP 采用"**共享出口 + 双栈 NAT**"架构，隧道内部地址与公网出口地址是两回事（实测 warp-0）：

| 层 | IPv4 | IPv6 |
|---|---|---|
| 隧道内部地址（注册返回，写入 sing-box） | `172.16.0.2`（WARP 内网固定值） | `2606:4700:110:8a5a:c7d0:68d8:f798:258a`（WARP 内部段，**每账号唯一**） |
| 公网出口地址（curl 实测） | `104.28.195.186/187`、`104.28.227.186/187`（**节点共享 NAT 池**） | `2a09:bac1:76a0:3d0::48c:1`（**公网段，每账号唯一**） |

**IPv4 为什么只有 4 个（NAT 池原理）：**

1. 隧道流量到达边缘节点后，节点做 **SNAT（源地址转换）**，把内部地址 `172.16.0.x` 折算成节点的公网出口 IP。
2. 每个边缘节点（colo）只配置了少量公网 IPv4（本项目接入的 LAX 节点为 4 个），**几万用户共享这 4 个出口 IP**。
3. SNAT 按连接（会话）动态挑选出口 IP，因此同一个实例的连续请求可能在 4 个 IP 间轮换——**这就是"共享出口、动态轮换"的本质**。
4. 节点 NAT 池的数量由 Cloudflare 的出口规划决定，**与注册账号数、客户端类型、隧道协议完全无关**（已用 sing-box / wgcf / 官方 warp-cli 三种方式实测证实）。

**IPv6 为什么每账号唯一：**

1. 注册时 Cloudflare 为每个账号分配一个唯一的内部 IPv6（`2606:4700:110:xxxx/128`）。
2. 出公网时边缘节点将其映射到 WARP 专属公网段 `2a09:bacx::/29`，**一一对应、互不重复**。
3. 因此账号数 = 唯一 IPv6 数，这是本系统"20 账号 = 20 个唯一出口"的原理基础。
4. 注意该 IPv6 属于 Cloudflare 的地址段（数据中心 IP），并非服务器自有的公网 IPv6。

**向目标网站发出的流量路径（双栈）：**

```text
业务程序
   │ SOCKS5 (127.0.0.1:10010)
   ▼
sing-box (WireGuard 隧道, interface=172.16.0.2 + 2606:4700:110:xxxx)
   │ 加密 UDP (endpoint:2408)
   ▼
Cloudflare 边缘节点 (colo=LAX)
   │ 双栈 NAT
   ├── IPv4 → 节点 NAT 池（4 个共享 IP，SNAT 动态选择）
   └── IPv6 → 账号专属公网地址（2a09:bacx::/29）
   ▼
目标网站看到：IPv4 为 104.28.227.x 之一；IPv6 为每实例唯一地址
```

### Docker 多容器方案实测（官方 warp-cli）

为验证"换成官方 warp-cli + Docker 多容器方案能否获得更多 IPv4 出口"，2026-08-14 在服务器实测了 [dublok/cloudflare-warp](https://github.com/ErcinDedeoglu/cloudflare-warp)（2026-08 仍在活跃更新）的官方客户端多容器方案：

| 项目 | sing-box 多实例（本系统） | warp-cli 多容器（Docker） |
|---|---|---|
| 隧道协议 | WireGuard（UDP 2408） | **MASQUE/QUIC**（新版 warp-cli 默认，非 WireGuard） |
| 注册方式 | 直连 CF API / wgcf CLI | `warp-cli registration new`（每容器独立注册） |
| IPv6 出口 | 每账号唯一 `2a09:bacx::/29` | 每账号唯一 `2a09:bacx::/29`（同段） |
| IPv4 出口 | LAX NAT 池 4 个 | **同一 LAX NAT 池**（实测命中 `104.28.227.186/187`） |

**实测数据（3 个隔离容器、各自独立注册账号，容器内代理直测）：**

| 容器 | 连接状态 | IPv4 出口（ip-api.com） | IPv6 出口 |
|---|---|---|---|
| warp-t1 | Connected | `104.28.227.187` | `2a09:bac5:6369:2da5::48c:5` |
| warp-t2 | Connected | `104.28.227.186` | `2a09:bac5:636e:1173::1bd:a0` |
| warp-t3 | DNS 失败（容器内解析异常） | - | - |

**结论：**

- **Docker 多容器方案同样拿不到 20 个 IPv4**。即使走完全不同的隧道协议（MASQUE/QUIC）与官方客户端，IPv4 出口仍落在同一个 LAX NAT 池（`104.28.227.186/187`），与账号数、容器数、接入方式均无关——IPv4 出口数量由边缘节点（colo）的 NAT 池固定。
- 该方案唯一的能力（每容器一个唯一 IPv6）本系统已通过 sing-box 多实例实现（进程化约 15MB/实例，或 Docker 容器化部署，能力等价）。
- 该项目的额外价值在于支持 **Zero Trust（Teams）组织注册** 与 **WARP+ license key 轮询**（见 [WARP Teams](#warp-teams-无限流量账户可选) 与搜索结论），若想扩展 IPv4 需从此类企业级接入入手，且需实测确认出口段差异。

### 多对端（Endpoint）优选与出口扩展

WARP 对端（endpoint）就是 sing-box WireGuard 配置中 `peers[].address` 指向的 Cloudflare 边缘接入点。**同一个 WARP 账号连接不同的 endpoint，得到的出口 IP 不同**（IPv4、IPv6 均有差异），因此：

```
出口 IP 组合数 ≈ 账号数 × 可用 endpoint 数
```

系统内置 4 个实测可用的 endpoint：

| endpoint 地址 | 端口 | 说明 |
|---|---:|---|
| `188.114.96.1` | 2408 | 洛杉矶节点 |
| `188.114.97.1` | 2408 | 洛杉矶节点 |
| `188.114.96.2` | 2408 | 洛杉矶节点 |
| `188.114.96.3` | 2408 | 洛杉矶节点 |

**工作机制：**

1. **启动探针**：服务启动时 `probeEndpoints()` 对 4 个候选端点做 UDP 连通性探测，剔除不可用项，生成 `availableEndpoints` 列表。
2. **新建实例分配**：`getEndpointForInstance(idx) = availableEndpoints[idx % 列表长度]`。相邻实例分配不同 endpoint，出口 IP 更分散。20 实例 × 4 端点 = 每端点恰好 5 个实例。
3. **换 IP 时端点轮换**：`nextEndpointForInstance(idx) = availableEndpoints[(idx+1) % 长度]`。自动换 IP 时轮换到下一个 endpoint 再注册新账号，避免同一 endpoint 换 IP 后出口仍相同。
4. **元数据记录**：每个实例的 `meta.json` 记录 `endpoint` 字段，`/api/containers` 返回每实例的 endpoint 与 ipv6，端点分配可观测。

> 注意：部分 endpoint（如 `162.159.192.1`、`162.159.193.1`）虽能完成 UDP 握手，但实际无法承载流量（出口 IP 检测全部超时），已从候选列表剔除。若某个实例频繁出现"隧道已建立但无出口 IP"，大概率是该 endpoint 或该账号的瞬时故障——实例会走 RECOVERY 复用失败自动清理进程并重新注册（见下方部署章节），无需人工干预。

### 账号注册机制与注册 API 解析

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

#### 注册 API 深度解析

**请求**（registerViaAPI 实际发送的完整内容）：

```bash
curl -s --compressed --max-time 25 -X POST "https://api.cloudflareclient.com/v0a2158/reg" \
  -H "User-Agent: okhttp/3.12.1" \
  -H "Content-Type: application/json" \
  --data-binary @reg-body.json
```

```json
{
  "key": "<本地生成的 X25519 公钥，base64>",
  "tos": "2023-07-17T10:24:48.735Z",
  "type": "Android",
  "model": "warp-manager",
  "serial_number": "warp-0",
  "device_name": "warp-manager-0",
  "warp_enabled": true,
  "team_token": "<可选，Teams 组织 token>"
}
```

| 请求字段 | 含义 |
|---|---|
| `key` | 本地生成的 X25519 公钥（WireGuard 握手用），服务端用它绑定账号 |
| `tos` | 用户协议版本时间戳（固定值即可） |
| `type` / `model` | 模拟客户端类型。不同的 type/UA 实测不影响出口 IP 池 |
| `serial_number` / `device_name` | 设备标识，本项目用实例序号区分 |
| `team_token` | 置入后注册的账号即为 **Teams（Zero Trust）账号**，无限流量 |

**响应**（关键字段）：

```json
{
  "id": "892297e5-f0b1-414c-b1fb-83ecd41b664a",
  "account": { "account_type": "free", "warp_plus": true },
  "config": {
    "client_id": "xxxxxx",
    "interface": {
      "addresses": { "v4": "172.16.0.2", "v6": "2606:4700:110:8a5a:...:258a" },
      "private_key": "<服务端生成的私钥>"
    },
    "peers": [{ "public_key": "<服务端公钥>", "endpoint": { "host": "engage.cloudflareclient.com", "v4": ["162.159.192.1"], "v6": ["2606:4700:d0::a29f:c001"], "port": 2408 } }]
  }
}
```

| 响应字段 | 用途 |
|---|---|
| `id` | 账号 ID（可用于后续查询/管理账号） |
| `config.interface.addresses.v4` | WARP 内网 IPv4（固定 `172.16.0.2`） |
| `config.interface.addresses.v6` | WARP 内网 IPv6（**每账号唯一**，写入 sing-box interface） |
| `config.interface.private_key` | 服务端生成的隧道私钥（写入 sing-box） |
| `config.peers[0].public_key` | 服务端公钥（写入 sing-box peer） |
| `config.peers[0].endpoint` | Cloudflare 接入端点列表（本项目用自建 5 端点列表替代） |

> 本项目将注册响应保存到实例目录 `registration.json`，便于排查；实际隧道配置只用到内部地址、私钥与服务端公钥。

### sing-box 配置详解

每个实例的 `singbox.json` 由 `generateSingboxConfig()` 生成，结构如下（以 warp-0 实测为例，密钥已脱敏）：

```json
{
  "dns": {
    "servers": [
      { "tag": "remote", "type": "tls", "server": "dns.quad9.net", "domain_resolver": "local", "detour": "direct-out" },
      { "tag": "local", "type": "udp", "server": "119.29.29.29", "detour": "direct-out" }
    ],
    "final": "remote",
    "reverse_mapping": true,
    "strategy": "prefer_ipv6"
  },
  "route": {
    "default_domain_resolver": { "server": "local", "rewrite_ttl": 60 },
    "rules": [
      { "inbound": "mixed-in", "action": "sniff" },
      { "protocol": "dns", "action": "hijack-dns" },
      { "action": "resolve", "strategy": "prefer_ipv6" },
      { "ip_is_private": true, "outbound": "direct-out" }
    ],
    "auto_detect_interface": true,
    "final": "WARP"
  },
  "inbounds": [
    { "type": "mixed", "tag": "mixed-in", "listen": "0.0.0.0", "listen_port": 10010 }
  ],
  "endpoints": [
    {
      "tag": "WARP",
      "type": "wireguard",
      "address": ["172.16.0.2/32", "2606:4700:110:8a5a:...:258a/128"],
      "private_key": "<注册返回的账号私钥>",
      "peers": [{
        "address": "188.114.97.1",
        "port": 2408,
        "public_key": "<服务端公钥>",
        "allowed_ips": ["0.0.0.0/0", "::/0"],
        "persistent_keepalive_interval": 30
      }],
      "mtu": 1408,
      "udp_fragment": true
    }
  ],
  "outbounds": [
    { "tag": "direct-out", "type": "direct", "udp_fragment": true }
  ]
}
```

| 配置块 | 关键字段 | 含义 |
|---|---|---|
| `endpoints[0]` | `type: wireguard` | 隧道类型（sing-box 用户态 WireGuard，内核无需模块） |
| | `address` | 隧道内部地址：固定 `172.16.0.2/32` + 账号唯一 IPv6 `/128` |
| | `private_key` | 注册返回的账号私钥（隧道加解密） |
| `endpoints[0].peers[0]` | `address` / `port` | **endpoint**：Cloudflare 边缘接入点（`188.114.97.1:2408`），由端点优选分配 |
| | `public_key` | 服务端公钥（握手验证） |
| | `allowed_ips` | `0.0.0.0/0` + `::/0`：所有流量进隧道 |
| | `persistent_keepalive_interval` | 30s 保活心跳，防止 NAT 老化断链 |
| | `mtu` | 1408（WireGuard 隧道典型值） |
| | `udp_fragment` | 允许 UDP 分片（兼容部分网络） |
| `inbounds[0]` | `type: mixed` | 同时提供 SOCKS5 + HTTP 代理，监听 `base_port + idx` |
| `dns` | `strategy: prefer_ipv6` | **IPv6 优先**：目标站支持 IPv6 就走每实例唯一的 IPv6 出口 |
| `route` | `final: WARP` | 默认全部流量进 WARP 隧道 |
| | `ip_is_private → direct-out` | 内网地址不走隧道（直连，避免环路） |
| `outbounds[0]` | `direct-out` | DNS 解析与内网流量直连出口，缺失会导致 `outbound detour not found` 报错 |

> 调优：如业务需要**强制 IPv4 出口**（共享池），将 `dns.strategy` 与 `route` 中 `resolve` 的 `strategy` 改为 `ipv4_only`；需要更高 MTU 时注意部分运营商对 1400+ 分片敏感。

### MASQUE 与 WireGuard 协议对比

2026-08 通过官方 warp-cli（Docker 容器）实测确认：**新版 warp-cli 默认走 MASQUE/QUIC 隧道**（日志中出现 `MasqueTunnelStatsUpdated`、QUIC 统计），而非 WireGuard。两者对比：

| 维度 | WireGuard（本系统） | MASQUE/QUIC（官方 warp-cli） |
|---|---|---|
| 底层协议 | UDP（端口 2408） | QUIC（基于 UDP 443） |
| 实现 | sing-box 用户态（轻量进程） | warp-svc 官方守护进程（约 50-100MB/实例） |
| 多路复用 | 无（单隧道） | 有（QUIC 多流复用） |
| 运营商 QoS | 中国区对 UDP 2408 有感知 | QUIC 更隐蔽、对 UDP 443 更友好 |
| endpoint 选择 | 手动指定（5 端点列表） | 自动选择最优边缘节点 |
| **IPv4 出口** | LAX NAT 池 4 个 | **同一 NAT 池**（实测 `104.28.227.186/187`） |
| **IPv6 出口** | `2a09:bacx::/29` 每账号唯一 | **同段每账号唯一**（实测 `2a09:bac5:...`） |

**结论：协议不影响出口 IP 池。** 无论 WireGuard 还是 MASQUE，最终都由 Cloudflare 边缘节点统一做出口 NAT，IPv4 仍是那 4 个共享 IP、IPv6 仍是每账号唯一的 `2a09:bacx::/29` 段。本项目选 WireGuard 的原因：sing-box 原生支持、轻量（进程化约 15MB/实例，亦可 Docker 容器化部署）、可精确控制 endpoint 做轮换扩展。

### 为什么 WARP 出口都是"数据中心 IP"

- IPv4 出口属于 Cloudflare（AS13335），WHOIS 标记为 "Cloudflare, Inc."
- IPv6 出口属于 Cloudflare WARP 专用段 `2a09:bac1::/29`
- 部分网站（如 **OpenAI / Claude 网页端**）会拦截所有数据中心 IP，无论 WARP、VPS 原生 IP 还是其他机房 IP，网页均返回 HTTP 403（Cloudflare 质询 `cf-mitigated: challenge`）
- 实测：WARP 出口访问 `chatgpt.com`、`auth.openai.com`、`platform.openai.com`、`claude.ai` **均 403**；但 `api.openai.com` / `api.anthropic.com` / `api.x.ai` / `api.deepseek.com` / `openrouter.ai` 等 **API 接口全部可达**（401 仅缺 Key / 200）
- 因此本系统适用于大多数站点（Google、普通网站、爬虫轮换 IP、大模型 API 调用等），但**不适用于 OpenAI 网页聊天与账号注册**。注册需要住宅代理（真实家庭宽带 IP），见 [OpenAI 403 的替代方案](#openai-403-的替代方案)。

### 检测端点的坑（重要）

- `api.ipify.org`、`ifconfig.co`、`api-ipv4.ip.sb` 等端点本身**托管在 Cloudflare 后面**，WARP 流量访问它们时，Cloudflare 内部看到统一出口，会**误报所有实例 IPv4 相同**
- 必须使用**非 Cloudflare 托管**的端点（如 `ip-api.com`、`checkip.amazonaws.com`）才能看到真实的 4 个 IPv4 NAT 池
- 系统默认 `ip_check_url` 使用 `api.ipify.org`（IPv4）、`api64.ipify.org`（IPv6 优先），管理页面展示的是 IPv6 出口（每实例唯一）

---

## 环境要求（服务器端）

- Linux（Debian/Ubuntu 等），内核无需额外模块（用户态 WireGuard）
- Node.js 18+（进程化方式）或 **Docker + Compose**（容器化方式）
- 服务器需能访问 `engage.cloudflareclient.com`（UDP 2408 端口）
- 部署产物：
  - 进程化：`/usr/local/bin/sing-box` + `/usr/local/bin/wgcf` + `/root/warp-instances/warp-{idx}/`
  - Docker：镜像 `warp-pool:latest` 已内置 sing-box/wgcf，只需挂载 `/root/warp-instances` 卷

---

## 安装与启动（服务器端）

### 方式 A：Docker 容器（推荐，可移植）

**镜像已发布到 Docker Hub**（`zhoushu1/warp-pool`），由 GitHub Actions 在每次 push 到 `main`/`master` 时自动构建并推送 `2.0`、`3.0` 与 `latest` 三个标签（见 [.github/workflows/docker-publish.yml](.github/workflows/docker-publish.yml)），**本地不执行任何推送**。镜像内置 sing-box 与 wgcf 二进制，宿主机无需预装 Node/sing-box/wgcf。

**拉取镜像：**

```bash
docker pull zhoushu1/warp-pool:latest
```

#### 方案 1：Docker Compose（推荐）

服务器上只需一份 [docker-compose.yml](docker-compose.yml)：

```yaml
services:
  warp:
    image: zhoushu1/warp-pool:latest   # 每次部署前 docker compose pull 拉取最新
    container_name: warp
    network_mode: host        # 需要 host 网络：实例端口 10010-10029 + 服务端口 4433 直接暴露
    restart: unless-stopped   # 开机自启 / 崩溃自动拉起
    volumes:
      - /root/warp-instances:/root/warp-instances   # 实例持久化（重启不丢账号）
      - /root/warp/config.json:/app/config.json     # 配置覆盖
      - /root/warp/logs:/app/logs                   # 日志持久化
    logging: { driver: json-file, options: { max-size: 20m, max-file: "3" } }
```

```bash
# 服务器上执行（需已安装 docker + compose 插件）
docker compose pull          # 拉取 zhoushu1/warp-pool:latest
docker compose up -d         # 启动（实例目录已存在则直接恢复，无则自动创建）
docker compose ps            # 查看状态
docker logs -f warp          # 查看日志
```

#### 方案 2：docker run 单命令（免 compose）

```bash
docker run -d --name warp \
  --network host \
  --restart unless-stopped \
  -v /root/warp-instances:/root/warp-instances \
  -v /root/warp/config.json:/app/config.json \
  -v /root/warp/logs:/app/logs \
  zhoushu1/warp-pool:latest
```

> 必须使用 `--network host`：实例 SOCKS5 端口（10010 起）与 API 端口 4433 直接暴露，sing-box UDP 2408 出站无 NAT 干扰。

**发布新版本**：改代码 → `git push` 到 `main`/`master` → GitHub Actions 自动构建推送（`2.0` / `3.0` / `latest` 三标签）→ 服务器 `docker compose pull && docker compose up -d`（或 `docker pull && docker rm -f warp && docker run ...`）升级，全程不碰本地构建。

容器启动后 RECOVERY 机制自动扫描 `/root/warp-instances`：
- **复用已有配置启动**：实例目录已有 `singbox.json` 时直接复用（不重新注册账号），每个实例约 9 秒恢复（8 秒等待隧道建立 + 出口检测），秒级恢复 20/20。
- **复用失败自动降级**：出口 IP 检测失败（配置损坏 / endpoint 不可达）时自动清理 sing-box 进程、移出代理池，转为重新注册 WARP 账号，无需人工干预。
- 重建缺失实例、恢复已有实例入池，无需初始化脚本。

> ⚠️ 服务器若配置了 Docker registry mirror（如 `mirror.gcr.io`），`docker compose pull` 可能长期拉到缓存的旧镜像。若新版本不生效，先检查 `/etc/docker/daemon.json` 移除 `registry-mirrors` 再 `systemctl restart docker`。

> 首次使用前需在 GitHub 仓库 **Settings → Secrets and variables → Actions** 配置：
> - `DOCKER_HUB_USERNAME`：Docker Hub 用户名
> - `DOCKER_HUB_TOKEN`：Docker Hub Access Token（需在 Docker Hub → Account Settings → Security 创建，勾选 Read/Write/Delete）

### 方式 B：进程化（无 Docker）

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

## 管理页面（Web UI）

服务内置一个单页 Web 管理界面（`public/index.html`），浏览器打开即可可视化操作整个代理池：

```text
http://<服务器IP>:4433
```

| 区域 | 功能 |
|---|---|
| 顶部状态栏 | 服务/Docker 运行状态检测、手动刷新 |
| 统计卡片 | 容器数、总实例、可用、使用中、不健康、独立 IP 数、活跃会话数 |
| 配置栏 | 修改实例数、粘性会话时长、403 自动换 IP 开关与阈值（保存即生效） |
| API 使用面板 | 一键复制普通提取 / 粘性提取的完整 curl 命令 |
| 工具栏 | 创建/重建实例、删除选中/全部、重建选中、强制换 IP（全部）、代理池会话面板、搜索过滤 |
| 实例表格 | 每实例：名称/端口/池状态/出口 IP/CF 403 率/健康状态/会话/操作（换 IP、停止、删除、启动） |
| 任务进度 | 创建/重建/换 IP 等批量任务的实时进度条与日志 |

> 页面所有操作均调用服务端 API（见 [API 参考](#api-参考)），适合人工巡检与运维；业务程序接入请直接用 API（见 [使用说明](#使用说明快速开始)）。

---

## 使用说明（快速开始）

部署完成后，业务程序按 **提取 → 使用 → 归还** 三步接入：

### 1. 提取代理（每次调用轮换出口 IP）

```bash
curl -s http://127.0.0.1:4433/api/pool/acquire
```

返回：

```json
{
  "success": true,
  "data": {
    "session_id": null,
    "proxy": "socks5://127.0.0.1:10010",
    "ip": "2a09:bac1:76e0:3d0::48c:5f",
    "sticky": false
  }
}
```

`proxy` 字段即当前可用的 SOCKS5 代理地址（服务部署在其他机器时，替换为对应公网/内网地址）。

### 2. 通过 SOCKS5 访问目标

```bash
# 验证出口 IP
curl -s --socks5-hostname 127.0.0.1:10010 https://api64.ipify.org
# 走代理调用大模型 API
curl -s --socks5-hostname 127.0.0.1:10010 https://api.openai.com/v1/models
```

Python 示例：

```python
import requests
resp = requests.get(
    "https://api64.ipify.org",
    proxies={"http": "socks5h://127.0.0.1:10010",
             "https": "socks5h://127.0.0.1:10010"},
)
print(resp.text)  # 出口 IPv6（每实例唯一）
```

### 3. 粘性会话（固定出口 IP）

需要多次请求保持同一出口 IP 时，传自定义 `session_id`：

```bash
curl -s "http://127.0.0.1:4433/api/pool/acquire?session_id=account-001"
```

同一 `session_id` 重复调用返回同一代理（默认 10 分钟，超时自动释放）。用完主动归还：

```bash
curl -s -X POST http://127.0.0.1:4433/api/pool/release \
  -H "Content-Type: application/json" \
  -d '{"session_id": "account-001"}'
```

### 4. 上报业务方真实状态码（可选）

SOCKS5 层无法感知 HTTPS 真实响应码，业务拿到 403/429 等状态码后可上报，用于 403 统计与自动换 IP：

```bash
curl -s -X POST http://127.0.0.1:4433/api/pool/report \
  -H "Content-Type: application/json" \
  -d '{"session_id": "account-001", "result": 403}'
```

> 普通提取（非粘性）不占用实例，无需归还；粘性会话建议用完主动 `release`，避免占用到超时。

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

`GET /api/containers` 响应示例（每实例含 endpoint / ip / ipv6 / health / pool_status，可观测端点与出口分配）：

```json
{
  "success": true,
  "data": [
    {
      "name": "warp-0",
      "port": 10010,
      "ip": "2a09:bac1:76a0:3d0::48c:1",
      "ipv6": "2a09:bac1:76a0:3d0::48c:1",
      "endpoint": "188.114.97.1:2408",
      "health": "ok",
      "pool_status": "available"
    }
  ]
}
```

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

## 出口 IP 风控值检测（scamalytics）

借鉴 wgcf/warp-sh"刷 IP"脚本的**验收闭环**思路：换 IP 不能只验连通性和唯一性，还要验"这个 IP 干不干净"。系统内置出口 IP 风控值检测：

### 原理

- 数据源：[scamalytics.com/ip/{ip}](https://scamalytics.com/ip/)（Fraud Score 0~100，免费、无需 API Key）
- **必须携带完整浏览器 UA**：scamalytics 对无 UA / 短 UA 的 curl 直接返回 403（Cloudflare 反爬），带 Chrome UA 才返回 200 和评分
- 解析页面中的 `Fraud Score: N` 与 `"risk":"medium"`，按 IP 缓存（默认 6 小时）避免重复请求

### 两个接入点（风控值 ≥ 阈值 → 自动换 IP）

1. **健康巡检 L1.5**：L1 IP 连通 → L1.5 风控值检测 → L2 CF 可访问性。风控值 ≥ 阈值则跳过后续检测，直接触发换 IP。
2. **换 IP 验收（核心）**：`rotateInstanceIP` 注册完新账号、拿到新出口 IP 后，先查风控值再入池——**高风险 IP 且还有重试次数就继续换**，直到验收通过或耗尽 `max_rebuild_attempts`。与 warp-sh"刷到目标站放行为止"同构。

### 配置项

| 配置项 | 默认值 | 说明 |
|---|---:|---|
| `ip_risk_check` | true | 风控值检测开关 |
| `ip_risk_threshold` | 70 | 风控值 ≥ 该值判定高风险并触发换 IP（0~100） |
| `ip_risk_ttl` | 21600 | 风控值缓存有效期（秒），6 小时 |
| `ip_risk_check_url` | https://scamalytics.com/ip/ | 风控检测数据源前缀 |

### API 新增字段

`/api/containers`、`/api/pool/status`、`/api/pool/release` 返回实例新增：

```json
"ip_risk": 20,
"ip_risk_label": "中风险"
```

`ip_risk` 为 scamalytics Fraud Score；`ip_risk_label` 为低/中/高风险（≥70 为高风险）。

### 实测

2026-08-14 在换 IP 验收时对 WARP 出口 `2a09:bac1:76c0:3d0::48c:47` 实测：

```text
[RISK] 风控值=20 (medium) (261ms)
换IP成功 2a09:bac1:76a0:3d0::3ff:4f → 2a09:bac1:76c0:3d0::48c:47
```

WARP 出口风控值约 20（中风险），远低于阈值 70，不会误触发换 IP；该机制用于出口被拉黑（风控值飙高）时自动剔除。

> 局限：风控分只反映"IP 是否被风控库标记"，**无法解决 OpenAI 的 Cloudflare 质询拦截**（那是基于数据中心 IP 段的一刀切策略，与风控分无关）。

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
| `ip_risk_check` | true | 出口 IP 风控值检测开关（scamalytics） |
| `ip_risk_threshold` | 70 | 风控值 ≥ 该值触发自动换 IP（0~100） |
| `ip_risk_ttl` | 21600 | 风控值缓存有效期（秒），6 小时 |
| `ip_risk_check_url` | https://scamalytics.com/ip/ | 风控检测数据源前缀 |
| `session_cleanup_interval` | 30 | 过期会话清理间隔（秒） |
| `cleanup_on_exit` | false | 退出时是否清理实例 |
| `rate_limit_enabled` | true | 提取 API 限流开关 |
| `rate_limit_per_minute` | 60 | 提取 API 每分钟限流次数 |
| `log_level` | info | 日志级别 |
| `public_ip` | auto-detect | 公网 IP（自动检测） |
| `api_key` | 随机生成 | API 密钥（存储于 config.json） |
| `team_token` | 空 | WARP Teams (Zero Trust) 团队 Token（见上文） |

---

## 当前生产环境实测数据（192.6.121.16）

### 出口 IP 汇总

| 类型 | 数量 | 具体地址 |
|---|---|---|
| IPv6 出口 | 20 个（每实例唯一） | `2a09:bac1:76e0:3d0::48c:5f` 等 |
| IPv4 出口 | 4 个（NAT 池轮换） | `104.28.195.186` `104.28.195.187` `104.28.227.186` `104.28.227.187` |
| 服务器原生 IP | 1 个 | `192.6.121.16`（LAX 机房） |

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

### 实例配置实测（warp-0）

从服务器实例目录直接读取的真实配置与注册数据（内部地址 vs 出口地址的对应关系）：

| 项 | 值 | 来源 |
|---|---|---|
| 隧道内部 IPv4 | `172.16.0.2/32` | `singbox.json` endpoints[0].address |
| 隧道内部 IPv6 | `2606:4700:110:8a5a:c7d0:68d8:f798:258a/128` | 注册返回，写入 singbox.json |
| 注册账号 ID | `892297e5-f0b1-414c-b1fb-83ecd41b664a` | `registration.json` |
| 账号类型 | `free`（warp_plus 标记为 true） | `registration.json` |
| endpoint | `188.114.97.1:2408` | `singbox.json` peers[0] |
| MTU / KeepAlive | `1408` / `30s` | `singbox.json` |
| **公网出口 IPv6** | `2a09:bac1:76a0:3d0::48c:1` | `meta.json`（curl 实测） |
| 出口检测 | 通过实例 SOCKS5 端口 `10010` 探测 | `ip-api.com` / `api64.ipify.org` |

→ 可见注册分配的内部地址（`2606:4700:110:...`）与公网出口（`2a09:bac1:...`）**完全不同段**，证实了"隧道内地址 ≠ 公网出口地址"的双栈 NAT 原理。

### 目标网站可达性实测

| 目标 | 通过 WARP | 服务器原生 IP | 说明 |
|---|---|---:|---:|---|
| Google | 200 | 200 | ✅ 正常 |
| ip-api.com / checkip.amazonaws.com | 200 | 200 | ✅ 可获取真实 IPv4 |
| chatgpt.com | **403** | **403** | ❌ Cloudflare 质询拦截（`cf-mitigated: challenge`），数据中心 IP 被封 |
| auth.openai.com | **403** | **403** | ❌ 登录/注册页被拦截 |
| platform.openai.com | **403** | **403** | ❌ 注册/申请 API Key 被拦截 |
| api.openai.com/v1/models | **401** | **401** | ✅ 链路通，仅缺 API Key → **已注册 Key 的 API 调用不受影响** |
| sentinel.openai.com | 200 | 200 | ✅ SDK 可访问 |

### IPv6 出口网站可达性全量实测（2026-08-14）

通过实例端口（`10010`，IPv6 出口 `2a09:bac1:76c0:3d0::48c:62`）强制 `curl -6` 走 IPv6 出口，测试 56 个重要网站，结果：

| 结果 | 数量 | 说明 |
|---|---:|---|
| ✅ IPv6 直达 | **53** | 返回正常 HTTP 状态码 |
| ⚠️ 仅 IPv4 | 2 | `www.microsoft.com`、`www.ele.me`（无 AAAA 或 v6 链路失败） |
| ❌ 不可达 | 1 | `www.douban.com`（无 AAAA + 屏蔽数据中心 IP） |

**国际主流（全部 IPv6 可达）**：google.com、youtube.com、github.com、bing.com、x.com、facebook.com、instagram.com、tiktok.com、netflix.com、wikipedia.org、apple.com、cloudflare.com、stackoverflow.com、linkedin.com、ebay.com、paypal.com、spotify.com、twitch.tv、discord.com、telegram.org、whatsapp.com、notion.so、vercel.com、news.ycombinator.com 等均 200/301/302 放行；google 系（mail/drive/translate）全通；amazon.com 返回 503、reddit.com 返回 403（链路通，属反爬拦截）。

**OpenAI 系（IPv6 出口实测）**：

| 域名 | 状态 | 说明 |
|---|---|---|
| api.openai.com | **401** | ✅ 可达，仅缺 API Key，已注册 Key 的调用正常 |
| sentinel.openai.com | 200 | ✅ SDK 可访问 |
| chatgpt.com / openai.com | **403** | ❌ Cloudflare 质询（`cf-mitigated: challenge`） |
| auth.openai.com / platform.openai.com | **403** | ❌ 登录/注册、API Key 申请被拦截 |
| claude.ai | **403** | ❌ Claude 网页同样被质询拦截 |

**其他大模型 API（IPv6 出口实测，均可走本代理池）**：

| 接口 | 状态 | 说明 |
|---|---|---|
| api.x.ai（Grok） | 401 | ✅ 可达，仅缺 Key |
| api.anthropic.com | 401 | ✅ 可达，仅缺 Key |
| api.deepseek.com | 401 | ✅ 可达，仅缺 Key |
| openrouter.ai | 200 | ✅ 聚合 GPT-4/Claude/Grok/DeepSeek，一个 Key 走全部 |

→ **OpenAI 账号/API Key 注册无法走 IPv6（数据中心 IP 被一刀切质询）**；但**已注册的 API Key 走 OpenAI 兼容协议（SDK、one-api/new-api 等中继）完全可用**，且 Grok / Anthropic / DeepSeek / OpenRouter 等大模型 API 均可直接走本代理池。网页聊天和注册需住宅代理，替代方案见下节。

**国内主流（绝大多数 IPv6 直达）**：baidu、taobao、tmall、jd、douyin、weibo、zhihu、bilibili、qq、sina、xiaohongshu、meituan、pinduoduo、aliyun、tencent、iqiyi、youku 等全部可达；163.com / sohu.com 返回 403（链路通，防护拦截）；`www.douban.com` 完全不可达；`www.ele.me` 仅 IPv4 可达。

复用测试脚本：`bash test-ipv6-sites.sh [端口]`（默认自动探测 10010～10029）

---

## OpenAI 403 的替代方案

`chatgpt.com` / `auth.openai.com` / `claude.ai` 的 403 是 **Cloudflare 质询拦截（`cf-mitigated: challenge`）**，基于数据中心 IP 段的一刀切策略，WARP / 原生 VPS / 任何机房 IP 都无法靠配置解决。替代路径如下：

| 方案 | 适用场景 | 成本/稳定性 |
|---|---|---|
| **① API 替代网页（推荐）** | GPT 项目只要"模型能力"，不需要网页 | 0 成本，代理池直接支持。`api.openai.com` / `api.x.ai` / `api.anthropic.com` / `api.deepseek.com` / `openrouter.ai` 实测全通，改 `base_url` 即可 |
| **② 住宅出口隧道** | 注册 / 网页场景 | 零成本（家里/朋友家常开设备做 Tailscale/WireGuard exit node），自建住宅 IP |
| **③ 原生 IP VPS** | 网页可用，比住宅代理便宜 | 部分未标记机房段可行，需真实浏览器实测；会被逐步标记，需定期换 |
| **④ Cloudflare Workers 反代** | 网页可用（大量 GPT 镜像的做法） | 免费但需维护转发代码，稳定性一般 |
| **⑤ Azure OpenAI** | 愿意换平台 | 微软承载的 GPT 走 Azure 数据中心 IP（AS8075），不受 OpenAI 质询影响 |
| **⑥ 自动解 JS 质询（cf_clearance）** | 仅应急取 accessToken | 见下方详解，**不推荐长期使用** |

### 方案 ⑥ 详解：自动解 JS 质询为什么不能注册

**原理**：curl 拿 403 是因为不执行质询 JS；无头浏览器/`curl_cffi` 执行 JS 后拿到 `cf_clearance` cookie（绑定 IP + UA + TLS 指纹），带 cookie 请求即绕过 Cloudflare。**这层确实能过。**

**但注册 OpenAI 要过三层关卡，解质询只解决第一层**：

| 关卡 | 内容 | 能否自动过 |
|---|---|---|
| 第一层 | Cloudflare JS 质询（`auth.openai.com` 的 403） | ✅ cf_clearance 可解 |
| 第二层 | Turnstile 人机验证 | ❌ 需真人或打码服务（capsolver 等） |
| 第三层 | **OpenAI 自身风控**：数据中心 IP 段识别 + 强制手机验证 | ❌❌ 决定性拦截 |

**第三层为什么必死**：WARP 整个 IPv6 段 `2a09:bac1::/29` 属 AS13335 数据中心，OpenAI 风控看**前缀信誉**（IPQualityScore 等），不只单个 IP；数据中心 IP 注册必触发手机验证，且**拒绝 VoIP/虚拟号**，接码平台的号大多被拒。2023-2024 年"WARP 注册 ChatGPT"教程有效是因为当时风控不严，2026 年已失效。

**cf_clearance 方案的其他硬伤**：
- cookie 30 分钟 ~ 24 小时过期，`__cf_bm` 约 30 分钟，需后台任务周期重解
- cookie 与 IP 强绑定，**WARP 换出口即作废**——与本系统主动轮换 IP 的特性天然冲突
- 高并发触发挑战风暴，OpenAI 检测后升级为 Turnstile 甚至封 IP 段
- 违反 OpenAI ToS，账号有连坐封禁风险

**结论**：本系统定位是 **API 调用代理池**（全部大模型 API 实测可用）；OpenAI 网页/注册请走方案 ①②③（住宅 IP 是唯一可靠路径），不要在解质询上投入。

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
| OpenAI 网页/注册全部 403 | 数据中心 IP 被 OpenAI/Claude 的 Cloudflare 质询拦截（`cf-mitigated: challenge`），WARP 出口属 AS13335 数据中心段 | **注册/网页无法解决**，自动解 JS 质询也不行（第三层 OpenAI 风控 + 强制手机验证会拦截，见 [OpenAI 403 的替代方案](#openai-403-的替代方案)）；需住宅代理。但已注册 Key 的 API 调用（`api.openai.com` 401 仅缺 Key）及 Grok/Anthropic/DeepSeek/OpenRouter 等 API 全部可用，见 [目标网站可达性实测](#目标网站可达性实测) |
| `sing-box: outbound detour not found` | 配置缺少 `direct-out` outbound | 使用系统 `generateSingboxConfig` 生成的完整配置 |
| 为什么 IPv4 只有 4 个？ | 出口由 LAX 边缘节点共享 NAT 池决定，与账号/容器/协议均无关 | 想增加 IPv4 需增加不同地区服务器（每台贡献 4 个），见 [多服务器扩展](#多服务器扩展增加-ipv4-出口) |
| 为什么注册返回 `2606:4700:110:...` 而出口是 `2a09:bacx::/29`？ | 前者是隧道内部地址，后者是边缘节点 NAT 后的公网出口地址 | 正常现象，两者一一对应；管理页面展示的是出口地址 |
| 换成 Docker 多容器（官方 warp-cli）能拿到更多 IPv4 吗？ | 不能。已实测 MASQUE 协议出口仍在同一 NAT 池 | 见 [Docker 多容器方案实测](#docker-多容器方案实测官方-warp-cli) |
| WARP+ license 与免费版出口有区别吗？ | 路由不同（Argo 优化）但 IPv4 仍在节点 NAT 池 | 一个 license 可绑定约 5 台设备；本项目未内置 license 支持 |
| 单实例流量被限速怎么办？ | 免费 WARP 约 1GB 高速流量后限速 | 配置 Teams `team_token` 注册无限流量账号，见 [WARP Teams](#warp-teams-无限流量账户可选) |
| 为什么有的实例同时有 IPv4 和 IPv6 出口，有的只有 IPv6？ | 取决于目标站是否支持 IPv6（`prefer_ipv6` 优先走唯一 IPv6） | IPv6 出口每实例唯一；IPv4 出口为共享池，两者可同时存在 |

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

---

## 安全提示

当前 API 未启用鉴权（`apiKeyAuth` 中间件已预留但默认放行），服务默认监听 `0.0.0.0`。暴露到公网时，应通过防火墙仅放行管理端口，或在反向代理层增加访问控制（API Key），避免任何人创建实例、提取代理或修改配置。SOCKS5 代理端口（10010 起）也应限制访问来源，防止被滥用为公共代理。
