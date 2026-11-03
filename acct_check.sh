#!/bin/bash
# Rule out account-specific breakage: one dial on warp-1..warp-5.
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
cd /root/warpgate-rs/app
for i in 1 2 3 4 5; do
  CFG=/root/warpgate-rs/instances/warp-$i/usque.json
  t0=$(date +%s)
  timeout 30 env NP_TRACE=1 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 1 \
      --io-timeout 8 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$DIR/acct_$i.log" 2>&1
  rc=$?
  t1=$(date +%s)
  dial=$(grep -c 'PHASE.*dial_ok' "$DIR/acct_$i.log")
  wr=$(grep -c 'PHASE.*write_ok' "$DIR/acct_$i.log")
  req=$(grep -c '^REQ' "$DIR/acct_$i.log")
  echo "warp-$i rc=$rc wall=$((t1-t0))s dial_ok=$dial write_ok=$wr req=$req"
done
