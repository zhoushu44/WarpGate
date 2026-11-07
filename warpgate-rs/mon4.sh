#!/bin/bash
# Sampler: pool health every 60s. Appends to a TSV so the trend is visible.
set -u
OUT=/root/mon4.tsv
[ -f "$OUT" ] || printf 'time\tver\tusque\tcf\thealthy\tips\tunhealthy\trecycled\tmem\tdisk\tproxy\n' > "$OUT"

for i in $(seq 1 12); do
  V=$(/root/warpgate-rs/app/target/release/warpgate --version 2>/dev/null | awk '{print $2}')
  U=$(pgrep -c usque 2>/dev/null || true)
  CF=$(ss -tn 2>/dev/null | grep -c 162.159 || true)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null || true)
  H=$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2 || true)
  IP=$(printf '%s' "$S" | grep -oE '"unique_ips":[0-9]+' | cut -d: -f2 || true)
  UN=$(printf '%s' "$S" | grep -oE '"unhealthy":[0-9]+' | cut -d: -f2 || true)
  RC=$(journalctl -u warpgate --no-pager 2>/dev/null | grep -c '引擎连接数异常' || true)
  MEM=$(free -m | awk '/^Mem:/{print $3}')
  DSK=$(df -h / | awk 'NR==2{print $4}')

  OK=0
  for p in 10011 10040 10070 10100; do
    C=$(curl -s --max-time 10 --socks5-hostname 127.0.0.1:$p -U "warpuser:test-pass-123" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
    [ "$C" = "200" ] && OK=$((OK+1))
  done

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s/4\n' \
    "$(date '+%H:%M:%S')" "${V:-?}" "${U:-0}" "${CF:-0}" "${H:-0}" "${IP:-0}" \
    "${UN:-0}" "${RC:-0}" "${MEM:-0}" "$DSK" "$OK" >> "$OUT"
  sleep 60
done

cat "$OUT"
