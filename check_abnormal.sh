#!/bin/bash
set -u
echo "=== r8 full ==="
cat -v /root/np/hr20_8.log | tail -20
echo
echo "=== r12 full ==="
cat -v /root/np/hr20_12.log | tail -20
echo
echo "=== panic grep across all rounds ==="
grep -al 'panic\|thread .* panicked\|SIGSEGV\|memory allocation' /root/np/hr20_*.log 2>/dev/null
echo "=== dmesg oom/kill ==="
dmesg 2>/dev/null | tail -20 | grep -iE 'oom|kill|segfault' || echo "none"
echo "=== current md5 of deployed source (must stay aa96000...) ==="
md5sum /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
