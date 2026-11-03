#!/bin/bash
cd /root/soak
echo "=== lines ==="
wc -l nativecheck.tsv
echo "=== last 8 ==="
tail -8 nativecheck.tsv
echo "=== ok/fail ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d\n", o+0, f+0}' nativecheck.tsv
echo "=== reason histogram ==="
awk -F'\t' '$4==0{print $9}' nativecheck.tsv | sort | uniq -c | sort -rn | head -10
echo "=== attempts histogram ==="
awk -F'\t' '{print $7}' nativecheck.tsv | sort -n | uniq -c
echo "=== latency p50/p90/max ==="
awk -F'\t' '{print $5}' nativecheck.tsv | sort -n | awk '{a[NR]=$1} END{printf "n=%d p50=%d p90=%d max=%d\n", NR, a[int(NR*0.5)], a[int(NR*0.9)], a[NR]}'
echo "=== monitor ==="
cat monitor.tsv 2>/dev/null
echo "=== time now ==="
date '+%F %T'
