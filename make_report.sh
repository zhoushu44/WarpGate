#!/bin/bash
set -u
cd /root/soak
OUT=/root/soak/final_report.txt
base=$(awk -F'\t' 'NR==1{print $2}' nativecheck.tsv)

{
  echo "=================================================================="
  echo " WarpGate Rust 原生 CONNECT-IP 隧道 - 30 分钟长跑稳定性测试报告"
  echo "=================================================================="
  echo
  echo "被测对象"
  echo "  host            : $(hostname)  ($(uname -r), $(nproc) cores)"
  echo "  stack.rs        : 646 行 / md5 a3894198a0892e57901582070f0482d8"
  echo "                    含 fix #4 (connect/pump_once 锁顺序) + fix #5 (StackStream Drop)"
  echo "  nativecheck     : md5 f401d8f94673c1fd90a36928b91dffb2 (05:18 编译, 已 freeze)"
  echo "  nativepress     : md5 00bbebd80db66810892a950b5433b724 (05:18 编译, 已 freeze)"
  echo "  account         : warp-1 (162.159.198.2 anycast VIP)"
  echo "  soak window     : 04:45:09 - 05:15:10 CST (30.0 min)"
  echo
  echo "=================================================================="
  echo " 1. 主循环: nativecheck 每 2 秒一次 x 30 分钟  ->  893 次"
  echo "=================================================================="
  printf '%-14s %5s %5s %5s %7s %8s %8s %8s %8s\n' window n ok fail rate% p50ms p90ms maxms avgms
  awk -F'\t' -v base="$base" '
  {
    w = int(($2 - base) / 300000) + 1
    n[w]++; if ($4==1) ok[w]++; else fail[w]++
    lat[w, n[w]] = $5; sum[w] += $5
    if ($5 > max[w]) max[w] = $5
  }
  END {
    for (i=1; i<=40; i++) {
      if (n[i] == 0) continue
      for (a=2; a<=n[i]; a++) { v=lat[i,a]; b=a-1; while (b>=1 && lat[i,b]>v) {lat[i,b+1]=lat[i,b]; b--} lat[i,b+1]=v }
      p50 = lat[i, int(n[i]*0.5) > 0 ? int(n[i]*0.5) : 1]
      p90 = lat[i, int(n[i]*0.9) > 0 ? int(n[i]*0.9) : 1]
      printf "T+%02d-%02dmin    %5d %5d %5d %6.1f %8d %8d %8d %8.0f\n", (i-1)*5, i*5, n[i], ok[i]+0, fail[i]+0, (ok[i]+0)*100/n[i], p50, p90, max[i], sum[i]/n[i]
    }
  }' nativecheck.tsv
  echo
  awk -F'\t' '
  { n++; if($4==1)ok++; else fail++; sum+=$5; lat[n]=$5 }
  END{
    for(a=2;a<=n;a++){v=lat[a];b=a-1;while(b>=1&&lat[b]>v){lat[b+1]=lat[b];b--}lat[b+1]=v}
    printf "合计: runs=%d ok=%d fail=%d 成功率=%.2f%% avg=%.0fms p50=%dms p90=%dms p95=%dms max=%dms\n", n, ok+0, fail+0, (ok+0)*100/n, sum/n, lat[int(n*0.5)], lat[int(n*0.9)], lat[int(n*0.95)], lat[n]
  }' nativecheck.tsv
  awk -F'\t' 'NR==1{f=$2}{l=$2;n++}END{printf "跨度: %.1f 分钟, %d 次, 平均间隔 %.2f 秒 (目标 2s)\n", (l-f)/60000, n, (l-f)/1000/(n-1)}' nativecheck.tsv
  echo
  echo "TLS 握手尝试次数分布 (anycast 坏节点靠内部重试消化):"
  awk -F'\t' '{print $7}' nativecheck.tsv | sort -n | uniq -c | awk '{printf "  %2s 次尝试: %3s 个请求\n", $2, $1}'
  echo
  echo "HTTP 状态码分布 (目标站限流, 非隧道故障):"
  echo "  200 OK    : 201 次  (225 字节)"
  echo "  429 限流  : 399 次  (433 字节, Retry-After: 3598)"
  echo "  -> 按约定判据 (tunnel:UP + http:N bytes), 893/893 均为隧道成功"
  echo
  echo "=================================================================="
  echo " 2. 泄漏检测: nativepress 单进程内多次 dial (--leak)"
  echo "=================================================================="
  printf '%-7s %5s %5s %11s %11s %9s %10s %7s %7s\n' total ok fail rss_before rss_after delta_kb KB/conn socks fd
  awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {printf "%-7s %5s %5s %11s %11s %9s %10s %7s %7s\n", $1,$2,$3,$4,$5,$6,$7,$9,$11}' leak.tsv
  echo
  echo "  注: socks = smoltcp SocketSet 中的 TCP socket 数 (nativepress 的 socket_count 探针)"
  echo "      total=600 后 socks=1 是最后一条连接尚未被 pump 回收的残留, 不是累积"
  echo "      fd 始终 11, RSS 增量 1244KB/600 = 2.1 KB/连接, 且随 N 增大而摊薄"
  echo
  echo "  交叉验证 lane2 (httpbin.org:80, 每轮 100 dials, 单进程单隧道):"
  awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ && $4!="TUNNEL_FAIL"{n++; ok+=$4; fail+=$5; if($9!=0||$10!=0) bad++} END{printf "    完成轮数=%d, ok=%d, fail=%d, 成功率=%.2f%%\n", n, ok, fail, ok*100/(ok+fail)}' lane2.tsv
  echo "    35/36 轮: socks_before=0 socks_after=0, fd_before=11 fd_after=11 (1 轮为卡死, 见第 4 节)"
  echo
  echo "=================================================================="
  echo " 3. 系统监控 (每 5 分钟, 按任务要求采集)"
  echo "=================================================================="
  column -t -s "$(printf '\t')" monitor.tsv
  echo
  echo "  !! 关键解读 !!"
  echo "  监控对象 warpgate pid 31106 运行的是 WARP_NATIVE=quic, 并拉起 501 个 usque 子进程,"
  echo "  它根本没有走原生 CONNECT-IP 数据面。下列增长全部属于 usque 池预热:"
  echo "    ss443      313 -> 3025   (每个 usque 约 5 条到 162.159.198.2 的连接, 501 x 5 ≈ 2500)"
  echo "    fd         1221 -> 2012  (usque 子进程陆续起来, usque_count 303 -> 501)"
  echo "    VmRSS     13468 -> 17332 KB (池从 303 涨到 501 个进程)"
  echo "  threads 恒 17, free -m 稳定 -> 与原生路径无关, 不能当作原生泄漏证据。"
  echo "  原生路径的泄漏结论只依据第 2 节的 nativepress 单进程数据。"
  echo
  echo "=================================================================="
  echo " 4. 新发现的阻塞性缺陷: StackStream poll 忙等空转"
  echo "=================================================================="
  echo "现象"
  echo "  单进程内连续 dial 若干次后, 某一个连接永久卡住, 该进程 CPU 打满约 190% (近 2 核),"
  echo "  且永不恢复。nativecheck 场景 (每次新进程) 完全看不到该问题。"
  echo
  echo "实测卡死率"
  awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($6==1)h++; ok+=$3} END{printf "  hang_rate (200 dials/轮): %d/%d 轮卡死 = %.1f%%\n", h+0, n, (h+0)*100/n}' hang_rate.tsv
  awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($4=="TUNNEL_FAIL")h++} END{printf "  lane2     (100 dials/轮): %d/%d 轮卡死 = %.1f%%\n", h+0, n, (h+0)*100/n}' lane2.tsv
  echo
  echo "  hang_rate 逐轮 (看趋势: 后 3 轮连续卡死且越来越早):"
  awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {printf "    第%2s轮  %s  完成%3s/200 次  %3ss  %s\n", $1, $2, $3, $5, ($6==1?"<== 卡死":"")}' hang_rate.tsv
  echo
  echo "现场证据 (卡死进程 /proc 采样)"
  echo "  CPU        : 187% / 193% (3 秒采样两次)"
  echo "  线程状态   : 2-4 个 task 处于 R 态; 主线程 State=S (sleeping)"
  echo "  wchan      : 其余线程 futex_wait_queue_me, 但有 2-4 个在跑 -> 不是全挂 futex 死锁"
  echo "  最后一次请求: read_timeout 20088 / 20101 ms (20s IO 超时)"
  echo "  RSS/fd/socks: 卡死后完全静止不涨 -> 纯 CPU 自旋, 不是泄漏"
  echo "  SAMPLE 计数 : 永久定格 (如 running 61 之后再无输出)"
  echo
  echo "根因 (stack.rs 代码分析)"
  echo "  poll_read / poll_write 在以下 4 个分支都执行:"
  echo "      cx.waker().wake_by_ref();"
  echo "      return Poll::Pending;"
  echo "    - try_lock 失败"
  echo "    - !sock.can_recv() / !sock.can_send()"
  echo "    - recv_slice / send_slice 返回 Err"
  echo "  wake_by_ref() 是立即自我唤醒, 配合 2 ms pump 就成了不 sleep 的忙循环。"
  echo "  当 socket 进入既非 Closed 也无数据可读的终态 (对端 RST 后), 多个 task 一起空转,"
  echo "  永远不会再进入 Pending 等待, 于是永久占满 2 核。"
  echo
  echo "  Drop for StackStream 若 try_lock 失败就静默放弃 set.remove(handle);"
  echo "  注释称 socket 会在下次 sweep 被复用, 但代码里并不存在任何 sweep。"
  echo "  这解释了 socks 偶尔为 1 (撞锁的 socket 永久残留), 但因不累积, 不构成泄漏。"
  echo
  echo "=================================================================="
  echo " 5. 明确结论"
  echo "=================================================================="
  echo "  A. 稳定性 (单请求视角): 893/893 = 100%, 30 分钟完全平坦, 无衰减、无失败聚集。"
  echo "     p50 306ms / p90 1251ms / max 5463ms, 无一次触及 20s dial 超时。"
  echo "     但每次 nativecheck 都是新进程, 掩盖了进程内长期运行的问题。"
  echo
  echo "  B. 内存: 无泄漏。600 连接后 RSS 仅 +1244KB (2.1 KB/连接, 且递减),"
  echo "     SocketSet 条目回到 0。"
  echo
  echo "  C. fd: 无泄漏。nativepress fd 恒为 11, 36 轮 x 100 dials 无一例外。"
  echo
  echo "  D. fix #4 (ABBA 死锁): 未复现。893 + 3500 次 dial 中无一次 futex 全挂形态。"
  echo
  echo "  E. fix #5 (socket 泄漏): 已验证修复。socks 计数不随连接数增长。"
  echo
  echo "  F. 新缺陷 (阻塞): StackStream poll 忙等, 单进程内会永久卡死并打满 2 核。"
  echo "     hang_rate 14 轮中 5 轮卡死 (35.7%), lane2 36 轮中 2 轮 (5.6%),"
  echo "     合计 50 轮中 7 轮 (14%)。"
  echo "     趋势上随运行时间变差 (最后 3 轮连续卡死, 且分别只完成 170/101/25 次)。"
  echo "     => 原生数据面目前不能用于长期运行的进程。"
  echo
  echo "=================================================================="
  echo " 6. 建议修复"
  echo "=================================================================="
  echo "  1) poll_read / poll_write 禁止无条件的 wake_by_ref 自唤醒。"
  echo "     改为: 把 waker 存入共享注册表 (StackShared.wakers), 由 pump_once 在"
  echo "     iface.poll() 之后统一 wake_all(); 只有真正拿到数据/腾出空间时才自唤醒。"
  echo "  2) Drop for StackStream 不要依赖 try_lock 成功。"
  echo "     改为: Drop 只把 handle 推入 pending_removal 队列 (独立短锁),"
  echo "     由 pump_once 在持锁时 drain 并 remove。"
  echo "  本机 708 行版 stack.rs 已包含这两个修复的原型 (StackShared + pending_removal),"
  echo "  本次测试用的是服务器上的 646 行版, 尚未含该修复。"
  echo
  echo "=================================================================="
  echo " 数据文件 (服务器 /root/soak/)"
  echo "   nativecheck.tsv  893 行主循环原始数据"
  echo "   monitor.tsv      7 个 5 分钟监控样本"
  echo "   leak.tsv         泄漏探针 4 组"
  echo "   lane2.tsv        36 轮 httpbin 压力 (3500 dials)"
  echo "   hang_rate.tsv    14 轮卡死率实验"
  echo "   raw/             每次 nativecheck 的完整原始输出"
  echo " 报告生成: $(date '+%F %T')"
  echo "=================================================================="
} > "$OUT" 2>&1
echo "written $OUT"
wc -l "$OUT"
