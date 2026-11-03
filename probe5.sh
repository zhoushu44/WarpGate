#!/bin/bash
APP=/root/warpgate-rs/app
echo "=== md5 server connectip ==="
md5sum $APP/crates/wg-tunnel/src/connectip/*.rs $APP/crates/wg-tunnel/src/masque.rs
echo "=== drop occurrences in stack.rs ==="
grep -n 'drop\|Drop' $APP/crates/wg-tunnel/src/connectip/stack.rs | head -20
echo "=== connect() lock acquisition order ==="
grep -n 'iface.lock().await\|sockets.lock().await' $APP/crates/wg-tunnel/src/connectip/stack.rs
echo "=== wg-bin Cargo.toml bins ==="
grep -n -B1 -A2 '\[\[bin\]\]' $APP/crates/wg-bin/Cargo.toml
echo "=== running builds ==="
ps -eo pid,etime,cmd | grep -E 'cargo build|rustc --crate' | grep -v grep | head -5
echo "=== local-bin ls ==="
ls -l --time-style=+%F_%T $APP/target/release/nativecheck $APP/target/release/nativepress 2>/dev/null
