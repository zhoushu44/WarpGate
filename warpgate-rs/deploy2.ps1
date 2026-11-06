# Deploy warpgate-rs to the server, replacing the source tree and rebuilding.
#
# Written as a standalone script because the old deploy-upload.ps1 killed the
# running service before uploading, and a partial transfer left the tree in a
# mixed state (Cargo.toml said 0.11.0 while the binary was gone).
#
# Order matters: upload to a staging path, unpack next to the live tree, build,
# and only then swap. A failed upload must never leave the server without a
# working binary.

$ErrorActionPreference = 'Stop'
Import-Module Posh-SSH -WarningAction SilentlyContinue 2>$null

$pw   = ConvertTo-SecureString 'xxlnKUDX2993' -AsPlainText -Force
$cred = New-Object System.Management.Automation.PSCredential('root', $pw)

function Run([string]$cmd, [int]$timeout = 400) {
    $s = $null
    for ($i = 0; $i -lt 5; $i++) {
        try { $s = New-SSHSession -ComputerName '45.205.27.41' -Credential $cred -AcceptKey -Force -ConnectionTimeout 30000; break }
        catch { Start-Sleep 3 }
    }
    if (-not $s) { throw "SSH connect failed" }
    try { return (Invoke-SSHCommand -SessionId $s.SessionId -TimeOut $timeout -Command $cmd) }
    finally { Remove-SSHSession -SessionId $s.SessionId 2>$null | Out-Null }
}

$src = 'C:\Users\zs\Desktop\WarpGate2\warpgate-rs'

# Stage without build output; target/ is 4.6 GB and irrelevant to the server.
$stage = Join-Path $env:TEMP 'wgstage2'
if (Test-Path $stage) { Remove-Item $stage -Recurse -Force }
New-Item -ItemType Directory -Path $stage | Out-Null
robocopy $src $stage /E /XD target .git /NFL /NDL /NJH /NJS /NP | Out-Null
$tar = Join-Path $env:TEMP 'warpgate-src.tar.gz'
if (Test-Path $tar) { Remove-Item $tar -Force }
tar -czf $tar -C $stage .
Write-Host "package: $((Get-Item $tar).Length) bytes"

$b64 = [Convert]::ToBase64String([IO.File]::ReadAllBytes($tar))
$chunk = 40000
$n = [math]::Ceiling($b64.Length / $chunk)
Write-Host "chunks: $n"

# Clean any previous partial upload, then push.
Run "rm -f /root/wg_src.b64; mkdir -p /root/warpgate-rs; echo OK" 60 | Out-Null
for ($i = 0; $i -lt $n; $i++) {
    $off = $i * $chunk
    $len = [Math]::Min($chunk, $b64.Length - $off)
    $part = $b64.Substring($off, $len)
    $op = if ($i -eq 0) { '>' } else { '>>' }
    $res = Run "printf '%s' '$part' $op /root/wg_src.b64; echo CHUNK$i" 120
    if ($res.Output -notmatch "CHUNK$i") { throw "chunk $i failed" }
}

Write-Host "uploaded, verifying size..."
$check = Run "stat -c%s /root/wg_src.b64" 60
Write-Host "remote b64 bytes: $($check.Output)  (expected $($b64.Length))"

# Unpack into a fresh directory and build there. The live tree stays intact
# until the build succeeds.
Write-Host "unpacking and building (this takes a few minutes)..."
$build = Run @'
set -e
rm -rf /root/wg_new
mkdir -p /root/wg_new
base64 -d /root/wg_src.b64 > /root/wg_src.tar.gz
tar -xzf /root/wg_src.tar.gz -C /root/wg_new
echo "--- staged version ---"
grep -m1 '^version' /root/wg_new/Cargo.toml
echo "--- health_loop present? ---"
grep -c "async fn health_loop" /root/wg_new/crates/wg-pool/src/lib.rs || echo 0
source $HOME/.cargo/env
cd /root/wg_new
cargo build --release 2>&1 | tail -5
ls -l target/release/warpgate
'@ 1500
Write-Host $build.Output
Write-Host "exit: $($build.ExitStatus)"
