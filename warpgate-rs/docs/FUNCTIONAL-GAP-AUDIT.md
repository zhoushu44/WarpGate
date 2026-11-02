# Rust 版 vs 原项目：功能差异核对

对照 `server.js`（3161 行，68 条路由）逐项核对。**这是移植过程中最容易漏掉的一类问题：API 能跑通不等于功能齐全。**

## 路由缺口（已补齐）

首轮实现只覆盖了 19 条路由，漏了 6 条：

| 路由 | 方法 | 作用 | 漏掉的后果 |
|---|---|---|---|
| `/api/pool/report` | POST | 客户端上报 HTTP 结果 | **CF 403 率永远是 `--`** |
| `/api/pool/dedicated` | GET | 固定 IP 列表 | 固定 IP 页空白 |
| `/api/pool/dedicated/history` | GET | 绑定历史 | 历史页空白 |
| `/api/pool/dedicated/bind` | POST | 绑定固定 IP | 「固定」按钮无效 |
| `/api/pool/dedicated/unbind` | POST | 解绑 | 「解绑」按钮无效 |
| `/api/pool/dedicated/remark` | POST | 备注 | 无法写备注 |
| `/api/creds/reset` | POST | 重置账密 | 无法轮换密码 |

## CF 403 率：一条完整的因果链

这是最隐蔽的缺口。控制台的「CF 403率」列读取 `cf_403_rate`，而该值只有一个来源：

```
客户端调用 → 拿到 HTTP 403
          → POST /api/pool/report {session_id, result: 403}
          → 该实例 cf_403_count += 1, cf_check_count += 1
          → /api/containers 计算 cf_403_rate = 403数 / 检查数 * 100
          → 控制台显示
```

**没有 `/api/pool/report`，整条链断在第一步**，所以所有实例永远显示 `--`。

三个必须照抄的细节：

1. **`result = 0` 不计入 CF 统计** —— 连接失败是引擎问题，不是 Cloudflare 封禁。混进去会污染 403 率。
2. **连续 2 次连接失败重启引擎** —— 单次多半是网络抖动，两次基本判定引擎僵死。
3. **`cf_403_rate` 为 `null` 时显示 `--`**，为 `0` 时显示 `0%`。两者语义不同。

## 字段名：Go 版和 Node 版控制台不是一套

`/api/containers` 的字段名，Go 版和实际部署的 `public/index.html` **完全不一致**：

| 控制台读取 | Go 版返回 | 结果 |
|---|---|---|
| `container` | `name` | 显示 `undefined` |
| `instance_index` | `index` | 显示 `undefined` |
| `host_port` | `port` | 显示 `--` |
| `pool_status` | — | 显示 `-` |
| `ipv4` / `ipv6` | `ip` / `ipv6` | 两列都是 `未检测到` |
| `ip_mode` | — | 永远显示 `IPv6` |
| `health` | — | 永远显示 `未知` |
| `username`/`password` | — | 永远显示 `免密` |

**根因**：我照着 Go 版实现（Go 是"参考实现"），但实际部署的 HTML 控制台是 Node 版时代的，读的是另一套键名。

修法：**两套键名同时返回**。控制台能用，按 Go 写的 API 客户端也不会坏。已有测试 `containers_carry_the_console_field_names` 锁住这个契约。

## IPv4 出口：不能只探 IPv6

老项目 `checkExitIP` 返回 `{ip, v4, v6}`，两个协议族都探。控制台有独立的「IPv4 出口」列。

原本只探 IPv6，导致：

- 「IPv4 出口」永远显示 `未检测到`
- `ip_mode` 永远是 `ip6`
- 少数只有 IPv4 出口的实例（Cloudflare 偶尔如此）整行显示为坏

`ip_mode` 的判定照抄 Node：

```js
ip_mode: (ipv4 && !ipv6) ? 'ip4' : 'ip6'
```

## 出口 IP 探测：走 80 端口

配置里是 `https://api6.ipify.org`，但探测发的是**明文 HTTP/1.1**。直接连 443 会因为缺少 TLS 握手挂到超时，表现为「所有实例出口 IP 都是空」。

回显服务在 80 和 443 上响应相同，所以探测统一降到 80：

```rust
fn parse_http_target(url: &str) -> Option<String> {
    // ... 显式指定端口则保留，否则一律 :80
    Some(format!("{}:80", authority))
}
```

这样探测路径不引入 TLS 栈，也不占用隧道的关键路径。

## 账密 vs 白名单：两个端口，两种语义

| 端口 | 规则 | 谁用 |
|---|---|---|
| `10010+i` | 必须带 `warpuser:test-pass-123` | 任意 IP |
| `11010+i` | 免密，靠源 IP 放行 | 白名单内 |

控制台「账密」列显示 `warpuser:test-pass-123`（点击可复制）；只有 `proxy_auth_enabled=false` 时才显示「免密」。

`/api/proxies` 的 `proxy` 字段**必须跟随 `port` 字段**：白名单模式下两者都用白名单端口。regiforge 的 `wary` provider 按 `proxy` → `proxy_whitelist` 顺序读取，若 `proxy` 返回账密端口而调用方不知道账密，就会报 `no acceptable authentication methods`。

## 核对方法

```bash
# Node 版全部路由
grep -oE "^app\.(get|post|put|delete)\('[^']+'" server.js | sort

# Rust 版全部路由
grep -oE '\.route\("[^"]+"' crates/wg-apiserver/src/routes.rs | sort
```

两边的差集就是缺口。**每次改动 API 后都应重跑这个对比。**
