#!/bin/bash
set -u
cd /root/soak
echo "=== a 433-byte response ==="
f=$(awk -F'\t' '$6==433{print $1; exit}' nativecheck.tsv)
echo "file=raw/$(printf '%04d' "$f").log"
sed -n '/^body:/,$p' raw/$(printf '%04d' "$f").log | head -20
echo
echo "=== a 225-byte response ==="
g=$(awk -F'\t' '$6==225{print $1; exit}' nativecheck.tsv)
sed -n '/^body:/,$p' raw/$(printf '%04d' "$g").log | head -20
echo
echo "=== first 433 run seq/time ==="
awk -F'\t' '$6==433{print $1, $3; exit}' nativecheck.tsv
echo "=== 225/433 interleave (last 40) ==="
tail -40 nativecheck.tsv | awk -F'\t' '{printf "%s:%s ", $1, $6} END{print ""}'
echo
echo "=== warpgate pid / restart evidence ==="
pgrep -a -x warpgate | head -2
ps -o pid,lstart,etime,rss,nlwp -p $(pgrep -x warpgate | head -1)
echo "=== service restart time ==="
systemctl show warpgate -p ActiveEnterTimestamp -p NRestarts 2>/dev/null
echo "=== usque children ==="
pgrep -c -x usque
echo "=== recent log (native path?) ==="
tail -6 /root/warpgate-rs/warpgate.log
