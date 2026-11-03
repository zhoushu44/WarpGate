#!/bin/bash
set -u
cd /root/soak
echo "=== lane1 final ==="
wc -l < nativecheck.tsv
cat loop.start
echo
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d rate=%.2f%%\n", o+0, f+0, (o+0)*100/((o+f)>0?(o+f):1)}' nativecheck.tsv
echo "=== span ==="
awk -F'\t' 'NR==1{f=$2} {l=$2;n++} END{printf "first=%s last=%s span_min=%.1f n=%d mean_gap_s=%.2f\n", f, l, (l-f)/60000, n, (l-f)/1000/(n-1)}' nativecheck.tsv
echo
echo "=== repro progress ==="
cat repro.out 2>/dev/null
echo
echo "=== repro logs so far ==="
ls -l --time-style=+%T hang_*.log 2>/dev/null
for f in hang_*.log; do
  [ -f "$f" ] || continue
  echo "--- $f reqs=$(grep -c '^REQ' $f) summary=$(grep -c '^SUMMARY' $f) last=$(grep '^SAMPLE.*running' $f | tail -1)"
done
echo
echo "=== lane2 still running? ==="
pgrep -af lane2.sh | head -2
tail -2 lane2.tsv
echo
echo "=== now ==="
date '+%F %T'
