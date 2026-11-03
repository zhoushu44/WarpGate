#!/bin/bash
# restart the soak cleanly: freeze binary, time-bound 1800 s, aligned monitor
set -u
pkill -f 'bash /root/soak_run.sh' 2>/dev/null
pkill -f 'bash /root/soak_monitor.sh' 2>/dev/null
sleep 1
pkill -f 'timeout 90 /root/warpgate-rs/app/target/release/nativecheck' 2>/dev/null
sleep 1

mkdir -p /root/soak/bin /root/soak/raw
cp -f /root/warpgate-rs/app/target/release/nativecheck /root/soak/bin/nativecheck
md5sum /root/soak/bin/nativecheck

# archive the pilot run
cd /root/soak
[ -s nativecheck.tsv ] && cp nativecheck.tsv pilot_v1.tsv
[ -s monitor.tsv ] && cp monitor.tsv pilot_v1_monitor.tsv
rm -f nativecheck.tsv monitor.tsv
rm -rf raw; mkdir -p raw

cd /root/soak
setsid nohup env DURATION=1800 GAP_MS=2000 bash /root/soak_run.sh > /root/soak/loop.out 2>&1 < /dev/null &
sleep 1
setsid nohup env N=7 GAP=300 bash /root/soak_monitor.sh > /root/soak/monitor.out 2>&1 < /dev/null &
sleep 3
cat /root/soak/loop.start
ps -eo pid,etime,cmd | grep -E 'soak_run|soak_monitor|nativecheck' | grep -v grep | head
