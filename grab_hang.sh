#!/bin/bash
set -u
cd /root/soak
echo "=== current nativepress processes ==="
ps -eo pid,ppid,etime,stat,cmd | grep 'nativepress' | grep -v grep

pid=$(pgrep -f 'soak/bin/nativepress' | head -1)
echo "pid=$pid"
if [ -n "${pid:-}" ]; then
  echo
  echo "=== /proc/$pid/status ==="
  grep -E 'VmRSS|Threads|State' /proc/$pid/status
  echo "=== fd count: $(ls /proc/$pid/fd 2>/dev/null | wc -l) ==="
  echo "=== fds ==="
  ls -l /proc/$pid/fd 2>/dev/null | sed 's/.*[0-9] //' | sort | uniq -c | head -10
  echo
  echo "=== wchan / syscall / stack ==="
  for t in /proc/$pid/task/*; do
    tid=$(basename $t)
    st=$(cat $t/stat 2>/dev/null | awk '{print $3}')
    wc=$(cat $t/wchan 2>/dev/null)
    sc=$(cat $t/syscall 2>/dev/null | awk '{print $1}')
    echo "tid=$tid state=$st wchan=$wc syscall=$sc"
  done
  echo
  echo "=== /proc/$pid/task/*/stack (kernel stacks, needs perm) ==="
  for t in /proc/$pid/task/*; do
    echo "--- $(basename $t) ---"
    cat $t/stack 2>/dev/null | head -8
  done
  echo
  echo "=== net connections of this pid ==="
  ss -tanp 2>/dev/null | grep "pid=$pid," | head -10
  echo
  echo "=== its 443 conn state ==="
  ss -tanp 2>/dev/null | grep "pid=$pid," | awk '{print $1}' | sort | uniq -c
fi
echo
echo "=== repro.out ==="
cat repro.out
echo "=== now ==="
date '+%F %T'
