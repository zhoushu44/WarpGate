# TLS 层排查报告：CONNECT-IP `CertificateUnknown`

**结论：客户端证书的 X.509 扩展完全不是问题。** 服务端从来不看 keyUsage / extendedKeyUsage / SAN / subject / 有效期。卡点有两个，都在 TLS 之外，只是被 TLS 错误码伪装了。

---

## 一、推翻了「证书扩展」假设（有实验证据）

### 1.1 usque 的证书实际长什么样

从 `usque/internal/utils.go` 的 `GenerateCert` 反推，用 Go 原样复刻后用 `openssl x509 -text` 看：

```
Version: 3            Serial Number: 0
Issuer:  (空)          Subject: (空)
Not Before: <now>     Not After: <now + 24h>
X509v3 extensions:    —— 一个都没有 ——
```

**空 Subject、空 Issuer、零扩展。**

### 1.2 我们的证书

`masque.rs::self_signed_der_for` 现在生成的是：

```
Serial Number: 0
Issuer:  CN=rcgen self signed cert
Subject: CN=rcgen self signed cert
Not Before: Jan  1 00:00:00 1975 GMT
Not After : Jan  1 00:00:00 4096 GMT
X509v3 extensions:
    X509v3 Subject Alternative Name:
        DNS:consumer-masque-proxy.cloudflareclient.com
```

（注意：`CertificateParams::new(vec![cn])` 只设 SAN，不设 CN —— `cn` 参数根本不会进 Subject。Subject 用的是 rcgen 的默认值 `rcgen self signed cert`，所以注释里「CN 已改」的说法是错的。）

### 1.3 逐属性 A/B（每个形态跑 3 次，n=18）

| 证书形态 | Subject | SAN | 有效期 | 结果 |
|---|---|---|---|---|
| UsqueEmpty | 空 | 无 | now..+24h | 200 / 400 / 200 |
| CnOnly | CN=SNI | 无 | now..+24h | 200 / 400 / 200 |
| SanOnly | 空 | 有 | now..+24h | 400 / 200 / 400 |
| **CurrentCode** | CN=rcgen self signed cert | 有 | 1975..4096 | 400 / 200 / 200 |
| CnAndSan | CN=SNI | 有 | now..+24h | 200 / 400 / 200 |
| EmptyWideValidity | 空 | 无 | 1975..4096 | 200 / 200 / 400 |

**每种形态都既有 200 也有 400，顺序随机。** 证书形态与结果无关。

### 1.4 2×2 矩阵（证书 × 请求形态）

| 证书 | 请求 | 结果 |
|---|---|---|
| usque 形态 | usque 普通 CONNECT | **200** |
| 我们的形态 | usque 普通 CONNECT | **400** |
| usque 形态 | 扩展 CONNECT（`:protocol`） | RST_STREAM(1) |
| 我们的形态 | 扩展 CONNECT | RST_STREAM(1) |

400 看起来与证书相关，其实不是——见 §2。

### 1.5 400 的真正来源：anycast 后端分流

对同一个 `162.159.198.2:443` 连续拨号 12 次，记录**服务端证书公钥**与**连接序言 SETTINGS 指纹**：

```
 8x  SETTINGS[0003,0004,0005,0006]  peer=0421a53b313a…  ← 与 pin 匹配
 4x  SETTINGS[0003,0004,0005]       peer=0475bcc0e0c9…  ← 与 pin 不匹配
```

两个后端：

| 后端 | 证书 | SETTINGS | 对 CONNECT-IP 的反应 |
|---|---|---|---|
| A（正确） | `CN=masque.cloudflareclient.com`，Cloudflare 自签根 | 含 0x6 | `200`，隧道可用 |
| B（错误） | `CN=cloudflareclient.com`，Let's Encrypt | 无 0x6 | `400` |

**`162.159.198.2` 是 anycast VIP，约 1/3 的概率落到 B 节点。** 这与证书内容毫无关系。

### 1.6 使用 usque 的本机对照

用 usque 二进制、**开启 pinning**、指向同一 IP：

```
Failed to connect tunnel: … x509: no valid chains built: remote endpoint has a
different public key than what we trust in config.json
… （重试）
Connected to MASQUE server      ← 第 3 次成功
```

**usque 也失败、也重试。** 它靠 `MaintainTunnel` 无限重连（默认 1s 间隔）扛过去。

---

## 二、原始症状的真正来源：`nativecheck` 用的是随机密钥

`masque.rs::build_tls_for_test`（原 `build_h2_tls_config_for_test_inner`）里：

```rust
let mut seed = [0u8; 32];
getrandom::getrandom(&mut seed)?;
let signing = SigningKey::from_bytes(&seed.into())?;   // ← 全新随机密钥
```

而 `native::diagnose()` 用它来建连。这个密钥**从未在 Cloudflare 注册过**。

复现（同一台服务器、同一个账号、同一段代码）：

```
=== RANDOM key（nativecheck 的做法）===
  tls: OK (client cert accepted)
  h2: handshake OK
  connect-ip: FAILED io: received fatal alert: CertificateUnknown

=== ENROLLED key（生产的做法）===
  tls: OK (client cert accepted)
  h2: handshake OK
  connect-ip: OK
```

**逐字复现了原始报错。** 三次运行中，随机密钥三次都复现，真实密钥三次都成功。

机制（这也回答了「为什么是 post-handshake alert」）：

1. TLS 1.3 握手中，Cloudflare **接受任意客户端证书**——它只在 TLS 层确认「有证书」，不做链校验（自签，无从校验）。
2. 握手完成后，HTTP/2 请求带上账号身份，边缘才去查「这张证书的公钥是否属于该账号」。
3. 查不到 → 发 **post-handshake 的致命 alert `certificate_unknown(46)`**。

所以 `tls: OK (client cert accepted)` 这一行本身就是误导——它只证明握手完成，不证明证书被授权。

**`CertificateUnknown` 与 keyUsage / EKU / SAN / 有效期 / 证书链 / certificate_authorities / post_handshake_auth 全都无关。** 这些都是死路。

---

## 三、TLS 配置里真正的可疑点（按严重程度）

### 🔴 P0 — `PinningVerifier` 完全不校验 `CertificateVerify` 签名（MITM 漏洞）

`masque.rs` 里 `verify_tls12_signature` / `verify_tls13_signature` 直接：

```rust
Ok(HandshakeSignatureValid::assertion())
```

Pinning 只回答「这是我要的公钥吗」，**不回答「对端拥有这个私钥吗」**。而证书是公开数据，`endpoint_pub_key` 更是明文写在每个 `usque.json` 里。

实测证明（本地起一个服务端，出示被 pin 的证书，但用**另一个私钥**签名）：

```
PinningVerifier result : HANDSHAKE SUCCEEDED  => CertificateVerify was NOT checked
stock verifier control : rejected as expected
```

**任何能拿到证书副本的人都能冒充边缘、中间人整个隧道。**

修复（已实施并验证）：

```rust
fn verify_handshake_signature(message, cert, dss, tls13) -> Result<HandshakeSignatureValid, Error> {
    let provider = rustls::crypto::ring::default_provider();
    let algs = &provider.signature_verification_algorithms;
    if tls13 { rustls::crypto::verify_tls13_signature(message, cert, dss, algs) }
    else     { rustls::crypto::verify_tls12_signature(message, cert, dss, algs) }
}
```

修复后同一攻击：`rejected (invalid peer certificate: BadSignature)`。

`AnyCertVerifier` 保留 `assertion()` 是正确的——它的存在意义就是「不建立身份」，且只在明确无 pin 的诊断路径使用。

### 🟠 P1 — 缺 pin 时静默降级为 accept-any（fail-open）

```rust
let verifier = match pinned_endpoint_pem {
    Some(pem) if !pem.trim().is_empty() => PinningVerifier,
    _ => AnyCertVerifier,     // ← endpoint_pub_key 为空 = 关掉 pinning
};
```

`usque.json` 缺字段、解析失败、或传空串，都会**静默**变成「信任任何证书」，而不是报错。usque 的 `insecure` 必须显式 `--insecure` 才开启。生产路径应 fail closed：`build_tls_config` / `build_h2_tls_config` 在 pin 为空时返回 `Err`，accept-any 只留给名字里带 `probe`/`test` 的诊断函数。

### 🟡 P2 — `nativecheck` 的 TLS 阶段用随机密钥 → 假阳性

已在 §2 定位。已修：`build_tls_for_test` 改为接收 `&SigningKey`，`diagnose()` 与 `compare_pin()` 都改用账号真实密钥（`compare_pin` 的签名相应加了 `private_key_b64` 参数，`nativecheck.rs` 调用点已更新）。

同时把输出从 `tls: OK (client cert accepted)` 改为 `tls: handshake OK (certificate NOT yet authorised)`，并在 `connect-ip: FAILED … CertificateUnknown` 时附加提示，避免下一个人再被引到证书格式上。

### 🟡 P2 — 无重试，扛不住 anycast 分流

`NativeTunnel::connect` 单次失败即返回，而约 1/3 的拨号会落到错误后端。usque 靠无限重连扛过去。

已修：8 次尝试、每次新 TCP 连接、间隔 1s（对齐 usque 的 `--reconnect-delay` 默认值）。

### 🟢 P3 — 证书本身（低优先级，但不该保留误导性注释）

- `self_signed_der_for` 的注释说「CN 必须是 SNI」——**错的**。`CertificateParams::new(vec![cn])` 只设 SAN，CN 仍是 rcgen 默认的 `rcgen self signed cert`。
- 有效期 `1975..4096` 是 rcgen 默认值，usque 用 24h。实测无影响（§1.3 `EmptyWideValidity` 一样会被接受），但与 usque 对齐更利于排查。
- Subject/Issuer 非空、带 SAN：**无害**。若想与 usque 完全同构，可改为空 DN + 无 SAN + 24h；但这不是修复项，只是减少未来排查的噪声。

**不要加** `clientAuth` EKU、`digitalSignature` keyUsage、`basicConstraints`、`certificate_authorities`、`post_handshake_auth`——服务端不检查，加了也没有用（§1.3 的 C 变体证实）。

### 🟢 P3 — QUIC 与 H2 共用同一个 `PinningVerifier`

修复 P0 后 QUIC 路径自动受益（`build_tls_config` 用的是同一个 verifier）。`WARP_NATIVE=quic` 路径未改动、未破坏。

---

## 四、已实施的改动

| 文件 | 改动 |
|---|---|
| `crates/wg-tunnel/src/masque.rs` | `PinningVerifier` 接入真实签名校验（P0）；`build_tls_for_test` 改用账号真实密钥（P2）；新增 `spki_pem` 辅助函数；新增 2 个回归测试 |
| `crates/wg-tunnel/src/connectip/native.rs` | `diagnose` 用真实密钥 + 修正误导性输出 + 加 `CertificateUnknown` 提示；`compare_pin` 加 `private_key_b64` 参数 |
| `crates/wg-bin/src/bin/nativecheck.rs` | 适配 `compare_pin` 新签名 |
| `crates/wg-tunnel/Cargo.toml` / `Cargo.lock` | 移除排查期间临时加的 `time` dev-dependency |

### 测试

`cargo test --workspace`：**188 passed, 0 failed**（原 171，+2 新回归测试，其余为并发改动带来的增量）。

新增回归测试 `pinning_verifier_rejects_a_forged_handshake_signature` 会**真的发起一次 TLS 握手**：服务端出示被 pin 的证书、用另一个私钥签名。修复前失败（握手被接受），修复后通过（握手被拒）。这条测试锁住了 P0，回退会立刻红。

---

## 五、服务器端到端验证

`nativecheck` 在服务器上重跑（用真实密钥）：

```
key: SEC1/PKCS8 parse OK
tls-config: alpn=[[104, 50]]
tcp: OK -> 162.159.198.2:443
tls: handshake OK (certificate NOT yet authorised)
h2: handshake OK
connect-ip: OK stream=1
tunnel: UP
http: 225 bytes
body: HTTP/1.1 200 OK
```

命中正确后端时**隧道完全可用**，HTTP 请求经隧道返回 200。

同一 IP 上仍会周期性落到错误后端（`B pinned+clientcert: FAILED … UnknownIssuer`），这是 anycast 的固有行为，靠 §3/P2 的重试消化——与 usque 的策略一致。

---

## 六、给下一个人的最短路径

如果又看到 `CertificateUnknown`：

1. **先确认用的是真密钥**，不是 `build_tls_for_test` 之类会生成随机密钥的诊断辅助函数。用 `endpoint_pub_key` 反推 SPKI、与 `private_key` 的 SPKI 比对。
2. **再确认落到的是哪个后端**：看服务端证书是 `CN=masque.cloudflareclient.com`（对）还是 `CN=cloudflareclient.com`（错），以及 SETTINGS 有没有 `0x6`。
3. 如果两者都对还失败，再看 HTTP/2 请求形态（普通 CONNECT vs 扩展 CONNECT）。
4. **最后**才看证书内容——按本次实验结果，它排在最后。
