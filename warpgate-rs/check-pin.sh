#!/bin/bash
# Read the live edge certificate's public key that usque must pin.
#
# Cloudflare rotates the key the consumer-masque edge presents; usque.json pins
# the one captured at registration. When they diverge, every dial fails with
# `x509: no valid chains built: remote endpoint has a different public key`, and
# with --always-reconnect that retries once per second, leaking sockets.
#
# Uses openssl only: the server has no python3.

set -u
SNI=consumer-masque.cloudflareclient.com
OUT=/root/endpoint_pin.pem

echo "=== 抓取当前端点证书公钥 ==="
for ep in 162.159.192.7 162.159.198.2 162.159.198.3 162.159.193.1; do
  echo ""
  echo "--- $ep ---"
  echo | timeout 15 openssl s_client -connect "$ep:443" -servername "$SNI" -alpn h2 2>/dev/null \
    | openssl x509 -pubkey -noout 2>/dev/null > /tmp/pin_$ep.pem

  if [ -s /tmp/pin_$ep.pem ]; then
    echo "  公钥前 60 字符: $(sed -n 2p /tmp/pin_$ep.pem | cut -c1-60)"
    echo "  指纹: $(echo | timeout 10 openssl s_client -connect "$ep:443" -servername "$SNI" 2>/dev/null | openssl x509 -noout -fingerprint -sha256 2>/dev/null | cut -d= -f2)"
  else
    echo "  抓取失败"
  fi
done

echo ""
echo "=== 与 warp-0 配置里存的对比 ==="
echo "配置中: $(sed -n '2p' /root/warpgate-rs/instances/warp-0/usque.json 2>/dev/null | head -c 60)"
echo ""
echo "=== 统计：有多少实例的 pub_key 相同 ==="
for f in /root/warpgate-rs/instances/warp-*/usque.json; do
  grep -o 'MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE[^\\]*' "$f" 2>/dev/null | head -c 50
  echo ""
done | sort | uniq -c | sort -rn | head -5
