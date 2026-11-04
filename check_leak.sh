#!/bin/bash
set -u
for f in /root/np/leak_*.log; do
  echo "$f: $(grep -ac '^SAMPLE' "$f") samples, $(grep -ac '^REQ' "$f") reqs, $(grep -ac '^SUMMARY' "$f") summary"
done
echo "--- n=150 samples ---"
grep -a '^SAMPLE' /root/np/leak_150.log
echo "--- n=150 tail ---"
tail -4 /root/np/leak_150.log | cat -v | head -8
