# 重启 warpgate 并验证 public_ip + 重跑注册
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if ($s) {
  # 强制重编 wg-pool（避免 cargo 缓存）
  $r0 = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 400 -Command 'cd /root/warpgate-rs/app; touch crates/wg-pool/src/lib.rs crates/wg-bin/src/main.rs; source $HOME/.cargo/env; cargo build --release 2>&1 | tail -2' 2>$null
  Write-Host "重建: $($r0.Output)"
  # 重启
  $r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 400 -Command 'pkill -f "target/release/warpgate"; sleep 2; rm -f /root/warpgate-rs/warpgate.log; /root/warpgate-rs/start.sh & disown; sleep 35; echo "=== API ==="; curl -s --max-time 5 -H "X-API-Key: zs1236547" http://127.0.0.1:14433/api/stats; echo ""; echo "=== /api/proxies ==="; curl -s --max-time 8 -H "X-API-Key: zs1236547" "http://127.0.0.1:14433/api/proxies?num=1&type=txt&format=n&sid=chatgpt-test&time=20"' 2>$null
  Write-Host $r.Output
  Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
}