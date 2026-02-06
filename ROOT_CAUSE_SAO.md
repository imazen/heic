# ROOT CAUSE IDENTIFIED: Missing SAO Decoding

## Summary

**The Rust decoder is missing SAO (Sample Adaptive Offset) parameter decoding**, causing CABAC state to diverge from the very beginning of slice decoding.

## Evidence

### CABAC State at Decision #0

**libde265:**
```
Position: (0, 0) depth=0
CABAC state: range=256, value=2340, bits_needed=-7
Context state: pStateIdx=2, valMPS=1
```

**Rust:**
```
Position: (0, 0) depth=0
CABAC state: range=510, value=18880, bits_needed=-8
Context state: pStateIdx=7, valMPS=1
```

Completely different states right from the first decision!

### Code Comparison

**libde265 (slice.cc:2916-2919):**
```c
if (sps.sample_adaptive_offset_enabled_flag) {
  read_sao(tctx, xCtb, yCtb, CtbAddrInSliceSeg);  // ← SAO decoded HERE
}

read_coding_quadtree(tctx, xCtbPixels, yCtbPixels, sps.Log2CtbSizeY, 0);
```

**Rust (ctu.rs:243-257):**
```rust
fn decode_ctu(&mut self, x_ctb: u32, y_ctb: u32, frame: &mut DecodedFrame) -> Result<()> {
    // ...reset state...

    // Decode the coding quadtree
    self.decode_coding_quadtree(x_ctb, y_ctb, log2_ctb_size, 0, frame)
    //                           ↑ SAO decoding is MISSING!
}
```

## What is SAO?

Sample Adaptive Offset (SAO) is defined in H.265 section 7.3.8.3. For each CTU, when enabled, the decoder reads:
- `sao_merge_left_flag` or `sao_merge_up_flag`
- `sao_type_idx_luma` and `sao_type_idx_chroma`
- SAO offset values and class information

These are CABAC-coded bins that modify the CABAC internal state.

## Why This Matters

When SAO is enabled in the bitstream (which it is for example.heic):
1. libde265 reads SAO parameters using CABAC, changing the CABAC state
2. Rust skips SAO entirely, keeping CABAC in its initial state
3. By the time both reach the first `split_cu_flag`, their CABAC states have diverged
4. All subsequent CABAC decoding produces different results

## The Fix

Add SAO parameter decoding to `decode_ctu()` before calling `decode_coding_quadtree()`:

```rust
fn decode_ctu(&mut self, x_ctb: u32, y_ctb: u32, frame: &mut DecodedFrame) -> Result<()> {
    let log2_ctb_size = self.sps.log2_ctb_size();

    // Reset per-CTU state
    if self.pps.cu_qp_delta_enabled_flag {
        self.is_cu_qp_delta_coded = false;
        self.cu_qp_delta = 0;
    }

    // **ADD SAO DECODING HERE**
    if self.sps.sample_adaptive_offset_enabled_flag {
        self.decode_sao(x_ctb, y_ctb)?;
    }

    // Decode the coding quadtree
    self.decode_coding_quadtree(x_ctb, y_ctb, log2_ctb_size, 0, frame)
}
```

The `decode_sao()` function needs to be implemented following H.265 section 7.3.8.3 and 7.4.9.5, reading all SAO syntax elements using CABAC.

## Context Array Note

The SAO context models (SAO_MERGE_FLAG at index 0-1) were already added to fix the context offset bug. Now we need to actually USE them by implementing SAO decoding!

## Files to Modify

1. `src/hevc/ctu.rs`: Add `decode_sao()` implementation
2. `src/hevc/cabac.rs`: SAO contexts already exist at indices 0-1 ✓

## Priority

**CRITICAL** - This is the root cause of all CABAC divergence. Must be fixed before any other coefficient decoding issues can be addressed.
