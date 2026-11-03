#!/bin/bash
# Watch for the parent's redeploy: wait until stack.rs has the Drop fix (~646 lines).
set -u
S=/root/warpgate-rs/app/crates/wg-tunnel/src/connectip/stack.rs
LOG=/root/soak/deploy_watch.log
MAXMIN=${MAXMIN:-25}
: > "$LOG"

for m in $(seq 1 "$MAXMIN"); do
  if [ -f "$S" ]; then
    n=$(grep -c 'impl Drop for StackStream' "$S" 2>/dev/null); n=${n:-0}
    lines=$(wc -l < "$S" 2>/dev/null); lines=${lines:-0}
    lockorder=$(grep -c 'Lock order MUST match' "$S" 2>/dev/null); lockorder=${lockorder:-0}
    sockcount=$(grep -c 'pub async fn socket_count' "$S" 2>/dev/null); sockcount=${sockcount:-0}
    bin=$(stat -c '%y' /root/warpgate-rs/app/target/release/nativecheck 2>/dev/null || echo none)
  else
    n=-1; lines=-1; lockorder=-1; sockcount=-1; bin="missing"
  fi
  printf '%s min=%02d drop=%s lines=%s lockorder=%s socket_count=%s nativecheck_mtime=%s\n' \
    "$(date '+%F %T')" "$m" "$n" "$lines" "$lockorder" "$sockcount" "$bin" >> "$LOG"
  if [ "$n" = "1" ] && [ "$lines" -gt 620 ]; then
    echo "READY $(date '+%F %T') lines=$lines lockorder=$lockorder socket_count=$sockcount" >> "$LOG"
    exit 0
  fi
  sleep 60
done
echo "TIMEOUT $(date '+%F %T')" >> "$LOG"
exit 1
