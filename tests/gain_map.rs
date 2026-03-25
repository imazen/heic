//! Integration tests for HDR gain map extraction.
//!
//! These tests are `#[ignore]` by default since they require real HEIC files
//! with HDR gain maps. Set `HEIC_TEST_DIR` to point to your test file directory.

use heic::{AuxiliaryImageType, DecoderConfig, ImageInfo};

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

/// iPhone photo with HDR gain map (Apple HDR format).
fn hdr_heic() -> String {
    format!(
        "{}/test-images/classic-car-iphone12pro.heic",
        heic_base_dir()
    )
}

/// Non-HDR HEIC file (Nokia conformance — no auxiliary images at all).
fn no_gainmap_heic() -> String {
    format!(
        "{}/test-images/nokia-conformance/conformance_files/C003.heic",
        heic_base_dir()
    )
}

// ---- Detection tests (require real files) ----

#[test]
#[ignore]
fn test_hdr_photo_has_gain_map() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.has_gain_map(&data).expect("has_gain_map failed");
    assert!(result, "HDR photo should have a gain map");
}

#[test]
#[ignore]
fn test_hdr_photo_info_has_gain_map() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(info.has_gain_map, "HDR photo should report has_gain_map");
}

#[test]
#[ignore]
fn test_no_gainmap_photo_detection() {
    let data = std::fs::read(no_gainmap_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.has_gain_map(&data).expect("has_gain_map failed");
    assert!(!result, "non-HDR photo should not have a gain map");
}

#[test]
#[ignore]
fn test_no_gainmap_photo_info() {
    let data = std::fs::read(no_gainmap_heic()).expect("Failed to read test file");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(
        !info.has_gain_map,
        "non-HDR photo should not report has_gain_map"
    );
}

// ---- Decode tests ----

#[test]
#[ignore]
fn test_decode_gain_map_returns_valid_pixels() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let gain_map = decoder
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    assert!(gain_map.width > 0, "gain map width should be nonzero");
    assert!(gain_map.height > 0, "gain map height should be nonzero");
    assert_eq!(
        gain_map.data.len(),
        (gain_map.width * gain_map.height) as usize,
        "data length should match width * height"
    );
    assert!(
        gain_map.bit_depth == 8 || gain_map.bit_depth == 10,
        "bit depth should be 8 or 10, got {}",
        gain_map.bit_depth
    );

    // Verify not all zeros or all 255
    let non_zero = gain_map.data.iter().any(|&v| v != 0);
    let non_max = gain_map.data.iter().any(|&v| v != 255);
    assert!(non_zero, "gain map should not be all zeros");
    assert!(non_max, "gain map should not be all 255");

    println!(
        "Gain map: {}x{}, bit_depth={}",
        gain_map.width, gain_map.height, gain_map.bit_depth
    );

    let min = gain_map.data.iter().copied().min().unwrap_or(0);
    let max = gain_map.data.iter().copied().max().unwrap_or(0);
    println!("Pixel range: {min}..{max}");
}

#[test]
#[ignore]
fn test_gain_map_lower_resolution_than_primary() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    let gain_map = decoder
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    // Gain maps are typically lower resolution than the primary image
    let primary_pixels = info.width as u64 * info.height as u64;
    let gainmap_pixels = gain_map.width as u64 * gain_map.height as u64;

    println!(
        "Primary: {}x{} ({} pixels), Gain map: {}x{} ({} pixels)",
        info.width, info.height, primary_pixels, gain_map.width, gain_map.height, gainmap_pixels,
    );

    assert!(
        gainmap_pixels <= primary_pixels,
        "gain map should be lower or equal resolution to primary"
    );
}

#[test]
#[ignore]
fn test_decode_gain_map_no_gainmap_returns_error() {
    let data = std::fs::read(no_gainmap_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.decode_gain_map(&data);
    assert!(
        result.is_err(),
        "decode_gain_map on a file without a gain map should fail"
    );
}

#[test]
#[ignore]
fn test_gain_map_auxiliary_types_include_hdrgainmap() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let types = decoder
        .auxiliary_types(&data)
        .expect("auxiliary_types failed");
    assert!(
        types.contains(&AuxiliaryImageType::HdrGainMap),
        "auxiliary types should include HdrGainMap; found: {:?}",
        types
    );
}

#[test]
#[ignore]
fn test_gain_map_xmp_present() {
    let data = std::fs::read(hdr_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let gain_map = decoder
        .decode_gain_map(&data)
        .expect("decode_gain_map failed");

    // Apple HDR HEIC files typically have XMP with gain map metadata,
    // but this depends on the specific file. Log the result either way.
    if let Some(ref xmp) = gain_map.xmp {
        let xmp_str = core::str::from_utf8(xmp).unwrap_or("<non-UTF8>");
        println!(
            "Gain map XMP ({} bytes): {}",
            xmp.len(),
            &xmp_str[..xmp_str.len().min(500)]
        );
        assert!(!xmp.is_empty(), "XMP bytes should not be empty");
    } else {
        println!("No XMP metadata associated with gain map item");
    }
}
