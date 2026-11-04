#!/bin/bash
# Step 0/1 for the re-test after the wake_all wiring fix.
set -u
cd /root/warpgate-rs/app
. /root/.cargo/env
S=crates/wg-tunnel/src/connectip/stack.rs
echo "=== source under test ==="
md5sum "$S"
wc -l "$S"
grep -c 'wake_by_ref' "$S"
grep -c 'register(' "$S"

echo "=== build ==="
cargo build --release --bin nativecheck --bin nativepress 2>&1 | tail -4
echo "=== artifacts ==="
ls -l --time-style=+%F_%T target/release/nativecheck target/release/nativepress
md5sum target/release/nativecheck target/release/nativepress

echo "=== wiring regression test ==="
cargo test -p wg-tunnel --lib wiring 2>&1 | tail -8
