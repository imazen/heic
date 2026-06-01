<#
.SYNOPSIS
  Run the Windows-native HEVC backend tests (Media Foundation + D3D11VA) on the
  Windows HOST, against a WSL checkout, using the Windows-native Rust toolchain
  and the host GPU / HEVC codec.

.DESCRIPTION
  The MediaFoundation and D3D11VA backends are `#[cfg(target_os = "windows")]`
  and need a real Windows GPU + the HEVC Video Extensions — neither of which
  exists inside WSL (no /dev/dri; VA-API is dead there). So when you develop in
  WSL, `cargo test` cannot exercise them. This script bridges that: it is
  invoked from the WSL side (see the `windows_backends_via_host` test in
  tests/backend_dispatch.rs and the `test-win` justfile recipe) and runs the
  backend_dispatch integration test natively on Windows against the same source
  tree (reached over the `\\wsl.localhost\<distro>\...` UNC path).

  Build artifacts go to a Windows-local target dir (the \\wsl$ 9p mount is slow
  to build on), so the Linux-side `target/` is untouched.

.PARAMETER CheckoutUnc
  The Windows UNC path to the WSL checkout, e.g.
  `\\wsl.localhost\Ubuntu-22.04\home\lilith\work\zen\heic`. Required.

.PARAMETER TargetDir
  Windows CARGO_TARGET_DIR for the build artifacts. Defaults to
  `$env:TEMP\heic-win-target`.

.PARAMETER RequireMfHevc
  '1' (default) makes the MF test require the HEVC codec; set '0' on a host
  without the HEVC Video Extensions so the MF decode test self-skips.
#>
param(
    [Parameter(Mandatory = $true)]
    [string]$CheckoutUnc,
    [string]$TargetDir = "$env:TEMP\heic-win-target",
    [string]$RequireMfHevc = '1'
)

$ErrorActionPreference = 'Stop'

if (-not (Test-Path -LiteralPath $CheckoutUnc)) {
    Write-Error "Checkout path not reachable from Windows: $CheckoutUnc"
    exit 2
}
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    Write-Error "No Windows-native 'cargo' on PATH. Install rustup on Windows (x86_64-pc-windows-msvc) to run the native-backend tests."
    exit 3
}

$env:CARGO_TARGET_DIR = $TargetDir
$env:HEIC_REQUIRE_MF_HEVC = $RequireMfHevc
$env:HEIC_D3D11VA_HW = '1'

Write-Host "== heic Windows-host backend tests =="
Write-Host "   checkout : $CheckoutUnc"
Write-Host "   target   : $TargetDir"
Write-Host "   MF HEVC  : require=$RequireMfHevc   D3D11VA HW: 1"

Set-Location -LiteralPath $CheckoutUnc

# MediaFoundation + D3D11VA in one run. Both are target_os=windows and compose.
cargo test --test backend_dispatch `
    --features 'backend-rust,backend-mediafoundation,backend-d3d11va,std' `
    --target x86_64-pc-windows-msvc

exit $LASTEXITCODE
