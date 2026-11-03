#!/bin/bash
set -u
cd /root/warpgate-rs/app
source $HOME/.cargo/env
S=crates/wg-tunnel/src/connectip/stack.rs
echo "=== prebuild source ==="
wc -l "$S"; md5sum "$S"
echo "=== build ==="
date '+%T'
cargo build --release --bin nativecheck --bin nativepress 2>&1 | tail -5
rc=$?
echo "build_rc=$rc"
date '+%T'
echo "=== artifacts ==="
ls -l --time-style=+%F_%T target/release/nativecheck target/release/nativepress
md5sum target/release/nativecheck target/release/nativepress
