#![cfg(feature = "image")]

#[test]
fn register_hook_and_decode_heic_via_image() {
    let _ = heic::register_decoding_hook();

    let img = image::open("testdata/libheif-examples/example.heic")
        .expect("image hook failed to decode HEIC");

    assert_eq!(img.width(), 1280);
    assert_eq!(img.height(), 854);
}

#[test]
fn invalid_data_reports_error() {
    let _ = heic::register_decoding_hook();

    let data = b"not a heic file";
    let err = image::load_from_memory(data).expect_err("expected decode error for invalid input");

    match err {
        image::ImageError::Decoding(_)
        | image::ImageError::IoError(_)
        | image::ImageError::Unsupported(_) => {}
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn image_hook_pixels_match_direct_decode() {
    let _ = heic::register_decoding_hook();
    let path = "testdata/libheif-examples/example.heic";
    let bytes = std::fs::read(path).expect("fixture is tracked in git");

    // The fixture is `mif1`-branded, so it goes through the extension hook
    // (see the `mif1` gap in the module docs).
    let via_image = image::open(path).expect("extension hook decodes");
    let direct = heic::DecoderConfig::new()
        .decode(&bytes, heic::PixelLayout::Rgb8)
        .expect("direct decode");

    assert_eq!(
        (via_image.width(), via_image.height()),
        (direct.width, direct.height)
    );
    assert_eq!(via_image.color(), image::ColorType::Rgb8);
    assert!(
        via_image.as_bytes() == direct.data.as_slice(),
        "image-hook pixels differ from heic's direct decode"
    );
}
