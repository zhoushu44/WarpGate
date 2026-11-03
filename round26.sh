#!/bin/bash
set -u
cd /root/soak
echo "=== lane2_26.log FULL (tail 40) ==="
tail -40 lane2_26.log
echo
echo "=== line count ==="
wc -l lane2_26.log
echo
echo "=== REQ count and last REQs ==="
grep -c '^REQ' lane2_26.log
grep '^REQ' lane2_26.log | tail -5
echo
echo "=== distinct REQ indices count ==="
grep '^REQ' lane2_26.log | cut -f2 | sort -n | uniq | wc -l
echo
echo "=== SAMPLE lines all ==="
grep '^SAMPLE' lane2_26.log
echo
echo "=== log mtime vs now ==="
ls -l --time-style=+%F_%T lane2_26.log
date '+%F %T'
