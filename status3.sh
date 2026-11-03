#!/bin/bash
set -u
cd /root/soak
echo "=== progress ==="
wc -l < nativecheck.tsv
echo "=== ok/fail ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d rate=%.2f%%\n", o+0, f+0, (o+0)*100/((o+f)>0?(o+f):1)}' nativecheck.tsv
echo "=== last 3 ==="
tail -3 nativecheck.tsv
echo "=== http bytes distribution ==="
awk -F'\t' '{print $6}' nativecheck.tsv | sort -n | uniq -c
echo "=== attempts distribution ==="
awk -F'\t' '{print $7}' nativecheck.tsv | sort -n | uniq -c
echo "=== latency ==="
awk -F'\t' '{print $5}' nativecheck.tsv | sort -n | awk '{a[NR]=$1} END{printf "n=%d p50=%d p90=%d p99=%d max=%d\n", NR, a[int(NR*0.5)], a[int(NR*0.9)], a[int(NR*0.99)], a[NR]}'
echo "=== loop process ==="
pgrep -af 'soak_v3.sh|soak_monitor.sh' | head
echo "=== monitor ==="
cat monitor.tsv
echo "=== leak so far ==="
cat leak.tsv 2>/dev/null
echo "=== now ==="
date '+%F %T'
