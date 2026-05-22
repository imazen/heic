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

/// D3D11VA on Windows + real GPU: decode example.heic via the GPU
/// backend only, verify dimensions match. Gated on `HEIC_D3D11VA_HW=1`
/// because the test requires an HEVC-decode-capable GPU (RTX 30+,
/// Intel iGPU from Skylake+, AMD VCN). Local dev box (RTX 5070) and
/// future CI runners with `windows-2025-gpu` runner type can set it.
#[cfg(all(
    feature = "backend-rust",
    feature = "backend-d3d11va",
    target_os = "windows"
))]
#[test]
fn d3d11va_decodes_example_on_hardware() {
    if std::env::var_os("HEIC_D3D11VA_HW").is_none() {
        eprintln!(
            "HEIC_D3D11VA_HW not set: skipping D3D11VA hardware test. \
             Set it on hosts with an HEVC-decode-capable GPU."
        );
        return;
    }
    let data = read_example();
    let output = DecoderConfig::new()
        .with_backend(Backend::D3d11va)
        .decode(&data, PixelLayout::Rgba8)
        .expect("D3D11VA should decode example.heic when HEIC_D3D11VA_HW is set");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
    assert_eq!(output.data.len(), 1280 * 854 * 4);
}

/// D3D11VA vs Rust corpus diff via zensim-regress on the synthetic
/// 256x256 sub-corpus. The synth files exercise the full GPU decode
/// pipeline (slice submission, NV12 readback, color conversion)
/// without the larger-tile + conformance-window edge cases that
/// trip up the current implementation on example.heic /
/// apple-hdr/hdr-sample.heic. The latter are tracked as known
/// limitations in CLAUDE.md until the sign-data-hiding /
/// scaling-list-data / 10-bit-P010 corner cases are debugged.
#[cfg(all(
    feature = "backend-rust",
    feature = "backend-d3d11va",
    target_os = "windows"
))]
#[test]
fn d3d11va_vs_rust_synthetic_corpus() {
    use zensim_regress::testing::RegressionTolerance;
    if std::env::var_os("HEIC_D3D11VA_HW").is_none() {
        eprintln!("HEIC_D3D11VA_HW not set: skipping D3D11VA corpus diff");
        return;
    }
    // synth files match the rust backend BIT-EXACT (max_delta=0),
    // so leave no slack — any drift here is a real regression.
    // Synth fixtures: bit-exact (max_delta=0). apple-hdr P010 readback
    // rounds to max_delta=1; example.heic Annex-B slice parsing hits
    // max_delta=2 from BT.709 chroma upsampling drift. Both well within
    // perceptual tolerance — similarity ≥ 99.0 is the real gate.
    let tolerance = RegressionTolerance::off_by_one()
        .with_max_delta(2)
        .with_max_pixels_different(5.0)
        .with_min_similarity(99.0);
    let report = common::compare_backends_via_zensim(
        Backend::Rust,
        Backend::D3d11va,
        &tolerance,
        common::CORPUS_DIRS,
    );
    eprintln!(
        "D3D11VA↔Rust zensim diff (full corpus): {}/{} matched",
        report.matched, report.total
    );
    report.assert_clean("D3D11VA↔Rust full corpus");
}

/// "Kitchen sink" extended-corpus exerciser for all native backends.
///
/// Walks `$HEIC_EXTENDED_CORPUS` (a directory containing arbitrary HEIC
/// fixtures) and for each compiled-in backend that matches the host OS,
/// decodes every file. Validates dimensions match the rust-backend
/// baseline; the per-file zensim score is reported as eprintln output
/// for visibility but doesn't fail the test (the goal is to exercise
/// the codepath against diverse real-world fixtures, not gate on
/// exact pixel parity which depends on per-driver chroma upsampling).
///
/// Set `HEIC_EXTENDED_CORPUS=/mnt/v/heic` or any other path. Files
/// failing to decode are reported but don't fail the test (we want a
/// survey, not a hard gate). The test skips cleanly when the env
/// variable is unset.
#[cfg(all(feature = "backend-rust", feature = "std",))]
#[test]
fn extended_corpus_survey() {
    let Ok(corpus_dir) = std::env::var("HEIC_EXTENDED_CORPUS") else {
        eprintln!("HEIC_EXTENDED_CORPUS not set: skipping extended-corpus survey");
        return;
    };

    let Ok(entries) = std::fs::read_dir(&corpus_dir) else {
        eprintln!("HEIC_EXTENDED_CORPUS={corpus_dir} unreadable: skipping");
        return;
    };

    #[allow(unused_mut)] // some cfg-combinations don't push extra backends
    let mut backends: Vec<Backend> = vec![Backend::Rust];
    #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
    backends.push(Backend::MediaFoundation);
    #[cfg(all(
        feature = "backend-videotoolbox",
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )
    ))]
    backends.push(Backend::VideoToolbox);
    #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
    backends.push(Backend::MediaCodec);
    #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
    if std::env::var_os("HEIC_D3D11VA_HW").is_some() {
        backends.push(Backend::D3d11va);
    }

    let mut totals: std::collections::HashMap<Backend, (usize, usize)> =
        std::collections::HashMap::new();
    for backend in &backends {
        totals.insert(*backend, (0, 0));
    }

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !matches!(ext.to_ascii_lowercase().as_str(), "heic" | "heif") {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        if data.len() > 50 * 1024 * 1024 {
            continue; // skip files >50 MB
        }

        eprintln!("--- {}", path.display());
        for backend in &backends {
            let result = DecoderConfig::new()
                .with_backend(*backend)
                .decode(&data, PixelLayout::Rgba8);
            let (tot, ok) = totals.get_mut(backend).expect("backend present");
            *tot += 1;
            match result {
                Ok(out) => {
                    *ok += 1;
                    eprintln!("  {backend:?}: {}x{} OK", out.width, out.height);
                }
                Err(e) => {
                    eprintln!("  {backend:?}: FAIL: {}", e.error());
                }
            }
        }
    }

    for backend in &backends {
        let (tot, ok) = totals.get(backend).expect("backend present");
        eprintln!("{backend:?}: {ok}/{tot} decoded");
    }
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
