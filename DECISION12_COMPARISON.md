# Decision #12 Detailed Comparison

## Summary

**libde265 and Rust have completely different CABAC states before decision #12.**

This means the divergence happened **before** decision #12, likely in one of the first 11 decisions.

## libde265 Trace (Decision #12)

```
=== BEFORE DECODING split_cu_flag #12 ===
Position: (0, 16) depth=2
Context: idx=3 (condL=0 condA=1)
Neighbor depths: left=255 above=3
CABAC state before: range=316 offset=16640 bits_needed=-1
Context state: pStateIdx=19 valMPS=1

  decode_CABAC_bit: range=316 value=16640 bits_needed=-1 ctx=(state=19 mps=1)
  q_range_idx=0 lps_range=53
  range after subtract=263
  scaled_range=33664
  MPS path: value 16640 < scaled_range 33664
  bin_val=1 (MPS)
  context after: state=20 mps=1
  before renorm: range=263 value=16640
  after renorm: range=263 value=16640 bits_needed=-1
  result: bin=1

AFTER DECODING: bit=1
CABAC state after: range=263 offset=16640 bits_needed=-1
Context state: pStateIdx=20 valMPS=1
```

**Result: split=1 (does split)**

## Rust Trace (Decision #12)

```
=== BEFORE DECODING split_cu_flag at (0, 16) depth=2 ===
Context: idx=3 (condL=0 condA=1)
CABAC state before: range=291 offset=285 bits=1616
Context state: pStateIdx=24 valMPS=1

  decode_bin: range=291 value=36576 bits_needed=-3 ctx=(state=24 mps=1)
  q_range_idx=0 lps_range=41
  range after subtract=250
  scaled_range=32000
  LPS path: value >= scaled_range
  value after subtract=4576 range=41
  bin_val=0 (LPS)
  context after: state=19 mps=1
  before renorm: range=41 value=4576
  after renorm: range=328 value=36733 bits_needed=-8
  result: bin=0

AFTER DECODING: bin=0
CABAC state after: range=328 offset=286 bits=1624
```

**Result: split=0 (no split)**

## Key Differences

| Aspect | libde265 | Rust | Match? |
|--------|----------|------|--------|
| **Initial range** | 316 | 291 | ❌ |
| **Initial value** | 16640 | 36576 | ❌ |
| **Initial bits_needed** | -1 | -3 | ❌ |
| **Context state** | 19 | 24 | ❌ |
| **Context MPS** | 1 | 1 | ✅ |
| **Context idx** | 3 | 3 | ✅ |
| **q_range_idx** | 0 | 0 | ✅ |
| **LPS range** | 53 | 41 | ❌ |
| **range after subtract** | 263 | 250 | ❌ |
| **scaled_range** | 33664 | 32000 | ❌ |
| **Path taken** | MPS | LPS | ❌ |
| **Result** | bin=1 | bin=0 | ❌ |

## Analysis

### Why LPS values differ

LPS is looked up from the LPS table using:
- `LPS = LPS_table[state][(range >> 6) - 4]`

libde265:
- state=19, range=316
- (316 >> 6) - 4 = (4) - 4 = 0
- LPS_table[19][0] = 53 ✓

Rust:
- state=24, range=291
- (291 >> 6) - 4 = (4) - 4 = 0
- LPS_table[24][0] = 41 ✓

Both calculations are correct for their respective states!

### Why paths differ

libde265:
- value < scaled_range? → 16640 < 33664 → YES → MPS path

Rust:
- value < scaled_range? → 36576 < 32000 → NO → LPS path

### Root Cause

**The CABAC internal state (range, value, bits_needed) and context state diverged BEFORE decision #12.**

This means we need to trace back through decisions #1-11 to find where the state first diverged.

## Next Steps

1. Add tracing to ALL split_cu_flag decisions (not just #12)
2. Find the first decision where CABAC state diverges
3. Once found, add detailed decode_bin tracing to that decision
4. Compare the arithmetic operations to find the exact bug
