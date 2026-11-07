#!/bin/bash
# Add 127.0.0.1 to the whitelist without breaking the JSON.
#
# The array is multi-line and the previous attempts either left it unclosed or
# produced a missing comma. This edits it line by line: find the closing "],"
# of ip_whitelist and append an entry before it, keeping trailing commas valid.

set -u
CFG=/root/warpgate-rs/rust-config.json
BAK=/root/warpgate-rs/rust-config.json.bak-20261107-144513

# Start from the known-good copy.
cp "$BAK" "$CFG"

# Locate the ip_whitelist block.
START=$(grep -n '"ip_whitelist": \[' "$CFG" | head -1 | cut -d: -f1)
if [ -z "$START" ]; then
  echo "找不到 ip_whitelist"
  exit 1
fi

# The closing bracket is the first line after START that is just "  ],"
END=""
i=$((START + 1))
while [ "$i" -le $((START + 40)) ]; do
  line=$(sed -n "${i}p" "$CFG")
  case "$line" in
    *"],"*|*"]"*) END=$i; break ;;
  esac
  i=$((i + 1))
done

if [ -z "$END" ]; then
  echo "找不到数组结束"
  exit 1
fi
echo "ip_whitelist 块: 第 $START 行 到 第 $END 行"

# Insert one entry just before the closing line, and make sure the previous
# entry ends with a comma.
head -n $((END - 1)) "$CFG" > /tmp/cfg.new
prev=$(tail -1 /tmp/cfg.new)
case "$prev" in
  *,*) : ;;
  *)   sed -i '$ s/$/,/' /tmp/cfg.new ;;
esac
echo '    "127.0.0.1"' >> /tmp/cfg.new
tail -n +"$END" "$CFG" >> /tmp/cfg.new
mv /tmp/cfg.new "$CFG"

echo ""
echo "=== 结果 ==="
sed -n "$((START)),$((END + 1))p" "$CFG"

echo ""
echo "=== 逗号/括号粗检 ==="
echo "  开括号: $(grep -c '\[$' "$CFG")  闭括号行: $(grep -c '^\s*\],\?$' "$CFG")"
echo "  127.0.0.1 出现: $(grep -c '127.0.0.1' "$CFG")"

echo ""
echo "=== 重启 ==="
systemctl restart warpgate
for i in $(seq 1 20); do
  sleep 3
  [ "$(curl -s --max-time 3 -o /dev/null -w '%{http_code}' http://127.0.0.1:4433/ 2>/dev/null)" = "200" ] && { echo "  console up"; break; }
done
journalctl -u warpgate --no-pager -n 20 2>/dev/null | grep -E "解析配置|实例目录|启动失败" | tail -2
