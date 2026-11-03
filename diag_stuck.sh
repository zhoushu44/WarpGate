#!/bin/bash
# Sample a stuck nativepress: CPU%, per-thread state, and its own PROGRESS line.
set -u
DIR=/root/np
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json

pkill -f 'release/nativepress' 2>/dev/null
sleep 1
cd /root/warpgate-rs/app
setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 2 --io-timeout 15 > "$DIR/stuck.log" 2>&1 < /dev/null &
sleep 4
P=$(pgrep -f 'release/nativepress' | head -1)
echo "pid=$P"
if [ -z "$P" ]; then echo "NOT RUNNING"; tail -20 "$DIR/stuck.log"; exit 1; fi

echo "=== t0 ==="
ps -o pid,pcpu,nlwp,rss,etime,stat -p "$P"
echo "=== t+3s ==="
sleep 3
ps -o pid,pcpu,nlwp,rss,etime,stat -p "$P"
echo "=== per-thread (tid pcpu stat wchan comm) ==="
ps -L -o tid,pcpu,stat,wchan:32,comm -p "$P"
echo "=== thread count in R (running) ==="
ps -L -o stat -p "$P" | grep -c '^R'
echo "=== log tail ==="
grep -v '^\[native\]' "$DIR/stuck.log" | tail -8
echo "=== last 3 PROGRESS ==="
grep '^PROGRESS' "$DIR/stuck.log" | tail -3
echo "=== 10s later: did PROGRESS move? ==="
sleep 10
ps -o pid,pcpu,nlwp,rss,etime,stat -p "$P"
grep '^PROGRESS' "$DIR/stuck.log" | tail -2
echo "=== sample /proc stack of hottest thread ==="
for t in $(ls /proc/$P/task | head -20); do
  st=$(awk '{print $3}' /proc/$P/task/$t/stat 2>/dev/null)
  if [ "$st" = "R" ]; then echo "TID $t state=R"; cat /proc/$P/task/$t/wchan 2>/dev/null; echo; fi
done
pkill -f 'release/nativepress' 2>/dev/null
echo "=== done ==="
