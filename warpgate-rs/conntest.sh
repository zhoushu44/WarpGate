#!/bin/bash
# Real connectivity test: not just "does the port answer", but can a client
# actually reach real destinations through the pool, repeatedly, including
# through the whitelist ports and the Docker-internal address that chat2api uses.

set -u
echo "### 1. 基础连通性（账密端口）"
OK=0; FAIL=0; SLOW=0
for p in 10011 10025 10040 10055 10070 10085 10095; do
  T0=$(date +%s%N)
  C=$(curl -s --max-time 25 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  T1=$(date +%s%N); MS=$(( (T1-T0)/1000000 ))
  if [ "$C" = "200" ]; then OK=$((OK+1)); [ "$MS" -gt 5000 ] && SLOW=$((SLOW+1)); else FAIL=$((FAIL+1)); fi
done
echo "  首次: OK=$OK FAIL=$FAIL 慢请求(>5s)=$SLOW"

OK=0
for p in 10011 10025 10040 10055 10070 10085 10095; do
  C=$(curl -s --max-time 15 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  [ "$C" = "200" ] && OK=$((OK+1))
done
echo "  二次: OK=$OK/7"

echo ""
echo "### 2. 真实目标站（HTTPS + 大站）"
for url in https://www.google.com https://api.openai.com/v1/models https://api.anthropic.com https://github.com; do
  P=10020
  C=$(curl -s --max-time 25 --socks5-hostname 127.0.0.1:$P -U "warpuser:test-pass-123" \
      -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || true)
  echo "  $url -> $C"
done

echo ""
echo "### 3. 白名单端口（免密，chat2api 用的这条）"
OK=0
for p in 11011 11040 11070 11100; do
  C=$(curl -s --max-time 25 --socks5-hostname 127.0.0.1:$p -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  [ "$C" = "200" ] && OK=$((OK+1))
done
echo "  白名单: OK=$OK/4"

echo ""
echo "### 4. Docker 内网地址（chat2api 视角：172.17.0.1）"
curl -s --max-time 10 -H "X-API-Key: zs1236547" \
  "http://172.17.0.1:4433/api/proxies?num=3&type=txt&format=n&sid=conntest-$(date +%s)&time=20" \
  -o /tmp/proxies.txt 2>/dev/null
echo "  池返回的代理:"
cat /tmp/proxies.txt 2>/dev/null | sed 's/^/    /'
FIRST=$(head -1 /tmp/proxies.txt 2>/dev/null)
if [ -n "$FIRST" ]; then
  C=$(curl -s --max-time 25 --socks5-hostname "$FIRST" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  echo "  用第一个代理实测 -> $C"
fi

echo ""
echo "### 5. 并发 5 个请求"
for i in 1 2 3 4 5; do
  ( C=$(curl -s --max-time 25 --socks5-hostname 127.0.0.1:100$((20 + i * 10)) -U "warpuser:test-pass-123" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true); echo "  #$i -> $C" ) &
done
wait

echo ""
echo "### 6. 池整体状态"
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null \
  | grep -oE '"(healthy_count|unique_ips|unhealthy|instance_count|in_use)":[0-9]+' | tr '\n' ' '
echo ""
echo "  CF连接: $(ss -tn 2>/dev/null | grep -c 162.159)  usque: $(pgrep -c usque 2>/dev/null || echo 0)"
