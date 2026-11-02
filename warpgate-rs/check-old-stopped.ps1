# 确认老系统已停 + 起新系统 100 实例 + 计时
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if ($s) {
  $r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 200 -Command 'echo "=== 老系统状态 ==="; echo -n "node: "; ps aux | grep "[n]ode server.js" | wc -l; echo -n "sing-box: "; ps aux | grep "[s]ing-box run" | wc -l; echo "=== 内存（老系统已停） ==="; free -m | sed -n 2p; echo "=== 新系统探针 ==="; ls /root/warpgate-rs/instances | wc -l' 2>$null
  Write-Host $r.Output
  Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
}