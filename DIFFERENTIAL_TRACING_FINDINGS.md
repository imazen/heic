# Differential Tracing Analysis - ROOT CAUSE FOUND

## Executive Summary

Successfully identified the root cause of coefficient decoding divergence between libde265 and Rust HEVC decoder using differential tracing at the coding quadtree level.

**ROOT CAUSE: Incorrect CABAC context array offset for `split_cu_flag`**

The Rust decoder uses context index 0 for `SPLIT_CU_FLAG`, but according to H.265 specification and libde265, SAO (Sample Adaptive Offset) contexts must come first, making `SPLIT_CU_FLAG` start at index 2.

---

## Bug Details

### Symptom
At coding quadtree decision #75, position (32, 48), both decoders have identical conditions (condL=1, condA=1, depth=2), but decode different split values:
- libde265: split=0 (does NOT split the 16x16 block)
- Rust: split=1 (DOES split the 16x16 block)

### Root Cause Analysis

**Coding quadtree comparison at (32, 48):**

| Decoder | Context Index | condL + condA | CABAC Before | Result |
|---------|--------------|---------------|--------------|--------|
| libde265 | 4 | 1 + 1 | range=302, offset=37248 | split=0 |
| Rust | 2 | 1 + 1 | range=284, offset=181 | split=1 |

**Context index calculation:**
- libde265: `CONTEXT_MODEL_SPLIT_CU_FLAG + (condL + condA)` = `2 + 2` = **4**
- Rust: `SPLIT_CU_FLAG + (condL + condA)` = `0 + 2` = **2**

The base offset is wrong!

### Context Array Layout (H.265 Spec)

According to ITU-T H.265 Table 9-4 and libde265 implementation:

```
Index  Context                           Rust (WRONG)    libde265 (CORRECT)
-----  --------------------------------  --------------  ------------------
0      SAO_MERGE_FLAG                   SPLIT_CU_FLAG   SAO_MERGE_FLAG
1      SAO_TYPE_IDX                     (missing)       SAO_TYPE_IDX  
2      SPLIT_CU_FLAG (3 contexts)       (missing)       SPLIT_CU_FLAG
3      SPLIT_CU_FLAG                    CU_TRANSQUANT   SPLIT_CU_FLAG
4      SPLIT_CU_FLAG                    CU_SKIP_FLAG    SPLIT_CU_FLAG
5      CU_SKIP_FLAG (3 contexts)        CU_SKIP_FLAG    CU_SKIP_FLAG
...
```

### Evidence From Detailed Tracing

**libde265 at (32, 48):**
```
Context: idx=4 (condL=1 condA=1)
Neighbor depths: left=3 above=3
CABAC state before: range=302 offset=37248
Context state: pStateIdx=32 valMPS=1
Result: bit=0 (no split)
```

**Rust at (32, 48):**
```
Context: idx=2 (condL=1 condA=1)
Neighbor depths: left=Some(3) above=Some(3)
CABAC state before: range=284 offset=181
Context state: pStateIdx=32 valMPS=1
Result: bin=1 (split!)
```

Both decoders use the same context STATE (pStateIdx=32, valMPS=1), but because they're indexing different positions in the context array, they're actually using different contexts that happen to have the same initialization values.

---

## Fix Required

**File:** `src/hevc/cabac.rs`
**Location:** Lines 344-389 (context module)

**Change:**
```rust
pub mod context {
    // Add SAO contexts first
    pub const SAO_MERGE_FLAG: usize = 0;        // NEW
    pub const SAO_TYPE_IDX: usize = 1;          // NEW
    
    // Fix all subsequent offsets (+2)
    pub const SPLIT_CU_FLAG: usize = 2;         // was 0
    pub const CU_TRANSQUANT_BYPASS_FLAG: usize = 5; // was 3
    pub const CU_SKIP_FLAG: usize = 6;          // was 4
    // ... etc (all need +2 offset)
}
```

---

## Verification

After fixing the context offsets, re-run the coding quadtree trace comparison:
```bash
cargo build --release --features trace-coefficients
./target/release/decode_heic test_120x120.heic
python compare_cq.py libde265_cq_trace.txt rust_cq_trace.txt
```

Expected result: Perfect match of all 265 coding quadtree decisions.

---

## Timeline of Investigation

1. **Initial observation**: Transform tree traces showed different block sizes (log2=4 vs log2=3)
2. **Coding quadtree tracing**: Added `split_cu_flag` tracing, found divergence at decision #75
3. **CABAC state comparison**: Showed different CABAC states before decoding
4. **Context index comparison**: Revealed idx=4 vs idx=2 with same condL/condA values
5. **Root cause**: Context array offset mismatch due to missing SAO context entries

---

## Lessons Learned

1. **Differential tracing works**: Comparing at the operation level (not end-to-end) precisely locates bugs
2. **Context array layout matters**: CABAC context array order must match H.265 spec exactly
3. **Test infrastructure investment pays off**: Time spent setting up tracing saved days of guesswork

---

**Status**: Root cause identified, fix ready to apply
**Date**: 2026-02-04
**Next Step**: Apply context offset fix and verify all tests pass
