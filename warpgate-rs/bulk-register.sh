#!/bin/bash
# Register Cloudflare WARP accounts until the instance directory holds $TARGET.
#
# Registration is rate limited: six concurrent workers get 429 with error 1015
# within seconds, while a serial loop at 2s spacing runs indefinitely. So this
# is serial with a small sleep, and it resumes from whatever already exists —
# re-running never re-registers an index that is already valid.

set -u
BASE=/root/warpgate-rs/instances
TARGET=${1:-1000}
SLEEP=${2:-2}
LOG=/root/register.log

echo "start target=$TARGET spacing=${SLEEP}s $(date '+%H:%M:%S')" >> "$LOG"

# Highest index we have, so new ones append instead of overwriting.
start=$(ls "$BASE" 2>/dev/null | sed 's/warp-//' | sort -n | tail -1)
start=${start:-0}
echo "existing=$start" >> "$LOG"

i=$((start + 1))
while [ "$i" -le "$TARGET" ]; do
  dir="$BASE/warp-$i"
  if [ -s "$dir/usque.json" ]; then
    i=$((i + 1))
    continue
  fi

  mkdir -p "$dir"
  tmp=$(mktemp -d)
  cd "$tmp" || exit 1
  if timeout 60 /usr/local/bin/usque register --accept-tos > /dev/null 2>&1 && [ -s config.json ]; then
    cp config.json "$dir/usque.json"
    rm -rf "$tmp"
    [ $((i % 25)) -eq 0 ] && echo "$(date '+%H:%M:%S') registered up to $i" >> "$LOG"
  else
    rm -rf "$tmp"
    echo "$(date '+%H:%M:%S') FAILED at $i" >> "$LOG"
  fi

  i=$((i + 1))
  sleep "$SLEEP"
done

echo "done $(date '+%H:%M:%S') total=$(ls "$BASE" | wc -l)" >> "$LOG"
