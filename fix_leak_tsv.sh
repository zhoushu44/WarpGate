#!/bin/bash
# Recompute leak.tsv from existing logs using grep -a (logs contain binary bytes).
set -u
DIR=/root/np
OUT=$DIR/leak.tsv
printf 'total\tok\tfail\treqs\trss_before_kb\trss_after_kb\tdelta_kb\tfd_before\tfd_after\telapsed_s\n' > "$OUT"
for n in 50 100 150 200 250 300 350 400 450 500; do
  f=$DIR/leak_$n.log
  [ -f "$f" ] || continue
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$f" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$f" | wc -l)
  reqs=$(grep -ac '^REQ' "$f")
  b=$(grep -a '^SAMPLE' "$f" | grep -a after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fb=$(grep -a '^SAMPLE' "$f" | grep -a after_bringup | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  a=$(grep -a '^SAMPLE' "$f" | grep -a after_load | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fa=$(grep -a '^SAMPLE' "$f" | grep -a after_load | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  sum=$(grep -ac '^SUMMARY' "$f")
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${b:-0}" "${a:-0}" "$(( ${a:-0} - ${b:-0} ))" "${fb:-0}" "${fa:-0}" "$sum" >> "$OUT"
  echo "n=$n ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0} fd=${fb:-0}->${fa:-0}"
done
echo "--- running? ---"
ps -eo pid,etime,args --no-headers | grep -E 'leak3.sh|nativepress' | grep -v grep | head -3
