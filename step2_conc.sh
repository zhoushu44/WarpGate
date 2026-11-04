#!/bin/bash
# STEP 2: concurrency 1 / 4 / 8 / 32, 20 dials each.
set -u
DIR=/root/np2
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
cd /root/warpgate-rs/app
OUT=$DIR/step2.tsv
printf 'conc\ttotal\tok\tfail\treqs\twall_s\tcpu_peak\tp50_ms\tp90_ms\tmax_ms\tsummary\tstatus\n' > "$OUT"

cpu_peak_of() {
  # $1=pid, $2=duration_s  -> peak CPU% sampled every 0.5 s while it ran
  :
}

for c in 1 4 8 32; do
  log=$DIR/step2_c$c.log
  t0=$(date +%s)
  setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency $c --total 20 \
      --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1 < /dev/null &
  pid=$!
  HZ=$(getconf CLK_TCK); peak=0; pc=""; pt=""
  while kill -0 "$pid" 2>/dev/null; do
    sleep 1
    now=$(date +%s); cn=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null)
    if [ -n "$pc" ] && [ -n "$cn" ]; then
      pct=$(( (cn - pc) * 100 / HZ / (now - pt) )); [ "$pct" -gt "$peak" ] && peak=$pct
    fi
    pc=$cn; pt=$now
  done
  wait "$pid" 2>/dev/null; rc=$?
  t1=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -ac '^REQ' "$log")
  sum=$(grep -ac '^SUMMARY' "$log")
  read -r p50 p90 mx < <(awk -F'\t' '$1=="REQ"{print $5}' "$log" | sort -n | awk '
    {a[NR]=$1} END{if(NR==0){print "0 0 0"; exit} print a[int(NR*0.5)], a[int(NR*0.9)], a[NR]}')
  printf '%s\t20\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$c" "$ok" "$fail" "$reqs" \
    "$((t1-t0))" "$peak" "${p50:-0}" "${p90:-0}" "${mx:-0}" "$sum" "$([ "$sum" -ge 1 ] && echo DONE || echo STUCK)" >> "$OUT"
  echo "conc=$c rc=$rc ok=$ok fail=$fail reqs=$reqs wall=$((t1-t0))s cpu_peak=$peak% p50=${p50}ms max=${mx}ms summary=$sum"
  sleep 1
done
echo "--- table ---"; cat "$OUT"
