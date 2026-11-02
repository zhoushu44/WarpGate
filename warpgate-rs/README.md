# warpgate-rs

WarpGate 的 Rust 重写：控制面与数据面目标为单进程。**7 个 crate 全部完成。**

```
cargo test --workspace     # 153 passed, 0 warnings
cargo build --release      # warpgate 3.7 MB（Linux）
```

## 生产部署验证（服务器 45.205.27.41，与老系统无缝替换）

**完整链路已通**，端口/密钥/实例与老系统完全一致：

```
管理台   4433              （同老系统）
账密端口 10010+i            （同老系统）
白名单   11010+i            （同老系统）
引擎端口 127.0.0.1:20000+i  （同老系统 ENGINE_PORT_BASE）
```

### 实测结果

| 指标 | 老系统（Node） | 新版（Rust） |
|---|---|---|
| 进程数 | 201（node + 100 sing-box + 100 usque） | 101（warpgate + 100 usque） |
| 控制面内存 | node 41 MB + sing-box 547 MB = 588 MB | **warpgate 4.69 MB** |
| 每实例边际 | 29.4 MB | 7.83 MB |
| 200 实例外推 | ~5.9 GB | ~1.5 GB（**省 75%**） |
| 100 实例启动 | — | **25.6 秒** |

### 端到端验证通过

```bash
$ curl -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
{"healthy_count":100,"instance_count":100,"mode":"embedded","transport":"masque"}

$ curl -s --socks5-hostname 127.0.0.1:20001 https://api.ipify.org
104.28.195.192                              # 真实 WARP 出口 IP

$ curl -X POST http://127.0.0.1:3077/v1/images/generations \
    -H "Authorization: Bearer zs1236547" \
    -d '{"prompt":"a red apple on a white table","model":"gpt-image-2"}'
{"created":0,"data":[{"b64_json":"/9j/2wCEAAMCAgM..."}]}   # 生图成功
```

**ChatGPT 注册也完整通过**：邮箱注册 → TOTP 2FA → 验活 → Session Token → 自动导入 chatgpt2api，耗时 71 秒。

### 关键修复（移植中发现）

1. **`ip_whitelist` 两种磁盘格式** —— Node 存对象数组 `[{ip,remark,region,added_at}]`，Go/Rust 用字符串数组。漏掉 `172.21.0.0/16`（regiforge_default 网段）会导致容器内白名单端口被拒。
2. **`/api/proxies` 的 `proxy` 字段必须跟随白名单端口** —— regiforge 的 `wary` provider 按 `proxy` → `proxy_whitelist` 顺序读取，返回账密端口会让它因 `no acceptable authentication methods` 失败。
3. **`public_ip` 不能是 `127.0.0.1`** —— `auto-detect` 时要返回 Docker 网桥地址（`172.17.0.1`），否则容器内连不通。
4. **`txt` 模式必须带账密** —— `socks5://user:pass@host:port`，调用方没有别的渠道获知账密。
5. **引擎端口基址必须是 20000** —— chatgpt2api-go 保存了 `127.0.0.1:20000+<index>` 形式的账号级代理绑定，换基址会让这些绑定全部失效。

## 原生 MASQUE 数据面（Phase 3）

不再依赖 `usque` 子进程。`wg-tunnel` 现在自己说 QUIC + HTTP/3。

### Cloudflare 实际用的协议

**不是 RFC 9484 的 CONNECT-IP。** L4（TCP）路径是普通的 HTTP/3 `CONNECT`，目标是**要访问的地址**：

```text
CONNECT https://<target-host>:<port> HTTP/1.1
Host: <target-host>:<port>
```

之后这条 stream 就是裸 TCP 字节。一个实例共享一条 QUIC 连接，每条 TCP 连接各开一个 stream——握手成本按实例付，不按连接付。

### 认证

双向 TLS：客户端出示 Enroll 过的 P-256 证书，服务端证书 pin 到 `usque.json` 的 `endpoint_pub_key`。SNI 固定为 `consumer-masque-proxy.cloudflareclient.com`。

### 实现的模块

| 模块 | 内容 |
|---|---|
| `masque.rs` | quinn 连接、TLS 公钥 pinning、CONNECT 帧、字节中继 |
| `connectip/h2.rs` | 手写 HTTP/2 帧（含 `:protocol` pseudo-header）+ capsule 协议 |
| `connectip/probe.rs` | 驱动 h2 客户端，是是否投入用户态栈的判据 |
| `register.rs` | `POST /v0a4471/reg` + `PATCH /reg/<id>`，账号注册与密钥登记 |

### 已验证 vs 未验证

**已验证**：本地 QUIC 服务器 4 个 round-trip 测试全绿（握手、CONNECT 帧、字节中继、多 stream 复用）；TLS 公钥 pinning 密钥实测逐字节一致。

**未验证**：真实 Cloudflare QUIC 端点。部署服务器 UDP 443 到 CF 被完全封禁（Go 探针实测 DNS 通、MASQUE 端点全丢包），需 UDP 443 可达的机器。

### 实测中修掉的三个坑

**1. `SendStream` 不自动 flush**
`write_all` 后不 flush，数据永远不到对端，对方只看到 stalled 连接。relay 两个方向都必须显式 flush。

**2. 克隆 fd 会让 relay 偷走调用方的字节**
`try_clone` 的 fd 共享 socket 缓冲区，relay task 的 read 会把调用方正在等的字节读走——症状是 `early eof`。改用 `tokio::io::duplex` 内存管道 + `split`，读写归属明确。

**3. `finish()` 不能早调**
写完 CONNECT head 就 finish，对端认为请求结束、停止转发 payload。

## 内存：核心指标（生产服务器实测）

**服务器**：`45.205.27.41`（16 核 / 15.8 GB，CentOS 7）。

### 100 实例同机实测

```
warpgate（Rust 控制面）: 4.69 MB   / 1 进程
usque（数据面）        : 783 MB    / 100 个（7.83 MB/实例）
─────────────────────────────────
总计                   : 788 MB

老系统对照             : node 41 MB + sing-box 547 MB + usque = 588 MB / 20 实例
```

**每实例边际成本：Node 29.4 MB → Rust 7.88 MB，省 73%。**

### 200 实例外推

```
Rust 版: 4.7 + 200 × 7.83 ≈ 1571 MB  (1.5 GB)
Node 版: 41 + 200 × 29.4 ≈ 5921 MB   (5.9 GB)
```

### 内存账：钱花在哪

**Go 进程空跑也占 ~20 MB**（runtime + GC + 调度器）。老系统 200 实例 = 401 个进程 ≈ 8 GB 基线，而真正的隧道状态只有几百 MB。Rust 没有 GC 基线，省的主要是这部分。

剩下 1.5 GB 在 usque 进程里，需原生 QUIC 路径跑通才能去掉。

## 已实现

| crate | 行数 | 测试 | 内容 |
|---|---|---|---|
| `wg-core` | ~330 | 7 | `{code,msg,data}` 信封、错误码、状态常量 |
| `wg-config` | ~400 | 9 | config.json 全 47 键 + 校验 + 端口算术 + 白名单双格式 |
| `wg-store` | ~700 | 23 | meta.json/usque.json 字节兼容（含 Node camelCase 兼容） |
| `wg-socks` | ~560 | 14 | SOCKS5 握手矩阵 + 白名单解析 |
| `wg-tunnel` | ~1000 | 40 | dial trait + 外部引擎 + resolver + 原生 MASQUE + HTTP/2 CONNECT-IP + 注册 API |
| `wg-pool` | ~1280 | 18 | 生命周期/RR/粘性/健康/SOCKS5 监听/public_ip 探测 |
| `wg-apiserver` | ~800 | 32 | 20 个 A 级端点 + 鉴权 + 限流 |
| `wg-bin` | ~130 | — | main + 信号处理 + CONNECT-IP 探针 |

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

## 其他实测修复

- usque `--idle-timeout` 参数已移除，传了会直接退出（症状：引擎端口始终不就绪）
- `kill_on_drop` 会杀掉刚 spawn 的子进程
- Node 版遗留的 stale `unhealthy` 状态导致实例无法调度
- `getrandom` 符号在 CentOS 7（glibc 2.17）不存在，需用 crate
- SOCKS5 管道不排空会堵死子进程

## 未实装（Phase 3）

- **用户态 TCP/IP 栈** —— HTTP/2 CONNECT-IP 传的是 IP 包，需要 gvisor 的 Rust 替代品
- Cloudflare 注册 API 的真实调用（代码已写，未联网验证）
- dedicated 固定 IP 绑定
- scamalytics 风控值检测
- 多服务器 SSH 管理

## 相关

- 参照实现：[`warpgate-go/`](../warpgate-go/)（Go 版，3160 行，含测试）
- 需求文档：[`warpgate-go/REQUIREMENTS-GO.md`](../warpgate-go/REQUIREMENTS-GO.md)
- 原始需求：[`REQUIREMENTS.md`](../REQUIREMENTS.md)
- 编排用的 seed：[`warpgate-rust-seed.json`](../warpgate-rust-seed.json)
