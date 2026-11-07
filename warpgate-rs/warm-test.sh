#!/bin/bash
# Find the combination that keeps tunnels warm (fast first request) without the
# connection leak.
#
# Leak mechanism: Cloudflare rotates its edge certificate, usque's
# EndpointPubKey pinning then fails (`x509: no valid chains built`), and
# --always-reconnect retries that failure every 1s, opening a new TCP each time
# and never closing the old one.
#
# So the fix is not "stop reconnecting" but "stop failing": skip the pin check,
# keep --always-reconnect for warm tunnels, and keep a slow retry as backstop.
#
# Variants measured:
#   A  --always-reconnect                      (original: warm but leaks 1/s)
#   B  (none)                                  (current: no leak, cold start)
#   C  --always-reconnect --insecure           (warm + no pin failures)
#   D  --always-reconnect --reconnect-delay 30s (warm + slower leak)

set -u
CFG=/root/warpgate-rs/instances/warp-0/usque.json
RESULT=/root/warmtest.txt
: > "$RESULT"

[ -s "$CFG" ] || { echo "缺少 $CFG"; exit 1; }

run_variant() {
  local name="$1"; shift
  local port="$1"; shift
  local log="/root/wt_${name}.log"

  /usr/local/bin/usque socks -c "$CFG" -b 127.0.0.1 -p "$port" --http2 "$@" > "$log" 2>&1 &
  local pid=$!
  sleep 10

  # First-request latency: this is what the user actually feels.
  local t0 t1 ms code
  t0=$(date +%s%N)
  code=$(curl -s --max-time 40 --socks5-hostname "127.0.0.1:$port" \
         -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  t1=$(date +%s%N)
  ms=$(( (t1 - t0) / 1000000 ))

  # Idle for 30s, then measure again: a warm tunnel answers fast.
  sleep 30
  local t2 t3 ms2 code2
  t2=$(date +%s%N)
  code2=$(curl -s --max-time 40 --socks5-hostname "127.0.0.1:$port" \
          -o /dev/null -w '%{http_code}' http://httpbin.org/ip 2>/dev/null || true)
  t3=$(date +%s%N)
  ms2=$(( (t3 - t2) / 1000000 ))

  local conns
  conns=$(ss -tnp 2>/dev/null | grep -c "pid=$pid," || true)

  local pinfail
  pinfail=$(grep -c "x509\|different public key" "$log" 2>/dev/null || true)

  echo "$name|$code|${ms}ms|$code2|${ms2}ms|conns=$conns|pinfail=$pinfail" | tee -a "$RESULT"
  kill -9 "$pid" 2>/dev/null
  sleep 2
}

echo "name|first_code|first_ms|second_code|second_ms|conns|pinfail"
run_variant A 28101 --always-reconnect
run_variant B 28102
run_variant C 28103 --always-reconnect --insecure
run_variant D 28104 --always-reconnect --reconnect-delay 30s

echo ""
echo "=== 汇总 ==="
cat "$RESULT"
