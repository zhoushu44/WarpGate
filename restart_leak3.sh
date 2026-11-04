#!/bin/bash
set -u
for p in $(ps -eo pid,comm --no-headers | awk '$2=="nativepress"{print $1}'); do kill -9 "$p" 2>/dev/null; done
for p in $(ps -eo pid,args --no-headers | grep -E 'bash /root/leak[0-9]*\.sh' | grep -v grep | awk '{print $1}'); do kill -9 "$p" 2>/dev/null; done
sleep 2
cd /root
setsid nohup bash /root/leak3.sh > /root/np/leak.out 2>&1 < /dev/null &
sleep 8
echo "--- leak.out ---"; cat /root/np/leak.out
echo "--- tsv ---"; cat /root/np/leak.tsv
