#!/bin/bash
# 彻底重启：杀干净所有 usque 和 warpgate，等端口释放，再起
set -x
pkill -9 -f "target/release/warpgate" 2>/dev/null
pkill -9 usque 2>/dev/null
sleep 5
echo "残留 usque: $(pgrep -c usque 2>/dev/null || echo 0)"
echo "残留 warpgate: $(pgrep -c warpgate 2>/dev/null || echo 0)"
echo "端口占用: $(ss -ltn 2>/dev/null | grep -cE ':(4433|10010|20000) ')"
set +x

cd /root/warpgate-rs/app
nohup ./target/release/warpgate \
  --config /root/warpgate-rs/rust-config.json \
  --instances /root/warpgate-rs/instances \
  > /root/warpgate-rs/warpgate.log 2>&1 &
PID=$!
disown $PID
echo "launched PID=$PID"
sleep 90
echo "=== 结果 ==="
echo "warpgate RSS: $(awk '/VmRSS/ {printf "%.2f MB", $2/1024}' /proc/$PID/status 2>/dev/null)"
N=$(pgrep -c usque 2>/dev/null || echo 0)
T=$(for p in $(pgrep usque 2>/dev/null); do awk '/VmRSS/ {print $2}' /proc/$p/status 2>/dev/null; done | awk '{s+=$1} END {print s+0}')
echo "usque: $N 个 合计 $((T/1024)) MB 平均 $((T/N/1024)) MB/个"
P=$(pgrep usque | head -1)
echo "首个 usque 的 GOGC: $(cat /proc/$P/environ 2>/dev/null | tr '\0' '\n' | grep GOGC || echo '(读不到)')"
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
