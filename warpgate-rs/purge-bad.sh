#!/bin/bash
# Purge the accounts registered by the buggy binary, then re-register them.
#
# The bad ones are identifiable without guessing: their private_key is PKCS#8
# ("MIGHAgEAMBMG...") instead of SEC1 ("MHcCAQEE..."). Anything created before
# the fix is unusable by usque and would keep failing at engine start.

set -u
BASE=/root/warpgate-rs/instances
BAD=/root/warpgate-rs/instances-bad
mkdir -p "$BAD"

keep=0
purge=0
for d in "$BASE"/warp-*; do
  [ -d "$d" ] || continue
  f="$d/usque.json"
  [ -f "$f" ] || { rm -rf "$d"; continue; }
  # PKCS#8 private keys start with MIG; SEC1 starts with MHc.
  if grep -q '"private_key": *"MIG' "$f"; then
    mv "$d" "$BAD/$(basename "$d")"
    purge=$((purge+1))
  else
    keep=$((keep+1))
  fi
done

echo "kept=$keep purged=$purge"
echo "remaining: $(ls "$BASE" | grep -c '^warp-')"
