#!/bin/bash
set -u
APP=/root/warpgate-rs/app
S=$APP/crates/wg-tunnel/src/connectip/stack.rs
echo "=== server stack.rs ==="
wc -l "$S"; md5sum "$S"; stat -c '%y' "$S"
echo "=== fix markers ==="
echo "StackShared=$(grep -c 'pub struct StackShared' $S)"
echo "pending_removal=$(grep -c 'pending_removal' $S)"
echo "register_waker=$(grep -c 'fn register' $S)"
echo "wake_all=$(grep -c 'fn wake_all' $S)"
echo "old_wake_by_ref=$(grep -c 'wake_by_ref' $S)"
echo "Drop_try_lock=$(grep -c 'if let Ok(mut set) = self.sockets.try_lock()' $S)"
echo
echo "=== binary vs source mtime ==="
stat -c '%y %n' $APP/target/release/nativecheck $APP/target/release/nativepress "$S"
echo
echo "=== native.rs socket_count ==="
grep -c 'pub async fn socket_count' $APP/crates/wg-tunnel/src/connectip/native.rs
echo "=== nativepress uses socket_count ==="
grep -c 'socket_count' $APP/crates/wg-bin/src/bin/nativepress.rs
echo
echo "=== builds running? ==="
ps -eo pid,etime,cmd | grep -E 'cargo build|rustc --crate' | grep -v grep | head -3
echo "=== now ==="
date '+%F %T'
