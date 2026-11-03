#!/bin/bash
# Aggregate the soak into 5-minute windows + monitor stats.
# Usage: analyze.sh [dir]
set -u
DIR=${1:-/root/soak}
T=$DIR/nativecheck.tsv
M=$DIR/monitor.tsv

if [ ! -s "$T" ]; then echo "no data in $T"; exit 1; fi

base=$(awk -F'\t' 'NR==1{print $2}' "$T")

echo "=============================================================="
echo " 5-minute windows (window boundaries from first sample)"
echo "=============================================================="
printf '%-22s %5s %5s %5s %7s %8s %8s %8s %8s\n' \
  "window" "n" "ok" "fail" "rate%" "p50_ms" "p90_ms" "max_ms" "avg_ms"
awk -F'\t' -v base="$base" '
{
  w = int(($2 - base) / 300000) + 1
  n[w]++; if ($4==1) ok[w]++; else fail[w]++
  lat[w, n[w]] = $5
  sum[w] += $5
  if ($5 > max[w]) max[w] = $5
}
END {
  for (i=1; i<=40; i++) {
    if (n[i] == 0) continue
    # insertion sort the window latencies
    for (a=2; a<=n[i]; a++) {
      v = lat[i,a]; b = a-1
      while (b >= 1 && lat[i,b] > v) { lat[i,b+1] = lat[i,b]; b-- }
      lat[i,b+1] = v
    }
    p50 = lat[i, int(n[i]*0.5) > 0 ? int(n[i]*0.5) : 1]
    p90 = lat[i, int(n[i]*0.9) > 0 ? int(n[i]*0.9) : 1]
    lo = (i-1)*5
    printf "T+%02d-%02dmin            %5d %5d %5d %6.1f %8d %8d %8d %8.0f\n", \
      lo, lo+5, n[i], ok[i]+0, fail[i]+0, (ok[i]+0)*100/n[i], p50, p90, max[i], sum[i]/n[i]
  }
}
' "$T"

echo
echo "=============================================================="
echo " overall"
echo "=============================================================="
awk -F'\t' '
{ n++; if ($4==1) ok++; else fail++; sum+=$5; lat[n]=$5 }
END {
  for (a=2; a<=n; a++) { v=lat[a]; b=a-1; while (b>=1 && lat[b]>v) { lat[b+1]=lat[b]; b-- } lat[b+1]=v }
  printf "runs=%d ok=%d fail=%d rate=%.2f%% avg=%.0fms p50=%dms p90=%dms p95=%dms max=%dms\n", \
    n, ok+0, fail+0, (ok+0)*100/n, sum/n, lat[int(n*0.5)], lat[int(n*0.9)], lat[int(n*0.95)], lat[n]
  # span
  printf "first=%s last=%s\n", $3, $3
}' "$T"

echo
echo "--- span / cadence ---"
awk -F'\t' 'NR==1{f=$2} {l=$2; n++} END{printf "first_ms=%d last_ms=%d span_min=%.1f runs=%d mean_gap_s=%.2f\n", f, l, (l-f)/60000, n, (l-f)/1000/(n-1)}' "$T"

echo
echo "--- failure reasons ---"
awk -F'\t' '$4==0{print $8}' "$T" | sort | uniq -c | sort -rn

echo
echo "--- TLS attempt histogram (how many tries a run needed) ---"
awk -F'\t' '{print $7}' "$T" | sort -n | uniq -c

echo
echo "--- failures per 5-min window ---"
awk -F'\t' -v base="$base" '$4==0{w=int(($2-base)/300000)+1; c[w]++} END{for(i=1;i<=40;i++) if(c[i]) printf "T+%02d-%02dmin: %d\n",(i-1)*5,i*5,c[i]}' "$T"

echo
echo "=============================================================="
echo " monitor samples (5 min)"
echo "=============================================================="
if [ -s "$M" ]; then
  column -t -s $'\t' "$M" 2>/dev/null || cat "$M"
else
  echo "no monitor data"
fi
