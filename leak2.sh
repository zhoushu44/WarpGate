#!/bin/bash
# Socket-leak probe, RSS/fd-only (this build has no socket_count()).
#
# METHOD CAVEAT (important, and it is itself a finding): the deployed build never
# wakes a blocked poll_read/poll_write (StackShared::wake_all has zero call
# sites), so a request only completes when a timeout timer happens to re-poll the
# task. The first write hangs forever unless the handshake already finished,
# hence NP_DELAY_MS=200; the read then returns only at io-timeout, hence
# io-timeout=1 to keep the sweep usable. Neither knob touches socket
# allocation/removal, which is what we are measuring.
#
# Because a residual writer can stay stuck forever, the round is capped by
# `timeout` and the final numbers are taken from the last PROGRESS heartbeat.
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
  timeout $(( n / 8 * 3 + 120 )) env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 \
      --concurrency 8 --total "$n" --io-timeout 1 --dial-timeout 10 \
      --target www.gstatic.com:80 --path /generate_204 > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -c '^REQ' "$log")
  b=$(grep '^SAMPLE' "$log" | grep after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fb=$(grep '^SAMPLE' "$log" | grep after_bringup | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  last=$(grep '^PROGRESS' "$log" | tail -1)
  a=$(echo "$last" | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fa=$(echo "$last" | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${b:-0}" "${a:-0}" "$(( ${a:-0} - ${b:-0} ))" "${fb:-0}" "${fa:-0}" "$((t1-t0))" >> "$OUT"
  echo "n=$n rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0} fd=${fb:-0}->${fa:-0} wall=$((t1-t0))s"
done
echo "#LEAKDONE $(date '+%F %T')" >> "$OUT"
