# 部署与运维

## systemd 托管

服务由 systemd 管理，**不要**再用 `nohup` / `at` / `setsid` 后台启动。

```bash
systemctl status warpgate      # 查看状态
systemctl restart warpgate     # 重启（部署后执行这个）
journalctl -u warpgate -f      # 跟日志
```

单元文件：`/etc/systemd/system/warpgate.service`

```ini
[Service]
WorkingDirectory=/root/warpgate-rs/app
Environment=WARP_NATIVE=quic
LimitNOFILE=65535
ExecStart=.../warpgate --config ... --instances ...
Restart=always
```

`LimitNOFILE=65535` 是必需的：500 实例约需 2000 个文件描述符，默认的 1024 不够。

## 部署流程

```bash
# 1. 本地构建并上传（会重新编译）
./deploy-upload.ps1

# 2. 重启
ssh root@host 'systemctl restart warpgate'

# 3. 验证
curl -s -H 'X-API-Key: <key>' http://127.0.0.1:4433/api/stats
```

**为什么用 systemd**：之前用 `at now + 1 minute` 和 `setsid nohup ... &` 启动，在 SSH 会话里经常静默失败——部署脚本 `pkill` 了旧进程，新进程又没起来，服务就停了。systemd 把"启动"从部署步骤里彻底分离。

## 账号池

实例数与账号数必须匹配。扩容：

```bash
# 注册到 N 个账号（8 秒间隔，Cloudflare 限流）
./target/release/growpool /root/warpgate-rs/instances 500 8000
```

**Cloudflare 注册有速率限制**：6 并发会立刻收到 429（`error code: 1015`），串行每 8 秒一个可以无限跑。

## usque.json 格式

两个字段的格式是硬约束，写错 `usque` 会拒绝启动：

| 字段 | 正确格式 | 错误格式的症状 |
|---|---|---|
| `private_key` | **SEC1** DER，base64 后以 `MHc` 开头 | `use ParsePKCS8PrivateKey instead` |
| `endpoint_pub_key` | **SPKI** PEM，91 字节 DER | `asn1: structure error: tags don't match` |

Cloudflare 的 `PATCH /reg/<id>` 返回的 `public_key` 是**裸 65 字节曲线点**，不是 SPKI。直接包 PEM 外壳会得到一个能 base64 解码但 ASN.1 解析失败的文档。

`endpoint_pub_key` 是 Cloudflare 的**服务端证书**，所有账号相同，可以安全地写死或用模板替换。

## 已知的坑

**子进程变僵尸**
`usque` 用 `kill_on_drop(false)` 启动后，如果没有 `wait()` 回收，退出时会留下僵尸进程。500 个僵尸之后整池看起来是死的（进程在，但端口不监听）。`ExternalEngine::start` 现在有一个 reaper 任务专门 `child.wait()`。

**stdout/stderr 必须排空**
`Stdio::piped()` 而不读，管道缓冲区满了子进程会阻塞在写操作上，永远绑不了端口。

**启动期间控制台必须可用**
`boot()` 串行 attach 500 个实例要 10 分钟以上。它现在跑在后台任务里，HTTP 服务先绑定——否则部署完打开页面会看到"打不开"，误以为失败。

## 界面字段契约

`public/index.html` 是 Node 时代留下的单文件控制台，它读的字段名与 Go 版 API **不一致**。两者必须同时返回，否则页面显示全是 `undefined` 或 `0`：

| 接口 | 控制台要的 | Go 版叫法 |
|---|---|---|
| `/api/stats` | `running_containers`, `total_instances`, `available`, `in_use`, `unhealthy`, `unique_ips`, `dedicated`, `sessions` | `resident_count`, `instance_count`, `healthy_count` |
| `/api/containers` | `container`, `instance_index`, `host_port`, `pool_status`, `ipv4`, `ipv6`, `ip_mode`, `health`, `username`, `password` | `name`, `index`, `port`, `status` |
| `/api/whitelist` | `ips`, `client_ip` | `whitelist` |
| `/api/pool/dedicated` | `items` | 裸数组 |

`/api/usage` 返回 `{days: [...], system: {...}}`，`system` 里是 CPU / 内存 / 网络速率，从 `/proc` 读。
