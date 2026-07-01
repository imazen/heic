//! Shared test helpers for cross-backend conformance.
//!
//! [`compare_backends_via_zensim`] is the workhorse: it sweeps a list of
//! corpus directories, decodes each HEIC file via two backends, and uses
//! `zensim_regress::check_regression` to gate inter-decoder drift. Tests
//! parameterize the pair of backends + the `RegressionTolerance` they
//! expect; this lets every native backend reuse the same harness instead
//! of duplicating the loop in `tests/backend_dispatch.rs`.

#![allow(dead_code)] // not every backend's test file uses every helper

use heic::{Backend, DecoderConfig, PixelLayout};

/// Default corpus directories the harness sweeps when the caller doesn't
/// override. Add directories here once when you bring new fixtures in
/// rather than per-test-file.
pub const CORPUS_DIRS: &[&str] = &[
    "testdata/libheif-examples",
    "testdata/synthetic",
    "testdata/apple-hdr",
];

/// Reinterpret a packed RGBA `&[u8]` (length divisible by 4) as `&[[u8; 4]]`.
/// `zensim::RgbaSlice` expects the pixel-array view; the underlying bytes
/// are identical, so this is a zero-copy cast.
pub fn pack_rgba(bytes: &[u8]) -> &[[u8; 4]] {
    // SAFETY: caller guarantees `bytes.len() % 4 == 0`. `[u8; 4]` has the
    // same alignment (1) as `u8`, and both are plain bytes.
    let len = bytes.len() / 4;
    unsafe { core::slice::from_raw_parts(bytes.as_ptr().cast::<[u8; 4]>(), len) }
}

/// Outcome of one backend vs backend comparison sweep.
#[derive(Debug, Default)]
pub struct DiffReport {
    /// Number of corpus files compared (includes both successful and failed).
    pub total: usize,
    /// Number of corpus files that passed the zensim tolerance check.
    pub matched: usize,
    /// Failure messages — one per file that failed, formatted for easy
    /// inclusion in an `assert!` panic message.
    pub failures: Vec<String>,
}

impl DiffReport {
    pub fn assert_clean(&self, label: &str) {
        if !self.failures.is_empty() {
            panic!(
                "{label} ({}/{} matched):\n{}",
                self.matched,
                self.total,
                self.failures.join("\n")
            );
        }
    }
}

/// Run a backend-vs-backend zensim comparison over every `.heic` file in
/// the given corpus directories.
///
/// - `reference`: the backend treated as ground truth.
/// - `under_test`: the backend whose output is compared against it.
/// - `tolerance`: zensim regression tolerance applied per file. Tune
///   per-backend based on inter-decoder rounding noise — typical native
///   ↔ rust comparisons need `max_delta=32`, `max_pixels_different=1.0`,
///   `min_similarity=40.0` once VUI / crop plumbing is correct.
/// - `dirs`: corpus directories to walk; missing directories are skipped.
///
/// Returns a [`DiffReport`] the caller can inspect or pass to
/// [`DiffReport::assert_clean`].
#[cfg(all(feature = "backend-rust", feature = "std"))]
pub fn compare_backends_via_zensim(
    reference: Backend,
    under_test: Backend,
    tolerance: &zensim_regress::testing::RegressionTolerance,
    dirs: &[&str],
) -> DiffReport {
    use zensim::{RgbaSlice, Zensim, ZensimProfile};
    use zensim_regress::testing::check_regression;

    let zensim = Zensim::new(ZensimProfile::A);
    let mut report = DiffReport::default();

    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "heic") {
                continue;
            }
            report.total += 1;
            let data = match std::fs::read(&path) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let ref_out = match DecoderConfig::new()
                .with_backend(reference)
                .decode(&data, PixelLayout::Rgba8)
            {
                Ok(o) => o,
                Err(e) => {
                    report.failures.push(format!(
                        "{}: {reference:?} decode failed: {e}",
                        path.display()
                    ));
                    continue;
                }
            };

            let test_out = match DecoderConfig::new()
                .with_backend(under_test)
                .decode(&data, PixelLayout::Rgba8)
            {
                Ok(o) => o,
                Err(e) => {
                    report.failures.push(format!(
                        "{}: {under_test:?} decode failed: {e}",
                        path.display()
                    ));
                    continue;
                }
            };

            if ref_out.width != test_out.width || ref_out.height != test_out.height {
                report.failures.push(format!(
                    "{}: dim mismatch — {reference:?} {}x{} vs {under_test:?} {}x{}",
                    path.display(),
                    ref_out.width,
                    ref_out.height,
                    test_out.width,
                    test_out.height
                ));
                continue;
            }

            let w = ref_out.width as usize;
            let h = ref_out.height as usize;
            let ref_pixels = pack_rgba(&ref_out.data);
            let test_pixels = pack_rgba(&test_out.data);
            let ref_src = RgbaSlice::new(ref_pixels, w, h);
            let test_src = RgbaSlice::new(test_pixels, w, h);

            match check_regression(&zensim, &ref_src, &test_src, tolerance) {
                Ok(r) if r.passed() => {
                    eprintln!(
                        "OK {}: {w}x{h} similarity={:.4} max_delta={:?}",
                        path.display(),
                        r.score(),
                        r.max_channel_delta()
                    );
                    report.matched += 1;
                }
                Ok(r) => {
                    report.failures.push(format!(
                        "{}: zensim regression failed — score={:.4} max_delta={:?}\n  {r}",
                        path.display(),
                        r.score(),
                        r.max_channel_delta()
                    ));
                }
                Err(e) => {
                    report
                        .failures
                        .push(format!("{}: zensim error: {e}", path.display()));
                }
            }
        }
    }
    report
}
