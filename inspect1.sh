#!/bin/bash
cd /root/soak
echo "=== pilot v1 summary ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d rate=%.1f%%\n", o+0, f+0, (o+0)*100/((o+f)>0?(o+f):1)}' pilot_v1.tsv 2>/dev/null
echo "=== pilot fails detail ==="
awk -F'\t' '$4==0{print $1, $5"ms", "att="$7, $8}' pilot_v1.tsv 2>/dev/null | head -20
echo
echo "=== current soak progress ==="
wc -l nativecheck.tsv
tail -3 nativecheck.tsv
echo "=== current ok/fail ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d\n", o+0, f+0}' nativecheck.tsv
echo "=== sample failed raw log (first fail) ==="
f=$(awk -F'\t' '$4==0{print $1; exit}' nativecheck.tsv)
if [ -n "$f" ]; then cat raw/$(printf '%04d' "$f").log | head -30; fi
echo "=== sample success raw log ==="
s=$(awk -F'\t' '$4==1{print $1; exit}' nativecheck.tsv)
if [ -n "$s" ]; then cat raw/$(printf '%04d' "$s").log | head -25; fi
