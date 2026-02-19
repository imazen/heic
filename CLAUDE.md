# HEIC Decoder Project Instructions

## Project Overview

Pure Rust HEIC/HEIF image decoder. No C/C++ dependencies.

## Build Commands

```bash
cargo build
cargo clippy --all-targets --all-features -- -D warnings
cargo test
cargo test --test compare_reference -- --nocapture  # SSIM2 comparison
cargo test --test compare_reference write_comparison_images -- --nocapture --ignored  # Write PPMs
```

## Test Files

- `/home/lilith/work/heic/libheif/examples/example.heic` (1280x854)
- `/home/lilith/work/heic/test-images/classic-car-iphone12pro.heic` (3024x4032)

## Reference Implementations

- libde265 (C++): `/home/lilith/work/heic/libde265-src/`
- OpenHEVC (C): `/home/lilith/work/heic/openhevc-src/`

## HEVC Specification

**ITU-T H.265 (08/2021)** organized by decoder component:
- `/home/lilith/work/heic/spec/sections/README.md` - Index
- `/home/lilith/work/heic/spec/sections/09-decoding/03-slice-decoding.md` - Slice/CTU/CU decoding
- `/home/lilith/work/heic/spec/sections/10-parsing/cabac/` - CABAC context derivation
- Key sections for coefficient decode: 9.3.4.2.5 (sig_coeff_flag ctx), 9.3.4.2.6 (greater1_flag ctx)

Do NOT use web searches for HEVC spec details - read the spec sections or reference implementations directly.

## API Design

Follows the zen codec three-layer pattern from `/home/lilith/work/codec-design/README.md`:

```rust
// Simple one-shot
let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;

// Full control with limits and cancellation
let output = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .with_limits(&limits)
    .with_stop(&cancel)
    .decode()?;

// Zero-copy into pre-allocated buffer
let info = ImageInfo::from_bytes(&data)?;
let mut buf = vec![0u8; info.output_buffer_size(PixelLayout::Rgba8).unwrap()];
let info = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .decode_into(&mut buf)?;

// Probe without decoding
let info = ImageInfo::from_bytes(&data)?;

// Raw YCbCr access
let frame = DecoderConfig::new().decode_to_frame(&data)?;

// HDR gain map
let gainmap = DecoderConfig::new().decode_gain_map(&data)?;
```

### Key Types
- `DecoderConfig` — HOW to decode (reusable, Clone)
- `DecodeRequest<'a>` — WHAT to decode (data + layout + limits + stop)
- `DecodeOutput` — decoded pixels (data + width + height + layout)
- `PixelLayout` — Rgb8, Rgba8, Bgr8, Bgra8
- `Limits` — max_width, max_height, max_pixels, max_memory_bytes
- `ImageInfo` — probe result (width, height, has_alpha, bit_depth, chroma_format)
- `enough::Stop` — cooperative cancellation (re-exported)

### Dependencies
- `enough` — cooperative cancellation (Stop trait)

## Code Style

- Use `div_ceil()` instead of `(x + n - 1) / n`
- Use `is_multiple_of()` instead of `x % n == 0`
- Collapse nested `if` with `&&` when possible
- Use iterators with `.enumerate()` instead of manual counters

## Current Implementation Status

### Completed
- Zen codec API (DecoderConfig → DecodeRequest → decode)
- PixelLayout (Rgb8, Rgba8, Bgr8, Bgra8), Limits, Stop cancellation
- decode_into zero-copy, ImageInfo::from_bytes probing
- HEIF container parsing (boxes.rs, parser.rs)
- NAL unit parsing (bitstream.rs)
- VPS/SPS/PPS parsing (params.rs)
- Slice header parsing (slice.rs)
- CTU/CU quad-tree decoding structure (ctu.rs)
- Intra prediction modes with TU-level ordering (intra.rs)
- Reference sample filtering (H.265 8.4.4.2.3)
- Reference sample substitution with forward propagation (H.265 8.4.4.2.2)
- Transform matrices and inverse DCT/DST (transform.rs)
- Transform skip mode (H.265 8.6.4.1) — proper bypass of inverse transform
- CABAC tables and decoder framework (cabac.rs) — bit-exact with libde265
- Frame buffer with YCbCr→RGB conversion (picture.rs)
- Transform coefficient parsing via CABAC (residual.rs)
- Adaptive Golomb-Rice coefficient decoding
- DC coefficient inference for coded sub-blocks
- Sign data hiding (all 280 CTUs decode)
- Debug infrastructure (debug.rs) with CABAC tracker
- sig_coeff_flag proper H.265 context derivation
- Conformance window cropping (to_rgb/to_rgba apply SPS conf_win_offset)
- Deblocking filter (deblock.rs) — H.265 8.7.2, strong/weak luma + chroma
- SAO filter (sao.rs) — H.265 8.7.3, band offset + edge offset
- Grid-based HEIC decoding (idat, iref/dimg, tile assembly)
- Alpha plane decoding from auxiliary images (auxl/auxC)
- HDR gain map extraction (Apple HDR aux format)
- Identity-derived (iden) and overlay (iovl) image types
- Image mirror (imir) with ordered transform application (ipma order)
- VUI color info parsing (video_full_range_flag, matrix_coefficients)
- YCbCr→RGB with BT.601, BT.709, BT.2020 matrices (full + limited range)
- colr nclx box color info override from HEIF container
- HEVC scaling list support (custom dequantization matrices from SPS/PPS)
- `#![forbid(unsafe_code)]` — zero unsafe blocks in codebase
- `no_std + alloc` support (compiles for wasm32-unknown-unknown)
- Integer overflow protection for dimension calculations
- Memory estimation before decode (DecoderConfig::estimate_memory)
- Hardened parser: checked arithmetic throughout, 16M fuzz runs clean
- cargo-fuzz targets: decode, decode_limits, probe

### Current Quality (RGB comparison vs libheif)
- 103/162 test files decode successfully
- Best: example_q95 65.7dB (98% pixel-exact), classic-car 77.3dB (BT.709)
- Nokia C001-C052: 50.5dB (77% pixel-exact)
- Grid images: image1 50.4dB, classic-car 77.3dB
- Scaling list files: iphone_rotated 55.3dB (91% exact), iphone_telephoto 50.9dB
- All CABAC SEs match libde265 perfectly
- YUV-level: pixel-perfect for q50+ (76.1dB for q10, 128 Y-plane diffs vs dec265)
- Remaining RGB PSNR gap from fixed-point vs float color conversion rounding

### Known Edge Cases
- MIAF003 (4:4:4 chroma, RExt profile): 5.7dB — chroma format not fully supported
- overlay_1000x680: 13.1dB — remaining diff from color conversion on fill regions
- example_q10: 36.1dB RGB — low-QP amplifies color conversion rounding

### Pending
- SIMD optimization
- whereat error location tracking

## Known Limitations

- Only I-slices supported (sufficient for HEIC still images)
- No inter prediction (P/B slices)
- 4:4:4 chroma format partially supported (SAO clamped, but decode artifacts remain)

## Known Bugs

(none)

## Module Structure

```
src/
├── lib.rs           # Public API
├── error.rs         # Error types
├── heif/
│   ├── mod.rs
│   ├── boxes.rs     # ISOBMFF box definitions
│   └── parser.rs    # Container parsing
└── hevc/
    ├── mod.rs       # Main decode entry point
    ├── bitstream.rs # NAL unit parsing, BitstreamReader
    ├── params.rs    # VPS, SPS, PPS
    ├── slice.rs     # Slice header parsing
    ├── ctu.rs       # CTU/CU decoding, SliceContext
    ├── intra.rs     # Intra prediction (35 modes)
    ├── cabac.rs     # CABAC decoder, context tables
    ├── residual.rs  # Transform coefficient parsing
    ├── transform.rs # Inverse DCT/DST
    ├── deblock.rs   # Deblocking filter (H.265 8.7.2)
    ├── sao.rs       # Sample Adaptive Offset (H.265 8.7.3)
    ├── debug.rs     # CABAC tracker, invariant checks
    └── picture.rs   # Frame buffer (+ deblock metadata)
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
