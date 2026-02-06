# CABAC State Comparison: libde265 vs Rust

## libde265 Early Decisions

From `libde265_early_decisions.txt`:

| Decision | Position | Depth | CABAC State Before |
|----------|----------|-------|-------------------|
| #0 | (0,0) | 0 | range=256, value=2340, bits=-7 |
| #1 | (0,0) | 1 | range=256, value=4680, bits=-6 |
| #2 | (0,0) | 2 | range=266, value=9360, bits=-5 |
| #7 | (16,0) | 2 | range=438, value=7419, bits=-8 |
| #12 | (0,16) | 2 | range=316, value=16640, bits=-1 |

## Rust Early Decisions

Need to add detailed tracing to get CABAC states for each decision.

From current output:
- #0: (0,0) depth=0, result=1
- #1: (0,0) depth=1, result=1
- #2: (0,0) depth=2, result=1
- #3-6: (quadrants at depth=3)
- #7: (16,0) depth=2, result=1
- #12: (0,16) depth=2, result=0 ❌

## Key Finding

**Decision #12 produces different results:**
- libde265: result=1 (split)
- Rust: result=0 (no split)

**CABAC states before decision #12 are different:**
- libde265: range=316, value=16640, bits=-1
- Rust: range=291, value=36576, bits=-3

This confirms the state diverged somewhere between decision #7 and decision #12.

## Next Steps

1. Add decision counter to Rust code
2. Add detailed CABAC state tracing for ALL early decisions
3. Find the exact decision where state first diverges
4. Add detailed decode_bin tracing to that decision
5. Compare arithmetic operations to find the bug
