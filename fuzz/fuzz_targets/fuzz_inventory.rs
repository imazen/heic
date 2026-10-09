#![no_main]

use heic::HeicDecoderConfig;
use libfuzzer_sys::fuzz_target;
use zencodec::decode::{DecodeJob, DecoderConfig};

/// Structural inventory fuzzer: `DecodeJob::inventory` must never panic, and
/// every inventory it returns must cover the input exactly and pass
/// `Inventory::validate`. The first byte also toggles the job options that
/// change dispositions (gain-map and depth extraction).
fuzz_target!(|data: &[u8]| {
    let flags = data.first().copied().unwrap_or(0);
    let config = HeicDecoderConfig::new()
        .with_extract_gain_map(flags & 1 != 0)
        .with_extract_depth(flags & 2 != 0);
    if let Ok(Some(inv)) = config.job().inventory(data) {
        assert_eq!(inv.input_len(), data.len() as u64);
        if let Err(e) = inv.validate() {
            panic!("invalid inventory: {e}\n{inv}");
        }
    }
});
