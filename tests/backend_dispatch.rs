//! End-to-end test of the backend allowlist dispatcher.
//!
//! Each test exercises a single backend through `DecoderConfig::with_backend`
//! and verifies that decode either succeeds with sensible output or falls
//! through to a later allowlist entry. The Rust backend is always exercised
//! (PR2 plumbing); native backends run only when their target_os matches
//! and the corresponding feature is enabled.
//!
//! Test fixture: `testdata/libheif-examples/example.heic` — a 1280x854
//! HEIC from the libheif examples directory, already used by the existing
//! conformance tests.

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
/// should produce the frame; otherwise the dispatcher falls through to
/// Rust and the decode still succeeds.
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
/// `HEIC_REQUIRE_MF_HEVC=1` env var; CI sets that on runners where the
/// extension package is confirmed installed.
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
