# 切换：停老系统（node + sing-box），起新系统 200 实例
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

function Invoke-OneCommand {
    param([string]$Command, [int]$TimeOut = 600)
    $s = $null
    for ($i = 0; $i -lt 5; $i++) {
        try { $s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000; break }
        catch { Start-Sleep 3 }
    }
    if (-not $s) { return $null }
    try { return (Invoke-SSHCommand -SessionId $s.SessionId -TimeOut $TimeOut -Command $Command) }
    finally { Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null }
}

Write-Host "=== 1. 关闭老系统 ==="
$r = Invoke-OneCommand @'
pkill -f "node server.js" 2>/dev/null
pkill -f "sing-box run" 2>/dev/null
sleep 3
echo -n "node 残留: "; ps aux | grep "[n]ode server.js" | wc -l
echo -n "sing-box 残留: "; ps aux | grep "[s]ing-box run" | wc -l
echo -n "usque 残留: "; ps aux | grep "[u]sque socks" | wc -l
echo "--- 当前内存 ---"
free -m | sed -n 2p
'@ 120
$r.Output

Write-Host "=== 2. 准备 200 个实例目录 ==="
$r2 = Invoke-OneCommand @'
# 新系统用独立目录，不动老系统的 warp-instances
rm -rf /root/warpgate-rs/instances
mkdir -p /root/warpgate-rs/instances
n=0
for d in /root/warp-instances/warp-*; do
  [ $n -ge 200 ] && break
  name=$(basename $d)
  bak="$d/usque.json.bak-20261001"
  if [ -f "$bak" ]; then
    mkdir -p /root/warpgate-rs/instances/$name
    cp "$bak" /root/warpgate-rs/instances/$name/usque.json
    cp $d/meta.json /root/warpgate-rs/instances/$name/ 2>/dev/null
    n=$((n+1))
  elif [ -f "$d/usque.json" ]; then
    mkdir -p /root/warpgate-rs/instances/$name
    cp $d/usque.json /root/warpgate-rs/instances/$name/
    cp $d/meta.json /root/warpgate-rs/instances/$name/ 2>/dev/null
    n=$((n+1))
  fi
done
echo "准备了 $n 个实例"
ls /root/warpgate-rs/instances | wc -l
'@ 300
$r2.Output
