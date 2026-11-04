#!/bin/bash
# Long-run leak confirmation WITHOUT touching the deployed source tree:
# 3000 dials at concurrency 8, RSS sampled by the harness every 2 s.
set -u
DIR=/root/np2
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-3/usque.json}
cd /root/warpgate-rs/app
LOG=$DIR/long3000.log
t0=$(date +%s)
env "$BIN" "$CFG" --tunnels 1 --concurrency 8 --total 3000 \
    --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
    > "$LOG" 2>&1
rc=$?
t1=$(date +%s)
ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$LOG" | wc -l)
fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$LOG" | wc -l)
reqs=$(grep -ac '^REQ' "$LOG")
b=$(grep -a '^SAMPLE' "$LOG" | grep -a after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
a=$(grep -a '^SAMPLE' "$LOG" | grep -a '	after_load	' | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
s=$(grep -a '^SAMPLE' "$LOG" | grep -a '	end	' | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
f=$(grep -a '^SAMPLE' "$LOG" | grep -a '	end	' | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
echo "rc=$rc ok=$ok fail=$fail reqs=$reqs rss=${b:-0}->${a:-0}(settle ${s:-0}) fd=${f:-0} wall=$((t1-t0))s"

echo "=== RSS curve (every ~200 done) ==="
printf '%6s %9s\n' done rss_kb
grep -a '^PROGRESS' "$LOG" | awk -F'\t' '
{
  split($2,x,"="); split($6,y,"=");
  d=x[2]+0; r=y[2]+0;
  if (d >= nd) { printf "%6d %9d\n", d, r; nd = d + 200 }
}'
echo "=== peak RSS ==="
grep -a '^PROGRESS' "$LOG" | awk -F'\t' '{split($6,y,"="); if(y[2]+0>m)m=y[2]+0} END{print "peak_rss_kb="m}'
