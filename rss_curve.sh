#!/bin/bash
set -u
L=/root/np/leak2000.log
echo "=== RSS vs completed dials (every ~100) ==="
printf '%8s %10s %8s\n' done rss_kb fd
grep -a '^PROGRESS' "$L" | awk -F'\t' '
{
  split($2,a,"="); split($6,r,"="); split($7,f,"=");
  d=a[2]+0; rss=r[2]+0; fd=f[2]+0;
  if (d >= next) { printf "%8d %10d %8d\n", d, rss, fd; next = int(d/100+1)*100 }
}' | awk 'NR==1 || NR%1==0' | head -25
echo
echo "=== first and last 3 PROGRESS ==="
grep -a '^PROGRESS' "$L" | head -3
echo "..."
grep -a '^PROGRESS' "$L" | tail -3
echo
echo "=== linearity check ==="
awk -F'\t' '$1=="2000"{printf "delta=%s KB over 2000 dials = %.2f KB/dial\n", $7, $7/2000}' /root/np/leak2000.tsv
awk -F'\t' '$1=="500"{printf "delta=%s KB over 500 dials = %.2f KB/dial\n", $7, $7/500}' /root/np/leak.tsv
echo
echo "=== latency proof: all successes land at ~io_timeout ==="
awk -F'\t' '$1=="REQ"&&$4==1{n++; if($5>=1150&&$5<=1300)inband++; sum+=$5} END{printf "n=%d in_1200ms_band=%d (%.1f%%) avg=%.0fms\n", n, inband, inband*100/n, sum/n}' "$L"
