#!/bin/bash
set -u
APP=/root/warpgate-rs/app
echo "=== nativepress source (socket_count usage) ==="
sed -n '40,70p' $APP/crates/wg-bin/src/bin/nativepress.rs
echo
echo "=== baseline nativecheck x3 on warp-1 (fixed code) ==="
for i in 1 2 3; do
  t0=$(date +%s%3N)
  out=$(timeout 120 $APP/target/release/nativecheck /root/warpgate-rs/instances/warp-1/usque.json 2>&1)
  rc=$?
  t1=$(date +%s%3N)
  up=$(echo "$out" | grep -c '^tunnel: UP')
  hb=$(echo "$out" | sed -n 's/^http: \([0-9]*\) bytes.*/\1/p' | head -1)
  att=$(echo "$out" | grep -c 'tls connecting')
  echo "run=$i rc=$rc ms=$((t1-t0)) tunnel_up=$up http_bytes=${hb:-0} attempts=$att"
done
echo
echo "=== nativepress quick 10-dial smoke (warp-1) ==="
timeout 300 $APP/target/release/nativepress /root/warpgate-rs/instances/warp-1/usque.json --tunnels 1 --concurrency 1 --total 10 --leak 2>/dev/null | grep -E '^(SAMPLE|SUMMARY|TUNNEL)'
echo
date '+%F %T'
