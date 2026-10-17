//! End-to-end VA-API runtime decode against the bundled corpus.
//!
//! Run with:
//!
//! ```bash
//! LIBVA_DRIVER_NAME=nvidia NVD_BACKEND=egl \
//!     cargo run --release --example vaapi_decode_test \
//!     --features "backend-rust,backend-vaapi,std"
//! ```
//!
//! On WSL2 with the patched nvidia-vaapi-driver installed at
//! `/usr/lib/x86_64-linux-gnu/dri/nvidia_drv_video.so` this should
//! decode every file in `tests/testdata/` via the VA-API backend
//! and report a per-file similarity vs the rust backend's output.
//!
//! `Backend::Vaapi` only exists on `target_os = "linux"` (Cargo
//! `required-features` cannot express a target_os predicate), so the body is
//! cfg-gated the same way `examples/mf_diff.rs` gates its Windows-only path.

#[cfg(all(
    feature = "backend-rust",
    feature = "backend-vaapi",
    target_os = "linux"
))]
mod imp {
    use heic::{Backend, DecoderConfig, PixelLayout};
    use std::path::Path;

    pub fn run() -> Result<(), Box<dyn std::error::Error>> {
        let corpus_dirs = [
            "testdata/libheif-examples",
            "testdata/synthetic",
            "testdata/apple-hdr",
        ];
        for dir in corpus_dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                eprintln!("--- {dir} not found, skipping");
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|e| e != "heic") {
                    continue;
                }
                test_file(&path)?;
            }
        }
        Ok(())
    }

    fn test_file(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
        let data = std::fs::read(path)?;
        let display = path.display();
        // Decode via rust backend (reference).
        let _ = display;
        let rust = match DecoderConfig::new()
            .with_backend(Backend::Rust)
            .decode(&data, PixelLayout::Rgba8)
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!("--- {display}\n  Rust backend failed: {}", e.error());
                return Ok(());
            }
        };

        // Decode via VA-API.
        let display = path.display();
        let vaapi = match DecoderConfig::new()
            .with_backend(Backend::Vaapi)
            .decode(&data, PixelLayout::Rgba8)
        {
            Ok(o) => o,
            Err(e) => {
                eprintln!(
                    "--- {display}\n  VA-API failed: {} (rust: {}x{})",
                    e.error(),
                    rust.width,
                    rust.height
                );
                return Ok(());
            }
        };

        if rust.width != vaapi.width || rust.height != vaapi.height {
            eprintln!(
                "--- {display}\n  DIM MISMATCH rust {}x{} vs vaapi {}x{}",
                rust.width, rust.height, vaapi.width, vaapi.height
            );
            return Ok(());
        }

        // Per-channel max delta + mean abs delta.
        let mut max_delta = [0u8; 3];
        let mut sum_abs = [0u64; 3];
        let mut diff_pix = 0u64;
        for (r, v) in rust.data.chunks_exact(4).zip(vaapi.data.chunks_exact(4)) {
            let mut differs = false;
            for c in 0..3 {
                let d = r[c].abs_diff(v[c]);
                if d > 0 {
                    differs = true;
                }
                if d > max_delta[c] {
                    max_delta[c] = d;
                }
                sum_abs[c] += u64::from(d);
            }
            if differs {
                diff_pix += 1;
            }
        }
        let pixels = (rust.width as u64) * (rust.height as u64);
        let pct_diff = 100.0 * (diff_pix as f64) / (pixels as f64);
        let mean_r = (sum_abs[0] as f64) / (pixels as f64);
        let mean_g = (sum_abs[1] as f64) / (pixels as f64);
        let mean_b = (sum_abs[2] as f64) / (pixels as f64);
        eprintln!(
            "OK {display}: {}x{} max=[{},{},{}] mean=[{:.2},{:.2},{:.2}] diff={:.2}%",
            rust.width,
            rust.height,
            max_delta[0],
            max_delta[1],
            max_delta[2],
            mean_r,
            mean_g,
            mean_b,
            pct_diff
        );
        Ok(())
    }
}

#[cfg(all(
    feature = "backend-rust",
    feature = "backend-vaapi",
    target_os = "linux"
))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    imp::run()
}

#[cfg(not(all(
    feature = "backend-rust",
    feature = "backend-vaapi",
    target_os = "linux"
)))]
fn main() {
    eprintln!(
        "vaapi_decode_test requires --features backend-rust,backend-vaapi and target_os=linux"
    );
}
