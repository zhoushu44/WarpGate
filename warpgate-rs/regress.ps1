# 一次性全量回归：所有路由 + 关键字段 + 数据完整性
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if (-not $s) { Write-Host "SSH FAIL"; exit 1 }

$script = @'
K=zs1236547; B=http://127.0.0.1:4433; H="X-API-Key: $K"; J="Content-Type: application/json"
pass=0; fail=0
chk() { # name expected actual
  if [ "$2" = "$3" ]; then pass=$((pass+1)); printf "  OK   %s\n" "$1"
  else fail=$((fail+1)); printf "  FAIL %s (want %s got %s)\n" "$1" "$2" "$3"; fi
}
code() { curl -s --max-time 40 -o /tmp/b.txt -w "%{http_code}" "$@"; }

echo "== 只读 =="
chk "stats"        200 $(code -H "$H" $B/api/stats)
chk "config"       200 $(code -H "$H" $B/api/config)
chk "containers"   200 $(code -H "$H" $B/api/containers)
chk "check"        200 $(code -H "$H" $B/api/check)
chk "whitelist"    200 $(code -H "$H" $B/api/whitelist)
chk "usage"        200 $(code -H "$H" $B/api/usage)
chk "pool/status"  200 $(code -H "$H" $B/api/pool/status)
chk "dedicated"    200 $(code -H "$H" $B/api/pool/dedicated)
chk "ded/history"  200 $(code -H "$H" $B/api/pool/dedicated/history)
chk "proxies txt"  200 $(code -H "$H" "$B/api/proxies?num=2")
chk "proxies json" 200 $(code -H "$H" "$B/api/proxies?num=1&type=json&sid=a")
chk "job"          200 $(code -H "$H" $B/api/job/nope)

echo "== 代理池 =="
chk "acquire"      200 $(code -H "$H" "$B/api/pool/acquire?sid=s1")
SID=$(curl -s -H "$H" "$B/api/pool/acquire?sid=s1" | grep -o '"session_id":"[^"]*"' | cut -d'"' -f4)
chk "report"       200 $(code -X POST -H "$H" -H "$J" -d "{\"session_id\":\"s1\",\"result\":403}" $B/api/pool/report)
chk "report code"  00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"session_id\":\"s1\",\"result\":200}" $B/api/pool/report | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
chk "release"      200 $(code -X POST -H "$H" -H "$J" -d '{"session_id":"s1"}' $B/api/pool/release)

echo "== 固定IP（用真实存在的实例）=="
N=$(curl -s -H "$H" $B/api/containers | grep -o '"container":"warp-[0-9]*"' | head -1 | cut -d'"' -f4)
echo "  用 $N"
chk "bind"         00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"name\":\"$N\",\"remark\":\"t\"}" $B/api/pool/dedicated/bind | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
chk "list+1"       1 $(curl -s -H "$H" $B/api/pool/dedicated | grep -c '"name"')
chk "remark"       00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"name\":\"$N\",\"remark\":\"r2\"}" $B/api/pool/dedicated/remark | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
chk "history"      00000 $(curl -s -H "$H" $B/api/pool/dedicated/history | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
chk "unbind"       00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"name\":\"$N\"}" $B/api/pool/dedicated/unbind | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
chk "list-0"       0 $(curl -s -H "$H" $B/api/pool/dedicated | grep -c '"name"')

echo "== 生命周期 =="
IDX=$(curl -s -H "$H" $B/api/containers | grep -o '"instance_index":[0-9]*' | tail -1 | cut -d: -f2)
chk "stop"         00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"index\":$IDX}" $B/api/stop | grep -o '"code":"[^"]*"' | cut -d'"' -f4)
sleep 3
chk "start"        00000 $(curl -s -X POST -H "$H" -H "$J" -d "{\"index\":$IDX}" $B/api/start | grep -o '"code":"[^"]*"' | cut -d'"' -f4)

echo "== 鉴权/页面 =="
chk "no-key 401"   401 $(code $B/api/config)
chk "console"      200 $(code $B/)
chk "docs"         200 $(code $B/docs)

echo "== 数据完整性 =="
chk "实例目录"     100 $(ls /root/warpgate-rs/instances | grep -c warp-)
chk "healthy"      100 $(curl -s -H "$H" $B/api/stats | grep -o '"healthy_count":[0-9]*' | cut -d: -f2)
TOT=$(curl -s -H "$H" $B/api/containers | grep -c '"container"')
echo "  containers=$TOT"
echo "  --- 首条字段 ---"
curl -s -H "$H" $B/api/containers | head -c 620
echo ""

echo ""
echo "===== 结果: PASS=$pass FAIL=$fail ====="
'@

$r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 800 -Command $script 2>$null
Write-Host $r.Output
Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
