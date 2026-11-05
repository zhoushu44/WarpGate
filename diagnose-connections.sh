#!/bin/bash
# Work out why individual usque processes accumulate TCP connections.
#
# Symptom: 507 engines hold 12,715 connections to Cloudflare (expected ~1-2 each),
# and SOCKS5 CONNECT never gets an ack on most ports.
set -u

echo "=== 1. 连接数分布 ==="
for p in $(pgrep usque); do
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p")
  echo "$n"
done | sort -n | uniq -c | tail -15

echo ""
echo "=== 2. 连接数 >5 的实例有多少 ==="
BAD=0; TOTAL=0
for p in $(pgrep usque); do
  TOTAL=$((TOTAL+1))
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p")
  [ "$n" -gt 5 ] && BAD=$((BAD+1))
done
echo "  总数=$TOTAL  异常(>5连接)=$BAD"

echo ""
echo "=== 3. 异常实例的命令行 ==="
for p in $(pgrep usque); do
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p")
  if [ "$n" -gt 20 ]; then
    echo "  pid=$p conns=$n"
    tr '\0' ' ' < /proc/$p/cmdline 2>/dev/null; echo ""
    echo "  存活: $(ps -o etime= -p $p 2>/dev/null | tr -d ' ')"
    break
  fi
done

echo ""
echo "=== 4. usque 日志（异常实例）==="
for p in $(pgrep usque); do
  n=$(ss -tnp 2>/dev/null | grep -c "pid=$p")
  if [ "$n" -gt 20 ]; then
    IDX=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -oE 'warp-[0-9]+' | head -1)
    echo "  实例 $IDX:"
    ls /root/warpgate-rs/instances/$IDX/ 2>/dev/null
    journalctl -u warpgate --no-pager -n 2000 2>/dev/null | grep "$IDX" | tail -5
    break
  fi
done

echo ""
echo "=== 5. 端口监听状态 ==="
echo "  账密端口段: $(ss -ltn 2>/dev/null | grep -cE ':10[0-5][0-9]{2}')"
echo "  白名单段:   $(ss -ltn 2>/dev/null | grep -cE ':11[0-5][0-9]{2}')"

echo ""
echo "=== 6. 连接状态分布 ==="
ss -tn 2>/dev/null | grep 162.159 | awk '{print $1}' | sort | uniq -c
