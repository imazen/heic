# CI & runtime-test strategy

How `heic` is tested, what runs where, and the plan for the runtime-decode gaps
on the native backends. Written 2026-05-31 alongside the deep safety review.

## Principle

`heic` decodes **untrusted input**, and the parent + `heic-core` are
`#![forbid(unsafe_code)]`. The whole memory-safety surface is the five native
FFI backend crates. So CI has two jobs:

1. **Catch panics / wrong pixels / OOB on real input** in the pure-Rust path —
   cheap, fully hosted, must be exhaustive.
2. **Actually execute the native FFI** — needs the target OS, and for VA-API /
   D3D11VA a real GPU. This is where the gaps are.

## Per-backend runtime coverage

| Backend | Compile / clippy | Runtime decode in CI | Where |
|---|---|---|---|
| `backend-rust` | all 6 OSes + i686 + wasm32 | ✅ corpus gate on all 6 OSes + i686 (32-bit) | `ci.yml` |
| MediaFoundation | windows-latest (x64+arm64) | 🏠 **Windows host** via `just test-win` (+ `windows-11-arm` CI when the codec is installed) | `scripts/win-test.ps1`, `ci.yml` |
| VideoToolbox | macOS arm64 + Intel | ✅ `macos-latest` + `macos-15-intel` | `ci.yml` |
| MediaCodec | aarch64-android (NDK) | ✅ **Android emulator** (x86_64, software HEVC) | `mediacodec-runtime.yml` |
| VA-API | ubuntu (libva-dev) | ⏳ real Linux+GPU only (no WSL — no `/dev/dri`) | `vaapi-runtime.yml` |
| D3D11VA | windows (x64+arm64) | 🏠 **Windows host** via `just test-win` | `scripts/win-test.ps1`, `d3d11va-runtime.yml` |

`✅` = runs on every push (hosted CI). `🏠` = runs on the local Windows host
from WSL via PowerShell (`just test-win`; $0, no extra hardware). `⏳` = needs a
real Linux+GPU box (any vendor).

## Tier 1 — hosted, free, already wired (`ci.yml`, `fuzz.yml`)

- **Corpus decode gate** (`tests/corpus_decode.rs`): decodes all 95 committed
  `testdata/` files (incl. 10-bit Apple HDR, grids, uncompressed HEIF) on every
  OS; loud-fails if `testdata/` is missing (no graceful skip). The main matrix
  no longer runs only `cargo test --lib`.
- **i686 + 32-bit decode correctness**: the `cross` job runs the corpus gate, not
  just `--lib` — exercises the 32-bit overflow/wrap fixes (NAL `checked_add`,
  `stsc` clamp, zencodec `checked_mul`).
- **Overflow-checked job**: corpus + every fuzz-regression seed under
  `-C overflow-checks=on` (the HEVC param-set overflow bugs only panic with
  checks on).
- **All 7 fuzz targets** run nightly (was 5).
- **`cargo-semver-checks`** (informational on 0.x), **`cargo doc -D warnings`**,
  and **`av1` + `unci`** lint/unit gating — all were missing.
- **MF runtime AppX step** now fails loud when `HEVC_APPX_URL_ARM64` is set but
  install fails (was `continue-on-error` → the gate could silently no-op).

### MediaFoundation runtime — codec licensing

The MF HEVC decode test on `windows-11-arm` needs the "HEVC Video Extensions"
codec, which the GitHub-hosted image doesn't reliably ship. **It cannot be
side-loaded by mirroring the `.appx`:** that package (Store product
`9N4WGH0Z6VHQ`, the free "from Device Manufacturer" variant) is a Microsoft
Store "Digital Good", and the Microsoft Services Agreement §14.j prohibits
redistributing/transferring copies — being free + Microsoft-signed does **not**
grant redistribution (verified against the MSA + the standard software license
terms). So the decode test **skips** by default (`HEIC_REQUIRE_MF_HEVC=0`). To
run it for real, acquire the codec license-cleanly:

- `winget install --id 9N4WGH0Z6VHQ --source msstore` on the runner (keeps
  acquisition inside the Store license; may require Store auth — test it works
  unattended on the hosted image), **or**
- a Microsoft Store for Business / Intune **offline license** you own, hosted at
  `HEVC_APPX_URL_ARM64`, **or**
- run the MF dispatch test on a licensed Windows host (e.g. `V:\heic-win-test.ps1`).

Do **not** point `HEVC_APPX_URL_ARM64` at a public mirror of the stock Store
`.appx` — it's a licensing risk, and the Store CDN
(`delivery.mp.microsoft.com`) URLs are short-lived and would rot anyway.

### Conformance suite — still a gap

The 49 ITU-T HEVC conformance vectors are **not committed** (only
`conformance/fetch.sh`), and the test hardcodes a local `dec265` path and
skips at every step, so "49/49 pass" is a local-only claim with no CI gate. To
close it, pick one:

- **(cheap, recommended first)** commit golden PSNR/hash expectations per vector
  and a gated CI job that runs `conformance/fetch.sh` (workflow-level env gate,
  not an in-test file check) + asserts against the goldens — no `dec265` needed.
- **(heavier)** build/install `dec265` in CI and run the existing comparison
  with hard `assert!`s, `dec265` path made configurable.

## Tier 2 — the GPU backends. Use existing hardware, not a mini-PC.

The native GPU backends need a real GPU; GitHub-hosted runners have none (the
one GitHub T4 GPU runner needs Team/Enterprise and exercises NVDEC via a
Firefox-only `nvidia-vaapi-driver` shim, not the real `libva` / DXVA paths).
An **Intel-iGPU mini-PC is NOT required** — the cleanest path uses hardware you
already have, split by platform:

### Windows: D3D11VA + MediaFoundation → the Windows host via PowerShell (no purchase)

These backends are `#[cfg(target_os = "windows")]` and need a Windows GPU +
the HEVC codec. If you develop in **WSL**, `cargo test` builds for Linux and
can't touch them — so a bridge runs them on the **Windows host** (which has a
real decode-capable GPU, e.g. an RTX 5070, and the HEVC Video Extensions):

- `just test-win` (or, under WSL, `just ci` runs it automatically; or bare
  `cargo test` triggers the `windows_backends_via_host` bridge test) →
  `scripts/win-test.ps1` runs `cargo test --test backend_dispatch
  --features backend-rust,backend-mediafoundation,backend-d3d11va
  --target x86_64-pc-windows-msvc` natively on Windows, against this same
  checkout over the `\\wsl.localhost\<distro>\…` UNC path, with build artifacts
  on a Windows-local target dir. `HEIC_SKIP_WIN_HOST_TESTS=1` skips it.
- $0, no new hardware, no Intel iGPU. This is the local dev gate. (For *hosted*
  CI, a self-hosted Windows+GPU runner via `d3d11va-runtime.yml` is still
  optional; the dev loop no longer needs it.)

### Linux: VA-API → the one path without a local alternative

VA-API does **not** run in WSL (no `/dev/dri`; only `/dev/dxg` — `vainfo`
fails). So this is the genuine gap. Options, **any GPU vendor — not
Intel-specific**:

- A real Linux box with a GPU + VA-API driver — **NVIDIA** (`nvidia-vaapi-driver`),
  **AMD** (Mesa `radeonsi`), or Intel (`iHD`); `vaapi-runtime.yml` already runs
  `examples/backend_decode … vaapi` + the bit-exact-vs-rust gate on a
  `[self-hosted, linux, vaapi]` runner. $0 marginal on owned hardware.
- Or make VA-API work *inside* WSL via Mesa's D3D12 Gallium VA-API driver
  (`LIBVA_DRIVER_NAME=d3d12`, which targets `/dev/dxg`) — future setup; not
  wired yet.

**Org budget backstop (free, do regardless):** set an Actions product-level
budget with "Stop usage when budget limit is reached" (e.g. $20/mo) so a
leaked token can't run up hosted-runner spend, regardless of the above.

## Implementation order

1. **Windows backends via the host bridge** — DONE: `just test-win` /
   `scripts/win-test.ps1` runs MF + D3D11VA on your Windows GPU from WSL today.
2. **VA-API on a real Linux+GPU box** (any vendor) — the remaining gap; wire
   `vaapi-runtime.yml` to it, or set up the WSL d3d12 VA-API driver.
3. **Conformance goldens** — closes the "49/49 is local-only" gap cheaply.

## The locally-verifiable runtime tool

`examples/backend_decode <file.heic> <backend>` forces one backend, no
fallback, and exits non-zero on failure (`OK <backend> WxH` on success). It's
the shared primitive the emulator + GPU gates run, and it works on a dev box
today (`backend_decode file.heic rust`).
