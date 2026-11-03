#!/bin/bash
set -u
BIN=/root/soak/bin/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json

echo "=== controlled repro attempt: 6 x 200 dials, watch for a run that stalls ==="
for r in 1 2 3 4 5 6; do
  log=/root/soak/hang_$r.log
  t0=$(date +%s)
  timeout 300 "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 200 --leak --target httpbin.org:80 > "$log" 2>&1
  rc=$?
  t1=$(date +%s)
  dur=$((t1-t0))
  reqs=$(grep -c '^REQ' "$log")
  ok=$(grep -c '^REQ.*	1	' "$log")
  sum=$(grep -c '^SUMMARY' "$log")
  last=$(grep '^SAMPLE.*running' "$log" | tail -1)
  echo "run=$r rc=$rc dur=${dur}s reqs=$reqs summary=$sum last_running=[$last]"
  if [ "$sum" = "0" ] || [ "$reqs" -lt 200 ]; then
    echo "  >>> STALLED. tail:"
    tail -12 "$log"
    echo "  >>> stack of the stuck process is gone (timeout killed it), but log preserved at $log"
  fi
done
echo
echo "=== done ==="
date '+%F %T'
