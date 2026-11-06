#!/bin/bash
# Swap in the freshly built 0.12.1 tree and start with a safe instance count.
#
# This is the recovery after the OOM incident: the previous build ignored
# resident_count and started all 999 directories (~318 MB each per `top`),
# which exhausted a 16 GB box. 0.12.1 caps boot at resident_count.

set -u
OLD=/root/warpgate-rs/app
NEW=/root/wg_new
CFG=/root/warpgate-rs/rust-config.json
TARGET=100

echo "=== 1. 版本确认 ==="
grep -m1 '^version' "$NEW/Cargo.toml"
"$NEW/target/release/warpgate" --version
grep -c "async fn health_loop" "$NEW/crates/wg-pool/src/lib.rs"

echo ""
echo "=== 2. 配置：实例数降到 $TARGET ==="
python3 - "$CFG" "$TARGET" <<'PY' 2>/dev/null || {
import json, sys
p, n = sys.argv[1], int(sys.argv[2])
d = json.load(open(p))
d["resident_count"] = n
d["max_instances"] = n
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
print("config updated via python")
PY
sed -i "s/\"resident_count\": *[0-9]*/\"resident_count\": $TARGET/" "$CFG"
sed -i "s/\"max_instances\": *[0-9]*/\"max_instances\": $TARGET/" "$CFG"
}
grep -oE '"(resident_count|max_instances|base_port)"[: ]+[0-9]+' "$CFG"

echo ""
echo "=== 3. 切换目录 ==="
systemctl stop warpgate 2>/dev/null || true
pkill -9 usque 2>/dev/null || true
sleep 3
rm -rf "$OLD.old"
[ -d "$OLD" ] && mv "$OLD" "$OLD.old"
mv "$NEW" "$OLD"
echo "  swapped, binary: $(ls -lh "$OLD/target/release/warpgate" | awk '{print $5}')"

echo ""
echo "=== 4. 启动 ==="
systemctl start warpgate
for i in $(seq 1 25); do
  sleep 3
  C=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)
  [ "$C" = "200" ] && { echo "  console up $((i*3))s"; break; }
done
journalctl -u warpgate --no-pager -n 40 2>/dev/null | grep -E "数据面|实例目录|常驻" | tail -3

echo ""
echo "=== 5. 观察 8 分钟（关键：实例数必须停在 $TARGET）==="
printf "  %-8s %-7s %-8s %-8s %-8s %s\n" "time" "usque" "healthy" "count" "mem_MB" "probe"
for i in $(seq 1 8); do
  sleep 60
  U=$(pgrep -c usque 2>/dev/null || echo 0)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null)
  HC=$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2)
  CN=$(printf '%s' "$S" | grep -oE '"instance_count":[0-9]+' | cut -d: -f2)
  MEM=$(free -m | awk '/^Mem:/{print $3}')
  OK=0
  for p in 10011 10035 10060; do
    C=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
    [ "$C" = "200" ] && OK=$((OK+1))
  done
  printf "  +%-7sm %-7s %-8s %-8s %-8s %s/3\n" "$i" "${U:-0}" "${HC:-0}" "${CN:-0}" "${MEM:-0}" "$OK"
done

echo ""
echo "=== 6. 最终 ==="
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
free -m | sed -n 2p
echo "健康巡检重启次数: $(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '重启引擎')"
