#!/bin/bash
APP=/root/warpgate-rs/app
echo "=== nativecheck.d ==="
cat $APP/target/release/nativecheck.d
echo "=== nativepress.d ==="
cat $APP/target/release/nativepress.d
echo "=== strings: lock-order comment in binaries? ==="
for b in nativecheck nativepress warpgate; do
  printf '%s: ' "$b"
  if strings $APP/target/release/$b 2>/dev/null | grep -q 'Lock order MUST match'; then echo "HAS_FIX_COMMENT"; else echo "no-fix-comment"; fi
done
echo "=== server connect() lock block ==="
sed -n '245,275p' $APP/crates/wg-tunnel/src/connectip/stack.rs
echo "=== StackStream struct + methods ==="
grep -n 'struct StackStream\|impl StackStream\|fn drop\|set.remove' $APP/crates/wg-tunnel/src/connectip/stack.rs
