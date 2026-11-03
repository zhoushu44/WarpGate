#!/bin/bash
# Set 500 instances and restart warpgate.
# The config is small and its keys are stable, so plain sed is enough; adding
# a JSON tool to a CentOS 7 box with no python and no jq is not worth it.
CFG=/root/warpgate-rs/rust-config.json

sed -i 's/"resident_count"[[:space:]]*:[[:space:]]*[0-9]*/"resident_count": 500/' "$CFG"
sed -i 's/"max_instances"[[:space:]]*:[[:space:]]*[0-9]*/"max_instances": 500/' "$CFG"
sed -i 's/"max_concurrent_create"[[:space:]]*:[[:space:]]*[0-9]*/"max_concurrent_create": 3/' "$CFG"
sed -i 's/"min_available"[[:space:]]*:[[:space:]]*[0-9]*/"min_available": 50/' "$CFG"

echo "--- config now ---"
grep -E '"(resident_count|max_instances|max_concurrent_create|base_port)"' "$CFG"

echo "--- accounts on disk ---"
ls /root/warpgate-rs/instances | grep -c '^warp-'

echo "--- restarting ---"
pkill -9 -f 'target/release/warpgate' 2>/dev/null || true
pkill -9 -f 'growpool' 2>/dev/null || true
pkill -9 usque 2>/dev/null || true
sleep 3
: > /root/warpgate-rs/warpgate.log
cd /root/warpgate-rs/app
export WARP_NATIVE=quic
ulimit -n 65535
setsid nohup ./target/release/warpgate \
  --config /root/warpgate-rs/rust-config.json \
  --instances /root/warpgate-rs/instances \
  >> /root/warpgate-rs/warpgate.log 2>&1 < /dev/null &
disown
echo "launched pid=$!"
