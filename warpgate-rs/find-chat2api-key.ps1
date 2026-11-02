# 找 chat2api 的 API key 和路由
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if ($s) {
  $r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 200 -Command 'echo "=== chat2api 容器（7863） ==="; docker ps --filter "publish=7863" --format "{{.Names}} {{.Image}}"; echo "=== 环境变量 ==="; CID=$(docker ps -q --filter "publish=7863" | head -1); docker exec $CID sh -c "env | grep -iE \"key|token|auth|api|admin\" " 2>/dev/null | head -20; echo "=== 路由 ==="; docker exec $CID sh -c "strings /app/* 2>/dev/null | grep -E \"^/(v1|api|admin)\" | sort -u | head -40" 2>/dev/null; echo "=== /v1/models 带空 key ==="; curl -s --max-time 5 http://127.0.0.1:7863/v1/models | head -c 300' 2>$null
  Write-Host $r.Output
  Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
}