#!/bin/bash
# Restore the WARP accounts and bring the pool back up at a safe size.
#
# The 999 directories were destroyed when the pool was force-stopped, but
# instances-backup-100.tar.gz holds 100 accounts and /root/warp-instances holds
# the old system's 100. Registration itself still works (tested: 1s), so the
# plan is restore-first, register-later.

set -u
BASE=/root/warpgate-rs/instances
CFG=/root/warpgate-rs/rust-config.json
BK=/root/warpgate-rs/instances-backup-100.tar.gz

echo "=== 1. 现状 ==="
echo "  实例目录: $(ls "$BASE" 2>/dev/null | wc -l)"
echo "  备份包:   $(ls -lh "$BK" 2>/dev/null | awk '{print $5}')"

echo ""
echo "=== 2. 从备份恢复 ==="
if [ -f "$BK" ]; then
  TMP=/root/wg_restore
  rm -rf "$TMP"; mkdir -p "$TMP"
  tar -xzf "$BK" -C "$TMP"
  # 备份可能是 instances/warp-N 或直接 warp-N
  SRC="$TMP"
  [ -d "$TMP/instances" ] && SRC="$TMP/instances"
  n=0
  for d in "$SRC"/warp-*; do
    [ -d "$d" ] || continue
    name=$(basename "$d")
    if [ -s "$d/usque.json" ] && [ ! -s "$BASE/$name/usque.json" ]; then
      cp -r "$d" "$BASE/$name" && n=$((n+1))
    fi
  done
  echo "  从备份补入 $n 个"
  rm -rf "$TMP"
fi

echo ""
echo "=== 3. 从老系统目录补 ==="
m=0
for d in /root/warp-instances/warp-*; do
  [ -d "$d" ] || continue
  name=$(basename "$d")
  [ -s "$BASE/$name/usque.json" ] && continue
  # 老系统的目录结构不同，找 usque.json 或 config.json
  if [ -s "$d/usque.json" ]; then
    cp -r "$d" "$BASE/$name" && m=$((m+1))
  fi
done
echo "  从老系统补入 $m 个"

echo ""
echo "=== 4. 最终账号数 ==="
echo "  $(ls "$BASE" 2>/dev/null | wc -l) 个"
grep -oE '"(resident_count|max_instances)"[: ]+[0-9]+' "$CFG"

echo ""
echo "=== 5. 重启 ==="
systemctl restart warpgate
for i in $(seq 1 20); do
  sleep 3
  [ "$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)" = "200" ] && { echo "  console up"; break; }
done

echo ""
echo "=== 6. 观察 6 分钟 ==="
for i in $(seq 1 6); do
  sleep 60
  U=$(pgrep -c usque 2>/dev/null || echo 0)
  S=$(curl -s --max-time 8 -H "X-API-Key: zs1236547" http://127.0.0.1:4433/api/stats 2>/dev/null)
  printf "  %s usque=%s healthy=%s count=%s mem=%sMB\n" \
    "$(date +%H:%M:%S)" "${U:-0}" \
    "$(printf '%s' "$S" | grep -oE '"healthy_count":[0-9]+' | cut -d: -f2)" \
    "$(printf '%s' "$S" | grep -oE '"instance_count":[0-9]+' | cut -d: -f2)" \
    "$(free -m | awk '/^Mem:/{print $3}')"
done
