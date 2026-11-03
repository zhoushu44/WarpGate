#!/bin/bash
# Quick probe: how many dials can the deployed build complete at all?
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app
for c in 1 4 8; do
  echo "### concurrency=$c total=20 io=8s cap=90s"
  t0=$(date +%s)
  timeout 90 "$BIN" "$CFG" --tunnels 1 --concurrency $c --total 20 --io-timeout 8 --dial-timeout 10 \
      --target www.gstatic.com:80 --path /generate_204 > "$DIR/probe_c$c.log" 2>&1
  rc=$?
  t1=$(date +%s)
  echo "rc=$rc wall=$((t1-t0))s reqs=$(grep -c '^REQ' "$DIR/probe_c$c.log") ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$DIR/probe_c$c.log" | wc -l) summary=$(grep -c '^SUMMARY' "$DIR/probe_c$c.log")"
  grep '^SUMMARY' "$DIR/probe_c$c.log"
  grep '^REQ' "$DIR/probe_c$c.log" | head -5
done
