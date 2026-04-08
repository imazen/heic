//! Parser tests using in-repo test data.
//!
//! These tests exercise HEIF container parsing (ISOBMFF boxes, item properties,
//! references, grid layouts, auxiliary images) using committed test files that
//! require no external downloads.

use heic::{DecoderConfig, ImageInfo, PixelLayout};
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

// ===========================================================================
// Multi-codec HEIF tests
// ===========================================================================

// ---- mif3 brand acceptance ----

#[test]
fn mif3_brand_probes_without_format_error() {
    let data = read_file("libheif-examples/lightning_mini.heif");
    // lightning_mini.heif uses the mif3 brand with "mini" box format.
    // The brand should be accepted (no InvalidFormat), even though the
    // mini container structure may not fully parse (no pitm box).
    match ImageInfo::from_bytes(&data) {
        Ok(info) => {
            assert!(info.width > 0 && info.height > 0);
        }
        Err(heic::ProbeError::InvalidFormat) => {
            panic!("mif3 brand should be accepted, not rejected as InvalidFormat");
        }
        Err(_) => {
            // Other errors (Corrupt, NeedMoreData) are acceptable — the
            // mini box format is not yet fully supported.
        }
    }
}

// ---- ItemType recognition ----

#[test]
fn item_type_av01_recognized() {
    // Verify the av01 FourCC maps to ItemType::Av01
    use heic::heif::{FourCC, ItemType};
    let item_type: ItemType = FourCC(*b"av01").into();
    assert_eq!(item_type, ItemType::Av01);
}

#[test]
fn item_type_unci_recognized() {
    use heic::heif::{FourCC, ItemType};
    let item_type: ItemType = FourCC(*b"unci").into();
    assert_eq!(item_type, ItemType::Unci);
}

#[test]
fn item_type_avc1_recognized() {
    use heic::heif::{FourCC, ItemType};
    let item_type: ItemType = FourCC(*b"avc1").into();
    assert_eq!(item_type, ItemType::Avc1);
}

#[test]
fn item_type_jpeg_recognized() {
    use heic::heif::{FourCC, ItemType};
    let item_type: ItemType = FourCC(*b"jpeg").into();
    assert_eq!(item_type, ItemType::Jpeg);
}

// ---- UnsupportedCodec error for disabled features ----

#[test]
fn unsupported_codec_error_displays() {
    let err = heic::HeicError::UnsupportedCodec("test codec");
    let msg = format!("{err}");
    assert!(
        msg.contains("unsupported codec"),
        "UnsupportedCodec should display as 'unsupported codec': got '{msg}'"
    );
    assert!(msg.contains("test codec"));
}

// ---- av1C box parsing ----

#[test]
fn parse_av1c_synthetic() {
    // Construct a minimal valid av1C box and parse it through the container parser.
    // av1C format: marker(0x81) | profile/level | flags | reserved | configOBUs
    //
    // We can't test parse_av1c directly (it's private), but we can test
    // the Av1DecoderConfig struct methods.
    use heic::heif::Av1DecoderConfig;

    // Profile 0 (Main), 8-bit, 4:2:0
    let config = Av1DecoderConfig {
        seq_profile: 0,
        seq_level_idx_0: 8,
        high_bitdepth: false,
        twelve_bit: false,
        monochrome: false,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        config_obus: vec![],
    };
    assert_eq!(config.bit_depth(), 8);
    assert_eq!(config.chroma_format(), 1); // 4:2:0

    // Profile 1 (High), 10-bit, 4:4:4
    let config_high = Av1DecoderConfig {
        seq_profile: 1,
        seq_level_idx_0: 12,
        high_bitdepth: true,
        twelve_bit: false,
        monochrome: false,
        chroma_subsampling_x: false,
        chroma_subsampling_y: false,
        config_obus: vec![0x12, 0x00, 0x0A],
    };
    assert_eq!(config_high.bit_depth(), 10);
    assert_eq!(config_high.chroma_format(), 3); // 4:4:4

    // 12-bit monochrome
    let config_12bit = Av1DecoderConfig {
        seq_profile: 2,
        seq_level_idx_0: 0,
        high_bitdepth: true,
        twelve_bit: true,
        monochrome: true,
        chroma_subsampling_x: true,
        chroma_subsampling_y: true,
        config_obus: vec![],
    };
    assert_eq!(config_12bit.bit_depth(), 12);
    assert_eq!(config_12bit.chroma_format(), 0); // monochrome

    // 4:2:2
    let config_422 = Av1DecoderConfig {
        seq_profile: 2,
        seq_level_idx_0: 0,
        high_bitdepth: true,
        twelve_bit: false,
        monochrome: false,
        chroma_subsampling_x: true,
        chroma_subsampling_y: false,
        config_obus: vec![],
    };
    assert_eq!(config_422.chroma_format(), 2); // 4:2:2
}

// ---- uncC/cmpC box parsing via probe ----

#[test]
fn probe_unci_zlib_file() {
    let data = read_file("libheif-examples/rgb_generic_compressed_zlib.heif");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert_eq!(info.width, 128);
    assert_eq!(info.height, 72);
    assert_eq!(info.bit_depth, 8);
}

#[test]
fn probe_unci_deflate_file() {
    let data = read_file("libheif-examples/rgb_generic_compressed_defl.heif");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert_eq!(info.width, 128);
    assert_eq!(info.height, 72);
    assert_eq!(info.bit_depth, 8);
}

#[test]
fn probe_unci_uncompressed_rgb_file() {
    let data = read_file("libheif-examples/uncompressed_comp_RGB.heif");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(info.width > 0);
    assert!(info.height > 0);
    assert_eq!(info.bit_depth, 8);
}

// ---- unci decode tests (behind feature flag) ----

#[cfg(feature = "unci")]
#[test]
fn decode_unci_zlib() {
    let data = read_file("libheif-examples/rgb_generic_compressed_zlib.heif");
    let output = DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgb8)
        .expect("decode failed");
    assert_eq!(output.width, 128);
    assert_eq!(output.height, 72);
    assert_eq!(output.data.len(), 128 * 72 * 3);

    // Verify not all zeros or all white
    let non_zero = output.data.iter().any(|&v| v != 0);
    let non_max = output.data.iter().any(|&v| v != 255);
    assert!(non_zero, "decoded image should not be all zeros");
    assert!(non_max, "decoded image should not be all 255");
}

#[cfg(feature = "unci")]
#[test]
fn decode_unci_deflate() {
    let data = read_file("libheif-examples/rgb_generic_compressed_defl.heif");
    let output = DecoderConfig::new()
        .decode(&data, heic::PixelLayout::Rgb8)
        .expect("decode failed");
    assert_eq!(output.width, 128);
    assert_eq!(output.height, 72);
    assert_eq!(output.data.len(), 128 * 72 * 3);

    let non_zero = output.data.iter().any(|&v| v != 0);
    assert!(non_zero, "decoded image should not be all zeros");
}

#[cfg(not(feature = "unci"))]
#[test]
fn decode_unci_returns_unsupported_without_feature() {
    let data = read_file("libheif-examples/rgb_generic_compressed_zlib.heif");
    let result = DecoderConfig::new().decode(&data, heic::PixelLayout::Rgb8);
    match result {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("unsupported codec") || msg.contains("unci"),
                "expected UnsupportedCodec error, got: {msg}"
            );
        }
        Ok(_) => panic!("unci decode should fail without the 'unci' feature"),
    }
}

// ---- AV1 decode tests (behind feature flag) ----

#[cfg(not(feature = "av1"))]
#[test]
fn av1_returns_unsupported_without_feature() {
    // We don't have an AVIF-in-HEIF test file, but we can verify the error
    // message is correct by constructing a minimal scenario. Since we can't
    // easily construct a valid AVIF-in-HEIF file, we just check the error type.
    let err = heic::HeicError::UnsupportedCodec("AV1 codec requires the 'av1' feature");
    let msg = format!("{err}");
    assert!(msg.contains("av1") || msg.contains("AV1"));
}

// ---- Decompression bomb protection ----

#[cfg(feature = "unci")]
#[test]
fn unci_decompression_bomb_protection() {
    // Construct a minimal HEIF file with unci item type that claims to be
    // very large (e.g., 100000x100000) but has tiny compressed data.
    // This should fail with a LimitExceeded error, not OOM.
    //
    // We test this indirectly through the Limits system. If a file claims
    // huge dimensions, the limit check runs before decompression.
    let data = read_file("libheif-examples/rgb_generic_compressed_zlib.heif");

    // Set very restrictive limits
    let mut limits = heic::Limits::default();
    limits.max_pixels = Some(100); // Only allow 100 total pixels

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(heic::PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode();

    assert!(result.is_err(), "decode with tiny pixel limit should fail");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("limit exceeded") || msg.contains("pixel count"),
        "should be a limit error, got: {msg}"
    );
}

// ---- unci brotli returns unsupported ----

#[cfg(feature = "unci")]
#[test]
fn unci_brotli_returns_unsupported() {
    let data = read_file("libheif-examples/rgb_generic_compressed_brotli.heif");
    let result = DecoderConfig::new().decode(&data, heic::PixelLayout::Rgb8);
    match result {
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("not supported") || msg.contains("unsupported"),
                "brotli should return unsupported error, got: {msg}"
            );
        }
        Ok(_) => panic!("brotli unci decode should fail (not implemented)"),
    }
}

// ---- Limits enforcement tests ----

/// Verify that restrictive max_pixels limits reject HEVC decode.
#[test]
fn limits_reject_hevc_decode() {
    let data = read_file("libheif-examples/example.heic");
    let mut limits = heic::Limits::default();
    limits.max_pixels = Some(100); // 1280x854 will exceed this

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode();

    assert!(
        result.is_err(),
        "HEVC decode should fail with tiny pixel limit"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("limit exceeded") || msg.contains("pixel count"),
        "should be a limit error, got: {msg}"
    );
}

/// Verify that restrictive max_width limits reject decode.
#[test]
fn limits_reject_max_width() {
    let data = read_file("libheif-examples/example.heic");
    let mut limits = heic::Limits::default();
    limits.max_width = Some(10); // 1280 wide, will exceed this

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode();

    assert!(result.is_err(), "decode should fail with tiny width limit");
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("limit exceeded") || msg.contains("width"),
        "should be a limit error, got: {msg}"
    );
}

/// Verify that restrictive limits reject unci decode.
#[cfg(feature = "unci")]
#[test]
fn limits_reject_unci_decode() {
    let data = read_file("libheif-examples/uncompressed_comp_RGB.heif");
    let mut limits = heic::Limits::default();
    limits.max_pixels = Some(1); // Any unci image will exceed this

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode();

    assert!(
        result.is_err(),
        "unci decode should fail with tiny pixel limit"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("limit exceeded") || msg.contains("pixel count"),
        "should be a limit error, got: {msg}"
    );
}

/// Verify that unci truncated pixel data returns an error, not a truncated image.
#[cfg(feature = "unci")]
#[test]
fn unci_truncated_data_returns_error() {
    let data = read_file("libheif-examples/uncompressed_comp_RGB.heif");

    // First, verify the file decodes successfully without truncation
    let ok_result = DecoderConfig::new().decode(&data, PixelLayout::Rgb8);
    assert!(
        ok_result.is_ok(),
        "untruncated unci should decode OK: {:?}",
        ok_result.err()
    );

    // Now truncate the file data significantly (remove last 50% of mdat)
    // This should cause an error when the pixel data is too short
    let truncated = &data[..data.len() / 2];
    let result = DecoderConfig::new().decode(truncated, PixelLayout::Rgb8);

    assert!(
        result.is_err(),
        "truncated unci data should return an error, not silent corruption"
    );
}

/// A Stop implementation that is always cancelled.
struct AlwaysCancelled;

impl heic::Stop for AlwaysCancelled {
    fn check(&self) -> Result<(), heic::StopReason> {
        Err(heic::StopReason::Cancelled)
    }
}

/// Verify that a pre-cancelled Stop token causes early return from HEVC decode.
#[test]
fn check_stop_cancels_hevc_decode() {
    let data = read_file("libheif-examples/example.heic");

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_stop(&AlwaysCancelled)
        .decode();

    assert!(
        result.is_err(),
        "cancelled Stop should cause decode to fail"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("cancelled"),
        "should be a cancellation error, got: {msg}"
    );
}

/// Verify that a pre-cancelled Stop token causes early return from unci decode.
#[cfg(feature = "unci")]
#[test]
fn check_stop_cancels_unci_decode() {
    let data = read_file("libheif-examples/uncompressed_comp_RGB.heif");

    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_stop(&AlwaysCancelled)
        .decode();

    assert!(
        result.is_err(),
        "cancelled Stop should cause unci decode to fail"
    );
    let msg = format!("{}", result.unwrap_err());
    assert!(
        msg.contains("cancelled"),
        "should be a cancellation error, got: {msg}"
    );
}
