# 抓包对比报告：usque vs native（HTTP/2 CONNECT-IP）

日期：2026-11-04 · 服务器 45.205.27.41 · 目标 `162.159.198.2:443`
账号：`warp-1`（`1f6ac59c-…`）、备用 `instances-bad/warp-82`（`6418cef4-…`）

---

## 0. 结论先说

**没有找到**"字节层面的差异导致 CertificateUnknown"。抓到并解出两份明文请求之后，
原来的假设（我们的 CONNECT-IP 报文写错了 → 服务器发 TLS alert）**被证伪**：

- `CertificateUnknown` 是 **TLS 层**的 alert，只可能由**客户端证书**触发，与 HTTP/2 HEADERS 无关。
  服务器已经在握手阶段就拒绝，压根没看到我们的 CONNECT-IP 请求。
- 真正原因有两个，互相独立：

| # | 根因 | 表现 | 证据 |
|---|------|------|------|
| **1** | `diagnose()` 用 `build_tls_for_test()`，它**每次现场随机生成一把 P-256 私钥**，不是 `usque.json` 里已登记的私钥 | `tls: OK (client cert accepted)` → `connect-ip: FAILED io: received fatal alert: CertificateUnknown` | 用已登记私钥 + 完全相同报文 = `200`；换成随机私钥 = `tls: unknown certificate`，逐次可复现 |
| **2** | `162.159.198.2` 这个 VIP 后面**混着两种后端**：一种会答 CONNECT-IP，另一种只是普通 Cloudflare 前端 | 该 IP 上 400/异常占比高、且 `pinned+clientcert` 偶发 `UnknownIssuer` | 24 次拨号中 7 次拿到 `CN=cloudflareclient.com`（Let's Encrypt）证书，全部回 `400`；拿到 `CN=masque.cloudflareclient.com` 的 17 次全部 `200` |

修掉 #1 之后 native 已经能端到端跑通（见 §5）。#2 靠重试/换 IP 规避。

---

## 1. 抓包工具确认

| 工具 | 结果 |
|------|------|
| `which tcpdump tshark ngrep` | **全都没有** |
| `yum install -y tcpdump` | 成功装到 `tcpdump 4.9.2`（EPEL/base 可用） |
| `tshark` | 服务器 yum 只有 1.10 老版；改用 `docker run nicolaka/netshoot`（自带 tshark 4.6.6）解析 |
| `docker` | 可用，`golang:1.26-alpine`、`nicolaka/netshoot` 都能拉 |
| `strace` / `ngrep` / `socat` / `python3` | 都没有（有 python 2.7、perl、openssl） |

所以 **方案 B（网卡直接抓）** 是主力，`tcpdump -i eth0 -s 0 -w`。方案 A（TCP 转发）最后演化成了本地的
**TLS/h2 sink + MITM**（§3），比裸转发有用得多，因为裸转发看不到 TLS 之后的内容。

---

## 2. 两份 pcap 的逐条差异

`/tmp/cap_usque.pcap`（28 包）与 `/tmp/cap_native.pcap`（110 包），都在 eth0 上抓 `host 162.159.198.2 and tcp port 443`。

### 2.1 TLS ClientHello

| 项 | usque (Go) | native (rustls) | 判定 |
|----|-----------|-----------------|------|
| SNI | `consumer-masque.cloudflareclient.com` | 同 | 一致 |
| ALPN | `h2` | `h2` | 一致 |
| supported_versions | TLS1.3, TLS1.2 | 同 | 一致 |
| **扩展数量** | 12 个 / 1429 字节 | **10 个 / 164 字节** | **不同** |
| **client key_share 组** | `X25519MLKEM768` + `x25519`（1258 字节，PQ 混合） | 只有 `x25519`（38 字节） | **不同** |
| supported_groups | X25519MLKEM768, SecP256r1MLKEM768, SecP384r1MLKEM1024, x25519, secp256r1, secp384r1, secp521r1 | x25519, secp256r1, secp384r1 | **不同** |
| cipher suites | 13 个（含 CBC 老套件） | 10 个（无 CBC） | 不同 |
| signature_algorithms | 10 个，顺序 rsa_pss_rsae_sha256 优先 | 6 个，ecdsa_secp256r1_sha256 优先 | 不同 |
| 有 `signature_algorithms_cert` | 有 | 无 | 不同 |
| 有 `signed_certificate_timestamp` | 有 | 无 | 不同 |
| 有 `renegotiation_info` | 有 | 无（改用 SCSV 0x00ff） | 不同 |
| 有 `session_ticket` | 无 | 有 | 不同 |
| ClientHello 长度 | 1537 字节 | 266 字节 | 不同 |

**共同点**：两条连接都先收到 **HelloRetryRequest**，再发第二个 ClientHello 才完成握手。
说明 HRR 不是失败原因（usque 也在吃 HRR）。

**这些差异全部不影响结果** —— 用 Go 客户端 + 已登记私钥（天然是 usque 的 ClientHello 形状）
与用 rustls 形状（我在 Go probe 里模拟的 `-cert rust`）都能拿到 `200`；反过来把证书换成随机密钥，
两种形状都是 TLS alert。**TLS 指纹不是变量。**

### 2.2 客户端证书（这是真正的分水岭）

从本地 TLS sink 拿到两边的完整证书 DER：

| 字段 | usque | native（`build_tls_for_test`） | native（`build_h2_tls_config`，正确路径） |
|------|-------|--------------------------------|------------------------------------------|
| 长度 | 239 字节 | **361 字节** | 361 字节 |
| Subject / Issuer | **空** | `CN=rcgen self signed cert` | `CN=rcgen self signed cert` |
| Serial | 0 | 0 | 0 |
| NotBefore / NotAfter | now / **now+24h** | **1975-01-01 / 4096-01-01** | 1975 / 4096 |
| SAN | 无 | `consumer-masque.cloudflareclient.com` | 同 |
| **SPKI EC point** | `04 5064 390b…63b18`（**已登记的那把**） | **每次连接都不同**（随机） | `04 5064 390b…63b18`（一致） |

- 证书**形状**（Subject 空 vs rcgen 默认、24h vs 4096 年、有无 SAN）**不是**问题：
  我用 Go 造了 rcgen 形状的证书 + 已登记私钥，连打 4 次，3 次 `200`、1 次 `400`（400 是 §2.4 的后端问题）。
- **私钥是不是已登记的那把，才是问题**：随机私钥连打 3 次，3 次全部
  `read: remote error: tls: unknown certificate` —— 与 nativecheck 报的
  `fatal alert: CertificateUnknown` 是同一个 alert（Go 与 rustls 的措辞不同）。

**alert 时机**也对得上：服务器在收到 ClientHello 后立刻回 alert，没有任何 HTTP/2 帧。
即"服务器根本没看我们的 CONNECT-IP 请求"。

### 2.3 HTTP/2 SETTINGS 帧

| 项 | usque | native |
|----|-------|--------|
| 客户端 SETTINGS | `0x2=0`(ENABLE_PUSH), `0x4=4194304`(INITIAL_WINDOW), `0x5=16384`(MAX_FRAME), `0x6=10485760`(MAX_HEADER_LIST) | 同样四项（当前版本已对齐） |
| 连接级 WINDOW_UPDATE | 有 | 有 |
| 服务器 SETTINGS | `0x3=256, 0x4=1MB, 0x5=16384, 0x6=131072` | 同 |
| 服务器是否发 `0x8` (ENABLE_CONNECT_PROTOCOL) | **不发** | **不发** |

**关键发现**：Cloudflare 这个端点**从不发 `SETTINGS_ENABLE_CONNECT_PROTOCOL (0x8)`**。
按 RFC 8441 §3，没有 0x8 就不能用 Extended CONNECT（`:protocol`）。usque 走的是**普通 CONNECT**。

### 2.4 HEADERS 帧（HPACK 解出来的真实列表）

我用本地 TLS sink（终止 TLS、明文打印帧）拿到了两边真正的请求头：

**usque（`connectip.DialH2` → Go http2）**
```
:method: CONNECT
:authority: cloudflareaccess.com:443
cf-connect-proto: cf-connect-ip
pq-enabled: false
accept-encoding: gzip
+ (Go 自动加的 user-agent: "" ，在 sink 里可见)
```

**native（`h2.rs::encode_connect_ip_headers`，仓库 HEAD 版本）**
```
:method: CONNECT
:scheme: https
:authority: cloudflareaccess.com
:path: /
:protocol: connect-ip          ← Extended CONNECT
capsule-protocol: ?1
cf-connect-proto: cf-connect-ip
pq-enabled: false
user-agent: ""
```

差异三条，逐条在真边缘上验证过（备用账号，无 usque 抢连）：

| 变体 | 结果 |
|------|------|
| usque 原始块 | `200` |
| usque + `:protocol: connect-ip` | **`RST_STREAM(PROTOCOL_ERROR)`** |
| usque + `:scheme`/`:path` | **`RST_STREAM(PROTOCOL_ERROR)`** |
| usque + 全部 Extended CONNECT 三件套 | `RST_STREAM` + `GOAWAY` |
| usque + `capsule-protocol: ?1` | `200`（无害） |
| `:authority` 带不带 `:443` | 都 `200`（不是变量） |

→ **`:protocol` / `:scheme` / `:path` 会被边缘直接 RST**，因为服务器没开 Extended CONNECT。
这是**第二个独立的 bug**，只是它表现为 `RST_STREAM`，不是 TLS alert，所以在"被 alert 挡住"的阶段
根本看不到。

### 2.5 附带发现：VIP 后端混布

`162.159.198.2:443` 同一个 IP:端口会轮流给出两种证书：

```
17/24  CN=masque.cloudflareclient.com   issuer=Cloudflare Self-Signed Root   → CONNECT-IP 200
 7/24  CN=cloudflareclient.com          issuer=Let's Encrypt YE1            → CONNECT-IP 400
```

`cert → outcome` 是**严格一对一**的（12 次采样：6 次 200 全在 masque 证书上，6 次 400 全在 LE 证书上）。
另外三个地址的混布比例差很多：

| 地址 | 20 次拨号中 masque 后端 | 非 masque 后端 |
|------|------------------------|----------------|
| 162.159.198.2 | 1 | **19** |
| 162.159.198.3 | **20** | 0 |
| 162.159.198.4 | 16 | 4 |
| 162.159.198.5 | 17 | 3 |

这解释了两件以前被当成"玄学"的现象：
1. `nativecheck` 的 `B pinned+clientcert` 有时 `OK`、有时 `UnknownIssuer` —— 不是 pin 代码错，是**这次拨号落到了错误后端**。
2. usque 之所以"看起来 100% 成功"：它带 `--always-reconnect`，失败会重连，日志只留下最后成功的
   `Connected to MASQUE server`；单次拨号它也一样会撞上 400。

---

## 3. 方法说明（做了什么，为什么可信）

1. **方案 B**：`tcpdump -i eth0 -s 0 -w`，先 usque 后 nativecheck，两份 pcap 用
   `docker run nicolaka/netshoot tshark` 解析。
2. 裸 pcap 只能看到 TLS 密文，所以补了 **本地 TLS/h2 sink**：Go 写的 ALPN=h2 服务端，
   要求客户端证书，把证书 DER 和每个 HTTP/2 帧（含 HPACK 解码后的 header 列表）明文打印。
   客户端只需要把 `endpoint_v4` 指过去 —— 用 `iptables -t nat OUTPUT -d 198.18.0.1 -j DNAT --to-destination 127.0.0.1:8443`
   实现，**不用改客户端代码**（native 硬编码 :443）。
3. **MITM**：另写一个 TCP 代理，客户端侧终止 TLS、上游用账号真实私钥重新握手到真边缘，
   于是"我们发什么"和"边缘回什么"能同时看到明文。用它确认了 usque 的请求块与真边缘的 `200` 响应。
4. **h2probe**：Go 写的单请求探针，可以注入任意 HPACK 块 / 任意证书形状 / 任意密钥，
   在真边缘上做 A/B。所有 §2.4 的结论都来自它，每组都跑了多次。
5. 全部测试用的都是备用账号 `instances-bad/warp-82`，**没有动 500 个线上实例**；
   `warp-1` 只是共享账号，测试期间线上 usque 一直在跑。

---

## 4. 修复建议

### 4.1 必须修：诊断路径用了随机密钥（就是本次卡住的直接原因）

`crates/wg-tunnel/src/connectip/native.rs::diagnose()` 里：

```rust
let tls = match crate::masque::build_tls_for_test(Some(endpoint_pub_key_pem.to_string())) {
```

而 `build_tls_for_test` → `build_h2_tls_config_for_test_inner` 里是 `getrandom` 现场造一把新私钥。
改成已登记的那把：

```rust
let signing = crate::masque::parse_private_key(private_key_b64)?;   // 已经解析过了
let tls = crate::masque::build_h2_tls_config(&signing, Some(endpoint_pub_key_pem))?;
```

（服务器上的源码 00:55 之后已经是这个写法了，`native.rs` 里还留了注释说明这个坑。
本地工作区同样已经是修好的版本；**但 `git HEAD`（v0.6.0 提交）里还是 `build_tls_for_test`**，
也就是说这个 bug 仍然在被提交的历史里，发布前要确认工作区的修复确实进了 commit。）

顺便：`build_tls_for_test` 这个函数名和用途都太危险，建议改名成
`build_h2_tls_config_with_ephemeral_key_for_negative_test()` 或直接删掉，
避免以后又被 `diagnose` 这类"只是想测一下 TLS"的路径误用。

### 4.2 必须修：请求头不能发 Extended CONNECT

`h2.rs::encode_connect_ip_headers` 要去掉 `:protocol` / `:scheme` / `:path`，
`:authority` 保持 `cloudflareaccess.com:443`：

```rust
let mut out = Vec::with_capacity(128);
push_literal(&mut out, b":method", b"CONNECT");
push_literal(&mut out, b":authority", authority.as_bytes());   // "cloudflareaccess.com:443"
push_literal(&mut out, b"capsule-protocol", b"?1");
push_literal(&mut out, b"cf-connect-proto", b"cf-connect-ip");
push_literal(&mut out, b"pq-enabled", b"false");
push_literal(&mut out, b"user-agent", b"");
out
```

配套：`read_response` 不要再等 `SETTINGS_ENABLE_CONNECT_PROTOCOL`（这个边缘永远不发，
等了就是超时）。本地 `h2.rs` 的注释已经写清了这一点，代码也已改好（`peer_enabled_extended_connect()`
只作诊断用，不参与判断），**但 `git HEAD`（v0.6.0）里仍是 Extended CONNECT 版本**（437 行那一版），
工作区是修好的 1361 行版本 —— 同样是"修复还没进 commit"的问题。

### 4.3 建议：把"后端混布"当成常态处理

`native.rs` 已经有 `CONNECT_ATTEMPTS = 4`（每次重开 TCP，让 VIP 重新选后端），这个设计是对的，
但建议再加两条：

1. **优先换 IP 而不是只重试同一 IP**：实测 `.2` 有 95% 概率落到非 MASQUE 后端，而 `.3` 是 0%。
   重试时把 endpoint 轮转起来（`.2 → .3 → .4 → .5`）比在同一 IP 上重试 4 次有效得多。
2. **区分可重试与不可重试**：`CertificateUnknown` / `UnknownIssuer`（落错后端）和
   `RST_STREAM(PROTOCOL_ERROR)`（请求写错）要分开记日志，否则线上看起来都是"连不上"。

另外 `usque.json` 里的 `endpoint_pub_key` 只对应 masque 后端那把；
拿到 LE 证书时报 `UnknownIssuer` 是**正确行为**（不要为了"能连上"去放宽 pin）。

---

## 5. 验证结果

同账号、`/root/warpgate-rs/app/target/release/nativecheck`：

```
--- 修复后的二进制（md5 f5b5a56c…，02:06 构建）---
run1:  connect-ip: OK stream=1   tunnel: UP
run2:  tunnel: UP
run3:  connect-ip: OK stream=1   tunnel: UP
run4:  tunnel: UP
run5:  connect-ip: OK stream=1   tunnel: UP
run6:  tunnel: UP
```

`tunnel: UP` 之后的 `http: 225 bytes` 是 `api.ipify.org` 的真实 200 响应，
说明 smoltcp 栈 + CONNECT-IP 数据面已经通了。

注意：`pin comparison` 段落里 `B pinned+clientcert` 仍会偶发 `UnknownIssuer`，
以及 `tls: FAILED invalid peer certificate: UnknownIssuer` —— 这是 §2.5 的后端混布，
**不是**回归。多次运行会看到它时好时坏，属预期。

---

## 6. 试过但没用的（诚实记录）

- 换 SNI（`consumer-masque-proxy…`）：不解决，且该 SNI 下 400 率更高。
- 换证书 CN：无效（证书形状本身已排除）。
- 换 pin / accept-any：只能证明 TLS 配置没问题，掩盖不了密钥错。
- 只改 HEADERS 不动密钥：仍是 TLS alert，因为握手就挂了。
- 只改密钥不动 HEADERS：TLS 过，但 `RST_STREAM(PROTOCOL_ERROR)`。
- 用本地 sink 只测 TLS：会误判"TLS 完全正常"，因为本地 sink 不校验客户端证书归属。

---

## 7. 环境残留

服务器上已清理：DNAT 规则、`h2sink`/`mitm` 容器、tcpdump 进程全部撤掉，
500 个 usque 实例未受影响（`ps aux | grep -c '[u]sque socks'` = 501）。
保留的仅 `/tmp/*.pcap`、`/tmp/usque-main`、`/root/{h2probe2,certprobe,certstatus,mitm,h2sink}` 等分析产物，
可随时删除。

安装/留存的包：`tcpdump 4.9.2`（yum 装的），docker 镜像 `nicolaka/netshoot`、`golang:1.26-alpine`。
