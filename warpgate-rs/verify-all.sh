#!/bin/bash
# Restart and verify every field the console reads.
set -u
cd /root/warpgate-rs/app
./target/release/patchweb public/index.html 2>/dev/null

pkill -9 -f 'target/release/warpgate' 2>/dev/null || true
pkill -9 usque 2>/dev/null || true
sleep 3
: > /root/warpgate-rs/warpgate.log
export WARP_NATIVE=quic
ulimit -n 65535

nohup ./target/release/warpgate \
  --config /root/warpgate-rs/rust-config.json \
  --instances /root/warpgate-rs/instances \
  >> /root/warpgate-rs/warpgate.log 2>&1 &
disown $!

# Wait for the console to answer (bind happens before pool fill now).
for i in $(seq 1 40); do
  sleep 2
  [ "$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)" = "200" ] && break
done

K=zs1236547
B=http://127.0.0.1:4433
H="X-API-Key: $K"

echo "=== webui: $(curl -s --max-time 5 -o /dev/null -w '%{http_code}' $B/) ==="
echo ""
echo "=== /api/stats (console fields) ==="
curl -s --max-time 10 -H "$H" $B/api/stats
echo ""
echo ""
echo "=== /api/whitelist (needs ips) ==="
curl -s --max-time 10 -H "$H" $B/api/whitelist
echo ""
echo ""
echo "=== /api/usage (needs days + system) ==="
curl -s --max-time 10 -H "$H" "$B/api/usage?days=2" | head -c 400
echo ""
echo ""
echo "=== /api/containers (first row) ==="
curl -s --max-time 15 -H "$H" $B/api/containers | head -c 500
echo ""
echo ""
echo "=== /api/pool/dedicated (needs items) ==="
curl -s --max-time 10 -H "$H" $B/api/pool/dedicated
echo ""
