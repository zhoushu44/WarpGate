#!/bin/bash
set -u
echo "=== deployed source must still be 095951dd1b26b6c4f313ecc5a495bcb1 ==="
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/native.rs
echo "=== no socket_count leaked into deployed tree ==="
grep -rn 'socket_count' /root/warpgate-rs/app/crates/ 2>/dev/null || echo "clean (none)"
echo "=== deployed binaries ==="
md5sum /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/app/target/release/nativepress
ls -l --time-style=+%F_%T /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/app/target/release/nativepress
echo "=== instrumented build isolated at /root/np2/instr (will be removed) ==="
du -sh /root/np2/instr_target 2>/dev/null
echo "=== no stuck processes left ==="
ps -eo pid,etime,pcpu,args --no-headers | grep -E 'nativepress|nativecheck' | grep -v grep || echo "none"
