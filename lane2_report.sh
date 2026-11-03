#!/bin/bash
set -u
cd /root/soak
echo "=== lane2 round 26 (TUNNEL_FAIL) ==="
cat lane2_26.log 2>/dev/null | grep -E 'SAMPLE|SUMMARY|TUNNEL|FATAL' | head -10
echo
echo "=== lane2 round 25 (before) vs 27 (after) ==="
grep -h '^SUMMARY' lane2_25.log lane2_27.log 2>/dev/null
echo
echo "=== lane2 fail rounds ==="
awk -F'\t' '$4=="TUNNEL_FAIL" || $5!="0"' lane2.tsv
echo
echo "=== lane2 aggregate ==="
awk -F'\t' 'NR>1 && $3==100 && $4!="TUNNEL_FAIL" {rounds++; ok+=$4; fail+=$5; if($6!="-"){w+=$6; wn++} if($9!=0) sbad++} END{printf "rounds=%d ok=%d fail=%d rate=%.2f%% avg_wall=%.0fms rounds_with_socks_leak=%d\n", rounds, ok, fail, ok*100/(ok+fail), w/wn, sbad+0}' lane2.tsv
echo
echo "=== lane2 fd/socks never changed? ==="
awk -F'\t' 'NR>1 && $3==100 && $4!="TUNNEL_FAIL" {print $9, $10, $11, $12}' lane2.tsv | sort | uniq -c
echo
echo "=== lane2 rss trend ==="
awk -F'\t' 'NR>1 && $3==100 && $4!="TUNNEL_FAIL" {print $2, $7, $8}' lane2.tsv | tail -8
echo
echo "=== now ==="
date '+%F %T'
