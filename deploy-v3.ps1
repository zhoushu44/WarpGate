Import-Module Posh-SSH
$pw = ConvertTo-SecureString '6Qz6ao0T1zvL' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

$local = 'E:\360MoveData\Users\Administrator\Desktop\WarpGate'

Get-SFTPSession | Remove-SFTPSession -ErrorAction SilentlyContinue
Get-SSHSession | Remove-SSHSession -ErrorAction SilentlyContinue

$sftp = New-SFTPSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force
# 服务端 public 目录若不存在则创建（通过 SSH）
$pre = New-SSHSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force
Invoke-SSHCommand -SessionId $pre.SessionId -Command "mkdir -p /root/warp/public" -TimeOut 15 | Out-Null
Remove-SSHSession -SessionId $pre.SessionId | Out-Null

Set-SFTPItem -SessionId $sftp.SessionId -Path "$local\server.js" -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path "$local\public\index.html" -Destination '/root/warp/public/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path "$local\public\whitelist.html" -Destination '/root/warp/public/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path "$local\public\docs.html" -Destination '/root/warp/public/' -Force
Remove-SFTPSession -SessionId $sftp.SessionId
Write-Host "UPLOADED"

$session = New-SSHSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force
$check = Invoke-SSHCommand -SessionId $session.SessionId -Command "cd /root/warp && node --check server.js && echo SYNTAX_OK" -TimeOut 30
Write-Host "CHECK: $($check.Output)"
$restart = Invoke-SSHCommand -SessionId $session.SessionId -Command "systemctl restart warp && sleep 8 && systemctl status warp --no-pager | head -5" -TimeOut 60
Write-Host "RESTART: $($restart.Output)"
Remove-SSHSession -SessionId $session.SessionId
