#!/bin/bash
# Update every instance's pinned edge key to the one the endpoint actually
# presents today.
#
# Why: Cloudflare rotated the consumer-masque edge certificate. usque.json still
# pins the key captured at registration, so every dial fails with
# `x509: no valid chains built: remote endpoint has a different public key`.
# With --always-reconnect that failure retries once per second and leaks a
# socket each time; without the flag the tunnel simply never comes up and the
# first request stalls until it times out.
#
# The correct key is whatever the configured endpoint returns right now, so it
# is fetched fresh per host rather than hard-coded.

set -u
BASE=/root/warpgate-rs/instances
SNI=consumer-masque.cloudflareclient.com
BACKUP=/root/pin-backup-$(date +%Y%m%d-%H%M%S).tar.gz

echo "=== 1. 备份所有配置 ==="
tar -czf "$BACKUP" -C "$BASE" . 2>/dev/null
echo "  $BACKUP ($(du -h "$BACKUP" | cut -f1))"

echo ""
echo "=== 2. 抓取每个在用端点的真实公钥 ==="
declare -A PIN

for f in "$BASE"/warp-*/usque.json; do
  [ -s "$f" ] || continue
  ep=$(grep -oE '"endpoint_h2_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  [ -z "$ep" ] && ep=$(grep -oE '"endpoint_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  [ -z "$ep" ] && continue
  if [ -z "${PIN[$ep]:-}" ]; then
    pem=$(echo | timeout 15 openssl s_client -connect "$ep:443" -servername "$SNI" -alpn h2 2>/dev/null \
          | openssl x509 -pubkey -noout 2>/dev/null)
    if [ -n "$pem" ]; then
      PIN[$ep]="$pem"
      echo "  $ep -> $(printf '%s' "$pem" | sed -n 2p | cut -c1-40)..."
    else
      echo "  $ep -> 抓取失败"
    fi
  fi
done

if [ ${#PIN[@]} -eq 0 ]; then
  echo "没有抓到任何公钥，中止"
  exit 1
fi

echo ""
echo "=== 3. 更新实例配置 ==="
updated=0
skipped=0
for f in "$BASE"/warp-*/usque.json; do
  [ -s "$f" ] || continue
  ep=$(grep -oE '"endpoint_h2_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  [ -z "$ep" ] && ep=$(grep -oE '"endpoint_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  pem="${PIN[$ep]:-}"
  if [ -z "$pem" ]; then skipped=$((skipped+1)); continue; fi

  # Escape for JSON: newlines become \n, quotes are not present in PEM.
  json_pem=$(printf '%s' "$pem" | awk 'BEGIN{ORS="\\n"} {print}')
  # Write with a temp file built by awk to avoid sed escaping pitfalls.
  awk -v new="\"endpoint_pub_key\": \"$json_pem\"" '
    /"endpoint_pub_key":/ { sub(/"endpoint_pub_key":.*/, new); print; next }
    { print }
  ' "$f" > "$f.tmp" && mv "$f.tmp" "$f"
  updated=$((updated+1))
done

echo "  更新 $updated 个，跳过 $skipped 个"

echo ""
echo "=== 4. 抽查一个 ==="
grep -oE '"endpoint_pub_key": *"[^"]{0,60}' "$BASE/warp-0/usque.json" | head -1

echo ""
echo "=== 5. 手动验证（warp-0，带 always-reconnect）==="
/usr/local/bin/usque socks -c "$BASE/warp-0/usque.json" -b 127.0.0.1 -p 28999 \
  --http2 --always-reconnect > /root/verify_warm.log 2>&1 &
VPID=$!
sleep 8
T0=$(date +%s%N)
C=$(curl -s --max-time 30 --socks5-hostname 127.0.0.1:28999 -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T1=$(date +%s%N)
echo "  首次请求: $C ($(( (T1-T0)/1000000 ))ms)"
sleep 20
T2=$(date +%s%N)
C2=$(curl -s --max-time 30 --socks5-hostname 127.0.0.1:28999 -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T3=$(date +%s%N)
echo "  二次请求: $C2 ($(( (T3-T2)/1000000 ))ms)"
echo "  连接数: $(ss -tnp 2>/dev/null | grep -c "pid=$VPID,")"
echo "  pin 失败: $(grep -c 'different public key' /root/verify_warm.log 2>/dev/null || echo 0)"
kill -9 $VPID 2>/dev/null
