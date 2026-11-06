#!/bin/bash
# Simple status sampler: one line every 60s, no fancy formatting.
set -u
for i in $(seq 1 12); do
  V=$(/root/warpgate-rs/app/target/release/warpgate --version 2>/dev/null | awk '{print $2}')
  U=$(pgrep -c usque 2>/dev/null || true)
  CF=$(ss -tn 2>/dev/null | grep -c 162.159 || true)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null || true)
  H=$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2 || true)
  IP=$(printf '%s' "$S" | grep -oE '"unique_ips":[0-9]+' | cut -d: -f2 || true)
  RC=$(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '引擎连接数异常' || true)
  MEM=$(free -m | awk '/^Mem:/{print $3}')

  OK=0
  FAIL=0
  for p in 10011 10030 10060 10090; do
    C=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
    if [ "$C" = "200" ]; then OK=$((OK+1)); else FAIL=$((FAIL+1)); fi
  done

  echo "$(date '+%H:%M:%S') ver=$V usque=${U:-0} cf=${CF:-0} healthy=${H:-0} ips=${IP:-0} recycled=${RC:-0} mem=${MEM}MB proxy=${OK}ok/${FAIL}fail"
  sleep 60
done
