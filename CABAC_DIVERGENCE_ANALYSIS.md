# CABAC State Divergence Analysis

## Status
- ✅ Context offsets fixed (SPLIT_CU_FLAG now at index 2)
- ✅ State transition tables verified identical
- ✅ LPS tables verified identical  
- 🔍 **CABAC state diverges at decision #74**

## Observed Divergence

### After Decision #74 (56, 40) depth=3
| Implementation | range | value/offset | bits_needed |
|----------------|-------|--------------|-------------|
| libde265       | 354   | 16832        | -2          |
| Rust           | 488   | 265 (<<7=33920) | -5       |

Both decode the same split decision (split=0), but end with different states!

## Key Implementation Difference: MPS Renormalization

### libde265 (cabac.cc:182-196)
```c
if (decoder->value < scaled_range) {
    // MPS path
    decoded_bit = model->MPSbit;
    model->state = next_state_MPS[model->state];

    if (scaled_range < (256 << 7)) {
        // Manual 1-bit renorm
        decoder->range = scaled_range >> 6;
        decoder->value <<= 1;
        decoder->bits_needed++;
        if (decoder->bits_needed == 0) {
            decoder->value |= *decoder->bitstream_curr++;
            decoder->bits_needed = -8;
        }
    }
    // else: no renorm needed
}
```

**Key:** libde265 does **conditional, optimized 1-bit renormalization** in MPS path

### Rust (cabac.rs:233-263)
```rust
if self.value < scaled_range {
    // MPS path  
    bin_val = ctx.mps;
    ctx.state = STATE_TRANS_MPS[ctx.state as usize];
} else {
    // LPS path (always renorms)
    ...
}

// ALWAYS renormalize regardless of path
self.renormalize()?;
```

**Key:** Rust **always calls renormalize()** which loops until range >= 256

## Decision #75 Trace (Rust)

```
range=284 value=23192 bits_needed=-5
q_range_idx=0 lps_range=27
range after subtract=257
scaled_range=32896
MPS path: value 23192 < scaled_range 32896
bin_val=1 (MPS)
before renorm: range=257
after renorm: range=257 (no change, already >= 256)
```

**Range 257 > 256, so no renormalization happens** (correct)

## Next Steps

1. ✅ Add detailed decode_bin tracing to both decoders
2. ⏳ Compare decode_bin execution for decision #74
3. ⏳ Verify if the renormalization difference causes the divergence
4. ⏳ Check if libde265's optimized renorm matches H.265 spec

## H.265 Spec Reference

**Section 9.3.4.3.2:** Renormalization process in arithmetic decoding engine

The spec requires:
```
while (ivlCurrRange < 256) {
    ivlCurrRange <<= 1
    ivlOffset <<= 1
    ivlOffset |= read_bits(1)
}
```

Both implementations should be equivalent if:
- libde265's 1-bit optimization is correct for `scaled_range < (256 << 7)`
- Rust's loop handles the same case correctly

**Critical:** Need to verify the condition `scaled_range < (256 << 7)` equals `range < 256`
- scaled_range = range << 7
- scaled_range < (256 << 7) ⟺ range << 7 < 256 << 7 ⟺ range < 256 ✓

So the logic should be equivalent!
