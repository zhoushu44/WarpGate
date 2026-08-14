# 需求文档完善 Spec

## Why

REQUIREMENTS.md 已覆盖 11 个需求模块，但逐条审查后发现 8 个关键缺口会导致开发时产生歧义或返工，需在开发前补齐。

## What Changes

- 补充公网 IP 返回问题（API 返回 `socks5://127.0.0.1:port` 在公网部署下不可用）
- 补充多容器端口分配策略（方案 C 下 4 容器 × 50 实例的端口分段）
- 补充代理池内存数据结构定义
- 补充并发安全（多请求同时 acquire 的竞态处理）
- 补充方案 C 下 docker run 完整命令示例
- 补充粘性会话超时清理机制（定时器 vs 惰性删除）
- 补充前端 Web UI 鉴权流程
- 补充方案 C 下 IP 去重逻辑（跨实例去重）

## Impact

- Affected code: REQUIREMENTS.md（仅文档修改，不涉及代码）
- Affected specs: 3.1 代理池 API、3.5 方案 C、3.3 配置

## ADDED Requirements

### Requirement: 公网 IP 返回

API acquire 返回的 proxy 地址必须使用服务器公网 IP，不能是 127.0.0.1。

#### Scenario: 公网部署

- **WHEN** 服务部署在公网服务器，客户端远程调用 acquire
- **THEN** 返回 `socks5://<server_public_ip>:10001`，客户端可直接连接
- **AND** 新增配置项 `public_ip`，启动时自动检测或手动配置

### Requirement: 多容器端口分配策略

方案 C 下多容器时，端口分段分配，避免冲突。

#### Scenario: 4 容器 × 50 实例

- **WHEN** 配置 4 个容器，每个 50 个实例
- **THEN** 容器 1 端口 10001-10050，容器 2 端口 10051-10100，容器 3 端口 10101-10150，容器 4 端口 10151-10200
- **AND** 端口分配公式：`base_host_port + container_index * instances_per_container + instance_index`

### Requirement: 代理池内存数据结构

明确定义代理池的内存数据结构。

#### Scenario: 数据结构

- **WHEN** 服务运行
- **THEN** 代理池维护 `pool` Map 和 `sessions` Map
- **AND** pool 存储实例状态：`{ container, instance_index, host_port, container_port, ip, health, status }`
- **AND** sessions 存储会话状态：`{ session_id, pool_key, acquired_at, expires_at }`

### Requirement: 并发安全

多请求同时 acquire 时必须保证不分配同一个实例。

#### Scenario: 并发 acquire

- **WHEN** 两个请求同时调用 acquire
- **THEN** 使用异步锁确保串行分配，不会返回同一个实例
- **AND** 锁粒度为代理池级别，不阻塞其他 API

### Requirement: 粘性会话超时清理

明确超时 session 的清理机制。

#### Scenario: 定时清理

- **WHEN** session 超过 sticky_minutes
- **THEN** 每 30 秒定时扫描 sessions，清理过期 session，归还 IP 到 available
- **AND** 同时支持惰性删除：acquire 时检查 session 是否过期，过期则重新分配

### Requirement: 方案 C docker run 完整命令

提供 docker run 完整命令示例，不依赖 docker-compose。

#### Scenario: 创建容器

- **WHEN** 使用方案 C 创建 1 个容器 20 实例
- **THEN** docker run 命令包含所有必要参数：镜像、端口映射、环境变量、cap_add、资源限制、日志限制
