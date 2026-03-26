# heic-decoder

[![Crates.io](https://img.shields.io/crates/v/heic-decoder.svg)](https://crates.io/crates/heic-decoder)
[![Documentation](https://docs.rs/heic-decoder/badge.svg)](https://docs.rs/heic-decoder)
[![License: AGPL-3.0](https://img.shields.io/badge/license-AGPL--3.0-blue.svg)](LICENSE)

Pure Rust HEIC/HEIF image decoder. No C/C++ dependencies, no unsafe code, 4 runtime crates.

- `#![forbid(unsafe_code)]` — zero unsafe blocks in the entire codebase
- `no_std + alloc` compatible (compiles for wasm32-unknown-unknown)
- AVX2/SSE4.1 SIMD acceleration with automatic scalar fallback
- Optional tile-parallel decoding via rayon

## Status

Decodes most HEIC files from iPhones and cameras. 103/162 test files decode successfully. Best quality: 77.3 dB PSNR (BT.709), 73% pixel-exact on example.heic.

### What works
- HEIF container parsing (ISOBMFF boxes, grid images, overlays, identity-derived items)
- Full HEVC I-frame decoding (VPS/SPS/PPS, CABAC, intra prediction, transforms)
- Deblocking filter and SAO (Sample Adaptive Offset)
- YCbCr→RGB with BT.601/BT.709/BT.2020 matrices (full + limited range)
- CICP color info from HEVC VUI and HEIF colr nclx (container overrides codec)
- 10-bit HEVC with transparent 8-bit downconvert or 16-bit output preservation
- Alpha plane decoding from auxiliary images
- HDR gain map extraction (Apple `urn:com:apple:photo:2020:aux:hdrgainmap`)
- EXIF, XMP, and ICC profile extraction (zero-copy where possible)
- Thumbnail decode, image rotation/mirror transforms (ipma ordering)
- HEVC scaling lists (custom dequantization matrices)
- AVX2 SIMD: color conversion, IDCT 8/16/32, residual add, dequantize
- SSE4.1 SIMD: IDST 4x4
- Tile-parallel grid decoding via rayon (`parallel` feature)

### Known limitations
- I-slices only (sufficient for HEIC still images; no inter prediction for video)
- PQ/HLG transfer functions: parsed and exposed via `ImageInfo`, but no EOTF applied — callers handle tone-mapping

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `std` | yes | Standard library support. Disable for `no_std + alloc`. |
| `parallel` | no | Parallel tile decoding via rayon. Implies `std`. |

## Usage

```rust
use heic_decoder::{DecoderConfig, PixelLayout};

let data = std::fs::read("image.heic")?;
let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;
println!("{}x{} image, {} bytes", output.width, output.height, output.data.len());
```

### Limits and cancellation

```rust
use heic_decoder::{DecoderConfig, PixelLayout, Limits};

let mut limits = Limits::default();
limits.max_width = Some(8192);
limits.max_height = Some(8192);
limits.max_pixels = Some(64_000_000);
limits.max_memory_bytes = Some(512 * 1024 * 1024);

let output = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .with_limits(&limits)
    .decode()?;
```

### Probe without decoding

```rust
use heic_decoder::ImageInfo;

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
// gainmap.data: Vec<f32> (normalized 0.0–1.0), gainmap.width, gainmap.height
// Apply Apple HDR reconstruction:
//   sdr_linear = sRGB_EOTF(sdr_pixel)
//   gain_linear = sRGB_EOTF(gainmap_pixel)
//   scale = 1.0 + (headroom - 1.0) * gain_linear
//   hdr_linear = sdr_linear * scale
```

## Performance

Benchmarked on AMD Ryzen 9 7950X, WSL2, Rust 1.93, release profile (thin LTO, codegen-units=1).

| Image | Time |
|-------|------|
| 1280x854 (single tile) | 54 ms |
| 3024x4032 (48-tile, sequential) | 451 ms |
| 3024x4032 (48-tile, `parallel`) | 180 ms |
| Probe (metadata only) | 1.3 µs |
| EXIF extraction | 4.4 µs |

SIMD-accelerated on x86-64 (AVX2 for color conversion, IDCT 8/16/32, residual add, dequantize; SSE4.1 for IDST 4x4). Scalar fallback on other architectures.

## Dependencies

4 runtime crates (default features), none with C/FFI:

```
heic-decoder
├── archmage          — SIMD dispatch via CPU feature tokens
│   └── safe_unaligned_simd  — safe wrappers over std::arch intrinsics
├── enough            — cooperative cancellation (0 unsafe)
├── safe_unaligned_simd
└── whereat           — error location tracking (deny(unsafe_code))
```

With `parallel`: adds rayon + crossbeam (6 more crates, all pure Rust).

## Memory

Use `DecoderConfig::estimate_memory()` to check memory requirements before decoding. `decode_into()` uses a streaming path for grid images that reduces peak memory by ~60% compared to `decode()`.

## License

Dual-licensed: [AGPL-3.0](LICENSE-AGPL3) or [commercial](LICENSE-COMMERCIAL).

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

See [LICENSE-COMMERCIAL](LICENSE-COMMERCIAL) for details.
## AI-Generated Code Notice

Developed with Claude (Anthropic). Not all code manually reviewed. Review critical paths before production use.
