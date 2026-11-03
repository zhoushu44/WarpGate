#!/bin/bash
/usr/bin/time -f 'MAXRSS_KB %M' -o /tmp/t.time /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/instances/warp-1/usque.json >/dev/null 2>&1
echo "rc=$?"
cat /tmp/t.time
echo "--- busybox time? ---"
command -v time
ls -l /usr/bin/time
