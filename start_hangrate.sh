#!/bin/bash
set -u
pkill -f 'bash /root/lane2.sh' 2>/dev/null
sleep 1
pkill -f 'soak/bin/nativepress' 2>/dev/null
sleep 2
cd /root/soak
setsid nohup env ROUNDS=14 bash /root/hang_rate.sh > /root/soak/hang_rate.out 2>&1 < /dev/null &
sleep 5
echo "started"
pgrep -af 'hang_rate|nativepress' | grep -v grep | head -3
