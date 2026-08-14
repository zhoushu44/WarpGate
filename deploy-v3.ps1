Import-Module Posh-SSH
$pw = ConvertTo-SecureString 'YOUR_SERVER_PASSWORD' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

Get-SFTPSession | Remove-SFTPSession -ErrorAction SilentlyContinue
Get-SSHSession | Remove-SSHSession -ErrorAction SilentlyContinue

$sftp = New-SFTPSession -ComputerName 'YOUR_SERVER_IP' -Credential $cred -AcceptKey -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\server.js' -Destination '/root/warp/' -Force
Remove-SFTPSession -SessionId $sftp.SessionId
Write-Host "UPLOADED"

$session = New-SSHSession -ComputerName 'YOUR_SERVER_IP' -Credential $cred -AcceptKey -Force
$check = Invoke-SSHCommand -SessionId $session.SessionId -Command "cd /root/warp && node --check server.js && echo SYNTAX_OK" -TimeOut 30
Write-Host "CHECK: $($check.Output)"
$restart = Invoke-SSHCommand -SessionId $session.SessionId -Command "systemctl restart warp && sleep 8 && systemctl status warp --no-pager | head -5" -TimeOut 60
Write-Host "RESTART: $($restart.Output)"
Remove-SSHSession -SessionId $session.SessionId
