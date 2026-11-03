#!/bin/bash
# In-process leak probe on the FIXED code.
# One long-lived tunnel per run, N sequential dials, all inside a single process,
# so a smoltcp socket that is never removed shows up as socks!=0 and RSS growth.
set -u
DIR=/root/soak
BIN=/root/soak/bin/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-2/usque.json}
OUT=$DIR/leak.tsv
: > "$OUT"
printf 'total\tok\tfail\trss_bringup_kb\trss_after_kb\tdelta_kb\tkb_per_conn\tsocks_bringup\tsocks_after\tfd_bringup\tfd_after\twall_ms\n' >> "$OUT"

for total in 30 100 300 600; do
  okrun=0
  for try in 1 2 3 4 5 6; do
    log=$DIR/leak_t${total}_${try}.log
    timeout 1800 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total "$total" --leak > "$log" 2>&1
    rc=$?
    if grep -q '^SUMMARY' "$log"; then
      sum=$(grep '^SUMMARY' "$log" | tr '\t' ' ')
      ok=$(echo "$sum" | sed -n 's/.*ok=\([0-9]*\).*/\1/p')
      fail=$(echo "$sum" | sed -n 's/.*fail=\([0-9]*\).*/\1/p')
      wall=$(echo "$sum" | sed -n 's/.*wall_ms=\([0-9]*\).*/\1/p')
      r0=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_bringup"{print $5}' "$log" | sed 's/rss_kb=//')
      r1=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_load"{print $5}' "$log" | sed 's/rss_kb=//')
      s0=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_bringup"{print $6}' "$log" | sed 's/socks=//')
      s1=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_load"{print $6}' "$log" | sed 's/socks=//')
      f0=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_bringup"{print $4}' "$log" | sed 's/fd=//')
      f1=$(awk -F'\t' '$1=="SAMPLE" && $2=="after_load"{print $4}' "$log" | sed 's/fd=//')
      d=$(( ${r1:-0} - ${r0:-0} ))
      per=$(awk -v d="$d" -v n="${ok:-0}" 'BEGIN{ if(n>0) printf "%.1f", d/n; else print "0" }')
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
        "$total" "${ok:-0}" "${fail:-0}" "${r0:-0}" "${r1:-0}" "$d" "$per" "${s0:-0}" "${s1:-0}" "${f0:-0}" "${f1:-0}" "${wall:-0}" >> "$OUT"
      okrun=1
      break
    fi
    echo "# retry total=$total try=$try rc=$rc" >> "$OUT.bad"
    sleep 3
  done
  [ "$okrun" = "1" ] || echo -e "$total\tTUNNEL_FAIL" >> "$OUT"
done
echo -e "#LEAKDONE\t$(date '+%F %T')" >> "$OUT"
