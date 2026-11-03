#!/bin/bash
set -u
cd /root/soak
echo "=== hang_4.log full tail 25 ==="
tail -25 hang_4.log
echo
echo "=== live hung processes right now ==="
ps -eo pid,etime,pcpu,stat,cmd | grep 'soak/bin/nativepress' | grep -v grep
for pid in $(pgrep -f 'soak/bin/nativepress'); do
  cmd=$(tr '\0' ' ' < /proc/$pid/cmdline 2>/dev/null | cut -c1-80)
  state=$(awk '/^State/{print $2, $3}' /proc/$pid/status 2>/dev/null)
  rss=$(awk '/^VmRSS/{print $2}' /proc/$pid/status 2>/dev/null)
  thr=$(awk '/^Threads/{print $2}' /proc/$pid/status 2>/dev/null)
  fd=$(ls /proc/$pid/fd 2>/dev/null | wc -l)
  # sample CPU over 3s
  c1=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null); sleep 3; c2=$(awk '{print $14+$15}' /proc/$pid/stat 2>/dev/null)
  hz=$(getconf CLK_TCK)
  cpu=$(awk -v a="$c1" -v b="$c2" -v h="$hz" 'BEGIN{printf "%.0f", (b-a)/3/h*100}')
  echo "pid=$pid state=$state rss=${rss}KB thr=$thr fd=$fd cpu_now=${cpu}%"
  echo "  running_tasks_polling=$(for t in /proc/$pid/task/*; do awk '{print $3}' $t/stat 2>/dev/null; done | grep -c R)"
done
echo
echo "=== repro.out ==="
cat repro.out
echo "=== now ==="
date '+%F %T'
