//! End-to-end test of the backend allowlist dispatcher.
//!
//! Each test exercises a single backend through `DecoderConfig::with_backend`
//! and verifies that decode either succeeds with sensible output or falls
//! through to a later allowlist entry. The Rust backend is always exercised
//! (PR2 plumbing); native backends run only when their `target_os` matches
//! and the corresponding feature is enabled.
//!
//! Test fixture: `testdata/libheif-examples/example.heic` — a 1280×854
//! HEIC from the libheif examples directory, already used by the existing
//! conformance tests.

#![allow(unused_imports)] // not every config builds every test

mod common;

use heic::{Backend, DecoderConfig, PixelLayout};

const EXAMPLE_HEIC: &str = "testdata/libheif-examples/example.heic";

fn read_example() -> Vec<u8> {
    std::fs::read(EXAMPLE_HEIC).expect("example.heic should be in testdata/")
}

#[cfg(feature = "backend-rust")]
#[test]
fn rust_backend_decodes_example() {
    let data = read_example();
    let output = DecoderConfig::new()
        .with_backend(Backend::Rust)
        .decode(&data, PixelLayout::Rgba8)
        .expect("Rust backend should decode example.heic");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
    assert_eq!(output.data.len(), 1280 * 854 * 4);
}

/// MediaFoundation slow path: select MF first, fall through to Rust on
/// failure. On a Windows host with HEVC Video Extensions installed, MF
/// produces the frame; otherwise the dispatcher falls through to Rust
/// and the decode still succeeds.
#[cfg(all(
    feature = "backend-rust",
    feature = "backend-mediafoundation",
    target_os = "windows"
))]
#[test]
fn mediafoundation_fallthrough_to_rust_succeeds() {
    let data = read_example();
    let output = DecoderConfig::new()
        .with_backends(&[Backend::MediaFoundation, Backend::Rust])
        .decode(&data, PixelLayout::Rgba8)
        .expect("MF→Rust allowlist should decode example.heic");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
}

/// MediaFoundation alone — fails decode if HEVC Video Extensions are
/// missing (legitimate on Windows Server / fresh installs). Gated by
/// `HEIC_REQUIRE_MF_HEVC=1` so CI runners without the AppX skip cleanly.
#[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
#[test]
fn mediafoundation_alone_decodes_when_required() {
    if std::env::var_os("HEIC_REQUIRE_MF_HEVC").is_none() {
        eprintln!(
            "HEIC_REQUIRE_MF_HEVC not set: skipping MF-alone decode test. \
             Set it on runners with the HEVC Video Extensions package \
             installed."
        );
        return;
    }
    let data = read_example();
    let output = DecoderConfig::new()
        .with_backend(Backend::MediaFoundation)
        .decode(&data, PixelLayout::Rgba8)
        .expect(
            "MediaFoundation alone should decode example.heic when \
             HEIC_REQUIRE_MF_HEVC is set",
        );
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
    assert_eq!(output.data.len(), 1280 * 854 * 4);
}

/// Cross-backend corpus sweep using the shared
/// [`common::compare_backends_via_zensim`] helper. Treats the rust
/// backend as ground truth and gates MF output via
/// [`zensim_regress::testing::check_regression`].
///
/// Tolerance: documented inter-decoder rounding noise — chroma upsample
/// + matrix-coefficient drift can produce ~21-step deltas; allow up to
/// 32 channel-steps and require ≥40 % perceptual similarity. With the
/// VUI + SPS-crop plumbing landed, every file in the bundled corpus
/// (including example.heic) hits 0 bad pixels.
#[cfg(all(
    feature = "backend-rust",
    feature = "backend-mediafoundation",
    target_os = "windows"
))]
#[test]
fn mediafoundation_vs_rust_corpus_diff() {
    use zensim_regress::testing::RegressionTolerance;

    if std::env::var_os("HEIC_REQUIRE_MF_HEVC").is_none() {
        eprintln!("HEIC_REQUIRE_MF_HEVC not set: skipping corpus diff");
        return;
    }
    let tolerance = RegressionTolerance::off_by_one()
        .with_max_delta(32)
        .with_max_pixels_different(1.0)
        .with_min_similarity(40.0);
    let report = common::compare_backends_via_zensim(
        Backend::Rust,
        Backend::MediaFoundation,
        &tolerance,
        common::CORPUS_DIRS,
    );
    eprintln!(
        "MF↔Rust zensim diff: {}/{} matched",
        report.matched, report.total
    );
    report.assert_clean("MF↔Rust corpus");
}

/// VideoToolbox on Apple: decode example.heic via the VT backend only
/// and assert dimensions match. VT is documented available on every
/// macOS 10.13+ / iOS 11+ release, so this is a no-skip test on Apple
/// targets — if it fails on CI, the VT FFI has regressed.
#[cfg(all(
    feature = "backend-videotoolbox",
    any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    )
))]
#[test]
fn videotoolbox_decodes_example() {
    let data = read_example();
    let output = DecoderConfig::new()
        .with_backend(Backend::VideoToolbox)
        .decode(&data, PixelLayout::Rgba8)
        .expect("VideoToolbox should decode example.heic on Apple targets");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
    assert_eq!(output.data.len(), 1280 * 854 * 4);
}

/// VideoToolbox vs Rust corpus diff via zensim-regress. Same tolerance
/// as the MF↔Rust comparison since both native decoders apply similar
/// chroma upsampling + matrix-coefficient rounding.
#[cfg(all(
    feature = "backend-rust",
    feature = "backend-videotoolbox",
    any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    )
))]
#[test]
fn videotoolbox_vs_rust_corpus_diff() {
    use zensim_regress::testing::RegressionTolerance;
    let tolerance = RegressionTolerance::off_by_one()
        .with_max_delta(32)
        .with_max_pixels_different(1.0)
        .with_min_similarity(40.0);
    let report = common::compare_backends_via_zensim(
        Backend::Rust,
        Backend::VideoToolbox,
        &tolerance,
        common::CORPUS_DIRS,
    );
    eprintln!(
        "VT↔Rust zensim diff: {}/{} matched",
        report.matched, report.total
    );
    report.assert_clean("VT↔Rust corpus");
}

/// Empty allowlist must produce `HeicError::NoBackendSelected`.
#[cfg(feature = "backend-rust")]
#[test]
fn empty_allowlist_errors_cleanly() {
    let data = read_example();
    let result = DecoderConfig::new()
        .with_backends(&[])
        .decode(&data, PixelLayout::Rgba8);
    let err = result.expect_err("empty allowlist should fail");
    let msg = format!("{}", err.error());
    assert!(
        msg.contains("no HEVC backend selected"),
        "unexpected error: {msg}"
    );
}
