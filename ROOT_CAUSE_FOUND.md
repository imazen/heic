# Root Cause: CABAC Initialization Difference

## Discovery

The CABAC state is **different right from decision #0** - the very first split_cu_flag decode!

### Decision #0 - Before decoding

**libde265:**
```
Position: (0, 0) depth=0
CABAC state before: range=256 offset=2340 bits_needed=-7
Context state: pStateIdx=2 valMPS=1
```

**Rust:**
```
Position: (0, 0) depth=0
CABAC state before: range=510 offset=147 bits=16
Context state: pStateIdx=7 valMPS=1
```

## Key Differences

| Aspect | libde265 | Rust | Difference |
|--------|----------|------|------------|
| **Initial range** | 256 | 510 | WRONG |
| **Initial value/offset** | 2340 | 147 | WRONG |
| **bits_needed/bits** | -7 | 16 | WRONG |
| **Context state** | 2 | 7 | WRONG |

## Analysis

### Range Initialization

H.265 spec section 9.3.2.2 states:
- `ivlCurrRange` is initialized to 510

**Findings:**
- Rust: range=510 ✓ CORRECT
- libde265: range=256 ❌ WRONG (should be 510!)

Wait, but libde265 is the reference implementation and produces correct output...

Let me check the actual CABAC initialization code in both decoders.

### Context Initialization

H.265 spec section 9.3.2.2: Context models are initialized based on slice QP and cabac_init_flag.

The SPLIT_CU_FLAG context at index 2 should have the same initial state if both decoders are using the same QP and initialization.

**Findings:**
- libde265: state=2, mps=1
- Rust: state=7, mps=1

Different initial context states! This suggests the context initialization formulas or tables are different.

## Hypothesis

The CABAC decoder initialization is different between libde265 and Rust. Possible causes:

1. **Range initialization**: Rust uses 510, libde265 shows 256 in trace
   - But libde265 should also use 510 per spec...
   - Maybe libde265's trace shows range AFTER some operation?

2. **Value/offset initialization**: Different bitstream reading
   - Rust: offset=147 (binary: 10010011)
   - libde265: value=2340 (binary: 100100100100)
   - These don't match at all!

3. **Context initialization**: Different initial states
   - SPLIT_CU_FLAG[0] (ctx_idx=2): state 2 vs state 7
   - This is a 5-state difference, significant!

## Next Steps

1. ✅ Check libde265's init_CABAC_decoder_2() function
2. ✅ Check Rust's CabacDecoder::new() function
3. ✅ Compare bitstream reading (how first bytes are loaded)
4. ✅ Compare context initialization (INIT_VALUES tables)
5. Fix the initialization bug in Rust
