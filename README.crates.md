<!-- GENERATED FROM README.md by zenutils gen-readme-crates.sh — DO NOT EDIT. -->

# heic

HEIC/HEIF image decoder for Rust. Ships with a pure-Rust HEVC backend AND optional native backends for Windows (Media Foundation), Apple (VideoToolbox), Android (MediaCodec), and Linux (VA-API) — pick the patent-licensed path that ships with the platform, or fall back to the pure-Rust decoder. The parent crate is `#![forbid(unsafe_code)]`; FFI lives in isolated subcrates.

> ⚠️ **Patent notice:** HEVC/HEIF may be covered by third-party patents
> (Access Advance and others). Imazen grants copyright permissions
> only — see [Patents](#patents).

- `#![forbid(unsafe_code)]` — zero unsafe blocks in the entire codebase
- `no_std + alloc` compatible (compiles for wasm32-unknown-unknown)
- Multi-codec: HEVC (built-in), AV1 via [rav1d-safe] (`av1` feature), uncompressed HEIF via [zenflate] (`unci` feature)
- AVX2/SSE4.1/NEON SIMD acceleration with automatic scalar fallback
- Cooperative cancellation via [enough] — all decode paths check `Stop` tokens
- Configurable resource limits (dimensions, pixel count, memory) enforced before allocation
- Optional tile-parallel decoding via rayon

## Quick start

```toml
[dependencies]
heic = { version = "0.2.0", default-features = false, features = ["backend-rust", "std"] }
```

```rust
use heic::{DecoderConfig, PixelLayout};

let data = std::fs::read("image.heic")?;
let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;
println!("{}x{} image, {} bytes", output.width, output.height, output.data.len());
```

The default feature set is **empty**: every build must opt into at least one HEVC
backend or it fails with a `compile_error!`. `backend-rust` is the always-available
pure-Rust decoder, and it does *not* pull in `std`, so a server lists `std` too (for
`std::fs`, threads, and the `parallel` feature). On Apple, Windows, Android, and
Linux you can instead route to the platform's own patent-licensed HEVC decoder via
the optional native-backend crates (`backend-videotoolbox`, `backend-mediafoundation`,
`backend-mediacodec`, `backend-vaapi`, `backend-d3d11va`) — their FFI lives in
isolated subcrates so the parent crate stays `#![forbid(unsafe_code)]`. See
[Backends](#backends) for the allowlist/fallthrough API.

## Status

Decodes most HEIC files from iPhones and cameras. 118/162 HEIF test files decode successfully (with `av1` + `unci` features). 49/49 ITU-T HEVC conformance vectors pass. Best quality: 77.3 dB PSNR (BT.709), 73% pixel-exact on example.heic.

### What works
- HEIF container parsing (ISOBMFF boxes, grid images, overlays, identity-derived items)
- HEIF image sequences (msf1/moov — first I-frame extraction from movie structure)
- Multi-codec dispatch: HEVC, AV1 (`av1` feature), uncompressed HEIF (`unci` feature)
- Full HEVC I-frame decoding (VPS/SPS/PPS, CABAC, intra prediction, transforms)
- AV1 still image decoding via rav1d-safe (`av1` feature)
- Uncompressed HEIF with deflate/zlib decompression via zenflate (`unci` feature)
- Deblocking filter and SAO (Sample Adaptive Offset)
- YCbCr→RGB with BT.601/BT.709/BT.2020 matrices (full + limited range)
- CICP color info from HEVC VUI and HEIF colr nclx (container overrides codec)
- 10-bit HEVC with transparent 8-bit downconvert or 16-bit output preservation
- Alpha plane decoding from auxiliary images (HEVC and AV1)
- HDR gain map extraction (Apple `urn:com:apple:photo:2020:aux:hdrgainmap`)
- EXIF, XMP, and ICC profile extraction (zero-copy where possible)
- Thumbnail decode, image rotation/mirror transforms (ipma ordering)
- HEVC scaling lists (custom dequantization matrices)
- AVX2 SIMD: color conversion, IDCT 8/16/32, residual add, dequantize
- SSE4.1 SIMD: IDST 4x4
- NEON SIMD: color conversion, IDCT 8/16/32, IDST 4x4
- Tile-parallel grid decoding via rayon (`parallel` feature)

### Known limitations
- HEVC range-extension (RExt) *coding tools* beyond the chroma formats and bit depths — e.g. the PPS `chroma_qp_offset_list` (`cu_chroma_qp_offset_flag`) — are not implemented; a stream using one fails with a CABAC desync error rather than decoding. 4:0:0 / 4:2:0 / 4:2:2 / 4:4:4 at 8 and 10 bit all decode (4:2:2 since #48).
- HEVC I-slices only (sufficient for HEIC still images; no inter prediction for video)
- JPEG and H.264/AVC codecs in HEIF: detected but not decoded
- Brotli-compressed uncompressed HEIF: not yet supported (deflate/zlib only)
- PQ/HLG transfer functions: parsed and exposed via `ImageInfo`, but no EOTF applied — callers handle tone-mapping

## Backends

`heic` defers HEVC bitstream decoding to a pluggable backend. The parent crate parses the HEIF container, manages grid / alpha / gain-map orchestration, and walks a runtime allowlist of backends — falling through when a backend reports unavailable. Default `cargo build` fails with a `compile_error!` directing you to pick at least one.

**Server / CLI setup — copy-paste this:**

```toml
heic = { version = "0.2.0", default-features = false, features = ["backend-rust", "std"] }
```

Two gotchas this line handles, both of which break a build if you miss them:

- **`default-features` is empty** — with no `backend-*` feature enabled, the crate emits a `compile_error!`. You must opt into at least one backend; `backend-rust` is the always-available pure-Rust decoder.
- **`backend-rust` does NOT imply `std`.** It is a `no_std + alloc` decoder by default, so a server that wants `std::fs`, threads, or the rayon `parallel` feature must list `std` explicitly. (Only the *native* backends — `backend-mediafoundation` etc. — pull `std` in transitively; `backend-rust` does not.) Omitting `std` is the most common first-try server build failure.

(On the currently-published `0.1.x`, the pure-Rust decoder is always compiled and `std` is on by default, so neither gotcha applies; the `backend-rust` feature and the empty-default `compile_error!` arrive with `0.2.0`. The line above is correct for `0.2.0` onward.)

| Backend | Cargo feature | Targets | Status |
|---|---|---|---|
| Pure-Rust HEVC | `backend-rust` | all | Production. 118/162 HEIF corpus, 49/49 ITU-T conformance vectors. |
| Media Foundation | `backend-mediafoundation` | Windows | Production. Runtime-CI verified on `windows-11-arm` with HEVC Video Extensions side-loaded. |
| VideoToolbox | `backend-videotoolbox` | macOS, iOS, tvOS, visionOS | FFI complete; CI on `macos-latest` + `macos-15-intel`. |
| MediaCodec | `backend-mediacodec` | Android | FFI complete; CI compile-only via Android NDK. |
| VA-API | `backend-vaapi` | Linux | Runtime FFI implemented (libva HEVC decode). No GPU on hosted CI — compile-only there; validate on a Linux+GPU host via `vaapi-runtime.yml`. |
| D3D11VA | `backend-d3d11va` | Windows | Runtime FFI implemented (DXVA HEVC decode). No HEVC-capable GPU on hosted Windows CI — compile-only there; validate on a Windows+GPU host. |

```rust
use heic::{Backend, DecoderConfig, PixelLayout};

// Single backend.
let output = DecoderConfig::new()
    .with_backend(Backend::MediaFoundation)
    .decode(&data, PixelLayout::Rgba8)?;

// Ordered allowlist with fallthrough.
let output = DecoderConfig::new()
    .with_backends(&[Backend::VideoToolbox, Backend::Rust])
    .decode(&data, PixelLayout::Rgba8)?;

// Auto-pick a sensible order from the compiled-in backends.
let output = DecoderConfig::new()
    .with_backends(&heic::recommended_backends())
    .decode(&data, PixelLayout::Rgba8)?;
```

`BackendError::Unavailable` (driver / DLL / OS support missing) and `BackendError::Decode` (bitstream rejected) both fall through to the next backend; `LimitsExceeded` and `Cancelled` short-circuit.

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `backend-rust` | no | Pure-Rust HEVC decoder. Always available; runs on every target. |
| `backend-mediafoundation` | no | Windows Media Foundation HEVC MFT (requires HEVC Video Extensions). |
| `backend-videotoolbox` | no | Apple VideoToolbox HEVC decoder (macOS / iOS / tvOS / visionOS). |
| `backend-mediacodec` | no | Android NDK `AMediaCodec` HEVC decoder (API 21+). |
| `backend-vaapi` | no | Linux libva HEVC decoder (runtime FFI implemented; needs a GPU + VA-API driver at runtime). |
| `backend-d3d11va` | no | Windows D3D11 DXVA HEVC decoder (runtime FFI implemented; needs an HEVC-capable GPU at runtime). |
| `std` | no | Standard library support (file I/O, threads). NOT pulled in by `backend-rust` — enable it explicitly for a server; the native backends imply it. Leave off for `no_std + alloc`. |
| `parallel` | no | Parallel tile decoding via rayon. Implies `std`. |
| `av1` | no | AV1 codec support via rav1d-safe. Implies `std`. |
| `unci` | no | Uncompressed HEIF (ISO 23001-17) with deflate/zlib decompression via zenflate. |

## Usage

```rust
use heic::{DecoderConfig, PixelLayout};

let data = std::fs::read("image.heic")?;
let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;
println!("{}x{} image, {} bytes", output.width, output.height, output.data.len());
```

### Limits and cancellation

`with_limits` rejects oversized inputs *before* allocation; `with_stop` lets you
abort an in-flight decode (request timeout, client disconnect). `Limits::default()`
is the same **safe cap** as `Limits::server_defaults()` — `16384×16384`, 256 MP,
1 GiB, *not* unlimited — and is also what the decoder applies when you pass no
`Limits` at all; set an individual field to `None` to lift only that cap. The stop
parameter is `&dyn enough::Stop`. The no-op token is `enough::Unstoppable`
(both are re-exported as `heic::Unstoppable` / `heic::Stop`); for a *cancellable*
token use `almost_enough::Stopper`, which is `Clone` + `Send` + `Sync` — hand a
clone to a timeout/disconnect watcher and call `.cancel()` to stop the decode,
which then returns `HeicError::Cancelled`.

```toml
# in addition to the `heic` line above:
enough = "0.4.4"          # the Stop trait (re-exported by heic, so optional)
almost-enough = "0.4.4"   # Stopper: a ready-made cancellable Stop
```

```rust
use heic::{DecoderConfig, PixelLayout, Limits};
use almost_enough::Stopper;

let mut limits = Limits::default();
limits.max_width = Some(8192);
limits.max_height = Some(8192);
limits.max_pixels = Some(64_000_000);
limits.max_memory_bytes = Some(512 * 1024 * 1024);

// A cancellable token. Clone it into whatever fires on timeout / disconnect.
let stop = Stopper::new();
{
    let stop = stop.clone();
    // e.g. spawn a watcher: on request timeout or socket close, `stop.cancel();`
    // (here we just show the call you'd make from that watcher)
    let _cancel_from_watcher = move || stop.cancel();
}

let output = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .with_limits(&limits)
    .with_stop(&stop)        // omit this (or pass &heic::Unstoppable) to never cancel
    .decode()?;              // returns Err(HeicError::Cancelled) if `stop.cancel()` fired
```

The one-shot `DecoderConfig::new().decode(&data, layout)` and probe/extract APIs
use `Unstoppable` internally, so they cannot be cancelled — reach for the
`decode_request(..).with_stop(..)` builder when you need to abort.

### Bit depth and HDR — what `PixelLayout::Rgba8` gives you

`PixelLayout` is 8-bit only (`Rgb8`, `Rgba8`, `Bgr8`, `Bgra8`). Decoding a
**10-bit HEIC** (iPhone HDR, BT.2020) to one of these **silently downconverts to
8 bits** by a right-shift of `bit_depth − 8` (i.e. `sample >> 2` for 10-bit) —
the low bits are dropped. The decode succeeds; there is no error or warning.

Just as important: **no transfer function (EOTF) is applied.** If the file is PQ
(`transfer_characteristics == 16`) or HLG (`== 18`), the 8-bit output samples are
still PQ/HLG-encoded code values, *not* tone-mapped SDR. The decoder parses and
exposes the transfer characteristic (`ImageInfo::transfer_characteristics`, and
`DecodedFrame::transfer_characteristics` after `decode_to_frame`) but leaves the
EOTF + tone-mapping to you. So `Rgba8` on an HDR HEIC = "the HDR signal,
quantized to 8 bits, in its original (PQ/HLG) domain" — fine for a quick preview,
wrong if you need correct color without doing the tone-map yourself.

To **keep the full 10-bit precision**, skip `PixelLayout` and use the YCbCr→RGB
`u16` path on the decoded frame, which scales each sample to the full `u16` range
(`val * 65535 / ((1 << bit_depth) − 1)`) instead of truncating:

```rust
use heic::DecoderConfig;

let frame = DecoderConfig::new().decode_to_frame(&data)?; // raw YCbCr, native depth
let rgba16: Vec<u16> = frame.to_rgba16()?;                // 4×u16 per pixel, precision kept
// frame.to_rgb16() for 3×u16. Cropping (conformance window) is applied.
// Still no EOTF — branch on frame.transfer_characteristics (16=PQ, 18=HLG) to
// apply the right transfer + tone-map for your output.
let (w, h) = (frame.cropped_width(), frame.cropped_height());
```

`decode_to_frame` does not take `with_limits` / `with_stop`; if you need limits or
cancellation on a 16-bit path, gate on `ImageInfo::from_bytes` first.

### Errors (for a server)

Decode methods return `Result<_, whereat::At<HeicError>>` — the `At` wrapper
records the source location for your logs (`format!("{e}")`). Borrow the inner
error with `e.error()` (or own it with `e.decompose().0`) and match `HeicError`
to pick an HTTP status. `HeicError` is `#[non_exhaustive]`, so keep a wildcard arm:

```rust
use heic::{DecoderConfig, PixelLayout, HeicError};

let status = match DecoderConfig::new().decode(&data, PixelLayout::Rgba8) {
    Ok(_output) => 200,
    Err(e) => match e.error() {
        HeicError::LimitExceeded(_) | HeicError::OutOfMemory => 413, // Payload Too Large
        HeicError::Unsupported(_) | HeicError::UnsupportedCodec(_) => 415, // Unsupported Media Type
        HeicError::Cancelled(_) => 499, // client closed request
        HeicError::NoBackendSelected => 500, // build misconfigured — enable a `backend-*` feature
        // malformed input: InvalidContainer, InvalidData, NoPrimaryImage, HevcDecode(_), ...
        _ => 400, // Bad Request
    },
};
```

### Probe without decoding

```rust
use heic::ImageInfo;

let info = ImageInfo::from_bytes(&data)?;
println!("{}x{}, bit_depth={}, alpha={}, exif={}",
    info.width, info.height, info.bit_depth, info.has_alpha, info.has_exif);
// CICP color info also available:
// info.color_primaries, info.transfer_characteristics, info.matrix_coefficients, info.video_full_range
```

### Zero-copy into pre-allocated buffer

```rust
let info = ImageInfo::from_bytes(&data)?;
let mut buf = vec![0u8; info.output_buffer_size(PixelLayout::Rgba8).unwrap()];
let (w, h) = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .decode_into(&mut buf)?;
```

For grid images (most iPhone photos), `decode_into` streams tile color conversion directly into the output buffer, avoiding the intermediate full-frame YCbCr allocation.

### Metadata extraction

```rust
use std::borrow::Cow;

let decoder = DecoderConfig::new();
// Zero-copy for single-extent items, owned for multi-extent
let exif: Option<Cow<'_, [u8]>> = decoder.extract_exif(&data)?;  // raw TIFF bytes
let xmp: Option<Cow<'_, [u8]>> = decoder.extract_xmp(&data)?;    // raw XML bytes
let icc: Option<Vec<u8>> = decoder.extract_icc(&data)?;           // ICC profile bytes
let thumb = decoder.decode_thumbnail(&data, PixelLayout::Rgb8)?;  // smaller preview
```

### HDR gain map

```rust
let gainmap = DecoderConfig::new().decode_gain_map(&data)?;
// gainmap.data: Vec<u8> (8-bit grayscale gain map pixels), gainmap.width, gainmap.height
// Apply Apple HDR reconstruction:
//   sdr_linear = sRGB_EOTF(sdr_pixel)
//   gain_linear = sRGB_EOTF(gainmap_pixel)
//   scale = 1.0 + (headroom - 1.0) * gain_linear
//   hdr_linear = sdr_linear * scale
```

## Performance


SIMD-accelerated on x86-64 (AVX2 for color conversion, IDCT 8/16/32, residual add, dequantize; SSE4.1 for IDST 4x4) and AArch64 (NEON for color conversion, IDCT 8/16/32, IDST 4x4). Scalar fallback when SIMD is unavailable.

## Dependencies

4 runtime crates (default features), none with C/FFI:

```
heic
├── archmage          — SIMD dispatch via CPU feature tokens
│   └── safe_unaligned_simd  — safe wrappers over std::arch intrinsics
├── enough            — cooperative cancellation (0 unsafe)
├── safe_unaligned_simd
└── whereat           — error location tracking (deny(unsafe_code))
```

With `parallel`: adds rayon + crossbeam (6 more crates, all pure Rust).
With `av1`: adds rav1d-safe (pure Rust AV1 decoder with archmage SIMD).
With `unci`: adds zenflate (pure Rust DEFLATE/zlib).

## Memory

Use `DecoderConfig::estimate_memory(width, height, layout)` (probe dimensions first with `ImageInfo::from_bytes`) to size the decode before committing memory. `decode_into()` uses a streaming path for grid images that reduces peak memory by ~60% compared to `decode()`.

## Security

Designed for untrusted input. All decode paths enforce resource limits before allocation, not after.

- **`#![forbid(unsafe_code)]`** — the entire crate, including SIMD dispatch via archmage
- **Pre-decode dimension checks** — `Limits` (max_width, max_height, max_pixels, max_memory_bytes) checked against HEIF ispe box dimensions before any codec allocates frame buffers
- **AV1 frame_size_limit** — `limits.max_pixels` fed directly into rav1d-safe's `Settings::frame_size_limit`, rejecting oversized frames during OBU parsing before pixel buffer allocation
- **Decompression bomb protection** — unci decompressor capped at `min(512 MiB, limits.max_memory_bytes)` with expected size validated against declared dimensions
- **Cooperative cancellation** — `enough::Stop` tokens checked in all decode loops (per-CTU for HEVC, per-row for unci, per-tile for grids, inside rav1d, inside zenflate decompression)
- **Checked arithmetic** — all dimension, offset, and size calculations use `checked_mul`/`checked_add` with explicit error returns
- **Fallible allocation** — `try_reserve` / `try_vec!` throughout; OOM returns an error, not a panic
- **Container parser hardening** — resource limits on item count (64K), property count (64K), extents per item (1K), references (64K), sample table entries (1M), string lengths (4K), NAL unit size (16 MiB), ICC profile size (4 MiB)
- **Fuzz targets** — 5 libfuzzer targets covering decode, decode-with-limits, probe, AV1 decode, and unci decode

Use `DecoderConfig::estimate_memory(width, height, layout)` to check memory requirements before decoding. Pass `Limits` to reject files that exceed your resource budget. Pass an `enough::Stop` token for cooperative cancellation of long-running decodes.

## Patents

HEVC (H.265) and its use in HEIF containers may be covered by patents
held by third parties, including (but not limited to) members of the
[Access Advance](https://accessadvance.com/) HEVC patent pool. This
decoder may or may not be subject to those patents depending on your
jurisdiction, use case, and distribution model.

**Imazen holds no patents** — only copyrights on the code we wrote.
The dual license (AGPL-3.0 / commercial) grants **copyright** permissions
for the implementation in this repository. It does **not** grant, and
cannot grant, any rights under third-party patents.

This codec is **decode-only**. If you ship this crate in a product,
you may need to determine whether a patent license is required for your
use and obtain one if so. We are not lawyers; consult a patent attorney
for legal advice.

## License

Dual-licensed: [AGPL-3.0](https://github.com/imazen/heic/blob/main/LICENSE-AGPL3) or [commercial](https://github.com/imazen/heic/blob/main/LICENSE-COMMERCIAL).

I've maintained and developed open-source image server software — and the 40+
library ecosystem it depends on — full-time since 2011. Fifteen years of
continual maintenance, backwards compatibility, support, and the (very rare)
security patch. That kind of stability requires sustainable funding, and
dual-licensing is how we make it work without venture capital or rug-pulls.
Support sustainable and secure software; swap patch tuesday for patch leap-year.

[Our open-source products](https://www.imazen.io/open-source)

**Your options:**

- **Startup license** — $1 if your company has under $1M revenue and fewer
  than 5 employees. [Get a key →](https://www.imazen.io/pricing)
- **Commercial subscription** — Governed by the Imazen Site-wide Subscription
  License v1.1 or later. Apache 2.0-like terms, no source-sharing requirement.
  Sliding scale by company size.
  [Pricing & 60-day free trial →](https://www.imazen.io/pricing)
- **AGPL v3** — Free and open. Share your source if you distribute.

See [LICENSE-COMMERCIAL](https://github.com/imazen/heic/blob/main/LICENSE-COMMERCIAL) for details.

## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.

## Image tech I maintain

| | |
|:--|:--|
| **Codecs** ¹ | [zenjpeg] · [zenpng] · [zenwebp] · [zengif] · [zenavif] · [zenjxl] · [zenbitmaps] · **heic** · [zentiff] · [zenpdf] · [zensvg] · [zenjp2] · [zenraw] · [ultrahdr] |
| Codec internals | [zenjxl-decoder] · [jxl-encoder] · [zenrav1e] · [rav1d-safe] · [zenavif-parse] · [zenavif-serialize] |
| Compression | [zenflate] · [zenzop] · [zenzstd] |
| Processing | [zenresize] · [zenquant] · [zenblend] · [zenfilters] · [zensally] · [zentone] |
| Pixels & color | [zenpixels] · [zenpixels-convert] · [linear-srgb] · [garb] |
| Pipeline & framework | [zenpipe] · [zencodec] · [zencodecs] · [zenlayout] · [zennode] · [zenwasm] · [zentract] |
| Metrics | [zensim] · [fast-ssim2] · [butteraugli] · [zenmetrics] · [resamplescope-rs] |
| Pickers & ML | [zenanalyze] · [zenpredict] · [zenpicker] |
| Products | [Imageflow] image engine ([.NET][imageflow-dotnet] · [Node][imageflow-node] · [Go][imageflow-go]) · [Imageflow Server] · [ImageResizer] (C#) |

<sub>¹ pure-Rust, `#![forbid(unsafe_code)]` codecs, as of 2026</sub>

### General Rust awesomeness

[zenbench] · [archmage] · [magetypes] · [enough] · [whereat] · [cargo-copter]

[Open source](https://www.imazen.io/open-source) · [@imazen](https://github.com/imazen) · [@lilith](https://github.com/lilith) · [lib.rs/~lilith](https://lib.rs/~lilith)

[zenjpeg]: https://github.com/imazen/zenjpeg
[zenpng]: https://github.com/imazen/zenpng
[zenwebp]: https://github.com/imazen/zenwebp
[zengif]: https://github.com/imazen/zengif
[zenavif]: https://github.com/imazen/zenavif
[zenjxl]: https://github.com/imazen/zenjxl
[zenbitmaps]: https://github.com/imazen/zenbitmaps
[zentiff]: https://github.com/imazen/zentiff
[zenpdf]: https://github.com/imazen/zenpdf
[zensvg]: https://github.com/imazen/zenextras
[zenjp2]: https://github.com/imazen/zenextras
[zenraw]: https://github.com/imazen/zenraw
[ultrahdr]: https://github.com/imazen/ultrahdr
[zenjxl-decoder]: https://github.com/imazen/zenjxl-decoder
[jxl-encoder]: https://github.com/imazen/jxl-encoder
[zenrav1e]: https://github.com/imazen/zenrav1e
[rav1d-safe]: https://github.com/imazen/rav1d-safe
[zenavif-parse]: https://github.com/imazen/zenavif-parse
[zenavif-serialize]: https://github.com/imazen/zenavif-serialize
[zenflate]: https://github.com/imazen/zenflate
[zenzop]: https://github.com/imazen/zenzop
[zenzstd]: https://github.com/imazen/zenzstd
[zenresize]: https://github.com/imazen/zenresize
[zenquant]: https://github.com/imazen/zenquant
[zenblend]: https://github.com/imazen/zenblend
[zenfilters]: https://github.com/imazen/zenfilters
[zensally]: https://github.com/imazen/zensally
[zentone]: https://github.com/imazen/zentone
[zenpixels]: https://github.com/imazen/zenpixels
[zenpixels-convert]: https://github.com/imazen/zenpixels
[linear-srgb]: https://github.com/imazen/linear-srgb
[garb]: https://github.com/imazen/garb
[zenpipe]: https://github.com/imazen/zenpipe
[zencodec]: https://github.com/imazen/zencodec
[zencodecs]: https://github.com/imazen/zencodecs
[zenlayout]: https://github.com/imazen/zenlayout
[zennode]: https://github.com/imazen/zennode
[zenwasm]: https://github.com/imazen/zenwasm
[zentract]: https://github.com/imazen/zentract
[zensim]: https://github.com/imazen/zensim
[fast-ssim2]: https://github.com/imazen/fast-ssim2
[butteraugli]: https://github.com/imazen/butteraugli
[zenmetrics]: https://github.com/imazen/zenmetrics
[resamplescope-rs]: https://github.com/imazen/resamplescope-rs
[zenanalyze]: https://github.com/imazen/zenanalyze
[zenpredict]: https://github.com/imazen/zenanalyze
[zenpicker]: https://github.com/imazen/zenanalyze
[zenbench]: https://github.com/imazen/zenbench
[archmage]: https://github.com/imazen/archmage
[magetypes]: https://github.com/imazen/archmage
[enough]: https://github.com/imazen/enough
[whereat]: https://github.com/lilith/whereat
[cargo-copter]: https://github.com/imazen/cargo-copter
[Imageflow]: https://github.com/imazen/imageflow
[Imageflow Server]: https://github.com/imazen/imageflow-dotnet-server
[ImageResizer]: https://github.com/imazen/resizer
[imageflow-dotnet]: https://github.com/imazen/imageflow-dotnet
[imageflow-node]: https://github.com/imazen/imageflow-node
[imageflow-go]: https://github.com/imazen/imageflow-go
