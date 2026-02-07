# decode_terminate() Investigation

## Problem Statement

Expert advised that calling `decode_terminate()` after every CTU is likely causing CABAC desync, since terminate bins should only exist at:
- **Non-WPP:** End of slice
- **WPP:** End of each CTB row

## Current Behavior

`decode_slice()` at line 301 calls `decode_terminate()` after EVERY CTU decoded.

For test_120x120.heic (4 CTUs, 2 rows, no WPP):
- Calls decode_terminate() 4 times
- First large coeff at byte 246
- 25 large coefficients total

For example.heic (280 CTUs, 14 rows, WPP enabled):
- Calls decode_terminate() 280 times
- 12 large coefficients total

## What decode_terminate() Does

From `cabac.rs` line 373:
```rust
pub fn decode_terminate(&mut self) -> Result<u8> {
    self.range -= 2;  // ALWAYS mutates state
    let scaled_range = self.range << 7;
    let bin_val = if self.value >= scaled_range {
        1u8
    } else {
        self.renormalize()?;  // Reads bits if bin=0
        0u8
    };
    Ok(bin_val)
}
```

**KEY:** This function ALWAYS mutates CABAC state, even when returning 0.

## Attempted Fix #1: Remove Per-CTU Calls

### Changes Made
1. Removed `decode_terminate()` call after every CTU
2. Added `decode_terminate()` only at WPP row boundaries
3. No `decode_terminate()` for non-WPP mid-slice

### Results
- **test_120x120.heic:** 16 large coeffs (down from 25), BUT quality WORSE:
  - Y avg_diff: 95.32 (was 58.81)
  - Cb avg_diff: 52.66 (was 30.11)
  - Cr avg_diff: 53.65 (was 28.21)

- **example.heic:** 63 large coeffs (UP from 12!) - much worse

### WPP Row-End Terminate Bins
All tested rows returned `end_of_substream=0` (correct - means "continue"):
```
DEBUG: row 0 end_of_substream=0
DEBUG: row 1 end_of_substream=0
DEBUG: row 2 end_of_substream=0
```

## Analysis

### Hypothesis 1: Terminate Bins ARE Present After Every CTU

Maybe our test files actually DO have terminate bins after every CTU (non-standard but possible). The original code calling decode_terminate() frequently might be CORRECT for these specific files.

### Hypothesis 2: Different Root Cause

The desync might not be from decode_terminate() calls, but from:
1. **Scan order issues** (but we verified scans match libde265)
2. **Context derivation errors** (but hevc-compare tests pass)
3. **Sequence logic bugs** (wrong number of operations)

### Hypothesis 3: Implementation Error in Fix

Maybe I implemented the fix incorrectly:
- WPP row transitions need terminate BEFORE or AFTER reinit?
- Non-WPP needs a final terminate call somewhere?
- Entry point offsets have off-by-one errors?

## Key Findings

1. ✅ **Scan orders are CORRECT** - our 4x4 diagonal scan exactly matches libde265
2. ✅ **WPP terminate bins exist** - present at row ends, correctly return 0
3. ✅ **Individual CABAC ops work** - hevc-compare tests pass
4. ❌ **Removing per-CTU terminates made things worse** - not the root cause?

## Next Steps

1. **Log all decode_terminate() return values** in original code
   - See if they're all 0 (expected) or mix of 0/1
   - Check byte positions where they're called

2. **Compare CABAC state evolution** between Rust and libde265
   - Trace range/value/bits_needed after each CTU
   - Find first divergence point

3. **Check entry point offset interpretation** for WPP
   - Are offsets relative to right base?
   - Are we reinitializing at correct byte positions?

4. **Verify coefficient decoding sequence** for first bad call
   - Log exact order of sig_coeff, greater1, greater2, remaining
   - Compare against libde265 for same TU

## Conclusion

The expert's advice about decode_terminate() may not apply to our specific test files. The original per-CTU calling pattern might actually be correct, or there's a different root cause for the CABAC desync.

**Current Priority:** Find the ACTUAL first divergence point through detailed CABAC state tracing, rather than assuming decode_terminate() is the issue.
