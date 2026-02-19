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

## API Design Guidelines

Follow `/home/lilith/work/codec-design/README.md` for codec API design patterns:

### Decoder Design Principles
- **Layered API**: Simple one-shot functions + builder for advanced use
- **Info before decode**: Allow inspection without full decode
- **Zero-copy decode_into**: For performance-critical paths
- **Multiple output formats**: RGBA, RGB, YUV, etc.

### Example API Shape (future)
```rust
// Simple one-shot
pub fn decode_rgba(data: &[u8]) -> Result<(Vec<u8>, u32, u32)>;

// Typed pixel output
pub fn decode<P: DecodePixel>(data: &[u8]) -> Result<(Vec<P>, u32, u32)>;

// Builder for advanced options
pub struct Decoder<'a> { ... }
impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Result<Self>;
    pub fn info(&self) -> &ImageInfo;
    pub fn decode_rgba(self) -> Result<ImgVec<RGBA8>>;
}

// Zero-copy into pre-allocated buffer
pub fn decode_rgba_into(
    data: &[u8],
    output: &mut [u8],
    stride_bytes: u32
) -> Result<(u32, u32)>;
```

### Essential Crates
- `rgb` - Typed pixel structs (RGB8, RGBA8, etc.)
- `imgref` - Strided 2D image views
- `bytemuck` - Safe transmute for SIMD

### SIMD Strategy
- Use `wide` crate (1.1.1) for portable SIMD types
- Use `multiversed` for runtime dispatch
- Place dispatch at high level, `#[inline(always)]` for inner loops
- See codec-design README for archmage usage in complex operations

## Code Style

- Use `div_ceil()` instead of `(x + n - 1) / n`
- Use `is_multiple_of()` instead of `x % n == 0`
- Collapse nested `if` with `&&` when possible
- Use iterators with `.enumerate()` instead of manual counters

## Current Implementation Status

### Completed
- HEIF container parsing (boxes.rs, parser.rs)
- NAL unit parsing (bitstream.rs)
- VPS/SPS/PPS parsing (params.rs)
- Slice header parsing (slice.rs)
- CTU/CU quad-tree decoding structure (ctu.rs)
- Intra prediction modes with TU-level ordering (intra.rs)
- Reference sample filtering (H.265 8.4.4.2.3)
- Reference sample substitution with forward propagation (H.265 8.4.4.2.2)
- Transform matrices and inverse DCT/DST (transform.rs)
- CABAC tables and decoder framework (cabac.rs) — bit-exact with libde265
- Frame buffer with YCbCr→RGB conversion (picture.rs)
- Transform coefficient parsing via CABAC (residual.rs)
- Adaptive Golomb-Rice coefficient decoding
- DC coefficient inference for coded sub-blocks
- Sign data hiding (all 280 CTUs decode)
- Debug infrastructure (debug.rs) with CABAC tracker
- sig_coeff_flag proper H.265 context derivation
- Conformance window cropping (to_rgb/to_rgba apply SPS conf_win_offset)

### Current Quality (vs libde265, no deblocking/SAO)
- Y: avg_diff=1.14, PSNR=41.0 dB, 49% exact pixels
- Cb: avg_diff=0.13, PSNR=56.3 dB, 88% exact
- Cr: avg_diff=0.16, PSNR=55.5 dB, 84% exact
- All 247,215 CABAC SEs match perfectly
- Remaining ±1 errors from minor rounding differences in prediction

### Pending
- Deblocking filter
- SAO (Sample Adaptive Offset)
- SIMD optimization
- Fix remaining ±1 rounding errors in prediction

## Known Limitations

- Only I-slices supported (sufficient for HEIC still images)
- No inter prediction (P/B slices)

## Known Bugs

### ±1 Rounding Errors in Prediction
- First error at pixel (277,0): ref=180, ours=179
- All early errors are exactly ±1
- Errors propagate and accumulate across the image (max=2 in first CTU row, max=124 by row 704)
- All prediction formulas (Planar, DC, Angular, boundary filters) verified correct against spec
- Likely from minor rounding differences in filter application or clipping order
- Deblocking filter and SAO not yet implemented (will affect final comparison)

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
    ├── debug.rs     # CABAC tracker, invariant checks
    └── picture.rs   # Frame buffer
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
