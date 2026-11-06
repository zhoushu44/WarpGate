#!/bin/bash
# Swap in v0.12.2 (connection-leak watchdog) and verify it works.
#
# The watchdog recycles an engine once it holds more than health_max_conns
# established connections (now 3). A healthy engine needs exactly one, so this
# fires within minutes of the leak starting instead of letting it reach 4,000
# sockets and starve the pool of usable IPs.

set -u
OLD=/root/warpgate-rs/app
NEW=/root/wg_new

echo "=== 1. 版本确认 ==="
"$NEW/target/release/warpgate" --version
grep -c "recycle_if_leaky" "$NEW/crates/wg-pool/src/lib.rs"

echo ""
echo "=== 2. 切换 ==="
systemctl stop warpgate 2>/dev/null || true
pkill -9 usque 2>/dev/null || true
sleep 3
rm -rf "$OLD.old"
[ -d "$OLD" ] && mv "$OLD" "$OLD.old"
mv "$NEW" "$OLD"
echo "  binary: $(ls -lh "$OLD/target/release/warpgate" | awk '{print $5}')"

echo ""
echo "=== 3. 启动 ==="
systemctl start warpgate
for i in $(seq 1 20); do
  sleep 3
  [ "$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)" = "200" ] && { echo "  console up"; break; }
done

echo ""
echo "=== 4. 等实例就绪 ==="
for i in $(seq 1 30); do
  sleep 10
  H=$(curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null | grep -oE '"healthy_count":[0-9]*' | cut -d: -f2)
  if [ -n "$H" ] && [ "$H" -ge 90 ]; then echo "  ready healthy=$H"; break; fi
  [ $((i % 5)) -eq 0 ] && echo "  [$((i*10))s] healthy=${H:-0}"
done

echo ""
echo "=== 5. 观察看门狗（每 2 分钟，共 3 轮）==="
printf "  %-8s %-8s %-10s %-8s %s\n" "time" "cf_conn" "usque" "healthy" "recycled"
for i in $(seq 1 3); do
  sleep 120
  CF=$(ss -tn 2>/dev/null | grep -c 162.159)
  U=$(pgrep -c usque 2>/dev/null || echo 0)
  H=$(curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null | grep -oE '"healthy_count":[0-9]*' | cut -d: -f2)
  RC=$(journalctl -u warpgate --no-pager --since "-125s" 2>/dev/null | grep -c "引擎连接数异常")
  printf "  %-8s %-8s %-10s %-8s %s\n" "$(date +%H:%M:%S)" "$CF" "$U" "${H:-0}" "$RC"
done

echo ""
echo "=== 6. 最终 ==="
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
free -m | sed -n 2p
echo "看门狗触发总数: $(journalctl -u warpgate --no-pager --since "-15min" 2>/dev/null | grep -c '引擎连接数异常')"
