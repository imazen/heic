# HEIC Decoder Project Instructions

## Project Overview

Pure Rust HEIC/HEIF image decoder. No C/C++ dependencies.

## Build Commands

```bash
cargo build
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Test Files

- `/home/lilith/work/heic/libheif/examples/example.heic` (1280x854)
- `/home/lilith/work/heic/test-images/classic-car-iphone12pro.heic` (3024x4032)

## Reference Implementations

- libde265 (C++): `/home/lilith/work/heic/libde265-src/`
- OpenHEVC (C): `/home/lilith/work/heic/openhevc-src/`

Do NOT use web searches for HEVC spec details - read the reference implementations directly.

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
- Intra prediction modes (intra.rs)
- Transform matrices and inverse DCT/DST (transform.rs)
- CABAC tables and decoder framework (cabac.rs)
- Frame buffer with YCbCr→RGB conversion (picture.rs)
- Transform coefficient parsing via CABAC (residual.rs)
- Adaptive Golomb-Rice coefficient decoding
- DC coefficient inference for coded sub-blocks
- Sign data hiding (partial: 269/280 CTUs decode)

### In Progress
- Sign data hiding edge case (11 CTUs fail with sign hiding enabled)
- Sub-block scan tables for 16x16 and 32x32 TUs

### Pending
- Conformance window cropping
- Deblocking filter
- SAO (Sample Adaptive Offset)
- SIMD optimization
- Clean up debug output

## Known Limitations

- Only I-slices supported (sufficient for HEIC still images)
- No inter prediction (P/B slices)
- Sub-block scan tables incomplete for TUs > 8x8
- sig_coeff_flag context is simplified (doesn't use neighbor info)
- coded_sub_block_flag context is simplified

## Known Bugs

- Sign data hiding causes CABAC desync at CTU 269 (11/280 CTUs fail)
  - With sign hiding enabled: 269/280 CTUs decode successfully
  - Without sign hiding: all 280 CTUs decode but with wrong chroma values
- Output dimensions 1280x856 vs reference 1280x854 (missing conformance window cropping)

## Investigation Notes

### Sign Data Hiding Progress (2026-01-21)

**Background:** HEVC has a "sign data hiding" feature (`sign_data_hiding_enabled_flag` in PPS)
that allows the encoder to infer one sign bit per 4x4 sub-block from coefficient parity.

**Fixes implemented:**
1. DC coefficient inference for coded sub-blocks (was decoding instead of inferring)
2. sig_coeff_flag decoding for position 15 in non-last sub-blocks (was skipping)
3. Sign decoding order matches libde265 (high scan pos to low)
4. Parity inference for hidden sign (sum & 1 flips sign)

**Progress:**
- Initially: CABAC desync at CTU 49 (49/280)
- After DC inference fix: CTU 161 (161/280)
- After position 15 fix: CTU 272 (272/280)
- After scan table investigation: CTU 269 (269/280)

**Remaining issue at CTU 269:**
- 11 CTUs near end of image fail to decode with sign hiding enabled
- Sign hiding disabled allows all 280 CTUs to decode
- The exact cause is not yet identified

**hevc-compare crate (crates/hevc-compare/):**
- Comparison crate for testing C++/Rust CABAC functions
- All basic CABAC tests pass (bypass decode, bypass bits, coeff_abs_level_remaining)
- Can be extended to test more coefficient decoding operations

### Chroma Bias Analysis (2026-01-21 Session 1)
- Test image: example.heic (1280x854)
- Y plane: avg=152 (reasonable for outdoor scene)
- Cb plane: avg=167 (should be ~128, ~39 too high)
- Cr plane: avg=209 (should be ~128, ~81 too high)
- First chroma block at (0,0) has values ~100-150 (reasonable)
- Bias is not uniform - some regions more affected than others
- Chroma QP = 17 (same as luma, PPS/slice offsets are 0)
- Diagonal scan tables have unconventional order but consistently so for both
  coefficient and sub-block scanning, suggesting they compensate for each other
- CTU column 0 chroma values are reasonable (avg ~128), bias appears starting at column 1+

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
    ├── transform.rs # Inverse DCT/DST
    └── picture.rs   # Frame buffer
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
