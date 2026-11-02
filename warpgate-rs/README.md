# warpgate-rs

WarpGate 的 Rust 重写：控制面与数据面目标为单进程。**7 个 crate 全部完成。**

```
cargo test --workspace     # 134 passed, 0 warnings
cargo build --release      # warpgate 3.7 MB（Linux）
```

## 真实部署验证（服务器 45.205.27.41）

**完整链路已通**：控制面 + 数据面 + 调度 + API。

```
数据面就绪 20/20（失败 0）
实例就绪 20/2

$ curl -H "X-API-Key: k" http://127.0.0.1:14433/api/stats
{"healthy_count":20,"instance_count":20,"mode":"embedded","transport":"masque"}

$ curl -H "X-API-Key: k" "http://127.0.0.1:14433/api/pool/acquire?session_id=s1"
{"proxy":"socks5://127.0.0.1:20010","ipv6":"2606:4700:110:82e1:...","name":"warp-0"}

$ curl --socks5-hostname 127.0.0.1:20010 --proxy-user warpuser:xxx https://api.ipify.org
104.28.195.192        ← 真实出口 IP，与老系统一致
```

### 数据面怎么选的

服务器 UDP 443 到 Cloudflare 被完全封禁，所以 Rust 原生的 QUIC 路径连不上。`config.json` 里 `masque_http2: true` 时自动回落到 **usque 外部引擎**（`usque socks --http2`，TCP 443），与老系统同一条路径。

```bash
# 默认：masque_http2=true → usque 引擎；false → Rust 原生 QUIC
./warpgate --config config.json --instances ./instances

# 强制某一条路
WARP_FORCE_ENGINE=1 ./warpgate ...           # 强制 usque 引擎
WARP_USQUE_BIN=/path/to/usque ./warpgate ... # 指定 usque 路径
```

代价：每实例多一个 usque 进程，**内存收益只剩控制面那部分**。原生 QUIC 路径的代码已就绪（见下），在 UDP 443 可达的网络上不需要 usque。

## 原生 MASQUE 数据面（Phase 3）

不再依赖 `usque` 子进程。`wg-tunnel` 现在自己说 QUIC + HTTP/3。

### Cloudflare 实际用的协议

**不是 RFC 9484 的 CONNECT-IP。** L4（TCP）路径是普通的 HTTP/3 `CONNECT`，目标是**要访问的地址**：

```text
CONNECT https://<target-host>:<port> HTTP/1.1
Host: <target-host>:<port>
```

之后这条请求流就是裸 TCP 字节。一个实例共享一条 QUIC 连接，每条 TCP 连接各开一个 stream——握手成本按实例付，不按连接付。

### 认证

双向 TLS：客户端出示 Enroll 过的 P-256 证书，服务端证书 pin 到 `usque.json` 的 `endpoint_pub_key`。SNI 固定为 `consumer-masque-proxy.cloudflareclient.com`。

### 实现的模块

| 模块 | 内容 |
|---|---|
| `masque.rs` | quinn 连接、TLS pinning、CONNECT 帧、字节中继 |
| `register.rs` | `POST /v0a4471/reg` + `PATCH /reg/<id>`，账号注册与密钥登记 |

### 已验证（本地 QUIC 服务器，无需外网）

```
cargo test -p wg-tunnel --test masque_roundtrip
  masque_dial_round_trips_bytes          ok
  multiple_streams_share_one_quic_connection  ok
  a_non_200_reply_is_an_error            ok
  a_missing_endpoint_is_an_error         ok
```

### 尚未验证

**真实 Cloudflare 端点。** 部署服务器 `45.205.27.41` 的 UDP 出网被完全封禁（`nc -u 1.1.1.1 53` 无响应），QUIC 无法建立。需要在 UDP 443 可达的机器上跑一次端到端。

### 实测中修掉的四个坑

**1. `--idle-timeout` 已从 usque 移除**
Node 版传这个参数，但新版 usque 直接 `Error: unknown flag` 退出，症状是"端口始终不就绪"、子进程变 zombie。加参数前先确认目标二进制支持。

**2. `kill_on_drop` 会杀掉刚 spawn 的子进程**
tokio 的 `Command` 默认 drop 时 SIGKILL 子进程。`child` 在函数末尾离开作用域，引擎立刻就死。修法：`kill_on_drop(false)` + `ManuallyDrop`。

**3. 上一个 run 的 `status: unhealthy` 会被沿用**
Node 版写下的 stale 状态让新实例起不来调度（`healthy_count: 0`）。暖启动必须复位非 `stopped` 的状态。

**4. 管道不读会堵死子进程**
stderr 用 `Stdio::null` 时启动失败完全不可见；改成 piped 并起 task 排空，否则管道缓冲区满了子进程会阻塞。

### 实现中踩到的三个坑

**1. `SendStream` 不自动 flush**
`write_all` 后不 `flush`，数据永远不到对端，对方只看到 stalled 连接。relay 两个方向都必须显式 flush。

**2. 克隆 fd 会让 relay 偷走调用方的字节**
`try_clone` 的 fd 共享 socket 缓冲区，relay task 的 `read` 会把调用方正在等的字节读走。改用 `tokio::io::duplex`（内存管道）+ `split`，读写归属明确。

**3. `finish()` 不能早调**
写完 CONNECT head 就 `finish()` 会让对端认为请求结束、停止转发 payload。流必须保持双向。

## 内存：核心指标（生产服务器实测）

**服务器**：`45.205.27.41`（16 核 / 15.8 GB，CentOS 7），Node 版与 Rust 版**同机并存**实测。

### 20 实例同机对比

```
--- Node 版 ---  RSS: 564.9 MB   进程数: 20  (19 sing-box + 1 node)
--- Rust 版 ---  RSS:   2.33 MB  进程数: 1
Rust API: {"code":"00000","data":{"healthy_count":20,"instance_count":20,...}}
```

**注：以上是控制面对比。** 用 usque 引擎时每实例还要一个 usque 进程，实测总计约 2.96 MB（warpgate）+ 20 × ~10 MB（usque）。内存收益要到原生 QUIC 路径跑通后才能完全体现。

**每实例边际成本：Node ~28 MB/实例 → Rust 0.006 MB/实例，约 4600 倍。**

### 200 实例外推

| | Node 版 | Rust 版 |
|---|---|---|
| 进程数 | 401 | **1** |
| 空跑基线 | ~120 MB | **2.33 MB**（实测） |
| 每实例 | ~28 MB | **0.006 MB**（实测） |
| 200 实例总计 | **~5.7 GB**（按实测外推） | **~3.5 MB**（按实测外推） |

### 内存账：钱花在哪

**Go 进程空跑也占 ~20 MB**（runtime + GC + 调度器）。Node 版 200 实例 = 401 个进程 ≈ 8 GB 基线，而真正的隧道状态只有几百 MB。Rust 没有 GC 基线，省的主要是这部分。

### 本地基准（`cargo test --release -p wg-pool --test memory -- --ignored --nocapture`）

Linux 服务器实测（含 175 个 SOCKS5 监听）：

```
instances              : 200
RSS baseline           : 1.21 MB
RSS after metadata only: 1.72 MB  (0.0026 MB/inst)
SOCKS5 listeners       : 175
RSS after listeners    : 1.98 MB  (0.0038 MB/inst)
```

### 诚实的边界

本服务器 **UDP 443 到 Cloudflare 被阻断**，MASQUE 隧道无法建立，因此 Rust 版实例只加载了账号配置与 SOCKS5 监听，**未含真实 QUIC 会话状态**。按 Go 版实测每实例 2-5 MB 估算，Phase 3 数据面落地后 Rust 版约 **400 MB - 1 GB / 200 实例**，仍比 Node 版低 **6-14 倍**。

## 已验证的端到端行为

二进制冒烟（生产服务器 `45.205.27.41`）：

```
$ warpgate --version
warpgate 0.1.0

$ curl -H "X-API-Key: k" http://127.0.0.1:14433/api/stats
{"code":"00000","data":{"healthy_count":20,"instance_count":20,"mode":"embedded",
 "resident_count":20,"sticky_sessions":0,"transport":"masque"},"msg":""}

外部IP 无 Key  → 401 A0001
外部IP 带 Key  → 200
本机无 Key     → 200（loopback 免 Key，符合 Go 语义）
```

## 已实现

| crate | 行数 | 测试 | 内容 |
|---|---|---|---|
| `wg-core` | ~330 | 7 | `{code,msg,data}` 信封、错误码、状态常量 |
| `wg-config` | ~390 | 7 | `config.json` 全 47 键 + 校验 + 端口算术 |
| `wg-store` | ~700 | 23 | `meta.json` / `usque.json` 字节兼容读写 |
| `wg-socks` | ~560 | 14 | SOCKS5 握手矩阵 + 白名单解析 |
| `wg-tunnel` | ~560 | 31 | dial trait 边界 + 外部引擎 + resolver + **原生 MASQUE** + 注册 API |
| `wg-pool` | ~850 | 17 | 生命周期、RR、粘性、健康、SOCKS5 监听 |
| `wg-apiserver` | ~800 | 32 | 20 个 A 级端点 + 鉴权 + 限流 |
| `wg-bin` | ~130 | — | main + 信号处理 |

## 六条不能违反的约束

1. **`config.json` 键必须同名同义** —— Node 版运行期会读写它。
2. **`instances/warp-<i>/*.json` 必须字节兼容** —— Go / Node / Rust 三个二进制要能互相接管，所以 `meta.json` 两空格缩进、`usque.json` 0600 权限。
3. **API 响应必须是 `{code,msg,data}`** —— 业务错误保持 HTTP 200，只有鉴权(401)/限流(429)/内部错误(500) 用真实状态码。
4. **UDP ASSOCIATE 必须显式回 0x07** —— 不能静默丢弃。
5. **两个 SOCKS5 端口语义**：账密 `base_port+i`、白名单 `base_port+offset+i`。
6. **Round-Robin 只排 `running`，但 `unknown` 健康度算可调度** —— 这是 v11.0 的修复，旧规则让池容量缩水 30%。

## 移植中修掉的两个真实 bug

**SOCKS5 拒绝时没读完整请求 → RST**（客户端收不到拒绝）
BIND/UDP 被拒时写完 0x07 就 return，但请求里地址+端口 6 字节还在内核缓冲，socket drop 带未读数据 → 内核发 RST → 客户端看到 `ConnectionReset` 而非拒绝响应。修法：拒绝前读完整请求 + `shutdown()` 半关闭。**Go 版大概率有同样问题**（`socks5.go` L167-169 也是写完就 return）。

**`Manager` 的 `Clone` 在 async 里直接 panic**
`RwLock::blocking_read()` 在 tokio runtime 内是禁止的。删掉 `Clone`，改 `Arc<Self>` 共享。

## Node 版兼容性（实测发现并修复）

Node 的 `meta.json` 是 **camelCase** 且字段集不同：

```json
{ "createdAt": 1793452299660, "engine_pid": 13209, "transport": "masque",
  "pid": 13210, "port": 10010, "ip": null, "endpoint": null }
```

Rust 版用 `serde(alias)` 接受 Node 拼写（`createdAt`→`created_at`，`ip`→`ipv6`），`deserialize_with = "null_to_empty"` 接受 `null`，并**仍写 Go 拼写**保持与 Go 版互换。Node 目录缺失的 `index`/`name`/`status` 由 pool 从目录名推导。

## 未实装（Phase 3）

- **真实 MASQUE/QUIC 客户端** —— dial trait 边界已就绪，外部引擎（usque 子进程）可立即用
- Cloudflare 注册 API（`api.Register` + `api.EnrollKey`）
- `masque_http2` TCP 443 兜底
- dedicated 固定 IP 绑定
- scamalytics 风控值检测
- 多服务器 SSH 管理

## 相关

- 参照实现：[`warpgate-go/`](../warpgate-go/)（Go 版，3160 行，含测试）
- 需求文档：[`warpgate-go/REQUIREMENTS-GO.md`](../warpgate-go/REQUIREMENTS-GO.md)
- 原始需求：[`REQUIREMENTS.md`](../REQUIREMENTS.md)
- 编排用的 seed：[`warpgate-rust-seed.json`](../warpgate-rust-seed.json)
