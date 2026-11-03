#!/bin/bash
# Start the official 30-minute soak on the FIXED binaries.
set -u
pkill -f 'bash /root/soak_v3.sh' 2>/dev/null
pkill -f 'bash /root/soak_monitor.sh' 2>/dev/null
pkill -f '/root/soak/bin/nativecheck' 2>/dev/null
sleep 2

APP=/root/warpgate-rs/app
mkdir -p /root/soak/bin /root/soak/raw /root/soak/fixed
# freeze the binaries so a later rebuild cannot silently change the subject
cp -f $APP/target/release/nativecheck /root/soak/bin/nativecheck
cp -f $APP/target/release/nativepress /root/soak/bin/nativepress
chmod +x /root/soak/bin/*

cd /root/soak
rm -f nativecheck.tsv monitor.tsv loop.progress loop.start loop.end
rm -rf raw; mkdir -p raw

# record exactly what we are testing
{
  echo "=== soak subject ==="
  date '+start %F %T %Z'
  echo "nativecheck_md5 $(md5sum /root/soak/bin/nativecheck | cut -d' ' -f1)"
  echo "nativepress_md5 $(md5sum /root/soak/bin/nativepress | cut -d' ' -f1)"
  echo "stack_rs_md5 $(md5sum $APP/crates/wg-tunnel/src/connectip/stack.rs | cut -d' ' -f1)"
  echo "stack_rs_lines $(wc -l < $APP/crates/wg-tunnel/src/connectip/stack.rs)"
  echo "drop_fix $(grep -c 'impl Drop for StackStream' $APP/crates/wg-tunnel/src/connectip/stack.rs)"
  echo "lockorder_fix $(grep -c 'Lock order MUST match' $APP/crates/wg-tunnel/src/connectip/stack.rs)"
  echo "host $(hostname)"
  echo "kernel $(uname -r)"
  echo "nproc $(nproc)"
} > /root/soak/subject.txt
cat /root/soak/subject.txt

setsid nohup env DURATION=1800 GAP_MS=2000 BIN=/root/soak/bin/nativecheck \
  bash /root/soak_v3.sh > /root/soak/loop.out 2>&1 < /dev/null &
sleep 1
setsid nohup env N=7 GAP=300 bash /root/soak_monitor.sh > /root/soak/monitor.out 2>&1 < /dev/null &
sleep 4
echo "=== loop.start ==="
cat /root/soak/loop.start
echo "=== processes ==="
ps -eo pid,etime,cmd | grep -E 'soak_v3|soak_monitor|soak/bin/nativecheck' | grep -v grep
echo "=== first rows ==="
cat /root/soak/nativecheck.tsv 2>/dev/null
