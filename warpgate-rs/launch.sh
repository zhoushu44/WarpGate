#!/bin/bash
# Launch warpgate detached, then poll until the console answers.
ulimit -n 65535
pkill -9 -f 'target/release/warpgate' 2>/dev/null || true
pkill -9 usque 2>/dev/null || true
sleep 3
: > /root/warpgate-rs/warpgate.log
cd /root/warpgate-rs/app
export WARP_NATIVE=quic

nohup ./target/release/warpgate \
  --config /root/warpgate-rs/rust-config.json \
  --instances /root/warpgate-rs/instances \
  >> /root/warpgate-rs/warpgate.log 2>&1 &
PID=$!
disown $PID

for i in $(seq 1 30); do
  sleep 2
  CODE=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)
  if [ "$CODE" = "200" ]; then
    echo "READY after $((i*2))s (pid=$PID)"
    exit 0
  fi
done

echo "NOT READY after 60s (pid=$PID)"
tail -10 /root/warpgate-rs/warpgate.log
