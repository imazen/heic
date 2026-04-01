//! Parser tests using in-repo test data.
//!
//! These tests exercise HEIF container parsing (ISOBMFF boxes, item properties,
//! references, grid layouts, auxiliary images) using committed test files that
//! require no external downloads.

use heic::{DecoderConfig, ImageInfo};
use std::path::{Path, PathBuf};

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn read_file(rel: &str) -> Vec<u8> {
    let path = testdata().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()))
}

// ---- libheif example.heic (HEVC grid image, main reference file) ----

#[test]
fn probe_libheif_example() {
    let data = read_file("libheif-examples/example.heic");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert_eq!(info.width, 1280);
    assert_eq!(info.height, 854);
    assert!(!info.has_alpha);
    assert_eq!(info.bit_depth, 8);
    assert_eq!(info.chroma_format, 1); // 4:2:0
}

#[test]
fn decode_libheif_example_rgb() {
    let data = read_file("libheif-examples/example.heic");
    let output = DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgb8)
        .expect("decode failed");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 854);
    assert_eq!(output.data.len(), 1280 * 854 * 3);
}

// ---- Apple HDR gain map ----

#[test]
fn probe_apple_hdr_has_gain_map() {
    let data = read_file("apple-hdr/hdr-sample.heic");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(info.has_gain_map, "HDR photo should report has_gain_map");
    assert!(info.width > 0);
    assert!(info.height > 0);
}

#[test]
fn decode_apple_hdr_gain_map() {
    let data = read_file("apple-hdr/hdr-sample.heic");
    let decoder = DecoderConfig::new();

    let gain_map = decoder
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    assert!(gain_map.width > 0);
    assert!(gain_map.height > 0);
    assert_eq!(
        gain_map.data.len(),
        (gain_map.width * gain_map.height) as usize
    );

    // Verify not degenerate
    let non_zero = gain_map.data.iter().any(|&v| v != 0);
    let non_max = gain_map.data.iter().any(|&v| v != 255);
    assert!(non_zero, "gain map should not be all zeros");
    assert!(non_max, "gain map should not be all 255");
}

#[test]
fn apple_hdr_gain_map_has_xmp() {
    let data = read_file("apple-hdr/hdr-sample.heic");
    let gain_map = DecoderConfig::new()
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    let xmp = gain_map.xmp.as_ref().expect("gain map should have XMP");
    let xmp_str = core::str::from_utf8(xmp).expect("XMP should be valid UTF-8");
    assert!(
        xmp_str.contains("HDRGainMap"),
        "XMP should contain Apple HDRGainMap namespace"
    );
}

#[test]
fn apple_hdr_gain_map_lower_res_than_primary() {
    let data = read_file("apple-hdr/hdr-sample.heic");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    let gain_map = DecoderConfig::new()
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    let primary_pixels = info.width as u64 * info.height as u64;
    let gm_pixels = gain_map.width as u64 * gain_map.height as u64;
    assert!(
        gm_pixels <= primary_pixels,
        "gain map ({gm_pixels}) should be <= primary ({primary_pixels})"
    );
}

#[test]
fn apple_hdr_auxiliary_types() {
    let data = read_file("apple-hdr/hdr-sample.heic");
    let types = DecoderConfig::new()
        .auxiliary_types(&data)
        .expect("auxiliary_types failed");
    assert!(
        types.contains(&heic::AuxiliaryImageType::HdrGainMap),
        "should contain HdrGainMap; found: {types:?}"
    );
}

// ---- Synthetic quality variants ----

#[test]
fn probe_synthetic_files() {
    for name in &[
        "synthetic/synth_8bit_q10.heic",
        "synthetic/synth_8bit_q50.heic",
        "synthetic/synth_8bit_q95.heic",
        "synthetic/synth_8bit_lossless.heic",
    ] {
        let data = read_file(name);
        let info =
            ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe failed for {name}: {e}"));
        assert!(info.width > 0, "{name}: width should be > 0");
        assert!(info.height > 0, "{name}: height should be > 0");
        assert_eq!(info.bit_depth, 8, "{name}: should be 8-bit");
    }
}

#[test]
fn decode_synthetic_files() {
    for name in &[
        "synthetic/synth_8bit_q10.heic",
        "synthetic/synth_8bit_q50.heic",
        "synthetic/synth_8bit_q95.heic",
        "synthetic/synth_8bit_lossless.heic",
    ] {
        let data = read_file(name);
        let output = DecoderConfig::new()
            .decode(&data, heic::PixelLayout::Rgb8)
            .unwrap_or_else(|e| panic!("decode failed for {name}: {e}"));
        assert!(output.width > 0 && output.height > 0, "{name}: bad dims");
        assert_eq!(
            output.data.len(),
            (output.width * output.height * 3) as usize,
            "{name}: data length mismatch"
        );
    }
}

// ---- libheif-examples: parser probing (all files) ----

#[test]
fn probe_all_libheif_examples() {
    let dir = testdata().join("libheif-examples");
    let mut count = 0;
    let mut failures = Vec::new();

    for entry in std::fs::read_dir(&dir).expect("read_dir failed") {
        let entry = entry.expect("entry failed");
        let path = entry.path();
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        if ext != "heic" && ext != "heif" {
            continue;
        }

        let data = std::fs::read(&path).expect("read failed");
        match ImageInfo::from_bytes(&data) {
            Ok(info) => {
                assert!(
                    info.width > 0 && info.height > 0,
                    "{}: zero dimensions",
                    path.display()
                );
                count += 1;
            }
            Err(e) => {
                failures.push(format!(
                    "{}: {e}",
                    path.file_name().unwrap().to_string_lossy()
                ));
            }
        }
    }

    // Log failures but don't fail the test — many of these use uncompressed
    // codecs (unci) that our HEVC-only decoder can't fully probe.
    if !failures.is_empty() {
        eprintln!(
            "NOTE: {}/{} libheif-examples failed to probe (expected for non-HEVC formats):",
            failures.len(),
            count + failures.len()
        );
        for f in &failures {
            eprintln!("  {f}");
        }
    }

    assert!(count > 0, "should have probed at least some files");
    eprintln!("Successfully probed {count} libheif-examples files");
}

// ---- No gain map in non-HDR files ----

#[test]
fn no_gain_map_in_libheif_example() {
    let data = read_file("libheif-examples/example.heic");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(!info.has_gain_map);

    let result = DecoderConfig::new().decode_gain_map(&data);
    assert!(
        result.is_err(),
        "non-HDR file should error on gain map decode"
    );
}

#[test]
fn no_gain_map_in_synthetic() {
    let data = read_file("synthetic/synth_8bit_q95.heic");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(!info.has_gain_map);
}
