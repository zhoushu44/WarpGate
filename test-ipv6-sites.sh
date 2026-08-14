#!/bin/bash
# ============================================================
# 测试 WARP 代理池 IPv6 出口对重要网站的访问情况
# 通过 sing-box SOCKS5 端口走 IPv6 出口（prefer_ipv6）
# 用法: bash test-ipv6-sites.sh [端口]  (默认自动探测可用端口)
# ============================================================

PORT="${1:-}"
SITES=(
  # ---- 国际主流 ----
  "https://www.google.com"
  "https://www.youtube.com"
  "https://github.com"
  "https://www.bing.com"
  "https://x.com"
  "https://www.facebook.com"
  "https://www.instagram.com"
  "https://www.tiktok.com"
  "https://www.reddit.com"
  "https://www.linkedin.com"
  "https://www.netflix.com"
  "https://www.amazon.com"
  "https://www.ebay.com"
  "https://www.wikipedia.org"
  "https://www.apple.com"
  "https://www.microsoft.com"
  "https://www.cloudflare.com"
  "https://www.notion.so"
  "https://vercel.com"
  "https://stackoverflow.com"
  "https://news.ycombinator.com"
  "https://discord.com"
  "https://www.telegram.org"
  "https://open.spotify.com"
  "https://www.paypal.com"
  "https://www.twitch.tv"
  "https://mail.google.com"
  "https://drive.google.com"
  "https://translate.google.com"
  "https://www.whatsapp.com"
  # ---- OpenAI ----
  "https://chatgpt.com"
  "https://openai.com"
  "https://auth.openai.com"
  # ---- 国内主流 ----
  "https://www.baidu.com"
  "https://www.taobao.com"
  "https://www.tmall.com"
  "https://www.jd.com"
  "https://www.douyin.com"
  "https://weibo.com"
  "https://www.zhihu.com"
  "https://www.bilibili.com"
  "https://www.qq.com"
  "https://www.163.com"
  "https://www.sohu.com"
  "https://www.sina.com.cn"
  "https://www.douban.com"
  "https://www.xiaohongshu.com"
  "https://www.meituan.com"
  "https://www.ele.me"
  "https://www.dianping.com"
  "https://www.pinduoduo.com"
  "https://www.aliyun.com"
  "https://www.tencent.com"
  "https://www.iqiyi.com"
  "https://v.qq.com"
  "https://www.youku.com"
)

# ---- 1. 探测可用端口与 IPv6 出口 ----
if [ -z "$PORT" ]; then
  for p in $(seq 10010 10029); do
    ip=$(curl -6 -s --max-time 8 --socks5-hostname 127.0.0.1:$p https://api64.ipify.org 2>/dev/null | tr -d '[:space:]')
    if [ -n "$ip" ] && echo "$ip" | grep -q ':'; then
      PORT=$p
      break
    fi
  done
fi

if [ -z "$PORT" ]; then
  echo "❌ 未找到可用实例端口 (10010-10029)"
  exit 1
fi

EXIT_IP=$(curl -6 -s --max-time 10 --socks5-hostname 127.0.0.1:$PORT https://api64.ipify.org 2>/dev/null | tr -d '[:space:]')
V4_IP=$(curl -4 -s --max-time 10 --socks5-hostname 127.0.0.1:$PORT https://api.ipify.org 2>/dev/null | tr -d '[:space:]')

echo "======================================================"
echo " WARP IPv6 出口可达性测试"
echo " 端口: $PORT | IPv6出口: ${EXIT_IP:-检测失败} | IPv4出口: ${V4_IP:-检测失败}"
echo " 测试站点数: ${#SITES[@]}  (curl -6 强制IPv6, 失败后回退普通测试)"
echo "======================================================"
echo ""

declare -A RESULT
declare -A TIME
PASS6=0; PASS4=0; FAIL=0

for url in "${SITES[@]}"; do
  host=$(echo "$url" | sed -E 's#https?://([^/]+).*#\1#')
  # 主测试: 强制 IPv6
  out6=$(curl -6 -s -o /dev/null -w "%{http_code}|%{time_total}" --max-time 15 --socks5-hostname 127.0.0.1:$PORT "$url" 2>/dev/null)
  r6="${out6%%|*}"; t6="${out6##*|}"

  if [ "$r6" != "000" ] && [ -n "$r6" ]; then
    RESULT[$host]="OK-v6 ($r6)"
    TIME[$host]="$t6"
    PASS6=$((PASS6+1))
    printf "%-38s ✅ IPv6  HTTP %-4s  %ss\n" "$host" "$r6" "$t6"
  else
    # 回退: 普通测试 (可能只支持IPv4)
    out=$(curl -s -o /dev/null -w "%{http_code}|%{time_total}" --max-time 15 --socks5-hostname 127.0.0.1:$PORT "$url" 2>/dev/null)
    r="${out%%|*}"; t="${out##*|}"
    if [ "$r" != "000" ] && [ -n "$r" ]; then
      RESULT[$host]="OK-v4 ($r)"
      TIME[$host]="$t"
      PASS4=$((PASS4+1))
      printf "%-38s ⚠️  IPv4only HTTP %-4s  %ss\n" "$host" "$r" "$t"
    else
      RESULT[$host]="FAIL"
      FAIL=$((FAIL+1))
      printf "%-38s ❌ 不可达   (v6=${r6:-000}, 普通=${r:-000})\n" "$host"
    fi
  fi
done

echo ""
echo "======================================================"
echo " 汇总: IPv6可达=${PASS6} | 仅IPv4可达=${PASS4} | 不可达=${FAIL} / 总数=${#SITES[@]}"
echo "======================================================"
echo ""
echo "--- 不可达站点明细 ---"
for url in "${SITES[@]}"; do
  host=$(echo "$url" | sed -E 's#https?://([^/]+).*#\1#')
  if [ "${RESULT[$host]}" = "FAIL" ]; then
    echo "  ❌ $host"
  fi
done
