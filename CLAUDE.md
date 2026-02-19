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

// EXIF/XMP extraction (zero-copy from input buffer)
let exif: Option<&[u8]> = DecoderConfig::new().extract_exif(&data)?;
let xmp: Option<&[u8]> = DecoderConfig::new().extract_xmp(&data)?;

// Thumbnail decode (smaller embedded preview image)
let thumb: Option<DecodeOutput> = DecoderConfig::new().decode_thumbnail(&data, PixelLayout::Rgb8)?;
```

### Key Types
- `DecoderConfig` — HOW to decode (reusable, Clone)
- `DecodeRequest<'a>` — WHAT to decode (data + layout + limits + stop)
- `DecodeOutput` — decoded pixels (data + width + height + layout)
- `PixelLayout` — Rgb8, Rgba8, Bgr8, Bgra8
- `Limits` — max_width, max_height, max_pixels, max_memory_bytes
- `ImageInfo` — probe result (width, height, has_alpha, bit_depth, chroma_format, has_exif, has_xmp, has_thumbnail)
- `enough::Stop` — cooperative cancellation (re-exported)

### Dependencies
- `enough` — cooperative cancellation (Stop trait)
- `whereat` — error location tracking (At<E> wrapper)

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
- whereat error location tracking (At<HeicError> Result type)
- EXIF extraction (zero-copy, strips 4-byte HEIF prefix, returns raw TIFF)
- XMP extraction (zero-copy, returns raw XML from mime items)
- ImageInfo::from_bytes grid/iden/iovl probing (reads ispe + first tile hvcC)
- Thumbnail decode support (thmb references, decode_thumbnail API)
- Zero compiler warnings (clippy clean, all doc comments present)
- Criterion benchmarks (57ms RGB, 1.3µs probe, 4.4µs EXIF, 4.2ms thumbnail)
- 10-bit HEVC support (u16 planes, transparent downconvert to 8-bit output)
- SIMD-accelerated color conversion via archmage (AVX2 with scalar fallback)
- SIMD-accelerated IDCT 8x8/16x16 via archmage AVX2 (madd_epi16 butterfly)

### Current Quality (RGB comparison vs libheif)
- 104/162 test files decode successfully
- Best: example_q95 65.7dB (98% pixel-exact), classic-car 77.3dB (BT.709)
- Nokia C001-C052: 50.5dB (77% pixel-exact)
- Grid images: image1 50.4dB, classic-car 77.3dB
- Scaling list files: iphone_rotated 55.3dB (91% exact), iphone_telephoto 50.9dB
- All CABAC SEs match libde265 perfectly
- YUV-level: pixel-perfect for q50+ (76.1dB for q10, 128 Y-plane diffs vs dec265)
- Color conversion: ×8192 fixed-point for limited-range, ×256 for full-range
- example.heic: 73.0% pixel-exact, SSIM2 91.86, avg diff 0.45, max diff 12

### Known Edge Cases
- MIAF003 (4:4:4 chroma, RExt profile): 5.7dB — chroma format not fully supported
- overlay_1000x680: 13.1dB — remaining diff from color conversion on fill regions
- example_q10: 36.1dB RGB — low-QP amplifies color conversion rounding

### Performance
- Release profile: thin LTO + codegen-units=1
- Callgrind: 632M instructions for 1280x854 decode (progression: 731M → 653M → 717M → 645M → 632M)
- Key optimizations applied:
  - Plane-direct writes, in-place dequant, border fill inlining (731M→653M)
  - Partial butterfly IDCT for 8/16/32 (decode_and_apply_residual -14%)
  - SAO edge interior/border split + lazy plane cloning (SAO -26%)
  - Color conversion: 4:2:0 specialization, ×8192 fixed-point (to_rgb -38%)
  - Row-slice bounds-check elimination in intra prediction and residual add
  - SIMD color conversion via archmage AVX2 (81M → 9.2M, -88%)
  - SIMD IDCT 8x8/16x16 via archmage AVX2 (decode_and_apply_residual 227M → 211M)
- Remaining hotspots: decode_and_apply_residual (33%), predict_intra (16%), CABAC (9%), memcpy/memset (11%)

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
    ├── transform.rs # Inverse DCT/DST (scalar + incant! dispatch)
    ├── transform_simd.rs # AVX2 SIMD IDCT 8x8/16x16
    ├── deblock.rs   # Deblocking filter (H.265 8.7.2)
    ├── sao.rs       # Sample Adaptive Offset (H.265 8.7.3)
    ├── debug.rs     # CABAC tracker, invariant checks
    └── picture.rs   # Frame buffer (+ deblock metadata)
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
