#!/bin/bash
# Answer the question directly: do 100 instances give 100 IPs?
#
# WARP has two exit paths and they behave differently:
#   * IPv6 is assigned per account, so N instances should give N distinct v6.
#   * IPv4 comes from the edge node's shared NAT pool — only a handful.
# Measure both, over as many instances as respond.

set -u
OK_V6=0; OK_V4=0; TRIED=0
: > /root/iptest.txt

for p in $(seq 10011 3 10106); do
  TRIED=$((TRIED+1))
  V6=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" https://api64.ipify.org 2>/dev/null || true)
  V4=$(curl -s --max-time 20 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" https://api.ipify.org 2>/dev/null || true)
  echo "$p|$V6|$V4" >> /root/iptest.txt
  [ -n "$V6" ] && OK_V6=$((OK_V6+1))
  [ -n "$V4" ] && OK_V4=$((OK_V4+1))
done

echo "=== 采样 $TRIED 个实例 ==="
echo ""
echo "--- 原始数据（端口 | IPv6出口 | IPv4出口）---"
cat /root/iptest.txt
echo ""
echo "--- 统计 ---"
echo "  有 IPv6 出口: $OK_V6 / $TRIED"
echo "  有 IPv4 出口: $OK_V4 / $TRIED"
echo ""
echo "  不同 IPv6 数量: $(cut -d'|' -f2 /root/iptest.txt | grep -v '^$' | sort -u | wc -l)"
echo "  不同 IPv4 数量: $(cut -d'|' -f3 /root/iptest.txt | grep -v '^$' | sort -u | wc -l)"
echo ""
echo "--- 不同 IPv4 列表 ---"
cut -d'|' -f3 /root/iptest.txt | grep -v '^$' | sort | uniq -c | sort -rn
