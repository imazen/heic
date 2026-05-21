# Native HEVC backends for the `heic` crate — design spec

> Status: design draft. Approved by user 2026-05-20.
> Implementation will land as a multi-PR series; see "Sequencing" below.

## Context

`heic` currently has one HEVC decoder: a pure-Rust, `#![forbid(unsafe_code)]`,
`no_std+alloc`-compatible implementation in `src/hevc/`. HEVC (H.265) is
patent-encumbered, and on Windows / Apple / Android / Linux-with-a-GPU the host
platform ships a patent-licensed HEVC decoder. Routing through those native
APIs sidesteps the per-distribution licensing question on those platforms while
keeping the pure-Rust path for everywhere else.

This spec adds **six backends**, all as sibling crates in a workspace —
including extracting today's pure-Rust decoder into its own crate so the
treatment is symmetric. The parent `heic` crate becomes a thin dispatcher: HEIF
container parsing + grid / alpha / gain-map orchestration + a runtime backend
allowlist. Selection at decode time is a user-provided **ordered allowlist**
(e.g. `[VideoToolbox, Rust]`); each entry that reports `BackendUnavailable`
falls through to the next.

Default `cargo build` emits `compile_error!` — users must opt into at least one
backend feature. Runtime decode with no allowlist set returns
`HeicError::NoBackendSelected`. A `recommended_backends()` constructor is
offered for convenience.

## Locked decisions

1. **Six backends**, all in sibling crates in a cargo workspace:
   - `heic-backend-rust` — pure Rust, today's decoder, lifted out of `src/hevc/`
   - `heic-backend-mediafoundation` — Windows Media Foundation
   - `heic-backend-videotoolbox` — Apple VideoToolbox (macOS + iOS simulator)
   - `heic-backend-mediacodec` — Android MediaCodec (NDK C API)
   - `heic-backend-vaapi` — Linux VA-API (all GPU vendors, libva)
   - `heic-backend-amf` — AMD AMF SDK (Windows + Linux, AMD GPUs)
2. **Boundary**: each backend implements `decode_hevc(hvcC config, image_data) → DecodedFrame`. Container, grid, alpha, gain-map, color conversion all stay in the parent / `heic-core`.
3. **Runtime selection: ordered allowlist + fallthrough.** `DecoderConfig::with_backends(&[Backend::Vaapi, Backend::Rust])`. First entry tried; on `BackendUnavailable` fall to the next; exhausted list returns the last error.
4. **No implicit default**: decode without an allowlist returns `HeicError::NoBackendSelected`. `DecoderConfig::recommended_backends()` builds a sensible order from compiled-in features for users who don't want to think about it.
5. **Default build fails**: `default = []`; `lib.rs` `compile_error!` cfg-gate enumerates the backend features.
6. **`unsafe` isolation**: parent `heic` and `heic-core` keep `#![forbid(unsafe_code)]`. All FFI lives in subcrates. Subcrates use `#![deny(unsafe_op_in_unsafe_fn)]` + `// SAFETY:` on every block.
7. **Android CI**: emulator via `reactivecircus/android-emulator-runner`.
8. **iOS CI**: simulator via macOS runner + `aarch64-apple-ios-sim` target + `xcrun simctl`.

## Workspace layout

```
heic/                                  # repo root, becomes a cargo workspace
├── Cargo.toml                         # [workspace] members = [...]
├── heic/                              # PARENT crate (public API)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                     # public API, DecoderConfig, allowlist
│       ├── decode.rs                  # grid/alpha/gainmap orchestration
│       ├── backend.rs                 # Backend enum, dispatch loop, fallthrough
│       ├── heif/                      # ISOBMFF box parsing (stays here)
│       ├── auxiliary.rs, codec.rs, error.rs, ...
├── heic-core/                         # NEW shared types/utilities crate
│   ├── Cargo.toml                     # no_std, forbid(unsafe)
│   └── src/
│       ├── lib.rs                     # HevcBackend trait, BackendError
│       ├── frame.rs                   # DecodedFrame (moved from hevc::picture)
│       ├── color.rs                   # YCbCr → RGB (moved from hevc::color_convert + picture::to_rgb*)
│       └── nal.rs                     # hvcC ↔ Annex B helpers, minimal SPS dim reader
├── heic-backend-rust/                 # NEW — extracted from src/hevc/
│   ├── Cargo.toml                     # no_std, forbid(unsafe), depends on heic-core
│   └── src/                           # all of today's src/hevc/*
├── heic-backend-mediafoundation/      # Windows MF
├── heic-backend-videotoolbox/         # Apple VT (macOS, iOS, iOS sim, tvOS, visionOS)
├── heic-backend-mediacodec/           # Android NDK AMediaCodec
├── heic-backend-vaapi/                # Linux VA-API
├── heic-backend-amf/                  # AMD AMF SDK
├── ci/
│   └── android-harness/               # Gradle project for Android emulator tests
└── .github/workflows/                 # ci.yml + per-backend matrix
```

`heic-core` is the contract crate every backend (and the parent) depends on.
The reason it exists rather than living in the parent: backend subcrates can't
import from the parent (cycle), so the `DecodedFrame` and the `HevcBackend`
trait must live in a separate crate they all reach.

## Backend contract (in `heic-core`)

```rust
// heic-core/src/lib.rs
#![no_std]
#![forbid(unsafe_code)]
extern crate alloc;

#[derive(Debug)]
#[non_exhaustive]
pub enum BackendError {
    /// Backend is not available at runtime — caller should try next in allowlist.
    /// e.g. "MediaFoundation HEVC MFT not installed", "VA-API: no HEVC profile".
    Unavailable(&'static str),
    /// Backend was called but the bitstream is malformed in a way it can't handle.
    /// Caller MAY try the next backend in the allowlist as a recovery attempt.
    Decode(alloc::string::String),
    /// Limits exceeded (oversized image, etc.). Do NOT fall through.
    LimitsExceeded(&'static str),
    /// Operation cancelled via Stop. Do NOT fall through.
    Cancelled,
}

pub trait HevcBackend: Send {
    /// Stable name for logging / error messages, e.g. "videotoolbox".
    fn name(&self) -> &'static str;

    /// True if this backend can actually run on this machine right now
    /// (e.g. MediaFoundation HEVC extensions installed, VA-API driver loaded).
    /// Allowed to be heuristic — fallthrough will catch false positives.
    fn is_available(&self) -> bool;

    /// Decode a single HEVC tile from hvcC config + length-prefixed slice data.
    fn decode_hevc(
        &mut self,
        config: &HvccParams,
        image_data: &[u8],
        stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError>;
}

/// Subset of HEVCDecoderConfigurationRecord that backends actually need.
/// (The parent owns the full `HevcDecoderConfig` and constructs this from it.)
pub struct HvccParams<'a> {
    pub nal_units: &'a [&'a [u8]],   // VPS / SPS / PPS payloads (RBSP)
    pub length_size: u8,             // 1, 2, or 4
    pub bit_depth_luma: u8,          // 8 or 10
    pub bit_depth_chroma: u8,
    pub chroma_format_idc: u8,
}

pub use frame::DecodedFrame;
```

Each backend crate exports a single constructor:

```rust
// e.g. heic-backend-videotoolbox/src/lib.rs
pub fn new() -> impl heic_core::HevcBackend { VideoToolboxBackend::default() }
```

## Allowlist API

```rust
// heic/src/lib.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    Rust,
    MediaFoundation,
    VideoToolbox,
    MediaCodec,
    Vaapi,
    Amf,
}

impl Backend {
    /// Whether this backend variant is **compiled in** for this build
    /// (feature + target_os both match).
    pub const fn is_compiled_in(self) -> bool { ... }

    /// Construct a backend instance. Returns `None` if not compiled in.
    fn instance(self) -> Option<Box<dyn HevcBackend>> { ... }
}

#[derive(Clone)]
pub struct DecoderConfig {
    // ...existing fields...
    backends: alloc::vec::Vec<Backend>,
}

impl DecoderConfig {
    pub fn with_backends(mut self, backends: &[Backend]) -> Self {
        self.backends = backends.to_vec();
        self
    }

    pub fn with_backend(self, backend: Backend) -> Self {
        self.with_backends(&[backend])
    }

    /// All compiled-in backends in a sensible order: native (platform-matching)
    /// first, then Rust as a final fallback if compiled in. Use this if you
    /// don't want to think about ordering.
    pub fn recommended_backends(self) -> Self { ... }
}
```

**Decode-time dispatch loop** in `heic/src/backend.rs`:

```rust
pub(crate) fn decode_one_tile(
    backends: &[Backend],
    config: &HevcDecoderConfig,
    image_data: &[u8],
    stop: &dyn Stop,
) -> Result<DecodedFrame> {
    if backends.is_empty() {
        return Err(HeicError::NoBackendSelected);
    }
    let mut last_err = None;
    for &b in backends {
        let Some(mut inst) = b.instance() else { continue }; // not compiled in for this build
        if !inst.is_available() { continue; }
        match inst.decode_hevc(&config.into(), image_data, stop) {
            Ok(frame) => return Ok(frame),
            Err(BackendError::LimitsExceeded(m)) => return Err(HeicError::LimitsExceeded(m)),
            Err(BackendError::Cancelled) => return Err(HeicError::Cancelled),
            Err(BackendError::Unavailable(m)) => { last_err = Some(format!("{}: {}", b.name(), m)); }
            Err(BackendError::Decode(m)) => { last_err = Some(format!("{}: {}", b.name(), m)); }
        }
    }
    Err(HeicError::AllBackendsFailed(last_err.unwrap_or_else(|| "no backends were available".into())))
}
```

This routes through each backend in user-given order; `Unavailable` and
`Decode` errors are recoverable (try next), `LimitsExceeded` and `Cancelled`
short-circuit. The decode-orchestration in `decode.rs` constructs one backend
chain per `DecodeRequest` and reuses it across tiles in a grid (so we don't
re-init MediaFoundation / VTDecompressionSession per tile). Backends that hold
expensive state (VT session, MF transform) cache it across calls in their
`&mut self`.

## Cargo features

`heic/Cargo.toml`:

```toml
[features]
default = []

# Backend selection — at least one must be picked for the target, or compile fails.
backend-rust              = ["dep:heic-backend-rust", "dep:heic-core"]
backend-mediafoundation   = ["std", "dep:heic-backend-mediafoundation", "dep:heic-core"]
backend-videotoolbox      = ["std", "dep:heic-backend-videotoolbox", "dep:heic-core"]
backend-mediacodec        = ["std", "dep:heic-backend-mediacodec", "dep:heic-core"]
backend-vaapi             = ["std", "dep:heic-backend-vaapi", "dep:heic-core"]
backend-amf               = ["std", "dep:heic-backend-amf", "dep:heic-core"]

# Existing features (stay)
std            = ["heic-core/std", "archmage/std", "ultrahdr-core?/std"]
fallible-alloc = []
parallel       = ["std", "dep:rayon"]
zencodec       = [ ... ]
```

`heic/src/lib.rs` compile_error gate:

```rust
#[cfg(not(any(
    feature = "backend-rust",
    all(feature = "backend-mediafoundation", target_os = "windows"),
    all(feature = "backend-videotoolbox",
        any(target_os = "macos", target_os = "ios", target_os = "tvos", target_os = "visionos")),
    all(feature = "backend-mediacodec", target_os = "android"),
    all(feature = "backend-vaapi", target_os = "linux"),
    all(feature = "backend-amf", any(target_os = "windows", target_os = "linux")),
)))]
compile_error!(
    "heic: no HEVC backend is active for this target. \
     Enable one of: `backend-rust` (any target), \
     `backend-mediafoundation` (windows), \
     `backend-videotoolbox` (apple), \
     `backend-mediacodec` (android), \
     `backend-vaapi` (linux), \
     `backend-amf` (windows or linux)."
);
```

Each native subcrate is structured so it compiles to an empty `lib` on the
wrong target (`#[cfg(target_os = "...")]` gates the FFI; the `pub fn new()`
returns a stub backend whose `is_available()` returns false). This means
enabling `backend-videotoolbox` on Linux compiles cleanly but produces a
backend that always falls through. The `compile_error!` in `heic/lib.rs` is the
safety net: if it's the *only* selected backend, the build still fails on that
target.

## Per-backend implementation notes

### `heic-backend-rust`

- Move all of today's `src/hevc/*.rs` into `heic-backend-rust/src/` verbatim.
- The internal `decode_with_config` becomes the `HevcBackend::decode_hevc` impl.
- Strip the `pub(crate)` annotations on items that were only accessed within `src/hevc/`; they become crate-private here.
- `DecodedFrame` no longer lives in this crate — import from `heic-core`.
- Tests (current ones in `tests/`, examples in `examples/`) move with it. The parent crate retains the integration tests that exercise the whole pipeline.
- `forbid(unsafe_code)` stays.
- Mechanically the biggest chunk of the refactor (~50 files, ~25k LOC moved).

### `heic-backend-mediafoundation` (Windows)

- Crate: `windows` (`Win32_Media_MediaFoundation`, `Win32_Media_KernelStreaming`, `Win32_System_Com`).
- `MFTEnumEx` for `MFT_CATEGORY_VIDEO_DECODER` + `MFVideoFormat_HEVC` → pick first synchronous-mode HEVC MFT.
- Input `IMFMediaType`: `MFVideoFormat_HEVC` + `MF_MT_FRAME_SIZE` + `MF_MT_MPEG_SEQUENCE_HEADER` = Annex-B VPS+SPS+PPS concatenation.
- Output `IMFMediaType`: NV12 (8-bit) or P010 (10-bit).
- Convert hvcC → Annex B before each `ProcessInput`.
- 10-bit P010: right-shift u16 by 6 to recover 10-bit range.
- Handle stride via `MF_MT_DEFAULT_STRIDE` / `IMF2DBuffer`.
- `BackendError::Unavailable` if the HEVC MFT isn't installed (Windows 10 LTSC, Server SKUs, fresh installs without "HEVC Video Extensions" — they ship paid on consumer Windows since ~2018).
- Targets: `x86_64-pc-windows-msvc`, `aarch64-pc-windows-msvc`.

### `heic-backend-videotoolbox` (Apple)

- Crates: `objc2`, `objc2-foundation`, `objc2-core-media`, `objc2-core-video`, `objc2-video-toolbox`, `block2`.
- `CMVideoFormatDescriptionCreateFromHEVCParameterSets` with `nal_unit_header_length = length_size`.
- `VTDecompressionSession` with destination pixel-buffer attributes: `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange` (8-bit) or `kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange` (10-bit), plus `kCVPixelBufferIOSurfacePropertiesKey = {}`.
- `CMBlockBuffer` from raw length-prefixed slice bytes (VT wants hvcC format directly — no Annex-B conversion).
- `VTDecompressionSessionDecodeFrame` with `kVTDecodeFrame_EnableAsynchronousDecompression = false`, then `WaitForAsynchronousFrames`.
- Read color metadata from `CVPixelBuffer` attachments: `kCVImageBufferYCbCrMatrixKey`, `kCVImageBufferColorPrimariesKey`, `kCVImageBufferTransferFunctionKey`. Prefer SPS values; use attachments only for missing fields.
- Targets: macOS aarch64, macOS x86_64 (`macos-26-intel`), iOS arm64 simulator, iOS arm64 device, tvOS, visionOS (compile-checked).

### `heic-backend-mediacodec` (Android)

- Crate: `ndk-sys` (raw NDK bindings) + `ndk` (safer wrappers where available). NDK `AMediaCodec` C API only — no JNI.
- Min API 21.
- `AMediaFormat` with `AMEDIAFORMAT_KEY_MIME = "video/hevc"`, `AMEDIAFORMAT_KEY_WIDTH/HEIGHT`, `AMEDIAFORMAT_KEY_CSD_0` = Annex-B VPS+SPS+PPS.
- `AMediaCodec_createDecoderByType` → `configure(surface = null)` → `start()` → input buffer loop with `BUFFER_FLAG_END_OF_STREAM` on drain → output buffer.
- Color format probing: handle `COLOR_FormatYUV420Flexible` mapping to NV12, NV21, YV12, or I420 device-dependent.
- 10-bit: gate on `COLOR_FormatYUVP010` profile availability via `MediaCodecList`.
- Targets: `aarch64-linux-android`, `x86_64-linux-android` (emulator), `armv7-linux-androideabi`, `i686-linux-android`.

### `heic-backend-vaapi` (Linux)

- Crate: `cros-libva` if its API covers HEVC decode; fall back to `libva-sys` raw bindings.
- `Display::open()` over DRM render-node `/dev/dri/renderD128`, X11 fallback.
- Query `VAProfileHEVCMain` (8-bit) / `VAProfileHEVCMain10` (10-bit) via `vaQueryConfigProfiles`. Missing → `BackendError::Unavailable`.
- `vaCreateConfig(VAEntrypointVLD, profile)` → `vaCreateSurfaces` → `vaCreateContext` → `vaBeginPicture` → `vaRenderPicture(buffers)` → `vaEndPicture` → `vaSyncSurface` → `vaDeriveImage`/`vaGetImage`.
- Parameter buffers: minimal HEVC SPS/PPS parser in `heic-core::nal` produces `VAPictureParameterBufferHEVC` / `VASliceParameterBufferHEVC` / `VAIQMatrixBufferHEVC`. Chromium does the same.
- Driver support varies: iHD (Intel), radeonsi (AMD mesa), nvidia-vaapi-driver (NVIDIA via libnvdec), i965 (legacy Intel). Driver bugs are common on edge profiles (10-bit 4:2:2, scaling lists) — surface as `BackendError::Unavailable` so the allowlist falls through.
- Targets: `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`.

### `heic-backend-amf` (AMD)

- AMD's Advanced Media Framework SDK, MIT-licensed at https://github.com/GPUOpen-LibrariesAndSDKs/AMF.
- `bindgen` over vendored AMF C headers (subset we need: factory + video decoder).
- Load `amfrt64.dll` (Windows) / `libamfrt64.so.1` (Linux) via `libloading` at runtime. Missing → `BackendError::Unavailable`.
- `AMFCreateContext` → `InitDX11` / `InitVulkan` → `CreateComponent(AMFVideoDecoderHW_H265_MAIN)` (or `..._MAIN10`).
- `SubmitInput(AMFBuffer)` with raw NAL bitstream → `QueryOutput(AMFSurface)` → `GetPlane` for Y / U / V.
- Mostly redundant on Linux where VA-API also covers AMD; AMF gives finer control (DirectX surfaces, hardware tone-mapping, encoder reuse paths).
- Targets: `x86_64-pc-windows-msvc`, `x86_64-unknown-linux-gnu`.

## File-by-file changes

**Existing `heic/` modifications:**

- `Cargo.toml` — switch to workspace, strip moved deps, add backend feature gates.
- `src/lib.rs` — `compile_error!` gate, `pub mod backend`, swap `pub use hevc::DecodedFrame` for `pub use heic_core::DecodedFrame`, gate `pub mod hevc` on `backend-rust`.
- `src/decode.rs` — 9 HEVC call sites (lines per Phase-1 explore: 104, 118, 128, 240, 242, 1200, 1484, 1660, 1857) re-routed through `backend::decode_one_tile(self.backends, ...)`.
- `src/codec.rs` — 1 call site updated, allowlist plumbed through zencodec adapter.
- `src/hevc/` — entire directory MOVED to `heic-backend-rust/src/`.
- `src/error.rs` — add `NoBackendSelected`, `AllBackendsFailed(String)`, `BackendUnavailable(String)`.

**New `heic-core/`:**

- `Cargo.toml` — minimal deps: `enough`, `whereat`, `archmage` (for SIMD color convert). no_std + alloc.
- `src/lib.rs` — `HevcBackend` trait, `BackendError`, `HvccParams`.
- `src/frame.rs` — `DecodedFrame` from `hevc::picture`.
- `src/color.rs` — `to_rgb`, `to_rgba`, `to_rgb16`, `to_rgba16`, `convert_420_to_rgb` from `hevc::picture` + `hevc::color_convert`. SIMD impls (AVX2 + NEON + WASM) come along.
- `src/nal.rs` — `hvcc_to_annexb`, `annexb_to_length_prefixed`, `read_sps_dimensions` (minimal SPS parser).

**New per-backend crates** — see above; each has Cargo.toml + `src/lib.rs` (impl `HevcBackend`) + README.md.

## CI matrix

`.github/workflows/ci.yml`:

```yaml
jobs:
  # 1. Rust backend on every existing runner
  test-rust:
    strategy:
      matrix:
        os: [ubuntu-latest, ubuntu-24.04-arm, macos-latest, macos-26-intel, windows-latest, windows-11-arm]
    steps:
      - cargo test --features backend-rust

  # 2. WMF on Windows
  test-mediafoundation:
    strategy:
      matrix:
        os: [windows-latest, windows-11-arm]
    steps:
      - winget install --id 9N4WGH0Z6VHQ (HEVC Video Extensions) if available
      - cargo test -p heic --features backend-mediafoundation
      - Cross-backend conformance vs Rust backend on bundled corpus

  # 3. VT on macOS
  test-videotoolbox-macos:
    strategy:
      matrix:
        os: [macos-latest, macos-26-intel]
    steps:
      - cargo test -p heic --features backend-videotoolbox
      - Cross-backend conformance

  # 4. VT on iOS simulator
  test-videotoolbox-ios-sim:
    runs-on: macos-latest
    steps:
      - rustup target add aarch64-apple-ios-sim
      - xcrun simctl boot "iPhone 16"
      - cargo build -p heic-backend-videotoolbox --target aarch64-apple-ios-sim
      - cargo build -p heic --features backend-videotoolbox --target aarch64-apple-ios-sim --tests
      - xcrun simctl spawn booted <test-binary>
      - xcrun simctl shutdown all

  # 5. MediaCodec on Android emulator
  test-mediacodec:
    runs-on: ubuntu-latest
    steps:
      - rustup target add x86_64-linux-android aarch64-linux-android
      - setup-ndk r27b
      - cargo build -p heic-backend-mediacodec --target x86_64-linux-android
      - cd ci/android-harness && ./gradlew assembleDebugAndroidTest
      - reactivecircus/android-emulator-runner@v2 api-level 34 arch x86_64
        → ./gradlew connectedDebugAndroidTest

  # 6. VA-API on Linux (compile only — no GPU on default runners)
  test-vaapi:
    runs-on: ubuntu-latest
    steps:
      - apt install libva-dev
      - cargo build -p heic-backend-vaapi
      - cargo build -p heic --features backend-vaapi
      - cargo test -p heic-backend-vaapi --no-run
      - (Runtime VA-API requires a self-hosted runner with a libva-capable GPU.)

  # 7. AMF compile-only
  test-amf:
    strategy:
      matrix:
        os: [windows-latest, ubuntu-latest]
    steps:
      - cargo build -p heic-backend-amf
      - cargo build -p heic --features backend-amf
      - (Runtime AMF requires AMD GPU hardware + AMF runtime DLL.)

  # 8. Default build must fail with helpful message
  test-no-backend:
    runs-on: ubuntu-latest
    steps:
      - "! cargo build --no-default-features 2>&1 | tee build.log"
      - grep -q "no HEVC backend is active" build.log

  # 9. Existing jobs (clippy, fmt, msrv, i686, coverage) updated for backend-rust feature
```

`.github/workflows/backend-conformance.yml` (nightly cron):

- Decode the entire `test-images/` corpus through each available backend on each platform.
- Produce a CSV of (file, backend, max_diff, mean_diff, psnr) vs the Rust backend.
- Upload as artifact; summary in workflow output.
- Initial pass criterion: no backend errors on the 103 currently-passing files; RGB delta ≤ documented tolerance.

## Sequencing (PRs)

Each PR keeps `cargo test --features backend-rust` green on all current CI runners.

1. **PR 1 — Workspace + `heic-core` extraction.** Create `heic-core` with `DecodedFrame`, color conversion, `HevcBackend` trait, `BackendError`. Convert repo to a cargo workspace. `src/hevc/` still compiles in-tree but imports `DecodedFrame` from `heic-core`. No behavior change.
2. **PR 2 — Extract `heic-backend-rust`.** Physically move `src/hevc/` → `heic-backend-rust/src/`. Parent gets a thin `backend::rust` wrapper. Allowlist API in `DecoderConfig`. Feature flag `backend-rust`. `compile_error!` gate. CI rename. Mechanically the largest PR.
3. **PR 3 — Windows MediaFoundation.** New crate, CI matrix entries for `windows-latest` + `windows-11-arm`.
4. **PR 4 — Apple VideoToolbox (macOS + iOS simulator).** New crate, CI for `macos-latest`, `macos-26-intel`, iOS sim.
5. **PR 5 — Android MediaCodec.** New crate, CI via emulator action.
6. **PR 6 — Linux VA-API.** New crate, compile-only CI. Documentation of self-hosted-runner path for functional CI.
7. **PR 7 — AMD AMF.** New crate, compile-only CI on Windows + Linux.
8. **PR 8 — Cross-backend conformance workflow + README updates + CHANGELOG.**

## Verification

After **PR 1** (workspace + heic-core):

- `cargo test` from the repo root runs all members; existing `heic` tests pass unchanged.
- `cargo check -p heic-core --no-default-features` succeeds (no_std).

After **PR 2** (extract heic-backend-rust):

- `cargo test --features backend-rust` matches behavior of pre-refactor `cargo test`. Bit-identical output across the 103-file corpus.
- `cargo build --no-default-features` fails with the new `compile_error!`.
- `cargo bench --features backend-rust` within 2 % of pre-refactor baseline.
- `cargo check -p heic-backend-rust --no-default-features` succeeds (preserves no_std).

After **PR 3** (WMF):

- Cross-backend conformance: `example.heic` and `classic-car-iphone12pro.heic` decoded via WMF vs Rust → max_diff ≤ 4/255 per channel.
- Fallthrough: allowlist `[MediaFoundation, Rust]` on a runner where HEVC extensions are missing must succeed via Rust and report MF unavailable in logs.

After **PR 4** (VT macOS + iOS sim):

- Conformance test on both macOS architectures.
- iOS simulator: decode `example.heic` (1280×854); assert output dimensions + first-pixel sample.

After **PR 5** (Android):

- `connectedDebugAndroidTest` passes on emulator API 34 x86_64.
- Decode `example.heic` and assert dimensions.

After **PR 6** (VA-API):

- Compile passes; runtime test left as a manual local-machine recipe in `heic-backend-vaapi/README.md`.

After **PR 7** (AMF):

- Compile passes; runtime test documented similarly.

After **PR 8** (conformance + docs):

- Nightly workflow produces CSV artifact.
- README documents backend selection, fallthrough, and which platforms have CI coverage vs compile-only.
- CHANGELOG entry under `[Unreleased]` covers the breaking change (`default-features` no longer gives a working decoder; new allowlist API).
- `cargo semver-checks` confirms this is a 0.2.0 bump from 0.1.6.

## Risks & mitigations

- **PR 2 size.** Extracting `src/hevc/` is mechanically large (~25 k LOC moved). Mitigation: pure file-move + import-fix commit, no logic edits in the same commit. Reviewers can `git log --follow` to confirm.
- **`heic` no longer publishable solo.** Crates.io needs each member published independently. Publish in dependency order (`heic-core` → backends → `heic`); document the order in `RELEASING.md`.
- **VA-API + AMF can't be functionally tested in default CI.** Document the self-hosted-runner path; ship compile-only initially. Open issues for adding self-hosted runners for each.
- **AMF SDK headers licensing.** AMF is MIT-licensed by AMD — safe to vendor. Bindgen at build time, no headers in the published crate. License-audit pass before PR 7.
- **Bitstream parameter-set parsing duplicated.** Both `heic-backend-rust` and `heic-backend-vaapi` need to parse the HEVC SPS. Minimal SPS dimension/bit-depth reader lives in `heic-core::nal`. Full SPS parser (scaling lists, etc.) stays in `heic-backend-rust`.
- **Runtime-DLL backends (WMF, AMF, Android MediaCodec, VA-API).** All four lazily load their runtime. The `is_available()` probe must not panic — failure to dlopen returns `false`, not abort. Each backend's tests must include an "unavailable" simulation path.
- **`forbid(unsafe_code)` audit.** Parent `heic` and `heic-core` keep `forbid(unsafe_code)`. CI runs an enforcement step (cargo-geiger or rg over the crate) on every PR.

## Out of scope

- **No encode paths.** Decode-only, as today.
- **No NVDEC or Intel QSV backends.** VA-API on Linux + MediaFoundation on Windows already cover those vendors. Can be added later if specific use cases need them.
- **No iOS / tvOS / visionOS device CI** — GH Actions has no real-device runners. Simulator is functional CI; device is left as a manual / TestFlight step.
- **No backend-aware zencodec adapter.** `codec.rs` adapter routes via the allowlist; explicit backend pinning from the zencodec layer is a future extension.
- **`heic-backend-*` crates do not initially publish to crates.io** as standalone deps — they remain workspace path-deps until heic 0.2 ships with a stable inter-crate ABI.
