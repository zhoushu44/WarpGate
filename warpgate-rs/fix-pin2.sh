#!/bin/bash
# Rewrite endpoint_pub_key keeping PEM newlines escaped as literal \n.
#
# The previous attempt inserted real newlines inside a JSON string, which makes
# the file unparseable (`invalid character '\n' in string literal`). usque.json
# stores the PEM as one line with two-character \n escapes, so the replacement
# must be built the same way.

set -u
BASE=/root/warpgate-rs/instances
SNI=consumer-masque.cloudflareclient.com
BACKUP=/root/pin-backup2-$(date +%Y%m%d-%H%M%S).tar.gz

echo "=== 1. 备份 ==="
tar -czf "$BACKUP" -C "$BASE" . 2>/dev/null
echo "  $BACKUP"

echo ""
echo "=== 2. 抓取端点公钥并转成单行转义格式 ==="
declare -A ESC

for f in "$BASE"/warp-*/usque.json; do
  [ -s "$f" ] || continue
  ep=$(grep -oE '"endpoint_h2_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  [ -z "$ep" ] && continue
  if [ -z "${ESC[$ep]:-}" ]; then
    pem=$(echo | timeout 15 openssl s_client -connect "$ep:443" -servername "$SNI" -alpn h2 2>/dev/null \
          | openssl x509 -pubkey -noout 2>/dev/null | tr -d '\r')
    if [ -n "$pem" ]; then
      # Turn real newlines into the two characters backslash + n, which is what
      # a JSON string needs.
      one=$(printf '%s' "$pem" | awk 'BEGIN{ORS=""} { if (NR>1) printf "\\n"; printf "%s", $0 } END{printf "\\n"}')
      ESC[$ep]="$one"
      echo "  $ep -> ${one:30:40}..."
    else
      echo "  $ep -> 抓取失败"
    fi
  fi
done

[ ${#ESC[@]} -eq 0 ] && { echo "没抓到公钥"; exit 1; }

echo ""
echo "=== 3. 逐文件替换（用 awk，把新值当作字面量）==="
updated=0
for f in "$BASE"/warp-*/usque.json; do
  [ -s "$f" ] || continue
  ep=$(grep -oE '"endpoint_h2_v4": *"[^"]*"' "$f" | head -1 | cut -d'"' -f4)
  new="${ESC[$ep]:-}"
  [ -z "$new" ] && continue

  # -v passes the value through awk's escape processing, which would eat the
  # backslashes; read it from a file instead and use ENVIRON-free string ops.
  printf '%s' "$new" > /tmp/onekey.txt
  awk '
    BEGIN { getline key < "/tmp/onekey.txt" }
    /"endpoint_pub_key":/ {
      # Keep everything up to the key name, then emit the new escaped PEM.
      sub(/"endpoint_pub_key": *".*",?[[:space:]]*$/, "\"endpoint_pub_key\": \"" key "\",")
      print
      next
    }
    { print }
  ' "$f" > "$f.tmp"

  # Trailing comma: if the replaced line is the last field it must lose the comma.
  if tail -3 "$f" | grep -q 'endpoint_pub_key'; then
    :
  fi
  mv "$f.tmp" "$f"
  updated=$((updated+1))
done
echo "  更新 $updated 个"

echo ""
echo "=== 4. 立即验证 JSON 可解析（usque 能否加载）==="
timeout 8 /usr/local/bin/usque socks -c "$BASE/warp-0/usque.json" -b 127.0.0.1 -p 28996 --http2 2>&1 | head -4

echo ""
echo "=== 5. 抽查公钥头部 ==="
grep -o '"endpoint_pub_key": "[^"]\{0,70\}' "$BASE/warp-0/usque.json" | head -1

echo ""
echo "=== 6. 真实连通性（warp-0 带 always-reconnect）==="
/usr/local/bin/usque socks -c "$BASE/warp-0/usque.json" -b 127.0.0.1 -p 28995 \
  --http2 --always-reconnect > /root/v2.log 2>&1 &
VP=$!
sleep 8
T0=$(date +%s%N)
C=$(curl -s --max-time 30 --socks5-hostname 127.0.0.1:28995 -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T1=$(date +%s%N)
echo "  首次: $C ($(( (T1-T0)/1000000 ))ms)"
sleep 15
T2=$(date +%s%N)
C2=$(curl -s --max-time 30 --socks5-hostname 127.0.0.1:28995 -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
T3=$(date +%s%N)
echo "  二次: $C2 ($(( (T3-T2)/1000000 ))ms)"
echo "  连接数: $(ss -tnp 2>/dev/null | grep -c "pid=$VP,")"
echo "  pin失败: $(grep -c 'different public key' /root/v2.log 2>/dev/null || echo 0)"
echo "  IP: $(curl -s --max-time 20 --socks5-hostname 127.0.0.1:28995 https://api64.ipify.org 2>/dev/null)"
kill -9 $VP 2>/dev/null
