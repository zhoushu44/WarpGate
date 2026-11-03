#!/bin/bash
APP=/root/warpgate-rs/app
echo "=== stack.rs StackStream region ==="
sed -n '400,470p' $APP/crates/wg-tunnel/src/connectip/stack.rs
echo "=== rlib times ==="
ls -l --time-style=+%F_%T $APP/target/release/deps/libwg_tunnel-*.rlib 2>/dev/null
echo "=== quick in-process leak probe (60 reqs) ==="
timeout 300 $APP/target/release/nativepress /root/warpgate-rs/instances/warp-1/usque.json --tunnels 1 --concurrency 1 --total 60 --leak 2>/dev/null | grep -E '^(SAMPLE|SUMMARY|TUNNEL)'
echo "probe_rc=$?"
