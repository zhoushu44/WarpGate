#!/bin/bash
# Can restarting one dead engine restore service?
# If yes, the fix is a connection-count watchdog that restarts engines; the
# tunnels are dead but usque keeps the SOCKS listener open, so clients hang
# waiting for a CONNECT ack that never comes.
set -u
IDX=${1:-warp-345}
PORT=$(printf '%s' "$IDX" | sed 's/warp-//' | awk '{print 20000 + $1}')

echo "=== before ==="
echo -n "  conns=$(ss -tnp 2>/dev/null | grep -c "pid=$(pgrep -f "$IDX/usque.json" | head -1),") "
echo -n "socks=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:$PORT -U 'warpuser:test-pass-123' \
  -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)"
echo ""

echo "=== restart engine ==="
PID=$(pgrep -f "$IDX/usque.json" | head -1)
echo "  killing pid=$PID"
kill -9 "$PID" 2>/dev/null
sleep 3

# warpgate's supervisor should notice the child exit and restart it.
for i in $(seq 1 20); do
  sleep 2
  NEWPID=$(pgrep -f "$IDX/usque.json" | head -1)
  if [ -n "$NEWPID" ] && [ "$NEWPID" != "$PID" ]; then
    echo "  new pid=$NEWPID after $((i*2))s"
    break
  fi
done

sleep 8
echo "=== after ==="
NEWPID=$(pgrep -f "$IDX/usque.json" | head -1)
echo -n "  conns=$(ss -tnp 2>/dev/null | grep -c "pid=$NEWPID,") "
echo -n "socks=$(curl -s --max-time 15 --socks5-hostname 127.0.0.1:$PORT -U 'warpuser:test-pass-123' \
  -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)"
echo ""

echo ""
echo "=== 手动重启 usque 对比（不经 warpgate）==="
echo "  说明: 若上面经 warpgate 重启无效，这里验证 usque 本身能否立刻恢复"
