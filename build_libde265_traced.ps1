# Build libde265 with coefficient tracing enabled
# This creates a version that outputs detailed trace information for comparison

$ErrorActionPreference = "Stop"

Write-Host "Building libde265 with coefficient tracing..." -ForegroundColor Cyan

cd "$PSScriptRoot\libde265"

# Create build directory
$buildDir = "build-traced"
if (-not (Test-Path $buildDir)) {
    Write-Host "Creating $buildDir directory..." -ForegroundColor Yellow
    New-Item -ItemType Directory -Path $buildDir | Out-Null
} else {
    # Check if build directory is locked by another process
    try {
        Remove-Item -Recurse -Force $buildDir -ErrorAction Stop
        Write-Host "Removed existing $buildDir directory..." -ForegroundColor Yellow
        New-Item -ItemType Directory -Path $buildDir | Out-Null
    } catch {
        Write-Host "Existing $buildDir directory is locked, creating new one..." -ForegroundColor Yellow
        $buildDir = "build-traced-$(Get-Date -Format 'yyyyMMdd-HHmmss')"
        New-Item -ItemType Directory -Path $buildDir | Out-Null
    }
}

cd $buildDir

# Check for CMake
if (-not (Get-Command cmake -ErrorAction SilentlyContinue)) {
    Write-Host "ERROR: CMake not found. Install with: winget install Kitware.CMake" -ForegroundColor Red
    exit 1
}

# Configure with CMake
Write-Host "`nConfiguring with CMake..." -ForegroundColor Yellow
cmake .. -G "Visual Studio 17 2022" -A x64 `
    -DCMAKE_CXX_FLAGS="/DTRACE_COEFFICIENTS /DTRACE_CABAC_OPS"

if ($LASTEXITCODE -ne 0) {
    Write-Host "CMake configuration failed. Trying with Ninja..." -ForegroundColor Yellow
    cmake .. -G Ninja -DCMAKE_BUILD_TYPE=Release `
        -DCMAKE_CXX_FLAGS="-DTRACE_COEFFICIENTS -DTRACE_CABAC_OPS"
}

if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: CMake configuration failed" -ForegroundColor Red
    exit 1
}

# Build
Write-Host "`nBuilding..." -ForegroundColor Yellow
cmake --build . --config Release --parallel

if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Build failed" -ForegroundColor Red
    exit 1
}

Write-Host "`n✅ Traced libde265 built successfully!" -ForegroundColor Green
Write-Host "Decoder location: $PSScriptRoot\libde265\build-traced\dec265\Release\dec265.exe" -ForegroundColor Cyan
Write-Host "Trace files will be written to: libde265_coeff_trace.txt, libde265_cabac_ops.txt" -ForegroundColor Cyan
