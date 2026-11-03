#!/bin/bash
# Hang-rate runner for the native CONNECT-IP data plane.
#
# A round is one long-lived process doing `total` dials with `conc` workers.
# "Hung" = either the process made zero forward progress for STALL seconds,
# or it outlived HARD_CAP, or it exited without a SUMMARY line.
#
# Per round we record: ok, fail, wall, CPU peak (%), hung, plus a stuck-process
# snapshot (CPU%, R-state thread count, RSS, fd) when it hangs.
set -u
DIR=/root/np
mkdir -p "$DIR"
BIN=/root/warpgate-rs/app/target/release/nativepress
CFG=/root/warpgate-rs/instances/warp-1/usque.json
TARGET=${TARGET:-www.gstatic.com:80}
PATHQ=${PATHQ:-/generate_204}
ROUNDS=${ROUNDS:-20}
TOTAL=${TOTAL:-200}
CONC=${CONC:-1}
STALL=${STALL:-60}
HARD_CAP=${HARD_CAP:-300}
TAG=${TAG:-hr}
OUT=$DIR/$TAG.tsv

HZ=$(getconf CLK_TCK)
cpu_ticks() { awk '{print $14+$15}' "/proc/$1/stat" 2>/dev/null; }

if [ ! -s "$OUT" ]; then
  printf 'round\tstart_iso\tok\tfail\treqs\twall_s\tcpu_peak\tstall_cpu\tstall_rthreads\tstall_rss_kb\thung\treason\n' > "$OUT"
fi

for r in $(seq 1 "$ROUNDS"); do
  log=$DIR/${TAG}_$r.log
  start=$(date +%s)
  setsid nohup "$BIN" "$CFG" --tunnels 1 --concurrency "$CONC" --total "$TOTAL" \
      --target "$TARGET" --path "$PATHQ" > "$log" 2>&1 < /dev/null &
  pid=$!
  cpu_peak=0
  last_done=-1
  last_change=$start
  hung=0
  reason=""
  stall_cpu=""; stall_r=""; stall_rss=""

  while kill -0 "$pid" 2>/dev/null; do
    sleep 2
    now=$(date +%s)
    c=$(cpu_ticks "$pid")
    if [ -n "${prev_c:-}" ] && [ -n "$c" ]; then
      pct=$(( (c - prev_c) * 100 / HZ / (now - prev_t) ))
      [ "$pct" -gt "$cpu_peak" ] && cpu_peak=$pct
    fi
    prev_c=$c; prev_t=$now

    done_n=$(grep -c '^REQ' "$log" 2>/dev/null)
    if [ "$done_n" != "$last_done" ]; then
      last_done=$done_n; last_change=$now
    fi

    if [ $((now - last_change)) -ge "$STALL" ]; then
      hung=1; reason="no_progress_${STALL}s"
      stall_cpu=$(cpu_ticks "$pid")
      stall_rss=$(awk '/VmRSS/{print $2}' "/proc/$pid/status" 2>/dev/null)
      stall_r=$(ps -L -o stat= -p "$pid" 2>/dev/null | grep -c '^R')
      echo "  [round $r] STALL detected after $((now-last_change))s, done=$done_n" 
      break
    fi
    if [ $((now - start)) -ge "$HARD_CAP" ]; then
      hung=1; reason="hard_cap_${HARD_CAP}s"
      stall_cpu=$(cpu_ticks "$pid")
      stall_rss=$(awk '/VmRSS/{print $2}' "/proc/$pid/status" 2>/dev/null)
      stall_r=$(ps -L -o stat= -p "$pid" 2>/dev/null | grep -c '^R')
      break
    fi
  done

  end=$(date +%s)
  if [ "$hung" = "1" ]; then
    # Snapshot the stuck process *before* killing it.
    if kill -0 "$pid" 2>/dev/null; then
      echo "=== round $r stuck snapshot ===" >> "$DIR/${TAG}_stuck.txt"
      echo "iso=$(date '+%F %T') pid=$pid" >> "$DIR/${TAG}_stuck.txt"
      ps -o pid,pcpu,nlwp,rss,etime,stat -p "$pid" >> "$DIR/${TAG}_stuck.txt" 2>&1
      ps -L -o tid,pcpu,stat,wchan:32 -p "$pid" >> "$DIR/${TAG}_stuck.txt" 2>&1
      grep '^PROGRESS' "$log" | tail -3 >> "$DIR/${TAG}_stuck.txt"
      echo >> "$DIR/${TAG}_stuck.txt"
      # second CPU reading 3 s later to show it is idle, not spinning
      c1=$(cpu_ticks "$pid"); t1=$(date +%s); sleep 3
      c2=$(cpu_ticks "$pid"); t2=$(date +%s)
      echo "cpu_between=$(( (c2-c1)*100/HZ/(t2-t1) ))%" >> "$DIR/${TAG}_stuck.txt"
      kill -9 "$pid" 2>/dev/null
    fi
  fi
  wait "$pid" 2>/dev/null

  reqs=$(grep -c '^REQ' "$log" 2>/dev/null)
  ok=$(awk -F'\t' '$1=="REQ" && $4==1' "$log" 2>/dev/null | wc -l)
  fail=$(awk -F'\t' '$1=="REQ" && $4==0' "$log" 2>/dev/null | wc -l)
  sum=$(grep -c '^SUMMARY' "$log" 2>/dev/null)
  if [ "$hung" = "0" ] && [ "$sum" = "0" ]; then hung=1; reason="exit_no_summary"; fi
  wall=$((end - start))

  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$r" "$(date '+%F %T')" "${ok:-0}" "${fail:-0}" "${reqs:-0}" "$wall" "$cpu_peak" \
    "${stall_cpu:-}" "${stall_r:-}" "${stall_rss:-}" "$hung" "${reason:-ok}" >> "$OUT"
  echo "round=$r ok=${ok:-0} fail=${fail:-0} reqs=${reqs:-0} wall=${wall}s cpu_peak=${cpu_peak}% hung=$hung ${reason}"
  sleep 1
done
echo "#DONE	$(date '+%F %T')" >> "$OUT"
echo "=== totals ==="
awk -F'\t' '$1 ~ /^[0-9]+$/ {n++; if($11==1)h++; ok+=$3; fail+=$4} END{printf "rounds=%d hung=%d rate=%.1f%% ok=%d fail=%d\n", n, h+0, (h+0)*100/n, ok, fail}' "$OUT"
