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
let (w, h) = DecoderConfig::new()
    .decode_request(&data)
    .with_output_layout(PixelLayout::Rgba8)
    .decode_into(&mut buf)?; // returns (width, height)

// Probe without decoding
let info = ImageInfo::from_bytes(&data)?;

// Raw YCbCr access
let frame = DecoderConfig::new().decode_to_frame(&data)?;

// HDR gain map
let gainmap = DecoderConfig::new().decode_gain_map(&data)?;

// EXIF/XMP extraction (zero-copy for single-extent, owned for multi-extent)
let exif: Option<Cow<'_, [u8]>> = DecoderConfig::new().extract_exif(&data)?;
let xmp: Option<Cow<'_, [u8]>> = DecoderConfig::new().extract_xmp(&data)?;

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
- `archmage` — SIMD dispatch via CPU feature tokens
- `safe_unaligned_simd` — safe wrappers over std::arch intrinsics

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
- Deblocking filter (deblock.rs) — H.265 8.7.2, strong/weak luma + chroma, inter-aware bS with TB/PB edge distinction
- SAO filter (sao.rs) — H.265 8.7.3, band offset + edge offset
- Grid-based HEIC decoding (idat, iref/dimg, tile assembly)
- Alpha plane decoding from auxiliary images (auxl/auxC)
- HDR gain map extraction (Apple HDR aux format)
- Identity-derived (iden) and overlay (iovl) image types
- Image mirror (imir) with ordered transform application (ipma order)
- VUI color info parsing (video_full_range_flag, matrix_coefficients, color_primaries, transfer_characteristics)
- YCbCr→RGB with BT.601, BT.709, BT.2020 matrices (full + limited range)
- colr nclx box color info override from HEIF container (all 4 CICP fields)
- CICP propagation through zencodec adapter (TF/primaries on PixelDescriptor, with_cicp on ImageInfo)
- ICC profile extraction (extract_icc API)
- RowSink streaming decode (decode_rows API, grid-to-sink streaming)
- HEVC scaling list support (custom dequantization matrices from SPS/PPS)
- `#![forbid(unsafe_code)]` — zero unsafe blocks in codebase
- `no_std + alloc` support (compiles for wasm32-unknown-unknown)
- Integer overflow protection for dimension calculations
- Memory estimation before decode (DecoderConfig::estimate_memory)
- Hardened parser: checked arithmetic, resource limits, fallible allocation, Stop cancellation
- Multi-extent item support (get_item_data returns Cow: borrow single, concat multi)
- Parser defensive validation (clap zero-denom, ispe bounds, hvcc length_size, string/NAL/ICC caps)
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
- SIMD-accelerated IDCT 8x8/16x16/32x32 via archmage AVX2 (madd_epi16 butterfly)
- SIMD-accelerated IDST 4x4 via archmage SSE4.1 (3.77x vs scalar)
- SIMD residual add (u16+i16→clamped u16) and dequantize via archmage AVX2
- Tile-parallel grid decoding via rayon (optional `parallel` feature)
- PCM mode support (H.265 7.3.8.8) — raw sample read + CABAC reinit
- Tile-aware CABAC context derivation (split_cu_flag, cu_skip_flag, SAO merge, intra MPM)
- Tile boundary QP prediction reset (H.265 8.6.1) and context/StatCoeff reinit
- HEIF image sequence (msf1/moov) support: moov/trak/stbl parsing, synthetic Item creation, all 11 Nokia C026-C041 decode

### Current Quality (RGB comparison vs libheif)
- 114/173 test files decode successfully (103 meta-based + 11 msf1 image sequences)
- Best: example_q95 65.7dB (98% pixel-exact), classic-car 77.3dB (BT.709)
- Nokia C001-C052: 50.5dB (77% pixel-exact)
- Grid images: image1 50.4dB, classic-car 77.3dB
- Scaling list files: iphone_rotated 55.3dB (91% exact), iphone_telephoto 50.9dB
- All CABAC SEs match libde265 perfectly
- YUV-level: pixel-perfect for q50+ (76.1dB for q10, 128 Y-plane diffs vs dec265)
- Color conversion: ×8192 fixed-point for limited-range, ×256 for full-range
- example.heic: 73.0% pixel-exact, SSIM2 91.86, avg diff 0.45, max diff 12

### Known Edge Cases
- MIAF003 (4:4:4 chroma, RExt profile): 61.9dB (97.8% exact, max diff 4)
- overlay_1000x680: 13.1dB — remaining diff from color conversion on fill regions
- example_q10: 36.1dB RGB — low-QP amplifies color conversion rounding

### Performance
- Release profile: thin LTO + codegen-units=1
- Criterion benchmarks: 54ms example.heic (1280x854), 451ms iPhone sequential (3024x4032), 180ms parallel
- Callgrind (iPhone, scalar under valgrind): 5090M instructions
- Key optimizations applied:
  - Plane-direct writes, in-place dequant, border fill inlining (731M→653M)
  - Partial butterfly IDCT for 8/16/32 (decode_and_apply_residual -14%)
  - SAO edge interior/border split + lazy plane cloning (SAO -26%)
  - Color conversion: 4:2:0 specialization, ×8192 fixed-point (to_rgb -38%)
  - Row-slice bounds-check elimination in intra prediction and residual add
  - SIMD color conversion via archmage AVX2 (81M → 9.2M, -88%)
  - SIMD IDCT 8x8/16x16/32x32 via archmage AVX2 (madd_epi16 butterfly)
  - SIMD IDST 4x4 via archmage SSE4.1 (14.1M → 3.7M, -73%)
  - SIMD residual add via archmage AVX2 (u16+i16→clamped u16)
  - SIMD dequantize via archmage AVX2 (flat scale only; scaled uses scalar)
  - Intra prediction: early-exit substitution, hoisted bounds, halved arrays
  - Deblocking: direct plane access with step_along/step_across
  - Residual buffer reuse across TU decode calls
  - Tile-parallel grid decode via rayon: 451ms → 180ms (2.5x) for 48-tile iPhone image
  - Streaming decode_into for grids: bypasses full-frame YCbCr, color-converts tiles directly to output
- Remaining hotspots: decode_and_apply_residual (32%), predict_intra (17%), CABAC (10%), memcpy/memset (7%)

## Known Limitations

- Inter prediction (P/B slices) on `inter-prediction` branch:
  - Full pipeline: syntax parsing, merge/AMVP/TMVP, scalar MC, DPB, VideoDecoder
  - Conformance: 48/48 vectors decode without crash, 1 pixel-exact (I-only)
  - CABAC verified BIT-EXACT vs dec265 (all 28 CTU byte positions match for MERGE_A)
  - MERGE_A: frames 1-7 small deblocking diffs (29-349 pixels, max_abs 2-6), frame 0 exact
  - SAO_B (3x1 tiles): 12.9dB (all frames decode, no UNINIT)
  - Fixed bugs:
    - Tile boundary CABAC context: split_cu_flag, cu_skip_flag, SAO merge left/up now check same-tile availability (matching libde265 6.4.1)
    - Tile QP reset: QP prediction, is_cu_qp_delta_coded, StatCoeff reset at tile boundaries
    - Intra MPM tile awareness: get_neighbor_intra_mode_left returns DC for cross-tile neighbors
    - PCM mode: pcm_flag decode via decode_terminate, raw sample read, CABAC reinit (H.265 7.3.8.8)
    - SAO merge slice check: uses actual slice_segment_address instead of hardcoded 0
    - interSplitFlag: missing forced TU split when max_transform_hierarchy_depth_inter==0 and PartMode!=2Nx2N (H.265 7.3.8.7). Caused CABAC desync in RQT_A B-frame.
    - Temporal MVP fallback: only tried one collocated position (bottom-right OR center), but H.265 8.5.3.2.8 requires trying bottom-right first, then falling back to center when collocated block is intra
    - Small PU L1 restriction: nPbW+nPbH==12 rule unconditionally disabled L1, but H.265 8.5.3.2.2 step 10 only disables L1 when both L0 and L1 are active (bi-prediction)
    - ref_idx decode: truncated unary consumed extra CABAC bin when num_active>=2 (CABAC desync)
    - AMVP: isScaledFlagLX, B-candidate always-run, same-list-first ordering
    - Temporal MVP: collocated MV selection (NoBackwardPredFlag, col_from_l0_flag), per-list derivation, collocated frame ref_poc stored in DPB
    - Earlier: CBF tracking for deblocking bS=2, DST/DCT for inter 4x4 TUs, chroma MC shifts (4→6), skip/no-residual CU boundary marking, WPP entry point offsets + CABAC reinit, cu_skip_flag cross-CTB-row context derivation, deblocking TB/PB edge distinction with separate bS derivation, bi-pred cross-list bS comparison, bi-pred H+V MC rounding offset
  - IMPORTANT: dec265 reference YUV is in DISPLAY ORDER (not decode order)
  - Deblock trace infrastructure: `enable_deblock_trace()` dumps all edge parameters to /tmp/our_deblock_trace.txt
  - Multi-slice: PictureMaps persistence across slices (ct_depth, intra modes, pred/mv, cbf, qp, sao)
  - Loop filters deferred to picture completion (prevents intra ref corruption in multi-slice)
  - Tile scan order: boundary detection, CABAC reinit at tile boundaries with entry point offsets
  - Remaining UNINIT vectors: TILES_A/B (complex tile CABAC desync within tiles), DELTAQP_A (PCM + complex content), DBLK_A/B (multi-slice inter desync), SDH_A (cu_qp_delta desync), MVDL1ZERO (scattered inter desync)
  - Deferred: SIMD MC (Phase 7), weighted prediction application
- 4:4:4 chroma: decodes correctly (61.9dB), but no SIMD color conversion path (uses scalar)
- Dependent slice segments: not supported (2 vectors fail)

## Known Bugs

(none)

## Investigation Notes

### UNINIT pixel triage (conformance vectors with negative PSNR)

10 vectors have UNINIT pixels (max_diff=65535 = UNINIT_SAMPLE sentinel).

**Vectors analyzed:**
- TILES_A/B: tiles_enabled=1 (5x5 non-uniform tiles), I-slices only. Tile CABAC reinit implemented.
- DELTAQP_A: cu_qp_delta_enabled=1, multi-slice (5 slices per I-frame). All frames UNINIT including frame 0 (I-frame).
- SDH_A: single-slice I+B. I-frame has 34% UNINIT, B-frame has 99% UNINIT.
- SAO_B: tiles=1 (3x1), B-slices
- RQT_A: FIXED — was missing interSplitFlag (H.265 7.3.8.7). Now 23.9dB (CABAC bit-exact, remaining diffs from deblock/SAO).
- CONFWIN_A: single-slice P/B. I-frame ~12dB (no UNINIT), later P/B frames get UNINIT.
- DBLK_A/B: multi-slice (4 slices per frame). ~12dB all frames, some have UNINIT.
- MVDL1ZERO_A: multi-slice, 500-frame sequence. Most frames ~12dB, 3 have UNINIT.

**Root cause analysis:**
- The dec265 CTU-CK checksum trace is ONLY printed for non-I slices (gated by `slice_type != I`), so earlier byte-position comparisons were between different frame types.
- MERGE_A (pixel-exact) has `cu_qp_delta_enabled=0`, `transform_skip_enabled=1`, QP=32.
- Failing I-frames (DELTAQP_A, SDH_A) have `cu_qp_delta_enabled=1` or different QP.
- The CABAC init formula and context tables match libde265 for the contexts checked (split_cu_flag, SAO, CBF).
- The CABAC engine init (range=510, value from first 2 bytes) matches libde265.
- Data offset computation verified correct for DELTAQP_A IDR slice (1 byte header → data_offset=1).
- First bin (split_cu_flag) verified manually: state=0,mps=0 at QP=26, LPS range=240, MPS threshold=34560, value=28334 → MPS (no split). This matches expected behavior for a flat-content first CTB.
- The UNINIT pixels are genuine (u16::MAX in Y plane), meaning some CTBs' pixels are never written. The decoder doesn't error out.

**Hypothesis:** The UNINIT appears to be from B/P frames referencing corrupted reference frames, not from I-frame decode failures. The I-frame first CTU decodes correctly (verified via SE trace). Need to compare more CTUs to find where decode diverges.

**Infrastructure built:**
- `PictureMaps` for cross-slice map persistence (ctu.rs)
- Deferred loop filter application in `finish_current_picture` (mod.rs)
- Tile boundary detection with CABAC reinit (ctu.rs)
- `compute_tile_boundaries`, `build_tile_scan_order`, `get_tile_id` helpers

## Module Structure

```
src/
├── lib.rs           # Public API types (DecoderConfig, DecodeRequest, Limits, etc.)
├── decode.rs        # Internal decode pipeline (grid, overlay, alpha, gain map, metadata)
├── error.rs         # Error types
├── auxiliary.rs     # Auxiliary image handling
├── codec.rs         # zencodec integration adapter
├── zennode_defs.rs  # zennode decode node definitions
├── heif/
│   ├── mod.rs
│   ├── boxes.rs     # ISOBMFF box definitions
│   └── parser.rs    # Container parsing
└── hevc/
    ├── mod.rs       # Main decode entry point
    ├── bitstream.rs # NAL unit parsing, BitstreamReader
    ├── params.rs    # VPS, SPS, PPS
    ├── slice.rs     # Slice header parsing (I/P/B)
    ├── ctu.rs       # CTU/CU decoding, SliceContext (intra + inter syntax)
    ├── intra.rs     # Intra prediction (35 modes)
    ├── inter.rs     # Inter prediction types, merge/AMVP candidate derivation
    ├── mc.rs        # Motion compensation (quarter-pel luma, eighth-pel chroma)
    ├── refpic.rs    # Reference picture set parsing, POC derivation, list construction
    ├── dpb.rs       # Decoded picture buffer management
    ├── cabac.rs     # CABAC decoder, context tables
    ├── residual.rs  # Transform coefficient parsing
    ├── transform.rs # Inverse DCT/DST (scalar + incant! dispatch)
    ├── transform_simd.rs # SIMD transforms: IDST 4x4, IDCT 8/16/32, residual add, dequantize
    ├── transform_simd_neon.rs # NEON SIMD transforms
    ├── color_convert.rs # YCbCr→RGB SIMD color conversion
    ├── color_convert_neon.rs # NEON color conversion
    ├── deblock.rs   # Deblocking filter (H.265 8.7.2, inter-aware bS)
    ├── sao.rs       # Sample Adaptive Offset (H.265 8.7.3)
    ├── debug.rs     # CABAC tracker, invariant checks
    ├── picture.rs   # Frame buffer, YCbCr→RGB conversion, deblock metadata
    └── transforms.rs # Spatial transforms: rotation (90/180/270) and mirror (H/V)
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
