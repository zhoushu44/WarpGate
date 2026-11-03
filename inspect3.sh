#!/bin/bash
set -u
cd /root/soak
echo "=== HTTP status per run (from raw body line) ==="
for f in raw/*.log; do
  s=$(sed -n 's/^body: HTTP\/1\.[0-9] \([0-9][0-9][0-9]\).*/\1/p' "$f" | head -1)
  echo "${s:-none}"
done | sort | uniq -c
echo
echo "=== first 429 run ==="
for f in $(ls raw/*.log | sort); do
  if grep -q '^body: HTTP/1.1 429' "$f"; then echo "$f"; break; fi
done
echo "=== warpgate env (still usque/quic?) ==="
tr '\0' '\n' < /proc/$(pgrep -x warpgate | head -1)/environ | grep -E 'WARP|PATH' 
echo "=== unit env after redeploy ==="
grep -E 'Environment|ExecStart' /etc/systemd/system/warpgate.service
echo "=== health endpoint? ==="
curl -s -m 5 http://127.0.0.1:4433/health 2>/dev/null | head -c 300; echo
echo "=== monitor ==="
cat monitor.tsv
echo "=== now ==="
date '+%F %T'
