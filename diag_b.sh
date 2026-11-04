#!/bin/bash
set -u
L=/root/np2/soak_b.log
echo "=== the 10 failures in run b ==="
awk -F'\t' '$1=="REQ"&&$4==0{print $2"\t"$5"ms\tphase="$8"\t"$9}' "$L"
echo "=== phase tally for run b ==="
awk -F'\t' '$1=="REQ"{p[$8]++} END{for(k in p) print k, p[k]}' "$L"
echo "=== slowest 10 successes in run b ==="
awk -F'\t' '$1=="REQ"&&$4==1{print $5"\t"$2}' "$L" | sort -rn | head -10
echo "=== run b timeline: wall=60s for 1000 dials at conc 8 ==="
grep -a '^PROGRESS' "$L" | head -5
grep -a '^PROGRESS' "$L" | tail -5
