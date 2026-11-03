#!/bin/bash
# Stop every test process started for the soak, archive what we have.
set -u
pkill -f 'bash /root/soak_run.sh' 2>/dev/null
pkill -f 'bash /root/soak_monitor.sh' 2>/dev/null
pkill -f 'bash /root/leak_probe.sh' 2>/dev/null
sleep 1
pkill -f '/root/soak/bin/nativecheck' 2>/dev/null
pkill -f 'release/nativepress' 2>/dev/null
sleep 2
echo "=== remaining test procs (should be empty) ==="
ps -eo pid,etime,cmd | grep -E 'soak_run|soak_monitor|leak_probe|nativecheck|nativepress' | grep -v grep
echo "=== archive old-code results ==="
cd /root/soak
ts=$(date +%Y%m%d_%H%M%S)
mkdir -p oldcode
[ -s nativecheck.tsv ] && cp nativecheck.tsv oldcode/nativecheck_${ts}.tsv
[ -s monitor.tsv ] && cp monitor.tsv oldcode/monitor_${ts}.tsv
[ -s leak.tsv ] && cp leak.tsv oldcode/leak_${ts}.tsv
echo "archived to /root/soak/oldcode"
ls -l /root/soak/oldcode | tail -5
echo "=== pre-wait source state ==="
S=/root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
grep -c 'impl Drop for StackStream' $S 2>/dev/null || echo "0 (or file missing)"
wc -l $S 2>/dev/null || echo "missing"
date '+%F %T'
