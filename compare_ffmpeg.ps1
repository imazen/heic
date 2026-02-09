# Simple FFmpeg vs Rust decoder comparison

Write-Host "`n=== HEIC Decoder Performance: Rust vs FFmpeg ===`n" -ForegroundColor Cyan

$testFile = "libheif/examples/example.heic"
$iterations = 10

# Test Rust decoder
Write-Host "Testing Rust decoder ($iterations iterations)..." -ForegroundColor Yellow
$rustTimes = @()
for ($i = 1; $i -le $iterations; $i++) {
    $elapsed = Measure-Command {
        .\target\release\decode_heic.exe $testFile 2>&1 | Out-Null
    }
    $rustTimes += $elapsed.TotalSeconds
    Write-Host "  Iteration $i : $($elapsed.TotalSeconds.ToString('F3'))s"
}
$rustAvg = ($rustTimes | Measure-Object -Average).Average
$rustMin = ($rustTimes | Measure-Object -Minimum).Minimum
$rustMax = ($rustTimes | Measure-Object -Maximum).Maximum

Write-Host "`nRust Results:" -ForegroundColor Green
Write-Host "  Average: $($rustAvg.ToString('F3'))s"
Write-Host "  Min:     $($rustMin.ToString('F3'))s"
Write-Host "  Max:     $($rustMax.ToString('F3'))s"

# Test FFmpeg (if available)
Write-Host "`nTesting FFmpeg ($iterations iterations)..." -ForegroundColor Yellow
$ffmpegAvailable = Get-Command ffmpeg -ErrorAction SilentlyContinue
if ($ffmpegAvailable) {
    $ffmpegTimes = @()
    for ($i = 1; $i -le $iterations; $i++) {
        $elapsed = Measure-Command {
            ffmpeg -i $testFile -f null - 2>&1 | Out-Null
        }
        $ffmpegTimes += $elapsed.TotalSeconds
        Write-Host "  Iteration $i : $($elapsed.TotalSeconds.ToString('F3'))s"
    }
    $ffmpegAvg = ($ffmpegTimes | Measure-Object -Average).Average
    $ffmpegMin = ($ffmpegTimes | Measure-Object -Minimum).Minimum
    $ffmpegMax = ($ffmpegTimes | Measure-Object -Maximum).Maximum

    Write-Host "`nFFmpeg Results:" -ForegroundColor Green
    Write-Host "  Average: $($ffmpegAvg.ToString('F3'))s"
    Write-Host "  Min:     $($ffmpegMin.ToString('F3'))s"
    Write-Host "  Max:     $($ffmpegMax.ToString('F3'))s"

    # Comparison
    Write-Host "`n=== COMPARISON ===" -ForegroundColor Cyan
    $diff = $rustAvg - $ffmpegAvg
    $pctDiff = ($diff / $ffmpegAvg) * 100

    Write-Host "Rust avg:   $($rustAvg.ToString('F3'))s"
    Write-Host "FFmpeg avg: $($ffmpegAvg.ToString('F3'))s"
    Write-Host "Difference: $($diff.ToString('F3'))s ($($pctDiff.ToString('F1'))%)"

    if ($pctDiff -lt 10) {
        Write-Host "`n✅ Excellent! Rust is competitive with FFmpeg" -ForegroundColor Green
    } elseif ($pctDiff -lt 30) {
        Write-Host "`n⚠️  Good, but room for optimization" -ForegroundColor Yellow
    } else {
        Write-Host "`n❌ Rust is significantly slower - needs optimization" -ForegroundColor Red
    }
} else {
    Write-Host "FFmpeg not found in PATH" -ForegroundColor Red
    Write-Host "Install FFmpeg to compare: https://ffmpeg.org/download.html"
}

Write-Host ""
