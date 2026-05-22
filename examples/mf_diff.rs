//! Diagnostic for the example.heic MF↔Rust drift.
//!
//! Decode a HEIC via both backends and print the per-row max-channel-delta
//! and bad-pixel count. The spatial pattern (bottom band, scattered, edge)
//! tells us whether the issue is chroma-plane offset, edge handling, or
//! per-row noise.
//!
//! Build + run on Windows from the repo root:
//!
//! ```pwsh
//! $env:CARGO_HOME='V:\packages\.cargo'
//! $env:CARGO_TARGET_DIR='V:\packages\heic-target'
//! cargo run --example mf_diff \
//!     --features backend-rust,backend-mediafoundation,std \
//!     --target x86_64-pc-windows-msvc \
//!     -- testdata/libheif-examples/example.heic
//! ```

#[cfg(all(
    feature = "backend-rust",
    feature = "backend-mediafoundation",
    target_os = "windows"
))]
fn main() {
    use heic::{Backend, DecoderConfig, PixelLayout};

    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "testdata/libheif-examples/example.heic".to_string());
    let data = std::fs::read(&path).expect("read input");

    let rust = DecoderConfig::new()
        .with_backend(Backend::Rust)
        .decode(&data, PixelLayout::Rgba8)
        .expect("rust decode");
    let mf = DecoderConfig::new()
        .with_backend(Backend::MediaFoundation)
        .decode(&data, PixelLayout::Rgba8)
        .expect("MF decode");

    assert_eq!((rust.width, rust.height), (mf.width, mf.height));
    let (w, h) = (rust.width as usize, rust.height as usize);
    println!("Dimensions: {w}x{h}");
    println!(
        "{:>5}  {:>5}  {:>5}  {:>5}  {:>6}",
        "row", "maxR", "maxG", "maxB", "bad"
    );

    let mut summary_total_bad = 0u32;
    let mut summary_rows_with_bad = 0u32;
    for y in 0..h {
        let mut mr: u32 = 0;
        let mut mg: u32 = 0;
        let mut mb: u32 = 0;
        let mut bad = 0u32;
        for x in 0..w {
            let off = (y * w + x) * 4;
            let dr = (rust.data[off] as i32 - mf.data[off] as i32).unsigned_abs();
            let dg = (rust.data[off + 1] as i32 - mf.data[off + 1] as i32).unsigned_abs();
            let db = (rust.data[off + 2] as i32 - mf.data[off + 2] as i32).unsigned_abs();
            mr = mr.max(dr);
            mg = mg.max(dg);
            mb = mb.max(db);
            if dr.max(dg).max(db) > 32 {
                bad += 1;
            }
        }
        if bad > 0 {
            summary_total_bad += bad;
            summary_rows_with_bad += 1;
            println!("{y:>5}  {mr:>5}  {mg:>5}  {mb:>5}  {bad:>6}");
        }
    }
    println!(
        "summary: {} bad pixels across {} rows ({:.2}% of image)",
        summary_total_bad,
        summary_rows_with_bad,
        summary_total_bad as f64 / (w * h) as f64 * 100.0
    );
}

#[cfg(not(all(
    feature = "backend-rust",
    feature = "backend-mediafoundation",
    target_os = "windows"
)))]
fn main() {
    eprintln!(
        "mf_diff requires --features backend-rust,backend-mediafoundation and target_os=windows"
    );
}
