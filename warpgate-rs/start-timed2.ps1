# 起 100 实例 + 精确计时（避免 bc，用 date +%s%3N）
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if ($s) {
  $r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 900 -Command 'pkill -f "target/release/warpgate" 2>/dev/null; sleep 2; cd /root/warpgate-rs/app; S=$(date +%s%3N); setsid nohup ./target/release/warpgate --config /root/warpgate-rs/rust-config.json --instances /root/warpgate-rs/instances > /root/warpgate-rs/warpgate.log 2>&1 < /dev/null & for i in $(seq 1 600); do N=$(curl -s --max-time 2 -H "X-API-Key: zs1236547" http://127.0.0.1:14433/api/stats 2>/dev/null | grep -o "\"healthy_count\":[0-9]*" | cut -d: -f2); [ -z "$N" ] && N=0; if [ "$N" -ge 100 ]; then E=$(date +%s%3N); echo "READY=$N ELAPSED_MS=$((E-S))"; break; fi; sleep 1; done; echo "--- log ---"; tail -5 /root/warpgate-rs/warpgate.log' 2>$null
  Write-Host $r.Output
  Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
}