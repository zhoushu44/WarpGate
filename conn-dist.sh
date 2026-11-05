#!/bin/bash
# Per-engine connection counts, to see whether the excess is a few leaking
# instances or every instance holding many.
set -u

ss -tnp 2>/dev/null | grep 162.159 > /tmp/cf_conns.txt
TOTAL=$(wc -l < /tmp/cf_conns.txt)
echo "cf_conn_total=$TOTAL"

echo ""
echo "=== 每引擎连接数分布 ==="
for p in $(pgrep usque); do
  grep -c "pid=$p," /tmp/cf_conns.txt 2>/dev/null || echo 0
done | sort -n | uniq -c | awk '{printf "  %4d 个引擎有 %s 条连接\n", $1, $2}'

echo ""
echo "=== 连接最多的 5 个引擎 ==="
for p in $(pgrep usque); do
  n=$(grep -c "pid=$p," /tmp/cf_conns.txt 2>/dev/null || echo 0)
  echo "$n $p"
done | sort -rn | head -5 | while read n p; do
  idx=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -oE 'warp-[0-9]+' | head -1)
  port=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -A1 '^-p$' | tail -1)
  echo "  pid=$p conns=$n inst=$idx port=$port age=$(ps -o etime= -p $p 2>/dev/null | tr -d ' ')"
done

echo ""
echo "=== 连接状态分布 ==="
awk '{print $1}' /tmp/cf_conns.txt | sort | uniq -c

echo ""
echo "=== 本机源端口是否耗尽 ==="
echo "  ephemeral range: $(cat /proc/sys/net/ipv4/ip_local_port_range)"
echo "  TIME_WAIT: $(ss -tan 2>/dev/null | grep -c TIME-WAIT)"
echo "  ESTAB:     $(ss -tan 2>/dev/null | grep -c ESTAB)"

echo ""
echo "=== usque 日志里有无反复重连 ==="
for p in $(pgrep usque); do
  n=$(grep -c "pid=$p," /tmp/cf_conns.txt 2>/dev/null || echo 0)
  if [ "$n" -gt 15 ]; then
    idx=$(tr '\0' '\n' < /proc/$p/cmdline 2>/dev/null | grep -oE 'warp-[0-9]+' | head -1)
    echo "  取样 $idx (conns=$n):"
    find /root/warpgate-rs/instances/$idx -name "*.log" 2>/dev/null | head -3
    tail -8 /root/warpgate-rs/instances/$idx/engine.log 2>/dev/null || echo "    (无 engine.log)"
    break
  fi
done
