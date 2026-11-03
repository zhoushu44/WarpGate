#!/bin/bash
echo "=== wc -l stack.rs ==="
wc -l /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
echo "=== Drop impl present? ==="
grep -n "impl Drop for StackStream" /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
echo "=== lock-order comment present? ==="
grep -n "Lock order MUST match" /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
echo "=== pump_once lock order ==="
sed -n '/async fn pump_once/,/^    }/p' /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
echo "=== connect lock order ==="
sed -n '/let handle = {/,/set.add(sock)/p' /root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
echo "=== binary build times ==="
stat -c '%y %n' /root/warpgate-rs/app/target/release/warpgate /root/warpgate-rs/app/target/release/nativecheck
echo "=== newest source file ==="
find /root/warpgate-rs/app/crates -name '*.rs' -printf '%T@ %TY-%Tm-%Td %TH:%TM %p\n' | sort -n | tail -3
