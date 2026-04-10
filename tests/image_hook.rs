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
