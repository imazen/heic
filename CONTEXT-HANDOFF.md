# CABAC Coefficient Decode Bug Investigation - Context Handoff

## Current State (2026-01-23)

**Status:** All 225 CTUs decode, but with 26 large coefficients (>500) indicating CABAC desync. First large coefficient at call#157 (byte 1112, value=841).

**Key Finding:** All 18 hevc-compare tests PASS - individual CABAC operations match C++. The bug is in accumulated state drift across residual calls.

## Investigation Summary

### What's Verified CORRECT

1. **Individual CABAC primitives** - All tests pass comparing C++ and Rust:
   - bypass decode ✓
   - bypass bits ✓
   - coeff_abs_level_remaining ✓
   - context init ✓
   - context-coded bin decode ✓
   - sig_coeff_flag context derivation ✓
   - greater1/greater2 flag contexts ✓
   - vertical scan handling ✓

2. **Context derivations match libde265:**
   - CTX_IDX_MAP_4X4: `[0,1,4,5,2,3,4,5,6,6,8,8,7,7,8,8]` ✓
   - cbf_luma: `ctx = CBF_LUMA + (trafo_depth == 0 ? 1 : 0)` ✓
   - cbf_cbcr: `ctx = CBF_CBCR + trafo_depth` ✓
   - greater1Ctx state machine ✓
   - ctxSet derivation (base + prev_gt1) ✓

3. **Operation sequences within a TU match:**
   - Sign decode order (high scan pos → low) ✓
   - Remaining decode order (high → low) ✓
   - DC handling (decode when can_infer_dc=false) ✓
   - needs_remaining matches coeff_has_max_base_level ✓

### The Problem

**Call#157 at byte 1101:**
```
log2=2 c_idx=0 scan=Vertical initial_state=(328,239)
14 sig_coeff + 8 g1 + 1 g2 + 12 signs + 4 remaining decodes
→ State becomes (420, 53696, -2) which is "hot" (value ≈ scaled_range=53760)
→ remaining n=8 produces 839 consecutive 1-bits → coefficient 841
```

The state (420, 53696, -2) is mathematically correct for the operations performed WITHIN call#157. The problem is the STARTING state (328, 239) at byte 1101 may already be wrong due to drift in earlier calls.

### Root Cause Hypothesis

**Accumulated state drift:** If even ONE earlier residual call decoded a different number of bits than libde265, all subsequent states would shift. Since individual operations are correct, the bug must be in the NUMBER of operations per call.

Possible causes:
1. Different coefficient positions being decoded
2. Extra/missing context-coded bins somewhere
3. Different decision logic for DC inference vs explicit decode

### Next Steps

1. **Compare CABAC state at START of each residual call:**
   - Add instrumentation to our decoder to log starting state
   - Do the same in libde265 via FFI
   - Find first call where states diverge

2. **Alternatively, compare coefficient arrays:**
   - Extend hevc-compare to decode full TUs and compare results
   - Find first TU with mismatched coefficients
   - Trace that specific TU's operation sequence

3. **Look for conditional differences:**
   - DC inference conditions
   - Coded sub-block flag decisions
   - Any other conditional decoding logic

## Key Files

- `src/hevc/residual.rs` - Coefficient decode (bug location)
- `src/hevc/ctu.rs` - CTU/TU decode, cbf flags
- `crates/hevc-compare/` - C++ comparison infrastructure
- `/home/lilith/work/heic/libde265-src/libde265/slice.cc` - Reference impl

## Debug Commands

```bash
# Run SSIM comparison (shows large coeff count)
cargo test --test compare_reference test_ssim2 -- --nocapture

# Run hevc-compare tests (verify primitives)
cd crates/hevc-compare && cargo test -- --nocapture

# Enable debug tracing for specific residual call
# Edit src/hevc/residual.rs line 203:
let debug_call = residual_call_num == 157;
```

## Spec References

- 9.3.4.2.5 - sig_coeff_flag context derivation
- 9.3.4.2.6 - greater1_flag context (ctxSet derivation)
- 9.3.4.2.7 - greater2_flag context
- 9.3.4.2.4 - coded_sub_block_flag context

## Don't Forget

- Delete this file after loading into new session
- Check git status before starting work
- Individual CABAC primitives are CORRECT - focus on operation counts
