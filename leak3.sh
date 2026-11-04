#!/bin/bash
# Socket-leak probe, RSS/fd-only (this build has no socket_count()).
#
# Two harness-only relief valves, neither of which touches socket
# allocation/removal:
#   NP_DELAY_MS=200  let the smoltcp handshake finish before the first write
#                    (otherwise the deployed build's poll_write never wakes)
#   --req-timeout    bound the whole request; production has no such bound,
#                    which is why a stuck poll_write wedges a task forever
# --io-timeout is the underlying baseline: poll_read only returns at a timer.
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-2/usque.json}
cd /root/warpgate-rs/app
mkdir -p "$DIR"
OUT=$DIR/leak.tsv
printf 'total\tok\tfail\treqs\trss_before_kb\trss_after_kb\tdelta_kb\tfd_before\tfd_after\telapsed_s\n' > "$OUT"
md5sum "$BIN" > "$DIR/leak_bin.md5"

for n in 50 100 150 200 250 300 350 400 450 500; do
  log=$DIR/leak_$n.log
  t0=$(date +%s)
  env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total "$n" \
      --io-timeout 1 --req-timeout 12 --dial-timeout 10 \
      --target www.gstatic.com:80 --path /generate_204 > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -c '^REQ' "$log")
  b=$(grep '^SAMPLE' "$log" | grep after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fb=$(grep '^SAMPLE' "$log" | grep after_bringup | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  a=$(grep '^SAMPLE' "$log" | grep after_load | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fa=$(grep '^SAMPLE' "$log" | grep after_load | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${b:-0}" "${a:-0}" "$(( ${a:-0} - ${b:-0} ))" "${fb:-0}" "${fa:-0}" "$((t1-t0))" >> "$OUT"
  echo "n=$n rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0} fd=${fb:-0}->${fa:-0} wall=$((t1-t0))s"
done
echo "#LEAKDONE $(date '+%F %T')" >> "$OUT"
