#!/bin/bash
# Swap in the freshly built 0.12.0 tree and verify the pool self-heals.
#
# Key thing to observe: most engines are currently dead (13k connections, SOCKS
# accepts then never acks). 0.12.0's health loop should demote them and restart
# their engines on its own. If it works, available ports climb without manual
# intervention.

set -u
OLD=/root/warpgate-rs/app
NEW=/root/wg_new
CFG=/root/warpgate-rs/rust-config.json
TARGET=${1:-300}

echo "=== 1. 切换目录 ==="
systemctl stop warpgate
pkill -9 usque 2>/dev/null || true
sleep 4
rm -rf "$OLD.old"
[ -d "$OLD" ] && mv "$OLD" "$OLD.old"
mv "$NEW" "$OLD"
echo "  version: $(grep -m1 '^version' "$OLD/Cargo.toml")"
ls -l "$OLD/target/release/warpgate" | awk '{print "  binary:", $5, "bytes", $6, $7, $8}'

echo ""
echo "=== 2. 实例数降到 $TARGET（15.9 GB 只够这个量级）==="
sed -i "s/\"resident_count\": *[0-9]*/\"resident_count\": $TARGET/" "$CFG"
sed -i "s/\"max_instances\": *[0-9]*/\"max_instances\": $TARGET/" "$CFG"
# 缩短巡检间隔，让自愈效果在观察窗口内可见
python3 - "$CFG" <<'PY' 2>/dev/null || sed -i 's/"health_check_interval": *[0-9]*/"health_check_interval": 60/' "$CFG"
import json, sys
p = sys.argv[1]
d = json.load(open(p))
d["health_check_interval"] = 60      # 60s per sweep
d["health_probe_batch"] = 6          # 6 instances per tick
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
PY
grep -oE '"(resident_count|max_instances|health_check_interval|health_probe_batch)": *[0-9]*' "$CFG"

echo ""
echo "=== 3. 启动 ==="
systemctl start warpgate
for i in $(seq 1 30); do
  sleep 3
  C=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)
  [ "$C" = "200" ] && { echo "  console up $((i*3))s"; break; }
done
journalctl -u warpgate --no-pager -n 30 2>/dev/null | grep -E "数据面|实例目录" | tail -2

echo ""
echo "=== 4. 观察自愈（每 60s 一次，共 10 分钟）==="
echo "  time     usque  cf_conn  healthy  unhealthy  ips   probe"
for i in $(seq 1 10); do
  sleep 60
  U=$(pgrep -c usque 2>/dev/null || echo 0)
  CF=$(ss -tn 2>/dev/null | grep -c 162.159)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null)
  HC=$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]*' | cut -d: -f2)
  UN=$(printf '%s' "$S" | grep -oE '"unhealthy":[0-9]*' | cut -d: -f2)
  IP=$(printf '%s' "$S" | grep -oE '"unique_ips":[0-9]*' | cut -d: -f2)
  BACK=$(journalctl -u warpgate --no-pager --since "-70s" 2>/dev/null | grep -c "重启引擎")
  OK=0
  for p in 10011 10050 10090 10130; do
    C=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
    [ "$C" = "200" ] && OK=$((OK+1))
  done
  printf "  +%-6s %-6s %-8s %-8s %-10s %-5s %s/4 (back=%s)\n" \
    "$((i))m" "${U:-0}" "${CF:-0}" "${HC:-0}" "${UN:-0}" "${IP:-0}" "$OK" "${BACK:-0}"
done

echo ""
echo "=== 5. 最终 ==="
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
free -m | sed -n 2p
echo "巡检重启总数: $(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '重启引擎')"
