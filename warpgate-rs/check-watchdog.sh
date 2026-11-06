#!/bin/bash
# Check whether the connection watchdog is actually firing.
set -u

echo "=== per-engine connection distribution ==="
for p in $(pgrep usque); do
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p,")
  echo "$n"
done | sort -n | awk '
{ cnt[$1]++ }
END { for (k in cnt) printf "  %2d conns: %d engines\n", k, cnt[k] }
' | sort -t: -k1 -n

echo ""
echo "=== engines over the threshold (3) ==="
over=0
for p in $(pgrep usque); do
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p,")
  if [ "$n" -gt 3 ]; then over=$((over+1)); fi
done
echo "  $over engines"

echo ""
echo "=== watchdog log lines ==="
journalctl -u warpgate --no-pager --since "-30min" 2>/dev/null \
  | grep -E "引擎连接数异常|健康巡检|重启引擎" | tail -8
echo "  (count: $(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '引擎连接数异常'))"

echo ""
echo "=== health config ==="
grep -oE '"(health_check_interval|health_probe_batch|health_max_conns|resident_count)": *[0-9]*' \
  /root/warpgate-rs/rust-config.json 2>/dev/null

echo ""
echo "=== totals ==="
echo "  usque=$(pgrep -c usque 2>/dev/null || echo 0)"
echo "  cf_conn=$(ss -tn 2>/dev/null | grep -c 162.159)"
