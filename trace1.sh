#!/bin/bash
# Phase-trace one dial to find exactly where the native path blocks.
set -u
DIR=/root/np
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app
pkill -f 'release/nativepress' 2>/dev/null
sleep 1
echo "=== dial 1, trace ==="
timeout 70 env NP_TRACE=1 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 2 --io-timeout 15 > "$DIR/trace.log" 2>&1
echo "rc=$?"
grep -E '^(PHASE|REQ|SUMMARY|SAMPLE|PROGRESS)' "$DIR/trace.log" | tail -25
echo "=== count ==="
echo "dial_ok=$(grep -c 'PHASE.*dial_ok' "$DIR/trace.log") write_ok=$(grep -c 'PHASE.*write_ok' "$DIR/trace.log") read_start=$(grep -c 'PHASE.*read_start' "$DIR/trace.log") req=$(grep -c '^REQ' "$DIR/trace.log")"
