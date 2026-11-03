#!/bin/bash
APP=/root/warpgate-rs/app
echo "=== source state check ==="
stat -c '%y %s %n' $APP/crates/wg-tunnel/src/connectip/stack.rs
grep -c 'impl Drop for StackStream' $APP/crates/wg-tunnel/src/connectip/stack.rs
echo "=== validate nativepress harness on warp-2 (10 dials) ==="
timeout 300 $APP/target/release/nativepress /root/warpgate-rs/instances/warp-2/usque.json --tunnels 1 --concurrency 1 --total 10 --leak 2>/dev/null | grep -E '^(SAMPLE|SUMMARY|TUNNEL|REQ)' | head -20
echo "=== usque.json sanity warp-2 ==="
ls -l /root/warpgate-rs/instances/warp-2/usque.json
