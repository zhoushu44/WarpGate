#!/bin/bash
# Does --always-reconnect cause the socket leak?
#
# Launch two usque instances on the same account config, one with the flag and
# one without, drive identical traffic through both, and sample their TCP
# connection counts every 30s. If the flag is the cause, the "with" instance
# accumulates sockets and the "without" one stays flat.

set -u
CFG=/root/warpgate-rs/instances/warp-0/usque.json
OUT=/root/leaktest.txt
: > "$OUT"

echo "config: $CFG"
[ -s "$CFG" ] || { echo "missing config"; exit 1; }

cleanup() {
  [ -n "${P1:-}" ] && kill -9 "$P1" 2>/dev/null
  [ -n "${P2:-}" ] && kill -9 "$P2" 2>/dev/null
}
trap cleanup EXIT

echo "=== start both ==="
/usr/local/bin/usque socks -c "$CFG" -b 127.0.0.1 -p 28801 --http2 --always-reconnect \
  > /root/lt_with.log 2>&1 &
P1=$!
/usr/local/bin/usque socks -c "$CFG" -b 127.0.0.1 -p 28802 --http2 \
  > /root/lt_without.log 2>&1 &
P2=$!
echo "with=$P1 without=$P2"
sleep 12

count() { grep -c "pid=$1," /tmp/ss_snap 2>/dev/null || echo 0; }

for i in $(seq 1 10); do
  ss -tnp 2>/dev/null > /tmp/ss_snap
  C1=$(count "$P1")
  C2=$(count "$P2")

  # identical load on both: one request every sample
  curl -s --max-time 8 --socks5-hostname 127.0.0.1:28801 -o /dev/null http://httpbin.org/ip 2>/dev/null &
  curl -s --max-time 8 --socks5-hostname 127.0.0.1:28802 -o /dev/null http://httpbin.org/ip 2>/dev/null &
  wait

  echo "$(date '+%H:%M:%S') with_flag=$C1 without_flag=$C2" | tee -a "$OUT"
  sleep 30
done

echo ""
echo "=== 各自日志尾部 ==="
echo "--- with --always-reconnect ---"
tail -4 /root/lt_with.log
echo "--- without ---"
tail -4 /root/lt_without.log
