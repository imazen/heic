//! Decode one file via a SPECIFIC backend, exiting non-zero on failure.
//!
//! Unlike `examples/decode.rs` (which walks the default allowlist and falls
//! back to the pure-Rust backend), this forces a single backend and does NOT
//! fall back — so a native-backend failure is a hard error. That is exactly
//! what the per-backend runtime CI gates need to PROVE the unsafe FFI actually
//! executes and decodes:
//!   * `mediacodec-runtime.yml` runs this on an Android emulator (`mediacodec`)
//!   * the self-hosted VA-API / D3D11VA GPU runners run it (`vaapi` / `d3d11va`)
//!
//! Usage: `backend_decode <file.heic> <backend>`
//!   backend ∈ rust | mediafoundation | videotoolbox | mediacodec | vaapi | d3d11va
//! Prints `OK <backend> <W>x<H>` and exits 0 on success; prints `ERR ...` and
//! exits 1 on decode failure (2 on usage / unknown-backend error).

use heic::{Backend, DecoderConfig, PixelLayout};

/// Map a backend name to a `Backend`, gated exactly like the enum variants so
/// it only resolves to a variant that is actually constructible on this build
/// + target.
fn backend_by_name(name: &str) -> Option<Backend> {
    match name {
        #[cfg(feature = "backend-rust")]
        "rust" => Some(Backend::Rust),
        #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
        "mediafoundation" => Some(Backend::MediaFoundation),
        #[cfg(all(
            feature = "backend-videotoolbox",
            any(
                target_os = "macos",
                target_os = "ios",
                target_os = "tvos",
                target_os = "visionos"
            )
        ))]
        "videotoolbox" => Some(Backend::VideoToolbox),
        #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
        "mediacodec" => Some(Backend::MediaCodec),
        #[cfg(all(feature = "backend-vaapi", target_os = "linux"))]
        "vaapi" => Some(Backend::Vaapi),
        #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
        "d3d11va" => Some(Backend::D3d11va),
        _ => None,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("Usage: {} <file.heic> <backend>", args[0]);
        std::process::exit(2);
    }
    let path = &args[1];
    let backend_name = &args[2];

    let Some(backend) = backend_by_name(backend_name) else {
        eprintln!("ERR backend '{backend_name}' is not compiled in / not available on this target");
        std::process::exit(2);
    };

    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("ERR read {path}: {e}");
            std::process::exit(1);
        }
    };

    // Single backend, NO pure-Rust fallback — the chosen backend must do the
    // work or this fails loudly.
    match DecoderConfig::new()
        .with_backend(backend)
        .decode(&data, PixelLayout::Rgba8)
    {
        Ok(out) => {
            assert_eq!(
                out.data.len(),
                out.width as usize * out.height as usize * 4,
                "RGBA8 buffer size disagrees with dimensions"
            );
            println!("OK {backend_name} {}x{}", out.width, out.height);
        }
        Err(e) => {
            eprintln!("ERR {backend_name} decode failed: {e:?}");
            std::process::exit(1);
        }
    }
}
