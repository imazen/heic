//! Real portrait regression. Oracle: libheif 1.21.2 + libde265 1.0.16.
//! Pinned input and exact full-plane sample hashes, never RGB/metric tolerances.
use codec_corpus::Corpus;
use heic as crate_under_test;
use sha2::{Digest, Sha256};

#[test]
fn late_cu_qp_delta_updates_all_depth_deblocking_samples() {
    let corpus = Corpus::new().unwrap();
    let root = corpus.zip_url("openize-heic-1640f6ed", "https://github.com/openize-com/openize-heic-net/archive/1640f6ed26dd4038b86ed9d548a7b34c2cdd7ba1.zip").unwrap();
    let path = root.join("Openize.Heic.Tests/TestsData/samples/iphone_portrait_photo.heic");
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(
        sha256(&bytes),
        "c782e03d1ef2a709f6a84ff1b1612ef2dfa909e4872958d3975e15c79ab523c3"
    );
    let native = crate_under_test::DecoderConfig::new()
        .decode_depth_with_limits(
            &bytes,
            &crate_under_test::Limits::default(),
            &enough::Unstoppable,
        )
        .unwrap();
    // Independent libheif 1.21.2 + libde265 oracle; reproduce with
    // tools/heic_depth_oracle.py. Hash includes every u16 code, row packed LE.
    assert_eq!(
        (native.width, native.height, native.bit_depth),
        (576, 768, 8)
    );
    assert_eq!(
        native.depth_info.representation_type,
        crate_under_test::DepthRepresentationType::UniformDisparity
    );
    assert_eq!(native.depth_info.d_min, Some(0.53662109375));
    assert_eq!(native.depth_info.d_max, Some(3.56640625));
    let raw: Vec<u8> = native.data.iter().flat_map(|v| v.to_le_bytes()).collect();
    assert_eq!(
        sha256(&raw),
        "4fc1e83b7bd9328f894b808c718ea94f24f2713824f4f215fd8a2de589622abc"
    );
    let mut tight = crate_under_test::Limits::default();
    tight.max_pixels = Some(1);
    assert!(
        crate_under_test::DecoderConfig::new()
            .decode_depth_with_limits(&bytes, &tight, &enough::Unstoppable)
            .is_err()
    );
}
fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
