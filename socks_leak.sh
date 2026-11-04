#!/bin/bash
# Precise socket-count leak probe with the instrumented build.
# Same shape as the baseline: concurrency 8, 500 dials, sample every 50.
set -u
DIR=/root/np2
BIN=/root/np2/instr_target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-2/usque.json}
OUT=$DIR/socks.tsv
printf 'total\tok\tfail\treqs\tsocks\trss_kb\tfd\telapsed_s\n' > "$OUT"
md5sum "$BIN" > "$DIR/socks_bin.md5"

for n in 50 100 150 200 250 300 350 400 450 500; do
  log=$DIR/socks_$n.log
  t0=$(date +%s)
  "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total "$n" \
      --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -ac '^REQ' "$log")
  e=$(grep -a '^SAMPLE' "$log" | grep -a '	end	')
  sk=$(echo "$e" | sed -n 's/.*socks=\([0-9]*\).*/\1/p')
  rs=$(echo "$e" | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fd=$(echo "$e" | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${sk:-0}" "${rs:-0}" "${fd:-0}" "$((t1-t0))" >> "$OUT"
  echo "n=$n rc=$rc ok=$ok fail=$fail reqs=$reqs socks=${sk:-0} rss=${rs:-0} fd=${fd:-0} wall=$((t1-t0))s"
done
echo "#DONE $(date '+%F %T')" >> "$OUT"
echo "=== ALSO: peak socks seen mid-run (running sample) ==="
for n in 500; do
  grep -a '^SAMPLE' "$DIR/socks_$n.log"
done
