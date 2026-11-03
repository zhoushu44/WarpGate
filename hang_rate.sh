#!/bin/bash
# Quantify the spin-hang rate: N rounds of 200 dials, count rounds that never finish.
set -u
DIR=/root/soak
BIN=/root/soak/bin/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
OUT=$DIR/hang_rate.tsv
: > "$OUT"
printf 'round\tstart_iso\treqs\tsummary\tdur_s\thung\n' >> "$OUT"

ROUNDS=${ROUNDS:-14}
for r in $(seq 1 "$ROUNDS"); do
  log=$DIR/rate_$r.log
  t0=$(date +%s)
  timeout 180 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 200 --leak --target httpbin.org:80 > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  reqs=$(grep -c '^REQ' "$log")
  sum=$(grep -c '^SUMMARY' "$log")
  hung=0
  [ "$sum" = "0" ] && hung=1
  printf '%s\t%s\t%s\t%s\t%s\t%s\n' "$r" "$(date '+%F %T')" "$reqs" "$sum" "$((t1-t0))" "$hung" >> "$OUT"
  echo "round=$r rc=$rc reqs=$reqs summary=$sum dur=$((t1-t0))s hung=$hung"
done
echo -e "#HANGDONE\t$(date '+%F %T')" >> "$OUT"
