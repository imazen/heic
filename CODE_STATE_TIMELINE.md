# Code State Timeline - February 6, 2026

## Output File Timestamps

Only **ONE** output.ppm file exists in the working directory:
- **File**: `output.ppm` (3,279,376 bytes)
- **Last Modified**: February 6, 2026, **1:23:54 AM** (most recent decoder run)

**Critical**: All earlier PPM outputs have been overwritten and lost. This output represents the "full 280 CTU decode with garbage pixels" state that the user observed.

---

## Git Commit History

**Last Committed State**: January 23, 2026 at 05:58:07
- Commit: `665dc65` - "docs: add libde265 comparison investigation notes"
- All changes since then are **UNCOMMITTED** in the working directory

### Commits Since Previous Session
1. **Jan 23, 05:58** - `665dc65` - Documentation commit (LAST COMMITTED)
2. Earlier commits dating back to Jan 21 focused on CABAC context fixes and coefficient correlation analysis

---

## Uncommitted Changes Made Today (February 6)

All of today's work is in **unstaged/uncommitted state**. Changes by file:

### 📊 Change Statistics
```
Total: 1,431 insertions, 299 deletions across 10 files
Most changes in:
  - src/hevc/ctu.rs            (995 lines changed)
  - src/hevc/cabac.rs          (332 lines changed)
  - src/hevc/residual.rs       (322 lines changed)
  - src/hevc/transform.rs      ( 34 lines changed)
```

---

## Key Fixes Made Today (Estimated Chronology)

### Fix #1: `.min(3)` Clamp Removal in `decode_last_sig_coeff_prefix`
**File**: `src/hevc/residual.rs` (~line 969)

**Before**:
```rust
let ctx_idx = ctx_base + ctx_offset + ((prefix as usize) >> ctx_shift as usize).min(3);
```

**After**:
```rust
let ctx_idx = ctx_base + ctx_offset + ((prefix as usize) >> ctx_shift as usize);
```

**Impact**:
- Allowed `ctxIdxInc` values 0-4 (was clamped to 0-3)
- Enables correct 32x32 luma TU coefficient parsing
- **CABAC diff result**: Pushed divergence from bin #65,739 → #110,893
- **Decode result**: All 280 CTUs now parse without errors (was 149 before)

**⚠️ Hypothesis**: This fix may have revealed a subsequent bug elsewhere, or the increased bin count exposed errors in later contexts

---

### Fix #2: DCT32 Symmetry Correction in `get_dct32_coef`
**File**: `src/hevc/transform.rs` (~lines 60-80)

**Before**:
```rust
// Even rows: WRONG - used col % 16 for cols >= 16
DCT16_MATRIX[row / 2][col % 16]

// Odd rows: WRONG - complex mirroring logic with modulo
DCT32_ODD[odd_row % 16][mirror_col]
```

**After**:
```rust
// Even rows: CORRECT - symmetric property
DCT16_MATRIX[row / 2][31 - col]   // for col >= 16

// Odd rows: CORRECT - anti-symmetric property
-DCT32_ODD[odd_row][31 - col]     // for col >= 16
```

**Impact**:
- Corrected symmetry property T[2k][col] = T[2k][31-col]
- Corrected anti-symmetry property T[2k+1][col] = -T[2k+1][31-col]
- **Visual Impact**: NONE observed on first 4x4 block (errors appear in 4x4, not 32x32)
- **Significance**: Correct for future 32x32 blocks, but not root cause of pixel[0,0] = 151 vs 168

---

### Fix #3: Scan Order Function Signature Change
**File**: `src/hevc/residual.rs` 

**Before**:
```rust
pub fn get_scan_order(log2_size: u8, intra_mode: u8) -> ScanOrder
```

**After**:
```rust
pub fn get_scan_order(log2_size: u8, intra_mode: u8, c_idx: u8) -> ScanOrder
```

**Added Logic**:
```rust
let use_mode_based = if log2_size == 2 {
    true
} else if log2_size == 3 {
    c_idx == 0  // Only luma uses mode-based at log2=3 for 4:2:0
} else {
    false
};
```

**Impact**:
- Fixed scan order for chroma blocks in 4:2:0
- Chroma at log2=3 now correctly uses diagonal scan (not mode-based)
- **Significance**: Could affect coefficient ordering for Cb/Cr blocks
- **Concern**: Scan order changes affect which coefficients are decoded in which order, which impacts CABAC context state

---

### Fix #4: 4x4 Diagonal Scan Order Reordering
**File**: `src/hevc/residual.rs`

**Before**: Incorrect diagonal anti-diagonal scan pattern ordering
**After**: Corrected to match H.265 Table 6-5 anti-diagonal scan (top-right → bottom-left)

```
Old: (0,0), (1,0), (0,1), (2,0), (1,1), (0,2), ...
New: (0,0), (0,1), (1,0), (0,2), (1,1), (2,0), ...  (anti-diagonal traversal)
```

**Impact**:
- **CRITICAL**: Scan order directly affects which coefficient positions are processed
- This reordering could cause residuals to be read at different positions
- Would cause pixel reconstruction to be completely different

---

### Fix #5: Extensive CTU Decoding Debug Infrastructure
**File**: `src/hevc/ctu.rs` (~995 lines changed)

**Added**:
- `RUST_CU[]` debug traces for partition mode decisions
- MPM candidate list debug output
- NxN PU intra mode decode tracing
- Debug output for first 10 CUs
- Structured tracing of `flag`, `mpm_idx`, `rem`, and final mode decision

**Impact**:
- **No functional change** - purely debug output
- Helps identify which modes are being decoded
- Found: First block uses mode=0 (Planar), matches libde265

---

## Candidate Root Causes for Regression

### 🔴 HIGH SUSPICION: Scan Order Reordering

The 4x4 diagonal scan pattern change **directly affects the order in which residual coefficients are decoded**. Even if individual coefficients are correct, their INCORRECT positional assignment in the 4x4 grid would cause catastrophic output errors.

**Evidence**:
- User: "Block 300 had recognizable output" → "Block 280 full decode is garbage"
- This change reorders how coefficients are spatially mapped
- Would explain why prediction=128 ✓ but reconstruction=151 ❌ (wrong coefficients at wrong positions)

### 🟡 MEDIUM SUSPICION: Scan Order Function Signature w/ 4:2:0 Chroma

The addition of `c_idx` parameter to scan order selection could have broken chroma coefficient parsing:
- If chroma is being decoded incorrectly after this change
- Would cause widespread reconstruction errors throughout the image
- Could result not just from wrong values but from reading wrong positions

### 🟡 MEDIUM SUSPICION: `.min(3)` Fix Combined with Later Context Changes

While the .min(3) alone shouldn't break things, it enables 32x32 parsing which might have exposed:
- Uninitialized context state
- Context array bounds issues
- Incorrect context numbering elsewhere

---

## What Happened to "Block 300 Recognizable State"?

### Theorem: The Scan Order Changes Broke It

**Timeline Hypothesis**:

1. **Earlier run** (not preserved in PPM): Decoded ~300 blocks
   - Old scan order (possibly wrong per spec?) produced recognizable image
   - CABAC divergence at ~40k bins was acceptable because output was correct

2. **Added scan order fixes**: Changed scan pattern to match H.265 spec
   - Now scanning coefficients in "correct" order per standard
   - But this reordering caused coefficients to be assigned to wrong spatial positions
   - Result: Garbage reconstruction despite correct CABAC bins

3. **Added `.min(3)` fix**: Allowed full 280 CTU parse
   - Successfully decodes all CTUs without errors
   - But coefficients are still in wrong positions (due to scan order)
   - Catastrophic visual output

### Alternative Hypothesis: Interaction Effects

The combination of:
1. Scan order reordering
2. `.min(3)` allowing 32x32 parsing
3. Chroma `c_idx` scan selection
4. Various CABAC context changes

...may have created a cascade of errors where small individual fixes interact to produce wrong output.

---

## Reversibility Assessment

### ✅ SAFE TO REVERT (Non-functional)
- `src/hevc/ctu.rs` debug output changes
- All `RUST_CU[]`, `eprintln!()` statements
- Documentation comments

### ⚠️ UNCERTAIN (Could be correct or wrong)
- `.min(3)` removal (enables 32x32, but side effects unknown)
- DCT32 symmetry fix (correct per math, but doesn't affect first block)
- Scan order function signature change

### 🔴 LIKELY CULPRIT (High-risk reversions)
- 4x4 diagonal scan reordering (directly breaks coefficient positioning)
- Chroma scan order `c_idx` logic (may break 4:2:0 chroma)

---

## Recommendation

Rather than revert everything, **incrementally revert the scan order changes**:

1. **Keep**: `.min(3)` removal (fundamental fix)
2. **Keep**: DCT32 symmetry fix (mathematically correct)
3. **REVERT**: 4x4 diagonal scan reordering to the original order
4. **REVERT**: Chroma `c_idx` scan logic to simple mode-based check
5. **Keep**: All debug infrastructure

Then rebuild and test if image becomes recognizable again. If it does, we know the scan order changes broke visual output despite being "more spec-compliant."

---

## Files Modified Today

```
src/hevc/ctu.rs          [~995 lines] - partition mode, CU decode, mode tracing
src/hevc/residual.rs     [~322 lines] - scan order changes, coefficient tracing
src/hevc/cabac.rs        [~332 lines] - context state changes, tracing
src/hevc/transform.rs    [ ~34 lines] - DCT32 symmetry fix
src/hevc/slice.rs        [ ~23 lines] - minor changes
src/hevc/params.rs       [ ~7 lines ] - parameter parsing
src/hevc/mod.rs          [ ~6 lines ] - module changes
src/lib.rs               [ ~5 lines ] - library changes
Cargo.toml               [ ~4 lines ] - dependency/test changes
tests/decode_heic.rs     [ ~2 lines ] - test changes
```

---

## Conclusion

The **current code state (1:23:54 AM, Feb 6)** represents a "full parsing but garbage output" situation. The `.min(3)` fix successfully enables all 280 CTUs to parse, but the **scan order reordering appears to have broken coefficient positioning**, resulting in the catastrophic visual regression from "recognizable at block 300" to "garbage pixels."

To recover the recognizable output, **the 4x4 diagonal scan reordering should be the first candidate for reversal**, while keeping the other fixes that are mathematically or functionally correct.
