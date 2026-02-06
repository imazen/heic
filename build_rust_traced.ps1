# Build Rust HEIC decoder with coefficient tracing enabled

$ErrorActionPreference = "Stop"

Write-Host "Building Rust HEIC decoder with tracing..." -ForegroundColor Cyan

cd $PSScriptRoot

# Check if Cargo.toml has been fixed
$cargoContent = Get-Content "Cargo.toml" -Raw
if ($cargoContent -match 'edition = "2024"') {
    Write-Host "ERROR: Cargo.toml still has edition 2024, should be 2021" -ForegroundColor Red
    Write-Host "Please run the fix first or manually edit Cargo.toml" -ForegroundColor Yellow
    exit 1
}

# Build with tracing feature
Write-Host "`nBuilding with trace-coefficients feature..." -ForegroundColor Yellow
cargo build --release --features trace-coefficients

if ($LASTEXITCODE -ne 0) {
    Write-Host "ERROR: Build failed" -ForegroundColor Red
    Write-Host "Common issues:" -ForegroundColor Yellow
    Write-Host "  1. Cargo.toml edition should be 2021, not 2024" -ForegroundColor Yellow
    Write-Host "  2. heic-wasm-rs dependency should be commented out" -ForegroundColor Yellow
    Write-Host "  3. Rust version should be 1.92+ (check with: rustup show)" -ForegroundColor Yellow
    exit 1
}

Write-Host "`n✅ Rust decoder built successfully!" -ForegroundColor Green
Write-Host "Binary location: $PSScriptRoot\target\release\heic-decoder.exe" -ForegroundColor Cyan
Write-Host "Trace file will be written to: rust_coeff_trace.txt" -ForegroundColor Cyan
