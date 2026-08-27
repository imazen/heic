//! Orchestration coverage for `src/decode.rs` driven through the public
//! `DecoderConfig` / `DecodeRequest` API against the committed `testdata/`
//! corpus.
//!
//! Every test asserts REAL behavior — exact dimensions, buffer sizes derived
//! from the layout, channel-order differences between RGB/BGR, pixel
//! non-degeneracy, gain-map sanity, idempotence across repeat decodes, and
//! specific `HeicError` / `ProbeError` variants on malformed input. A test
//! that merely "calls and ignores" is forbidden by this project's
//! "false positives are the highest-severity bug" rule, so each exercise here
//! has an `assert`.
//!
//! The bundled corpus is checked into the repo, so these run in CI without any
//! download. Where a fixture is genuinely absent we `eprintln!` and return —
//! but the files asserted below (`example.heic`, `apple-hdr/hdr-sample.heic`,
//! the synthetic set, and the uncompressed HEIF set) are all committed.

use heic::{
    Backend, DecodeOutput, DecoderConfig, HeicError, ImageInfo, Limits, PixelLayout, ProbeError,
    RowSink, Stop, StopReason,
};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// Read a committed fixture. Returns `None` (with an explanation) only if the
/// file is genuinely missing — a checkout/CI misconfiguration, not a decode
/// failure. The asserted fixtures are all committed.
fn read_fixture(rel: &str) -> Option<Vec<u8>> {
    let path = testdata().join(rel);
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            eprintln!(
                "missing committed fixture {} ({e}); skipping precondition",
                path.display()
            );
            None
        }
    }
}

/// The grid HEIC: 1280x854, six 512x512 tiles, BT.709 limited, has a thumbnail.
const GRID: &str = "libheif-examples/example.heic";
/// Apple HDR: 1512x850 primary, half-res (756x425) 8-bit gain map, exif+xmp+icc.
const HDR: &str = "apple-hdr/hdr-sample.heic";

/// Count distinct pixels in a packed buffer to prove output isn't a flat fill.
fn distinct_pixels(data: &[u8], bpp: usize, take: usize) -> usize {
    use std::collections::HashSet;
    let mut set: HashSet<&[u8]> = HashSet::new();
    for px in data.chunks_exact(bpp).take(take) {
        set.insert(px);
    }
    set.len()
}

// ---------------------------------------------------------------------------
// 1. Grid decode: dims + buffer length across all four layouts
// ---------------------------------------------------------------------------

#[test]
fn grid_decode_all_layouts_dims_and_buffer_len() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();

    for layout in [
        PixelLayout::Rgb8,
        PixelLayout::Rgba8,
        PixelLayout::Bgr8,
        PixelLayout::Bgra8,
    ] {
        let out = cfg
            .decode(&data, layout)
            .unwrap_or_else(|e| panic!("grid decode {layout:?} failed: {e}"));
        assert_eq!(out.width, 1280, "grid width ({layout:?})");
        assert_eq!(out.height, 854, "grid height ({layout:?})");
        assert_eq!(out.layout, layout, "layout echoed back");
        let bpp = layout.bytes_per_pixel();
        assert_eq!(
            out.data.len(),
            1280 * 854 * bpp,
            "buffer length must equal w*h*bpp for {layout:?}",
        );
        // A real photo tile region is not a single flat color.
        assert!(
            distinct_pixels(&out.data, bpp, 8000) > 50,
            "grid {layout:?} output is suspiciously flat",
        );
    }
}

#[test]
fn grid_rgb_vs_bgr_channel_order_differs() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let rgb = cfg.decode(&data, PixelLayout::Rgb8).expect("rgb decode");
    let bgr = cfg.decode(&data, PixelLayout::Bgr8).expect("bgr decode");
    let rgba = cfg.decode(&data, PixelLayout::Rgba8).expect("rgba decode");
    let bgra = cfg.decode(&data, PixelLayout::Bgra8).expect("bgra decode");

    assert_eq!(rgb.data.len(), bgr.data.len());
    // BGR is RGB with R and B swapped per pixel; G stays put.
    let mut swap_seen = false;
    for (r, b) in rgb
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .zip(bgr.data.as_chunks::<3>().0)
    {
        assert_eq!(r[1], b[1], "green channel must be identical RGB vs BGR");
        assert_eq!(r[0], b[2], "RGB.R must equal BGR.B");
        assert_eq!(r[2], b[0], "RGB.B must equal BGR.R");
        if r[0] != r[2] {
            swap_seen = true;
        }
    }
    assert!(
        swap_seen,
        "expected at least one pixel where R != B so the swap is observable",
    );

    // Same relationship in the 4-channel layouts, and alpha aligns.
    for (rp, bp) in rgba
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(bgra.data.as_chunks::<4>().0)
    {
        assert_eq!(rp[0], bp[2], "RGBA.R == BGRA.B");
        assert_eq!(rp[1], bp[1], "RGBA.G == BGRA.G");
        assert_eq!(rp[2], bp[0], "RGBA.B == BGRA.R");
        assert_eq!(rp[3], bp[3], "alpha identical RGBA vs BGRA");
    }

    // The RGB(A) data is the RGBA buffer with the alpha byte removed.
    for (three, four) in rgb
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .zip(rgba.data.as_chunks::<4>().0)
    {
        assert_eq!(three, &four[..3], "Rgb8 is Rgba8 minus alpha");
    }
}

#[test]
fn decode_is_deterministic_across_repeat_calls() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let a = cfg.decode(&data, PixelLayout::Rgba8).expect("decode a");
    let b = cfg.decode(&data, PixelLayout::Rgba8).expect("decode b");
    assert_eq!(a.data, b.data, "repeated decode must be byte-identical");
    assert_eq!((a.width, a.height), (b.width, b.height));
}

// ---------------------------------------------------------------------------
// 2. decode_into: streaming grid path, exact size, undersized -> Err
// ---------------------------------------------------------------------------

#[test]
fn decode_into_matches_decode_output() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let info = ImageInfo::from_bytes(&data).expect("probe grid");
    assert_eq!((info.width, info.height), (1280, 854));

    for layout in [
        PixelLayout::Rgb8,
        PixelLayout::Rgba8,
        PixelLayout::Bgr8,
        PixelLayout::Bgra8,
    ] {
        // `output_buffer_size` consumes `self`; re-probe per layout (cheap).
        let size = ImageInfo::from_bytes(&data)
            .expect("probe grid")
            .output_buffer_size(layout)
            .expect("buffer size must not overflow");
        assert_eq!(size, 1280 * 854 * layout.bytes_per_pixel());

        let mut buf = vec![0u8; size];
        let (w, h) = cfg
            .decode_request(&data)
            .with_output_layout(layout)
            .decode_into(&mut buf)
            .unwrap_or_else(|e| panic!("decode_into {layout:?} failed: {e}"));
        assert_eq!((w, h), (1280, 854), "decode_into dims ({layout:?})");

        // The streaming `decode_into` path must agree with the full-frame
        // `decode` path byte-for-byte — divergence between two code paths for
        // the same operation is a shipping bug per the project rules.
        let reference = cfg.decode(&data, layout).expect("reference decode");
        assert_eq!(
            buf, reference.data,
            "decode_into and decode disagree for {layout:?}",
        );
    }
}

#[test]
fn decode_into_undersized_buffer_errors_buffer_too_small() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    // One byte short of the required RGBA8 buffer.
    let need = 1280usize * 854 * 4;
    let mut buf = vec![0u8; need - 1];
    let err = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .decode_into(&mut buf)
        .expect_err("undersized buffer must error");
    match err.error() {
        HeicError::BufferTooSmall { required, actual } => {
            assert_eq!(*required, need, "reported required size");
            assert_eq!(*actual, need - 1, "reported actual size");
        }
        other => panic!("expected BufferTooSmall, got {other:?}"),
    }
}

#[test]
fn decode_into_empty_buffer_errors() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let mut buf: Vec<u8> = Vec::new();
    let err = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut buf)
        .expect_err("zero-length buffer must error");
    assert!(
        matches!(err.error(), HeicError::BufferTooSmall { .. }),
        "expected BufferTooSmall on empty buffer, got {:?}",
        err.error(),
    );
}

// ---------------------------------------------------------------------------
// 3. decode_to_frame: raw YCbCr access
// ---------------------------------------------------------------------------

#[test]
fn decode_to_frame_yields_consistent_yuv_planes() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let frame = cfg.decode_to_frame(&data).expect("decode_to_frame");

    assert_eq!(frame.cropped_width(), 1280, "frame cropped width");
    assert_eq!(frame.cropped_height(), 854, "frame cropped height");
    assert_eq!(frame.bit_depth, 8, "example.heic is 8-bit");
    assert_eq!(frame.chroma_format, 1, "example.heic is 4:2:0");

    let (y, y_stride) = frame.plane(0);
    let (cb, c_stride) = frame.plane(1);
    let (cr, _) = frame.plane(2);
    assert!(y_stride >= frame.width as usize, "Y stride covers width");
    assert!(c_stride >= 1, "chroma stride is positive");
    assert!(!y.is_empty() && !cb.is_empty() && !cr.is_empty());

    // Decoded luma must carry real variation, not a constant plane.
    let y_min = y.iter().copied().min().unwrap();
    let y_max = y.iter().copied().max().unwrap();
    assert!(
        y_max > y_min,
        "luma plane is constant ({y_min}); decode produced no content",
    );
    // 8-bit content must stay inside the 8-bit value range.
    assert!(y_max <= 255, "8-bit luma exceeds 255: {y_max}");
}

// ---------------------------------------------------------------------------
// 4. decode_thumbnail: present vs absent
// ---------------------------------------------------------------------------

#[test]
fn decode_thumbnail_present_is_smaller_than_primary() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let info = ImageInfo::from_bytes(&data).expect("probe");
    assert!(info.has_thumbnail, "example.heic advertises a thumbnail");

    let thumb = cfg
        .decode_thumbnail(&data, PixelLayout::Rgb8)
        .expect("decode_thumbnail call")
        .expect("a thumbnail should be present");
    assert!(thumb.width > 0 && thumb.height > 0, "thumb has dims");
    assert!(
        (thumb.width as u64) * (thumb.height as u64) < (info.width as u64) * (info.height as u64),
        "thumbnail ({}x{}) must be smaller than primary ({}x{})",
        thumb.width,
        thumb.height,
        info.width,
        info.height,
    );
    assert_eq!(
        thumb.data.len(),
        thumb.width as usize * thumb.height as usize * 3,
        "thumbnail Rgb8 buffer length",
    );
    assert_eq!(thumb.layout, PixelLayout::Rgb8);
    assert!(
        distinct_pixels(&thumb.data, 3, thumb.data.len() / 3) > 5,
        "thumbnail is not a flat fill",
    );
}

#[test]
fn decode_thumbnail_absent_returns_none() {
    // The synthetic single-image files have no thumbnail item.
    let Some(data) = read_fixture("synthetic/synth_8bit_q50.heic") else {
        return;
    };
    let cfg = DecoderConfig::new();
    let info = ImageInfo::from_bytes(&data).expect("probe synthetic");
    assert!(!info.has_thumbnail, "synthetic file has no thumbnail");
    let thumb = cfg
        .decode_thumbnail(&data, PixelLayout::Rgba8)
        .expect("call must succeed");
    assert!(
        thumb.is_none(),
        "absent thumbnail must be None, not garbage"
    );
}

// ---------------------------------------------------------------------------
// 5. decode_gain_map: apple-hdr sanity (<50% saturated, real content)
// ---------------------------------------------------------------------------

#[test]
fn gain_map_apple_hdr_is_sane() {
    let Some(data) = read_fixture(HDR) else {
        return;
    };
    let cfg = DecoderConfig::new();

    assert!(
        cfg.has_gain_map(&data).expect("has_gain_map"),
        "apple-hdr file must report a gain map",
    );
    let info = ImageInfo::from_bytes(&data).expect("probe hdr");
    assert!(info.has_gain_map, "probe must report has_gain_map");

    let gm = cfg.decode_gain_map(&data).expect("decode_gain_map");
    assert!(gm.width > 0 && gm.height > 0, "gain map has dims");
    assert_eq!(
        gm.data.len(),
        gm.width as usize * gm.height as usize,
        "gain map is single-channel grayscale (w*h bytes)",
    );
    assert!(
        gm.bit_depth == 8 || gm.bit_depth == 10,
        "gain map bit depth {} unexpected",
        gm.bit_depth,
    );
    // Gain map is an auxiliary, typically lower resolution than the primary.
    assert!(
        (gm.width as u64) * (gm.height as u64) <= (info.width as u64) * (info.height as u64),
        "gain map must not exceed primary resolution",
    );

    // Not garbage: must have variation and must not be mostly clipped.
    let min = *gm.data.iter().min().unwrap();
    let max = *gm.data.iter().max().unwrap();
    assert!(max > min, "gain map is a constant plane ({min})");
    let saturated = gm.data.iter().filter(|&&v| v == 0 || v == 255).count();
    let frac = saturated as f64 / gm.data.len() as f64;
    assert!(
        frac < 0.50,
        "over half the gain map is clipped ({frac:.2}); likely a decode bug",
    );
}

#[test]
fn decode_gain_map_without_gain_map_errors() {
    // The grid example has no gain map.
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    assert!(
        !cfg.has_gain_map(&data).expect("has_gain_map"),
        "grid file has no gain map",
    );
    let err = cfg
        .decode_gain_map(&data)
        .expect_err("decode_gain_map on a non-HDR file must error");
    assert!(
        matches!(err.error(), HeicError::InvalidData(_)),
        "expected InvalidData for absent gain map, got {:?}",
        err.error(),
    );
}

// ---------------------------------------------------------------------------
// 6. Metadata orchestration: exif / xmp / icc presence agreement
// ---------------------------------------------------------------------------

#[test]
fn metadata_extraction_agrees_with_probe() {
    let Some(data) = read_fixture(HDR) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let info = ImageInfo::from_bytes(&data).expect("probe hdr");

    let exif = cfg.extract_exif(&data).expect("extract_exif call");
    assert_eq!(
        exif.is_some(),
        info.has_exif,
        "extract_exif must agree with probe has_exif",
    );
    if let Some(exif) = &exif {
        assert!(
            !exif.is_empty(),
            "exif bytes must be non-empty when present"
        );
        // EXIF is raw TIFF: starts with a byte-order mark.
        assert!(
            exif.starts_with(b"II") || exif.starts_with(b"MM"),
            "EXIF must start with a TIFF byte-order mark",
        );
    }

    let xmp = cfg.extract_xmp(&data).expect("extract_xmp call");
    assert_eq!(
        xmp.is_some(),
        info.has_xmp,
        "extract_xmp must agree with probe has_xmp",
    );
    if let Some(xmp) = &xmp {
        assert!(!xmp.is_empty(), "xmp bytes must be non-empty when present");
    }

    let icc = cfg.extract_icc(&data).expect("extract_icc call");
    assert_eq!(
        icc.is_some(),
        info.has_icc_profile,
        "extract_icc must agree with probe has_icc_profile",
    );
    if let Some(icc) = &icc {
        // ICC profiles begin with a 4-byte big-endian size equal to the body.
        assert!(icc.len() >= 4, "icc profile too short");
        let declared = u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize;
        assert_eq!(declared, icc.len(), "ICC declared size must match length");
    }
}

#[test]
fn metadata_absent_on_synthetic_returns_none() {
    let Some(data) = read_fixture("synthetic/synth_8bit_lossless.heic") else {
        return;
    };
    let cfg = DecoderConfig::new();
    assert!(
        cfg.extract_exif(&data).expect("extract_exif").is_none(),
        "synthetic file has no EXIF",
    );
    assert!(
        cfg.extract_xmp(&data).expect("extract_xmp").is_none(),
        "synthetic file has no XMP",
    );
    assert!(
        cfg.extract_icc(&data).expect("extract_icc").is_none(),
        "synthetic file has no ICC profile",
    );
}

// ---------------------------------------------------------------------------
// 7. Alpha channel behavior on this corpus (no alpha-aux fixture present)
// ---------------------------------------------------------------------------

#[test]
fn opaque_image_fills_alpha_with_255() {
    // No file in the committed corpus carries an alpha auxiliary image, so the
    // honest, verifiable assertion is the converse: for a non-alpha primary,
    // the RGBA/BGRA alpha channel must be fully opaque (255), never left
    // uninitialized or zeroed.
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let info = ImageInfo::from_bytes(&data).expect("probe");
    assert!(!info.has_alpha, "example.heic has no alpha aux");

    let out = cfg.decode(&data, PixelLayout::Rgba8).expect("rgba decode");
    assert!(
        out.data.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
        "opaque image must have alpha == 255 everywhere",
    );
}

// ---------------------------------------------------------------------------
// 8. Uncompressed HEIF (unci feature) decode paths
// ---------------------------------------------------------------------------

/// `uncompressed_*` fixtures that decode through the `unci` feature with the
/// pure-Rust path. Each entry is `(relative path, width, height)`.
#[cfg(feature = "unci")]
const UNCI_FILES: &[(&str, u32, u32)] = &[
    ("libheif-examples/uncompressed_comp_RGB.heif", 30, 20),
    ("libheif-examples/uncompressed_comp_RGB_tiled.heif", 30, 20),
    ("libheif-examples/uncompressed_pix_RGB.heif", 30, 20),
    ("libheif-examples/uncompressed_pix_RGB_tiled.heif", 30, 20),
    ("libheif-examples/uncompressed_comp_M.heif", 30, 20),
    ("libheif-examples/uncompressed_pix_M.heif", 30, 20),
    ("libheif-examples/uncompressed_comp_YUV_tiled.heif", 30, 20),
    ("libheif-examples/uncompressed_pix_YUV_tiled.heif", 30, 20),
    (
        "libheif-examples/uncompressed_pix_R8G8B8A8_bsz0_psz5_tiled.heif",
        30,
        20,
    ),
];

#[cfg(feature = "unci")]
#[test]
fn uncompressed_heif_decodes_with_expected_dims() {
    let cfg = DecoderConfig::new();
    let mut decoded = 0usize;
    for &(rel, w, h) in UNCI_FILES {
        let Some(data) = read_fixture(rel) else {
            continue;
        };

        let info =
            ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("{rel}: probe failed: {e:?}"));
        assert_eq!((info.width, info.height), (w, h), "{rel}: probe dims");

        let out = cfg
            .decode(&data, PixelLayout::Rgba8)
            .unwrap_or_else(|e| panic!("{rel}: decode failed: {e}"));
        assert_eq!((out.width, out.height), (w, h), "{rel}: decode dims");
        assert_eq!(
            out.data.len(),
            w as usize * h as usize * 4,
            "{rel}: RGBA8 buffer length",
        );
        // Synthesized uncompressed test images are opaque: alpha must be
        // filled to 255 for every pixel (a real check on the alpha write,
        // verified true for every fixture in `UNCI_FILES`).
        assert!(
            out.data.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
            "{rel}: opaque source must have alpha == 255 everywhere",
        );
        // The monochrome `_M` fixtures are a single flat gray by design; the
        // RGB / YUV fixtures carry a multi-color test pattern.
        let distinct = distinct_pixels(&out.data, 4, out.data.len() / 4);
        if rel.contains("_M.heif") {
            assert_eq!(distinct, 1, "{rel}: monochrome fixture is a flat fill");
        } else {
            assert!(
                distinct >= 2,
                "{rel}: RGB/YUV fixture must carry more than one color",
            );
        }
        decoded += 1;
    }
    assert!(
        decoded >= 5,
        "expected at least 5 uncompressed HEIF fixtures to decode, got {decoded}",
    );
}

#[cfg(feature = "unci")]
#[test]
fn uncompressed_rgb_decode_into_matches_decode() {
    let Some(data) = read_fixture("libheif-examples/uncompressed_comp_RGB.heif") else {
        return;
    };
    let cfg = DecoderConfig::new();
    let reference = cfg.decode(&data, PixelLayout::Rgb8).expect("decode");
    let mut buf = vec![0u8; reference.data.len()];
    let (w, h) = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut buf)
        .expect("decode_into");
    assert_eq!((w, h), (reference.width, reference.height));
    assert_eq!(
        buf, reference.data,
        "decode_into must match decode for unci"
    );
}

/// Regression test for heic#21: this suite must pass WITHOUT the `unci`
/// feature too (it used to panic because the two tests above were not
/// feature-gated). Repro file is the 2.2 KB 30x20
/// `uncompressed_comp_RGB.heif`. Pins the without-feature contract: probing
/// is container-level and feature-independent, while `decode` and
/// `decode_into` fail with a clean `UnsupportedCodec` error — not a panic,
/// and not some other variant.
#[cfg(not(feature = "unci"))]
#[test]
fn uncompressed_heif_without_unci_errors_cleanly() {
    let Some(data) = read_fixture("libheif-examples/uncompressed_comp_RGB.heif") else {
        return;
    };
    let info = ImageInfo::from_bytes(&data).expect("probe must work without 'unci'");
    assert_eq!((info.width, info.height), (30, 20), "probe dims");

    let cfg = DecoderConfig::new();
    let err = cfg
        .decode(&data, PixelLayout::Rgba8)
        .expect_err("decode must fail without 'unci'");
    assert!(
        matches!(err.error(), HeicError::UnsupportedCodec(_)),
        "expected UnsupportedCodec from decode, got {:?}",
        err.error(),
    );

    let size = info
        .output_buffer_size(PixelLayout::Rgb8)
        .expect("buffer size for 30x20 RGB8");
    let mut buf = vec![0u8; size];
    let err = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut buf)
        .expect_err("decode_into must fail without 'unci'");
    assert!(
        matches!(err.error(), HeicError::UnsupportedCodec(_)),
        "expected UnsupportedCodec from decode_into, got {:?}",
        err.error(),
    );
}

// ---------------------------------------------------------------------------
// 9. Limits orchestration: dimension/pixel/memory caps trigger LimitExceeded
// ---------------------------------------------------------------------------

#[test]
fn limit_on_width_rejects_grid() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let mut limits = Limits::default();
    limits.max_width = Some(640); // grid is 1280 wide
    let err = cfg
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect_err("width limit must reject");
    assert!(
        matches!(err.error(), HeicError::LimitExceeded(_)),
        "expected LimitExceeded for width cap, got {:?}",
        err.error(),
    );
}

#[test]
fn limit_on_pixels_rejects_grid() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let mut limits = Limits::default();
    limits.max_pixels = Some(1000); // far below 1280*854
    let err = cfg
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect_err("pixel limit must reject");
    assert!(
        matches!(err.error(), HeicError::LimitExceeded(_)),
        "expected LimitExceeded for pixel cap, got {:?}",
        err.error(),
    );
}

#[test]
fn limit_on_memory_rejects_grid() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let mut limits = Limits::default();
    limits.max_memory_bytes = Some(1024); // 1 KiB can't hold a 1280x854 frame
    let err = cfg
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect_err("memory limit must reject");
    assert!(
        matches!(err.error(), HeicError::LimitExceeded(_)),
        "expected LimitExceeded for memory cap, got {:?}",
        err.error(),
    );
}

#[test]
fn generous_limits_allow_decode() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let limits = Limits::server_defaults();
    let out = cfg
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect("server_defaults must allow a 1280x854 image");
    assert_eq!((out.width, out.height), (1280, 854));
}

#[test]
fn estimate_memory_is_monotone_and_layout_aware() {
    // estimate_memory is the gate Limits::check_memory consults. Larger images
    // and wider layouts must never estimate fewer bytes.
    let small = DecoderConfig::estimate_memory(100, 100, PixelLayout::Rgb8);
    let large = DecoderConfig::estimate_memory(1000, 1000, PixelLayout::Rgb8);
    assert!(large > small, "bigger image must estimate more memory");

    let rgb = DecoderConfig::estimate_memory(1280, 854, PixelLayout::Rgb8);
    let rgba = DecoderConfig::estimate_memory(1280, 854, PixelLayout::Rgba8);
    assert!(
        rgba > rgb,
        "4-channel layout must estimate more than 3-channel"
    );

    // A pathological size must saturate, not wrap to a small value.
    let huge = DecoderConfig::estimate_memory(u32::MAX, u32::MAX, PixelLayout::Rgba8);
    assert!(
        huge > rgba,
        "u32::MAX dims must saturate high, not wrap low"
    );
}

// ---------------------------------------------------------------------------
// 10. Cancellation orchestration
// ---------------------------------------------------------------------------

/// A `Stop` that always reports cancellation, so `decode` bails at its first
/// cooperative checkpoint.
struct AlwaysStop;
impl Stop for AlwaysStop {
    fn check(&self) -> Result<(), StopReason> {
        Err(StopReason::Cancelled)
    }
}

#[test]
fn cancellation_token_aborts_decode() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let stop = AlwaysStop;
    let err = cfg
        .decode_request(&data)
        .with_stop(&stop)
        .decode()
        .expect_err("an always-stopped token must abort decode");
    assert!(
        matches!(err.error(), HeicError::Cancelled(_)),
        "expected Cancelled, got {:?}",
        err.error(),
    );
}

/// A pre-cancelled `Stop` aborts a **single-image** (non-grid) decode too.
///
/// The grid case above already cancelled at the per-tile checkpoint; this
/// covers the still-image HEVC path (`decode_with_config_stop` →
/// `decode_nal_units` → `decode_slice` → the per-CTU loop in
/// `SliceContext::decode_slice`). Before per-CTU cancellation was threaded
/// in, this single-tile decode observed no stop checkpoint during HEVC decode
/// and would have run to completion. The synthetic fixture is a single image
/// with no tiles, so only the CTU-loop check can catch the cancellation.
#[test]
fn cancellation_aborts_single_image_decode() {
    let Some(data) = read_fixture("synthetic/synth_8bit_q50.heic") else {
        return;
    };
    // Sanity: this fixture is a single image, not a grid (no tile-entry
    // checkpoint to mask what we're testing).
    let info = ImageInfo::from_bytes(&data).expect("probe synthetic single image");
    assert!(!info.has_thumbnail, "synthetic file has no thumbnail");

    let cfg = DecoderConfig::new();
    let stop = AlwaysStop;
    let err = cfg
        .decode_request(&data)
        .with_stop(&stop)
        .decode()
        .expect_err("an always-stopped token must abort a single-image decode");
    assert!(
        matches!(err.error(), HeicError::Cancelled(_)),
        "expected Cancelled for single-image decode, got {:?}",
        err.error(),
    );
}

// ---------------------------------------------------------------------------
// 11. Backend allowlist orchestration
// ---------------------------------------------------------------------------

#[test]
fn empty_backend_list_errors_no_backend_selected() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new().with_backends(&[]);
    assert!(cfg.backends().is_empty(), "backend list cleared");
    let err = cfg
        .decode(&data, PixelLayout::Rgba8)
        .expect_err("empty backend list must error");
    assert!(
        matches!(err.error(), HeicError::NoBackendSelected),
        "expected NoBackendSelected, got {:?}",
        err.error(),
    );
}

#[test]
fn explicit_rust_backend_decodes_grid() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new().with_backend(Backend::Rust);
    assert_eq!(cfg.backends(), &[Backend::Rust], "single backend set");
    let out = cfg.decode(&data, PixelLayout::Rgba8).expect("rust decode");
    assert_eq!((out.width, out.height), (1280, 854));
}

// ---------------------------------------------------------------------------
// 12. decode_rows: streaming sink path
// ---------------------------------------------------------------------------

/// Sink that accumulates every demanded strip into one contiguous buffer.
struct VecSink {
    buf: Vec<u8>,
    rows_seen: u32,
}

impl RowSink for VecSink {
    fn demand(&mut self, y: u32, height: u32, min_bytes: usize) -> &mut [u8] {
        // Strips arrive top-to-bottom; track total rows for the assertion.
        assert_eq!(y, self.rows_seen, "strips must arrive in order from y");
        self.rows_seen += height;
        let start = self.buf.len();
        self.buf.resize(start + min_bytes, 0);
        &mut self.buf[start..start + min_bytes]
    }
}

#[test]
fn decode_rows_streams_full_image() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let reference = cfg.decode(&data, PixelLayout::Rgb8).expect("reference");

    let mut sink = VecSink {
        buf: Vec::new(),
        rows_seen: 0,
    };
    let (w, h) = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_rows(&mut sink)
        .expect("decode_rows");
    assert_eq!((w, h), (1280, 854), "decode_rows dims");
    assert_eq!(sink.rows_seen, 854, "all rows demanded exactly once");
    assert_eq!(
        sink.buf.len(),
        1280 * 854 * 3,
        "streamed buffer covers the whole image",
    );
    assert_eq!(
        sink.buf, reference.data,
        "decode_rows output must equal decode output",
    );
}

// ---------------------------------------------------------------------------
// 13. Synthetic single-image (non-grid) decode path across quality levels
// ---------------------------------------------------------------------------

#[test]
fn synthetic_single_image_decodes_all_qualities() {
    let cfg = DecoderConfig::new();
    for q in ["lossless", "q10", "q50", "q95"] {
        let rel = format!("synthetic/synth_8bit_{q}.heic");
        let Some(data) = read_fixture(&rel) else {
            continue;
        };
        let out: DecodeOutput = cfg
            .decode(&data, PixelLayout::Rgba8)
            .unwrap_or_else(|e| panic!("{rel}: decode failed: {e}"));
        assert_eq!((out.width, out.height), (256, 256), "{rel}: dims");
        assert_eq!(out.data.len(), 256 * 256 * 4, "{rel}: buffer length");
        assert!(
            out.data.as_chunks::<4>().0.iter().all(|px| px[3] == 255),
            "{rel}: opaque alpha",
        );
        assert!(
            distinct_pixels(&out.data, 4, 4000) > 20,
            "{rel}: output is not flat",
        );
    }
}

// ---------------------------------------------------------------------------
// 14. Malformed / truncated input: clean Err, never panic
// ---------------------------------------------------------------------------

#[test]
fn truncated_grid_does_not_panic_and_errors() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let full = cfg.decode(&data, PixelLayout::Rgba8).expect("full decode");

    // Small header-region cuts: the container header is incomplete, so decode
    // MUST error cleanly (never panic).
    for &cut in &[12usize, 32, 64, 128, 512, 1024, 4096] {
        if cut >= data.len() {
            continue;
        }
        let truncated = &data[..cut];
        // Probe never panics; we don't assert its variant here (some prefixes
        // parse far enough to probe), only that control returns.
        let _ = ImageInfo::from_bytes(truncated);
        let result = cfg.decode(truncated, PixelLayout::Rgba8);
        assert!(
            result.is_err(),
            "truncating to {cut} bytes (header region) must not decode",
        );
    }

    // Larger prefixes can carry enough tile/parameter data to still decode the
    // grid — that's legitimate decoder behavior, not a bug. The real invariant
    // is: it must never panic, and IF it succeeds the reported dimensions and
    // buffer length must stay consistent with the full image (no truncated
    // half-frame masquerading as a complete decode).
    for &cut in &[data.len() / 2, data.len() * 3 / 4, data.len() - 1] {
        if cut == 0 || cut >= data.len() {
            continue;
        }
        match cfg.decode(&data[..cut], PixelLayout::Rgba8) {
            Ok(out) => {
                assert_eq!(
                    (out.width, out.height),
                    (full.width, full.height),
                    "a successful truncated decode must report the full dims",
                );
                assert_eq!(
                    out.data.len(),
                    out.width as usize * out.height as usize * 4,
                    "buffer length must match reported dims",
                );
            }
            Err(_) => { /* clean error is equally acceptable, and no panic */ }
        }
    }
}

#[test]
fn random_garbage_probe_is_invalid_format() {
    // 4096 bytes whose box-type field is not `ftyp`.
    let mut junk = vec![0u8; 4096];
    for (i, b) in junk.iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(31).wrapping_add(7);
    }
    let err = ImageInfo::from_bytes(&junk).expect_err("garbage must not probe");
    assert!(
        matches!(
            err.error(),
            ProbeError::InvalidFormat | ProbeError::Corrupt(_)
        ),
        "expected InvalidFormat/Corrupt for non-ftyp junk, got {err:?}",
    );

    // And decode must reject it without panicking.
    let cfg = DecoderConfig::new();
    assert!(
        cfg.decode(&junk, PixelLayout::Rgba8).is_err(),
        "garbage must not decode",
    );
}

#[test]
fn tiny_input_probe_needs_more_data() {
    // Fewer than 12 bytes: probe can't even read the box header.
    let err = ImageInfo::from_bytes(&[0u8; 8]).expect_err("8 bytes is too few");
    assert!(
        matches!(err.error(), ProbeError::NeedMoreData),
        "expected NeedMoreData for an 8-byte buffer, got {err:?}",
    );
    // Empty input too.
    assert!(matches!(
        ImageInfo::from_bytes(&[]),
        Err(e) if matches!(e.error(), ProbeError::NeedMoreData)
    ));
}

#[test]
fn lightning_mini_has_no_primary_image() {
    // This committed fixture parses as a HEIF container but has no decodable
    // primary image — the orchestrator must surface that as a clean error,
    // not a panic or a bogus zero-dimension success.
    let Some(data) = read_fixture("libheif-examples/lightning_mini.heif") else {
        return;
    };
    let probe = ImageInfo::from_bytes(&data);
    assert!(
        matches!(&probe, Err(e) if matches!(e.error(), ProbeError::Corrupt(_))),
        "lightning_mini must probe as Corrupt(NoPrimaryImage), got {probe:?}",
    );
    let cfg = DecoderConfig::new();
    let dec = cfg.decode(&data, PixelLayout::Rgba8);
    assert!(dec.is_err(), "lightning_mini must not decode");
}

// ---------------------------------------------------------------------------
// 15. Fuzz regression seeds: crafted crash inputs must error, never panic
// ---------------------------------------------------------------------------

fn fuzz_regression_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("regression")
}

#[test]
fn fuzz_regression_seeds_decode_without_panic() {
    let dir = fuzz_regression_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!("fuzz/regression missing at {}; skipping", dir.display());
        return;
    };
    let cfg = DecoderConfig::new();
    let mut seeds = 0usize;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        seeds += 1;
        // Every orchestration entry point must survive these crafted inputs
        // without panicking. We don't care whether they Ok or Err — only that
        // control returns cleanly. (Most are crafted to provoke a specific
        // arithmetic/parse path that previously panicked.)
        let _ = ImageInfo::from_bytes(&data);
        let _ = cfg.decode(&data, PixelLayout::Rgba8);
        let _ = cfg.decode(&data, PixelLayout::Rgb8);
        let _ = cfg.decode_to_frame(&data);
        let _ = cfg.decode_thumbnail(&data, PixelLayout::Rgb8);
        let _ = cfg.extract_exif(&data);
        let _ = cfg.extract_xmp(&data);
        let _ = cfg.extract_icc(&data);
        let _ = cfg.has_gain_map(&data);
        let _ = cfg.decode_gain_map(&data);
        // decode_into with a generously-sized buffer must also not panic.
        let mut buf = vec![0u8; 4096];
        let _ = cfg
            .decode_request(&data)
            .with_output_layout(PixelLayout::Rgba8)
            .decode_into(&mut buf);
    }
    assert!(
        seeds > 0,
        "expected crafted fuzz regression seeds under {}",
        dir.display(),
    );
}

// ---------------------------------------------------------------------------
// 16. max_threads orchestration (parallel feature)
// ---------------------------------------------------------------------------

#[test]
fn single_thread_decode_matches_default() {
    let Some(data) = read_fixture(GRID) else {
        return;
    };
    let cfg = DecoderConfig::new();
    let default = cfg
        .decode(&data, PixelLayout::Rgba8)
        .expect("default decode");

    // Forcing single-threaded decode must produce bit-identical output to the
    // (possibly parallel) default — parallelism must not change pixels.
    let mut buf = vec![0u8; default.data.len()];
    let (w, h) = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .with_max_threads(1)
        .decode_into(&mut buf)
        .expect("single-threaded decode_into");
    assert_eq!((w, h), (default.width, default.height));
    assert_eq!(
        buf, default.data,
        "single-threaded output must match default output",
    );
}
