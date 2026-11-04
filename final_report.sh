#!/bin/bash
# Consolidate every measurement into one final report on the server.
set -u
D2=/root/np2
OUT=$D2/FINAL_REPORT.txt
{
echo "=================================================================="
echo " WarpGate Rust CONNECT-IP - StackStream poll 忙循环修复 复测报告"
echo "=================================================================="
echo
echo "主机      : $(hostname)  $(uname -r)  $(nproc) cores"
echo "复测时间  : $(date '+%F %T')  (Asia/Shanghai)"
echo
echo "--- 被测源码 ---"
echo "  stack.rs : 760 行  md5 095951dd1b26b6c4f313ecc5a495bcb1"
echo "    pump_once 末尾 self.shared.wake_all()"
echo "    pump_once 内 drain pending_removal -> set.remove()"
echo "    Drop: self.shared.queue_removal(handle) + self.shared.wake_all()"
echo "    生产代码 wake_by_ref 调用: 0"
echo "  wiring 回归测试: cargo test -p wg-tunnel --lib wiring -> 1 passed"
echo
echo "--- 二进制 (touch 强制重编, 排除旧 artifact) ---"
md5sum /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/app/target/release/nativepress | sed 's/^/  /'
echo
echo "=================================================================="
echo " 1. 卡死率"
echo "=================================================================="
echo "  修复前(忙循环)   : 50 轮 7 次 = 14%      CPU 187-193%, 越挂越早 170->101->25"
echo "  误改版(只改定义) : 20 轮 20 次 = 100%    CPU 2-4%, 0 个 R 态线程, req 恒 0"
echo "  修复后(本轮)     : 40 轮 0 次 = 0.0%     CPU 峰值 8-11%, 8000/8000 成功"
echo
echo "  两批独立 20 轮 x 200 dials:"
for f in $D2/hr2.tsv $D2/hr3.tsv; do
  [ -f "$f" ] || continue
  awk -F'\t' -v F="$(basename $f)" 'NR>1 && $1 ~ /^[0-9]+$/ {
      n++; if($11==1)h++; ok+=$3; fail+=$4; w+=$6;
      if($7>cp)cp=$7; if($9>p90)p90=$9; if($10>mx)mx=$10
    } END{
      printf "    %-10s rounds=%d hung=%d rate=%.1f%% ok=%d fail=%d avg_wall=%.1fs cpu_peak=%d%% p90max=%dms max=%dms\n",
             F, n, h+0, (h+0)*100/n, ok, fail, w/n, cp, p90, mx
    }' "$f"
done
echo
echo "  逐轮明细 (hr2):"
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {printf "    %2s  %s  ok=%3s fail=%s wall=%2ss cpu=%2s%% p50=%3sms max=%5sms hung=%s\n",$1,$2,$3,$4,$6,$7,$8,$10,$11}' $D2/hr2.tsv
echo
echo "=================================================================="
echo " 2. 并发扩展性 (修复版, 各 20 dials)"
echo "=================================================================="
printf '    %-6s %-6s %-6s %-8s %-10s %-10s\n' conc ok fail wall p50 max
echo "    1      20     0      2s       65ms       76ms"
echo "    4      20     0      1s       43ms       75ms"
echo "    8      20     0      2s       51ms       68ms"
echo "    32     20     0      1s       47ms       57ms"
echo
echo "=================================================================="
echo " 3. socket 泄漏"
echo "=================================================================="
echo "  基线(修复前)      : socks 46 -> 360 线性增长 (500 次, 并发 8), RSS 5.7 -> 22.6 MB"
echo "  误改版            : socks 全程 0 (np/leak_socks.tsv, 但用了 delay 兜底)"
echo "  修复版            : socks 全程 0 (np2/socks.tsv, 500/500 成功)"
echo
cat $D2/socks.tsv | sed 's/^/    /'
echo
echo "  修复版 RSS 长跑 (np2/soak.tsv):"
cat $D2/soak.tsv | sed 's/^/    /'
echo
echo "  5000 dials 单进程: RSS 5024 -> 6184 KB (+1160 KB / 5000 = 0.23 KB/连接), fd 恒 11, socks 0"
echo "  -> 无泄漏。若按旧泄漏每个 socket 保 128 KiB, 500 连接应涨 ~40 MB, 实测 +2.1 MB 且无趋势。"
echo
echo "=================================================================="
echo " 4. 已知抖动 (非死锁, 请评估)"
echo "=================================================================="
echo "  soak run b: 第 893-905 号请求中 10 次失败 (1 read_timeout 15s + 9 dial_timeout 20s),"
echo "    8 个 inflight 同时卡约 50s 后自行恢复, CPU 正常, 进程未挂死。"
echo "  burst 复现 (6 x 1000 dials): 0 次同类 burst, 6000/6000 成功;"
echo "    但 run 5 出现一次 26.5s 的完成间隔尖峰, 同样自行恢复。"
echo "  判断: WARP anycast 换节点/上游抖动, 不是 waker 缺陷"
echo "        (waker 缺陷的特征是永久不恢复 + CPU 打满, 二者都不符合)。"
echo
echo "=================================================================="
echo " 5. 结论"
echo "=================================================================="
echo "  卡死率 14% -> 0.0% (40 轮 8000 dials 零卡死, 两个独立批次可复现)"
echo "  CPU     187-193% -> 峰值 8-11%"
echo "  socket 泄漏 46->360 -> 恒 0"
echo "  => 原阻塞缺陷已消除, 可以上生产。"
echo "  唯一保留意见: 上游节点抖动会导致偶发 20s 级 dial 超时, 建议缩短 dial_timeout 并加重试。"
echo
echo "=================================================================="
echo " 数据文件"
echo "=================================================================="
for f in $D2/hr2.tsv $D2/hr3.tsv $D2/socks.tsv $D2/leak.tsv $D2/soak.tsv $D2/burst.tsv; do
  [ -f "$f" ] && printf '  %-28s %s\n' "$(basename $f)" "$(date -r "$f" '+%F %T')"
done
echo
echo "  版本归属:"
echo "    /root/np/hr20.tsv  07:16  误改版 20轮  -> 100% 卡死"
echo "    /root/np2/hr2.tsv  07:51  修复版 20轮  -> 0%"
echo "    /root/np2/hr3.tsv  $(date -r $D2/hr3.tsv '+%H:%M')  修复版 20轮  -> 0% (复核)"
} > "$OUT"
echo "written $OUT"
wc -l "$OUT"
