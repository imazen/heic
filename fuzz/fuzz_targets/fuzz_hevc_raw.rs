#![no_main]

use libfuzzer_sys::fuzz_target;

/// Raw HEVC bitstream fuzzer — bypasses HEIF container parsing entirely.
/// Feeds arbitrary bytes directly to the HEVC Annex B decoder.
/// Most fuzz-found bugs (refpic, residual, intra, dequantize) are in the
/// HEVC decoder core, not the HEIF container parser.
fuzz_target!(|data: &[u8]| {
    let _ = heic::hevc::decode(data);
});
