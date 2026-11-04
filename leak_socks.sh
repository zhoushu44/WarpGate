#!/bin/bash
# Precise SocketSet leak probe, using the temporary read-only socket_count().
# Mirror of the original baseline: concurrency 8, 500 dials, sample every 50.
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-3/usque.json}
cd /root/warpgate-rs/app
OUT=$DIR/leak_socks.tsv
printf 'total\tok\tfail\treqs\trss_kb\tsocks\tfd\telapsed_s\n' > "$OUT"
md5sum "$BIN" > "$DIR/leak_socks_bin.md5"

for n in 50 100 150 200 250 300 350 400 450 500; do
  log=$DIR/socks_$n.log
  t0=$(date +%s)
  env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total "$n" \
      --io-timeout 1 --req-timeout 12 --dial-timeout 10 \
      --target www.gstatic.com:80 --path /generate_204 > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -ac '^REQ' "$log")
  # `end` sample is taken 500 ms after all workers finished: exactly what the
  # original baseline (socks 46 -> 360) measured.
  e=$(grep -a '^SAMPLE' "$log" | grep -a '	end	')
  rss=$(echo "$e" | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  socks=$(echo "$e" | sed -n 's/.*socks=\([0-9]*\).*/\1/p')
  fd=$(echo "$e" | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${rss:-0}" "${socks:-0}" "${fd:-0}" "$((t1-t0))" >> "$OUT"
  echo "n=$n rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${rss:-0} socks=${socks:-0} fd=${fd:-0} wall=$((t1-t0))s"
done
echo "#DONE $(date '+%F %T')" >> "$OUT"
