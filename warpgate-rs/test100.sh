#!/bin/bash
# Scale test: 100 native CONNECT-IP instances, isolated from production.
set -u
T=/root/nativetest
APP=/root/warpgate-rs/app

pkill -f "nativetest/config.json" 2>/dev/null || true
sleep 2

rm -rf "$T/instances"
mkdir -p "$T/instances"
n=0
for d in /root/warpgate-rs/instances/warp-*; do
  [ "$n" -ge 100 ] && break
  b=$(basename "$d")
  if cp -r "$d" "$T/instances/$b" 2>/dev/null; then n=$((n+1)); fi
done
echo "copied=$n"

cp /root/warpgate-rs/rust-config.json "$T/config.json"
sed -i 's/"port": *[0-9]*/"port": 34433/' "$T/config.json"
sed -i 's/"resident_count": *[0-9]*/"resident_count": 100/' "$T/config.json"
sed -i 's/"max_instances": *[0-9]*/"max_instances": 100/' "$T/config.json"
sed -i 's/"base_port": *[0-9]*/"base_port": 30010/' "$T/config.json"
echo "cfg: $(grep -oE '"'"'"(port|resident_count|max_instances|base_port)": *[0-9]*'"'"' "$T/config.json" | tr '\n' ' ')"

cd "$APP"
export WARP_NATIVE=h2
ulimit -n 65535
nohup ./target/release/warpgate --config "$T/config.json" --instances "$T/instances" > "$T/run100.log" 2>&1 &
PID=$!
disown $PID
echo "pid=$PID started $(date +%H:%M:%S)"

for i in $(seq 1 60); do
  sleep 5
  H=$(curl -s --max-time 3 -H "X-API-Key: zs1236547" http://127.0.0.1:34433/api/stats 2>/dev/null)
  HCNT=$(echo "$H" | grep -o '"healthy_count":[0-9]*' | cut -d: -f2)
  if [ $((i % 4)) -eq 0 ]; then echo "  [$((i*5))s] healthy=$HCNT"; fi
  if [ -n "$HCNT" ] && [ "$HCNT" -ge 90 ]; then echo "READY at $((i*5))s"; break; fi
done

echo "=== final stats ==="
curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:34433/api/stats
echo ""
echo "=== memory ==="
awk '/VmRSS/ {printf "RSS %.2f MB\n", $2/1024}' /proc/$PID/status 2>/dev/null
grep Threads /proc/$PID/status 2>/dev/null
echo "fd: $(ls /proc/$PID/fd 2>/dev/null | wc -l)"
echo "listening: $(ss -ltn 2>/dev/null | grep -cE ':30[0-9]{3}|:31[0-9]{3}')"

echo "=== proxy ==="
for p in 30011 30050 30109; do
  echo -n "  $p: "
  curl -s --max-time 25 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" -o /dev/null -w 'HTTP %{http_code} %{time_total}s\n' http://httpbin.org/ip 2>&1
done
echo -n "  exit IP: "
curl -s --max-time 25 --socks5-hostname 127.0.0.1:30011 -U "warpuser:test-pass-123" https://api.ipify.org 2>&1
echo ""
