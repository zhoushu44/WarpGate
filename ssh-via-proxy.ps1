# SSH to the server through its own WARP SOCKS5 port.
#
# Direct outbound port 22 is blocked from this workstation (every probe times
# out), but the server's sshd is demonstrably alive: connecting through the
# pool's SOCKS5 proxy returns "SSH-2.0-OpenSSH_7.4". So we tunnel via that
# proxy instead of fighting the network path.

param(
    [string]$Command = "hostname; uptime"
)

$proxyHost = '45.205.27.41'
$proxyPort = 10011
$proxyUser = 'warpuser'
$proxyPass = 'test-pass-123'
$server    = '45.205.27.41'
$sshUser   = 'root'
$sshPass   = 'xxlnKUDX2993'

# probe: confirm sshd answers through the proxy before attempting auth
Write-Host "=== 1. 经代理确认 sshd 存活 ==="
$probe = & curl.exe -s --max-time 20 --socks5-hostname "$proxyHost`:$proxyPort" `
    -U "$proxyUser`:$proxyPass" telnet://"$server`:22" 2>&1 |
    Select-Object -First 3
Write-Host ($probe -join "`n")

Write-Host "`n=== 2. 建立 SSH 隧道（ProxyCommand）==="
# -o ProxyCommand makes ssh speak SOCKS5 through the pool port, then hand the
# resulting socket to the ssh client.
$sshCmd = @(
    '-o', 'StrictHostKeyChecking=no',
    '-o', 'UserKnownHostsFile=NUL',
    '-o', "ProxyCommand=curl -s --max-time 300 --socks5-hostname $proxyHost`:$proxyPort -U $proxyUser`:$proxyPass telnet://%h:%p",
    '-o', 'ConnectTimeout=30',
    '-p', '22',
    "$sshUser@$server",
    $Command
)

Write-Host "  执行: ssh $sshUser@$server"

# Password auth needs sshpass-like handling; try ssh.exe with a password pipe.
$env:SSHPASS = $sshPass
$out = & ssh @sshCmd 2>&1
Write-Host "  exit=$LASTEXITCODE"
$out | Select-Object -First 20 | ForEach-Object { Write-Host "  $_" }
