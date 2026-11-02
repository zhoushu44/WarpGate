# 强制重编 wg-socks（touch 源码使缓存失效）
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null
$pw = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)
$s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000 2>$null
if ($s) {
  $r = Invoke-SSHCommand -SessionId $s.SessionId -TimeOut 600 -Command 'cd /root/warpgate-rs/app; echo "=== 服务器上的源码还有 DIAG 吗 ==="; grep -rn "DIAG:" crates/ | head -5; echo "(以上为空则源码是新的)"; echo "=== touch 强制重编 ==="; touch crates/wg-socks/src/lib.rs crates/wg-bin/src/main.rs; source $HOME/.cargo/env; cargo build --release 2>&1 | tail -5; echo "=== 二进制里的 DIAG ==="; strings target/release/warpgate | grep -c "DIAG:"' 2>$null
  Write-Host $r.Output
  Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null
}