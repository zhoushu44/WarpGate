#!/bin/bash
# Retest --always-reconnect on an account production is NOT using.
#
# Earlier manual tests used warp-0/1/2/96 while warpgate was already running
# those same accounts. Two clients sharing one WARP identity get rejected by
# Cloudflare with `connect-ip: server responded with 400`, which invalidated
# every measurement — including the conclusion that --always-reconnect must go.
#
# Here: pick instance directories outside resident_count (production only runs
# warp-0..warp-99) so the account is free.

set -u
BASE=/root/warpgate-rs/instances
PORT=28777
LOG=/root/idle_warm.log
: > "$LOG"

# Find a spare account.
CFG=""
for idx in $(seq 200 400); do
  f="$BASE/warp-$idx/usque.json"
  if [ -s "$f" ]; then CFG="$f"; break; fi
done
[ -z "$CFG" ] && { echo "找不到空闲账号"; exit 1; }
echo "使用账号: $CFG"
echo ""

echo "=== 启动：--always-reconnect ==="
/usr/local/bin/usque socks -c "$CFG" -b 127.0.0.1 -p $PORT --http2 --always-reconnect > "$LOG" 2>&1 &
PID=$!
echo "pid=$PID"
sleep 10

echo ""
echo "=== 立即测首次请求（此时隧道应已预热）==="
T0=$(date +%s%N)
C=$(curl -s --max-time 30 --socks5-hostname "127.0.0.1:$PORT" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T1=$(date +%s%N)
MS=$(( (T1-T0)/1000000 ))
echo "  首次: $C (${MS}ms)"

echo ""
echo "=== 观察 2 分钟：连接数是否增长 ==="
for i in $(seq 1 8); do
  sleep 15
  CFN=$(ss -tnp 2>/dev/null | grep -c "pid=$PID," || true)
  PINF=$(grep -c "different public key" "$LOG" 2>/dev/null || true)
  BAD400=$(grep -c "400" "$LOG" 2>/dev/null || true)
  echo "  +$((i*15))s conns=$CFN pin_fail=$PINF http400=$BAD400"
done

echo ""
echo "=== 再测一次请求（隧道应仍然热）==="
T2=$(date +%s%N)
C2=$(curl -s --max-time 30 --socks5-hostname "127.0.0.1:$PORT" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T3=$(date +%s%N)
echo "  二次: $C2 ($(( (T3-T2)/1000000 ))ms)"

echo ""
echo "=== 出口 IP ==="
curl -s --max-time 20 --socks5-hostname "127.0.0.1:$PORT" https://api64.ipify.org 2>/dev/null
echo ""

echo ""
echo "=== 日志关键行 ==="
grep -vE "^Hint|^HTTP/2 mode|^Using|^SOCKS proxy" "$LOG" 2>/dev/null | head -8

kill -9 $PID 2>/dev/null
