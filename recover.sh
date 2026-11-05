#!/bin/bash
# Recover the pool.
#
# Findings so far:
#   * Every engine except warp-1 holds 17-41 connections to Cloudflare and its
#     SOCKS port answers CONNECT with silence (no ack) -> clients see 000.
#   * Killing an engine does NOT bring it back: the supervisor only restarts a
#     child when it notices the exit, and here the listener vanished with it.
#   * Memory is at 12.7/15.9 GB with only 507 engines, so 1000 is not viable.
#
# So: stop everything, drop to a size that fits in RAM, restart clean, verify.

set -u
CFG=/root/warpgate-rs/rust-config.json
TARGET=${1:-300}

echo "=== 1. 停服务并清干净 ==="
systemctl stop warpgate
pkill -9 usque 2>/dev/null || true
sleep 5
echo "  usque=$(pgrep -c usque 2>/dev/null || echo 0)"
echo "  cf_conn=$(ss -tn 2>/dev/null | grep -c 162.159)"

echo ""
echo "=== 2. 调整实例数到 $TARGET（内存只够这个量级）==="
sed -i "s/\"resident_count\": *[0-9]*/\"resident_count\": $TARGET/" "$CFG"
sed -i "s/\"max_instances\": *[0-9]*/\"max_instances\": $TARGET/" "$CFG"
grep -oE '"(resident_count|max_instances|base_port)": *[0-9]*' "$CFG"

echo ""
echo "=== 3. 启动 ==="
systemctl start warpgate
for i in $(seq 1 40); do
  sleep 5
  C=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)
  [ "$C" = "200" ] && { echo "  console up $((i*5))s"; break; }
done

echo ""
echo "=== 4. 等实例就绪（最多 8 分钟）==="
for i in $(seq 1 48); do
  sleep 10
  H=$(curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null)
  HC=$(printf '%s' "$H" | grep -oE '"healthy_count":[0-9]*' | cut -d: -f2)
  [ $((i % 6)) -eq 0 ] && echo "  [$((i*10))s] healthy=${HC:-0}/$TARGET"
  if [ -n "$HC" ] && [ "$HC" -ge $((TARGET * 9 / 10)) ]; then
    echo "  ready $((i*10))s healthy=$HC"
    break
  fi
done

echo ""
echo "=== 5. 状态 ==="
systemctl is-active warpgate
echo "  usque=$(pgrep -c usque)  cf_conn=$(ss -tn 2>/dev/null | grep -c 162.159)"
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
echo "  mem: $(free -m | sed -n 2p)"

echo ""
echo "=== 6. 抽样 10 个端口 ==="
OK=0; FAIL=0
for p in $(seq 10011 20 10191); do
  C=$(curl -s --max-time 12 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  if [ "$C" = "200" ]; then OK=$((OK+1)); else FAIL=$((FAIL+1)); echo "    fail $p -> $C"; fi
done
echo "  OK=$OK FAIL=$FAIL"
