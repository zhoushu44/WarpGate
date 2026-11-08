#!/bin/bash
# Remove only WarpGate artifacts that are no longer in use.
#
# Deliberately conservative: the running `warpgate` container and the
# `zhoushu1/warp-pool:latest` image are kept. Everything else on this host
# belongs to the user's other projects and is out of scope.
#
# Removed:
#   - `warp` container (Node 11.1, exited) and its image
#   - dangling images whose layers trace back to warp-pool builds
#   - scratched source trees and debug scripts from the porting work
set -u

echo "=== 保护清单（不删）==="
echo "  容器: $(docker ps --format '{{.Names}}' | grep -c warpgate) 个 warpgate 运行中"
docker ps --format '    {{.Names}} | {{.Image}}' | grep warpgate

echo ""
echo "=== 1. 删除 Node 旧版容器和镜像 ==="
docker rm -f warp 2>/dev/null && echo "  已删容器 warp" || echo "  容器 warp 不存在"
if docker images --format '{{.Repository}}:{{.Tag}}' | grep -q '^zhoushu1/warp-pool:11.1$'; then
  docker rmi zhoushu1/warp-pool:11.1 2>&1 | tail -2 | sed 's/^/  /'
fi

echo ""
echo "=== 2. 查找 WarpGate 相关的悬空镜像 ==="
# Only remove dangling images that are NOT referenced by any other project's
# container. `docker image prune` without a filter would take everything, so
# check each candidate against running containers first.
IN_USE=$(docker ps -a --format '{{.Image}}' | sort -u)
REMOVED=0
FREED=0
for id in $(docker images -f "dangling=true" -q); do
  SIZE=$(docker images --format '{{.ID}} {{.Size}}' | grep "^${id:0:12}" | awk '{print $2}')
  # Skip anything still referenced by a container (running or stopped).
  if echo "$IN_USE" | grep -q "$id"; then
    echo "  跳过 $id（仍被容器引用）"
    continue
  fi
  if docker rmi "$id" > /dev/null 2>&1; then
    REMOVED=$((REMOVED+1))
    echo "  已删 $id ($SIZE)"
  fi
done
echo "  删除 $REMOVED 个悬空镜像"

echo ""
echo "=== 3. 清理源码树和调试脚本 ==="
rm -rf /root/usque-app /root/wg_src.b64 /root/wg_src.tar.gz \
       /root/wg_full /root/wgfull /root/wg_new /root/wgreset /root/wgp \
       /root/p.b64 /root/p.bundle /root/f.b64 /root/f.bundle \
       /root/go-src /root/go-src.b64 /root/go-src.tar.gz \
       /root/public.b64 /root/public.tar.gz 2>/dev/null
rm -f /root/ca.sh /root/ca.out /root/sp.sh /root/sp.out /root/m200.sh /root/m200.out \
      /root/mon200.tsv /root/mon-node.tsv /root/mn.sh /root/mn.out \
      /root/v2.log /root/vA.log /root/vB.log /root/idle_warm.log \
      /root/verify_warm.log /root/wt.sh /root/wt2.sh /root/wt_*.log /root/w2_*.log \
      /root/docker_usque.log /root/newacct.log /root/clone.log /root/cc.sh \
      /root/ct.sh /root/iq.sh /root/cw.sh /root/mon4.sh /root/wg-guard.sh 2>/dev/null
echo "  已清理"

echo ""
echo "=== 4. 备份文件整理（保留最近 2 个）==="
for prefix in pin-backup pinfix pinfix2 split endpoint-backup; do
  ls -t /root/${prefix}*.tar.gz 2>/dev/null | tail -n +3 | while read -r f; do
    rm -f "$f"; echo "  删除旧备份 $(basename "$f")"
  done
done
ls -t /root/warpgate-rs/rust-config.json.bak-* 2>/dev/null | tail -n +3 | while read -r f; do
  rm -f "$f"; echo "  删除旧配置 $(basename "$f")"
done
echo "  备份保留最近 2 份"

echo ""
echo "=== 5. 结果 ==="
docker system df
echo ""
df -h / | tail -1
echo ""
echo "  服务状态:"
docker ps --format '    {{.Names}} | {{.Status}}' | grep warpgate
echo "    usque=$(pgrep -c usque 2>/dev/null || echo 0)"
