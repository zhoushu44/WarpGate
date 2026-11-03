#!/bin/bash
# Kill only nativepress processes (excluding this shell), then restart the leak probe.
set -u
for p in $(ps -eo pid,comm --no-headers | awk '$2=="nativepress"{print $1}'); do kill -9 "$p" 2>/dev/null; done
for p in $(ps -eo pid,args --no-headers | grep 'bash /root/leak2.sh' | grep -v grep | awk '{print $1}'); do kill -9 "$p" 2>/dev/null; done
sleep 2
echo "remaining:"; ps -eo pid,args --no-headers | grep -E 'nativepress|leak2' | grep -v grep | head -5
cd /root
setsid nohup bash /root/leak2.sh > /root/np/leak.out 2>&1 < /dev/null &
sleep 4
echo "--- leak.out ---"; cat /root/np/leak.out
echo "--- running ---"; ps -eo pid,args --no-headers | grep 'leak2.sh' | grep -v grep | head -2
