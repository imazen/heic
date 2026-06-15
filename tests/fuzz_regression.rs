//! Regression tests for fuzz-discovered bugs.
//!
//! Each file in `fuzz/regression/` triggered a crash or OOM before the fix.
//! These tests verify they decode (or error) without panicking.

use heic::{DecoderConfig, Limits, PixelLayout};
use std::path::Path;

fn regression_dir() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regression")
}

/// Run all regression inputs through the full decode pipeline with strict limits.
/// Must not panic — errors are fine.
#[test]
fn fuzz_regression_decode() {
    let dir = regression_dir();
    if !dir.exists() {
        eprintln!("SKIP: fuzz/regression/ not found");
        return;
    }

    let mut limits = Limits::default();
    limits.max_width = Some(4096);
    limits.max_height = Some(4096);
    limits.max_pixels = Some(4_000_000);
    limits.max_memory_bytes = Some(64 * 1024 * 1024);

    let mut count = 0;
    for entry in std::fs::read_dir(&dir).expect("read regression dir") {
        let path = entry.expect("entry").path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path).expect("read file");
        let _name = path.file_name().unwrap().to_string_lossy().to_string();

        // Must not panic — any Result (Ok or Err) is acceptable
        let _ = DecoderConfig::new()
            .decode_request(&data)
            .with_output_layout(PixelLayout::Rgba8)
            .with_limits(&limits)
            .decode();

        count += 1;
    }
    assert!(
        count >= 10,
        "expected at least 10 regression files, got {count}"
    );
}

/// Run all regression inputs through the raw HEVC decoder (`heic::hevc::decode`).
///
/// Most fuzz-found crashes live in the HEVC core, not the HEIF container, and
/// are reached via `fuzz_hevc_raw` — which feeds bytes straight to this entry
/// point, bypassing container parsing. The container `decode()` test above does
/// NOT exercise that path (a raw HEVC seed isn't a valid HEIF file and bails at
/// the box parser), so without this the raw-bitstream regressions (e.g. the
/// slice_segment_address OOB, heic#26) silently wouldn't be covered. Must not
/// panic — any `Result` is acceptable.
#[test]
fn fuzz_regression_hevc_raw() {
    let dir = regression_dir();
    if !dir.exists() {
        eprintln!("SKIP: fuzz/regression/ not found");
        return;
    }
    for entry in std::fs::read_dir(&dir).expect("read regression dir") {
        let path = entry.expect("entry").path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path).expect("read file");
        let _ = heic::hevc::decode(&data);
    }
}

/// Run all regression inputs through the probe pipeline.
/// Must not panic.
#[test]
fn fuzz_regression_probe() {
    let dir = regression_dir();
    if !dir.exists() {
        return;
    }

    for entry in std::fs::read_dir(&dir).expect("read regression dir") {
        let path = entry.expect("entry").path();
        if !path.is_file() {
            continue;
        }
        let data = std::fs::read(&path).expect("read file");
        let _ = heic::ImageInfo::from_bytes(&data);
    }
}
