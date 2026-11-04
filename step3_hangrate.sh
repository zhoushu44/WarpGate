#!/bin/bash
# STEP 3: 20 rounds x 200 dials. Hang = 60 s without a new REQ, or over hard cap.
set -u
DIR=/root/np2
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=${CFG:-/root/warpgate-rs/instances/warp-1/usque.json}
TOTAL=${TOTAL:-200}
CONC=${CONC:-1}
ROUNDS=${ROUNDS:-20}
STALL=${STALL:-60}
CAP=${CAP:-300}
TAG=${TAG:-hr}
OUT=$DIR/$TAG.tsv
HZ=$(getconf CLK_TCK)

[ -s "$OUT" ] || printf 'round\tstart_iso\tok\tfail\treqs\twall_s\tcpu_peak\tp50_ms\tp90_ms\tmax_ms\thung\treason\n' > "$OUT"

for r in $(seq 1 "$ROUNDS"); do
  log=$DIR/${TAG}_$r.log
  start=$(date +%s)
  setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency "$CONC" --total "$TOTAL" \
      --io-timeout 15 --dial-timeout 20 --target www.gstatic.com:80 --path /generate_204 \
      > "$log" 2>&1 < /dev/null &
  pid=$!
  last=-1; change=$start; hung=0; reason=""; peak=0; pc=""; pt=""
  while kill -0 "$pid" 2>/dev/null; do
    sleep 1
    now=$(date +%s)
    cn=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null)
    if [ -n "$pc" ] && [ -n "$cn" ]; then
      pct=$(( (cn - pc) * 100 / HZ / (now - pt) )); [ "$pct" -gt "$peak" ] && peak=$pct
    fi
    pc=$cn; pt=$now
    d=$(grep -ac '^REQ' "$log" 2>/dev/null)
    [ "$d" != "$last" ] && { last=$d; change=$now; }
    if [ $((now - change)) -ge "$STALL" ]; then
      hung=1; reason="no_progress_${STALL}s"
      echo "=== $TAG round $r stuck ===" >> "$DIR/${TAG}_stuck.txt"
      echo "iso=$(date '+%F %T') pid=$pid done=$d" >> "$DIR/${TAG}_stuck.txt"
      ps -o pid,pcpu,nlwp,rss,etime,stat -p "$pid" >> "$DIR/${TAG}_stuck.txt" 2>&1
      ps -L -o tid,pcpu,stat,wchan:32 -p "$pid" >> "$DIR/${TAG}_stuck.txt" 2>&1
      grep -a '^PROGRESS' "$log" | tail -3 >> "$DIR/${TAG}_stuck.txt"
      echo >> "$DIR/${TAG}_stuck.txt"
      kill -9 "$pid" 2>/dev/null
      break
    fi
    [ $((now - start)) -ge "$CAP" ] && { hung=1; reason="cap_${CAP}s"; kill -9 "$pid" 2>/dev/null; break; }
  done
  wait "$pid" 2>/dev/null
  end=$(date +%s)
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  reqs=$(grep -ac '^REQ' "$log")
  sum=$(grep -ac '^SUMMARY' "$log")
  [ "$hung" = "0" ] && [ "$sum" = "0" ] && { hung=1; reason="exit_no_summary"; }
  read -r p50 p90 mx < <(awk -F'\t' '$1=="REQ"{print $5}' "$log" | sort -n | awk '{a[NR]=$1} END{if(NR==0){print 0,0,0; exit} print a[int(NR*0.5)], a[int(NR*0.9)], a[NR]}')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$r" "$(date '+%F %T')" \
    "$ok" "$fail" "$reqs" "$((end-start))" "$peak" "${p50:-0}" "${p90:-0}" "${mx:-0}" "$hung" "${reason:-ok}" >> "$OUT"
  echo "round=$r ok=$ok fail=$fail reqs=$reqs wall=$((end-start))s cpu_peak=$peak% p50=${p50}ms max=${mx}ms hung=$hung ${reason}"
  sleep 1
done
echo "#DONE $(date '+%F %T')" >> "$OUT"
awk -F'\t' '$1 ~ /^[0-9]+$/ {n++; if($11==1)h++; ok+=$3; fail+=$4; w+=$6; if($7>cpmax)cpmax=$7}
END{printf "=== rounds=%d hung=%d rate=%.1f%% ok=%d fail=%d avg_wall=%.1fs cpu_peak_max=%d%%\n", n, h+0, (h+0)*100/n, ok, fail, w/n, cpmax}' "$OUT"
