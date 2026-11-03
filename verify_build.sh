#!/bin/bash
set -u
APP=/root/warpgate-rs/app
S=$APP/crates/wg-tunnel/src/connectip/stack.rs
N=$APP/crates/wg-tunnel/src/connectip/native.rs
echo "=== stack.rs now ==="
wc -l "$S"; md5sum "$S"
echo "=== fixes present ==="
grep -c 'impl Drop for StackStream' "$S"
grep -c 'Lock order MUST match' "$S"
grep -c 'pub async fn socket_count' "$S"
grep -c 'pub async fn socket_count' "$N"
echo "=== nativepress uses socket_count? ==="
grep -n 'socket_count\|SOCK' $APP/crates/wg-bin/src/bin/nativepress.rs | head -20
echo "=== connect() lock order (should be iface THEN sockets) ==="
sed -n '/let handle = {/,/set.add(sock)/p' "$S"
echo "=== build artifacts ==="
ls -l --time-style=+%F_%T $APP/target/release/nativecheck $APP/target/release/nativepress
echo "=== any build still running? ==="
ps -eo pid,etime,cmd | grep -E 'cargo|rustc' | grep -v grep | head -3
echo "=== time ==="
date '+%F %T'
