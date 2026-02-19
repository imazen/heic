#![no_main]

use libfuzzer_sys::fuzz_target;
use heic_decoder::ImageInfo;

/// Probe fuzzer: test the lightweight header parsing path.
/// ImageInfo::from_bytes should never panic on arbitrary input.
fuzz_target!(|data: &[u8]| {
    let _ = ImageInfo::from_bytes(data);
});
