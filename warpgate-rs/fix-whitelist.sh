#!/bin/bash
# Make the whitelist effective by editing the config file and restarting.
#
# v0.13.0's POST /api/whitelist only validates and returns; it never wrote the
# file, so adding 127.0.0.1 appeared to work and then vanished on the next read.
# v0.13.1 fixes that API, but until it is deployed the file is the only way to
# make the change stick.

set -u
CFG=/root/warpgate-rs/rust-config.json
BAK="$CFG.bak-$(date +%Y%m%d-%H%M%S)"

echo "=== 1. 备份 ==="
cp "$CFG" "$BAK" && echo "  $BAK"

echo ""
echo "=== 2. 用 python 精确改写 ip_whitelist ==="
python3 - "$CFG" <<'PY'
import json, sys
p = sys.argv[1]
d = json.load(open(p))
wl = d.get("ip_whitelist", [])
# 127.0.0.1 covers on-host tests and docker-proxy; 172.17.0.0/16 covers the
# bridge network that chat2api lives on.
for entry in ("127.0.0.1", "172.17.0.0/16", "172.18.0.0/16", "172.21.0.0/16"):
    if entry not in wl:
        wl.append(entry)
d["ip_whitelist"] = wl
json.dump(d, open(p, "w"), ensure_ascii=False, indent=2)
print("  ip_whitelist =", wl)
PY

if [ $? -ne 0 ]; then
  echo "  python3 不可用，改用 sed"
  # Fall back: replace the whole array on one line if it is a single line.
  grep -q '127.0.0.1' "$CFG" || sed -i 's|"ip_whitelist": \[|"ip_whitelist": ["127.0.0.1","172.17.0.0/16","172.18.0.0/16","172.21.0.0/16",|' "$CFG"
fi

echo ""
echo "=== 3. 校验 JSON ==="
python3 -c "import json;json.load(open('$CFG'));print('  JSON 合法')" 2>/dev/null || echo "  (无 python3，跳过校验)"
grep -o '"ip_whitelist":[^]]*]' "$CFG" | head -c 300
echo ""

echo ""
echo "=== 4. 重启让监听器重建 ==="
systemctl restart warpgate
for i in $(seq 1 20); do
  sleep 3
  [ "$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)" = "200" ] && { echo "  console up"; break; }
done

echo ""
echo "=== 5. 等实例就绪（最多 8 分钟）==="
for i in $(seq 1 48); do
  sleep 10
  H=$(curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2)
  if [ -n "$H" ] && [ "$H" -ge 80 ]; then echo "  ready healthy=$H"; break; fi
  [ $((i % 6)) -eq 0 ] && echo "  [$((i*10))s] healthy=${H:-0}"
done

echo ""
echo "=== 6. 白名单端口测试 ==="
OK=0
for p in 11011 11040 11070 11100; do
  C=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  echo "  $p -> $C"
  [ "$C" = "200" ] && OK=$((OK+1))
done
echo "  白名单 OK=$OK/4"

echo ""
echo "=== 7. 账密端口测试 ==="
OK2=0
for p in 10011 10040 10070 10100; do
  C=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null)
  echo "  $p -> $C"
  [ "$C" = "200" ] && OK2=$((OK2+1))
done
echo "  账密 OK=$OK2/4"
