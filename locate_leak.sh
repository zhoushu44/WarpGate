#!/bin/bash
set -u
echo "=== who owns the :443 ESTABLISHED sockets? (top processes) ==="
ss -tanp 2>/dev/null | awk '$1=="ESTAB" && $0 ~ /:443/ {
  # process field is like users:(("usque",pid=123,fd=7))
  if (match($0, /\(\("[^"]+",pid=[0-9]+/)) {
    s = substr($0, RSTART+2, RLENGTH-2)
    split(s, a, ",")
    print a[1], a[2]
  } else print "unknown"
}' | sort | uniq -c | sort -rn | head -15

echo
echo "=== total ESTAB to :443 ==="
ss -tan 2>/dev/null | awk '$1=="ESTAB" && /:443/{c++} END{print c+0}'

echo
echo "=== count ESTAB by owning pid (top 10) ==="
ss -tanp 2>/dev/null | awk '$1=="ESTAB" && /:443/{
  if (match($0, /pid=[0-9]+/)) print substr($0, RSTART+4, RLENGTH-4)
}' | sort -n | uniq -c | sort -rn | head -10

echo
echo "=== per-pid detail for top pid ==="
toppid=$(ss -tanp 2>/dev/null | awk '$1=="ESTAB" && /:443/{ if (match($0,/pid=[0-9]+/)) print substr($0,RSTART+4,RLENGTH-4) }' | sort -n | uniq -c | sort -rn | head -1 | awk '{print $2}')
echo "top_pid=$toppid"
if [ -n "${toppid:-}" ]; then
  ps -o pid,ppid,etime,rss,nlwp,cmd -p "$toppid" 2>/dev/null
  echo "fd_count=$(ls /proc/$toppid/fd 2>/dev/null | wc -l)"
  echo "socket_inodes=$(ls -l /proc/$toppid/fd 2>/dev/null | grep -c socket:)"
  echo "--- its 443 peers (top 5 remote IPs) ---"
  ss -tanp 2>/dev/null | awk -v p="pid=$toppid," '$1=="ESTAB" && /:443/ && index($0,p){split($5,a,":"); print a[1]}' | sort | uniq -c | sort -rn | head -5
fi

echo
echo "=== warpgate pid fds ==="
wp=$(pgrep -x warpgate | head -1); echo "warpgate=$wp fd=$(ls /proc/$wp/fd 2>/dev/null | wc -l) socket=$(ls -l /proc/$wp/fd 2>/dev/null | grep -c socket:)"

echo
echo "=== all usque children fd total ==="
total=0; max=0; maxpid=0
for p in $(pgrep -x usque); do
  n=$(ls /proc/$p/fd 2>/dev/null | wc -l)
  total=$((total+n))
  if [ "$n" -gt "$max" ]; then max=$n; maxpid=$p; fi
done
echo "usque_count=$(pgrep -c -x usque) fd_total=$total fd_max=$max (pid $maxpid)"

echo
echo "=== tcp state histogram ==="
ss -tan 2>/dev/null | awk 'NR>1{print $1}' | sort | uniq -c

echo
echo "=== now ==="
date '+%F %T'
