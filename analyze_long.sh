#!/bin/bash
set -u
L=/root/np2/long3000.log
echo "=== PROGRESS rss/fd over the whole run ==="
grep -a '^PROGRESS' "$L" | awk -F'\t' '
{
  split($2,x,"="); split($6,y,"="); split($7,z,"=");
  printf "%5d  rss=%6d  fd=%s\n", x[2]+0, y[2]+0, z[2]
}' | awk 'NR%5==1'
echo "=== fd distribution ==="
grep -a '^PROGRESS' "$L" | sed -n 's/.*fd=\([0-9]*\).*/\1/p' | sort -n | uniq -c
echo "=== samples ==="
grep -a '^SAMPLE' "$L"
echo "=== any fd>11 and when ==="
grep -a '^PROGRESS' "$L" | grep -a 'fd=1[2-9]' | head -5
echo "=== latency stats (3000) ==="
awk -F'\t' '$1=="REQ"{a[NR]=$5; sum+=$5} END{
  n=asort(a);
  printf "n=%d avg=%.0fms p50=%dms p90=%dms p99=%dms max=%dms\n", NR, sum/NR, a[int(n*0.5)], a[int(n*0.9)], a[int(n*0.99)], a[n]
}' "$L" 2>/dev/null || awk -F'\t' '$1=="REQ"{print $5}' "$L" | sort -n | awk '{a[NR]=$1; s+=$1} END{printf "n=%d avg=%.0fms p50=%dms p90=%dms p99=%dms max=%dms\n", NR, s/NR, a[int(NR*0.5)], a[int(NR*0.9)], a[int(NR*0.99)], a[NR]}'
