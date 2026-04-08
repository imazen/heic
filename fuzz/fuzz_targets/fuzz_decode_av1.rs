#![no_main]

use libfuzzer_sys::fuzz_target;
use heic::{DecoderConfig, Limits, PixelLayout};

/// AV1 decode fuzzer: exercise the av1 feature path with strict limits.
/// Any crash, panic, or uncontrolled OOM is a bug.
fuzz_target!(|data: &[u8]| {
    let mut limits = Limits::default();
    limits.max_width = Some(4096);
    limits.max_height = Some(4096);
    limits.max_pixels = Some(4_000_000);
    limits.max_memory_bytes = Some(64 * 1024 * 1024);

    let _ = DecoderConfig::new()
        .decode_request(data)
        .with_output_layout(PixelLayout::Rgba8)
        .with_limits(&limits)
        .decode();
});
