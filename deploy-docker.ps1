# WARP 代理池 Docker 化部署脚本
# 1. 上传构建文件到服务器 /root/warp/
# 2. 停用旧 systemd warp.service（保留文件可回滚）
# 3. 备份实例目录后 docker compose 重建
# 4. 轮询验证 20 实例全部 healthy
Import-Module Posh-SSH
$pw = ConvertTo-SecureString '6Qz6ao0T1zvL' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

Get-SFTPSession | Remove-SFTPSession -ErrorAction SilentlyContinue
Get-SSHSession | Remove-SSHSession -ErrorAction SilentlyContinue

Write-Host "===== 1. 上传构建文件 ====="
$sftp = New-SFTPSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\Dockerfile' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\docker-compose.yml' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\.dockerignore' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\server.js' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\logger.js' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\package.json' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\package-lock.json' -Destination '/root/warp/' -Force
Set-SFTPItem -SessionId $sftp.SessionId -Path 'c:\Users\zs\Desktop\warp\public\index.html' -Destination '/root/warp/public/' -Force
Remove-SFTPSession -SessionId $sftp.SessionId
Write-Host "UPLOADED"

$session = New-SSHSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force

Write-Host "===== 2. 语法检查 ====="
$check = Invoke-SSHCommand -SessionId $session.SessionId -Command "cd /root/warp && node --check server.js && echo SYNTAX_OK" -TimeOut 30
Write-Host $check.Output

Write-Host "===== 3. 停用旧 systemd 服务 ====="
$stop = Invoke-SSHCommand -SessionId $session.SessionId -Command "systemctl stop warp && systemctl disable warp 2>&1 | tail -1; echo STOPPED" -TimeOut 60
Write-Host $stop.Output

Write-Host "===== 4. 备份实例目录 + 构建启动容器 ====="
$build = Invoke-SSHCommand -SessionId $session.SessionId -Command @'
set -e
cd /root/warp
if [ -d /root/warp-instances ] && [ ! -L /root/warp-instances ]; then
  TS=$(date +%Y%m%d%H%M%S)
  mv /root/warp-instances /root/warp-instances.bak.$TS
  echo "实例目录已备份: /root/warp-instances.bak.$TS"
fi
docker compose up -d --build 2>&1 | tail -20
echo BUILD_DONE
'@ -TimeOut 600
Write-Host $build.Output

Remove-SSHSession -SessionId $session.SessionId

Write-Host "===== 5. 轮询验证（最多等待 8 分钟）====="
$session2 = New-SSHSession -ComputerName '192.6.121.16' -Credential $cred -AcceptKey -Force
for ($i = 1; $i -le 32; $i++) {
  Start-Sleep -Seconds 15
  $stats = Invoke-SSHCommand -SessionId $session2.SessionId -Command "curl -s --max-time 5 http://127.0.0.1:4433/api/stats | grep -oP '\"total_instances\":\d+|\"available\":\d+|\"unhealthy\":\d+|\"unique_ips\":\d+'" -TimeOut 20
  $out = ($stats.Output -join ' ')
  Write-Host "  [$i] $out"
  if ($out -match '"total_instances":20' -and $out -match '"unhealthy":0' -and $out -match '"unique_ips":20') {
    Write-Host "=== 20/20 实例全部就绪 ==="
    break
  }
}
Remove-SSHSession -SessionId $session2.SessionId
