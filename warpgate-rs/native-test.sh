#!/bin/bash
# Test the native CONNECT-IP data plane in isolation.
#
# Uses a separate instance dir and port range so the running production pool
# (500 usque children on 10010+) is untouched.

set -u
APP=/root/warpgate-rs/app
T=/root/nativetest
rm -rf "$T"
mkdir -p "$T/instances"

# Copy 3 accounts and rewrite their endpoints to a spare port range.
n=0
for d in /root/warpgate-rs/instances/warp-1 /root/warpgate-rs/instances/warp-2 /root/warpgate-rs/instances/warp-3; do
  [ -d "$d" ] || continue
  cp -r "$d" "$T/instances/$(basename "$d")"
  n=$((n+1))
done
echo "copied $n accounts"

# Config: 3 instances, ports 30010+, whitelist 31010+
cat > "$T/config.json" <<'JSON'
{
  "port": 34433,
  "resident_count": 3,
  "max_instances": 3,
  "base_port": 30010,
  "whitelist_port_offset": 1000,
  "bind_address": "0.0.0.0",
  "mode": "resident",
  "sticky_minutes": 3,
  "max_concurrent_create": 1,
  "min_available": 1,
  "proxy_auth_enabled": true,
  "proxy_username": "warpuser",
  "proxy_password": "test-pass-123",
  "api_key": "zs1236547",
  "ip_whitelist": ["45.205.27.41", "127.0.0.1", "172.17.0.0/16"],
  "whitelist_port_offset": 1000,
  "warp_transport": "masque",
  "masque_http2": true,
  "log_level": "info",
  "public_ip": "auto-detect",
  "ip_check_url": "http://api.ipify.org",
  "ip_check_url_v6": "http://api6.ipify.org",
  "ip_check_retries": 3,
  "ip_check_interval": 2000,
  "rate_limit_enabled": false,
  "dns_servers": ["1.1.1.1"]
}
JSON

echo "=== config written ==="

cd "$APP"
export WARP_NATIVE=h2
ulimit -n 65535
nohup ./target/release/warpgate --config "$T/config.json" --instances "$T/instances" > "$T/run.log" 2>&1 &
PID=$!
disown $PID
echo "launched pid=$PID"

for i in $(seq 1 40); do
  sleep 3
  C=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:34433/ 2>/dev/null)
  [ "$C" = "200" ] && { echo "console up after $((i*3))s"; break; }
done

echo "=== 数据面 ==="
journalctl 2>/dev/null | tail -1 >/dev/null
grep -E "数据面:" "$T/run.log" | tail -1
echo "usque children of this pid: $(pgrep -P $PID 2>/dev/null | wc -l)"

echo "=== 等实例 ==="
for i in $(seq 1 30); do
  sleep 4
  H=$(curl -s --max-time 3 -H "X-API-Key: zs1236547" http://127.0.0.1:34433/api/stats 2>/dev/null)
  echo "  [$((i*4))s] $H"
  echo "$H" | grep -qE '"healthy_count":[1-9]' && break
done

echo "=== stats ==="
curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:34433/api/stats
echo ""
echo "=== 端口 ==="
ss -ltn 2>/dev/null | grep -E ":3[01][0-9]{3}" | head -6

echo "=== 代理测试 ==="
for p in 30011 30012 30013; do
  echo -n "  $p: "
  curl -s --max-time 25 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" -o /dev/null -w "HTTP %{http_code} (%{time_total}s)\n" http://httpbin.org/ip 2>&1
done
echo -n "  出口IP: "
curl -s --max-time 25 --socks5-hostname 127.0.0.1:30011 -U "warpuser:test-pass-123" https://api.ipify.org 2>&1
echo ""

echo "=== 内存 ==="
awk '/VmRSS/ {printf "warpgate: %.2f MB\n", $2/1024}' /proc/$PID/status 2>/dev/null
grep Threads /proc/$PID/status 2>/dev/null
echo "fd: $(ls /proc/$PID/fd 2>/dev/null | wc -l)"
echo ""
echo "=== 保留进程用于观察，PID=$PID 目录=$T ==="
