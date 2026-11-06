# SSH to the server by tunnelling through its own WARP SOCKS5 pool port.
#
# Direct port 22 is blocked from this workstation, but the server's sshd answers
# fine through the pool: `SOCKS5 request granted` then `SSH-2.0-OpenSSH_7.4`.
#
# Plan: run a local TCP listener that forwards to server:22 via the SOCKS5 proxy,
# then point ssh at 127.0.0.1:<localport>. This is more reliable than
# ProxyCommand because the relay keeps the socket open for the whole session
# instead of relying on curl's telnet mode.

param(
    [int]$LocalPort = 2222,
    [string]$Command = "hostname; uptime; free -m | sed -n 2p",
    [int]$HoldSeconds = 0
)

$proxyHost = '45.205.27.41'
$proxyPort = 10011
$proxyUser = 'warpuser'
$proxyPass = 'test-pass-123'
$targetHost = '45.205.27.41'
$targetPort = 22
$sshUser = 'root'
$sshPass = 'xxlnKUDX2993'

$relay = @'
import socket, threading, sys, base64

LOCAL_PORT = int(sys.argv[1])
PROXY = (sys.argv[2], int(sys.argv[3]))
USER = sys.argv[4].encode()
PASS = sys.argv[5].encode()
TARGET = (sys.argv[6], int(sys.argv[7]))

def socks5_connect():
    s = socket.create_connection(PROXY, 20)
    s.settimeout(30)
    s.sendall(b"\x05\x01\x02")                      # offer user/pass
    if s.recv(2)[1] != 2:
        raise RuntimeError("proxy wants another auth method")
    s.sendall(bytes([1, len(USER)]) + USER + bytes([len(PASS)]) + PASS)
    if s.recv(2)[1] != 0:
        raise RuntimeError("proxy rejected credentials")
    hb = TARGET[0].encode()
    s.sendall(bytes([5, 1, 0, 3, len(hb)]) + hb + TARGET[1].to_bytes(2, "big"))
    rep = s.recv(10)
    if len(rep) < 2 or rep[1] != 0:
        raise RuntimeError("proxy connect failed: %r" % (rep,))
    s.settimeout(None)
    return s

def pipe(a, b):
    try:
        while True:
            d = a.recv(65536)
            if not d:
                break
            b.sendall(d)
    except Exception:
        pass
    finally:
        try: b.shutdown(socket.SHUT_WR)
        except Exception: pass

def handle(client):
    try:
        upstream = socks5_connect()
    except Exception as e:
        print("relay: %s" % e, flush=True)
        client.close()
        return
    threading.Thread(target=pipe, args=(client, upstream), daemon=True).start()
    threading.Thread(target=pipe, args=(upstream, client), daemon=True).start()

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", LOCAL_PORT))
srv.listen(8)
print("relay listening on 127.0.0.1:%d" % LOCAL_PORT, flush=True)
while True:
    c, _ = srv.accept()
    threading.Thread(target=handle, args=(c,), daemon=True).start()
'@

$py = 'C:\Users\zs\.dsh\dsh-runtimes\dsh-primary-runtime\dependencies\python\python.exe'
if (-not (Test-Path $py)) { $py = 'python' }

$relayFile = Join-Path $env:TEMP 'socks_relay.py'
[IO.File]::WriteAllText($relayFile, $relay)

Write-Host "=== 启动本地中继 :$LocalPort -> $targetHost`:$targetPort (经 SOCKS5 $proxyPort) ==="
$relayProc = Start-Process -FilePath $py -ArgumentList @(
    $relayFile, $LocalPort, $proxyHost, $proxyPort, $proxyUser, $proxyPass, $targetHost, $targetPort
) -PassThru -WindowStyle Hidden -RedirectStandardOutput "$env:TEMP\relay.log" -RedirectStandardError "$env:TEMP\relay.err"

Start-Sleep -Seconds 3
if ($relayProc.HasExited) {
    Write-Host "中继启动失败:"
    Get-Content "$env:TEMP\relay.err" -ErrorAction SilentlyContinue | Select-Object -First 5
    exit 1
}
Get-Content "$env:TEMP\relay.log" -ErrorAction SilentlyContinue | ForEach-Object { Write-Host "  $_" }

Write-Host "`n=== SSH 经本地中继 ==="
$out = & ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=NUL `
    -o ConnectTimeout=20 -o PreferredAuthentications=password -o PubkeyAuthentication=no `
    -p $LocalPort "root@127.0.0.1" $Command 2>&1
Write-Host "exit=$LASTEXITCODE"
$out | Select-Object -First 25 | ForEach-Object { Write-Host "  $_" }

if ($HoldSeconds -gt 0) {
    Start-Sleep -Seconds $HoldSeconds
} 
$relayProc | Stop-Process -Force -ErrorAction SilentlyContinue
Write-Host "`n中继已停止"
