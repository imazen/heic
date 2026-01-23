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
- Intra prediction modes (intra.rs)
- Transform matrices and inverse DCT/DST (transform.rs)
- CABAC tables and decoder framework (cabac.rs)
- Frame buffer with YCbCr→RGB conversion (picture.rs)
- Transform coefficient parsing via CABAC (residual.rs)
- Adaptive Golomb-Rice coefficient decoding
- DC coefficient inference for coded sub-blocks
- Sign data hiding (all 280 CTUs now decode)
- Debug infrastructure (debug.rs) with CABAC tracker
- sig_coeff_flag proper H.265 context derivation
- Conformance window cropping (to_rgb/to_rgba apply SPS conf_win_offset)

### Pending
- Deblocking filter
- SAO (Sample Adaptive Offset)
- SIMD optimization

## Known Limitations

- Only I-slices supported (sufficient for HEIC still images)
- No inter prediction (P/B slices)
- CABAC desync causes early decode_terminate and corrupted coefficients

## Known Bugs

### CABAC Context Derivation (Partially Fixed)
- **sig_coeff_flag:** ✅ Fixed with proper H.265 9.3.4.2.5 context derivation
- **prev_csbf bit ordering:** ✅ FIXED (2026-01-22) - bit0=right, bit1=below per libde265
  - Previous code incorrectly swapped bits (claimed H.265 spec was opposite)
  - Fix moved first corruption from byte 124 to byte 4481 (35x later)
- **greater1_flag:** ✅ Fixed with ctxSet*4 + greater1Ctx formula per H.265
- **greater2_flag:** ✅ Fixed to use ctxSet (0-3) instead of always 0
- **coded_sub_block_flag:** Now using proper neighbor-based context derivation
- **Current status after fixes (2026-01-22):**
  - Decodes 109/280 CTUs (decode_terminate triggers early due to CABAC desync)
  - Large coefficients (>500): 19 starting at byte 4481
  - Conformance window cropping implemented (1280x854 output)
  - Note: With wrong prev_csbf, decoded all 280 CTUs but with worse corruption

### SSIM2 Comparison Results (2026-01-22)

Comparison against reference decoder (heic-wasm-rs / libheif):
- **SSIM2 score: -1097** (very poor, >90 is imperceptible, >70 is good)
- First corruption at byte 4481 (moved from byte 124 after prev_csbf fix)
- 19 large coefficients (>500) causing image corruption

- **last_significant_coeff_prefix context:**
  - Using flat ctxOffset=0 for luma instead of size-dependent offset
  - The H.265 correct approach (ctxOffset = 3*(log2Size-2) + ((log2Size-1)>>2)) causes early termination at CTU 67
  - This suggests additional desync is present but masked by the simpler context derivation

### Remaining Chroma Bias - Root Cause Analysis (2026-01-22)

**Symptoms:**
- Cr plane average ~209 instead of expected ~128
- CTU columns 0-3 have reasonable Cr (~124-135)
- CTU columns 4+ have elevated Cr (183-230)

**Root cause traced to Cr TU at (104,0):**
- Coefficient at scan position 3 (buffer position 2) has value 1064
- This coefficient has remaining_level = 1062 (base=2, so 2+1062=1064)
- Value 1062 requires prefix=12 in Golomb-Rice decoding (12 consecutive 1-bits)

**CABAC state analysis:**
- After decoding pos=5 remaining, CABAC state is (range=356, value>>7=355)
- This state is at the boundary where bypass decoding produces many 1-bits
- value=45440, range<<7=45568, so after shift: value*2 >= range<<7 always true

**Corruption propagation:**
1. Large coefficient 1064 after inverse transform gives residual ~248 at some positions
2. Adding residual 248 to prediction 135 overflows to 255 (clipped)
3. Neighboring TUs use corrupted reference samples (255 instead of ~135)
4. Corruption spreads through intra prediction to subsequent CTUs

**Key file locations:**
- `residual.rs:285` - decode_residual call #285 (problematic TU)
- `residual.rs:801-813` - coeff_abs_level_remaining with large prefix
- `ctu.rs:722-725` - Coefficient buffer at (104,0) showing [0, -9, 1064, 4, ...]

**Investigation update (2026-01-22):**
- First high coefficient (>100) appears at call #8 (value=-175), byte 55
- First large coefficient (>500) appears at call #258 (value=514), byte 1421
- Rice parameter reaches max (4) due to accumulating high coefficients
- Pattern suggests bitstream desync starting very early

## Debugging Strategy: Avoid Local Optima

**Problem:** With multiple interacting bugs, optimizing for "CTUs decoded" or "SSIM score" leads to local optima. The prev_csbf fix demonstrated this: wrong code decoded all 280 CTUs, correct code only decodes 109. Wrong contexts produce wrong-but-plausible values that don't trigger early termination.

**Solution:** Compare at the coefficient level, not end-to-end metrics.

### Correct Approach
1. **Differential testing per TU**: Compare decoded coefficients against libde265
   ```
   libde265 TU(0,0): [1, -3, -2, 1, 2, -1, ...]
   our decoder:      [1, -3, -2, 1, ?, ?, ...]  ← find first mismatch
   ```
2. **First divergence point** tells us exactly where to focus
3. **Don't use "decode success" as metric** - a decoder can "succeed" with wrong values

### Implementation Plan
1. Instrument libde265 to log coefficients per TU (or use FFI in hevc-compare)
2. Add matching logging to our decoder
3. Diff outputs to find first mismatched TU
4. Focus debugging on that specific TU's CABAC operations

### What We Know
- CABAC primitives are verified correct (hevc-compare tests pass)
- Bug is in **which operations we call and in what order**
- Need to compare the SEQUENCE of operations, not just individual operations

### hevc-compare Extension Needed
Extend `crates/hevc-compare/` to call libde265's `decode_residual_block()` via FFI and compare coefficient arrays directly. This gives ground truth without relying on end-to-end metrics.


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

### greater1_flag/greater2_flag Context Fix (2026-01-22)

**Problem:** Chroma averages were 198/210 instead of ~128.

**Root cause:** Context index derivation for coeff_abs_level_greater1_flag was incorrect.
Our implementation used `c1` directly (0-3), but H.265/libde265 requires:
- `ctxSet` (0-3): based on subblock position and previous subblock's c1 state
- `greater1Ctx` (0-3): starts at 1 each subblock, modified per coefficient

**Formula:** `ctxSet * 4 + min(greater1Ctx, 3) + (c_idx > 0 ? 16 : 0)`

**ctxSet derivation:**
- DC block (sb_idx==0) or chroma: base = 0
- Non-DC luma: base = 2
- If previous subblock ended with c1==0: ctxSet++

**greater1Ctx state machine:**
- Reset to 1 at start of each subblock
- Before decoding each coefficient (except first):
  - If previous greater1_flag was 1: greater1Ctx = 0
  - Else if greater1Ctx > 0: greater1Ctx++ (capped at 3 when using)

**greater2_flag:** Uses `ctxSet + (c_idx > 0 ? 4 : 0)` instead of just the chroma offset.

**Results:** Chroma averages improved from 198/210 to 161/173. Still not at target ~128.

### Session 2026-01-23: Operation Sequence Investigation

**Current state:**
- 225/280 CTUs decoded
- SSIM2 = -1130
- 26 large coefficients (>500)
- First large coeff at call#157, byte 1112, value=841

**Key finding: All 18 hevc-compare tests PASS**
- Individual CABAC operations match C++ exactly
- Bug must be in SEQUENCE of operations, not individual ops

**Call#157 analysis (first large coefficient):**
```
log2=2 c_idx=0 scan=Vertical byte=1101 cabac=(328,239)
remaining n=8 base=2 rice=2 byte=1106 cabac=(420,53696,-2) → remaining=839
```
- CABAC state (420, 53696, -2) is "hot" - value nearly equals scaled_range
- scaled_range = 420 << 7 = 53760, value = 53696 (diff = 64)
- Causes bypass decode to produce many consecutive 1-bits

**Verified CORRECT (matches libde265):**
- cbf_luma context: `ctx_offset = if trafo_depth == 0 { 1 } else { 0 }`
- cbf_cbcr context: `ctx_idx = CBF_CBCR + trafo_depth`
- split_cu_flag context: `ctx_idx = condL + condA`
- sig_coeff_flag context derivation (all scan types)
- greater1Ctx state machine
- Vertical scan coordinate swap

**What causes CABAC state to reach (420, 53696, -2)?**
The state is CUMULATIVE from all prior operations since slice start.
If call#156 ends correctly, call#157's starting state should match libde265.

**CBF_LUMA decode verified correct around byte 1100:**
```
CBF_LUMA: byte=1094->1094 ctx=31 val=true (350,319)->(343,319)
CBF_LUMA: byte=1101->1101 ctx=31 val=true (335,239)->(328,239)
```
- ctx=31 means CBF_LUMA(31) + 0 (trafo_depth > 0)
- State transitions look correct for context-coded bin decode

**Call#157 context derivation verified:**
- ctx_set=0 for sb_idx=0 in 4x4 luma (base=0, prev_gt1=false) ✓
- greater1Ctx starts at 1, increments/resets correctly ✓
- Vertical scan coordinate swap: (2,3) → local(3,2) ✓
- last_pos_in_sb=14 matches position (3,2) in vertical scan ✓

**Key insight:** All individual operations verified correct. Bug is likely in
NUMBER of operations - we may be doing extra/missing operations somewhere.

**Session 2026-01-23 (Continued):**

Additional verification performed:
1. **CTX_IDX_MAP_4X4** matches C++ exactly: `[0,1,4,5,2,3,4,5,6,6,8,8,7,7,8,8]`
2. **Sign decode order** matches libde265: high scan pos → low scan pos
3. **Remaining decode order** matches libde265: iterating from high to low
4. **DC handling** verified: `DC DECODE before: cabac=(340,13090,-7) → after: (295,13090,-7)`
5. **needs_remaining logic** matches libde265's `coeff_has_max_base_level`:
   - g1=0 positions → don't need remaining
   - g1=1 positions → need remaining
   - Positions beyond first 8 → always need remaining

**Detailed operation count for call#157:**
- 14 sig_coeff decodes (13 down to 1, plus DC)
- 8 greater1 decodes (first 8 significant coefficients)
- 1 greater2 decode (for first g1=1)
- 12 sign bypass bits (13 sig - 1 hidden)
- 11 remaining decodes

**Hot state occurs at position n=8:**
Before n=8 remaining: state = (420, 53696, -2)
- scaled_range = 420 << 7 = 53760
- value = 53696 → only 64 below threshold
- Bypass decode produces 839 consecutive 1-bits

**Root cause hypothesis:** State drift accumulated from earlier residual calls.
All operations within call#157 appear correct, so the initial state (328, 239)
at byte 1101 may already be wrong. Need to compare starting states across
all residual calls to find first divergence point.

**Recommended next step:** Add test that compares CABAC state at START of each
residual call between Rust and C++ (requires deeper libde265 integration).

### Context Derivation Analysis (2026-01-22)

**Debug infrastructure added:** CabacTracker in debug.rs tracks:
- CTU start byte positions
- Large coefficient occurrences (>500, indicates CABAC desync)
- First desync location for debugging

**Findings from example.heic:**
- First large coefficient at byte 1713 (in CTU 1, very early)
- 38 total large coefficients detected
- CABAC state becomes corrupt progressively
- Chroma prediction averages drift: 128 → 156 → 207 → 367 (impossible)

**Root cause identified:**
The simplified context selection for sig_coeff_flag (residual.rs:625):
```rust
let ctx_idx = context::SIG_COEFF_FLAG + if c_idx > 0 { 27 } else { 0 };
```
Uses a single context regardless of position, instead of the full H.265 derivation
which depends on position, sub-block location, TU size, and neighbors.

**Fix needed:** Implement full context derivation per H.265 section 9.3.4.2.5:
- Calculate sigCtx based on position within 4x4 sub-block
- Consider coded sub-block flag of neighbors
- Different logic for luma vs chroma
- Different logic for position 0 (DC) vs others

**Reference:** libde265 `decode_sig_coeff_flag()` in slice.cc

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
    ├── residual.rs  # Transform coefficient parsing
    ├── transform.rs # Inverse DCT/DST
    ├── debug.rs     # CABAC tracker, invariant checks
    └── picture.rs   # Frame buffer
```

## FEEDBACK.md

See `/home/lilith/.claude/CLAUDE.md` for global instructions including feedback logging.
