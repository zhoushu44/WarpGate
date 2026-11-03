#!/bin/bash
# Clean up every test artefact we created. Does NOT touch the production service,
# the app source tree, or the instances directory.
set -u

echo "=== 1. kill any remaining test process ==="
pkill -f 'bash /root/soak_v3.sh' 2>/dev/null
pkill -f 'bash /root/soak_monitor.sh' 2>/dev/null
pkill -f 'bash /root/lane2.sh' 2>/dev/null
pkill -f 'bash /root/leak_probe' 2>/dev/null
pkill -f 'bash /root/hang_rate.sh' 2>/dev/null
pkill -f 'bash /root/watch_deploy.sh' 2>/dev/null
pkill -f 'soak/bin/nativecheck' 2>/dev/null
pkill -f 'soak/bin/nativepress' 2>/dev/null
pkill -f 'release/nativepress' 2>/dev/null
sleep 2
echo "remaining test procs:"
ps -eo pid,cmd | grep -E 'soak_v3|soak_monitor|lane2|leak_probe|hang_rate|watch_deploy|soak/bin/native' | grep -v grep || echo "  none"

echo
echo "=== 2. verify the production service is untouched and healthy ==="
systemctl is-active warpgate
pgrep -x warpgate | head -1
pgrep -c -x usque

echo
echo "=== 3. archive data to a single tarball, then remove the working dir ==="
cd /root
if [ -d /root/soak ]; then
  tar czf /root/warpgate-native-soak-$(date +%Y%m%d_%H%M).tar.gz soak 2>/dev/null
  ls -lh /root/warpgate-native-soak-*.tar.gz | tail -1
fi

echo
echo "=== 4. remove scratch scripts (ours only) ==="
rm -f /root/probe.sh /root/probe2.sh /root/probe3.sh /root/probe4.sh /root/probe5.sh \
      /root/probe6.sh /root/probe7.sh /root/probe8.sh /root/probe9.sh \
      /root/status.sh /root/status2.sh /root/status3.sh /root/status4.sh \
      /root/inspect1.sh /root/inspect2.sh /root/inspect3.sh \
      /root/pilot.sh /root/validate_leak.sh /root/oldcode_report.sh \
      /root/restart_soak.sh /root/stop_all.sh /root/start_official.sh \
      /root/build_tools.sh /root/verify_build.sh /root/baseline_fixed.sh \
      /root/soak_run.sh /root/soak_monitor.sh /root/soak_v3.sh /root/analyze.sh \
      /root/leak_probe.sh /root/leak_probe_fixed.sh /root/lane2.sh /root/lane2_report.sh \
      /root/pick_target.sh /root/restart_lane2.sh /root/repro_hang.sh /root/grab_hang.sh \
      /root/capture2.sh /root/capture3.sh /root/round26.sh /root/final_check.sh \
      /root/hang_rate.sh /root/hang_summary.sh /root/start_hangrate.sh \
      /root/locate_leak.sh /root/rate_detail.sh /root/check_src2.sh /root/make_report.sh \
      /root/watch_deploy.sh /root/fetch_results.py 2>/dev/null
echo "scripts removed"

echo
echo "=== 5. leave only the tarball + report ==="
cp /root/soak/final_report.txt /root/warpgate-native-soak-report.txt 2>/dev/null
rm -rf /root/soak
ls -lh /root/warpgate-native-soak* 2>/dev/null
echo
echo "=== 6. final service check ==="
systemctl is-active warpgate
ps -o pid,etime,rss,nlwp -p $(pgrep -x warpgate | head -1)
date '+%F %T'
