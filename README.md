# WarpGate — Cloudflare WARP 代理池（Rust）

基于 **Rust + Cloudflare WARP MASQUE** 的多出口 IP SOCKS5 代理池。单台 Linux 服务器跑上千个相互隔离的 WARP 实例，每个实例独立注册 Cloudflare 账号，拥有**独立的 IPv6 出口 IP**，共享边缘节点的 **IPv4 NAT 池**。

- **Rust 原生数据面**：`crates/wg-tunnel` 直接用 `rustls` + 手写 HTTP/2 + CONNECT-IP + `smoltcp` 实现隧道，不需要 Node、sing-box 或任何外部代理进程
- 单机支持 1～1000+ 个 WARP 实例（生产实测 500 实例，压测 200 实例 24 分钟零失败）
- **每实例一个唯一 IPv6 出口**（`2a09:bacx::/29` 段）
- **4 个 IPv4 出口**（由 Cloudflare 边缘节点 NAT 池决定，动态轮换）
- API 按 Round-Robin 提取代理，连续调用返回不同出口
- 粘性会话（`sid` + `time` 指定分钟数，到期自动回收）
- 健康检查、IP 唯一性检测、CF 403 率统计、出口 IP 风控值检测
- Web 管理台：创建/重建/停止/删除/换 IP/改配置

两种数据面，用 `WARP_NATIVE` 切换：

| `WARP_NATIVE` | 实现 | 每实例内存 | 适用 |
|---|---|---|---|
| `quic`（默认） | `usque` 子进程，MASQUE over QUIC/UDP 443 | 约 10 MB | UDP 443 可达的机房；当前生产 |
| `h2` | **Rust 原生** CONNECT-IP over TCP 443 | 约 0.17 MB | UDP 被封的机房；**省 98% 内存** |

> `h2` 路径在 200 实例、24 分钟压测中扛住 779 次 Cloudflare stream 重置，全部自动重建，零失败。切过去前请读 [原生数据面的坑](#原生数据面的坑)。

---

## 更新日志

### v0.11.0（2026-11-05）

- **原生 CONNECT-IP 通过 200 实例压测**：24 分钟、779 次 stream 重置、778 次自动重建、探测成功率 100%、RSS 从 8.3 MB 涨到 24.6 MB 后收敛。结论：可以上生产。
- 修复 `ResilientTunnel` 的重连从未生效（v0.10.2）：`StackShared.dead` 和 `ResilientTunnel.dead` 是两个互不相通的标志，reader 任务标了死但 `ensure()` 看不见。现在 `ensure()` 会查底层 `is_dead()`，并新增 20 秒一次的空闲健康巡检。

### v0.10.2（2026-11-05）

- **修 ResilientTunnel 重连失效**：200 实例跑 10 分钟，Cloudflare 重置 197 个 stream，`rebuilds` 恒为 0，扫 20 个端口只有 1 个能通。根因见上。

### v0.10.1（2026-11-05）

- **修 `/api/pool/acquire` 的 `minutes` 参数被静默忽略**：请求 10 分钟实际按配置的 3 分钟过期，IP 被提前回收。`AcquireQuery.time` 字段和文档都在，但 `acquire()` 签名里没有这个参数，只是把它回显给客户端。

### v0.10.0（2026-11-05）

- **修孤儿引擎进程导致整池失效**：systemd 只停主进程，`usque` 带 `--always-reconnect` 会继续重连。重启后 500 个孤儿累积了 24,543 个 TCP 连接到 Cloudflare，配额耗尽，500 个端口全在监听但 20 个抽样只有 1 个能通。现在启动时扫描 `/proc`，SIGKILL 掉 `PPid==1` 的引擎进程。

### v0.9.0（2026-11-05）

- 原生 CONNECT-IP 数据面打通。根因是 Cloudflare 边缘**不实现 RFC 9484 Extended CONNECT**：发 `:protocol: connect-ip` 会被 `RST_STREAM(PROTOCOL_ERROR)`，rustls 把它报成 `CertificateUnknown`，误导了很久。正确形状是 plain CONNECT + `cf-connect-proto: cf-connect-ip`，`:authority` 必须带端口。
- 顺带修掉 7 个 bug：双 preface、`diagnose()` 用随机密钥、smoltcp 从不 poll、capsule 用 HPACK 整数、ABBA 死锁、socket 泄漏、poll 忙等空转（14% 概率永久卡死 + CPU 193%）。

### v11.1 及更早（Node/sing-box 版本）

<details>
<summary>历史更新日志（点击展开）</summary>

### v11.1（2026-10-31）

- 扩容到 150 实例；`sticky_minutes` 默认 3。
- 配套 chat2api 27.5 egress 修复。

### v11.0（2026-10-30）

- 修复 `health=unknown` 实例永久不可用：`pickProxy()` 只挑 `healthy`，实测 100 个实例中 29 个卡在 `unknown`，池容量缩水 30%。改为仅排除 `unhealthy`。
- `sticky_minutes` 10 → 60；`min_available` 5 → 20。
- 新增 `WARPGATE_VERSION` 常量。

### v10.0（2026-10-02）

- 引擎僵死自愈：usque 隧道约每 5 分钟被掐断，断线窗口内引擎进入僵死态（TCP 能连但数据不通）。新增 `restartInstanceEngine()`，用原账号配置重启，出口 IP 不变。
- `/api/pool/release` 和 `/api/pool/report` 支持 `failure_type`，连续 2 次失败触发引擎重启。

### v8.0（2026-10-01）

- MASQUE 端点自愈：默认端点 `162.159.198.2` 被封锁导致大批实例 unhealthy。新增 `masque_endpoint_candidates` 候选列表，用 usque 本体做金丝雀试连，真实取回出口 IP 才采用。
- 修复防火墙放行范围遗漏：放行只到 `10100` 而实例用到 `10109`，末尾 9 个端口「假健康」。

### v6.0（2026-09-10）

- 新增 Go 版本实现（`warpgate-go/`），作为替换 Node 版的技术储备。

### v5.0（2026-09-10）

- 日志治理：引擎日志降为 `warn`，超 10MB 自动截断。

</details>

---

## 快速部署（Docker）

镜像由 GitHub Actions 在 push 到 `main`/`master` 时自动构建推送，本地无需构建。

```bash
# 拉取并启动
docker compose pull
docker compose up -d

# 查看状态
docker compose ps
docker logs -f warpgate
```

管理台：`http://<服务器IP>:4433`

### 手动 docker run

```bash
docker run -d --name warpgate \
  --network host \
  --restart unless-stopped \
  --ulimit nofile=200000:200000 \
  --log-opt max-size=20m --log-opt max-file=3 \
  -e WARP_NATIVE=quic \
  -v /root/warpgate-rs/instances:/root/warpgate-rs/instances \
  -v /root/warpgate-rs/rust-config.json:/app/config.json \
  zhoushu1/warp-pool:latest
```

### 镜像标签

| 标签 | 说明 |
|---|---|
| `latest` | 最新构建 |
| `0.11.0` | 版本号（从 `warpgate-rs/Cargo.toml` 读取） |
| `11.1` | 兼容旧部署脚本的固定标签，现在指向 Rust 版 |

### 开放端口（防火墙/安全组）

端口段随实例数伸缩：实例 N 对应账密端口 `base_port ~ base_port+N-1`，白名单端口 `+1000`。

| 端口 | 用途 | 是否必须外网 |
|---|---|---|
| `4433` | 管理台 / API / 文档 | 必须 |
| `10010 ~ 10010+N-1` | 账密 SOCKS5（`base_port + 索引`） | 账密直连则开 |
| `11010 ~ 11010+N-1` | 白名单免密 SOCKS5 | 白名单直连则开 |

> **必须放行完整端口段。** 少放会导致末尾实例「服务器内 `ss -ltn` 正常、API 报 healthy，但外部连接超时」的假健康——本机自测走回环不受防火墙影响。

<details>
<summary>firewalld 示例（1000 实例）</summary>

```bash
firewall-cmd --permanent --add-port=10010-11009/tcp   # 账密段
firewall-cmd --permanent --add-port=11010-12009/tcp   # 白名单段
firewall-cmd --reload
firewall-cmd --list-ports
```

</details>

入站**无需**开放 UDP 2408（隧道是出站方向）。若云厂商限制出站，需放行 TCP/UDP 443 到 Cloudflare。

---

## 目录

- [更新日志](#更新日志)
- [快速部署（Docker）](#快速部署docker)
- [系统用途](#系统用途)
- [商用化接入](#商用化接入)
- [架构](#架构)
- [原生数据面的坑](#原生数据面的坑)
- [技术原理](#技术原理)
- [环境要求](#环境要求服务器端)
- [安装与启动](#安装与启动服务器端)
- [管理页面](#管理页面web-ui)
- [使用说明](#使用说明快速开始)
- [API 参考](#api-参考)
- [主要配置](#主要配置)
- [推荐实例数](#推荐实例数按服务器配置)
- [实测数据](#实测数据)
- [故障排查](#故障排查faq)
- [测试](#测试)

---

## 系统用途

**一台服务器 → 上千个不同出口 IP 的自动维护代理池。** 利用 WARP「每注册账号分配独立 IPv6 出口」的特性扩展单台 VPS。

### 适用场景

- **爬虫/采集**：轮换出口 IP，降低目标站对单 IP 的频率限制
- **多账号业务**：每次会话独立 IP；粘性会话可固定 IP
- **IPv6 测试/业务**：每实例独立 IPv6 出口
- **API 调用**：OpenAI 兼容协议（`api.openai.com`）、Telegram 等对数据中心 IP 友好的 API

### 不适用

- OpenAI **网页/注册**（`chatgpt.com` / `auth.openai.com` / `platform.openai.com` 被 Cloudflare 质询 403，需住宅代理）
- 需要住宅 IP 的严格风控场景

### 使用流程

```text
GET /api/pool/acquire   → SOCKS5 端口 + 会话 ID（每次轮换出口）
  → 业务程序走 SOCKS5 访问目标
POST /api/pool/release  → 归还实例
```

---

## 商用化接入

对齐商用代理平台的接入方式：**无 API Key**，通过 **IP 白名单** 鉴权；代理端口支持 **账密 / 白名单免密** 两种模式。

### 接入模式

| 模式 | 对象 | 用法 |
|---|---|---|
| **白名单模式** | 白名单内来源 IP | ① 调 API 免凭据；② 免密直连白名单端口（实例端口+1000） |
| **账密模式** | 任何 IP | 主端口用 `socks5://user:pass@ip:port`，每实例独立账密 |

- 白名单外 IP 调 API 返回 `401 A0001`（响应带 `client_ip` 和添加命令提示）；`127.0.0.1` 始终放行。
- 白名单/账密变更后热更新全部实例配置，复用账号不重新注册，出口 IP 不变。

### 提取 API

```bash
# 批量提取：num 数量 / type=txt|json / format=n|rn / sid 粘性会话 / time 分钟 / ip_mode=ip6|ip4
curl -s "http://<ip>:4433/api/proxies?num=5&type=txt&format=n&ip_mode=ip6"

# 粘性提取（同 sid 固定出口，time 指定分钟数）
curl -s "http://<ip>:4433/api/proxies?num=1&type=json&sid=account-001&time=30"

# 添加白名单
curl -X POST http://127.0.0.1:4433/api/whitelist \
  -H "Content-Type: application/json" -d '{"add":"1.2.3.4"}'

# 用量统计
curl -s "http://<ip>:4433/api/usage?days=30"
```

> `time` 参数在 v0.10.1 之前是失效的（只回显不生效），现在会真正覆盖 `sticky_minutes`。

### 长效静态 IP（固定绑定）

`POST /api/pool/dedicated/bind` 把实例**独占绑定**：锁定 IPv6 出口，不参与轮换、豁免所有自动换 IP / 自动重建，直到 `unbind`。状态持久化在 `meta.json`，重启自动恢复。绑定实例默认拒绝重建/删除（`D0001`，需 `force=true`）。

> 固定的是每实例唯一的 **IPv6** 出口；IPv4 仍是共享 NAT 池动态轮换。

### 统一响应与错误码

```json
{ "code": "00000", "msg": "", "data": {} }
```

`00000` 成功；`A0001` 不在白名单(401)；`A0002` 限流(429)；`P0001` 无可用代理；`S0001` 会话不存在；`I0001/I0002` 参数/资源错误；`D0001` 固定绑定保护；`R0001` 实例操作失败；`99999` 系统异常。

### 控制台页面

| 页面 | 路径 | 内容 |
|---|---|---|
| 管理台 | `/` | 数据概览（统计卡+用量图表）/ 流量代理（API 提取生成器）/ 长效静态 IP / 实例管理 |
| 白名单管理 | `/whitelist` | 增删/批量删除/一键添加我的 IP |
| API 文档 | `/docs` | 完整接口文档 |

---

## 架构

Cargo workspace，8 个 crate：

```text
warpgate-rs/
├── crates/
│   ├── wg-core/       共享类型、错误
│   ├── wg-config/     配置加载与校验
│   ├── wg-store/      实例目录读写（usque.json / meta.json）
│   ├── wg-socks/      SOCKS5 服务端（账密 + 白名单两种鉴权）
│   ├── wg-tunnel/     隧道层
│   │   ├── masque.rs      TLS 配置、证书 pinning
│   │   ├── connectip/     原生 CONNECT-IP
│   │   │   ├── h2.rs        手写 HTTP/2 客户端
│   │   │   ├── stack.rs     smoltcp 用户态 TCP 栈
│   │   │   ├── native.rs    TLS + h2 + CONNECT-IP 组装
│   │   │   └── resilient.rs 自动重连包装
│   │   └── register.rs    Cloudflare 账号注册
│   ├── wg-pool/       实例池管理（注册、启停、轮换、探测、会话）
│   ├── wg-apiserver/  HTTP API + WebUI 静态文件
│   └── wg-bin/        主程序与运维工具
└── public/            管理台前端
```

### 数据面

**`WARP_NATIVE=quic`（默认）** — 每个实例跑一个 `usque socks` 子进程：

```text
业务 → SOCKS5(10010+i) → wg-socks → usque 子进程 → QUIC/UDP 443 → Cloudflare
```

**`WARP_NATIVE=h2`** — 纯 Rust，无子进程：

```text
业务 → SOCKS5(10010+i) → wg-socks → wg-tunnel
                                       ├── TLS (rustls, h2 ALPN)
                                       ├── HTTP/2 CONNECT (手写)
                                       ├── CONNECT-IP + capsule
                                       └── smoltcp 用户态 TCP 栈
                                          ↓
                                    TCP 443 → Cloudflare
```

---

## 原生数据面的坑

`h2` 路径能省 98% 内存，但有几个必须知道的事实。

### Cloudflare 会重置 stream

单 IP 上隧道数超过约 100 个，Cloudflare 开始 `RST_STREAM`，日志是：

```text
connect-ip recv_packet failed; reader task exits
error = server reset stream 1 with error code 0x0
```

这不是 bug，是服务端限流。200 实例压测里 24 分钟被重置 779 次。

**缓解**：`ResilientTunnel` 会自动重建。reader 任务标记 death → 下次 dial 或 20 秒健康巡检触发重建 → 新隧道接上。实测重建与重置 1:1 追平，用户侧无感。

### 必须检查的两个标志

`ResilientTunnel` 有一个自己的 `dead` 标志，`StackShared` 也有一个。**两个必须连通**：`ensure()` 拿到 tunnel 后要调 `t.is_dead()` 检查底层状态。历史教训：只改定义不接调用（v0.10.2 之前），197 次重置产生 0 次重建。

`crates/wg-tunnel/src/connectip/resilient.rs` 里的 `ensure_consults_the_stack_dead_flag` 测试锁住了这条契约。

### `poll_read` / `poll_write` 不能自唤醒

```rust
// 错：立即自我唤醒，配合 2ms pump 变成忙循环，CPU 193%
cx.waker().wake_by_ref();

// 对：存 waker，由 pump 每轮 poll 后统一唤醒
this.shared.register(cx.waker());
```

`shared_helpers_are_wired` 测试锁住了 `wake_all()` / `queue_removal()` 的三个调用点。

### `connect()` 不能用 `is_active()` 判握手完成

smoltcp 0.11 的 `is_active()` 定义为 `!matches!(state, Closed|TimeWait|Listen)`，**`SynSent` 就是 true**。用它等握手会在第一轮就返回，后续 write 全丢。

必须用 `sock.state() == tcp::State::Established`（见 `handshake_done()`）。同理 `poll_read` 里 `!may_recv()` 不能当 EOF——`SynSent`/`SynRecv` 时它天然为 false。

### 孤儿进程会耗尽配额

`usque` 带 `--always-reconnect`，systemd 停主进程时它不会跟着退。上一轮的 500 个孤儿累积过 24,543 个 TCP 连接，把 Cloudflare 配额耗尽，整池「假健康」。

启动时会扫 `/proc` 清理 `PPid==1` 的引擎进程（`reap_orphan_engines`）。只碰孤儿，属于活着的 warpgate 的引擎不动。

---

## 技术原理

<details>
<summary>WARP 是什么 / 出口 IP 分配机制 / 账号注册（点击展开）</summary>

### WARP 是什么

Cloudflare WARP（1.1.1.1）是免费 VPN，底层 WireGuard 或 MASQUE/QUIC。本质是把流量加密送进 Cloudflare 边缘节点，由 Cloudflare 代你访问目标——所以**出口 IP 是 Cloudflare 的**（AS13335）。

### 出口 IP 分配机制

```text
你的服务器
   │ 就近接入
   ▼
Cloudflare 边缘节点 (colo=LAX)
   ├── IPv6 出口：每账号一个唯一地址（2a09:bacx::/29）
   └── IPv4 出口：边缘节点共享 NAT 池（4 个 IP 动态轮换）
```

**关键结论：**

1. **IPv6 数量 = 账号数量**。每次注册生成唯一 IPv6，这是扩展出口 IP 的基础。
2. **IPv4 数量由边缘节点决定，与账号数无关**。同台服务器永远连最近节点，NAT 池固定。
3. **想增加 IPv4 出口**，只能增加不同地区/机房的服务器。
4. **隧道内部地址 ≠ 公网出口**。注册返回的 `2606:4700:110:xxxx` 是内网地址，公网出口由边缘节点 NAT 转换。

### 账号注册

`POST https://api.cloudflareclient.com/v0a2158/reg`，返回账号 ID、私钥、唯一 IPv6 内部地址。

**限流**：6 并发注册几秒内就 429 `error code: 1015`。串行 + 2 秒间隔可以无限跑；触发限流后需等 30 分钟到几小时。

### 检测端点的坑

`api.ipify.org`、`ifconfig.co` 等本身托管在 Cloudflare 后面，WARP 流量访问时会**误报所有实例 IPv4 相同**。要用非 Cloudflare 托管的端点（`ip-api.com`、`checkip.amazonaws.com`）。

</details>

---

## 环境要求（服务器端）

- Linux x86_64
- **Docker 部署**：只需 Docker + Compose
- **systemd 部署**：Rust 1.83+（或直接用预编译二进制）、`usque` 二进制（`WARP_NATIVE=quic` 时需要）
- 出站需能访问 Cloudflare 的 TCP 443（`h2` 路径）或 UDP 443（`quic` 路径）
- `ulimit -n` 建议 ≥ 200000（1000 实例要开 2000+ 监听 socket）

---

## 安装与启动（服务器端）

### 方式 A：Docker（推荐）

见 [快速部署](#快速部署docker)。

### 方式 B：systemd

```ini
[Unit]
Description=WarpGate Rust proxy pool
After=network.target

[Service]
Type=simple
Environment=WARP_NATIVE=quic
Environment=WARP_FORCE_ENGINE=1
ExecStart=/root/warpgate-rs/app/target/release/warpgate \
  --config /root/warpgate-rs/rust-config.json \
  --instances /root/warpgate-rs/instances
Restart=always
RestartSec=5
LimitNOFILE=65535

[Install]
WantedBy=multi-user.target
```

```bash
systemctl daemon-reload
systemctl enable --now warpgate
systemctl status warpgate
```

> `WARP_NATIVE=quic` 和 `WARP_FORCE_ENGINE=1` 同时设是为了显式走 usque 子进程。只设 `WARP_NATIVE=quic` 在 v0.9.0 有过一次事故：代码把它当成「用原生 QUIC」，但这台机器 UDP 443 被封，500 实例全部握手超时。

### 从源码构建

```bash
cd warpgate-rs
cargo build --release
# 产物：target/release/warpgate
```

---

## 管理页面（Web UI）

浏览器打开 `http://<服务器IP>:4433`。

| 区域 | 功能 |
|---|---|
| 顶部状态栏 | 服务状态、手动刷新 |
| 统计卡片 | 实例数、可用、使用中、不健康、独立 IP 数、活跃会话 |
| 系统监控 | CPU / 内存 / 网络速率 / 负载 |
| 配置栏 | 实例数、粘性时长、403 自动换 IP 开关与阈值 |
| API 面板 | 一键复制提取命令 |
| 工具栏 | 创建/重建/删除/换 IP、搜索过滤 |
| 实例表格 | 名称/端口/池状态/出口 IP/403 率/健康/会话/操作 |

---

## 使用说明（快速开始）

### 1. 提取代理

```bash
curl -s http://127.0.0.1:4433/api/pool/acquire
```

```json
{
  "code": "00000",
  "data": {
    "proxy": "socks5://45.205.27.41:10011",
    "name": "warp-1",
    "ip": "2606:4700:110:8a5a:...",
    "session_id": null
  }
}
```

### 2. 通过 SOCKS5 访问

```bash
curl -s --socks5-hostname 127.0.0.1:10011 -U user:pass https://api64.ipify.org
```

Python：

```python
import requests
resp = requests.get(
    "https://api64.ipify.org",
    proxies={"http": "socks5h://user:pass@127.0.0.1:10011",
             "https": "socks5h://user:pass@127.0.0.1:10011"},
)
print(resp.text)
```

### 3. 粘性会话

```bash
# 同一 sid 固定同一实例，30 分钟内不换
curl -s "http://127.0.0.1:4433/api/pool/acquire?session_id=task-001&minutes=30"
```

到期后 `cleanup_loop`（每 30 秒）自动清扫，下次同 sid 重新分配。

### 4. 归还

```bash
curl -X POST http://127.0.0.1:4433/api/pool/release \
  -H "Content-Type: application/json" -d '{"session_id":"task-001"}'
```

---

## API 参考

<details>
<summary>完整端点列表（点击展开）</summary>

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/api/stats` | 池统计 |
| GET | `/api/containers` | 实例列表 |
| GET | `/api/pool/acquire` | 提取代理（`session_id`/`time`/`sid`） |
| POST | `/api/pool/release` | 归还 |
| POST | `/api/pool/report` | 上报 HTTP 状态码 |
| GET | `/api/proxies` | 批量提取（`num`/`type`/`sid`/`time`/`ip_mode`） |
| POST | `/api/pool/dedicated/bind` | 绑定静态 IP |
| POST | `/api/pool/dedicated/unbind` | 解绑 |
| GET | `/api/pool/dedicated` | 绑定列表 |
| POST | `/api/whitelist` | 白名单增删 |
| GET | `/api/usage?days=30` | 用量 + 系统指标 |
| POST | `/api/start` | 启动实例（`index`） |
| POST | `/api/stop` | 停止实例 |
| POST | `/api/delete` | 删除实例 |
| POST | `/api/rotate` | 换 IP（必须显式 `{"all":true}`） |
| POST | `/api/creds/set` | 设置账密 |
| POST | `/api/creds/reset` | 重置账密 |

</details>

---

## 主要配置

`rust-config.json`：

| 键 | 说明 | 生产值 |
|---|---|---|
| `port` | 管理台端口 | `4433` |
| `base_port` | 账密 SOCKS5 起始端口 | `10010` |
| `whitelist_port_offset` | 白名单端口偏移 | `1000` |
| `resident_count` | 常驻实例数 | `500` |
| `max_instances` | 实例上限 | `500` |
| `sticky_minutes` | 粘性会话默认时长 | `3` |
| `session_cleanup_interval` | 过期会话清扫间隔（秒） | `30` |
| `max_concurrent_create` | 并发创建上限 | `3` |
| `api_key` | API 鉴权 | 见配置 |
| `proxy_auth_enabled` | 是否开账密 | `true` |
| `proxy_username` / `proxy_password` | 账密 | 见配置 |
| `ip_whitelist` | 白名单网段 | 见配置 |
| `ip_check_url` | IPv4 出口探测端点 | `http://api.ipify.org` |
| `ip_check_url_v6` | IPv6 出口探测端点 | `http://api6.ipify.org` |

环境变量：

| 变量 | 值 | 说明 |
|---|---|---|
| `WARP_NATIVE` | `quic` / `h2` | 数据面选择 |
| `WARP_FORCE_ENGINE` | `1` | 强制走 usque 子进程 |
| `WARP_USQUE_BIN` | 路径 | usque 二进制位置，默认 `/usr/local/bin/usque` |
| `RUST_LOG` | `info` / `debug` | 日志级别 |

---

## 推荐实例数（按服务器配置）

内存取决于数据面：

| 实例数 | `quic`（usque） | `h2`（原生） |
|---|---|---|
| 100 | 约 1.1 GB | 约 17 MB |
| 500 | 约 5.3 GB | 约 85 MB |
| 1000 | 约 10.6 GB | 约 170 MB |
| 3000 | 约 32 GB | 约 510 MB |

`h2` 的数值按 200 实例 24.6 MB 线性推算。

**其他约束：**

- **端口**：1000 实例需要 `10010~11009` + `11010~12009`，共 2000 个监听 socket。`ulimit -n` 要够。
- **磁盘**：每实例约 12 KB（`usque.json` + 元数据），1000 实例约 12 MB。真正的磁盘消耗是编译产物（约 4.6 GB），Docker 部署无此问题。
- **账号**：N 个实例需要 N 个 Cloudflare 账号。注册串行 2 秒/个，1000 个约 33 分钟，但 Cloudflare 会在几百个后限流（`error 1015`），实际要分批等。
- **Cloudflare 单 IP 隧道数**：`h2` 路径超过约 100 个隧道会持续被 `RST_STREAM`，靠 `ResilientTunnel` 重建维持。`quic` 路径 500 实例实测稳定。

---

## 实测数据

### 原生数据面压测（v0.11.0，2026-11-05）

200 实例，独立环境，24 分钟：

| 指标 | 值 |
|---|---|
| healthy | 199/200 全程不变 |
| stream 重置 | 779 次 |
| 自动重建 | 778 次 |
| 探测成功率 | 100%（ok=0 采样 0 次） |
| dial 失败 / 重建失败 | 0 / 0 |
| RSS | 8.3 → 24.6 MB（增速收敛） |

对照 v0.10.1 同一测试：reset 197 次、rebuild 0 次、20 端口只通 1 个。

### 生产（v0.11.0，500 实例，quic 路径）

| 指标 | 值 |
|---|---|
| 实例数 | 500 |
| healthy | 499/500 |
| 唯一出口 IP | 480+ |
| warpgate RSS | 17 MB |
| 500 × usque | 5053 MB |
| 合计 | 约 5.07 GB |
| 单请求延迟 | 168–450 ms |

### 出口 IP 汇总

| 类型 | 数量 | 说明 |
|---|---|---|
| IPv6 | = 账号数 | `2a09:bacx::/29` 段，每账号唯一 |
| IPv4 | 4 | LAX 节点 NAT 池，动态轮换 |

---

## 故障排查（FAQ）

<details>
<summary>常见问题（点击展开）</summary>

**Q: 端口在监听，API 报 healthy，但外部连不上**

防火墙没放行完整端口段。`ss -ltn` 和本机 curl 走回环，不受防火墙影响。

**Q: 500 个端口全在监听，但大部分连不上**

孤儿 usque 进程累积了太多 TCP 连接，Cloudflare 配额耗尽。查：

```bash
pgrep -c usque                              # 应该是 500
ss -tn | grep -c 162.159                    # 应该是 700-1500，不该上万
for p in $(pgrep usque); do awk '/PPid:/{print $2}' /proc/$p/status; done | grep -c "^1$"
```

孤儿数不为 0 就 `pkill -9 usque && systemctl restart warpgate`，v0.10.0 之后会自动清理。

**Q: 注册账号报 `error code: 1015`**

Cloudflare 限流。停 30 分钟到几小时，间隔改 8 秒。

**Q: 原生模式下 20 个端口只有 1 个能通**

`ResilientTunnel` 的重连失效。检查 `stack.rs` 里 reader 任务失败时是否调了 `shared_state.mark_dead()`，以及 `resilient.rs` 的 `ensure()` 是否调了 `t.is_dead()`。

**Q: `WARP_NATIVE=quic` 但日志说「Rust 原生 MASQUE」**

v0.9.0 的 bug，把 `quic` 当成了原生 QUIC。这台机器 UDP 443 被封，会全部握手超时。升级到 v0.10.0+，或临时去掉 `WARP_NATIVE` 只留 `WARP_FORCE_ENGINE=1`。

**Q: 粘性会话没到时间就换 IP 了**

`minutes` 参数在 v0.10.1 之前失效，实际按 `sticky_minutes` 走。升级。

</details>

---

## 测试

```bash
cd warpgate-rs
cargo test --workspace
# 200 passed
```

关键回归测试：

| 测试 | 锁住的契约 |
|---|---|
| `shared_helpers_are_wired` | `wake_all()` / `queue_removal()` 的三个调用点必须存在 |
| `ensure_consults_the_stack_dead_flag` | `ensure()` 必须查底层 `is_dead()`，且必须有健康巡检 |
| `explicit_minutes_override_the_default_ttl` | `minutes` 参数必须真正覆盖 `sticky_minutes` |
| `omitted_minutes_falls_back_to_the_configured_ttl` | 不传 `minutes` 时用配置默认值 |

---

## 安全提示

- 管理台 `4433` 不要直接暴露公网，或至少配好 `ip_whitelist`
- `api_key` 和 `proxy_password` 不要用默认值
- 白名单只加可信 IP
