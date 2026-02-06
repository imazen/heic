# HEIC Decoder – CABAC Debugging Progress Report

## Where We Are

We're building a pure-Rust HEIC/HEVC decoder, validating it bin-by-bin against libde265 (the reference C decoder). The test image is `example.heic` (1280×854, 280 CTUs, QP=17, I-slice).

### Current Match: 138 context-coded bins (up from 27)

The decoder now matches libde265 for the **first 387 global bins** (138 context-coded + 249 bypass bins). The first divergence occurs at **BIN#388** (context bin #139), where both decoders use different context indices for `sig_coeff_flag`, meaning they believe they're decoding different coefficient positions.

### Visual Progress

The partial decode already produces a recognizable image: the first ~11 CTU areas show realistic pixel values (avg luminance ~157, varied colors), followed by green artifacts from the divergence cascading through remaining CTUs.

---

## What We Fixed (Chronological)

### 1. PART_MODE init values
Context models for partition mode had wrong initialization values. Fixed to match libde265's `initValue` table.

### 2. CU_QP_DELTA_ABS init values  
Similar init value fix for QP delta contexts.

### 3. NxN intra mode syntax order
For Part_NxN, the spec requires decoding all `prev_intra_luma_pred_flag` values first, then all `rem_intra_luma_pred_mode`/`mpm_idx` values. We were interleaving them.

### 4. IntraSplitFlag in transform tree
For NxN partition mode, the transform tree must force-split at depth 0 and use `MaxTrafoDepth + 1` as the effective depth limit.

### 5. cu_qp_delta_abs decode
Implemented the truncated unary + EG(0) bypass suffix decoding that was previously missing. This is decoded before the residual when `cu_qp_delta_enabled_flag=true`.

### 6. cbf_luma condition for INTRA
The `cbf_luma` flag must always be decoded for INTRA CUs, regardless of other conditions.

### 7. ★ Diagonal scan order x/y transposition (THIS SESSION — biggest impact)
The 4×4 diagonal scan table had x/y coordinates **transposed within each anti-diagonal**. The spec's up-right diagonal scan starts from the leftmost position (x=0) and moves to x++, y--. Our table had the opposite ordering within each anti-diagonal, causing scan position 1 to map to (1,0) instead of the correct (0,1).

**Impact**: Moved the first divergence from context bin #27 to #139 — a 5× improvement in match depth.

---

## Current Divergence Analysis (BIN#388)

### The Symptom
At BIN#388, both decoders have consumed identical bitstream data (387 bins with matching types and values), but they use different CABAC context indices:
- **Rust**: ci=109 → `SIG_COEFF_FLAG(82) + 27` → chroma ctxIdxInc=0 (DC at position 0,0)
- **libde265**: ci=97 → `SIG_COEFF_FLAG(65) + 32` → chroma ctxIdxInc=5 (non-DC position like 3,0)

### What This Means
Both decoders agree on every decoded bit, but **interpret the CU/TU tree structure differently**. Our decoder thinks it's decoding the DC coefficient of a 4×4 chroma TU at position (0,0), while libde265 is at a different coefficient position entirely.

### Verified: Context Derivation Code Is Correct
A thorough comparison of `calc_sig_coeff_flag_ctx()` against libde265's equivalent confirmed:
- ✅ prevCsbf bit convention (bit0=right, bit1=below) matches
- ✅ Size-dependent offsets (9/15/21 for luma, 9/12 for chroma) match
- ✅ "+3 if not first sub-block" logic matches
- ✅ Chroma offset of 27 matches
- ✅ CTX_IDX_MAP_4×4 lookup table matches
- ❌ Only the doc comment is wrong (says bit0=below but code correctly uses bit0=right)

### Root Cause: Structural Divergence in CU/TU Tree
Since the CABAC arithmetic and context derivation are both correct, the divergence must come from how **earlier decoded values** produce a different CU/TU tree. Specifically, somewhere in the first CU's processing, the two decoders take different structural paths — perhaps a different `cbf_chroma` decision, a different transform tree split, or a different coefficient count — that causes them to enter the chroma residual at different states.

### Key Observation: libde265 Parallel Bypass Tracing
libde265's `decode_CABAC_FL_bypass_parallel()` decodes multiple bypass bins at once and prints the **final state** for all bits in the batch. This causes apparent v/bn mismatches in the traces (e.g., at BYP#372) that are **tracing artifacts only** — the actual decoded values and final states match perfectly.

---

## Agent Trace Suggestions Review

The agent suggested checking:
1. **ctxIdxMap construction** — ✅ Verified identical between both codebases
2. **SIGNIFICANT_COEFF_FLAG increment** — ✅ Both use the same ctxIdxInc derivation
3. **4×4 context table blocks** — ✅ CTX_IDX_MAP_4×4 matches
4. **scan path conventions and log2TrafoSize** — ✅ Scan tables now correct, log2_size passed correctly
5. **Minimal reproducer instrumentation** — Done, with ci= field in BIN# traces

The agent's suggestions were all validated — the residual context derivation is correct. The actual bug is upstream in the CU/TU tree parsing.

---

## ffmpeg as Alternative Reference

Per the custom instructions: *"If libde265 doesn't work, use ffmpeg to decode heic and hevc. It is in path."*

ffmpeg can decode full HEIC files without needing to extract raw HEVC bitstream. This is useful for:
- **Ground truth pixel comparison**: `ffmpeg -i example.heic -pix_fmt rgb24 reference.raw`
- **Quick validation** when libde265 build issues arise
- **Decoding HEIF containers** directly (libde265 only handles raw HEVC)

However, for bin-level CABAC debugging, libde265 with TRACE_COEFFICIENTS is still essential since ffmpeg doesn't provide that level of tracing.

---

## Next Steps

### Immediate: Find CU/TU Tree Parsing Bug
The structural divergence means something in `decode_coding_unit()` or `decode_transform_tree()` (in [ctu.rs](src/hevc/ctu.rs)) is parsing the tree differently from libde265. The plan:

1. **Add syntax-element-level tracing** to both decoders — tag each BIN/BYP with which syntax element it belongs to (e.g., "split_transform_flag", "cbf_chroma", "sig_coeff_flag"). This will reveal exactly where the CU/TU tree parsing paths diverge.

2. **Compare transform tree structure** — The chroma Cb block at call#4 (pos 0,0, log2=2) follows luma call#3 (pos 4,4, log2=2). Check whether libde265 is processing a different luma block at this point, or whether it has a different cbf_chroma / split_transform decision.

3. **Check cbf_chroma propagation** — In HEVC, `cbf_chroma` flags propagate through the transform tree. If our code makes a different split/cbf decision, the chroma residual blocks would be decoded at different points.

### After Fix: Continue Bin Comparison
Each fix typically reveals the next divergence further along. The progression has been:
- Fix 1-6: ~27 context bins → various CTU counts
- Fix 7 (scan order): 27 → 139 context bins
- Next fix: aim for 200+ context bins matching

### Eventually: Full 280 CTU Decode
Goal is all 280 CTUs decoding without errors, producing output that matches ffmpeg/libde265 pixel-for-pixel.

---

## Key Files

| File | Purpose |
|------|---------|
| [src/hevc/cabac.rs](src/hevc/cabac.rs) | CABAC arithmetic decoder, context model layout and init |
| [src/hevc/ctu.rs](src/hevc/ctu.rs) | CU/TU tree parsing, main decode loop |
| [src/hevc/residual.rs](src/hevc/residual.rs) | Residual coefficient decoding |
| [compare_bins.py](compare_bins.py) | Bin-level comparison tool |
| rust_trace_full.txt | Current Rust trace (500 bins) |
| libde265_trace_full.txt | Current libde265 trace (500 bins) |
