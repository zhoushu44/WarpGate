#!/bin/bash
set -u
cd /root/soak
echo "=== old-code archive checksums ==="
md5sum oldcode/*.tsv
echo "=== old-code nativecheck: ok/fail ==="
awk -F'\t' '{if($4==1)o++;else f++} END{printf "ok=%d fail=%d total=%d rate=%.1f%%\n", o+0, f+0, o+f, (o+0)*100/((o+f)>0?(o+f):1)}' oldcode/nativecheck_*.tsv
echo "=== old-code failure reasons ==="
awk -F'\t' '$4==0{print $8}' oldcode/nativecheck_*.tsv | sort | uniq -c | sort -rn
echo "=== old-code attempts histogram (1=single try, 8=all retries burned) ==="
awk -F'\t' '{print $7}' oldcode/nativecheck_*.tsv | sort -n | uniq -c
echo "=== restart deploy watcher ==="
pkill -f watch_deploy.sh 2>/dev/null
sleep 1
setsid nohup bash /root/watch_deploy.sh > /root/soak/watch.out 2>&1 < /dev/null &
sleep 2
cat /root/soak/deploy_watch.log
