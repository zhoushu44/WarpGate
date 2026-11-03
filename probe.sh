#!/bin/bash
echo "=== pgrep warpgate ==="
pgrep -a -x warpgate
echo "=== systemd ==="
systemctl status warpgate --no-pager 2>&1 | head -20
echo "=== listening ==="
ss -tlnp 2>/dev/null | head -30
echo "=== tools ==="
for t in timeout awk sed curl socat jq setsid nohup bg nice ionice tar; do
  printf '%s: %s\n' "$t" "$(command -v $t 2>/dev/null)"
done
echo "=== nativecheck help ==="
cd /root/warpgate-rs/app && ./target/release/nativecheck --help 2>&1 | head -40
echo "=== nativecheck strings (usage) ==="
strings /root/warpgate-rs/app/target/release/nativecheck 2>/dev/null | grep -iE 'usage|args|--' | head -30
echo "=== instances count ==="
ls /root/warpgate-rs/instances | wc -l
echo "=== free ==="
free -m
echo "=== nproc ==="
nproc
