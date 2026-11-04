#!/bin/bash
set -u
echo "=== smoltcp is_active / can_send definitions ==="
SM=$(find /root/.cargo/registry/src -maxdepth 2 -name 'smoltcp-*' -type d 2>/dev/null | head -1)
echo "crate: $SM"
grep -n -A8 'pub fn is_active' "$SM/src/socket/tcp.rs" | head -20
echo "--- can_send ---"
grep -n -A10 'pub fn can_send' "$SM/src/socket/tcp.rs" | head -16
echo "--- can_recv ---"
grep -n -A10 'pub fn can_recv' "$SM/src/socket/tcp.rs" | head -16
echo
echo "=== 1000-dial RSS curve (background-safe, request timeout caps it) ==="
