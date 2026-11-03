#!/bin/bash
# In-process leak probe: one long-lived tunnel, N sequential dials.
# Measures RSS/fd inside the SAME process, which is the only way to see a
# smoltcp socket that is never removed (it is heap state, not an fd).
set -u
DIR=/root/soak
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-3/usque.json}
OUT=$DIR/leak.tsv
: > "$OUT"
printf 'total\ttry\tok\tfail\trss_bringup_kb\trss_after_kb\tdelta_kb\tkb_per_conn\tfd_bringup\tfd_after\n' >> "$OUT"

for total in 50 150 300 600; do
  for try in 1 2 3; do
    log=$DIR/leak_t${total}_${try}.log
    timeout 1800 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total "$total" --leak > "$log" 2>&1
    rc=$?
    if grep -q '^SUMMARY' "$log"; then
      sum=$(grep '^SUMMARY' "$log" | tr '\t' ' ')
      ok=$(echo "$sum" | sed -n 's/.*ok=\([0-9]*\).*/\1/p')
      fail=$(echo "$sum" | sed -n 's/.*fail=\([0-9]*\).*/\1/p')
      r0=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_bringup"{print $5}' "$log" | sed 's/rss_kb=//')
      r1=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_load"{print $5}' "$log" | sed 's/rss_kb=//')
      f0=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_bringup"{print $4}' "$log" | sed 's/fd=//')
      f1=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_load"{print $4}' "$log" | sed 's/fd=//')
      d=$(( ${r1:-0} - ${r0:-0} ))
      per=$(awk -v d="$d" -v n="$ok" 'BEGIN{ if(n>0) printf "%.1f", d/n; else print "0" }')
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$total" "$try" "${ok:-0}" "${fail:-0}" "${r0:-0}" "${r1:-0}" "$d" "$per" "${f0:-0}" "${f1:-0}" >> "$OUT"
      break
    fi
    echo "# retry $total try=$try rc=$rc" >> "$OUT"
    sleep 3
  done
done
echo -e "LEAKDONE\t$(date '+%F %T')" >> "$OUT"
