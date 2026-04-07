# heic-decoder development tasks

# Run unit tests
test:
    cargo test --lib

# Run all tests (requires test files + heic-wasm-rs)
test-all:
    cargo test

# Run tests with parallel feature
test-parallel:
    cargo test --lib --features parallel

# Check all feature permutations
feature-check:
    cargo check
    cargo check --features parallel
    cargo check --features zencodec
    cargo check --no-default-features

# Clippy
clippy:
    cargo clippy --all-targets --features parallel -- -D warnings

# Format
fmt:
    cargo fmt

# Local CI sanity check
ci: fmt clippy feature-check test test-parallel

# Run reference comparison tests (requires test files)
compare:
    cargo test --test compare_reference -- --nocapture

# Write comparison images (requires test files)
compare-images:
    cargo test --test compare_reference write_comparison_images -- --nocapture --ignored

# Run benchmarks
bench *ARGS:
    cargo bench {{ARGS}}

# --- Fuzzing ---

# Run all fuzzers for N seconds each (default 120)
fuzz seconds="120":
    #!/usr/bin/env bash
    set -euo pipefail
    for target in fuzz_probe fuzz_decode fuzz_hevc_raw fuzz_decode_av1 fuzz_decode_unci; do
        echo "=== $target ({{seconds}}s) ==="
        mkdir -p fuzz/corpus/$target
        cp fuzz/regression/* fuzz/corpus/$target/ 2>/dev/null || true
        cargo +nightly fuzz run $target \
            -- -max_total_time={{seconds}} -dict=fuzz/heif.dict -rss_limit_mb=2048 -jobs=1 \
            || { echo "CRASH in $target"; exit 1; }
    done
    echo "All fuzzers clean."

# Run a single fuzzer (e.g., just fuzz-one fuzz_hevc_raw 300)
fuzz-one target seconds="120":
    mkdir -p fuzz/corpus/{{target}}
    cp fuzz/regression/* fuzz/corpus/{{target}}/ 2>/dev/null || true
    cargo +nightly fuzz run {{target}} \
        -- -max_total_time={{seconds}} -dict=fuzz/heif.dict -rss_limit_mb=2048

# Minimize all fuzz corpora (dedup by coverage)
fuzz-cmin:
    #!/usr/bin/env bash
    set -euo pipefail
    for target in fuzz_probe fuzz_decode fuzz_hevc_raw fuzz_decode_av1 fuzz_decode_unci; do
        if [ -d "fuzz/corpus/$target" ]; then
            before=$(ls fuzz/corpus/$target/ | wc -l)
            cargo +nightly fuzz cmin $target -- -rss_limit_mb=2048
            after=$(ls fuzz/corpus/$target/ | wc -l)
            echo "$target: $before → $after seeds"
        fi
    done

# Generate fuzz coverage report
fuzz-coverage:
    #!/usr/bin/env bash
    set -euo pipefail
    LLVM_PROFDATA=$(find ~/.rustup -name "llvm-profdata" -path "*/nightly*" -type f | head -1)
    LLVM_COV=$(find ~/.rustup -name "llvm-cov" -path "*/nightly*" -type f | head -1)
    rm -rf /tmp/fuzz_cov_combined && mkdir -p /tmp/fuzz_cov_combined
    cd fuzz
    RUSTFLAGS="-C instrument-coverage" cargo +nightly build \
        --bin fuzz_decode --bin fuzz_probe --bin fuzz_hevc_raw
    cd ..
    for target in fuzz_probe fuzz_decode fuzz_hevc_raw; do
        if [ -d "fuzz/corpus/$target" ]; then
            echo "Collecting coverage for $target..."
            LLVM_PROFILE_FILE="/tmp/fuzz_cov_combined/${target}_%m_%p.profraw" \
                fuzz/target/debug/$target fuzz/corpus/$target -runs=0 -rss_limit_mb=512 2>/dev/null || true
        fi
    done
    $LLVM_PROFDATA merge -sparse /tmp/fuzz_cov_combined/ -o /tmp/fuzz_combined.profdata
    echo ""
    echo "=== Combined fuzz coverage ==="
    $LLVM_COV report fuzz/target/debug/fuzz_decode \
        --instr-profile=/tmp/fuzz_combined.profdata \
        --ignore-filename-regex="\.cargo|rustc|registry" \
        | grep -E "^work/zen/heic/src|TOTAL"

# Run fuzz regression tests
fuzz-regression:
    cargo test --all-features --test fuzz_regression
