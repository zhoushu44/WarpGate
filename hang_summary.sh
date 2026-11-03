#!/bin/bash
set -u
cd /root/soak
echo "=== hang_rate summary ==="
cat hang_rate.tsv
echo
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($6==1)h++; ok+=$3} END{printf "rounds=%d hung=%d rate=%.1f%% reqs_completed=%d\n", n, h+0, (h+0)*100/n, ok}' hang_rate.tsv
echo
echo "=== all hang incidences across every experiment ==="
echo "--- lane2 (100/round) ---"
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($4=="TUNNEL_FAIL")h++} END{printf "rounds=%d hung=%d rate=%.1f%%\n", n, h+0, (h+0)*100/n}' lane2.tsv
echo "--- repro_hang + hang_rate (200/round) ---"
awk -F'\t' 'NR>1 && $1 ~ /^[0-9]+$/ {n++; if($6==1)h++} END{printf "hang_rate rounds=%d hung=%d\n", n, h+0}' hang_rate.tsv
echo
echo "=== is it running now? ==="
pgrep -af hang_rate.sh | grep -v grep | head -2
echo "=== now ==="
date '+%F %T'
