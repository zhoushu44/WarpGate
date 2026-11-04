#!/bin/bash
# Replace rounds 8 and 12 that MY OWN cleanup pkill killed (they had 0 reqs too,
# but the exit was external). Produces two clean observations.
set -u
DIR=/root/np
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
OUT=$DIR/hrfix.tsv
printf 'round\tstart_iso\tok\tfail\treqs\twall_s\tcpu_peak\tstall_cpu\tstall_rthreads\tstall_rss_kb\thung\treason\n' > "$OUT"

for r in 1 2; do
  log=$DIR/hrfix_$r.log
  start=$(date +%s)
  setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency 1 --total 200 \
      --target www.gstatic.com:80 --path /generate_204 > "$log" 2>&1 < /dev/null &
  pid=$!
  last_done=-1; last_change=$start; hung=0; reason=""; cpu_peak=0; prev_c=""; prev_t=""
  HZ=$(getconf CLK_TCK)
  while kill -0 "$pid" 2>/dev/null; do
    sleep 2
    now=$(date +%s)
    c=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null)
    if [ -n "$prev_c" ] && [ -n "$c" ]; then
      pct=$(( (c - prev_c) * 100 / HZ / (now - prev_t) ))
      [ "$pct" -gt "$cpu_peak" ] && cpu_peak=$pct
    fi
    prev_c=$c; prev_t=$now
    d=$(grep -ac '^REQ' "$log" 2>/dev/null)
    [ "$d" != "$last_done" ] && { last_done=$d; last_change=$now; }
    if [ $((now - last_change)) -ge 60 ]; then
      hung=1; reason="no_progress_60s"
      sc=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null)
      sr=$(ps -L -o stat= -p "$pid" 2>/dev/null | grep -c '^R')
      srs=$(awk '/VmRSS/{print $2}' "/proc/$pid/status" 2>/dev/null)
      echo "=== hrfix round $r ===" >> "$DIR/hrfix_stuck.txt"
      echo "iso=$(date '+%F %T') pid=$pid" >> "$DIR/hrfix_stuck.txt"
      ps -o pid,pcpu,nlwp,rss,etime,stat -p "$pid" >> "$DIR/hrfix_stuck.txt" 2>&1
      ps -L -o tid,pcpu,stat,wchan:32 -p "$pid" >> "$DIR/hrfix_stuck.txt" 2>&1
      grep -a '^PROGRESS' "$log" | tail -3 >> "$DIR/hrfix_stuck.txt"
      c1=$sc; t1=$(date +%s); sleep 3
      c2=$(awk '{print $14+$15}' "/proc/$pid/stat" 2>/dev/null); t2=$(date +%s)
      echo "cpu_between=$(( (c2-c1)*100/HZ/(t2-t1) ))%" >> "$DIR/hrfix_stuck.txt"
      echo >> "$DIR/hrfix_stuck.txt"
      kill -9 "$pid" 2>/dev/null
      break
    fi
    [ $((now - start)) -ge 300 ] && { hung=1; reason="hard_cap"; break; }
  done
  wait "$pid" 2>/dev/null
  end=$(date +%s)
  reqs=$(grep -ac '^REQ' "$log")
  ok=$(awk -F'\t' '$1=="REQ"&&$4==1' "$log" | wc -l)
  fail=$(awk -F'\t' '$1=="REQ"&&$4==0' "$log" | wc -l)
  sum=$(grep -ac '^SUMMARY' "$log")
  [ "$hung" = "0" ] && [ "$sum" = "0" ] && { hung=1; reason="exit_no_summary"; }
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$r" "$(date '+%F %T')" \
    "$ok" "$fail" "$reqs" "$((end-start))" "$cpu_peak" "-" "$sr" "${srs:-}" "$hung" "$reason" >> "$OUT"
  echo "replacement round=$r ok=$ok fail=$fail reqs=$reqs wall=$((end-start))s cpu_peak=$cpu_peak% hung=$hung $reason"
  sleep 1
done
