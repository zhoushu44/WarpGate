#!/bin/bash
P=$(pgrep usque | head -1)
echo "pid=$P"
echo "=== /proc/PID/environ (tr) ==="
cat /proc/$P/environ 2>&1 | tr '\0' '\n' | head -20
echo "=== 直接 grep 二进制内容 ==="
grep -c GOGC /proc/$P/environ 2>&1
echo "=== 二进制里有 GOGC 注入代码吗 ==="
strings /root/warpgate-rs/app/target/release/warpgate 2>/dev/null | grep -c "GOMEMLIMIT"
echo "=== warpgate 版本 ==="
/root/warpgate-rs/app/target/release/warpgate --version 2>&1
