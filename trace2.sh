#!/bin/bash
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app
echo "### trace, delay=200ms, total=2, io=25s"
timeout 90 env NP_TRACE=1 NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 2 \
  --io-timeout 25 --dial-timeout 30 --target www.gstatic.com:80 --path /generate_204 > "$DIR/tr2.log" 2>&1
echo "rc=$?"
grep -E '^(PHASE|REQ|SUMMARY)' "$DIR/tr2.log"
echo "### trace, delay=0, total=1, io=25s (expect stuck in write)"
timeout 45 env NP_TRACE=1 NP_DELAY_MS=0 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 1 \
  --io-timeout 25 --dial-timeout 30 --target www.gstatic.com:80 --path /generate_204 > "$DIR/tr3.log" 2>&1
echo "rc=$?"
grep -E '^(PHASE|REQ|SUMMARY)' "$DIR/tr3.log"
