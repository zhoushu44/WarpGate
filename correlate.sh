#!/bin/bash
# Does connection count predict SOCKS5 failure?
# Test ports of engines with few vs many connections.
set -u

ss -tnp 2>/dev/null | grep 162.159 > /tmp/cf.txt

echo "inst|conns|port|result"
for p in $(pgrep usque); do
  n=$(grep -c "pid=$p," /tmp/cf.txt 2>/dev/null || echo 0)
  port=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -A1 '^-p$' | tail -1)
  idx=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -oE 'warp-[0-9]+' | head -1)
  [ -z "$port" ] && continue
  echo "$n $idx $port"
done | sort -n > /tmp/engines.txt

echo "=== 连接最少的 5 个 ==="
head -5 /tmp/engines.txt | while read n idx port; do
  c=$(curl -s --max-time 12 --socks5-hostname 127.0.0.1:$port -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  echo "  $idx conns=$n port=$port -> $c"
done

echo ""
echo "=== 连接最多的 5 个 ==="
tail -5 /tmp/engines.txt | while read n idx port; do
  c=$(curl -s --max-time 12 --socks5-hostname 127.0.0.1:$port -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  echo "  $idx conns=$n port=$port -> $c"
done

echo ""
echo "=== 中位数附近 5 个 ==="
M=$(wc -l < /tmp/engines.txt); S=$((M / 2 - 2))
sed -n "${S},$((S + 4))p" /tmp/engines.txt | while read n idx port; do
  c=$(curl -s --max-time 12 --socks5-hostname 127.0.0.1:$port -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  echo "  $idx conns=$n port=$port -> $c"
done

echo ""
echo "=== usque 进程参数（看有无 keepalive 限制）==="
P=$(pgrep usque | head -1)
tr '\0' ' ' < /proc/$P/cmdline 2>/dev/null; echo ""
echo "=== usque 支持哪些参数 ==="
/usr/local/bin/usque socks --help 2>&1 | head -40
