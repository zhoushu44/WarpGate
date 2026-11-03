#!/bin/bash
set -u
cd /root/soak
echo "=== round 1 (hung at 9 reqs) tail ==="
tail -20 rate_1.log
echo
echo "=== round 3 (hung at 150) tail ==="
tail -8 rate_3.log
echo
echo "=== raw REQ count vs SAMPLE 'running' counter ==="
grep '^REQ' rate_1.log | cut -f2 | tr '\n' ' '
echo
echo "=== live hung proc CPU ==="
for pid in $(pgrep -f 'soak/bin/nativepress'); do
  c1=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null); sleep 2; c2=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)
  hz=$(getconf CLK_TCK)
  cpu=$(awk -v a="$c1" -v b="$c2" -v h="$hz" 'BEGIN{printf "%.0f", (b-a)/2/h*100}')
  r=$(for t in /proc/$pid/task/*; do awk '{print $3}' $t/stat 2>/dev/null; done | grep -c R)
  echo "pid=$pid cpu=${cpu}% running_tasks=$r socks_scan=$(grep -c 'socks=' /proc/$pid/../$pid/nonexistent 2>/dev/null || echo n/a)"
done
echo
echo "=== now ==="
date '+%F %T'
