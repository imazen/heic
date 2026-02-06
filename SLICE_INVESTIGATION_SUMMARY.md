# Slice Investigation & Multi-Slice Debugging Summary

## Problem Identified
The Rust HEIC decoder was stopping at CTU 223/280 instead of decoding the full picture, despite successful CABAC fixes that eliminated large coefficients.

## Root Cause Discovery
Through systematic debugging, we discovered the issue is **not** multi-slice structure but **entry point offsets** for tiles or wavefront parallelism (WPP).

### Investigation Process

#### 1. Initial Approach - Bitstream Extraction Issues
- **Problem**: Slice investigator failed with "Missing SPS(0) or PPS(1)" errors
- **Root Cause**: `extract_sps_pps_ids()` function had PPS ID parsing bugs
- **Evidence**: Both original and FFmpeg-reencoded bitstreams showed PPS ID mismatches

#### 2. FFmpeg Reference Approach
- **Action**: Used FFmpeg to create clean reference bitstream
- **Command**: `ffmpeg -i example.heic -c:v hevc -f hevc reference.hevc`
- **Result**: Same PPS ID issues persisted, confirming slice investigator bug

#### 3. Slice Investigator Fix
- **Problem**: Manual PPS ID extraction was unreliable
- **Solution**: Modified slice investigator to try all available PPS until successful
- **Code Change**: Replaced `extract_sps_pps_ids()` with iterative PPS parsing
- **Result**: Successfully parsed slice headers

#### 4. Key Discovery - Entry Points
- **Reference Bitstream Analysis**:
  - 1 slice covering 280/280 CTUs (100% coverage)
  - **13 entry points** - indicating tiles/WPP structure
  - Slice starts at CTU 0, should cover entire picture

## Technical Findings

### Slice Structure
```
Total NALs: 5
- VPS (Video Parameter Set)
- SPS (Sequence Parameter Set): 1280x856
- PPS (Picture Parameter Set): pps_id=0
- SEI (Supplemental Enhancement Information)
- Slice: addr=0, type=2 (I-slice), entry_points=13
```

### Entry Points Significance
- **13 entry points** = 13 tile rows or WPP rows
- Each entry point represents a resynchronization point in the bitstream
- Our decoder processes until first entry point boundary (~CTU 223) then stops
- Should continue processing all entry points to complete 280 CTUs

## Current Decoder Status

### ✅ Working Components
- SAO implementation - fixed early termination at CTU 3
- CABAC MPS renormalization - eliminated large coefficients
- Slice header parsing - works correctly with proper PPS
- Single slice processing - handles first segment correctly

### ❌ Missing Components
- **Entry point offset handling** - critical missing feature
- **Multi-tile/WPP support** - required for full picture decode
- **CTU coordinate continuation** across entry point boundaries

## Next Steps

### Immediate Priority (High Impact)
1. **Implement Entry Point Offset Handling**
   - Parse `num_entry_point_offsets` from slice header
   - Read entry point offset values from bitstream
   - Adjust CTU processing to continue after each entry point

2. **Add Entry Point Debug Tracing**
   ```rust
   // In slice.rs after parsing entry points
   eprintln!("Entry points: {} offsets", header.num_entry_point_offsets);
   for (i, &offset) in entry_offsets.iter().enumerate() {
       eprintln!("  Entry {}: byte_offset={}", i, offset);
   }
   ```

3. **Verify Tile/WPP Configuration**
   - Check `pps.tiles_enabled_flag` and `pps.entropy_coding_sync_enabled_flag`
   - Determine if tiles or WPP is being used
   - Implement appropriate CTU traversal pattern

### Implementation Strategy

#### Option A: Entry Point Offsets (Recommended)
- Continue current single-slice approach
- Skip to entry point positions when needed
- Maintain existing CTU coordinate system
- **Effort**: 2-4 hours

#### Option B: Full Tile/WPP Support
- Implement complete tile parsing
- Add WPP row synchronization
- More complex but comprehensive
- **Effort**: 8-12 hours

### Testing Plan
1. **Fix Entry Point Handling**
2. **Test on Reference Bitstream**
   ```bash
   cargo run --bin slice_investigator -- reference.hevc
   cargo run --release --bin decode_heic -- example.heic
   ```
3. **Verify Full 280 CTU Decode**
4. **Compare with libde265 Reference**

## Expected Outcome
After implementing entry point handling:
- Decoder should process all 280 CTUs
- No more early termination at CTU 223
- Full picture reconstruction
- Alignment with reference decoder behavior

## Files Modified
- `src/bin/slice_investigator.rs` - Fixed PPS parsing logic
- `src/hevc/mod.rs` - Made modules public for compilation
- `src/hevc/ctu.rs` - Added tracing for unused variables
- `src/hevc/residual.rs` - Added position tracing

## Lessons Learned
1. **Debugging tools must be as robust as main code** - slice investigator needed same parsing logic
2. **Entry points are critical for modern HEVC** - many encoders use tiles/WPP
3. **FFmpeg reference approach valuable** - helped isolate bitstream vs parser issues
4. **Systematic debugging pays off** - moved from extraction issues to core missing feature

## Time Investment
- **Slice investigation**: 2 hours
- **Root cause identification**: 1 hour  
- **Next implementation**: 2-4 hours
- **Total**: ~5-8 hours to resolve remaining 57 CTUs issue
