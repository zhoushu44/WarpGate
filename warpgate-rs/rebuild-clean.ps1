# 清掉服务器 target 后重新编译（硬链接导致二进制未更新）
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

$r = Invoke-OneCommand @'
cd /root/warpgate-rs/app
echo "=== 旧二进制 ==="
ls -l target/release/warpgate
strings target/release/warpgate | grep -c "DIAG:" || true
echo "=== 删除并重编 ==="
rm -rf target
source $HOME/.cargo/env
cargo build --release 2>&1 | tail -3
echo "=== 新二进制 ==="
ls -l target/release/warpgate
strings target/release/warpgate | grep -c "DIAG:" || echo "0 (clean)"
'@ 900
$r.Output
