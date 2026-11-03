#!/bin/bash
# 5-minute sampler for the 30-minute soak.
# Writes one TSV row per sample with the requested metrics plus extras.
set -u
DIR=/root/soak
OUT=$DIR/monitor.tsv
N=${N:-7}
GAP=${GAP:-300}

mkdir -p "$DIR"
if [ ! -s "$OUT" ]; then
  printf 'iso\tepoch_ms\tpid\trss_kb\tthreads\tfd_count\tss443\tfree_mb\tfree_used_mb\tfree_bufcache_mb\tusque_children\ttimewait\tephemeral_used\tload1\testab_total\n' > "$OUT"
fi

for i in $(seq 1 "$N"); do
  pid=$(pgrep -x warpgate | head -1)
  iso=$(date '+%F %T')
  ep=$(date +%s%3N)
  if [ -n "${pid:-}" ] && [ -r "/proc/$pid/status" ]; then
    rss=$(awk '/^VmRSS:/{print $2}' /proc/$pid/status)
    thr=$(awk '/^Threads:/{print $2}' /proc/$pid/status)
    fd=$(ls /proc/$pid/fd 2>/dev/null | wc -l)
  else
    pid=0; rss=0; thr=0; fd=0
  fi
  ss443=$(ss -tn 2>/dev/null | grep -c ':443')
  read -r fm fu fb < <(free -m | sed -n 2p | awk '{print $2"\t"$3"\t"$6}')
  usq=$(pgrep -c -x usque)
  tw=$(ss -tan 2>/dev/null | awk '$1=="TIME-WAIT"{c++} END{print c+0}')
  # local ephemeral ports in use (32768..60999), from /proc/net/tcp + tcp6
  eph=$(awk 'NR>1{split($2,a,":"); p=strtonum("0x" a[2]); if(p>=32768 && p<=60999) c++} END{print c+0}' /proc/net/tcp /proc/net/tcp6)
  load1=$(awk '{print $1}' /proc/loadavg)
  est=$(ss -tan 2>/dev/null | awk '$1=="ESTAB"{c++} END{print c+0}')
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$iso" "$ep" "$pid" "${rss:-0}" "${thr:-0}" "${fd:-0}" "$ss443" "$fm" "$fu" "$fb" "$usq" "$tw" "$eph" "$load1" "$est" >> "$OUT"
  if [ "$i" -lt "$N" ]; then sleep "$GAP"; fi
done
