//! Print recommended backend order + try to decode a tiny HEIC via each.
//!
//! Useful for debugging the allowlist dispatcher and confirming that
//! `recommended_backends()` returns sensible values on the current host.
//!
//! ```sh
//! cargo run --example probe_backends \
//!     --features "backend-rust,backend-vaapi,backend-d3d11va,std"
//! ```

#[cfg(feature = "backend-rust")]
fn main() {
    let order = heic::recommended_backends();
    println!("recommended_backends() = {order:?}");
    for backend in &order {
        let result = heic::DecoderConfig::new()
            .with_backend(*backend)
            .decode(b"\0\0\0\0", heic::PixelLayout::Rgba8);
        // A 4-byte garbage input is expected to fail with a decode error;
        // the interesting signal is the error message, which surfaces
        // whether the backend got past its probe.
        match result {
            Ok(_) => println!("  {backend:?}: unexpectedly decoded garbage"),
            Err(e) => println!("  {backend:?}: {}", e.error()),
        }
    }
}

#[cfg(not(feature = "backend-rust"))]
fn main() {
    eprintln!("probe_backends requires --features backend-rust");
}
