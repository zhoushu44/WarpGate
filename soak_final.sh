#!/bin/bash
# Final soak on the deployed build: 3 x 1000 dials at concurrency 8,
# plus one long-lived 5000-dial process to catch slow leaks or late hangs.
set -u
DIR=/root/np2
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-4/usque.json}
OUT=$DIR/soak.tsv
printf 'run\ttotal\tconc\tok\tfail\treqs\twall_s\tcpu_peak\trss_before_kb\trss_after_kb\tsocks_end\tfd_end\thung\n' > "$OUT"
HZ=$(getconf CLK_TCK)

run_one() {
  local total=$1 conc=$2 tag=$3
  local log=$DIR/soak_$tag.log
  local t0=$(date +%s)
  setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency "$conc" --total "$total" \
      --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1 < /dev/null &
  local pid=$!
  local peak=0 pc="" pt="" last=-1 change=$t0 hung=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 2
    local now=$(date +%s)
    local cn=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null)
    if [ -n "$pc" ] && [ -n "$cn" ]; then
      local pct=$(( (cn - pc) * 100 / HZ / (now - pt) ))
      [ "$pct" -gt "$peak" ] && peak=$pct
    fi
    pc=$cn; pt=$now
    local d=$(grep -ac '^REQ' "$log" 2>/dev/null)
    [ "$d" != "$last" ] && { last=$d; change=$now; }
    if [ $((now - change)) -ge 60 ]; then hung=1; kill -9 "$pid" 2>/dev/null; break; fi
    [ $((now - t0)) -ge 600 ] && { hung=1; kill -9 "$pid" 2>/dev/null; break; }
  done
  wait "$pid" 2>/dev/null
  local t1=$(date +%s)
  local ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  local fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  local reqs=$(grep -ac '^REQ' "$log")
  local b=$(grep -a '^SAMPLE' "$log" | grep -a after_bringup | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  local a=$(grep -a '^SAMPLE' "$log" | grep -a '	end	' | sed -n 's/.*rss_kb=\([0-9]*\).*/\1/p')
  local sk=$(grep -a '^SAMPLE' "$log" | grep -a '	end	' | sed -n 's/.*socks=\([0-9]*\).*/\1/p')
  local fd=$(grep -a '^SAMPLE' "$log" | grep -a '	end	' | sed -n 's/.*fd=\([0-9]*\).*/\1/p')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$tag" "$total" "$conc" \
    "$ok" "$fail" "$reqs" "$((t1-t0))" "$peak" "${b:-0}" "${a:-0}" "${sk:-0}" "${fd:-0}" "$hung" >> "$OUT"
  echo "run=$tag total=$total conc=$conc ok=$ok fail=$fail reqs=$reqs wall=$((t1-t0))s cpu_peak=$peak% rss=${b:-0}->${a:-0} fd=${fd:-0} hung=$hung"
}

run_one 1000 8 a
run_one 1000 8 b
run_one 1000 8 c
run_one 5000 8 long
echo "--- table ---"; cat "$OUT"
