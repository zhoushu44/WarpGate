#!/bin/bash
# 对照实验：GOGC/GOMEMLIMIT 对 usque 单进程内存的影响
pkill -f "p 20999" 2>/dev/null
sleep 2
GOGC=50 GOMEMLIMIT=48MiB setsid /usr/local/bin/usque socks \
  -c /root/warpgate-rs/instances/warp-2/usque.json \
  -b 127.0.0.1 -p 20999 --always-reconnect --http2 \
  > /tmp/usc2.log 2>&1 < /dev/null &
sleep 25

NP=$(pgrep -f "p 20999" | head -1)
echo "pid=$NP"
if [ -n "$NP" ]; then
  echo "VmRSS=$(awk '/VmRSS/ {print $2}' /proc/$NP/status) kB"
fi
echo "port: $(ss -ltn 2>/dev/null | grep -c 20999)"
echo "exit: $(curl -s --max-time 15 --socks5-hostname 127.0.0.1:20999 https://api.ipify.org 2>&1)"
echo "--- log ---"
tail -4 /tmp/usc2.log 2>/dev/null || echo "(no log)"
echo ""
echo "=== 对照：池中一个正常进程 ==="
OP=$(pgrep usque | sed -n '2p')
if [ -n "$OP" ]; then
  echo "pid=$OP VmRSS=$(awk '/VmRSS/ {print $2}' /proc/$OP/status) kB"
fi
