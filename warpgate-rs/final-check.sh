#!/bin/bash
# Definitive availability test on the live pool.
#
# Everything so far measured engines I started by hand, which are not what
# clients use. This measures the ports clients actually hit: the credential
# ports that warpgate owns.

set -u
echo "=== 池状态 ==="
curl -s --max-time 10 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null \
  | grep -oE '"(healthy_count|unique_ips|instance_count|in_use)":[0-9]+' | tr '\n' ' '
echo ""

echo ""
echo "=== 账密端口 20 个（每个 2 次尝试）==="
OK=0; FAIL=0; TOTAL=0
for p in $(seq 10011 5 10106); do
  TOTAL=$((TOTAL+1))
  ok=0
  for attempt in 1 2; do
    C=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p \
        -U "warpuser:test-pass-123" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
    [ "$C" = "200" ] && { ok=1; break; }
  done
  if [ "$ok" = "1" ]; then OK=$((OK+1)); else FAIL=$((FAIL+1)); fi
done
echo "  OK=$OK FAIL=$FAIL  (共 $TOTAL 个，成功率 $((OK*100/TOTAL))%)"

echo ""
echo "=== 白名单端口 10 个 ==="
OK2=0; FAIL2=0
for p in $(seq 11011 10 11106); do
  ok=0
  for attempt in 1 2; do
    C=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
    [ "$C" = "200" ] && { ok=1; break; }
  done
  if [ "$ok" = "1" ]; then OK2=$((OK2+1)); else FAIL2=$((FAIL2+1)); fi
done
echo "  OK=$OK2 FAIL=$FAIL2"

echo ""
echo "=== /api/proxies 交付的代理实测 ==="
curl -s --max-time 15 -H "X-API-Key: zs1236547" \
  "http://127.0.0.1:4433/api/proxies?num=5&type=txt&format=n&sid=final-$(date +%s)&time=20" \
  -o /tmp/final_proxies.txt 2>/dev/null
cat /tmp/final_proxies.txt | sed 's/^/    /'
while read -r pr; do
  [ -z "$pr" ] && continue
  C=$(curl -s --max-time 20 --socks5-hostname "$pr" -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  echo "    $pr -> $C"
done < /tmp/final_proxies.txt

echo ""
echo "=== 出口 IP 抽样 ==="
for pr in $(head -3 /tmp/final_proxies.txt); do
  echo -n "    $pr -> "
  curl -s --max-time 20 --socks5-hostname "$pr" https://api64.ipify.org 2>/dev/null
  echo ""
done
