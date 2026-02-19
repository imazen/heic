#![no_main]

use libfuzzer_sys::fuzz_target;
use heic_decoder::{DecoderConfig, Limits, PixelLayout};

/// Limits fuzzer: verify Limits enforcement under adversarial input.
/// Decode with strict resource limits — should never exceed them.
fuzz_target!(|data: &[u8]| {
    let limits = Limits {
        max_width: Some(4096),
        max_height: Some(4096),
        max_pixels: Some(4_000_000),
        max_memory_bytes: Some(64 * 1024 * 1024), // 64 MB
    };

    let result = DecoderConfig::new()
        .decode_request(data)
        .with_output_layout(PixelLayout::Rgba8)
        .with_limits(&limits)
        .decode();

    // The decode may succeed or fail with an error — both are fine.
    // What matters is that it doesn't panic, OOM, or exceed the limits.
    let _ = result;
});
