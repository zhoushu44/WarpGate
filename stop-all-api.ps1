# Stop every instance through the API (no SSH needed).
#
# v0.12.0's /api/config answers saved:true but changes nothing, so the only
# lever available on this build is POST /api/stop per index. Stop from the
# highest index down so the low-index instances survive longest and a later
# grow reuses them.

param(
    [int]$Keep = 0,
    [int]$Rate = 15
)

$base = 'http://45.205.27.41:4433'
$hdr  = @{ 'X-API-Key' = 'zs1236547' }

function Api([string]$path, [string]$method = 'GET', [string]$body = $null) {
    for ($i = 0; $i -lt 3; $i++) {
        try {
            $p = @{ Uri = "$base$path"; Method = $method; Headers = $hdr; TimeoutSec = 25; UseBasicParsing = $true }
            if ($body) { $p.Body = $body; $p.ContentType = 'application/json' }
            return (Invoke-WebRequest @p -ErrorAction Stop).Content
        } catch { Start-Sleep -Milliseconds 400 }
    }
    return $null
}

$stats = Api '/api/stats'
if (-not $stats) { Write-Host "API 无响应，停止"; exit 1 }
$total = [int](($stats | ConvertFrom-Json).data.instance_count)
Write-Host "当前实例: $total, 保留: $Keep"

if ($total -le $Keep) { Write-Host "无需操作"; exit 0 }

$containers = (Api '/api/containers' | ConvertFrom-Json).data
$idxs = $containers | ForEach-Object { [int]$_.index } | Sort-Object -Descending
Write-Host "候选: $($idxs.Count)"

$stopped = 0
$delay = [int](1000 / [Math]::Max($Rate, 1))

foreach ($idx in $idxs) {
    if (($total - $stopped) -le $Keep) { break }
    $r = Api '/api/stop' 'POST' ("{`"index`":$idx}")
    if ($r -and $r -match '"code":"00000"') {
        $stopped++
        if ($stopped % 20 -eq 0) { Write-Host "  已停 $stopped" }
    }
    Start-Sleep -Milliseconds $delay
}

Write-Host "`n停了 $stopped 个"
Start-Sleep -Seconds 4
$after = (Api '/api/stats' | ConvertFrom-Json).data
Write-Host "现在: count=$($after.instance_count) healthy=$($after.healthy_count) resident=$($after.resident_count)"
