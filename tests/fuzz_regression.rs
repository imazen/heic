//! Regression tests for fuzz-discovered bugs.
//!
//! Every file under `fuzz/regression/` triggered a crash, OOM or timeout
//! before its fix. This test replays each of them through **every entry point
//! the `fuzz/fuzz_targets/*` binaries drive**, and fails if any of them panics.
//!
//! ## Why this file carries its own seed-expectation machinery
//!
//! A regression suite that replays *zero* seeds passes — loudly, quickly, and
//! green — while testing nothing. Every way a corpus can go missing (renamed
//! directory, seeds swallowed by `.gitignore`, a path the target platform
//! refuses to open) lands on that same outcome. So the corpus scan below is
//! deliberately unforgiving: a missing or unreadable seed directory is a
//! **failure**, not a skip, and the replayed-seed count is pinned to what is
//! actually tracked in git.
//!
//! This mirrors the `min_seeds` / `RegressionReport` API of the shared
//! `zenutils-fuzz` crate, which this crate does not yet depend on. When that
//! API is published, migration is mechanical: delete the `regress` module,
//! `use zenutils_fuzz::{RegressionSuite, RegressionReport};`, and leave the
//! `RegressionSuite::new(..).min_seeds(..).target(..).run()` chain below
//! untouched.
//!
//! ## History (the bug this file's own guards exist to prevent)
//!
//! Until 2026-08-29 this harness:
//!
//! * treated a missing `fuzz/regression/` as `eprintln!("SKIP")` + `return` —
//!   a silent pass, and exactly the runtime self-skip the project rules ban;
//! * scanned the directory non-recursively, so the three seeds under
//!   `fuzz/regression/fuzz_hevc_raw/` were **never replayed at all**;
//! * asserted `count >= 10` against a 35-seed corpus, so 25 seeds could be
//!   deleted without the gate noticing;
//! * covered 3 of the 5 distinct entry points the fuzz targets drive (the
//!   unlimited `decode()` of `fuzz_target_1` and the colour-conversion
//!   surface of `fuzz_color_transform` were both unreplayed).

use heic::{DecoderConfig, Limits, PixelLayout};
use std::path::{Path, PathBuf};

use regress::RegressionSuite;

/// Number of seeds tracked under `fuzz/regression/` (including the
/// `fuzz_hevc_raw/` subdirectory; `README`-style meta files never count).
///
/// Pinned, not a floor-of-convenience: if a seed is deleted or a subdirectory
/// stops being scanned, this test fails and says how many went missing. Bump
/// it in the same commit that adds seeds.
const TRACKED_SEEDS: usize = 35;

fn regression_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regression")
}

fn strict_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_width = Some(4096);
    limits.max_height = Some(4096);
    limits.max_pixels = Some(4_000_000);
    limits.max_memory_bytes = Some(64 * 1024 * 1024);
    limits
}

/// Replay every regression seed through every fuzz entry point.
///
/// The five targets below cover all seven `fuzz/fuzz_targets/*` binaries.
/// `fuzz_decode_av1`, `fuzz_decode_unci` and `fuzz_decode_limits` drive the
/// same entry point with the same four caps — their sources differ only by a
/// trailing comment and an intermediate `let result =` binding that is
/// immediately discarded, and `fuzz/Cargo.toml` builds every binary from one
/// `heic` with `backend-rust,av1,unci`, so they are not distinguished by
/// features either. They exist as separate binaries so libFuzzer keeps a
/// separate corpus per input class; one `decode_limits` replay covers all
/// three.
#[test]
fn fuzz_regression() {
    let report = RegressionSuite::new(regression_dir())
        .min_seeds(TRACKED_SEEDS)
        // Mirrors fuzz_targets/fuzz_target_1.rs (the `fuzz_decode` binary):
        // the full pipeline at the decoder's own fallback caps.
        .target("decode_default", |data| {
            let _ = DecoderConfig::new().decode(data, PixelLayout::Rgba8);
        })
        // Mirrors fuzz_targets/fuzz_decode_limits.rs — and, to the same four
        // caps and the same call chain, fuzz_decode_av1.rs and
        // fuzz_decode_unci.rs.
        .target("decode_limits", |data| {
            let limits = strict_limits();
            let _ = DecoderConfig::new()
                .decode_request(data)
                .with_output_layout(PixelLayout::Rgba8)
                .with_limits(&limits)
                .decode();
        })
        // Mirrors fuzz_targets/fuzz_hevc_raw.rs. Most fuzz-found crashes live
        // in the HEVC core rather than the HEIF container, and are reached by
        // feeding bytes straight to this entry point — a raw HEVC seed is not
        // a valid HEIF file and bails at the box parser, so the container
        // targets above do NOT cover it.
        .target("hevc_raw", |data| {
            let _ = heic::hevc::decode(data);
        })
        // Mirrors fuzz_targets/fuzz_probe.rs.
        .target("probe", |data| {
            let _ = heic::ImageInfo::from_bytes(data);
        })
        // Mirrors fuzz_targets/fuzz_color_transform.rs: the seed bytes steer
        // frame geometry and colour signalling, then drive every YCbCr->RGB
        // conversion path (scalar + SIMD) exactly as the fuzz target does.
        .target("color_transform", replay_color_transform)
        .run();

    println!("{report}");
    assert_eq!(
        report.seeds_replayed(),
        TRACKED_SEEDS,
        "seed count drifted from the pinned value; update TRACKED_SEEDS in the \
         same commit that adds or removes a seed"
    );
}

/// Body of `fuzz/fuzz_targets/fuzz_color_transform.rs`, transcribed so a
/// regression seed exercises the colour-conversion surface too.
fn replay_color_transform(data: &[u8]) {
    use heic::DecodedFrame;

    if data.len() < 12 {
        return;
    }

    let width = (data[0] as u32 % 64) + 2; // 2..65
    let height = (data[1] as u32 % 64) + 2; // 2..65
    let bit_depth = if data[2] & 1 == 0 { 8u8 } else { 10u8 };
    let chroma_format = match data[3] % 3 {
        0 => 1u8, // 4:2:0
        1 => 2u8, // 4:2:2
        _ => 3u8, // 4:4:4
    };
    let full_range = data[4] & 1 != 0;
    let matrix_coeffs = match data[5] % 4 {
        0 => 1u8, // BT.709
        1 => 5u8, // BT.601
        2 => 9u8, // BT.2020
        _ => 2u8, // unspecified
    };
    let output_format = data[7] % 7;

    // Make dimensions even for 4:2:0.
    let width = if chroma_format == 1 {
        (width + 1) & !1
    } else {
        width
    };
    let height = if chroma_format == 1 {
        (height + 1) & !1
    } else {
        height
    };

    let y_size = (width * height) as usize;
    let (cw, ch) = match chroma_format {
        1 => (width / 2, height / 2),
        2 => (width / 2, height),
        _ => (width, height),
    };
    let c_size = (cw * ch) as usize;

    let rest = &data[8..];
    let max_val = (1u16 << bit_depth) - 1;

    let mut y_plane = vec![0u16; y_size];
    let mut cb_plane = vec![0u16; c_size];
    let mut cr_plane = vec![0u16; c_size];

    for (i, val) in y_plane.iter_mut().enumerate() {
        *val = if i < rest.len() {
            (rest[i] as u16) * (max_val / 255)
        } else {
            128
        };
    }
    let cb_offset = y_size.min(rest.len());
    for (i, val) in cb_plane.iter_mut().enumerate() {
        let idx = cb_offset + i;
        *val = if idx < rest.len() {
            (rest[idx] as u16) * (max_val / 255)
        } else {
            128
        };
    }
    let cr_offset = (cb_offset + c_size).min(rest.len());
    for (i, val) in cr_plane.iter_mut().enumerate() {
        let idx = cr_offset + i;
        *val = if idx < rest.len() {
            (rest[idx] as u16) * (max_val / 255)
        } else {
            128
        };
    }

    let Ok(mut frame) = DecodedFrame::with_params(width, height, bit_depth, chroma_format) else {
        return;
    };
    frame.y_plane = y_plane;
    frame.cb_plane = cb_plane;
    frame.cr_plane = cr_plane;
    frame.full_range = full_range;
    frame.matrix_coeffs = matrix_coeffs;

    match output_format {
        0 => {
            let _ = frame.to_rgb();
        }
        1 => {
            let _ = frame.to_rgba();
        }
        2 => {
            let _ = frame.to_bgra();
        }
        3 => {
            let _ = frame.to_bgr();
        }
        4 => {
            let _ = frame.to_rgb16();
        }
        5 => {
            let _ = frame.to_rgba16();
        }
        6 => {
            let mut buf = vec![0u8; (width * height * 3) as usize];
            frame.write_rgb_into(&mut buf);
        }
        _ => {}
    }
}

/// Local stand-in for `zenutils_fuzz::RegressionSuite`.
///
/// Same builder shape and same semantics as the shared crate's unpublished
/// seed-expectation API, so swapping this module out for the real one is a
/// two-line change. The one rule that matters: **the counter lives inside the
/// filter**, so the number this reports can never drift from the number it
/// actually replayed. Hand-rolled guards that count directory entries
/// separately from the walk are how `README.md` ends up counted as a seed.
mod regress {
    use std::fmt;
    use std::fs;
    use std::io;
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use std::path::{Path, PathBuf};

    type TargetFn = Box<dyn Fn(&[u8]) + Send + Sync>;

    /// Why scanning the seed directory did not produce a seed list.
    enum ScanError {
        /// The seed directory does not exist.
        Absent,
        /// The seed directory (or something inside it) could not be read, or
        /// the seed path is not a directory at all.
        Io { path: PathBuf, err: io::Error },
    }

    /// What a completed [`RegressionSuite::run`] actually did.
    pub struct RegressionReport {
        seed_dir: PathBuf,
        seed_paths: Vec<PathBuf>,
        target_count: usize,
    }

    impl RegressionReport {
        /// Number of seed files replayed through every target.
        pub fn seeds_replayed(&self) -> usize {
            self.seed_paths.len()
        }

        /// Number of registered targets.
        pub fn targets(&self) -> usize {
            self.target_count
        }
    }

    impl fmt::Display for RegressionReport {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(
                f,
                "fuzz regression: replayed {} seed(s) from {:?} through {} target(s) = {} invocation(s)",
                self.seeds_replayed(),
                self.seed_dir,
                self.targets(),
                self.seeds_replayed() * self.targets()
            )
        }
    }

    /// Builder + runner for a fuzz-regression seed corpus.
    pub struct RegressionSuite {
        seed_dir: PathBuf,
        targets: Vec<(String, TargetFn)>,
        min_seeds: Option<usize>,
    }

    impl RegressionSuite {
        pub fn new<P: Into<PathBuf>>(seed_dir: P) -> Self {
            Self {
                seed_dir: seed_dir.into(),
                targets: Vec::new(),
                min_seeds: None,
            }
        }

        /// Require the corpus to replay at least `n` seeds.
        ///
        /// The seed directory must exist and be readable; a missing,
        /// unreadable, empty or short corpus fails [`Self::run`] with a
        /// message saying which of those it was. `n` counts *replayed* seeds
        /// — dotfiles, `*.md` and `*.txt` never count, so a `README.md` in the
        /// corpus directory does not inflate the number passed here.
        pub fn min_seeds(mut self, n: usize) -> Self {
            self.min_seeds = Some(n);
            self
        }

        pub fn target<F>(mut self, name: &str, f: F) -> Self
        where
            F: Fn(&[u8]) + Send + Sync + 'static,
        {
            self.targets.push((name.to_string(), Box::new(f)));
            self
        }

        /// Replay every seed through every target.
        ///
        /// Panics — which is what a `#[test]` wants — if no seed expectation
        /// was declared, if no targets were registered, if the corpus does not
        /// meet the expectation, or if a target panics on a seed.
        pub fn run(self) -> RegressionReport {
            let Some(min_seeds) = self.min_seeds else {
                panic!(
                    "RegressionSuite at {:?}: no seed expectation declared, so this \
                     suite would pass without proving it replayed anything. Call \
                     `.min_seeds(n)`.",
                    self.seed_dir
                );
            };
            assert!(
                !self.targets.is_empty(),
                "RegressionSuite at {:?}: no targets registered. Call \
                 `.target(name, fn)` at least once before `.run()`.",
                self.seed_dir
            );

            let seeds = match collect_seeds(&self.seed_dir) {
                Ok(seeds) => seeds,
                Err(ScanError::Absent) => panic!(
                    "RegressionSuite: seed directory {:?} does not exist, but at least \
                     {min_seeds} seed(s) were required. The corpus was renamed, never \
                     checked out, or the path does not resolve on this target. A missing \
                     corpus is a FAILURE, never a skip: skipping would report green while \
                     replaying nothing.",
                    self.seed_dir
                ),
                Err(ScanError::Io { path, err }) => panic!(
                    "RegressionSuite: seed directory {:?} exists but could not be scanned \
                     ({path:?}: {err}). This is a broken harness, not an empty corpus: the \
                     suite would otherwise have replayed nothing and passed.",
                    self.seed_dir
                ),
            };

            assert!(
                seeds.len() >= min_seeds,
                "RegressionSuite: seed directory {:?} yielded {} seed(s) but at least \
                 {min_seeds} were required — {} seed(s) went missing. (Dotfiles, `*.md` \
                 and `*.txt` are never counted as seeds, so a directory holding only a \
                 README counts as empty.) Replayed: {:?}",
                self.seed_dir,
                seeds.len(),
                min_seeds - seeds.len(),
                seeds,
            );

            for seed_path in &seeds {
                let bytes = match fs::read(seed_path) {
                    Ok(b) => b,
                    Err(e) => {
                        panic!("RegressionSuite: failed to read seed {seed_path:?}: {e}")
                    }
                };

                for (target_name, target_fn) in &self.targets {
                    let res = catch_unwind(AssertUnwindSafe(|| target_fn(&bytes)));
                    if let Err(payload) = res {
                        panic!(
                            "RegressionSuite: target {target_name:?} panicked on seed \
                             {seed_path:?} ({} bytes, first 32: {:?}): {}",
                            bytes.len(),
                            &bytes[..bytes.len().min(32)],
                            panic_payload_str(&*payload),
                        );
                    }
                }
            }

            RegressionReport {
                seed_dir: self.seed_dir,
                seed_paths: seeds,
                target_count: self.targets.len(),
            }
        }
    }

    fn collect_seeds(dir: &Path) -> Result<Vec<PathBuf>, ScanError> {
        match fs::metadata(dir) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(ScanError::Io {
                    path: dir.to_path_buf(),
                    err: io::Error::other("seed path exists but is not a directory"),
                });
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(ScanError::Absent),
            Err(err) => {
                return Err(ScanError::Io {
                    path: dir.to_path_buf(),
                    err,
                });
            }
        }
        let mut seeds = Vec::new();
        walk(dir, &mut seeds)?;
        seeds.sort();
        Ok(seeds)
    }

    /// Recursive walk. Skips dotfiles (`.gitkeep`, `.DS_Store`) and the
    /// `*.md` / `*.txt` meta files a corpus directory carries alongside its
    /// seeds. Every I/O error propagates — a directory that cannot be read is
    /// a broken gate, not an empty one.
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ScanError> {
        let entries = fs::read_dir(dir).map_err(|err| ScanError::Io {
            path: dir.to_path_buf(),
            err,
        })?;
        for entry in entries {
            let entry = entry.map_err(|err| ScanError::Io {
                path: dir.to_path_buf(),
                err,
            })?;
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            if name.starts_with('.') {
                continue;
            }
            let ft = entry.file_type().map_err(|err| ScanError::Io {
                path: path.clone(),
                err,
            })?;
            if ft.is_dir() {
                walk(&path, out)?;
                continue;
            }
            if !ft.is_file() {
                continue;
            }
            let lower = name.to_ascii_lowercase();
            if lower.ends_with(".md") || lower.ends_with(".txt") {
                continue;
            }
            out.push(path);
        }
        Ok(())
    }

    fn panic_payload_str(payload: &(dyn std::any::Any + Send)) -> String {
        if let Some(s) = payload.downcast_ref::<&'static str>() {
            (*s).to_string()
        } else if let Some(s) = payload.downcast_ref::<String>() {
            s.clone()
        } else {
            "<non-string panic payload>".to_string()
        }
    }
}
