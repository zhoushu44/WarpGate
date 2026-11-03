#!/bin/bash
# Soak v3: fixed 2 s start grid, time-bounded, frozen binary.
# Columns: seq, t_start_ms, t_start_iso, ok, ms, http_bytes, tls_attempts, reason
set -u
DIR=${DIR:-/root/soak}
BIN=${BIN:-/root/soak/bin/nativecheck}
CFG=${CFG:-/root/warpgate-rs/instances/warp-1/usque.json}
TSV=${TSV:-$DIR/nativecheck.tsv}
RAW=${RAW:-$DIR/raw}
mkdir -p "$RAW"
: > "$TSV"

DURATION=${DURATION:-1800}
GAP_MS=${GAP_MS:-2000}
RUN_TIMEOUT=${RUN_TIMEOUT:-120}

start=$(date +%s%3N)
echo "START $start $(date '+%F %T') dur=${DURATION}s gap=${GAP_MS}ms cfg=$CFG" > $DIR/loop.start
echo "BIN_MD5 $(md5sum "$BIN" | cut -d' ' -f1)" >> $DIR/loop.start
echo "SRC_MD5 $(md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs | cut -d' ' -f1)" >> $DIR/loop.start

i=0
while :; do
  now=$(date +%s%3N)
  elapsed=$(( now - start ))
  [ "$elapsed" -ge $(( DURATION * 1000 )) ] && break

  # fixed 2 s grid: slot index derived from elapsed time
  slot=$(( elapsed / GAP_MS + 1 ))
  i=$((i+1))

  t0=$(date +%s%3N)
  iso=$(date '+%F %T')
  out=$RAW/$(printf '%04d' "$i").log
  timeout "$RUN_TIMEOUT" "$BIN" "$CFG" > "$out" 2>&1
  rc=$?
  t1=$(date +%s%3N)
  ms=$(( t1 - t0 ))

  bytes=$(sed -n 's/^http: \([0-9][0-9]*\) bytes.*/\1/p' "$out" | head -1)
  [ -z "$bytes" ] && bytes=0
  attempts=$(grep -c '^\[native\] tls connecting' "$out")

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
  echo "DONE $i ok=$ok ${ms}ms att=$attempts" >> $DIR/loop.progress

  # hold the grid: if this run finished inside its 2 s slot, wait out the rest
  now=$(date +%s%3N)
  remain=$(( GAP_MS - (now - t0) ))
  if [ "$remain" -gt 0 ]; then
    sleep $(( remain / 1000 ))
    r=$(( remain % 1000 ))
    [ "$r" -gt 0 ] && sleep 0.$(printf '%03d' "$r")
  fi
done
date +%s%3N > $DIR/loop.end
echo "END $(date '+%F %T') runs=$i" >> $DIR/loop.start
