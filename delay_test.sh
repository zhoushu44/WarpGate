#!/bin/bash
# Prove the mechanism: if a blocked poll_write never gets woken, then sleeping
# before the first write (letting the handshake finish) makes it stop hanging.
set -u
DIR=/root/np
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app

for d in 0 5 20 200; do
  echo "### NP_DELAY_MS=$d"
  timeout 45 env NP_DELAY_MS=$d "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 5 \
      --io-timeout 10 --dial-timeout 15 --target www.gstatic.com:80 --path /generate_204 \
      > "$DIR/delay_$d.log" 2>&1
  rc=$?
  echo "rc=$rc reqs=$(grep -c '^REQ' "$DIR/delay_$d.log") ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$DIR/delay_$d.log" | wc -l) summary=$(grep -c '^SUMMARY' "$DIR/delay_$d.log")"
  grep '^SUMMARY' "$DIR/delay_$d.log" | head -1
  awk -F'\t' '$1=="REQ"{print "  REQ",$2,"ok="$4,$5"ms","bytes="$6,"status="$7,"phase="$8,$9}' "$DIR/delay_$d.log" | head -6
done
