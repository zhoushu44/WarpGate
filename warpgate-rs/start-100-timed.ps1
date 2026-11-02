# 起新系统 100 实例 + 计时 + 测内存
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

function Invoke-OneCommand {
    param([string]$Command, [int]$TimeOut = 900)
    $s = $null
    for ($i = 0; $i -lt 5; $i++) {
        try { $s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000; break }
        catch { Start-Sleep 3 }
    }
    if (-not $s) { return $null }
    try { return (Invoke-SSHCommand -SessionId $s.SessionId -TimeOut $TimeOut -Command $Command) }
    finally { Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null }
}

# config: 100 实例
$c = Get-Content "C:\Users\zs\Desktop\WarpGate2\config.json" -Raw | ConvertFrom-Json
$c.port = 14433
$c.resident_count = 100
$c.max_instances = 300
$c.base_port = 20010
$c.log_level = "info"
[IO.File]::WriteAllText("$env:TEMP\rust-config.json", ($c | ConvertTo-Json -Depth 10))
$b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes("$env:TEMP\rust-config.json"))
Invoke-OneCommand "printf '%s' '$b64' | base64 -d > /root/warpgate-rs/rust-config.json && wc -c /root/warpgate-rs/rust-config.json" 120 | Select-Object -ExpandProperty Output

Write-Host "=== 启动（计时） ==="
$r = Invoke-OneCommand @'
pkill -f 'target/release/warpgate' 2>/dev/null
sleep 2
cd /root/warpgate-rs/app
START=$(date +%s.%N)
setsid nohup ./target/release/warpgate --config /root/warpgate-rs/rust-config.json --instances /root/warpgate-rs/instances > /root/warpgate-rs/warpgate.log 2>&1 < /dev/null &
# 等到 healthy_count 达到 100 或超时 300s
for i in $(seq 1 300); do
  N=$(curl -s --max-time 2 -H "X-API-Key: zs1236547" http://127.0.0.1:14433/api/stats 2>/dev/null | grep -o '"healthy_count":[0-9]*' | cut -d: -f2)
  [ -z "$N" ] && N=0
  if [ "$N" -ge 100 ]; then
    END=$(date +%s.%N)
    echo "READY_AT=$N"
    echo "ELAPSED=$(echo "$END - $START" | bc)"
    break
  fi
  sleep 1
done
echo "--- 日志 ---"
tail -6 /root/warpgate-rs/warpgate.log
'@ 900
$r.Output
