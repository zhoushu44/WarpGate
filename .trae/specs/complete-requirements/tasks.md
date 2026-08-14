# Tasks

- [x] Task 1: 补充公网 IP 返回问题到 REQUIREMENTS.md
  - [x] 在 3.1.3 API 响应中，proxy 字段从 `socks5://127.0.0.1:10001` 改为 `socks5://<public_ip>:10001`
  - [x] 在配置结构中新增 `public_ip` 配置项（auto-detect 或手动指定）
  - [x] 补充说明：本地部署时 public_ip = 127.0.0.1，公网部署时为服务器公网 IP

- [x] Task 2: 补充多容器端口分配策略到 REQUIREMENTS.md
  - [x] 在方案 C 改造方案中新增"端口分配公式"小节
  - [x] 说明多容器场景下端口分段：容器 i 的实例 j 端口 = base_host_port + i * instances_per_container + j
  - [x] 补充 4 容器 × 50 实例的端口分配示例表

- [x] Task 3: 补充代理池内存数据结构到 REQUIREMENTS.md
  - [x] 在 3.1.4 轮询与锁定机制后新增"内存数据结构"小节（3.1.6）
  - [x] 定义 pool Map 的 key 格式和 value 结构
  - [x] 定义 sessions Map 的 key 格式和 value 结构
  - [x] 说明状态流转：available → in_use → available

- [x] Task 4: 补充并发安全到 REQUIREMENTS.md
  - [x] 在 3.1.7 中补充并发安全说明
  - [x] 说明使用异步锁确保 acquire 串行执行
  - [x] 说明锁粒度（代理池级别）和锁释放时机

- [x] Task 5: 补充粘性会话超时清理机制到 REQUIREMENTS.md
  - [x] 在 3.1.2 中补充超时清理机制说明
  - [x] 定时清理：每 30 秒扫描，清理过期 session
  - [x] 惰性删除：acquire 时检查 session 是否过期
  - [x] 新增配置项 `session_cleanup_interval`（默认 30 秒）

- [x] Task 6: 补充方案 C docker run 完整命令到 REQUIREMENTS.md
  - [x] 在方案 C 改造方案中，补充 docker run 完整命令示例
  - [x] 包含：镜像、端口范围映射、WARP_INSTANCES、cap_add、--cpus、--memory、--log-opt、--restart
  - [x] 与现有代码的 docker run 命令对比说明差异

- [x] Task 7: 补充前端 Web UI 鉴权流程到 REQUIREMENTS.md
  - [x] 在 3.6.1 中补充前端鉴权说明
  - [x] 说明 Web UI 打开时弹出 API Key 输入框
  - [x] API Key 存储在 localStorage，每次请求携带 X-API-Key header
  - [x] 修改 API Key 后更新 localStorage

- [x] Task 8: 补充方案 C 下 IP 去重逻辑到 REQUIREMENTS.md
  - [x] 在 3.2 IP 检测中补充跨实例 IP 去重（3.2.4）
  - [x] 说明：创建实例后检测 IP，如果与池中已有 IP 重复，触发该实例重注册
  - [x] 去重范围：整个代理池（跨容器跨实例）
  - [x] 最大重试次数：与 ip_check_retries 一致

# Task Dependencies
- All tasks are independent, can be done in parallel
