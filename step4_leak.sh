#!/bin/bash
# STEP 4: socket leak, concurrency 8, 500 dials, sample every 50.
# Uses the deployed build as-is (no socket_count() in this revision), so the
# evidence is RSS + fd + a post-run settle check.
set -u
DIR=/root/np2
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-2/usque.json}
cd /root/warpgate-rs/app
OUT=$DIR/leak.tsv
printf 'total\tok\tfail\treqs\trss_before_kb\trss_after_kb\tdelta_kb\trss_settle_kb\tsettle_delta_kb\tfd_before\tfd_after\telapsed_s\n' > "$OUT"

for n in 50 100 150 200 250 300 350 400 450 500; do
  log=$DIR/leak_$n.log
  t0=$(date +%s)
  env "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total "$n" \
      --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -ac '^REQ' "$log")
  b=$(grep -a '^SAMPLE' "$log" | grep -a after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  a=$(grep -a '^SAMPLE' "$log" | grep -a '	after_load	' | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  s=$(grep -a '^SAMPLE' "$log" | grep -a '	end	' | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  fb=$(grep -a '^SAMPLE' "$log" | grep -a after_bringup | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  fa=$(grep -a '^SAMPLE' "$log" | grep -a '	after_load	' | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$n" "$ok" "$fail" "$reqs" \
    "${b:-0}" "${a:-0}" "$(( ${a:-0} - ${b:-0} ))" "${s:-0}" "$(( ${s:-0} - ${a:-0} ))" \
    "${fb:-0}" "${fa:-0}" "$((t1-t0))" >> "$OUT"
  echo "n=$n rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0}(settle ${s:-0}) fd=${fb:-0}->${fa:-0} wall=$((t1-t0))s"
done
echo "#LEAKDONE $(date '+%F %T')" >> "$OUT"
