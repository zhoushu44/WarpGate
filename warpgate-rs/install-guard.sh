#!/bin/bash
# Install an hourly safety net for the usque connection leak.
#
# usque --always-reconnect opens a new TCP connection on every redial and never
# closes the old one. Cloudflare drops the tunnel about every 5 minutes, so each
# engine gains ~10 sockets per hour; 100 engines reached 4,076 before this was
# noticed, and the pool's usable IPs fell from 91 to 31 as the per-IP allowance
# ran out. v0.12.2 adds a watchdog, but this cron keeps the pool safe until that
# build is deployed (and acts as a backstop afterwards).

set -u

cat > /root/wg-guard.sh <<'GUARD'
#!/bin/bash
# Hourly safety net while usque leaks one socket per redial.
# Restarting warpgate returns every engine's connection count to zero.
CF=$(ss -tn 2>/dev/null | grep -c 162.159)
echo "$(date '+%F %T') restart warpgate (cf_conn=$CF)" >> /root/wg-guard.log
systemctl restart warpgate
GUARD

chmod +x /root/wg-guard.sh

# Preserve any existing crontab entries, replace just our line.
( crontab -l 2>/dev/null | grep -v "wg-guard" ; echo "0 * * * * /root/wg-guard.sh" ) | crontab -

echo "=== crontab ==="
crontab -l 2>/dev/null

echo ""
echo "=== 语法检查 ==="
bash -n /root/wg-guard.sh && echo "OK"

echo ""
echo "=== 当前状态 ==="
echo "版本: $(/root/warpgate-rs/app/target/release/warpgate --version 2>/dev/null)"
echo "CF连接: $(ss -tn 2>/dev/null | grep -c 162.159)"
echo "usque: $(pgrep -c usque 2>/dev/null || echo 0)"
curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null \
  | grep -oE '"(healthy_count|unique_ips|instance_count)":[0-9]*' | tr '\n' ' '
echo ""

echo ""
echo "=== 编译状态 ==="
if [ -f /root/wg_new/target/release/warpgate ]; then
  echo "DONE $(/root/wg_new/target/release/warpgate --version)"
else
  echo "WIP cargo=$(pgrep -c cargo 2>/dev/null || echo 0)"
fi
