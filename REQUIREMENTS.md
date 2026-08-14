# WARP 容器管理系统 — 需求文档

## 一、项目概述

基于 Cloudflare WARP + Docker 的 SOCKS5 代理池管理系统，部署在公网服务器上，通过 API 提供代理提取服务。核心用途：为 ChatGPT 批量注册脚本提供独立 IP 代理池。

### 使用场景

- 注册脚本通过 API 提取一个代理地址（SOCKS5），使用完毕后释放
- 粘性会话确保注册流程中多步请求使用同一 IP
- 每个 IP 用完后可销毁容器并创建新容器换取新 IP
- 公网部署，多脚本/多用户远程调用

---

## 二、现有功能（已完成）

| 功能 | 说明 |
|------|------|
| Web 管理界面 | 容器列表、状态、IP 显示，支持搜索/全选 |
| 批量创建 | 一次创建 N 个 WARP 容器，可确保 IP 唯一性（最多重试 10 次/个） |
| 批量删除 | 删除选中/全部容器 |
| 批量重建 | 重建容器直到换到新 IP（最多 20 次重试） |
| 容器管理 | 单个启动/停止/删除/重建 |
| 任务队列 | 异步执行批量任务，前端轮询进度和日志 |
| 统计面板 | 总容器数、运行中、已停止、独立 IP 数 |

---

## 三、新增需求

### 3.1 代理池提取 API（核心）

#### 3.1.1 鉴权

- 所有代理池 API 需要 **API Key** 鉴权
- 请求头：`X-API-Key: <key>`
- API Key 通过环境变量或配置文件设置，启动时生成默认 Key 并打印到控制台
- 无 Key 或 Key 错误返回 `401 Unauthorized`

#### 3.1.2 粘性会话（参照商用代理标准）

采用 **Session ID 绑定模式**（参照 Bright Data / Smartproxy 等商用代理的粘性会话定义）：

```
1. 客户端调用提取 API，可选传入 session_id
   - 不传 session_id → 系统生成一个 UUID 返回
   - 传了 session_id → 复用已有会话

2. 系统从可用 IP 池中轮询取出一个 IP，绑定到该 session_id
   - 该 session_id 在粘性窗口内始终返回同一个 IP
   - 粘性窗口可配置：5 分钟 / 10 分钟 / 30 分钟（默认 10 分钟）

3. 窗口超时后，IP 自动归还到可用池，session 失效
4. 客户端可主动调用释放 API，立即归还 IP
5. 同一 session_id 在窗口内重复调用，返回相同的代理地址
6. 超时清理机制（双重保障）：
   - **定时清理**：每 `session_cleanup_interval` 秒（默认 30 秒）扫描 sessions Map，清理过期 session，归还 IP 到 available 池
   - **惰性删除**：acquire 时检查传入的 session_id 是否已过期，过期则视为新 session 重新分配
   - 定时清理日志：`清理 N 个过期 session，归还 N 个 IP`
```

#### 3.1.3 API 接口定义

**提取代理**

```
GET /api/pool/acquire?session_id=<可选>&sticky_minutes=<可选>
Header: X-API-Key: <key>
```

响应：
```json
{
  "success": true,
  "data": {
    "session_id": "a1b2c3d4-...",
    "proxy": "socks5://<public_ip>:10001",
    "ip": "104.28.x.x",
    "expires_at": "2026-08-13T10:30:00Z",
    "sticky_minutes": 10
  }
}
```

无可用代理时：
```json
{
  "success": false,
  "error": "无可用代理，请稍后重试"
}
```

**释放代理**

```
POST /api/pool/release
Header: X-API-Key: <key>
Body: { "session_id": "a1b2c3d4-..." }
```

响应：
```json
{
  "success": true,
  "data": { "session_id": "a1b2c3d4-...", "released_ip": "104.28.x.x" }
}
```

**查看池状态**

```
GET /api/pool/status
Header: X-API-Key: <key>
```

响应：
```json
{
  "success": true,
  "data": {
    "total": 20,
    "available": 15,
    "in_use": 5,
    "sessions": [
      { "session_id": "a1b2c3d4-...", "ip": "104.28.x.x", "port": 10001, "acquired_at": "...", "expires_at": "..." }
    ]
  }
}
```

#### 3.1.4 轮询与锁定机制

- 可用 IP 池维护一个指针，轮询分配（round-robin），避免总是分配同一个
- IP 被提取后标记为 `in_use`，不会被分配给其他 session
- 超时或主动释放后，IP 回到 `available` 状态
- 如果可用池为空，返回错误（不做排队等待，客户端自行重试）

#### 3.1.5 公网 IP 返回

- API 返回的 `proxy` 字段使用服务器公网 IP，不能是 `127.0.0.1`（公网部署时客户端无法连接本地地址）
- 配置项 `public_ip`：
  - `"auto-detect"`（默认）— 启动时自动检测公网 IP（通过 `curl https://api.ipify.org`）
  - 手动指定 — 如 `"203.0.113.50"`
  - `"127.0.0.1"` — 本地开发模式
- 如果自动检测失败，回退到 `127.0.0.1` 并打印警告日志

#### 3.1.6 代理池内存数据结构

代理池维护两个内存 Map：

**pool Map**（key = `container_index:instance_index`）

```javascript
{
  container: "warp-0",           // Docker 容器名
  instance_index: 0,             // 实例序号
  host_port: 10001,              // 宿主机端口
  container_port: 40000,         // 容器内端口
  ip: "104.28.x.x",              // 出口 IP（检测后填充）
  health: "healthy",             // healthy | unhealthy | unknown
  status: "available"            // available | in_use
}
```

**sessions Map**（key = `session_id`）

```javascript
{
  session_id: "a1b2c3d4-...",
  pool_key: "0:0",               // 对应 pool Map 的 key
  acquired_at: 1693521000000,    // 获取时间戳
  expires_at: 1693521600000      // 过期时间戳
}
```

**状态流转**：

```
pool.status:  available  ──acquire──→  in_use  ──release/timeout──→  available
pool.health:  unknown  ──检测通过──→  healthy  ──检测失败──→  unhealthy  ──重建成功──→  healthy
```

#### 3.1.7 并发安全

- 多请求同时调用 `acquire` 时，使用**异步锁**确保串行分配，不会返回同一个实例
- 锁粒度：代理池级别（一个全局锁），不阻塞 `/api/pool/status` 等读操作
- 锁实现：简单的 Promise 队列，acquire 开始时获取锁，分配完成后释放
- 其他 API（release、status、rotate）不需要锁（release 仅修改 session 状态，status 仅读取）

---

### 3.2 IP 可用性检测

#### 3.2.1 检测内容

两层检测，按顺序执行：

| 层级 | 检测内容 | 方法 | 超时 |
|------|---------|------|------|
| L1 连通性 | 代理能正常联网 | `curl --proxy socks5h://127.0.0.1:port https://api.ipify.org` | 10s |
| L2 OpenAI 可访问性 | 代理能访问 OpenAI 服务 | `curl --proxy socks5h://127.0.0.1:port -s -o /dev/null -w "%{http_code}" https://chatgpt.com` | 15s |

- L1 通过才执行 L2
- L2 返回 200/302/403 均视为"可访问"（403 可能是区域限制但 IP 本身可用）
- L2 返回连接超时/拒绝连接视为"不可访问"

#### 3.2.2 检测时机

| 时机 | 动作 |
|------|------|
| 容器创建后 | 立即检测，不通过则重建（现有逻辑已有 IP 检测，需增加 L2） |
| 定时巡检 | 每 10 分钟对所有运行中容器执行 L1+L2 检测 |
| 检测失败 | 自动触发重建，最多重试 3 次，仍失败则标记为 `unhealthy` 并从代理池移除 |

#### 3.2.3 健康状态

每个容器增加 `health` 字段：

- `healthy` — L1 + L2 均通过，可分配
- `unhealthy` — L2 不通过，不可分配，等待重建
- `unknown` — 尚未检测

#### 3.2.4 跨实例 IP 去重（方案 C 专属）

方案 C 下多个实例可能分配到相同的 Cloudflare IP，需要跨实例去重：

1. 每个实例创建后检测出口 IP（L1）
2. 将 IP 与代理池中已有实例的 IP 对比
3. 如果 IP 重复，触发该实例重注册（`warp-cli registration delete && new`）
4. 重注册后重新检测 IP，最多重试 `ip_check_retries` 次（默认 10 次）
5. 超过重试次数仍重复，标记该实例为 `unhealthy`，从代理池移除
6. 去重范围：整个代理池（跨容器、跨实例）

```
实例创建 → L1 检测 IP → 查重
  ├─ IP 唯一 → 加入代理池（status=available, health=healthy）
  ├─ IP 重复 → 重注册实例 → 重新检测（最多 10 次）
  └─ 10 次仍重复 → 标记 unhealthy，不加入代理池
```

---

### 3.3 可配置常驻容器数量

#### 3.3.1 前端 UI

- 顶部统计栏增加一个可编辑输入框："常驻容器数：[ 20 ]"
- 修改后点击"应用"按钮，立即生效
- 如果新数量 < 当前运行数，自动停止并删除多余的容器（从端口最大的开始删）
- 如果新数量 > 当前运行数，自动创建不足的容器

#### 3.3.2 后端

- `/api/config` GET 获取当前配置，POST 修改配置
- 配置项持久化到 `config.json` 文件，服务重启不丢失
- 最大值限制由 `MAX_CONTAINERS`（默认 200）兜底

配置结构：
```json
{
  "max_containers": 200,
  "resident_count": 20,
  "sticky_minutes": 10,
  "auto_rebuild_on_unhealthy": true,
  "health_check_interval": 600,
  "api_key": "auto-generated-uuid",
  "bind_address": "127.0.0.1",
  "mode": "resident",
  "warp_image": "dublok/cloudflare-warp:latest",
  "warp_instances_per_container": 20,
  "base_host_port": 10001,
  "base_container_port": 40000,
  "container_prefix": "warp-",
  "min_available": 5,
  "max_concurrent_create": 3,
  "ip_check_url": "https://api.ipify.org",
  "openai_check_url": "https://chatgpt.com",
  "ip_check_retries": 10,
  "ip_check_interval": 3000,
  "max_rebuild_attempts": 3,
  "container_cpu_limit": 0.5,
  "container_memory_limit": "128m",
  "cleanup_on_exit": false,
  "rate_limit_enabled": true,
  "rate_limit_per_minute": 60,
  "log_level": "info",
  "public_ip": "auto-detect",
  "session_cleanup_interval": 30
}
```

---

### 3.4 动态模式（待测 — 第二阶段）

#### 3.4.1 核心逻辑

每个 IP 用完一次后销毁容器，立刻创建新容器换 IP：

```
客户端释放代理(session_id)
  → 销毁该 session 对应的容器
  → 立即创建新容器补位
  → 新容器 IP 检测通过后加入可用池
```

#### 3.4.2 与常驻模式的关系

- **常驻模式**（默认）：N 个容器始终运行，IP 池轮询分配，超时归还，定时巡检
- **动态模式**（可选开启）：释放即销毁 + 立即补位，保证每次提取都是新 IP
- 两种模式通过配置开关切换：`"mode": "resident" | "dynamic"`

#### 3.4.3 动态模式参数

```json
{
  "mode": "dynamic",
  "min_available": 5,
  "max_concurrent_create": 3
}
```

- `min_available`：可用池最少保持几个，低于此值触发批量创建
- `max_concurrent_create`：同时创建容器的并发数，防止资源爆发

#### 3.4.4 标记为待测的原因

- 需要验证 WARP 容器创建+IP检测的耗时（估计 15-30s/个）
- 需要验证高频创建/销毁对 Docker 和系统资源的压力
- 需要验证能否满足注册脚本的并发需求

---

### 3.5 IP 不变问题分析与解决方案（核心问题）

#### 3.5.1 问题现象

当前代码 `docker rm -f` + `docker run` 重建容器后，出口 IP 经常与之前相同，无法可靠获取新 IP。

#### 3.5.2 根因分析

通过研究开源项目和 Cloudflare WARP 机制，确认以下原因：

| 原因 | 说明 |
|------|------|
| **WARP IP 分配机制** | WARP 的出口 IP 由 Cloudflare 根据连接的边缘节点（edge node）分配，同一台服务器始终连接同一个边缘节点，从同一个 IP 池中分配 |
| **IP 池有限** | 每个边缘节点的 IP 池有限，且多人共享（NAT），重建容器只是重新注册一个 WARP 账号，但出口 IP 可能仍然相同 |
| **注册数据未清除** | `caomingjun/warp` 镜像将注册数据存在 `/var/lib/cloudflare-warp`，如果挂载了 volume，重建后复用同一注册 → 同一 IP（当前代码未挂载 volume，但 IP 仍可能相同） |
| **warp-svc 守护进程** | 官方守护进程重连时倾向于复用之前的连接参数，不会主动更换 WireGuard 密钥 |

#### 3.5.3 三个解决方案对比

---

## 方案 C：ErcinDedeoglu 多实例单容器（重点测试方案）

### 来源

[ErcinDedeoglu/cloudflare-warp](https://github.com/ErcinDedeoglu/cloudflare-warp) — 镜像 `dublok/cloudflare-warp:latest`

### 核心原理

在**单个容器内运行 N 个独立的 warp-svc 实例**，每个实例：
- 独立注册（`warp-cli registration new`）→ 获得独立设备身份 → 分配不同 Cloudflare IP
- 使用 `STATE_DIRECTORY` 环境变量隔离数据目录（`/var/lib/cloudflare-warp/instance-{i}`）
- 使用 `RUNTIME_DIRECTORY` 环境变量隔离 IPC socket（`/run/warp-{i}`）
- 每个实例有独立的 D-Bus daemon
- 每个实例监听独立端口（`40000 + i`）
- **不需要 `--privileged`，不需要 `SYS_ADMIN`**

### 架构图

```
┌─────────────────────────────────────────────────────────┐
│  单个 Docker 容器 (dublok/cloudflare-warp)               │
│                                                         │
│  ┌─────────┐  ┌─────────┐  ┌─────────┐      ┌────────┐ │
│  │warp-svc │  │warp-svc │  │warp-svc │ ...  │warp-svc│ │
│  │ inst-0  │  │ inst-1  │  │ inst-2  │      │inst-N  │ │
│  │:40000   │  │:40001   │  │:40002   │      │:4000N  │ │
│  │IP-A     │  │IP-B     │  │IP-C     │      │IP-X    │ │
│  └────┬────┘  └────┬────┘  └────┬────┘      └───┬────┘ │
│       │            │            │                │      │
│       ▼            ▼            ▼                ▼      │
│  ┌─────────────────────────────────────────────────┐    │
│  │              GOST 代理层 (round-robin)           │    │
│  │    :1080 (SOCKS5)  :8080 (HTTP)  :8388 (SS)     │    │
│  └─────────────────────────────────────────────────┘    │
│                                                         │
│  ※ 我们绕过 GOST，直接映射各实例端口到宿主机             │
└─────────────────────────────────────────────────────────┘
```

### 为什么最贴合本项目需求

| 需求 | 方案 C 如何满足 |
|------|----------------|
| **每个代理有独立 IP** | 每个实例独立注册 → 不同设备身份 → 不同 IP |
| **粘性会话（指定 IP）** | 绕过 GOST round-robin，直接访问实例端口 40000+i → 固定到某个 IP |
| **换 IP** | 对某个实例执行 `warp-cli registration delete && registration new` → 新身份 → 新 IP |
| **资源效率** | 1 个容器跑 20 个实例 vs 20 个容器，省去容器开销 |
| **IPv6 支持** | 使用官方 warp-svc，完整 IPv4+IPv6 双栈 |
| **稳定性** | 官方 warp-svc 守护进程，比 wireproxy 更稳定 |
| **不需要特权** | 不需要 `--privileged`，仅需 `NET_ADMIN` + `MKNOD` |

### 源码分析要点

**1. 实例隔离机制**（`start-warp-instance.sh`）

```bash
# 每个实例使用独立的数据目录和运行时目录
DATA_DIR="/var/lib/cloudflare-warp/instance-${INSTANCE}"
RUN_DIR="/run/warp-${INSTANCE}"

# 独立的 D-Bus daemon
sudo dbus-daemon --address="unix:path=${DBUS_SOCK}" ...

# 通过环境变量让 warp-svc 使用独立路径
sudo env STATE_DIRECTORY="$DATA_DIR" RUNTIME_DIRECTORY="$RUN_DIR" \
    warp-svc --accept-tos &

# 通过环境变量让 warp-cli 操作对应实例
wcli() {
    sudo env RUNTIME_DIRECTORY="$RUN_DIR" \
        DBUS_SYSTEM_BUS_ADDRESS="unix:path=${DBUS_SOCK}" \
        warp-cli --accept-tos "$@"
}
```

**2. 独立注册流程**

```bash
# 每个实例独立注册（不是复用）
if [ ! -f "${DATA_DIR}/reg.json" ]; then
    wcli registration new          # 新设备身份
    wcli mode proxy                # 代理模式
    wcli proxy port "$PORT"        # 独立端口 40000+i
    wcli connect                   # 连接 WARP
fi
```

**3. 注册失败重试**（指数退避 + 随机抖动）

```bash
for attempt in $(seq 1 15); do
    REG_OUT=$(wcli registration new 2>&1) && break || {
        BACKOFF=$(( (1 << attempt) + RANDOM % (1 << attempt) ))
        [ "$BACKOFF" -gt 120 ] && BACKOFF=120
        sleep "$BACKOFF"
    }
done
```

**4. 实例启动间隔**（防止 API 限流）

```bash
for i in $(seq 0 $((WARP_INSTANCES - 1))); do
    /start-warp-instance.sh "$i" "$PORT" ... &
    sleep $((5 + RANDOM % 5))  # 5-9秒随机间隔
done
```

**5. 优雅关闭**（注销设备，避免占用 WARP+ 设备配额）

```bash
cleanup() {
    for i in $(seq 0 $((WARP_INSTANCES - 1))); do
        wcli registration delete  # 注销设备
    done
}
```

### 适配本项目的改造方案

原项目用 GOST 做 round-robin 轮询，不适用于粘性会话。改造方案：

**改造 1：暴露每个实例的独立端口**

docker-compose 方式：

```yaml
# docker-compose.yml
services:
  warp:
    image: dublok/cloudflare-warp:latest
    ports:
      - "10001-10020:40000-40019"  # 映射 20 个实例端口
    environment:
      - WARP_INSTANCES=20
    cap_add:
      - NET_ADMIN
      - MKNOD
```

docker run 方式（与现有代码一致，推荐）：

```bash
docker run -d \
  --name warp-0 \
  --restart unless-stopped \
  --cap-add NET_ADMIN \
  --cap-add MKNOD \
  --cpus 0.5 \
  --memory 128m \
  --memory-swap 128m \
  --log-opt max-size=10m \
  --log-opt max-file=3 \
  -p 10001-10020:40000-40019 \
  -e WARP_INSTANCES=20 \
  dublok/cloudflare-warp:latest
```

与现有代码 docker run 的差异对比：

| 参数 | 现有代码（caomingjun/warp） | 方案 C（dublok/cloudflare-warp） |
|------|---------------------------|-------------------------------|
| 镜像 | `caomingjun/warp` | `dublok/cloudflare-warp:latest` |
| 端口映射 | `-p 10001:1080`（单端口） | `-p 10001-10020:40000-40019`（端口范围） |
| 环境变量 | 无 | `-e WARP_INSTANCES=20` |
| 特权 | `--cap-add NET_ADMIN` | `--cap-add NET_ADMIN --cap-add MKNOD` |
| 资源限制 | 无 | `--cpus 0.5 --memory 128m --memory-swap 128m` |
| 日志限制 | 无 | `--log-opt max-size=10m --log-opt max-file=3` |

**改造 2：代理池直接管理实例端口**

不再用 GOST round-robin，代理池 API 直接分配实例端口：

```
acquire(session_id) → 返回 socks5://<public_ip>:10001 (实例0, IP-A)
acquire(session_id) → 返回 socks5://<public_ip>:10002 (实例1, IP-B)
```

**改造 2.1：多容器端口分配公式**

多容器场景下，端口分段分配避免冲突：

```
容器 i 的实例 j 的宿主端口 = base_host_port + i * warp_instances_per_container + j
容器 i 的实例 j 的容器端口 = base_container_port + j
```

示例（4 容器 × 50 实例）：

| 容器 | 实例范围 | 宿主端口范围 | 容器端口范围 |
|------|---------|-------------|-------------|
| warp-0 | 0-49 | 10001-10050 | 40000-40049 |
| warp-1 | 0-49 | 10051-10100 | 40000-40049 |
| warp-2 | 0-49 | 10101-10150 | 40000-40049 |
| warp-3 | 0-49 | 10151-10200 | 40000-40049 |

注意：每个容器内的实例端口都是从 `base_container_port` 开始（40000+i），因为它们在各自独立的网络命名空间内。宿主端口则全局唯一。

docker run 端口映射参数（容器 i）：

```bash
# 容器 0：映射 10001-10050 → 40000-40049
docker run -p 10001-10050:40000-40049 ...

# 容器 1：映射 10051-10100 → 40000-40049
docker run -p 10051-10100:40000-40049 ...

# 容器 2：映射 10101-10150 → 40000-40049
docker run -p 10101-10150:40000-40049 ...
```

**改造 3：实例级 IP 轮换**

不需要重建容器，在容器内对单个实例重注册：

```bash
# 对实例 i 执行换 IP
docker exec <container> bash -c '
  RUN_DIR="/run/warp-0"
  DBUS_SOCK="/run/dbus-0/system_bus_socket"
  sudo env RUNTIME_DIRECTORY="$RUN_DIR" DBUS_SYSTEM_BUS_ADDRESS="unix:path=$DBUS_SOCK" \
    warp-cli registration delete
  sudo env RUNTIME_DIRECTORY="$RUN_DIR" DBUS_SYSTEM_BUS_ADDRESS="unix:path=$DBUS_SOCK" \
    warp-cli registration new
  sudo env RUNTIME_DIRECTORY="$RUN_DIR" DBUS_SYSTEM_BUS_ADDRESS="unix:path=$DBUS_SOCK" \
    warp-cli mode proxy
  sudo env RUNTIME_DIRECTORY="$RUN_DIR" DBUS_SYSTEM_BUS_ADDRESS="unix:path=$DBUS_SOCK" \
    warp-cli proxy port 40000
  sudo env RUNTIME_DIRECTORY="$RUN_DIR" DBUS_SYSTEM_BUS_ADDRESS="unix:path=$DBUS_SOCK" \
    warp-cli connect
'
```

**改造 4：容器布局**

| 方案 | 容器数 | 每容器实例数 | 总 IP 数 | 预估内存 |
|------|--------|-------------|---------|---------|
| 1 容器 20 实例 | 1 | 20 | 20 | 1-2GB |
| 1 容器 50 实例 | 1 | 50 | 50 | 2.5-5GB |
| 2 容器 50 实例 | 2 | 50 | 100 | 5-10GB |
| 4 容器 50 实例 | 4 | 50 | 200 | 10-20GB |

### 资源占用预估

| 指标 | 单实例 | 20 实例 | 50 实例 |
|------|--------|---------|---------|
| 内存 | ~50-100MB | 1-2GB | 2.5-5GB |
| CPU 空闲 | ~0.1-0.3% | 2-6% | 5-15% |
| 启动时间 | ~10s | 100-180s（错开启动） | 250-450s |

### 方案 C 优势 vs 其他方案

| 对比项 | 方案 C（Ercin） | 方案 A（wgcf） | 方案 B（caomingjun） |
|--------|----------------|---------------|---------------------|
| 换 IP 成功率 | 待测（预计高） | 待测（预计高） | 待测（预计中） |
| 资源效率 | 最高（1容器N实例） | 低（1容器1实例） | 最低（warp-svc 占用高） |
| IPv6 支持 | 是 | 否 | 是 |
| 注册限流风险 | 低（warp-cli） | 高（wgcf API） | 低 |
| 粘性会话适配 | 需改造（绕过 GOST） | 原生支持 | 原生支持 |
| 换 IP 速度 | 快（容器内操作 5-10s） | 快（5-10s） | 慢（重建容器 20-30s） |
| 镜像成熟度 | 高（124 commits，CI/CD） | 中（50 commits） | 高（成熟项目） |
| 稳定性 | 高（官方 warp-svc） | 中（wireproxy 偶有断连） | 高（官方 warp-svc） |

---

## 方案 A：wgcf + wireproxy（备选方案）

### 来源

[kschen202115/warp-proxy-docker](https://github.com/kschen202115/warp-proxy-docker)

### 核心原理

- 使用 `wgcf` 工具每次注册生成全新 WARP 账号（新 WireGuard 密钥对）
- 使用 `wireproxy` 替代官方 `warp-svc`，资源占用极低
- 换 IP 流程：删除旧配置 → `wgcf register` → `wgcf generate` → 重启 wireproxy

### 换 IP 流程

```
1. 删除旧的 wgcf-account.toml 和 wgcf-profile.conf
2. wgcf register --accept-tos  → 注册新账号，获得新密钥
3. wgcf generate               → 生成新 WireGuard 配置
4. 重启 wireproxy               → 新隧道连接，大概率获得不同 IP
```

### 优缺点

- 优点：每次重建 = 全新身份，换 IP 成功率预计最高；CPU/内存占用极低
- 缺点：仅支持 IPv4 出口；wgcf 注册有频率限制（频繁注册可能被限流 "Too many requests"）
- 风险：项目作者警告 CPU 占满问题，需限制 CPU（`--cpus=0.5`）

---

## 方案 B：caomingjun/warp 内重注册（最小改动方案）

### 核心原理

不换镜像，在重建时通过 `docker exec` 执行 warp-cli 重注册：

```bash
warp-cli registration delete
warp-cli registration new
# 或 rm -rf /var/lib/cloudflare-warp/* && docker restart <name>
```

### 优缺点

- 优点：不换镜像，改动最小
- 缺点：换 IP 成功率预计最低（同一边缘节点同一 IP 池）；warp-svc 重注册行为不确定

---

#### 3.5.4 实施计划（更新）

| 步骤 | 方案 | 测试项 | 通过标准 |
|------|------|--------|---------|
| 1 | **方案 C** | T7-C: 启动 1 容器 10 实例，检测各实例 IP 唯一性 | ≥7/10 实例 IP 不同 |
| 2 | **方案 C** | T8-C: 对单实例执行重注册，检测 IP 是否变化 | ≥70% 换 IP 成功 |
| 3 | **方案 C** | T9-C: 单容器 20 实例的资源占用（CPU/内存） | 内存 ≤2GB，CPU ≤10% |
| 4 | **方案 A** | T7-A: wgcf 连续注册 10 次，统计不同 IP 数 | ≥7/10 IP 不同 |
| 5 | **方案 A** | T9-A: wgcf 1 分钟内注册 20 次，观察限流 | 记录限流阈值 |
| 6 | **方案 B** | T8-B: warp-cli 重注册 10 次，统计不同 IP 数 | 对比方案 A/C |

**决策逻辑**：
- 方案 C 通过 T7-C + T8-C → 直接采用方案 C，不再测试 A/B
- 方案 C 未通过 → 测试方案 A，A 通过 → 采用方案 A
- 方案 A 未通过 → 测试方案 B，作为兜底

#### 3.5.5 镜像选择变更

| 配置项 | 当前值 | 方案 C | 方案 A |
|--------|--------|--------|--------|
| `WARP_IMAGE` | `caomingjun/warp` | `dublok/cloudflare-warp` | 自构建 `warp-wireproxy` |
| 容器:实例比 | 1:1 | 1:N | 1:1 |
| 代理端口 | 1080（GOST） | 40000+i（各实例独立） | 1080（wireproxy） |
| 换 IP 方式 | docker rm + run | warp-cli registration delete + new | wgcf register + 重启 wireproxy |
| IPv6 支持 | 是 | 是 | 否 |
| 特权要求 | `NET_ADMIN` | `NET_ADMIN` + `MKNOD` | 无 |

#### 3.5.6 新增 API：强制换 IP

```
POST /api/pool/rotate
Header: X-API-Key: <key>
Body: { "session_id": "a1b2c3d4-..." }  # 可选，不传则对所有实例执行
```

- 方案 C：对容器内单个实例执行 `warp-cli registration delete + new`
- 方案 A：删除 wgcf 配置 + 重新 register + 重启 wireproxy
- 不删除容器，比 `docker rm + run` 快得多（5-10s vs 20-30s）
- 执行后重新检测 IP，更新代理池

---

### 3.6 安全加固

#### 3.6.1 API Key 鉴权

- 代理池 API（`/api/pool/*`）必须携带 API Key
- 管理API（`/api/create`、`/api/delete`、`/api/rebuild`）也加 API Key
- 查询API（`/api/containers`、`/api/stats`、`/api/check`）可选鉴权（配置项控制）

**前端 Web UI 鉴权流程**：

1. 用户打开 Web UI 时，检查 `localStorage.getItem('warp_api_key')`
2. 如果没有 Key，弹出输入框提示输入 API Key
3. 用户输入后，Key 存储到 `localStorage`，所有后续 API 请求携带 `X-API-Key` header
4. 如果 API 返回 `401 Unauthorized`，清除 localStorage 中的 Key，重新弹出输入框
5. 页面右上角显示当前 API Key（脱敏显示，如 `a1b2****-****-****`），点击可修改
6. 首次启动时，API Key 打印到控制台，用户从控制台复制到 Web UI 输入框

#### 3.6.2 输入校验

- 容器名称只允许 `warp-\d+` 格式，拒绝任何特殊字符
- 端口号必须为数字且在 `BASE_PORT+1` ~ `BASE_PORT+MAX_CONTAINERS` 范围内
- 所有 `docker` 命令的参数使用白名单校验，防止命令注入

#### 3.6.3 绑定地址

- 新增配置项 `bind_address`，默认 `127.0.0.1`（仅本地）
- 公网部署时手动设为 `0.0.0.0`，并配合防火墙限制来源 IP

#### 3.6.4 API 限流

- 公网部署必须限流，防止 API 被滥用
- 采用简单计数限流：每个 API Key 每分钟最多 60 次请求
- 超限返回 `429 Too Many Requests`，响应头 `Retry-After: 60`
- 限流配置项：

```json
{
  "rate_limit_enabled": true,
  "rate_limit_per_minute": 60
}
```

---

### 3.7 日志系统

#### 3.7.1 问题

当前代码大量 `catch (e) {}` 吞掉错误，无任何日志输出，排查问题困难。

#### 3.7.2 需求

| 日志类型 | 说明 | 输出方式 |
|---------|------|---------|
| 请求日志 | 每个 API 请求的方法、路径、状态码、耗时 | 控制台 + 文件 |
| 错误日志 | 所有 catch 块的错误信息、堆栈 | 控制台 + 文件 |
| 容器操作日志 | 创建/删除/重建/换 IP 的操作记录 | 控制台 + 文件 |
| 健康检测日志 | L1/L2 检测结果、失败原因 | 控制台 + 文件 |

- 日志格式：`[2026-08-13 10:30:00] [INFO] [MODULE] message`
- 日志文件：`logs/app-2026-08-13.log`，按天轮转，保留 7 天
- 日志级别可配置：`debug | info | warn | error`（默认 `info`）
- 所有 `catch (e) {}` 替换为 `catch (e) { logger.error('module', e); }`

---

### 3.8 容器资源限制

#### 3.8.1 Docker 资源限制

创建容器时必须设置资源上限，防止单个实例拖垮宿主机：

```bash
docker run \
  --cpus=0.5 \          # 单实例最多 0.5 核
  --memory=128m \       # 单实例最多 128MB 内存
  --memory-swap=128m \  # 禁止 swap
  --restart=unless-stopped \
  ...
```

#### 3.8.2 配置项

```json
{
  "container_cpu_limit": 0.5,
  "container_memory_limit": "128m"
}
```

#### 3.8.3 日志限制

Docker 容器日志设置上限，防止磁盘撑满：

```bash
docker run \
  --log-opt max-size=10m \
  --log-opt max-file=3 \
  ...
```

---

### 3.9 服务启动恢复逻辑

#### 3.9.1 问题

当前服务重启后，内存中的 `jobs`、代理池状态全部丢失。已有的 Docker 容器仍在运行但系统不知道它们的存在。

#### 3.9.2 需求

服务启动时执行恢复流程：

```
1. 扫描 Docker 中所有 warp-* 容器
2. 对每个运行中容器，检测各实例的 IP 和健康状态
3. 重建代理池内存状态（available / in_use）
4. 如果运行中容器数 < resident_count，自动创建补齐
5. 如果运行中容器数 > resident_count，删除多余的
6. 启动定时健康巡检定时器
7. 日志输出恢复结果："恢复 N 个容器，M 个实例，K 个可用 IP"
```

#### 3.9.3 优雅关闭

服务收到 `SIGINT`/`SIGTERM` 时：

| 配置项 | 行为 |
|--------|------|
| `cleanup_on_exit: false`（默认） | 仅停止接受新请求，容器保持运行，下次启动时恢复 |
| `cleanup_on_exit: true` | 停止所有容器（`docker stop`），注销 WARP 设备，退出 |

配置项：
```json
{
  "cleanup_on_exit": false
}
```

---

### 3.10 端口冲突检测

#### 3.10.1 问题

当前 `getNextAvailablePort()` 只检查已分配端口，不检测宿主机端口是否被其他进程占用，导致 `docker run -p` 可能失败。

#### 3.10.2 需求

分配端口前增加宿主机端口占用检测：

```javascript
// 方案：尝试 TCP 连接，端口已被占用则跳过
const net = require('net');
function isPortAvailable(port) {
  return new Promise((resolve) => {
    const tester = net.createServer()
      .once('error', () => resolve(false))
      .once('listening', () => { tester.close(); resolve(true); })
      .listen(port);
  });
}
```

- 分配端口时跳过被占用的端口
- 记录日志：`端口 10005 被占用，跳过`

---

### 3.11 统一 API 响应格式

#### 3.11.1 成功响应

```json
{
  "success": true,
  "data": { ... }
}
```

#### 3.11.2 错误响应

```json
{
  "success": false,
  "error": {
    "code": "NO_AVAILABLE_PROXY",
    "message": "无可用代理，请稍后重试"
  }
}
```

#### 3.11.3 错误码定义

| 错误码 | HTTP 状态码 | 说明 |
|--------|------------|------|
| `UNAUTHORIZED` | 401 | API Key 缺失或错误 |
| `RATE_LIMITED` | 429 | 请求频率超限 |
| `NO_AVAILABLE_PROXY` | 503 | 代理池为空 |
| `SESSION_NOT_FOUND` | 404 | session_id 不存在或已过期 |
| `INVALID_INPUT` | 400 | 参数校验失败 |
| `CONTAINER_ERROR` | 500 | Docker 操作失败 |
| `IP_CHECK_FAILED` | 503 | IP 检测失败 |
| `INTERNAL_ERROR` | 500 | 未知错误 |

---

## 四、待测试项（Docker 就绪后执行）

| 编号 | 测试内容 | 验证目标 |
|------|---------|---------|
| T1 | 创建单个 WARP 容器，检测出口 IP | 验证基础功能可用 |
| T2 | 通过 WARP 代理访问 `https://chatgpt.com` | 验证 WARP IP 是否被 OpenAI 封锁 |
| T3 | `docker stats` 查看单容器 CPU/内存 | 推算 20/50/200 容器资源需求 |
| T4 | 批量创建 5 个容器，检查 IP 唯一性 | 验证 IP 去重逻辑 |
| T5 | 高频创建/销毁 10 次容器，观察资源波动 | 为动态模式提供数据支撑 |
| T6 | WARP 容器创建到可用的耗时 | 评估动态模式可行性 |
| T7 | 方案 C（Ercin 多实例）：1 容器 10 实例，检测各实例 IP 唯一性 | 验证多实例是否真的分配不同 IP |
| T8 | 方案 C：对单实例执行 warp-cli 重注册，检测 IP 是否变化 | 验证实例级换 IP 可行性 |
| T9 | 方案 C：单容器 20 实例的资源占用 | 验证 20 实例内存 ≤2GB |
| T10 | 方案 A（wgcf+wireproxy）换 IP 成功率：连续注册 10 次 | 对比方案 C |
| T11 | wgcf 注册频率限制测试：1 分钟内注册 20 次 | 评估方案 A 动态模式上限 |
| T12 | 方案 B（warp-cli 重注册）换 IP 成功率：连续 10 次 | 对比方案 A/C |

---

## 五、技术栈

| 组件 | 技术 |
|------|------|
| 后端 | Node.js + Express |
| 前端 | 原生 HTML/CSS/JS（单文件） |
| 容器 | Docker + dublok/cloudflare-warp 镜像（方案 C 优先）或 caomingjun/warp（方案 B） |
| 代理协议 | SOCKS5（官方 warp-svc proxy 模式，各实例端口 40000+i） |
| WARP 注册 | warp-cli CLI 工具（方案 C/B）或 wgcf（方案 A） |
| 配置存储 | config.json 文件持久化 |
| 部署环境 | Windows / Linux 公网服务器 |

---

## 六、开发优先级

| 优先级 | 模块 | 依赖 |
|--------|------|------|
| P0 | 换 IP 方案测试（T7/T8/T9 优先方案 C） | Docker 就绪 |
| P0 | 日志系统（logger.js + 替换所有 catch(e){}） | 无 |
| P0 | 安全加固（API Key + 输入校验 + API 限流） | 无 |
| P0 | 统一 API 响应格式 + 错误码 | 无 |
| P0 | IP 可用性检测（L1 + L2 + 自动重建） | 换 IP 方案确定 |
| P0 | 代理池提取 API（acquire/release/status/rotate） | IP 检测完成 |
| P0 | 服务启动恢复逻辑 | 代理池 API 完成 |
| P1 | 可配置常驻容器/实例数量（前端 UI + 后端配置） | 无 |
| P1 | 容器资源限制（--cpus/--memory/--log-opt） | 无 |
| P1 | 端口冲突检测 | 无 |
| P1 | 优雅关闭（SIGINT/SIGTERM） | 无 |
| P1 | 项目 README 文档 | 功能基本定型 |
| P2 | 动态模式（用完销毁 + 立即补位） | T7/T8/T9 测试通过 |

---

## 七、现有代码改造范围

### 7.1 术语变更

| 原术语 | 新术语（方案 C） | 说明 |
|--------|-----------------|------|
| 容器 (container) | 容器 (container) | 物理 Docker 容器，1 个容器内含 N 个实例 |
| — | 实例 (instance) | 容器内的 warp-svc 进程，每个实例 = 1 个独立 IP |
| 端口 (port) | 宿主端口 → 实例端口 | `10001` → `40000`，`10002` → `40001`... |

### 7.2 需修改的文件

| 文件 | 改造内容 |
|------|---------|
| `server.js` | 1. 添加 API Key 中间件<br>2. 新增 `/api/pool/*` 代理池 API<br>3. 新增 `/api/config` 配置 API<br>4. 新增 `/api/pool/rotate` 换 IP API<br>5. 容器管理逻辑从 1 容器 1 IP 改为 1 容器 N 实例<br>6. 新增实例级 IP 检测（L1+L2）<br>7. 新增定时健康巡检定时器<br>8. 输入校验（容器名、端口号白名单）<br>9. bind_address 配置支持<br>10. 日志系统（替换所有 catch(e){}）<br>11. 服务启动恢复逻辑（扫描已有容器）<br>12. 优雅关闭（SIGINT/SIGTERM 处理）<br>13. API 限流中间件<br>14. 统一错误响应格式<br>15. 端口冲突检测（net.createServer 探测）<br>16. Docker 资源限制（--cpus/--memory/--log-opt） |
| `public/index.html` | 1. 顶部增加常驻实例数输入框 + 应用按钮<br>2. 容器列表改为实例列表（显示容器名+实例号+IP+健康状态）<br>3. 增加"代理池状态"面板<br>4. 增加 API Key 输入框（管理 API 调用时携带）<br>5. 增加"强制换 IP"按钮 |
| `start.bat` | 1. 移除自动打开浏览器（公网部署不需要）<br>2. 启动前检查 config.json 是否存在，不存在则生成默认配置<br>3. 打印 API Key 到控制台 |
| `config.json`（新增） | 持久化配置，首次启动自动生成 |
| `logger.js`（新增） | 日志模块，按天轮转，控制台+文件双输出 |

### 7.3 新增的文件

| 文件 | 用途 |
|------|------|
| `config.json` | 配置持久化 |
| `logger.js` | 日志模块 |
| `REQUIREMENTS.md` | 本文档 |
| `README.md` | 项目说明文档（P1 优先级） |

---



## 八、已知风险

| 风险 | 影响 | 应对 |
|------|------|------|
| WARP IP 被 OpenAI 封锁 | 整个方案不可用 | T2 测试验证，若被封需换方案 |
| WARP IP 池有限 | 200 个容器可能只拿到几十个独立 IP | T4/T7 测试验证实际独立 IP 数 |
| 重建后 IP 不变 | 代理池无法提供独立 IP | 方案 A/B/C 测试（T7/T8），选成功率最高的 |
| wgcf 注册被限流 | 动态模式无法高频换 IP | T9 测试限流阈值，设置合理的注册间隔 |
| 高频创建/销毁 Docker 卡死 | 动态模式不可行 | T5 测试验证，必要时降级为常驻模式 |
| 公网暴露 Docker 管理接口 | 安全风险 | API Key + 输入校验 + API 限流 + 防火墙 |
| 服务重启任务丢失 | 批量任务中断 | 服务启动恢复逻辑（3.9）扫描已有容器重建状态 |
| 服务重启代理池状态丢失 | 已分配的 session 失效 | 启动时重建池状态，session 不持久化（客户端需重新 acquire） |
| WARP 出口 IP 为机房 IP | 部分网站识别并封锁 | 已知限制，需 T2 验证目标网站是否接受 |
| Docker 容器日志撑满磁盘 | 服务不可用 | --log-opt max-size=10m max-file=3 |
| 端口被宿主机其他进程占用 | 容器创建失败 | 端口冲突检测（3.10），自动跳过占用端口 |
