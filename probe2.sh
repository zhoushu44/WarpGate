#!/bin/bash
echo "=== tcp states ==="
ss -tan 2>/dev/null | awk 'NR>1{print $1}' | sort | uniq -c
echo "=== established to 443 ==="
ss -tn state established '( dport = :443 )' 2>/dev/null | wc -l
echo "=== total to 443 (any state) ==="
ss -tan 2>/dev/null | grep -c ':443'
echo "=== warpgate fds ==="
ls /proc/1119/fd 2>/dev/null | wc -l
echo "=== usque children ==="
pgrep -c -x usque
echo "=== warpgate threads ==="
grep Threads /proc/1119/status
echo "=== rss ==="
grep VmRSS /proc/1119/status
echo "=== listening count ==="
ss -tln 2>/dev/null | wc -l
echo "=== established total ==="
ss -tn state established 2>/dev/null | wc -l
