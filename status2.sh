#!/bin/bash
cd /root/soak
echo "=== progress ==="
wc -l < nativecheck.tsv
echo "=== ok/fail ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d rate=%.1f%%\n", o+0, f+0, (o+0)*100/((o+f)>0?(o+f):1)}' nativecheck.tsv
echo "=== last 3 ==="
tail -3 nativecheck.tsv
echo "=== monitor rows ==="
cat monitor.tsv
echo "=== failures by minute ==="
awk -F'\t' '$4==0{split($3,t," "); m=substr(t[2],1,5); c[m]++} END{for(k in c) print k, c[k]}' nativecheck.tsv | sort | tail -12
echo "=== now ==="
date '+%F %T'
echo "=== loop alive? ==="
pgrep -f 'bash /root/soak_run.sh' >/dev/null && echo RUNNING || echo STOPPED
