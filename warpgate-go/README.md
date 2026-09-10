# WarpGate-Go（嵌入式单进程版）

Node 版 [server.js](../server.js) 的 Go 重写：控制面与 MASQUE 数据面**同进程**，每实例从「sing-box + usque 两个进程」变为「进程内一组对象」，MASQUE 模式无需切回 WireGuard。

- 需求与验收标准：[REQUIREMENTS-GO.md](REQUIREMENTS-GO.md)
- 数据面：[usque](https://github.com/Diniboy1123/usque)（Go，MIT）的 `api` 包进程内嵌入，`L4Proxy` 提供 MASQUE CONNECT-IP 隧道
- 与 Node 版共享 `config.json` 语义与实例目录格式（`warp-<i>/usque.json`、`meta.json`），可原地互换

## 构建

```bash
cd warpgate-go
go build -o warpgate .          # linux/amd64: GOOS=linux GOARCH=amd64 CGO_ENABLED=0 go build -o warpgate .
```

需要 Go ≥ 1.26.3（低版本工具链会自动下载）。

## 运行

```bash
# 完全复用 Node 版的 config.json 与实例目录（暖启动已有账号）
./warpgate --config /root/warp/config.json
# 实例目录解析顺序：--instances > 环境变量 WARP_INSTANCES_DIR > config.instances_dir > ./instances
```

管理台与 Node 版同端口语义（`config.port`，默认 4433），API 路径兼容（`/api/login`、`/api/stats`、`/api/pool/acquire` 等，详见需求 §3 FR-5）。

## 测试

```bash
go test ./...                    # 离线单测 + 控制面端到端（SOCKS5/调度/API，注入假隧道）
WARP_IT=1 go test -tags integration -run TestIntegrationCreateAndTunnel -v ./internal/pool/ -timeout 300s
                                 # 真实注册 + 真实 MASQUE 隧道冒烟（需能直连 Cloudflare WARP 端点的网络）
```

> 注意：企业/家宽网络若封锁 Cloudflare WARP 边缘（UDP 443），集成测试会以 `timeout: no recent network activity` 失败——这与官方 `usque l4-socks` 行为一致，属环境限制而非代码问题。请在可连通的生产服务器（当前 Node 版能跑 MASQUE 的机器）上执行。

## Docker

```bash
docker build -t zhoushu1/warp-pool-go:1.0 .
docker run -d --name warp-go --network host --restart unless-stopped \
  -v /root/warp-instances:/root/warp-instances \
  -v /root/warp/config.json:/app/config.json \
  zhoushu1/warp-pool-go:1.0
```

## 已知边界（v1）

- `warp_transport` 仅支持 `masque`；`masque_http2`（TCP 443 兜底）未实装，启动时告警忽略
- SOCKS5 仅 TCP CONNECT（UDP ASSOCIATE 明确拒绝 0x07）——与 Node 版 sing-box 配置行为一致
- dedicated 固定 IP 绑定、scamalytics 风控值、多机 SSH 管理为 v2
- usque `api` 包按 go.mod 锁定 commit `6aa03fc`，升级需回归测试
