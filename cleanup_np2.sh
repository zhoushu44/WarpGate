#!/bin/bash
# Cleanup of my temporary artefacts (instrumented copy + its 480 MB target dir).
# The deployed tree and binaries are left untouched.
set -u
echo "=== before ==="
df -h / | tail -1
echo "=== removing /root/np2/instr and /root/np2/instr_target ==="
rm -rf /root/np2/instr_target
rm -rf /root/np2/instr
echo "=== after ==="
df -h / | tail -1
echo
echo "=== deployed tree integrity (must be unchanged) ==="
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/native.rs
grep -rn 'socket_count' /root/warpgate-rs/app/crates/ 2>/dev/null || echo "  socket_count: clean"
md5sum /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/app/target/release/nativepress
echo
echo "=== no stray processes ==="
ps -eo pid,etime,args --no-headers | grep -E 'nativepress|nativecheck|hangrate|leak[0-9]' | grep -v grep || echo "  none"
