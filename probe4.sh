#!/bin/bash
echo "=== now ==="
date '+%F %T %Z'
echo "=== nativecheck stat ==="
stat -c '%y %s %h %n' /root/warpgate-rs/app/target/release/nativecheck /root/warpgate-rs/app/target/release/warpgate
echo "=== cargo/rustc running? ==="
ps -eo pid,ppid,etime,cmd | grep -E 'cargo|rustc' | grep -v grep
echo "=== active ssh sessions ==="
who
ps -eo pid,etime,cmd | grep -E 'sshd: root' | grep -v grep
echo "=== other app copies ==="
ls -d /root/warpgate-rs/*/ 2>/dev/null
find /root -maxdepth 4 -name 'stack.rs' -path '*connectip*' -printf '%TY-%Tm-%Td %TH:%TM %p\n' 2>/dev/null
echo "=== recent shell history ==="
tail -15 /root/.bash_history 2>/dev/null
echo "=== warpgate log tail ==="
tail -5 /root/warpgate-rs/warpgate.log
