#!/bin/bash
# Reproduce the "all 8 inflight dials time out together, then recover" stall.
# A 20 s hang on all workers at once points at the tunnel/H2 layer, not the
# smoltcp socket: if it were the socket waker bug, it would be one task and
# permanent. Run several 1000-dial rounds and record every burst.
set -u
DIR=/root/np2
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-5/usque.json}
OUT=$DIR/burst.tsv
printf 'run\ttotal\tok\tfail\tdial_to\tread_to\treq_to\tmax_stall_gap_s\twall_s\tburst\n' > "$OUT"

for r in 1 2 3 4 5 6; do
  log=$DIR/burst_$r.log
  t0=$(date +%s)
  "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total 1000 \
      --io-timeout 8 --dial-timeout 12 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  dt=$(awk -F'\t' '$1=="REQ"&&$9 ~ /dial_timeout/' "$log" | wc -l)
  rt=$(awk -F'\t' '$1=="REQ"&&$9 ~ /read_timeout/' "$log" | wc -l)
  qt=$(awk -F'\t' '$1=="REQ"&&$9 ~ /req_timeout/' "$log" | wc -l)
  # largest gap between consecutive REQ completions (a burst shows as ~timeout)
  gap=$(awk -F'\t' '$1=="REQ"{ if(p) { d=$5/1000; if(d>m) m=d } p=1 } END{printf "%.1f", m}' "$log")
  burst="no"
  [ "$dt" -gt 3 ] && burst="YES"
  [ "$rt" -gt 3 ] && burst="YES"
  [ "$qt" -gt 0 ] && burst="YES"
  printf '%s\t1000\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$r" "$ok" "$fail" "$dt" "$rt" "$qt" "$gap" "$((t1-t0))" "$burst" >> "$OUT"
  echo "run=$r rc=$rc ok=$ok fail=$fail dial_to=$dt read_to=$rt req_to=$qt max_gap=${gap}s wall=$((t1-t0))s burst=$burst"
done
echo "--- table ---"; cat "$OUT"
