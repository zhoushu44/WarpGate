#!/bin/bash
set -u
echo "=================== A. SOURCE VERSIONS ==================="
echo "aa960002555011e6edf3ea880a43678c  656 lines  MIS-FIXED (register only; wake_all/queue_removal defined but never called)"
echo "095951dd1b26b6c4f313ecc5a495bcb1  760 lines  FIXED (pump_once drains pending_removal + wake_all; Drop queues + wakes)"
echo "-- current --"
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs

echo
echo "=================== B. FILE MANIFEST ==================="
printf '%-42s %-19s %s\n' FILE MTIME BYTES
for f in /root/np/hr20.tsv /root/np/hrfix.tsv /root/np/leak.tsv /root/np/leak2000.tsv \
         /root/np/leak_socks.tsv /root/np2/hr2.tsv /root/np2/leak.tsv /root/np2/socks.tsv \
         /root/np2/soak.tsv /root/np2/burst.tsv; do
  if [ -f "$f" ]; then
    printf '%-42s %-19s %s\n' "$f" "$(date -r "$f" '+%F %T')" "$(stat -c%s "$f")"
  fi
done

echo
echo "=== C. np/hr20.tsv  (MIS-FIXED, 20 rounds) ==="
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($11==1)h++; ok+=$3} END{printf "rounds=%d hung=%d rate=%.0f%% ok=%d\n",n,h,h*100/n,ok}' /root/np/hr20.tsv

echo
echo "=== D. np2/hr2.tsv  (FIXED, 20 rounds) ==="
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($11==1)h++; ok+=$3; fail+=$4; w+=$6; if($7>m)m=$7} END{printf "rounds=%d hung=%d rate=%.0f%% ok=%d fail=%d avg_wall=%.1fs cpu_peak_max=%d%%\n",n,h,h*100/n,ok,fail,w/n,m}' /root/np2/hr2.tsv

echo
echo "=== E. np/leak_socks.tsv (MIS-FIXED, delay hack) ==="
cat /root/np/leak_socks.tsv

echo
echo "=== F. np2/socks.tsv (FIXED, instrumented copy) ==="
cat /root/np2/socks.tsv

echo
echo "=== G. np/leak2000.tsv (MIS-FIXED) ==="
cat /root/np/leak2000.tsv

echo
echo "=== H. np2/soak.tsv (FIXED) ==="
cat /root/np2/soak.tsv
