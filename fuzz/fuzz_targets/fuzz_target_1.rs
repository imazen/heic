#![no_main]

use libfuzzer_sys::fuzz_target;

/// Decode fuzzer: arbitrary bytes → decoder
/// This is the primary attack surface — any crash, panic, or OOM is a bug.
fuzz_target!(|data: &[u8]| {
    // Try the full decode pipeline with default settings
    let _ = heic::DecoderConfig::new()
        .decode(data, heic::PixelLayout::Rgba8);
});
