#!/bin/bash
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app
echo "### does the read finish at io_timeout? try io=3,6 with delay=200"
for io in 3 6; do
  timeout 60 env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 2 \
     --io-timeout $io --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
     > "$DIR/io_$io.log" 2>&1
  echo "io=$io rc=$?"
  awk -F'\t' '$1=="REQ"{print "  REQ",$2,"ok="$4,$5"ms","bytes="$6,"status="$7,"phase="$8,$9}' "$DIR/io_$io.log"
done
echo "### same but concurrency=8, total=16, io=6"
t0=$(date +%s)
timeout 120 env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total 16 \
   --io-timeout 6 --dial-timeout 15 --target www.gstatic.com:80 --path /generate_204 > "$DIR/c8.log" 2>&1
rc=$?
echo "rc=$rc wall=$(( $(date +%s)-t0 ))s"
awk -F'\t' '$1=="REQ"{print "  REQ",$2,"ok="$4,$5"ms","status="$7}' "$DIR/c8.log"
grep '^SUMMARY' "$DIR/c8.log"
