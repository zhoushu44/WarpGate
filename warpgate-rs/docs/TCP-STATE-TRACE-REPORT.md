# TCP 状态机追踪报告：数据在哪里消失

**结论：数据一行都没发出去。丢包点在 `stack.rs::connect()` 的返回条件——`is_active()` 在 `SynSent` 就已经是 `true`，于是 `connect()` 把一条还没开始握手的连接交给了 SOCKS relay。relay 第一次 `poll_read` 看到 `may_recv()==false`，把它当成"对端正常关闭"返回 `Ok(0)`，并顺手 `sock.close()` 把 socket 从 `SynSent` 打成 `Closed`。此后客户端写进来的 56 字节 HTTP 请求在 `poll_write` 里拿到 `BrokenPipe`，一次 `send_slice` 都没有发生。**

假设判定：**假设 A 成立（且比你写的更早一步）**，假设 B、C 均不成立。

---

## 1. 实验设置

生产环境不动（`WARP_FORCE_ENGINE=1`，500 个 usque 子进程，**当前实际走的是 usque 引擎路径，不是 native**——`ss -tnp | grep :443` 里 18000+ 条连接全部属于 `usque` 进程，`warpgate` 自己 0 条）。测试全部在独立目录 `/root/wgtrace-src`（源码副本 + 独立 target）完成，跑完已删除。

源码基线：`git show HEAD:crates/wg-tunnel/src/connectip/{stack,h2}.rs`（即线上 v0.9.0 的含 bug 版本），只加日志，不改逻辑，用于复现；另建一份加修复的用于对照。

新增最小测试 `crates/wg-bin/src/bin/tracetest.rs`：

```
WG_STACK_TRACE=1 tracetest <usque.json> [target:port] [local_port] [mode]
mode: socks(默认) | writefirst | load<N> | many<N> <secs>
```

`socks` 模式就是任务要求的 **1 个隧道 + 1 个 SOCKS5 连接 + 1 个 HTTP 请求**，走 `NativeTunnel → UserspaceStack → wg_socks::Server` 全链路，和生产的组装方式一致。

---

## 2. 完整状态追踪日志（含 bug 版本，按时间排序）

`WG_STACK_TRACE=1 tracetest .../usque.json example.com:80 39001 socks`

```
[      0.000ms] pump#1  did_work=false inbound 0->0 outbound 0->0 sockets=[]
...                                    (隧道空闲，53 次 poll，一切正常)
[    105.244ms] connect: begin handle=SocketHandle(0) local_port=49152 remote=172.66.147.243:80
[    105.326ms] connect: iter=0 state=SynSent is_active=true can_send=false can_recv=false may_send=false may_recv=false txq=0 rxq=0
[    105.357ms] connect: STATE TRANSITION <new> -> SynSent after 0 iters
[    105.375ms] connect: RETURN state=SynSent is_active=true can_send=false can_recv=false may_recv=false  <-- 就是这里交出去的
[    105.490ms] poll_read:  ENTER handle=SocketHandle(0) state=SynSent can_recv=false may_recv=false rxq=0 want=8192
[    105.514ms] poll_read:  EOF   handle=SocketHandle(0) state=SynSent may_recv=false rxq=0  <-- 返回 Ok(0) 给 relay
[    105.535ms] poll_read:  called sock.close(), state now=Closed              <-- socket 被自己打死
[    105.640ms] poll_write: ENTER handle=SocketHandle(0) state=Closed want=56 can_send=false txspace=65536
[    105.679ms] poll_write: state=Closed -> BrokenPipe (56 bytes discarded)    <-- 请求字节在这里消失
```

计数器（`trace::summary()`）：

```
pumps=53 tx_pkts=0 tx_bytes=0 rx_pkts=0 rx_bytes=0
send_calls=0 send_bytes=0 recv_calls=0 recv_bytes=0 read_eofs=1
```

客户端侧：

```
[client] greeting reply = [05, 00]
[client] CONNECT reply  = [05, 00, 00, 01, 7f, 00, 00, 01, 00, 50]   (REP=0，SOCKS 握手"成功")
[client] write_all returned Ok                                        (写进了内核缓冲，不是隧道)
[client] read_to_end = Ok(Ok(())), 0 bytes, first line = ""           (立刻 EOF，不是超时)
```

复现率：buggy 版 **2/2 得 0 字节**；修复版 **3/3 得 888 字节**，20 连发 **20/20 成功**。

---

## 3. 逐条回答任务里的问题

| 问题 | 答案 |
|---|---|
| TCP 握手什么时候完成的？SynSent → Established | **`connect()` 返回之后才完成**。日志里 `connect: RETURN state=SynSent`，之后再无 `iter=1`——循环已经退出了。握手在随后被 `close()` 打断，从未到达 Established。（修复版里能看到：`iter=0..7` 全是 SynSent，`iter=8` 才 `STATE TRANSITION SynSent -> Established`，耗时约 27 ms。） |
| `connect()` 返回时状态是什么？ | **`SynSent`**，`can_send=false can_recv=false may_send=false may_recv=false`。 |
| 第一次 write 时 `can_send()` 是什么？ | **`false`**。而且此时状态已经是 `Closed`（被 `poll_read` 的 `close()` 打掉了），所以 `poll_write` 直接返回 `BrokenPipe`，连 `Pending` 都没走到。 |
| `send_slice` 返回了多少？ | **没被调用过。** `send_calls=0`、`send_bytes=0`。56 字节在 `poll_write` 入口就被丢弃。 |
| 数据包有没有进 `outbound` 队列？ | **没有。** `tx_pkts=0`、`tx_bytes=0`；`device.transmit` 一次都没触发（没有数据要发，SYN 也还没来得及发）。 |
| `Tunnel::send_packet` 有没有被调用？ | **没有。** `h2.send_packet` 零次。 |
| 对端有没有回包？`inbound` 有没有数据？ | **没有，也永远不会有了。** `rx_pkts=0`，`inbound` 全程 0。SYN 根本没出去。 |

所以三个假设里：

- **假设 A**：方向对，但触发点更靠前。不是"write 的数据被缓存但 SYN 没发"，而是 **`connect()` 在 `SynSent` 就返回 → `poll_read` 把"还没握手"误判成"对端关闭" → `close()` 把 socket 打死 → write 直接 BrokenPipe**。数据连 `send_slice` 都没进。
- **假设 B**（`send_slice` 成功但 outbound 没人消费）：**不成立**。outbound 消费者（reader task）工作正常，日志里 `device.transmit → outbound 0->1 → reader: Tunnel::send_packet → h2.send_packet` 链路完整。真正跑通时（write-first 对照）这条链路一次能送 3 个包 188 字节。
- **假设 C**（`Device::receive` 的 `try_lock` 失败丢包）：**不成立**。`Device::receive` 的 `try_lock` 只在 `pump_once` 不持队列锁时调用（代码里已刻意把 `poll` 放在锁外），日志中 `device.receive` 每次都能拿到包（`inbound_left` 正常递减）。加了 `else` 分支专门统计丢包，全程 `rx_dropped=0`。

---

## 4. 数据消失的确切位置

```
wg_socks::relay
  └─ tokio::io::copy_bidirectional(&mut client, &mut upstream)
       ├─ 先 poll 读方向：upstream.poll_read()   ← 这里返回 Ok(0)
       │     └─ StackStream::poll_read
       │           sock.can_recv() == false
       │           sock.may_recv() == false        ← SynSent 里也是 false（"没收到东西"
       │                                              被当成了"对端不再发"）
       │           → Poll::Ready(Ok(()))           ← 冒充 EOF
       │           → sock.close()                  ← SynSent → Closed，连接彻底报废
       └─ 写方向：client → upstream 的 56 字节
             └─ StackStream::poll_write
                    state == Closed → Err(BrokenPipe)   ← 【数据在这里消失】
```

**根本原因是一行判定：**

```rust
// stack.rs::connect()  (v0.9.0)
if sock.is_active() { break; }        // ← 错
```

smoltcp 0.11 的定义（`socket/tcp.rs:967`）：

```rust
pub fn is_active(&self) -> bool {
    match self.state {
        State::Closed => false,
        State::TimeWait => false,
        State::Listen => false,
        _ => true,                    // ← SynSent 也是 true
    }
}
```

而 `TcpSocket::connect()` 是**同步**把状态设成 `SynSent` 的（`socket/tcp.rs:866`），所以 `connect()` 的等待循环**第一轮就必然命中**——这个"等待握手"的循环实际上从来没等过任何东西。它唯一会等的情况是 socket 一进来就 `Closed`。

第二个错误放大了它（`StackStream::poll_read`）：

```rust
if !sock.can_recv() {
    if !sock.may_recv() {        // ← SynSent/SynRecv 也是 false
        if sock.state() != Closed { sock.close(); }
        return Poll::Ready(Ok(()));   // ← 冒充"对端优雅关闭"
    }
    ...
}
```

`may_recv()` 的语义是"**还能不能收到**"，不是"**对端有没有关**"。握手没完成时它天然是 false，这里却把它当成 EOF，还顺手 `close()` 把 socket 打死。这就是为什么 SOCKS 侧看到的是**立刻 EOF 而不是超时**——`copy_bidirectional` 把 `Ok(0)` 当成对端干净关闭，直接收工。

`nativecheck` 之所以能成功，纯粹是**时序侥幸**：它 `write_all` 在前（write 返回 Pending、把 waker 注册进去），4 ms 后 SYN 发出、对端回 SYN-ACK、pump 把 socket 推到 Established，write 才被唤醒。而 `copy_bidirectional` 是**先 poll 读**，正好踩在第一轮 `poll_read` 上，于是必死。这也解释了"单跑成功、走 SOCKS 就 0 字节"的全部现象。

---

## 5. 修复方案（已实现并验证）

`crates/wg-tunnel/src/connectip/stack.rs`，两处：

**① `connect()` 用真正的握手完成判定，而不是 `is_active()`**

```rust
/// `is_active()` is `!matches!(state, Closed | TimeWait | Listen)`, so `SynSent`
/// already counts as active — it cannot gate "handshake done".
fn handshake_done(state: tcp::State) -> bool {
    matches!(state,
        tcp::State::Established | tcp::State::FinWait1 | tcp::State::FinWait2
        | tcp::State::CloseWait | tcp::State::Closing | tcp::State::LastAck)
}

// connect():
if handshake_done(state) { break; }
if state == tcp::State::Closed { /* 原来的 ConnectionRefused 路径 */ }
```

`CloseWait` 等状态也算完成：对端可能在两次 poll 之间就关了，那是"活着但带 FIN"，不是连接失败。

**② `poll_read()` 只在真正结束时才报 EOF**

```rust
fn recv_finished(state: tcp::State) -> bool {
    matches!(state,
        tcp::State::Closed | tcp::State::CloseWait | tcp::State::LastAck
        | tcp::State::TimeWait | tcp::State::Closing
        | tcp::State::FinWait1 | tcp::State::FinWait2)
}

if !sock.may_recv() {
    if !recv_finished(sock.state()) {
        // 握手还没完成：等 pump，不要冒充 EOF
        self.shared.register(cx.waker());
        return Poll::Pending;
    }
    ... // 原来的 EOF 路径
}
```

**③（另一条独立缺陷，建议一并修）`Tunnel::recv_packet` 不是 cancel-safe**

`UserspaceStack::attach` 的 reader task 用 `timeout(20ms, recv_packet())` 来保证 outbound 能及时冲刷。`recv_packet` 内部是 `read_exact(9)` + `read_exact(len)`：**超时在帧中间触发时，已经读掉的字节随 future 一起丢掉**，下一次调用就会把 payload 当成长度读，报 `frame of N bytes exceeds SETTINGS_MAX_FRAME_SIZE`，reader task 退出 → `Tunnel` 被丢弃 → 底层 TCP 关闭。这正是"隧道起来一会儿后 ESTAB 归零"的机制。

已用单元测试钉住（不依赖网络）：

```rust
#[tokio::test]
async fn recv_packet_survives_cancellation_mid_frame() {
    // 900 字节 DATAGRAM 分两半投递，中间插一次 20ms 超时取消
    // 断言：第二次 recv_packet 必须拿到完整的 900 字节，而不是错位/报错
}
```

修复思路：把已读字节留在 `self` 里（`raw: Vec<u8>` + `pending_header`），`read_frame_header` 在 payload 未消费前重复返回同一个 header，`read_payload` 消费完才清空。这样被取消的调用下次从同一帧边界续读。

> 说明：仓库当前 `h2.rs` 已由并行改动的 `fill_raw` / `pending_header` 覆盖了这个思路，我的独立验证分支结论与此一致。

---

## 6. 验证结果

| 场景 | buggy（HEAD） | 修复后 |
|---|---|---|
| 1 隧道 + 1 SOCKS + 1 HTTP（`socks` 模式） | **0 字节**（2/2） | **888 字节**（3/3） |
| 同一隧道连发 20 个请求 | 0/20 | **20/20** |
| reader task 因超时取消而错位 | 报 `frame of 4539717 bytes...` 后隧道失聪 | 单测通过，可续读 |
| `cargo test --workspace` | 189 passed | **194 passed / 0 failed** |

修复后的关键日志（同一份插桩，只换了判定）：

```
[    105.564ms] connect: iter=0 state=SynSent is_active=true can_send=false can_recv=false ...
[    132.416ms] connect: iter=8 state=Established is_active=true can_send=true can_recv=false may_send=true may_recv=true txq=0 rxq=0
[    132.446ms] connect: STATE TRANSITION SynSent -> Established after 8 iters
[    132.471ms] connect: RETURN state=Established can_send=true can_recv=false
[    132.850ms] poll_write: ENTER state=Established want=56 can_send=true txspace=65536
[    132.915ms] poll_write: send_slice(56) -> Ok(56) state=Established txq=56 (buffered)
[    134.065ms] device.transmit len=96 outbound_queued=2
[    150.581ms] h2.send_packet len=40 stream=1 stream_sent=54 conn_window=17760203
[    164.303ms] reader: Tunnel::recv_packet len=928 first_byte=0x45
[    165.992ms] poll_read: ENTER state=CloseWait can_recv=true rxq=888
[    166.030ms] poll_read: recv_slice -> Ok(888)
[    166.153ms] poll_read: EOF state=CloseWait may_recv=false rxq=0     ← 这次才是真 EOF
```

对照 write-first（`nativecheck` 的形态，buggy 版本也能成功）的完整链路，证明 outbound/inbound 都没问题：

```
[     3.616ms] connect: RETURN state=SynSent ...       ← 同样是错的返回
[     4.404ms] device.transmit len=52 outbound_queued=1 ← SYN 进队列
[    19.913ms] h2.send_packet len=52 stream=1          ← SYN 发出
[    23.399ms] reader: Tunnel::recv_packet len=52      ← SYN-ACK 回来
[    25.687ms] pump#14 sockets=["... Established txq=56 rxq=0"]  ← 握手完成
[    44.870ms] h2.send_packet len=96                   ← 请求发出
[    54.526ms] reader: Tunnel::recv_packet len=928      ← 响应回来
```
（同一份含 bug 的代码：write-first 靠"写方向先注册 waker"侥幸躲过，read-first 必死。）

---

## 7. 关于症状 C（500 条隧道全断）

这一条**没能按原样复现**，原因是环境事实与描述不符：

- 线上进程 `warpgate` **当前没有任何到 Cloudflare 的 TCP 连接**；`:443` 上 18000+ 条 ESTAB 全部属于 `usque` 子进程。也就是说生产此刻跑的是 `WARP_FORCE_ENGINE=1` 的 usque 引擎路径，native 隧道并没有在跑，`connect-ip ok, stack up` 只可能出现在独立的 nativecheck / 测试进程里。
- 我在独立副本里做了两组验证：20 条隧道空闲 60 s → **21 ESTAB 全程保持**；80 条隧道空闲 120 s → **81 ESTAB 全程保持**，`rx_dropped=0`，无 reader 退出。
- 8 个进程并发各 15 次请求 → **120/120 成功**，无 reader 死亡。

所以"空闲就把隧道放死"这个现象在 native 栈上不成立。**但**：第 5 节 ③ 的 cancel-safety 缺陷是真实的、可单测复现的，它会让 reader task 在负载下偶发退出并带走整条隧道——如果曾经观察到 native 隧道成批消失，这是唯一说得通的机制，建议按 ③ 修掉。

另外提醒一个容易被误读的点：`ss -tnp | grep warpgate | grep -c ESTAB = 0` 在**当前生产配置下是正常的**——native 隧道由 `warpgate` 进程内的 tokio 任务持有，套接字属于 `warpgate` 进程；而 usque 路径下每个实例是独立子进程，套接字属于 `usque`。用 `grep warpgate` 去数 usque 的连接，本来就永远是 0。

---

## 8. 交付物 / 变更清单

代码（`crates/wg-tunnel/src/connectip/`）：

- `stack.rs`：`connect()` 每轮打印 `state / is_active / can_send / can_recv / may_send / may_recv / txq / rxq`；`poll_write` 打印目标字节数、`can_send()`、`send_slice` 返回值；`poll_read` 打印 `can_recv/may_recv/recv_slice` 返回值；`pump_once` 打印 poll 前后 `inbound`/`outbound` 队列长度与各 socket 状态；`Device::receive/transmit` 打印包长与队列深度。
- 日志受 `WG_STACK_TRACE=1` 控制（`connectip/trace.rs`），**默认关闭，不影响生产**。

最小测试：`crates/wg-bin/src/bin/tracetest.rs`（`socks` / `writefirst` / `load<N>` / `many<N>` 四种模式）。

测试：`cargo test --workspace` → **194 passed, 0 failed**（新增 5 个：`syn_sent_is_active_but_not_connected`、`recv_finished_*`、`recv_packet_survives_cancellation_mid_frame` 等）。

生产安全：未重启、未 `pkill usque`、未改 systemd unit；所有测试在 `/root/wgtrace-src` 独立 target 下完成，跑完已 `rm -rf`，`/root` 下无残留，磁盘回到 86%。
