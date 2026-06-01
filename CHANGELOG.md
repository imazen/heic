# Changelog

All notable changes to the `heic` crate are documented in this file. Format follows [Keep a Changelog](https://keepachangelog.com/). This file was backfilled from git history on 2026-04-15; dates for `[0.1.0]`, `[0.1.1]`, and `[0.1.2]` reflect the commit dates of the corresponding release tags (`v0.1.0`, `v0.1.1`, `v0.1.2`) rather than crates.io publish dates.

## [Unreleased]

### QUEUED BREAKING CHANGES
<!-- Breaking changes that will ship together in the next major (or minor for 0.x) release.
     Add items here as you discover them. Do NOT ship these piecemeal — batch them. -->

- **Default `cargo build` now fails with a `compile_error!` directing the user to enable a backend feature.** Previously the pure-Rust decoder shipped automatically as `default = ["std"]`; now the user MUST opt into at least one of `backend-rust`, `backend-mediafoundation`, `backend-videotoolbox`, `backend-mediacodec`, `backend-vaapi`, or `backend-d3d11va`. This is the 0.2.0 breaking change. The existing `default` build pulled in `heic`'s entire HEVC implementation unconditionally; the new layout makes the backend explicit so users on Apple / Android / Windows can pick the patent-licensed native decoder instead.
- **`DecoderConfig` gains an allowlist API** (`with_backend`, `with_backends`, `recommended_backends`). Decoding without any backend in the allowlist returns `HeicError::NoBackendSelected`. `DecoderConfig::recommended_backends()` constructs a platform-aware default order from the compiled-in backends.

### Security — 2026-05-31 deep safety review (untrusted-input hardening)
A 43-agent adversarially-verified review of all crates + native backends. Fixes for crafted-`.heic` panic/DoS/overflow/OOB reachable from the shipping `backend-rust` path and the native FFI:
- **Pure-Rust untrusted input**: reject out-of-range derived transform-block size (inter-CU OOB into the fixed `[i16;1024]` coeff buffer), HEVC tile-grid > 20×22, `scaling_list_dc_coef_minus8` outside `[-7,247]` (overflow), and long-term-RPS counts > DPB (overflow); these all previously panicked or wrote wrong pixels (95d6eab). NAL length-prefix `checked_add` (32-bit wrap → slice panic) and crop `saturating_sub` (clap+irot crop underflow → panic/OOB) (5be0203, 5f23346). `stsc` sample-resolver iteration clamped to `num_chunks` + zero-sample rejection (a ~650-byte `msf1` file spun ~4.3e9 uncancellable iterations) and clap-crop clamp + iovl negative-offset clipping (4bf4f5e). zencodec streaming grid decoder now enforces the default 16384²/256Mpx/1GiB ceiling when no `Limits` is passed + `checked_mul` strip size (uncapped alloc / 32-bit OOB) (be7533e).
- **Native FFI memory-safety**: VA-API `VAPictureParameterBufferHEVC` field order corrected to the libva ABI (`slice_parsing_fields` was 4 bytes early → driver read every later field wrong; pinned with compile-time `offset_of` asserts) + `unpack_planes` geometry validation against the mapped `VAImage` (OOB read) (274db91). D3D11VA bitstream-buffer bound check made width-safe (`as u32` truncation → >4 GiB OOB GPU write) (c840a02). MediaFoundation linear-unpack bounds check + IMFSample release-on-retry leak; native backends now reject non-4:2:0 sources (fall through to pure-Rust) instead of mislabeling a 4:2:0 buffer; VideoToolbox crop + plane null-check; MediaCodec NV12/NV21/planar disambiguation + crop bounds; D3D11VA per-slice slice-control + 0xFF ref-pic sentinel (19a998c).
- **Fuzz-found panics on crafted slice headers / SPS** (nightly `fuzz_decode` + `fuzz_decode_limits`): entry-point byte-offset accumulation now saturates (`ctu.rs` — large `entry_point_offsets` overflowed the `u32` cumulative position); the slice-header `offset_minus1 + 1` saturates (`slice.rs` — `offset_len == 32` could read `u32::MAX`); and the SAO band/edge appliers clamp their region to the actual plane buffer (`sao.rs` — a malformed SPS could make `x_end`/`y_end`/`stride` exceed the plane, OOB-indexing `plane[y*stride+x]`). All three are no-ops for valid images (corpus + feature tests + `example.heic` unchanged) and pinned by regression seeds under `fuzz/regression/`.

### Changed — packaging (2026-06-01)
- Trim `tests/` (29 files, 512 KB) and `benches/` (2 files) from the published tarball; downstream consumers don't build the test suite. Removed dead `/fuzz/regression/**` entry from the include list (fuzz/ is a detached workspace; its files were never packaged anyway). Source, examples, README, CHANGELOG, and licenses are unchanged.

### Added — lossless 4:4:4 decode (2026-06-01)
- **Lossless HEVC (`cu_transquant_bypass`) now decodes** — 10-bit 4:4:4 intra-NxN,
  `matrix_coefficients=0` (GBR), bit-exact vs libheif. Five SE-trace-guided fixes:
  the 4:4:4 `cbf` carve-out at the 4×4 leaf, 4:4:4 NxN per-PB chroma modes, parsing
  the intra transform tree under bypass, the per-quadrant chroma **scan order**
  (Angular 10/26 → vertical/horizontal, the final CABAC desync), and direct
  bypass reconstruction (residual = coeff, + implicit RDPCM gated on the SPS flag).
  Supported by full VUI + `hrd_parameters` + `sps_range_extension` parsing and
  `matrix_coefficients=0` identity color (R=Cr, G=Y, B=Cb). No regression to the
  existing corpus / 4:4:4 (`nokia_444` MAE 1.4) decode.

### Added — per-feature test coverage + CI (2026-05-31)
- **`testdata/features/` — 25 small (<20 KB) per-feature HEIF fixtures** + `tests/cov_features.rs` (24 pixel-verified tests). Covers the `src/decode.rs` orchestration paths the synthetic/uncompressed corpora never reached: derived `grid` (2×2 + Nokia 3×2) / `iden` / `iovl`, `irot`/`imir`/`clap` transforms, auxiliary alpha, thumbnail, monochrome 4:0:0, 10-bit, 4:4:4 (Nokia MIAF003), nclx BT.709/BT.2020 signalling, EXIF/XMP, plus the overlay `version`-reject / canvas-`Limits` / 32-bit large-format / negative-offset `src_skip` branches and the `MAX_DERIVED_DEPTH` iden-chain rejection. Every transform/iden/iovl/4:4:4 golden was cross-checked pixel-for-pixel against libheif `heif-dec` 1.21.2. Reproducible via `scripts/gen_feature_fixtures.py`. This lifts `decode.rs` region coverage from 34.9 % to 51.3 %.
- **CI gating**: the 24 feature tests run on every OS (behavior step) and feed the coverage job; the recursive corpus gate also walks `testdata/features/` (all 6 OSes + i686 + overflow-checks). `heic-backend-mediacodec` is now linted (`clippy -D warnings`, `--target aarch64-linux-android`) — previously the only compile-only backend.
- **VA-API runtime coverage**: `HEIC_VAAPI_HW`-gated `vaapi_decodes_example_on_hardware` + `vaapi_vs_rust_corpus_diff` tests (mirroring the D3D11VA/VideoToolbox pattern), wired into `vaapi-runtime.yml`. Hosted CI keeps the compile-time `offset_of!` ABI gate + clippy; real-GPU decode runs on a self-hosted Linux runner (any vendor — Mesa `radeonsi` incl. RDNA2 iGPU, Intel `iHD`, NVIDIA). `docs/CI.md` updated: VA-API works on native Linux; only WSL + hosted runners (no `/dev/dri`) can't.

### Fixed — 2026-05-31 review (correctness)
- **10/12-bit limited-range YCbCr→RGB was ~4× too dark** — the `9576` Y-scale constant (8-bit fixed-point) was not rescaled as the shift grew with bit depth, clipping 10-bit limited-range white at ~25% brightness (every iPhone HDR / BT.2020 image through the 16-bit path). Now scales with bit depth; regression test covers 8/10/12-bit white→`0xFFFF` / black→0 (5be0203).
- **Monochrome (4:0:0) HEVC decoded ~95% garbage** — the 4:0:0 path read `cbf_cb`/`cbf_cr` and `intra_chroma_pred_mode` syntax that a monochrome bitstream doesn't contain (H.265 gates them on `ChromaArrayType != 0`); each phantom CABAC bin desynced the decode after a few CTBs. The Apple HDR gain map (a 768×432 monochrome image) came out ~95% UNINIT/white; now decodes 0/331776 UNINIT. 4:2:0/4:2:2/4:4:4 unaffected (87c79fd8).
- **MediaCodec rejected legitimate decoder-cropped output** — the geometry check demanded the SPS coded height even when MediaCodec emitted the (shorter) cropped buffer, erroring on every HEIC with coded≠visible height (e.g. example.heic 856→854). Caught by the new on-device Android-emulator runtime gate (c7b298c).
- **Flat dequantize overflowed at high QP + high bit depth** — `scale * (1 << qp_per)` and `coef * combined_scale` overflowed i32 for 10/12/16-bit RExt content (panic under overflow-checks / fuzz, wrapped garbage in release). i64 fallback for the rare overflow-prone case; the perf-tuned i32 SIMD path is unchanged for 8-bit + normal-QP 10-bit (30304659).

### Added — decode-completeness guard
- The pure-Rust HEVC decoder now **errors on an incomplete decode** instead of returning sentinel pixels: if any coded luma sample remains at the UNINIT marker after slice decode (a truncated/corrupt bitstream the CABAC EOF-tolerance would otherwise let "succeed"), it returns `InvalidBitstream` rather than leaking `u16::MAX` (white) into the output. Verified against the bundled corpus (87c79fd8).

### Added — native HEVC backends + workspace
- **`heic-core` workspace member**: shared types (`HevcBackend` trait, `BackendError`, `HvccParams`, `DecodedFrame`) and platform-neutral helpers (NAL conversion, YCbCr→RGB SIMD) used by every backend. `no_std + alloc`, `#![forbid(unsafe_code)]`, minimal dep surface. Lets backend crates depend on the shared contract without pulling in the parent crate. Commits c6ee4ab, edf1c0c.
- **`Backend` enum + allowlist API**: `Backend::{Rust, MediaFoundation, VideoToolbox, MediaCodec, Vaapi, D3d11va}`. `DecoderConfig::with_backend(Backend::MediaFoundation)` or `with_backends(&[Backend::VideoToolbox, Backend::Rust])` configures the ordered allowlist; the dispatcher walks it on every decode and falls through on `BackendError::Unavailable` / `BackendError::Decode`. Commits 7d559a6, c0ac39c.
- **SPS metadata extraction**: `HvccParams` now carries the bitstream-coded dimensions (`coded_width`, `coded_height`), the conformance-window crop offsets (`crop_left/right/top/bottom`), and the SPS VUI color metadata (`full_range`, `matrix_coeffs`, `color_primaries`, `transfer_characteristics`). The parent crate parses these once from the first SPS NAL — emulation-prevention bytes stripped via `parse_single_nal` — and threads them through every backend. This fixes the example.heic chroma offset (max delta 255 → 0) and corrects every CICP-aware color decision on HDR / BT.2020 streams. Commits 7872ab4, 9d74221.

### Added — `heic-backend-mediafoundation` (Windows)
- Full Media Foundation Transform driver: `MFTEnumEx` → HEVC decoder MFT → `MF_MT_MPEG_SEQUENCE_HEADER` + AU-inline VPS/SPS/PPS for legacy AppX MFTs → `ProcessInput`/`END_OF_STREAM`/`DRAIN`/`ProcessOutput` dance with stream-change renegotiation → `IMF2DBuffer::Lock2D` with `GetContiguousLength`-aware aligned-height + negative-stride rebase → NV12/P010 unpack honoring SPS conformance window. Commits 208598f, f623336, 58f1ded, 9d74221.
- Runtime CI on `windows-11-arm` with HEVC Video Extensions side-loaded; 5/5 dispatch tests pass + a zensim-regress corpus diff vs the Rust backend (every fixture, including example.heic, hits 0 bad pixels). Commits f72a32e, 4cb9c8e.

### Added — `heic-backend-videotoolbox` (Apple)
- Full VideoToolbox FFI: `CMVideoFormatDescriptionCreateFromHEVCParameterSets` → `VTDecompressionSessionCreate` (cached per-dimensions across decodes) → `CMSampleBuffer`-wrapped hvcC slice → `VTDecompressionSession::decode_frame` with synchronous output callback → `CVPixelBufferLockBaseAddress` + per-plane unpack of NV12 (8-bit) or P010 (10-bit, LSB-aligned, low-10-bit masked — opposite of Windows MF's MSB alignment). Targets macOS, iOS device + simulator, tvOS, visionOS. Commit 9758e51, 8764ea7, 626adf6, 04690a8.
- **Fixed CMBlockBuffer ownership: passed `kCFAllocatorNull` for the borrowed `&[u8]` bitstream memory.** The previous `blockAllocator: None` translated to `kCFAllocatorDefault`, so CM tried to `free()` the slice's data pointer on `CMBlockBuffer` release → SIGABRT "Non-aligned pointer 0x...3bd being freed" → entire VT runtime path was unusable. Captured via the new `vt-debug.yml` lldb workflow's full crash backtrace. After the fix, `videotoolbox_vs_rust_corpus_diff` decodes the full bundled corpus on both `macos-latest` (arm64) and `macos-15-intel` — synth files at `max_delta=[21,20,20]` (VT's BT.709 chroma upsampling differs from the rust backend's reference but stays within perceptual tolerance), example.heic at `max_delta=[2,2,1]` similarity 99.05, apple-hdr/hdr-sample at `max_delta=[29,25,34]` arm64 / `[21,21,20]` Intel.

### Added — `vt-debug.yml` workflow_dispatch debug runner
- New manual-trigger workflow that runs the VT test inside `lldb --batch -k` with full thread backtraces + register state + image list + `MallocStackLogging` for offending-pointer alloc/free stacks, then collects every macOS crash report from `~/Library/Logs/DiagnosticReports/` and uploads as a `vt-debug-<os>` artifact. Optional `interactive: true` input opens an `mxschmitt/action-tmate@v3` SSH session after the diagnostic steps so a maintainer can attach lldb / dtrace / Instruments manually. Used to root-cause the CMBlockBuffer ownership bug above; lives in tree as the standing tool for future VT FFI triage.

### Added — `heic-backend-mediacodec` (Android)
- Full NDK `AMediaCodec` FFI: `createDecoderByType("video/hevc")` → `AMediaFormat` with KEY_WIDTH/HEIGHT + KEY_CSD_0 (Annex-B VPS+SPS+PPS) → `configure` with null surface (ByteBuffer mode) → input queue + EOS → output dequeue loop handling `INFO_OUTPUT_FORMAT_CHANGED` / `INFO_TRY_AGAIN_LATER` → per-color-format unpack for `COLOR_FormatYUV420Planar` (I420), `COLOR_FormatYUV420SemiPlanar` + `Flexible` (NV12), and `COLOR_FormatYUVP010` (10-bit). RAII teardown via `Cached`'s Drop. Commit 3352aa1.

### Added — `heic-backend-vaapi` (Linux)
- Full libva HEVC decode FFI (dlopen `libva.so.2` / `libva-drm.so.2`): probe → `vaCreateConfig`/`vaCreateContext`/`vaCreateSurfaces` → `VAPictureParameterBufferHEVC` + `VASliceParameterBufferHEVC` + IQ-matrix buffers → `vaBeginPicture`/`vaRenderPicture`/`vaEndPicture` → `vaDeriveImage`/`vaMapBuffer` NV12/P010 readback. Compile-only on hosted CI (no GPU on `ubuntu-latest`); runtime decode validated on a Linux+GPU host via `vaapi-runtime.yml`. Commits 0545266, 970e656.

### Added — kitchen-sink cross-backend corpus survey
- New `extended_corpus_survey` test gated on `HEIC_EXTENDED_CORPUS=<dir>` walks an arbitrary corpus directory (e.g. block-storage iPhone HEIC dumps) and exercises every compiled-in backend against every `.heic` / `.heif` file. Reports per-backend success counts to stderr. The test verifies decode succeeds + dimensions match — it's a survey-style sanity gate, not a pixel-parity check. Validated on the RTX 5070 + Windows MF: 9/9 iPhone HDR files (12 MP standard photos up to 42 MP panoramas at 11102×3828) decode successfully across all three Windows backends (Rust, MediaFoundation, D3D11VA).

### Added — `heic-backend-d3d11va` real GPU decode (RTX 5070, partial)
- Full DXVA HEVC decode pipeline: `D3D11CreateDevice` → `CheckVideoDecoderFormat` → `GetVideoDecoderConfig` (picks `ConfigBitstreamRaw == 1` short-format per spec) → `CreateVideoDecoder` → cached `DecoderSession` per (coded_w, coded_h, bit_depth) → per-frame `DecoderBeginFrame` → `GetDecoderBuffer` / `ReleaseDecoderBuffer` / `SubmitDecoderBuffers` for PICTURE_PARAMETERS + BITSTREAM + SLICE_CONTROL → `DecoderEndFrame` → staging-texture `CopyResource` + `Map` → NV12 / P010 unpack to planar `u16` with SPS conformance-window crop applied at copy time. Commits d19a678, 1dfe6d6, 4a6e030, 4c4348b, a4df66a.
- `DxvaPicParamsHevc` populator + `from_sps_pps()` constructor mirror chromium `media/gpu/windows/d3d11_h265_accelerator.cc::PicParamsFromSPS` + `PicParamsFromPPS`. Per-picture overrides (`CurrPic`, `CurrPicOrderCntVal`, `StatusReportFeedbackNumber`, IDR/IRAP/INTRA flags) layered in `Inner::decode`. Commit c911420, 9a068d5, ed6b466, 7f5b925.
- Bit-exact synthetic-corpus decode verified on the RTX 5070: all 4 `testdata/synthetic/*` files round-trip with `max_delta=0` / `similarity=100.0000` vs the rust backend. Regression test `d3d11va_vs_rust_synthetic_corpus` (`HEIC_D3D11VA_HW=1` gated) locks this in.
- Diagnostic harness behind `HEIC_D3D11VA_DEBUG=1`: prints SPS+PPS flag combinations, NAL framing, and first-pixel samples per decode call. Used to triage the failing fixtures.
- `INVERSE_QUANTIZATION_MATRIX` buffer submission via the new `DxvaQmatrixHevc` struct + `default_qmatrix_hevc()` constructor: when `sps.scaling_list_enabled_flag` is set, the host MUST submit the iq_matrix buffer per DXVA spec section 4.2 (chromium `d3d11_h265_accelerator.cc:559` does the same). Without it NVIDIA drivers silently produce midgray output. Adding the buffer with HEVC default scaling lists (spec tables 7-3 / 7-4) unblocked the entire Main10 P010 path. Verified on RTX 5070 against 9 real-world iPhone HDR files (12-42 MP) — every one decodes via D3D11VA + matches the rust backend's dimensions.
- Custom scaling-list propagation through `ParsedSps::scaling_list` / `ParsedPps::pps_scaling_list` (new `heic_core::sps::HevcScalingListData` type) + `qmatrix_from_parsed()` populator. The parent crate's HEVC parser already reads `scaling_list_data()` into `ScalingListData`; `populate_parsed_sps` / `populate_parsed_pps` now propagate it, and `Inner::decode` prefers the encoder's actual lists over HEVC defaults per spec 7.4.3.3.1 preference order (PPS > SPS > defaults).
- VCL NAL filtering in `Inner::decode`: the `BITSTREAM` buffer now contains only VCL slice NAL units (types 0..=9 and 16..=21 per HEVC spec table 7-1) — SEI prefix / SEI suffix / VPS / SPS / PPS / AUD / FD / EOS / EOB NALs are excluded. Without the filter, when an hvcC slice payload starts with `[SEI prefix][IDR slice]` (as `example.heic`'s full-image item does), the driver's slice parser at `BSNALunitDataLocation = 0` ends up reading the SEI NAL as a slice header → silent midgray output.
- **Net effect: all 6 of 6 bundled corpus files now decode via D3D11VA on the RTX 5070 with `similarity ≥ 99.0` against the rust backend** — synth (4 files bit-exact), apple-hdr/hdr-sample (99.23, max_delta=1), libheif-examples/example (99.05, max_delta=2). The full corpus regression test `d3d11va_vs_rust_corpus_diff` (gated on `HEIC_D3D11VA_HW=1`) is the bit-exact gate; the `d3d11va_vs_rust_synthetic_corpus` subset stays available for environments without GPU access to apple-hdr / example.heic test fixtures.

### Added — test infrastructure
- `tests/common/compare_backends_via_zensim`: shared corpus-sweep harness that drives any pair of `Backend` variants through the testdata corpus and gates inter-decoder drift via `zensim_regress::testing::check_regression`. Replaces the hand-rolled diff loop in `mediafoundation_vs_rust_corpus_diff`; new backend test files reuse it. Commit 9b04d6b.
- `examples/mf_diff`: per-row max-channel-delta + bad-pixel-count diagnostic for the MF backend. Locally verified that the VUI + crop fix takes example.heic from 4.48 % bad pixels to 0. Commit 9d74221.

### CI
- Per-backend matrix jobs in `.github/workflows/ci.yml`: compile-only on the cross targets (aarch64-pc-windows-msvc, x86_64-pc-windows-msvc, macos-latest, macos-15-intel, ubuntu-latest, aarch64-linux-android) + runtime on `windows-11-arm` for MF. Existing fmt / clippy / msrv / coverage jobs updated for the workspace + backend-rust feature.

## [0.1.6] - 2026-05-19

### Packaging
- Drop `testdata/` from the published crate. The directory is still present in the source repo for developer testing; tests now skip with `SKIP` messages when files are absent, so `cargo test` on the packaged crate stays green. Compressed crate shrinks from 1.2 MiB to 313 KiB (74% reduction); file count drops from 168 to 73.

## [0.1.5] - 2026-05-18

### Added
- HEIF Amendment 1 / ISO 23008-12:2025 `tmap` derived image item support (#8, f4156c1). `decode_gain_map` now detects either the Apple aux-item URN (existing) or a `tmap` derived item with `dimg` references to a base SDR image and a grayscale gain map. The new `HdrGainMap::iso21496` field carries the raw ISO 21496-1 binary metadata (AVIF tmap variant) when the source is `tmap`; `HdrGainMap::origin` (new `GainMapOrigin` enum) names which mechanism the gain map was decoded from. Parse the binary metadata via `zencodec::gainmap::parse_iso21496_fmt(_, Iso21496Format::AvifTmap)`. Probing via `ImageInfo::from_bytes` now reports `has_gain_map = true` for both paths.
- wasm32 SIMD parity for YCbCr→RGB 4:2:0 color conversion (#3, 0b2662e). The wasm128 path was previously a delegation to scalar; it is now a native `v128` implementation processing 8 pixels per outer iteration with `u32x4_*` arithmetic mirroring the AArch64 NEON layout. Math matches the scalar reference bit-for-bit. Closes the only remaining gap from the SIMD-platform-parity audit.

### Fixed
- Image overlay (`iovl`) descriptor parsing now matches ISO/IEC 23008-12 (40d4e34): 2-byte version+flags (not 4), always four u16 canvas fill entries (not a variable count derived from descriptor length). Fill values are interpreted as RGB and converted to YCbCr via the first tile's matrix/range before filling the canvas planes, matching libheif's RGB-space compositing. The Nokia `overlay_1000x680.heic` reference jumps from 13.1 dB PSNR to 74.6 dB.
- Convert WPP / tile entry point offsets from EBSP byte space to RBSP before seeking, so HEIC tiles whose slice data contains an emulation prevention byte (`0x000003`) inside a WPP substream no longer produce garbled rows past the first 0x03 byte (#12, 775c030)
- Reject SPS with `pic_width_in_luma_samples` / `pic_height_in_luma_samples` outside `1..=16384` and conformance-window offsets that exceed picture dimensions, closing a panic / multi-GiB allocation reachable from the default no-limits decode path (security audit CR-1, CR-2, H-3, 13d8663)
- `cropped_width` / `cropped_height` now use `saturating_sub` and `set_crop` clamps oversized offsets, so out-of-range crops cannot wrap to ~`u32::MAX` and reach `Vec::with_capacity` (CR-1, 13d8663)
- Promote pixel-index calculations in `to_bgra` / `to_bgr` / `to_rgba` / `to_rgb` / `get_chroma` 4:4:4 / `get_y` and `decode_alpha_plane` to `usize` before multiplication, defeating u32 overflow on 32-bit targets (H-1, 13d8663)
- Lower derived-image (iden / grid / iovl) recursion depth from 8 to 3 and add a per-request `decode_item` invocation cap of 32 768 to bound CPU cost from crafted fan-out graphs (H-2, 13d8663)
- Cap `parse_moov` track count at 16 so per-track sample / chunk / stsc tables cannot multiply unbounded (H-4, 13d8663)
- Poll the cancellation token inside the `resolve_sample_offset` chunk loop so a 1M-chunk stsc run remains responsive (H-5, 13d8663)
- Apply a sane default `Limits` (16 384×16 384, 256 Mpx, 1 GiB) when the caller does not supply one, replacing the previous all-`None` sentinel that bypassed every dimension and memory check (CR-2, 13d8663)

### Packaging
- Ship `testdata/` and `fuzz/regression/` in the published crate; route developer-corpus tests through `HEIC_TEST_CORPUS_DIR` and skip gracefully when the corpus is absent so `cargo test` is green out of the box on the packaged crate (#7, c8a83b3)

### Tests
- Harden the `#12` WPP-EP regression to sample stable YUV plane locations via `decode_to_frame` rather than 16×16 RGB averages, and clean up clippy warnings (#14, 775c030)

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
