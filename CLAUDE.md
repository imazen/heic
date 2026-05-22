# HEIC Decoder Project Instructions

## ABSOLUTE MANDATE: NEVER STOP PUSHING FORWARD

**While working on a multi-step task in this repo, you do not stop, pause, lazily
ask "shall I continue?", or wrap up the session early — unless one of the
genuine stop conditions in `~/.claude/CLAUDE.md` ("NEVER PAUSE LAZILY") fires.**
The default is: ship the next chunk. Then the next. Then the next.

**Stopping criteria — these are the ONLY legitimate reasons to pause:**

1. The next step is **destructive or externally visible** (force-push, publish,
   posting outside this org, deleting shared state) — confirm per the global
   "Executing actions with care" rules.
2. The next step **branches in a way that's not derivable from prior
   instructions** — two equally-valid paths with no expressed preference.
3. You hit a **measured-and-verified** blocker (no GPU, no Apple HW, missing
   library version not on crates.io) and there is no chunk you can ship
   that doesn't depend on that block.
4. Native-API runtime tests across all six backend targets pass, AND every
   item on the user's last list is shipped or has a documented chunk that
   cannot be shipped today.

**Forbidden — these are NOT stop conditions:**

* "I've done a lot already, let me summarize."
* "Want me to keep going?"
* "Should I continue with X, or hold?"
* "The next step is heavy."
* "Compile takes a while."
* "CI is yellow on one job."
* "I'm not sure if you want me to..."
* End-of-session-feeling because the conversation is long.

If a chunk is genuinely too large to ship in one pass, decompose into the
smallest demoable chunk (per the global "NEVER GIVE UP ON A USER-DIRECTED
LIST" rule), land that chunk with a passing test, document the next chunk
with file paths + signatures, and **keep going on the next chunk** in the
same turn. Do not stop to ask.

## Writing Good Code — patterns imported from jxl-encoder

These patterns are mandatory reading and apply to every commit. Adapted from
`~/work/zen/jxl-encoder/CLAUDE.md` "Patterns of Mistakes to Avoid" + "Proof-
by-Tests Investigation Methodology" + "Invariant Preservation Across Sessions".

### 1. False positives are the highest-severity bug

Tests that pass without exercising the thing they claim to test are worse
than no tests — they manufacture false confidence and waste future
investigation time. For a decoder, "the parser accepted the bytes" is
**not** evidence "the image decoded correctly" — you must call all the
way through `decode_to_frame` / `to_rgba` / the backend's `decode_hevc`
and inspect actual pixels. The `mf_diff` example + zensim-regress corpus
diff are the canonical templates for this codebase.

Rules:
- **Never** declare a backend "works" based on `is_available()` alone.
  Decode example.heic via the backend and verify dimensions + pixel
  equivalence (zensim ≥ 95) against the rust backend.
- **Never** trust a test-count delta. Verify what the tests actually do.
- When fixing a "tests pass but it's wrong" bug, audit every other test
  that uses the same pattern.

### 2. Read existing docs before investigating

Before "investigating" any bug, read:

1. This file's "Known Bugs" and "Investigation Notes" sections.
2. `CHANGELOG.md` `[Unreleased]` and the most recent shipped version.
3. `git log --since="3 days ago" --oneline -30`.
4. `git log --grep="<error fragment>" --all`.
5. The relevant backend's `PORTING.md` if applicable.

If the bug is already documented, continue the existing investigation —
do not start a new note. Update in place.

### 3. Test the test infrastructure

When you add a test helper (corpus harness, fake fixtures, diff utility),
exercise it against known-good and known-bad inputs before relying on it.
A test helper bug is worse than the bug it's trying to catch — it
poisons every test built on top.

### 4. Documentation reflects what's verified, not what's intended

Before claiming a feature "works" in CLAUDE.md, README, CHANGELOG, or a
commit message:

- Decode a real HEIC end-to-end via the path being claimed.
- Compare against libheif / the rust backend / dec265 — at least two
  external sources of truth.
- For native backends, the bar is `compare_backends_via_zensim` reporting
  zero failed files in the bundled corpus.

Status markers (used in CLAUDE.md + CHANGELOG):

| Marker | Meaning |
|---|---|
| ✓ Complete | Works end-to-end, ≥ 2 cross-checks, runtime CI green. |
| ⚠ Partial | Some inputs work, others fail; failure mode documented. |
| ⚙ In Progress | Implementation exists, not yet exercised against real input. |
| ✗ Broken | Implementation exists, known-failing test pinned. |
| ❌ Not Started | No implementation. |

### 5. One commit, one complete fix

Multiple `fix: correct X` commits for the same `X` within a day means the
first fix was shipped without understanding. Before fixing a bug:

- Trace every consumer of the wrong data.
- Write a failing test that reproduces it.
- Understand **why** the bug exists, not just where.

After: verify with a different code path (e.g. the MF backend fix
verified via both `mediafoundation_alone_decodes_when_required` and the
corpus zensim diff).

### 6. Investigation lives in ONE place

CLAUDE.md "Investigation Notes" is the single source of truth. Do NOT
create `STATUS.md`, `NOTES.md`, `INVESTIGATION-of-foo.md` files — use
dated entries in CLAUDE.md instead. Multiple symptoms (UnexpectedEof,
InvalidEnum, byte corruption) may share a root cause; link related
findings instead of duplicating them.

### 7. Read code before claiming you understand it

Before committing an implementation:

- Read it line by line. Verify variable names match semantics
  (`crop_y` vs `coded_y`, `width` vs `coded_width`).
- Check that doc comments match what the code does.
- Verify every computed value is actually used (don't compute a value
  and then not consume it).

For ports (chromium → Rust):

- Read the reference implementation completely before writing the port.
- Don't assume "similar" Rust code does the same thing as the C++.
- Verify matching inputs produce matching outputs (parity tests).

### 8. Add tracing FIRST when writing bitstream / FFI code

For new bitstream paths or native FFI: add diagnostic logging *before*
shipping the first `unsafe` block. The `mf_diff` example was added
*after* example.heic was discovered to be broken — should have been
written the moment we started writing the MF unpacker. Future native
backends (D3D11VA, VA-API real decode): write the equivalent of
`mf_diff` for that backend before declaring decode "complete".

### 9. Proof-by-Tests investigation (layered invariants)

When debugging, build a stack of invariant tests from coarsest to finest
and commit each one as it passes:

- **Layer 0**: Does it compile? Do existing tests pass?
- **Layer 1**: Does the new component roundtrip in isolation?
- **Layer 2**: Does the byte-level serialization match the reference?
- **Layer 3**: Does the full pipeline produce output a reference decoder
  accepts? (libheif, dec265, the rust backend)
- **Layer 4**: Is the output perceptually correct on real photos?
  (zensim, SSIM2)

When a layer passes, record it in this file with the test name and the
commit hash. Don't re-investigate passed layers. Focus on the first
failing layer — that's where the bug lives.

### 10. Pre-commit checklist

Run before every commit:

```bash
cargo fmt --all
cargo clippy --workspace --features backend-rust -- -D warnings
cargo test --features backend-rust,std --lib
```

For changes to MF / VT / MediaCodec / VA-API / D3D11VA backends, also
verify the per-backend cross-compile path that matches CI:

```bash
cargo clippy -p heic-backend-mediafoundation --target x86_64-pc-windows-gnu -- -D warnings
cargo clippy -p heic-backend-mediacodec --target aarch64-linux-android -- -D warnings
```

For MF-specific changes, run the Windows host tests via
`pwsh.exe -File V:\heic-win-test.ps1` and confirm 5/5 dispatch tests pass.

### 11. Bitstream / FFI tracing is permanent

`examples/mf_diff.rs`, `examples/probe_backends.rs`, the zensim corpus
diff harness — these stay in tree forever. They're the regression gate
that catches the next "chroma offset of 4 pixels" bug. Do not remove
them once they catch their first bug; they catch the next one too.

## Reference: chromium HEVC source

The chromium tree is sparse-checked-out at `~/work/chromium` with only
the HEVC-relevant files materialized:

* `media/gpu/h265_decoder.{cc,h}` — generic Accelerator-delegated
  decoder loop.
* `media/gpu/windows/d3d11_h265_accelerator.{cc,h}` — DXVA decode FFI.
* `media/gpu/vaapi/h265_vaapi_video_decoder_delegate.{cc,h}` — libva
  decode FFI.
* `media/parsers/h265_parser.{cc,h}` — SPS/PPS/slice header parsing.

Use these as reference when porting decoder paths. The
`heic-backend-d3d11va/PORTING.md` and `heic-backend-vaapi/PORTING.md`
guides cross-reference these files with line numbers.

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

### Current Quality (RGB comparison vs libheif)
- 103/162 test files decode successfully
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

### D3D11VA real-decode: midgray output on 2 of 6 corpus files (2026-05-22)

Status: ⚠ Partial. 4 of 6 `tests/testdata/*` files decode bit-exact
on the RTX 5070 via the D3D11VA backend. Two fail:

- `tests/testdata/libheif-examples/example.heic` (1280×854, grid of
  6× 512×512 tiles, BT.709 limited): all 6 tiles return Y=128 /
  Cb=128 / Cr=128 (midgray). PPS sdh=false.
- `tests/testdata/apple-hdr/hdr-sample.heic` (1512×850, Main10,
  BT.2020): also midgray on every tile. PPS sdh=false.

Synthetic corpus all 4 files pass:
- synth_8bit_q10/q50/lossless: amp=true, sao=true, scaling=false
- synth_8bit_q95: amp=false, sao=true, scaling=true (closest SPS
  match to the example tiles — only sdh=true differs)

Triage attempted: SPS+PPS dump via `HEIC_D3D11VA_DEBUG=1` shows the
two failing files differ from synth_q95 ONLY in `sdh=false`. Bit
position 25 of `dwCodingParamToolFlags` confirmed correct by
synth_q95 success. Likely remaining causes:

1. `scaling_list_data_present_flag` not propagated; driver expects
   INVERSE_QUANTIZATION_MATRIX buffer we don't send.
2. `pps_scaling_list_data_present_flag` similar.
3. Conformance-window math (854 visible vs 858 coded).
4. Main10 / P010 path needs distinct pic-param handling.

Reproduce: `HEIC_D3D11VA_HW=1 HEIC_D3D11VA_DEBUG=1 cargo test ...
d3d11va_vs_rust_synthetic_corpus` and uncomment the corpus_diff
test to see the failures. Diagnostics print first-pixel samples +
every relevant SPS/PPS flag.

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
