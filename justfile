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
