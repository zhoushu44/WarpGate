# 全量 API 冒烟测试：逐条打所有路由，记录状态码与关键字段
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if (-not $s) { Write-Host "SSH failed"; exit 1 }

$script = @'
K=zs1236547
B=http://127.0.0.1:4433
H="X-API-Key: $K"
J="Content-Type: application/json"

hit() {
  local method=$1 path=$2 data=$3
  local out code
  if [ -n "$data" ]; then
    out=$(curl -s --max-time 40 -o /tmp/r.txt -w "%{http_code}" -X $method -H "$H" -H "$J" -d "$data" "$B$path")
  else
    out=$(curl -s --max-time 40 -o /tmp/r.txt -w "%{http_code}" -X $method -H "$H" "$B$path")
  fi
  code=$out
  local body=$(head -c 110 /tmp/r.txt | tr -d '\n')
  printf "%-6s %-34s %s  %s\n" "$method" "$path" "$code" "$body"
}

echo "===== 只读接口 ====="
hit GET /api/stats
hit GET /api/config
hit GET /api/containers
hit GET /api/check
hit GET /api/whitelist
hit GET /api/usage
hit GET /api/pool/status
hit GET /api/pool/dedicated
hit GET /api/pool/dedicated/history
hit GET /api/proxies?num=2
hit GET "/api/proxies?num=1&type=json&sid=smoke"

echo ""
echo "===== 代理池 ====="
hit GET "/api/pool/acquire?sid=smoke1"
hit POST /api/pool/rotate '{"index":0}'
hit POST /api/pool/report '{"session_id":"smoke1","result":403}'
hit POST /api/pool/release '{"session_id":"smoke1"}'

echo ""
echo "===== 固定 IP ====="
hit POST /api/pool/dedicated/bind '{"name":"warp-0","remark":"smoke"}'
hit POST /api/pool/dedicated/remark '{"name":"warp-0","remark":"smoke2"}'
hit POST /api/pool/dedicated/unbind '{"name":"warp-0"}'

echo ""
echo "===== 白名单 ====="
hit POST /api/whitelist '{"ip":"1.2.3.4"}'

echo ""
echo "===== 生命周期（用最后一个实例，不影响主池）====="
hit POST /api/stop '{"index":99}'
sleep 2
hit POST /api/start '{"index":99}'

echo ""
echo "===== 鉴权 ====="
printf "%-6s %-34s %s\n" "GET" "/api/stats (无key)" "$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' $B/api/stats)"
printf "%-6s %-34s %s\n" "POST" "/api/login (错key)" "$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' -X POST -H "$J" -d '{"key":"wrong"}' $B/api/login)"
printf "%-6s %-34s %s\n" "GET" "/ (控制台)" "$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' $B/)"
printf "%-6s %-34s %s\n" "GET" "/docs" "$(curl -s --max-time 10 -o /dev/null -w '%{http_code}' $B/docs)"

echo ""
echo "===== 关键字段抽查 ====="
curl -s --max-time 15 -H "$H" $B/api/containers > /tmp/c.json
node -e '
const d=JSON.parse(require("fs").readFileSync("/tmp/c.json","utf8")).data;
const need=["container","instance_index","host_port","pool_status","ipv6","ipv4","ip_mode","health","username","password","cf_403_rate"];
const missing=need.filter(k=>!(k in d[0]));
console.log("字段缺失:", missing.length?missing.join(","):"无");
console.log("首条:", JSON.stringify({container:d[0].container,port:d[0].host_port,status:d[0].pool_status,health:d[0].health,ipv6:d[0].ipv6,ipv4:d[0].ipv4,mode:d[0].ip_mode,user:d[0].username}));
const withV4=d.filter(x=>x.ipv4).length, withV6=d.filter(x=>x.ipv6).length;
console.log(`出口探测: IPv6 ${withV6}/${d.length}, IPv4 ${withV4}/${d.length}`);
' 2>/dev/null || echo "(node 不可用，跳过字段抽查)"
'@

$r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 600 -Command $script 2>$null
Write-Host $r.Output
Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
