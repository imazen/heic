# Changelog

All notable changes to the `heic` crate are documented in this file. Format follows [Keep a Changelog](https://keepachangelog.com/). This file was backfilled from git history on 2026-04-15; dates for `[0.1.0]`, `[0.1.1]`, and `[0.1.2]` reflect the commit dates of the corresponding release tags (`v0.1.0`, `v0.1.1`, `v0.1.2`) rather than crates.io publish dates.

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next major (or minor for 0.x) release.
     Add items here as you discover them. Do NOT ship these piecemeal — batch them. -->

### Fixed
- Reject SPS with `pic_width_in_luma_samples` / `pic_height_in_luma_samples` outside `1..=16384` and conformance-window offsets that exceed picture dimensions, closing a panic / multi-GiB allocation reachable from the default no-limits decode path (security audit CR-1, CR-2, H-3)
- `cropped_width` / `cropped_height` now use `saturating_sub` and `set_crop` clamps oversized offsets, so out-of-range crops cannot wrap to ~`u32::MAX` and reach `Vec::with_capacity` (CR-1)
- Promote pixel-index calculations in `to_bgra` / `to_bgr` / `to_rgba` / `to_rgb` / `get_chroma` 4:4:4 / `get_y` and `decode_alpha_plane` to `usize` before multiplication, defeating u32 overflow on 32-bit targets (H-1)
- Lower derived-image (iden / grid / iovl) recursion depth from 8 to 3 and add a per-request `decode_item` invocation cap of 32 768 to bound CPU cost from crafted fan-out graphs (H-2)
- Cap `parse_moov` track count at 16 so per-track sample / chunk / stsc tables cannot multiply unbounded (H-4)
- Poll the cancellation token inside the `resolve_sample_offset` chunk loop so a 1M-chunk stsc run remains responsive (H-5)
- Apply a sane default `Limits` (16 384×16 384, 256 Mpx, 1 GiB) when the caller does not supply one, replacing the previous all-`None` sentinel that bypassed every dimension and memory check (CR-2)

## [0.1.4] - 2026-04-20

### Documentation
- Add patent notice to README and crate docs clarifying that HEVC/HEIF may be covered by third-party patents (Access Advance pool), Imazen holds none, and this codec is decode-only (6df7070)

### Dependencies
- Bump minimum `zenpixels` and `zenpixels-convert` to 0.2.10, picking up the TF-change planner fix, `AlphaPolicy::CompositeOnto` correctness on premultiplied sources, and first-class Gamma 2.2 transfer in the fast path (ee363a2)

## [0.1.3] - 2026-04-17

### Added
- Parse Apple HDR gain map metadata into `GainMapPresence::Available` (#10, 8e6ecaf)

### Changed
- Set `ColorAuthority::Cicp` for HEIC nclx precedence (#6, 394e94f)
- Migrate `ThreadingPolicy` handling to the `is_parallel()` helper; prefer `Sequential`/`Parallel` over the deprecated `SingleThread`/`Unlimited` variants (339d59b)

### Performance
- `memchr`-based NAL start-code scan in `parse_annexb` (#9, 8115b3e, 134067b)

### Dependencies
- Bump `zencodec` to 0.1.19 (339d59b)

### Tests
- Probe-vs-decode parity test (5aa84e2)

## [0.1.2] - 2026-04-10

Multi-codec HEIF support, image sequences, wasm128 SIMD tier, fallible public API, and extensive fuzz-driven hardening. `0.2.0` was published and immediately yanked after `cargo semver-checks` confirmed no breaking changes versus `0.1.1`; the same code was re-released as `0.1.2` (2232b2f).

### Added
- AV1 decode via `rav1d-safe` behind the `av1` feature flag (5e42ea7, b89239a)
- Uncompressed HEIF (`unci`) decode via `zenflate` behind the `unci` feature (0161ad2, 9cbae36)
- Multi-codec dispatch for AV1, `unci`, JPEG, and H.264 item types (ff7bad9, 5dbb954)
- Accept `mif3`, `mif2`, `avif`, and `avis` brands in the HEIF container parser (4aec096, 45480e5)
- HEIF image sequence (`msf1`/`moov`) parsing and decode support (91e990a, e984923)
- `wasm128` tier added to every SIMD dispatch point, including native `wasm128` implementations for IDST4, IDCT8, dequantize, and color convert (b0bf9ee, 10294af, d892563, 54b044f)
- `fuzz_color_transform` fuzz target plus HEVC raw fuzzer, HEIF dictionary, and AV1/`unci` fuzz targets (3dc30c7, e30be8f, 0de104a, 19f8150)
- CI fuzz workflow and `justfile` recipes (3e636f8)
- Minimized fuzz regression seeds committed to `fuzz/regression/` (dfa9453)
- Comprehensive tests for multi-codec HEIF support (4a3bb71, 78e27a8)
- Limits, cancellation, and truncation tests for hardened decode paths (2767c23, 53af478)
- Security section in README; intro updated for multi-codec and hardening (fc7244a, 57db308)

### Changed
- `DecodedFrame` public methods are now fallible and return `Result` (25d8ae7, 5abda47)
- `zencodec` adapter is allocation-safe and no longer panics (4d009fe)
- Remaining hot-path allocations converted to `try_vec!` (77dad87)
- Limits and stop-cancellation wired through AV1, `unci`, and alpha decode paths; pre-decode limit check for HEVC tiles (42d2692, 9fd1c26, 19f8150, 4042b72)
- Fuzz review pass: surface errors instead of silent tolerance (a6c51ff)
- Fuzz corpus is now gitignored; minimize artifacts land as corpus seeds (4a6c029)
- `fuzz/Cargo.lock` committed for reproducible fuzz builds (c274682)
- Gitignore tooling noise and exclude it from published packages (5e7a7db)

### Dependencies
- Bump `fast-ssim2` to 0.8.0 (7f3a014)
- Bump `zencodec` to 0.1.13 (dccc6bd)
- Bump `archmage` and `magetypes` to 0.9.16 (217dad6)

### Fixed
- Guard against arithmetic overflow in HEVC parsing and formatting (575586d, 6fdb5e6)
- Validate `minus_one` fields at parse time, error on out-of-bounds tile coordinates (5f9cf1d)
- `num_ref_idx` out-of-bounds in pred weight table (30205d1)
- Scaling list coefficient delta overflow (d7895d4)
- `u8` overflow in `minus_one + 1` patterns (9759d86)
- `iinf` `entry_count` read without bounds check (54910c2)
- `iloc` `item_count` read without bounds check (cb794a3)
- Remaining reference-picture POC arithmetic overflows (95ea81d)
- `delta_poc_minus1 + 1` overflow in reference-picture parsing (5e7341e)
- Harden SPS parsing and intra prediction bounds (60b2a2a)
- Clamp SPS `bit_depth`/`poc_bits` and fix overflow sites (59d64aa)
- `stsc` zero `first_chunk` underflow in `resolve_sample_offset` (31bb6e0)
- Integer overflows in reference-picture and residual parsing (8c17f32)
- Dequantize `i32` overflow and multi-extent size cap (4446b1b)
- `cargo fmt` correction in `codec.rs` (f94531c)

### Performance
- Zero-copy `u16` → `Rgb`/`Rgba` conversion via `bytemuck::try_cast_vec` (6d89a99)

## [0.1.1] - 2026-04-01

Security audit follow-up, switchable fallible allocation, libheif comparison tooling, and CI cleanup.

### Added
- `heif-ref` Docker reference tool for comparison testing (e8ac7d9, 101fa21)
- libheif comparison test suite via Docker (cdb21c3, 0b16c28)
- In-repo test data and parser tests (3c20759, 6178db8)

### Changed
- `try_vec!` macro for switchable fallible/infallible allocation (d54fa03)
- Normalized `Cargo.toml` formatting (7cc2d90)
- `cargo fmt` pass (e24e8af, e1d04c3)

### Dependencies
- Bump `zenpixels`/`zenpixels-convert` to 0.2.2 (21e338c)
- Update `zencodec` to 0.1.11 (d9a47ef)

### Fixed
- Security audit: panic removal, fallible allocation, SAO corruption, bounds checks (754a029)
- `heif_ref` API compatibility for libheif 1.18+ auxiliary type free (9b57a14, 3c61e16)
- Restrict clippy and coverage CI to `--lib` to skip debug test files and `heic-wasm-rs` (72bd533, a8406fb, e037697)
- Clean up CI to remove `sed` patching of path dependencies (949cfc1)
- Gate 64-bit overflow tests with `cfg(target_pointer_width)` (bf0d6d8)
- Remove commented `zennode` dep (9b6bea8)

## [0.1.0] - 2026-03-29

Initial release of the `heic` crate (renamed from `heic-decoder`). Pure-Rust HEIC/HEIF decoder with SIMD, `no_std + alloc` compatible, integrated with the `zencodec` trait family.

### Added
- Crate renamed from `heic-decoder` to `heic` (38cc711)
- `zennode` decode node for the HEIC decoder (16001fc)
- Extract `cLLi`/`mDCv` HDR metadata from the HEIF container (071d1b5)
- Feature permutation CI coverage (4569d7f)
- Standard `.gitignore` exclusions (f86d3f6)

### Changed
- Adapt to `zencodec` `Orientation` unification (31dce07)
- Adapt to consuming `DecoderConfig::job(self)` signature (16001fc, bf0e79e, 3409a50)
- Archive `zennode_defs.md` as markdown; disable `zennode` (44e6517, 03b1232)
- Standardize dual-license (AGPL-3.0 / Commercial) (6cb3161)
- Honor `DecodePolicy`, add HDR metadata extraction (4b568dd)
- Make gain map and depth map decode opt-in (76d5054)
- `zencodec` trait compliance audit fixes (002a5d2)

### Dependencies
- Require `zenpixels` 0.2.1 (gamut matrix, serde, ICC profiles, bug fixes) (5950999)
- Bump `archmage` to 0.9.14 (9b07cb9)
- Update `zencodec` through 0.1.2 → 0.1.3 → 0.1.4 → 0.1.5 → 0.1.8 (43f1a6c, 2d4a275, f28195e, c9e1faf, 5d4094b)

### Fixed
- Prepare `heic` crate for publish (4d88072)
- Declare `threads_supported_range` conditional on the `parallel` feature (a3e8426)
- Add GAT lifetime to `DecoderConfig::Job` type alias and `job` method (bf0e79e)
- Stale README, CLAUDE, and CABAC-DEBUG-HANDOFF docs (84d2a0d, 1a378b7, 95fadf1)
- Rustdoc footnote resolution (d47be28)

### Performance
- Reuse MC scratch buffer instead of per-block allocation (8e11ec3)
