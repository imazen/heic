# ARM audit, 2026-09-06

Baseline main: `9e7f7561`. Apple M4 Pro, Rust 1.98, runtime dispatch without
`target-cpu=native`. Builds serialized with nice -n19 and four workers.

Residual addition used wrapping i16 arithmetic in NEON, AVX2 and WASM.
The new regression failed on the original NEON implementation for an 8-bit
prediction of 2 plus residual 32766: 0 instead of 255. The fix XOR-biases
unsigned predictions into the signed domain, saturates the signed addition,
unbiases and clamps with unsigned minimum. This covers u16 predictions too.

The regression exhausts prediction values at depths 8/10/12/15/16, seven
residual extremes, sizes 4/8/16/17/32, row padding and nonzero offsets.
It compares complete planes against independent widened i32 arithmetic.
Native ARM token permutations, x86_64 through Rosetta and WASM SIMD via
Wasmtime pass. WASM requires its compile-time SIMD token; native tests
require at least two token permutations. x86 results are emulated correctness
checks, not native x86 performance evidence.

Native libraries: 82 heic + 26 heic-core tests pass; strict clippy including
test targets passes. The existing Criterion dependency disables only its
Rayon feature so WASI library tests compile; plotting and benchmark support
remain enabled. No test is skipped or ignored by this change.

Commands (prefix heavy invocations with the resource settings above):

```sh
cargo test -p heic -p heic-core --features backend-rust,std,_dev --lib
cargo clippy -p heic -p heic-core --features backend-rust,std,_dev --lib --tests --no-deps -- -D warnings
cargo test -p heic --target x86_64-apple-darwin --features backend-rust,std,_dev --lib residual_add_matches_widened_scalar_at_every_prediction_value -- --nocapture
RUSTFLAGS='-C target-feature=+simd128' cargo test -p heic --target wasm32-wasip1 --features backend-rust,std,_dev --lib residual_add_matches_widened_scalar_at_every_prediction_value -- --nocapture
```

Whole-decoder timing and parity are still to be measured. Source inspection
finds that NEON IDCT16 and IDCT32 currently call the scalar implementations;
this report does not count those dispatch labels as vectorized kernels.
