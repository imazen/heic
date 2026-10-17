//! Corpus decode gate: decode every committed `testdata/` HEIC/HEIF through the
//! pure-Rust backend and assert nothing panics. The bundled corpus (95 files,
//! incl. `example.heic`, the 10-bit `apple-hdr/hdr-sample.heic`, grids, and
//! uncompressed HEIF) ships in the source checkout, so this runs in CI WITHOUT
//! any download — closing the "CI only runs `cargo test --lib`" gap.
//!
//! NO graceful skip (per the project rule): if `testdata/` is missing or thin
//! the test FAILS, because a checkout / CI misconfiguration must be loud, not
//! silent. Individual files that legitimately don't decode with the enabled
//! features (brotli-compressed uncompressed HEIF, JPEG-in-HEIF, AV1 without the
//! `av1` feature) are allowed to return `Err` — they just must not panic.

use heic::{DecoderConfig, PixelLayout};
use std::path::{Path, PathBuf};

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// Recursively collect committed `.heic` / `.heif` files under `testdata/`.
fn corpus_files() -> Vec<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                walk(&p, out);
            } else if let Some(ext) = p.extension().and_then(|e| e.to_str())
                && (ext.eq_ignore_ascii_case("heic") || ext.eq_ignore_ascii_case("heif"))
            {
                out.push(p);
            }
        }
    }
    let mut out = Vec::new();
    walk(&testdata_dir(), &mut out);
    out.sort();
    out
}

/// HEVC-coded files that MUST decode via `backend-rust` (no `av1`/`unci`
/// dependency). `(relative path, expected width, expected height)`; a zero
/// dimension means "assert > 0 only".
const MUST_DECODE: &[(&str, u32, u32)] = &[
    ("libheif-examples/example.heic", 1280, 854),
    ("apple-hdr/hdr-sample.heic", 0, 0),
];

/// Decode every committed corpus file; assert no panic and that successful
/// decodes have consistent dimensions / buffer size.
#[test]
fn corpus_decodes_without_panic() {
    let files = corpus_files();
    assert!(
        files.len() >= 50,
        "testdata corpus missing or incomplete ({} files under {}); the committed \
         corpus must be present for this gate (no graceful skip)",
        files.len(),
        testdata_dir().display()
    );

    let mut decoded = 0usize;
    let mut clean_errored = 0usize;
    for path in &files {
        let data = std::fs::read(path).expect("read corpus file");
        // A panic inside decode aborts the test — that is exactly what this
        // gate is here to catch on untrusted-shaped input.
        match DecoderConfig::new().decode(&data, PixelLayout::Rgba8) {
            Ok(out) => {
                assert!(
                    out.width > 0 && out.height > 0,
                    "{}: decoded to zero dimensions",
                    path.display()
                );
                assert_eq!(
                    out.data.len(),
                    out.width as usize * out.height as usize * 4,
                    "{}: RGBA8 buffer length disagrees with dimensions",
                    path.display()
                );
                decoded += 1;
            }
            // Legitimate: codec/feature not enabled for this file. Must not panic.
            Err(_) => clean_errored += 1,
        }
    }
    eprintln!(
        "corpus: {} files, {} decoded, {} clean-errored",
        files.len(),
        decoded,
        clean_errored
    );
    assert!(
        decoded > 0,
        "no corpus file decoded — backend-rust may be broken"
    );
}

/// The HEVC must-decode set decodes via `backend-rust` with the expected
/// dimensions.
#[test]
fn must_decode_set_succeeds() {
    for &(rel, w, h) in MUST_DECODE {
        let path = testdata_dir().join(rel);
        let data = std::fs::read(&path)
            .unwrap_or_else(|_| panic!("missing required corpus file: {}", path.display()));
        let out = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgb8)
            .unwrap_or_else(|e| panic!("{rel} must decode via backend-rust, got {e:?}"));
        assert!(out.width > 0 && out.height > 0, "{rel}: zero dimensions");
        assert_eq!(
            out.data.len(),
            out.width as usize * out.height as usize * 3,
            "{rel}: RGB8 buffer length disagrees with dimensions"
        );
        if w > 0 {
            assert_eq!(
                (out.width, out.height),
                (w, h),
                "{rel}: unexpected dimensions"
            );
        }
    }
}
