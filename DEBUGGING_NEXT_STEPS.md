# HEIC Decoder Debugging Summary & Next Steps

## Current Status

### ✅ Completed
- **SAO Implementation**: Added Sample Adaptive Offset decoding in `decode_ctu()` which fixed early termination at CTU 3.
- **CABAC MPS Renormalization**: Fixed critical divergence at decision #74 by implementing libde265-style conditional 1-bit renorm for MPS path.
- **Large Coefficient Issue**: Eliminated - no more large coefficients (>500) after CABAC fix.

### 📊 Current Performance
- **Before Fix**: 3/280 CTUs decoded, 1 large coefficient (-528)
- **After Fix**: 223/280 CTUs decoded, 0 large coefficients
- **Termination**: Still stops at CTU 223 via `end_of_slice`

### 🔍 Remaining Issue
The decoder processes only 223/280 CTUs then terminates with `end_of_slice`. This suggests:
1. Multiple slices/tiles in the bitstream
2. Missing entry point offset handling
3. Incorrect slice_segment_address continuation

## Investigation Paths

### Path A: Multi-Slice/Tile Handling (Most Likely)
**Evidence**: Single slice should cover all 280 CTUs for a 1280x856 image with 32x32 CTUs.
**Files to examine**:
- `src/hevc/slice.rs` - slice header parsing, entry point offsets
- `src/hevc/mod.rs` - NAL unit iteration and slice decoding loop
- `src/hevc/bitstream.rs` - NAL unit parsing

**Key questions**:
- Does `example_full.hevc` contain multiple slice NALs?
- Are entry point offsets being parsed correctly?
- Is slice_segment_address > 0 for subsequent slices?

### Path B: Tile/WPP Entry Points
**Evidence**: Some encoders use tiles or wavefront parallelism.
**Check**:
- `pps.tiles_enabled_flag` and `pps.entropy_coding_sync_enabled_flag`
- Entry point offset parsing and CTU coordinate continuation

### Path C: Bitstream Extraction Issue
**Evidence**: Previous issues with incomplete HEVC extraction from HEIC.
**Check**:
- Verify `example_full.hevc` contains all slices
- Compare with libde265's input format

## Recommended Next Steps

### Step 1: Quick NAL Count (Alternative to slow Python script)
```bash
# Use hexdump/strings to count slice NALs faster
hexdump -C example_full.hevc | grep "00 00 01" | wc -l
# Or use a compiled tool
```

### Step 2: Add Slice Debug Output
Add to `src/hevc/mod.rs` in `decode_nal_units()`:
```rust
eprintln!("Found NAL type: {:?}, total NALs: {}", nal.nal_type, nal_units.len());
```

### Step 3: Verify Multi-Slice Logic
Check if `slice_segment_address` is > 0 for any slice:
```rust
// In slice.rs after parsing
eprintln!("slice_segment_address: {}", slice_header.slice_segment_address);
```

### Step 4: Test with libde265 Reference
Run libde265 on the same file to confirm expected CTU count:
```bash
libde265 --show-slices example_full.hevc
```

### Step 5: Implement Multi-Slice Continuation
If multiple slices exist, ensure:
- CTU coordinates continue correctly after slice boundary
- CABAC state is reset per slice
- Entry point offsets are honored

## Priority Assessment

### High Priority (Do Now)
1. **Add slice debug output** - minimal effort, high information gain
2. **Check slice_segment_address values** - confirms multi-slice theory
3. **Verify NAL count** - quick sanity check

### Medium Priority (If needed)
1. **Implement entry point offset handling**
2. **Add multi-slice CTU coordinate continuation**
3. **Compare with libde265 slice parsing**

### Low Priority (Last resort)
1. **Regenerate test file with single slice**
2. **Deep dive into tile/WPP specifications**

## Expected Outcome

If the issue is multi-slice handling:
- We'll see slice_segment_address > 0 for subsequent slices
- Fixing slice continuation should allow full 280 CTU decode
- Differential tracing will then show proper alignment with libde265

If the issue is elsewhere:
- Single slice with slice_segment_address = 0
- Need to investigate tile/WPP or bitstream extraction

## Time Estimate

- **Debug output addition**: 5 minutes
- **NAL count verification**: 2 minutes  
- **Multi-slice fix implementation**: 30-60 minutes
- **Testing and verification**: 15 minutes

Total: **1-2 hours** to resolve remaining 57 CTUs issue.
