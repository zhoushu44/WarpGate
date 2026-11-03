#!/bin/bash
# Rebuild every usque.json from a fresh template, keeping only the per-account
# fields.
#
# String-patching an already-corrupted JSON keeps failing: the escape sequences
# are ambiguous and a half-fixed file still will not parse. Rebuilding from a
# template usque wrote itself removes that whole class of problem — the template
# supplies correct syntax, and we only substitute the four values that differ
# per account.

set -u
BASE=/root/warpgate-rs/instances
T=/tmp/usque-tmpl/config.json

rm -rf /tmp/usque-tmpl && mkdir -p /tmp/usque-tmpl
cd /tmp/usque-tmpl
timeout 60 /usr/local/bin/usque register --accept-tos > /dev/null 2>&1
if [ ! -f "$T" ]; then
  echo "template registration failed"
  exit 1
fi
echo "template ready"

rebuilt=0
skipped=0
for d in "$BASE"/warp-*; do
  f="$d/usque.json"
  [ -f "$f" ] || continue

  # Pull the four per-account values with a tolerant regex; a corrupted file
  # still has them, and if it does not we skip rather than write nonsense.
  ID=$(grep -o '"id": *"[^"]*"' "$f" 2>/dev/null | head -1 | sed 's/.*"id": *"//; s/"$//')
  TOKEN=$(grep -o '"access_token": *"[^"]*"' "$f" 2>/dev/null | head -1 | sed 's/.*"access_token": *"//; s/"$//')
  PK=$(grep -o '"private_key": *"[^"]*"' "$f" 2>/dev/null | head -1 | sed 's/.*"private_key": *"//; s/"$//')
  V6=$(grep -o '"ipv6": *"[^"]*"' "$f" 2>/dev/null | head -1 | sed 's/.*"ipv6": *"//; s/"$//')

  if [ -z "$ID" ] || [ -z "$TOKEN" ] || [ -z "$PK" ]; then
    skipped=$((skipped+1))
    continue
  fi

  # Start from the template, overwrite the per-account fields.
  cp "$T" "$f"
  python_ok=0
  # No python on this box; use sed with the extracted values. They are all
  # base64/uuid, so they contain no regex metacharacters that matter here.
  sed -i "s|\"id\": *\"[^\"]*\"|\"id\": \"$ID\"|" "$f"
  sed -i "s|\"access_token\": *\"[^\"]*\"|\"access_token\": \"$TOKEN\"|" "$f"
  sed -i "s|\"private_key\": *\"[^\"]*\"|\"private_key\": \"$PK\"|" "$f"
  if [ -n "$V6" ]; then
    sed -i "s|\"ipv6\": *\"[^\"]*\"|\"ipv6\": \"$V6\"|" "$f"
  fi
  rebuilt=$((rebuilt+1))
done

echo "rebuilt=$rebuilt skipped=$skipped"
echo ""
echo "=== verify ==="
for w in warp-100 warp-300 warp-500; do
  f="$BASE/$w/usque.json"
  [ -f "$f" ] || continue
  OUT=$(timeout 10 /usr/local/bin/usque socks -c "$f" -b 127.0.0.1 -p 20896 --always-reconnect --http2 2>&1 | grep -E 'listening|Failed|Config' | head -1)
  echo "$w: ${OUT:-<none>}"
done
