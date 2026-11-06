# Stop surplus instances through the API.
#
# v0.12.0's /api/config answers saved:true and changes nothing, so the only way
# to shed load on this build is POST /api/stop per index. Stop from the highest
# index down: that keeps the low, long-lived instances and their IPs stable.

param(
    [int]$Keep = 100,
    [int]$Rate = 12          # requests per second
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

$stats = Api '/api/stats' | ConvertFrom-Json
$total = [int]$stats.data.instance_count
Write-Host "当前实例数: $total, 目标保留: $Keep"

if ($total -le $Keep) { Write-Host "无需缩容"; exit 0 }

# Collect indexes, newest first.
$containers = (Api '/api/containers' | ConvertFrom-Json).data
$idxs = $containers | ForEach-Object { [int]$_.index } | Sort-Object -Descending
Write-Host "待停止: $($idxs.Count) 个候选"

$stopped = 0
$delay = [int](1000 / [Math]::Max($Rate, 1))

foreach ($idx in $idxs) {
    if (($total - $stopped) -le $Keep) { break }
    $r = Api '/api/stop' 'POST' ("{`"index`":$idx}")
    if ($r -and $r -match '"code":"00000"') {
        $stopped++
        if ($stopped % 25 -eq 0) {
            Write-Host "  已停 $stopped 个（剩余 $($total - $stopped)）"
        }
    }
    Start-Sleep -Milliseconds $delay
}

Write-Host "`n完成，停了 $stopped 个"
Start-Sleep -Seconds 5
$after = (Api '/api/stats' | ConvertFrom-Json).data
Write-Host "现在: count=$($after.instance_count) healthy=$($after.healthy_count) resident=$($after.resident_count)"
