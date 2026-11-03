#!/bin/bash
set -u
cd /root/soak
echo "=== lane1 progress ==="
wc -l < nativecheck.tsv
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d rate=%.2f%%\n", o+0, f+0, (o+0)*100/((o+f)>0?(o+f):1)}' nativecheck.tsv
echo "=== HTTP status mix ==="
for f in raw/*.log; do sed -n 's/^body: HTTP\/1\.[0-9] \([0-9][0-9][0-9]\).*/\1/p' "$f" | head -1; done | sort | uniq -c
echo "=== lane2 ==="
cat lane2.tsv 2>/dev/null
echo "=== lane2 last log ==="
tail -20 $(ls -t lane2_*.log 2>/dev/null | head -1) 2>/dev/null | grep -E 'SAMPLE|SUMMARY|REQ' | tail -6
echo "=== monitor ==="
cat monitor.tsv
echo "=== procs ==="
pgrep -af 'soak_v3|soak_monitor|lane2|leak_probe' | head
echo "=== now ==="
date '+%F %T'
