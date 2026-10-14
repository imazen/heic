//! Incremental (sans-I/O) decode through `dyn_incremental_decoder`: the
//! testkit's incremental contract on committed HEIC files.
#![cfg(all(feature = "zencodec", feature = "backend-rust"))]

use heic::HeicDecoderConfig;
use zencodec::decode::DecoderConfig as _;

#[test]
fn incremental_contract_holds() {
    assert!(HeicDecoderConfig::capabilities().incremental());
    for name in [
        "features/single.heic",
        "features/bt709.heic",
        "features/lossless.heic",
        "features/depth10.heic",
    ] {
        let path = format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"));
        let data = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
        zencodec_testkit::check_incremental_capability_honesty(HeicDecoderConfig::new(), &data)
            .unwrap_or_else(|e| panic!("{name}: {e:?}"));
    }
}
