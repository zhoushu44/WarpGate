#!/bin/bash
# Switch to the native CONNECT-IP data plane.
#
# Rollback is one command: cp /root/warpgate.service.quic.bak /etc/systemd/system/warpgate.service
#                           systemctl daemon-reload && systemctl restart warpgate
#
# Starting small on purpose: 500 instances all at once is 500 chances to fail
# with no way to tell whether one bad account or the whole stack is at fault.
# This brings up 20, exercises them, and only then scales.

set -u
CFG=/root/warpgate-rs/rust-config.json

echo "=== 1. lower resident_count to 20 for the trial ==="
sed -i 's/"resident_count"[[:space:]]*:[[:space:]]*[0-9]*/"resident_count": 20/' "$CFG"
grep -o '"resident_count": *[0-9]*' "$CFG"

echo "=== 2. switch the unit to native ==="
sed -i 's/^Environment=WARP_NATIVE=.*/Environment=WARP_NATIVE=h2/' /etc/systemd/system/warpgate.service
grep '^Environment=' /etc/systemd/system/warpgate.service

echo "=== 3. stop usque children and restart ==="
systemctl daemon-reload
pkill -9 usque 2>/dev/null || true
sleep 3
systemctl restart warpgate

echo "=== 4. wait for the console ==="
for i in $(seq 1 40); do
  sleep 3
  CODE=$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)
  [ "$CODE" = "200" ] && { echo "console up after $((i*3))s"; break; }
done

echo "=== 5. state ==="
systemctl is-active warpgate
echo "usque children: $(pgrep -c usque 2>/dev/null || echo 0)"
curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats
echo ""
echo "=== 6. log tail ==="
tail -12 /root/warpgate-rs/warpgate.log
