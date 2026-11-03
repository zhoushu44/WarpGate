#!/bin/bash
P=$(pgrep usque | head -1)
echo "pid=$P"
echo "=== 1. environ 大小 ==="
wc -c /proc/$P/environ 2>&1
echo "=== 2. ps e 方式 ==="
ps eww -p $P 2>/dev/null | tr ' ' '\n' | grep -E "GOGC|GOMEMLIMIT"
echo "=== 3. 直接 od 前 2000 字节 ==="
head -c 2000 /proc/$P/environ 2>&1 | tr '\0' '\n'
echo "=== 4. 内存对比（重启后）==="
N=$(pgrep -c usque); T=$(for p in $(pgrep usque); do awk '/VmRSS/ {print $2}' /proc/$p/status 2>/dev/null; done | awk '{s+=$1} END {print s+0}')
echo "usque: $N 个 $((T/1024)) MB 平均 $((T/N/1024)) MB"
echo "=== 5. 单进程明细 ==="
for p in $(pgrep usque | head -5); do echo "  pid=$p $(awk '/VmRSS/ {print $2" kB"}' /proc/$p/status 2>/dev/null)"; done
