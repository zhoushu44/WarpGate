#!/bin/bash
# Long RSS sweep: 2000 dials, concurrency 8, RSS sampled every 100.
# A leaked smoltcp socket keeps 128 KiB of buffers (64K rx + 64K tx) alive, so
# a 46->360 style leak would show up as ~40 MB, far above allocator noise.
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-3/usque.json
cd /root/warpgate-rs/app
OUT=$DIR/leak2000.tsv
printf 'total\tok\tfail\treqs\trss_before_kb\trss_final_kb\tdelta_kb\tfd\telapsed_s\n' > "$OUT"

log=$DIR/leak2000.log
t0=$(date +%s)
env NP_DELAY_MS=200 "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total 2000 \
    --io-timeout 1 --req-timeout 12 --dial-timeout 10 \
    --target www.gstatic.com:80 --path /generate_204 > "$log" 2>&1
rc=$?
t1=$(date +%s)
b=$(grep -a '^SAMPLE' "$log" | grep -a after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
a=$(grep -a '^SAMPLE' "$log" | grep -a after_load | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
f=$(grep -a '^SAMPLE' "$log" | grep -a after_load | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
reqs=$(grep -ac '^REQ' "$log")
printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' 2000 "$ok" "$fail" "$reqs" \
  "${b:-0}" "${a:-0}" "$(( ${a:-0} - ${b:-0} ))" "${f:-0}" "$((t1-t0))" >> "$OUT"
echo "rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0} fd=${f:-0} wall=$((t1-t0))s"

echo "=== phase tally (all 500/1000/2000 logs) ==="
for l in "$DIR"/leak2000.log "$DIR"/leak_500.log; do
  [ -f "$l" ] || continue
  echo "-- $l"
  awk -F'\t' '$1=="REQ"{p[$8]++; if($4==1){s[$5]++}} END{for(k in p) print "  phase="k, p[k]}' "$l"
  awk -F'\t' '$1=="REQ"&&$4==1{n++; sum+=$5; if($5<min||min==0)min=$5; if($5>max)max=$5} END{printf "  success latency: n=%d avg=%.0fms min=%dms max=%dms\n", n, sum/n, min, max}' "$l"
done
