#!/bin/bash
# STEP 1 (gate): one dial, then an immediate write. The old wiring hung here.
set -u
DIR=/root/np2
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app

echo "=== step1a: 1 dial, immediate write, no delay knob, hard cap 30s ==="
t0=$(date +%s)
timeout 30 env NP_TRACE=1 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 1 \
    --io-timeout 10 --dial-timeout 15 --target www.gstatic.com:80 --path /generate_204 \
    > "$DIR/s1.log" 2>&1
rc=$?
t1=$(date +%s)
echo "rc=$rc wall=$((t1-t0))s"
grep -aE '^(PHASE|REQ|SUMMARY)' "$DIR/s1.log" || echo "  (no phase lines)"
echo "  dial_ok=$(grep -ac 'PHASE.*dial_ok' "$DIR/s1.log") write_ok=$(grep -ac 'PHASE.*write_ok' "$DIR/s1.log") req=$(grep -ac '^REQ' "$DIR/s1.log")"

if [ "$(grep -ac 'PHASE.*write_ok' "$DIR/s1.log")" = "0" ]; then
  echo ">>> GATE FAILED: write never completed. Stopping here as instructed."
  echo "--- last 6 lines ---"
  tail -6 "$DIR/s1.log"
  exit 1
fi
echo ">>> GATE PASSED: write completed"
