# CABAC Coefficient Desync Investigation

## Problem Summary

**Symptom:** Single chroma coefficient (Cb) gets wrong value (-2974 instead of small value) causing rainbow color artifacts that propagate through intra prediction.

**Location:** example.heic, call#16534, c_idx=1 (Cb), byte ~13523 (WPP row 1)

**Impact:** Buildings have rainbow effect while water/sky look correct. Details recognizable but colors mangled.

## HEVC Coefficient Decoding Flow

Transform coefficients are decoded using CABAC in this order:

### 1. Last Significant Position (lines 190-330 in residual.rs)
```rust
last_sig_coeff_x_prefix  // Context-coded
last_sig_coeff_y_prefix  // Context-coded
last_sig_coeff_x_suffix  // Bypass (if prefix >= 3)
last_sig_coeff_y_suffix  // Bypass (if prefix >= 3)
```

### 2. Coded Sub-Block Flags (lines 370-430)
```rust
// For each 4x4 sub-block (diagonal scan from last to DC)
coded_sub_block_flag[sb_idx]  // Context-coded, neighbor-dependent
```

### 3. Significant Coefficient Flags (lines 460-550)
```rust
// For each position in sub-block (reverse scan order)
sig_coeff_flag[n]  // Context-coded per H.265 section 9.3.4.2.5
// Context depends on: position, neighbors, sub-block location, scan type
```

### 4. Coefficient Levels (lines 470-650)
```rust
// Phase 1: Greater-1 flags for first 8 significant coefficients
for n in first_8_sig_coeffs {
    coeff_abs_level_greater1_flag[n]  // Context-coded
    // Context: ctxSet * 4 + min(greater1Ctx, 3) + chroma_offset
    // If flag=1: base_level = 2
    // If flag=0: base_level = 1
}

// Phase 2: Greater-2 flag for first greater-1 coefficient
if first_g1_exists {
    coeff_abs_level_greater2_flag  // Context-coded
    // Context: ctxSet + chroma_offset
    // If flag=1: base_level = 3, needs_remaining = true
}

// Phase 3: Remaining levels (Golomb-Rice bypass)
for n in all_sig_coeffs_needing_remaining {
    coeff_abs_level_remaining[n]  // BYPASS (not context-coded)
    // Uses adaptive Rice parameter (0-4)
}
```

### 5. Signs (lines 560-610)
```rust
// Bypass bits, with sign data hiding
for coeff in sig_coeffs {
    sign_flag[coeff]  // Bypass
}
// Last coefficient sign may be inferred from parity if sign_hiding enabled
```

## Critical Function: decode_coeff_abs_level_remaining

**Location:** src/hevc/residual.rs, lines 1072-1109

```rust
fn decode_coeff_abs_level_remaining(
    cabac: &mut CabacDecoder<'_>,
    rice_param: u8,        // 0-4, adaptive
    base_level: i16,       // 1, 2, or 3 from greater flags
) -> Result<(i16, u8)> {
    // Decode prefix (unary part) - BYPASS
    let mut prefix = 0u32;
    while cabac.decode_bypass()? != 0 && prefix < 32 {
        prefix += 1;
    }

    let value = if prefix <= 3 {
        // TR (Truncated Rice): value = (prefix << rice_param) + suffix
        let suffix = if rice_param > 0 {
            cabac.decode_bypass_bits(rice_param)?  // BYPASS
        } else {
            0
        };
        ((prefix << rice_param) + suffix) as i16
    } else {
        // EGk (Exponential Golomb): suffix_bits = prefix - 3 + rice_param
        let suffix_bits = (prefix - 3 + rice_param as u32) as u8;
        let suffix = cabac.decode_bypass_bits(suffix_bits)?;  // BYPASS
        let base = ((1u32 << (prefix - 3)) + 2) << rice_param;
        (base + suffix) as i16
    };

    // Update rice parameter adaptively
    let threshold = 3 * (1 << rice_param);
    let new_rice_param = if (base_level.unsigned_abs() as u32 + value as u32) > threshold {
        (rice_param + 1).min(4)
    } else {
        rice_param
    };

    Ok((value, new_rice_param))
}
```

## Where Desync Likely Occurs

The -2974 value suggests a very long prefix was decoded. Let's analyze:

```rust
// If rice_param = 0 and prefix = 12:
// prefix > 3, so EGk mode
// suffix_bits = 12 - 3 + 0 = 9 bits
// base = ((1 << 9) + 2) << 0 = 512 + 2 = 514
// If suffix reads 9 bits getting value X, total = 514 + X

// To get -2974 (abs = 2974):
// 2974 = base + suffix
// If rice=0, prefix=11: base=((1<<8)+2)<<0=258, suffix=2716 (needs 11 bits)
// If rice=1, prefix=10: base=((1<<7)+2)<<1=260, suffix=2714 (needs 10 bits)
// Etc.
```

The question: **Why did the prefix decode loop read so many 1-bits?**

### Possible Causes

1. **Wrong CABAC state entering this function**
   - Previous context-coded bins (sig_coeff_flag, greater1_flag, etc.) had wrong context
   - CABAC state drifted, making bypass bits appear as 1s

2. **Wrong rice_param value**
   - If rice_param is too high, more suffix bits are read
   - Rice param updates based on previous coefficients in scan order

3. **Wrong base_level from greater flags**
   - If greater1_flag or greater2_flag was decoded incorrectly
   - Wrong needs_remaining determination

4. **Off-by-one in scan order or sub-block iteration**
   - Decoding coefficients in wrong order
   - Skipping or duplicating positions

## Verification Strategy

The hevc-compare tests show that individual CABAC operations are CORRECT when isolated. This means:
- ✅ `decode_bypass()` works correctly
- ✅ `decode_bin()` with specific context works correctly
- ✅ `decode_bypass_bits(n)` works correctly

**Therefore:** The bug is in the SEQUENCE or LOGIC, not the primitives.

### What to Check

1. **Context derivation for call#16533 (one before the bad one)**
   - What sig_coeff_flag contexts were used?
   - What was the greater1Ctx state?
   - What was ctx_set?

2. **Rice parameter history**
   - What rice_param was passed to call#16534?
   - What were the previous coefficient values that updated it?

3. **Scan order and position**
   - Which sub-block (sb_idx)?
   - Which position (n) within the sub-block?
   - What was last_x, last_y?

4. **Greater flag values**
   - What were the greater1_flag values for this sub-block?
   - Was greater2_flag decoded?
   - What was the base_level (1, 2, or 3)?

## Known Correct Behaviors

From our investigation:
- ✅ test_120x120.heic first 6 TUs are PERFECT (coefficient-level match)
- ✅ CABAC bins match libde265 for first 50K bins
- ✅ Raw coefficients match libde265 for first 100 calls
- ✅ All hevc-compare individual operation tests pass
- ✅ Context derivation formulas match H.265 spec and libde265

## Code Locations for Sharing

**Main file:** `src/hevc/residual.rs` (1150 lines)

**Key functions:**
- Line 170: `decode_residual()` - Main entry point
- Line 470: Coefficient level decoding loop (greater1, greater2, remaining)
- Line 613: Remaining level decoding loop
- Line 1072: `decode_coeff_abs_level_remaining()` - Where large value decoded

**Context derivation:**
- Line 562: `ctx_set` calculation
- Line 484: `greater1Ctx` state machine
- Line 410: `coded_sub_block_flag` context (neighbor-based)

**CABAC primitives:** `src/hevc/cabac.rs`
- Line 200: `decode_bin()` - Context-coded
- Line 255: `decode_bypass()` - Bypass single bit
- Line 280: `decode_bypass_bits()` - Bypass N bits

## Differential Tracing Needed

To find the exact divergence point, we need to compare:
1. CABAC state at START of call#16534 (Rust vs libde265)
2. Rice parameter value (Rust vs libde265)
3. Base level value (Rust vs libde265)
4. Each bypass bit during prefix decode (Rust vs libde265)
5. Context indices for previous greater1_flag decodes

The desync is cumulative - a small error in an earlier call (maybe call#16500-16533) causes wrong CABAC state entering call#16534, leading to the catastrophic -2974 value.
