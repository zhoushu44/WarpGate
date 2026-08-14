Import-Module Posh-SSH
$pw = ConvertTo-SecureString 'YOUR_SERVER_PASSWORD' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

Get-SSHSession | Remove-SSHSession -ErrorAction SilentlyContinue

$sftp = New-SFTPSession -ComputerName 'YOUR_SERVER_IP' -Credential $cred -AcceptKey -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\verify-endpoints.sh' -Destination '/tmp/' -Force
Remove-SFTPSession -SessionId $sftp.SessionId

$session = New-SSHSession -ComputerName 'YOUR_SERVER_IP' -Credential $cred -AcceptKey -Force
$result = Invoke-SSHCommand -SessionId $session.SessionId -Command "bash /tmp/verify-endpoints.sh" -TimeOut 240
Remove-SSHSession -SessionId $session.SessionId

Write-Host $result.Output
