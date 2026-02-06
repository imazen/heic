# HEIC Decoder - Key Learnings

## Root Cause Found: CABAC Context Array Offset Bug (2026-02-05)

**FIXED:** The Rust decoder had incorrect CABAC context array offsets. SAO contexts (indices 0-1) were missing, causing all subsequent contexts to be offset by -2.

- Fixed: `SPLIT_CU_FLAG` moved from index 0 → 2
- Fixed: All subsequent context indices shifted by +2
- Status: Both decoders now use correct context indices ✓

## Remaining Issue: CABAC State Divergence

Even after context fix, CABAC internal state (range/offset) diverges during decoding:
- Decision #74: libde265 range=354, Rust range=488
- All 75 split **decisions** match, but **states** differ

**Key Finding:** Potential difference in MPS renormalization:
- libde265: Manual 1-bit renorm in MPS path when `scaled_range < (256 << 7)`
- Rust: Always calls `renormalize()` loop until `range >= 256`

## Differential Tracing Strategy (Works!)

1. Add tracing at operation level (not end-to-end metrics)
2. Find first divergence point precisely
3. Compare CABAC state before/after each operation
4. Trace down to individual arithmetic operations

This avoids "local optima" where wrong values produce plausible results.

## CABAC Context Layout (H.265 Spec)

MUST follow exact order from ITU-T H.265 Table 9-4:
```
0-1:   SAO_MERGE_FLAG, SAO_TYPE_IDX
2-4:   SPLIT_CU_FLAG (3 contexts)
5-7:   CU_SKIP_FLAG (3 contexts)
...
```

See `src/hevc/cabac.rs` context module for full layout.

## Next Steps

1. Verify state transition tables match H.265 spec
2. Compare renormalization logic between implementations
3. Add detailed tracing to both decoders' arithmetic operations
4. Check LPS table values match

## Files Modified

- `src/hevc/cabac.rs`: Fixed context offsets, added decode_bin tracing
- `src/hevc/ctu.rs`: Added CQ split tracing
- `libde265/libde265/slice.cc`: Added CQ split tracing
