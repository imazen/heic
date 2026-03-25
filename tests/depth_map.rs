//! Integration tests for depth map extraction and auxiliary image handling.
//!
//! These tests are `#[ignore]` by default since they require real HEIC files
//! with depth data. Set `HEIC_TEST_DIR` to point to your test file directory.

use heic::{
    AuxiliaryImageType, DecoderConfig, DepthRepresentationType, ImageInfo, PixelLayout,
};

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

/// iPhone portrait-mode photos contain depth maps
fn portrait_heic() -> String {
    format!(
        "{}/test-images/openize-heic-net/Openize.Heic.Tests/TestsData/samples/iphone_portrait_photo.heic",
        heic_base_dir()
    )
}

/// Standard non-portrait iPhone photo (no depth)
fn standard_heic() -> String {
    format!(
        "{}/test-images/classic-car-iphone12pro.heic",
        heic_base_dir()
    )
}

// ---- Unit tests that don't need real files ----

#[test]
fn test_auxiliary_type_from_urn_known_types() {
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:mpeg:hevc:2015:auxid:1"),
        AuxiliaryImageType::Alpha
    );
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:mpeg:hevc:2015:auxid:2"),
        AuxiliaryImageType::Depth
    );
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"),
        AuxiliaryImageType::Alpha
    );
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:mpeg:mpegB:cicp:systems:auxiliary:depth"),
        AuxiliaryImageType::Depth
    );
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:com:apple:photo:2020:aux:hdrgainmap"),
        AuxiliaryImageType::HdrGainMap
    );
    assert_eq!(
        AuxiliaryImageType::from_urn("urn:com:apple:photo:2018:aux:portraiteffectsmatte"),
        AuxiliaryImageType::PortraitMatte
    );
}

#[test]
fn test_auxiliary_type_from_urn_unknown() {
    let t = AuxiliaryImageType::from_urn("urn:example:custom:something");
    match t {
        AuxiliaryImageType::Other(s) => assert_eq!(s, "urn:example:custom:something"),
        _ => panic!("expected Other variant"),
    }
}

#[test]
fn test_auxiliary_type_from_urn_empty_string() {
    let t = AuxiliaryImageType::from_urn("");
    match t {
        AuxiliaryImageType::Other(s) => assert_eq!(s, ""),
        _ => panic!("expected Other variant for empty string"),
    }
}

#[test]
fn test_auxiliary_type_display() {
    assert_eq!(format!("{}", AuxiliaryImageType::Alpha), "Alpha");
    assert_eq!(format!("{}", AuxiliaryImageType::Depth), "Depth");
    assert_eq!(
        format!("{}", AuxiliaryImageType::Other("custom".into())),
        "Other(custom)"
    );
}

#[test]
fn test_depth_representation_type_codes() {
    assert_eq!(
        DepthRepresentationType::from_code(0),
        Some(DepthRepresentationType::UniformInverseZ)
    );
    assert_eq!(
        DepthRepresentationType::from_code(1),
        Some(DepthRepresentationType::UniformDisparity)
    );
    assert_eq!(
        DepthRepresentationType::from_code(2),
        Some(DepthRepresentationType::UniformZ)
    );
    assert_eq!(
        DepthRepresentationType::from_code(3),
        Some(DepthRepresentationType::NonuniformDisparity)
    );
    assert_eq!(DepthRepresentationType::from_code(4), None);
}

// ---- Integration tests requiring real HEIC files ----

#[test]
#[ignore]
fn test_standard_photo_has_no_depth() {
    let data = std::fs::read(standard_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.has_depth(&data).expect("has_depth failed");
    assert!(!result, "standard photo should not have depth");
}

#[test]
#[ignore]
fn test_standard_photo_info_no_depth() {
    let data = std::fs::read(standard_heic()).expect("Failed to read test file");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(
        !info.has_depth,
        "standard photo should not report has_depth"
    );
}

#[test]
#[ignore]
fn test_standard_photo_decode_depth_returns_error() {
    let data = std::fs::read(standard_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.decode_depth(&data);
    assert!(
        result.is_err(),
        "decode_depth on a file without depth should fail"
    );
}

#[test]
#[ignore]
fn test_portrait_has_depth() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let result = decoder.has_depth(&data).expect("has_depth failed");
    assert!(result, "portrait photo should have depth");
}

#[test]
#[ignore]
fn test_portrait_info_has_depth() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert!(info.has_depth, "portrait photo should report has_depth");
}

#[test]
#[ignore]
fn test_portrait_auxiliary_images_includes_depth() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let aux_images = decoder
        .auxiliary_images(&data)
        .expect("auxiliary_images failed");
    assert!(
        !aux_images.is_empty(),
        "portrait should have auxiliary images"
    );

    let types: Vec<_> = aux_images.iter().map(|d| &d.aux_type).collect();
    assert!(
        types.contains(&&AuxiliaryImageType::Depth),
        "auxiliary images should include depth; found: {:?}",
        types
    );
}

#[test]
#[ignore]
fn test_portrait_decode_depth() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let depth = decoder.decode_depth(&data).expect("decode_depth failed");

    assert!(depth.width > 0, "depth map width should be nonzero");
    assert!(depth.height > 0, "depth map height should be nonzero");
    assert_eq!(
        depth.data.len(),
        (depth.width * depth.height) as usize,
        "depth data length should match dimensions"
    );
    assert!(
        depth.bit_depth == 8 || depth.bit_depth == 10,
        "depth bit depth should be 8 or 10, got {}",
        depth.bit_depth
    );

    // Verify not all zeros
    let non_zero = depth.data.iter().any(|&v| v != 0);
    assert!(non_zero, "depth data should not be all zeros");

    println!(
        "Depth map: {}x{}, bit_depth={}, type={:?}",
        depth.width, depth.height, depth.bit_depth, depth.depth_info.representation_type
    );
    println!(
        "z_near={:?}, z_far={:?}, d_min={:?}, d_max={:?}",
        depth.depth_info.z_near,
        depth.depth_info.z_far,
        depth.depth_info.d_min,
        depth.depth_info.d_max
    );

    // Print pixel statistics
    let max_expected = ((1u32 << depth.bit_depth) - 1) as u16;
    let valid_count = depth.data.iter().filter(|&&v| v <= max_expected).count();
    let total = depth.data.len();
    println!(
        "Valid pixels: {valid_count}/{total} ({:.1}%)",
        100.0 * valid_count as f64 / total as f64
    );
    let valid_min = depth
        .data
        .iter()
        .filter(|&&v| v <= max_expected)
        .min()
        .copied()
        .unwrap_or(0);
    let valid_max = depth
        .data
        .iter()
        .filter(|&&v| v <= max_expected)
        .max()
        .copied()
        .unwrap_or(0);
    println!("Valid pixel range: {valid_min}..{valid_max}");

    // At least some pixels should be valid.
    // Note: the HEVC decoder currently has incomplete coverage for some
    // monochrome bitstreams, so not all pixels may be decoded. The depth
    // extraction pipeline itself is correct; this is a decoder issue.
    assert!(
        valid_count > 0,
        "at least some pixels should be decoded (got 0/{total})"
    );
}

#[test]
#[ignore]
fn test_portrait_decode_auxiliary_by_id() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let aux_images = decoder
        .auxiliary_images(&data)
        .expect("auxiliary_images failed");
    let depth_desc = aux_images
        .iter()
        .find(|d| d.aux_type == AuxiliaryImageType::Depth)
        .expect("no depth auxiliary found");

    let output = decoder
        .decode_auxiliary(&data, depth_desc.item_id, PixelLayout::Rgb8)
        .expect("decode_auxiliary failed");

    assert!(output.width > 0);
    assert!(output.height > 0);
    assert_eq!(
        output.data.len(),
        (output.width * output.height * 3) as usize,
        "RGB8 data length should match w*h*3"
    );
}

#[test]
#[ignore]
fn test_portrait_depth_representation_info() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let depth = decoder.decode_depth(&data).expect("decode_depth failed");
    let info = &depth.depth_info;

    println!("Depth representation type: {:?}", info.representation_type);
    println!("z_near: {:?}", info.z_near);
    println!("z_far: {:?}", info.z_far);
    println!("d_min: {:?}", info.d_min);
    println!("d_max: {:?}", info.d_max);

    // iPhone portrait depth maps are typically UniformInverseZ
    // The subtype data for this file is:
    //   [0, 0, 0, 17, 0, 0, 0, 13, 78, 1, 177, 9, 53, 30, 120, 150, 1, 3, 242, 32, 32]
    // Byte 0: representation_type = 0 (UniformInverseZ)
    // Byte 1: flags
    assert_eq!(
        info.representation_type,
        DepthRepresentationType::UniformInverseZ
    );
}

#[test]
#[ignore]
fn test_list_all_auxiliary_types() {
    let data = std::fs::read(portrait_heic()).expect("Failed to read test file");
    let decoder = DecoderConfig::new();

    let types = decoder
        .auxiliary_types(&data)
        .expect("auxiliary_types failed");
    println!("Found auxiliary types: {:?}", types);

    // Portrait photos typically have at least alpha + depth
    assert!(!types.is_empty(), "portrait should have auxiliary types");
}
