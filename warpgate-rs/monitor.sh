#!/bin/bash
# Monitor the pool after deploying v0.12.2.
#
# What we are watching for:
#   * cf_conn must stay bounded — before the watchdog it grew ~80 per 15 min
#   * the watchdog must actually fire (recycled > 0) once engines start leaking
#   * usable IPs must climb as dead tunnels get recycled
#   * proxy calls must succeed from a client's point of view

set -u
DUR=${1:-900}     # seconds
STEP=60

echo "time,ver,usque,cf_conn,healthy,ips,recycled,rss_mb,proxy_ok,proxy_fail"
END=$(( $(date +%s) + DUR ))
while [ "$(date +%s)" -lt "$END" ]; do
  V=$(/root/warpgate-rs/app/target/release/warpgate --version 2>/dev/null | awk '{print $2}')
  U=$(pgrep -c usque 2>/dev/null || echo 0)
  CF=$(ss -tn 2>/dev/null | grep -c 162.159)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null)
  H=$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2)
  IP=$(printf '%s' "$S" | grep -oE '"unique_ips":[0-9]+' | cut -d: -f2)
  RC=$(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '引擎连接数异常')
  RSS=$(ps -o rss= -p $(pgrep -x warpgate | head -1) 2>/dev/null | awk '{printf "%.0f", $1/1024}')

  OK=0; FAIL=0
  for p in 10011 10030 10050 10070 10090; do
    C=$(curl -s --max-time 12 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
    [ "$C" = "200" ] && OK=$((OK+1)) || FAIL=$((FAIL+1))
  done

  echo "$(date '+%H:%M:%S'),$V,${U:-0},$CF,${H:-0},${IP:-0},${RC:-0},${RSS:-0},$OK,$FAIL"
  sleep "$STEP"
done

echo ""
echo "=== 汇总 ==="
echo "看门狗总触发: $(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '引擎连接数异常')"
echo "当前实例: $(curl -s --max-time 8 -H 'X-API-Key: zs1236547' http://127.0.0.1:4433/api/stats 2>/dev/null | grep -oE '"instance_count":[0-9]+')"
free -m | sed -n 2p
