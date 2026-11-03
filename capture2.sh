#!/bin/bash
set -u
cd /root/soak
echo "=== repro.out ==="
cat repro.out
echo
echo "=== live nativepress processes ==="
ps -eo pid,etime,stat,pcpu,cmd | grep 'soak/bin/nativepress' | grep -v grep
echo
for pid in $(pgrep -f 'soak/bin/nativepress'); do
  echo "--- pid=$pid ---"
  awk '/^State|^Threads|^VmRSS/{print}' /proc/$pid/status 2>/dev/null
  echo "cpu_ticks=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)"
  sleep 2
  echo "cpu_ticks_2s=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)"
  echo "wchans:"
  for t in /proc/$pid/task/*; do echo "  $(basename $t) $(cat $t/wchan 2>/dev/null) state=$(awk '{print $3}' $t/stat 2>/dev/null)"; done | sort | uniq -c | sort -rn | head -8
done
echo
echo "=== hang logs ==="
for f in hang_*.log; do
  [ -f "$f" ] || continue
  echo "$f reqs=$(grep -c '^REQ' $f) summary=$(grep -c '^SUMMARY' $f) lastreq=$(grep '^REQ' $f | tail -1)"
done
echo "=== now ==="
date '+%F %T'
