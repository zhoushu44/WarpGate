#!/bin/bash
# Retest with several accounts, since warp-0 may simply be a bad account.
# Measures: can a tunnel be established at all, and how long does the first
# request take. Runs one variant (--insecure) because it removed pin failures.

set -u
OUT=/root/warm2.txt
: > "$OUT"

echo "account|first_code|first_ms|conns|log_tail"
N=0
for idx in 1 2 3 4 5; do
  CFG="/root/warpgate-rs/instances/warp-$idx/usque.json"
  [ -s "$CFG" ] || continue
  N=$((N+1))
  port=$((28200 + idx))
  log="/root/w2_$idx.log"

  /usr/local/bin/usque socks -c "$CFG" -b 127.0.0.1 -p "$port" --http2 --insecure > "$log" 2>&1 &
  pid=$!
  sleep 8

  t0=$(date +%s%N)
  code=$(curl -s --max-time 30 --socks5-hostname "127.0.0.1:$port" \
        -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  t1=$(date +%s%N)
  ms=$(( (t1 - t0) / 1000000 ))

  conns=$(ss -tnp 2>/dev/null | grep -c "pid=$pid," || true)
  tail1=$(grep -iE "error|failed|Connected|listening" "$log" 2>/dev/null | tail -1 | cut -c1-80)

  echo "warp-$idx|$code|${ms}ms|conns=$conns|$tail1" | tee -a "$OUT"

  kill -9 "$pid" 2>/dev/null
  sleep 2
done

echo ""
echo "=== 汇总 ==="
cat "$OUT"
echo ""
echo "  成功: $(awk -F'|' '$2=="200"' "$OUT" | wc -l) / $N"
