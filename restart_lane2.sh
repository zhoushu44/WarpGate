#!/bin/bash
set -u
pkill -f 'bash /root/lane2.sh' 2>/dev/null
sleep 1
cd /root/soak
rm -f lane2.tsv lane2_*.log
setsid nohup env TARGET=httpbin.org:80 bash /root/lane2.sh > /root/soak/lane2.out 2>&1 < /dev/null &
sleep 30
echo "=== lane2 after 30s ==="
cat /root/soak/lane2.tsv 2>/dev/null
echo "=== lane2 latest log ==="
tail -6 $(ls -t /root/soak/lane2_*.log 2>/dev/null | head -1) 2>/dev/null
