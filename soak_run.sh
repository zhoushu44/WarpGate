#!/bin/bash
# 30-minute nativecheck soak, time-bounded (default 1800 s).
# One nativecheck per iteration, starting no sooner than GAP_MS after the previous start.
# Columns: seq, t_start_ms, t_start_iso, ok, ms, http_bytes, tls_attempts, reason
set -u
DIR=/root/soak
BIN=${BIN:-$DIR/bin/nativecheck}
CFG=${CFG:-/root/warpgate-rs/instances/warp-1/usque.json}
TSV=${TSV:-$DIR/nativecheck.tsv}
RAW=${RAW:-$DIR/raw}
mkdir -p "$RAW" "$(dirname "$TSV")"
: > "$TSV"

DURATION=${DURATION:-1800}
GAP_MS=${GAP_MS:-2000}
RUN_TIMEOUT=${RUN_TIMEOUT:-120}

start=$(date +%s%3N)
deadline=$(( start + DURATION * 1000 ))
echo "START $start $(date '+%F %T') dur=${DURATION}s gap=${GAP_MS}ms cfg=$CFG bin=$BIN" > $DIR/loop.start
echo "BIN_MD5 $(md5sum "$BIN" | cut -d' ' -f1)" >> $DIR/loop.start
: > $DIR/loop.progress

i=0
while :; do
  now=$(date +%s%3N)
  [ "$now" -ge "$deadline" ] && break
  i=$((i+1))

  # pace: at least GAP_MS between starts
  if [ "$i" -gt 1 ]; then
    target=$(( last_start + GAP_MS ))
    now=$(date +%s%3N)
    rem=$(( target - now ))
    if [ "$rem" -gt 0 ]; then
      sleep $(( rem / 1000 ))
      r=$(( rem % 1000 ))
      if [ "$r" -gt 0 ]; then sleep 0.$(printf '%03d' "$r"); fi
    fi
  fi

  t0=$(date +%s%3N)
  last_start=$t0
  iso=$(date '+%F %T')
  out=$RAW/$(printf '%04d' "$i").log
  timeout "$RUN_TIMEOUT" "$BIN" "$CFG" > "$out" 2>&1
  rc=$?
  t1=$(date +%s%3N)
  ms=$(( t1 - t0 ))

  bytes=$(sed -n 's/^http: \([0-9][0-9]*\) bytes.*/\1/p' "$out" | head -1)
  [ -z "$bytes" ] && bytes=0
  attempts=$(grep -c '^\[native\] tls connecting' "$out")
  peers=$(grep -c '^  peer     =' "$out")

  if grep -q '^tunnel: UP' "$out" && [ "$bytes" -gt 0 ]; then
    ok=1; reason="-"
  else
    ok=0
    reason=$(grep -m1 -E '^tunnel failed:|^tunnel timeout|^dial failed:|^dial timeout' "$out" | cut -c1-120)
    [ -z "$reason" ] && reason="rc=$rc no-http"
    reason=$(echo "$reason" | tr '\t' ' ')
  fi

  printf '%d\t%s\t%s\t%d\t%d\t%s\t%s\t%s\n' \
    "$i" "$t0" "$iso" "$ok" "$ms" "$bytes" "${attempts:-0}" "$reason" >> "$TSV"
  echo "DONE $i ok=$ok ${ms}ms att=$attempts peers=$peers rc=$rc" >> $DIR/loop.progress
done
date +%s%3N > $DIR/loop.end
echo "END $(date '+%F %T') runs=$i" >> $DIR/loop.start
