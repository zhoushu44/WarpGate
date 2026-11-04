#!/bin/bash
set -u
cd /root/warpgate-rs/app
S=crates/wg-tunnel/src/connectip/stack.rs
N=crates/wg-tunnel/src/connectip/native.rs
echo "=== md5 ==="
md5sum "$S" "$N"
echo "=== lines ==="
wc -l "$S"
echo "=== my socket_count instrumentation still present? ==="
grep -n 'socket_count' "$S" "$N" || echo "none"
echo "=== wiring call sites ==="
grep -n 'wake_all\|queue_removal\|pending_removal' "$S" || echo "none"
echo "=== wake_by_ref ==="
grep -c 'wake_by_ref' "$S"
echo "=== pump_once ==="
sed -n '/async fn pump_once/,/^    }$/p' "$S"
echo "=== Drop ==="
sed -n '/impl Drop for StackStream/,/^}$/p' "$S"
echo "=== mtime ==="
ls -l --time-style=+%F_%T "$S" "$N"
