#!/bin/bash
echo "=== active app-related processes ==="
ps -eo pid,ppid,etime,pcpu,cmd | grep -E 'nativecheck|nativepress|cargo|rustc|soak' | grep -v grep
echo "=== ssh sessions ==="
who
echo "=== load ==="
uptime
echo "=== free ==="
free -m | sed -n 2p
echo "=== warp-1 recent activity? ==="
ls -l --time-style=+%T /root/warpgate-rs/instances/warp-1/ 2>/dev/null
echo "=== log tail ==="
tail -3 /root/warpgate-rs/warpgate.log
echo "=== soak dir ==="
ls -la /root/soak 2>/dev/null
