#!/bin/bash
set -u
BIN=/root/soak/bin/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
for tgt in example.com:80 neverssl.com:80 www.example.com:80 httpbin.org:80 api.ipify.org:80; do
  out=$(timeout 120 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 3 --target "$tgt" 2>/dev/null)
  code=$(echo "$out" | awk -F'\t' '$1=="REQ"{print $6}' | sort | uniq -c | tr '\n' ' ')
  err=$(echo "$out" | awk -F'\t' '$1=="REQ" && $3=="0"{print $7}' | head -1)
  echo "$tgt -> statuses: ${code:-none} err=${err:-none}"
done
