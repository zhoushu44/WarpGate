#!/bin/bash
BIN=/root/warpgate-rs/app/target/release/nativecheck
CFG=/root/warpgate-rs/instances/warp-1/usque.json
for i in 1 2 3; do
  t0=$(date +%s%3N)
  out=$(timeout 90 $BIN "$CFG" 2>&1)
  rc=$?
  t1=$(date +%s%3N)
  echo "run=$i rc=$rc ms=$((t1-t0))"
  echo "$out" | grep -E '^tunnel:|^http:|tls: FAILED|connect-ip: FAILED'
  echo "attempts=$(echo "$out" | grep -c 'tls connecting')"
  echo "----"
done
