#!/bin/bash
# Lane 2: high-volume, clean-target native stress on the fixed binary.
# nativecheck hardcodes api.ipify.org:80, which started returning 429 after ~150
# requests (per-egress-IP rate limit — a test artefact, not a tunnel fault).
# nativepress accepts --target, so use a host that does not rate-limit, and run
# many dials inside ONE process (which is also the only way to see socket leaks).
set -u
DIR=/root/soak
BIN=/root/soak/bin/nativepress
OUT=$DIR/lane2.tsv
: > "$OUT"
printf 'round\tstart_iso\ttotal\tok\tfail\twall_ms\trss_before_kb\trss_after_kb\tsocks_before\tsocks_after\tfd_before\tfd_after\n' >> "$OUT"

TARGET=${TARGET:-www.cloudflare.com:80}
CFG=${CFG:-/root/warpgate-rs/instances/warp-1/usque.json}
ROUND_END=$(( $(date +%s) + 1500 ))

r=0
while [ "$(date +%s)" -lt "$ROUND_END" ]; do
  r=$((r+1))
  log=$DIR/lane2_$r.log
  timeout 400 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 100 --leak --target "$TARGET" > "$log" 2>&1
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
    printf '%s\t%s\t100\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
      "$r" "$(date '+%F %T')" "${ok:-0}" "${fail:-0}" "${wall:-0}" \
      "${r0:-0}" "${r1:-0}" "${s0:-0}" "${s1:-0}" "${f0:-0}" "${f1:-0}" >> "$OUT"
  else
    printf '%s\t%s\t100\tTUNNEL_FAIL\t-\t-\t-\t-\t-\t-\t-\t-\n' "$r" "$(date '+%F %T')" >> "$OUT"
    sleep 3
  fi
done
echo -e "#LANE2DONE\t$(date '+%F %T')" >> "$OUT"
