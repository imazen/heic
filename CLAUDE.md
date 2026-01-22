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
- All 280 CTUs decode successfully for example.heic

### In Progress
- Chroma plane accuracy (Cb=167, Cr=209 should be ~128)
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

- Chroma planes have wrong averages (167/209 vs ~128), causing wrong RGB colors
- Output dimensions 1280x856 vs reference 1280x854 (missing conformance window cropping)

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
