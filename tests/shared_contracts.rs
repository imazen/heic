#![cfg(all(feature = "zencodec", feature = "backend-rust"))]
use heic::HeicDecoderConfig;
use std::borrow::Cow;
use zencodec::decode::{Decode, DecodeJob, DecodePolicy, DecoderConfig};

#[test]
fn native_fixture_policy_and_inventory_contracts() {
    let bytes = include_bytes!("../testdata/synthetic/synth_8bit_q95.heic");
    let dec = HeicDecoderConfig::new();
    let expected = dec
        .clone()
        .job()
        .decoder(Cow::Borrowed(bytes.as_slice()), &[])
        .unwrap()
        .decode()
        .unwrap()
        .metadata();
    for (policy, accepted) in [
        (DecodePolicy::none(), true),
        (DecodePolicy::strict(), false),
    ] {
        zencodec_testkit::check_decode_policy_admission(
            dec.clone(),
            bytes,
            DecodePolicy::none(),
            policy,
            accepted,
            &expected,
        )
        .unwrap();
    }
    zencodec_testkit::check_inventory_dispatch(dec.clone(), bytes).unwrap();
    #[cfg(feature = "inventory")]
    zencodec_testkit::check_inventory_input_limit(dec, bytes).unwrap();
}
