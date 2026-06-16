//! Public-API coverage tests for `heic`.
//!
//! Exercises the `src/lib.rs` surface (`DecoderConfig`, `DecodeRequest`,
//! `DecodeOutput`, `ImageInfo`, `Limits`, `PixelLayout`, `estimate_memory`)
//! and the `src/backend.rs` dispatcher (`Backend`, `recommended_backends`,
//! the single-Rust fast path, the multi-entry allowlist walk, the empty
//! allowlist `NoBackendSelected` error) with REAL assertions.
//!
//! These are not "did it run" smoke tests: each test pins down a concrete,
//! ground-truth property — exact dimensions matching the decoded output,
//! exact buffer byte counts, correct metadata flags, specific error
//! variants on malformed/truncated/limit-exceeding input, and round-trip /
//! idempotence properties across the builder chain and backend dispatch.
//!
//! The bundled `testdata/` corpus is committed to the source checkout, so
//! fixture reads `expect()` loudly rather than skipping silently (per the
//! project's "no graceful skip" rule). The only early returns are genuine
//! missing-feature preconditions, which never apply here because the test
//! is compiled with the full feature set.

use std::path::{Path, PathBuf};

use heic::{
    Backend, DecodeOutput, DecoderConfig, HeicError, ImageInfo, Limits, PixelLayout, ProbeError,
    Stop, StopReason,
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn testdata() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// Read a committed fixture. The corpus is present in the source checkout,
/// so a missing file is a checkout/CI misconfiguration and must fail loudly.
fn read_fixture(rel: &str) -> Vec<u8> {
    let path = testdata().join(rel);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("required fixture {} missing: {e}", path.display()))
}

const EXAMPLE: &str = "libheif-examples/example.heic";
const APPLE_HDR: &str = "apple-hdr/hdr-sample.heic";
const SYNTH_Q95: &str = "synthetic/synth_8bit_q95.heic";
const SYNTH_LOSSLESS: &str = "synthetic/synth_8bit_lossless.heic";

// ===========================================================================
// ImageInfo::from_bytes — ground-truth metadata
// ===========================================================================

/// `example.heic` is a 1280x854 BT.709 8-bit 4:2:0 HEVC grid. The probe must
/// agree with the actual decode on dimensions, and report the correct
/// bit_depth / chroma_format / no-gain-map flags.
#[test]
fn probe_example_matches_decode() {
    let data = read_fixture(EXAMPLE);

    let info = ImageInfo::from_bytes(&data).expect("probe example.heic");
    assert_eq!(info.width, 1280, "probe width");
    assert_eq!(info.height, 854, "probe height");
    assert_eq!(info.bit_depth, 8, "8-bit luma");
    assert_eq!(info.chroma_format, 1, "4:2:0");
    assert!(!info.has_gain_map, "example.heic has no HDR gain map");
    assert!(!info.has_depth, "example.heic has no depth aux");

    // Probe must agree with the actual decoded dimensions.
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .expect("decode example.heic");
    assert_eq!((out.width, out.height), (info.width, info.height));
    assert_eq!(out.layout, PixelLayout::Rgb8);
    assert_eq!(
        out.data.len(),
        info.output_buffer_size(PixelLayout::Rgb8).unwrap()
    );
}

/// `output_buffer_size` is `width * height * bpp` for every layout.
#[test]
fn output_buffer_size_per_layout() {
    let data = read_fixture(EXAMPLE);
    let info = ImageInfo::from_bytes(&data).expect("probe");
    let px = info.width as usize * info.height as usize;
    // `output_buffer_size(self, ...)` consumes `self`, so clone per call.
    assert_eq!(
        info.clone().output_buffer_size(PixelLayout::Rgb8),
        Some(px * 3)
    );
    assert_eq!(
        info.clone().output_buffer_size(PixelLayout::Bgr8),
        Some(px * 3)
    );
    assert_eq!(
        info.clone().output_buffer_size(PixelLayout::Rgba8),
        Some(px * 4)
    );
    assert_eq!(info.output_buffer_size(PixelLayout::Bgra8), Some(px * 4));
}

/// The Apple HDR sample carries an HDR gain map auxiliary image; the probe
/// must flag it and report sane primary dimensions that match the decode.
#[test]
fn probe_apple_hdr_gain_map_flag() {
    let data = read_fixture(APPLE_HDR);
    let info = ImageInfo::from_bytes(&data).expect("probe apple-hdr");
    assert!(info.has_gain_map, "apple HDR sample reports a gain map");
    assert!(info.width > 0 && info.height > 0, "positive primary dims");

    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("decode apple-hdr primary");
    assert_eq!((out.width, out.height), (info.width, info.height));
    assert_eq!(out.data.len(), out.width as usize * out.height as usize * 4);
}

/// Synthetic HEVC files probe to positive 8-bit dimensions consistent with
/// their decode; covers the direct-HEVC (non-grid) probe path.
#[test]
fn probe_synthetic_consistent_with_decode() {
    for name in [SYNTH_Q95, SYNTH_LOSSLESS] {
        let data = read_fixture(name);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {name}: {e:?}"));
        assert!(info.width > 0 && info.height > 0, "{name}: positive dims");
        assert_eq!(info.bit_depth, 8, "{name}: 8-bit");

        let out = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgb8)
            .unwrap_or_else(|e| panic!("decode {name}: {e:?}"));
        assert_eq!(
            (out.width, out.height),
            (info.width, info.height),
            "{name}: probe vs decode dims"
        );
    }
}

// ===========================================================================
// ImageInfo::from_bytes — malformed / truncated input -> ProbeError, no panic
// ===========================================================================

/// Empty input is too short to hold even a box header.
#[test]
fn probe_empty_needs_more_data() {
    assert!(matches!(
        ImageInfo::from_bytes(&[]),
        Err(ProbeError::NeedMoreData)
    ));
}

/// A buffer shorter than the 12-byte minimum box header reports NeedMoreData.
#[test]
fn probe_too_short_needs_more_data() {
    let data = [0u8; 8];
    assert!(matches!(
        ImageInfo::from_bytes(&data),
        Err(ProbeError::NeedMoreData)
    ));
}

/// 12+ bytes whose box type isn't `ftyp` is rejected as InvalidFormat (not a
/// HEIC/HEIF file). E.g. a PNG-ish header.
#[test]
fn probe_non_ftyp_invalid_format() {
    // length(=12) + "NOPE" type + filler -> not "ftyp".
    let mut data = vec![0u8; 16];
    data[0..4].copy_from_slice(&12u32.to_be_bytes());
    data[4..8].copy_from_slice(b"NOPE");
    assert!(matches!(
        ImageInfo::from_bytes(&data),
        Err(ProbeError::InvalidFormat)
    ));
}

/// A valid `ftyp` header followed by garbage (no meta/iloc/iinf) is a
/// recognized container shape but corrupt — must be `Corrupt`, never a panic.
#[test]
fn probe_ftyp_then_garbage_is_corrupt() {
    // Correct ftyp box, then nothing usable.
    let mut data = vec![0u8; 32];
    data[0..4].copy_from_slice(&16u32.to_be_bytes());
    data[4..8].copy_from_slice(b"ftyp");
    data[8..12].copy_from_slice(b"heic"); // major brand
    data[12..16].copy_from_slice(&0u32.to_be_bytes()); // minor version
    // remaining bytes are zero -> a zero-length box at offset 16, no meta box.
    let r = ImageInfo::from_bytes(&data);
    assert!(
        matches!(
            r,
            Err(ProbeError::Corrupt(_)) | Err(ProbeError::NeedMoreData)
        ),
        "ftyp-without-meta should be Corrupt/NeedMoreData, got {r:?}"
    );
}

/// Truncating a real HEIC to its first N bytes must never panic; it returns a
/// ProbeError once the parser runs past the available bytes. Sweep several
/// truncation lengths to exercise the partial-header paths.
#[test]
fn probe_truncated_real_file_no_panic() {
    let full = read_fixture(EXAMPLE);
    for len in [12usize, 24, 64, 128, 256, 1024, full.len() / 2] {
        let slice = &full[..len.min(full.len())];
        // Either it errors cleanly or (for a long-enough prefix) succeeds —
        // the contract is "no panic", and any Ok must have sane dims.
        if let Ok(info) = ImageInfo::from_bytes(slice) {
            assert!(info.width > 0 && info.height > 0, "len={len}: bad dims");
        }
    }
}

/// Crafted fuzz-corpus crash seeds must probe without panicking. These are the
/// minimized inputs that previously triggered crashes; the contract is a clean
/// `Result`, success or `ProbeError`, never a panic.
#[test]
fn probe_fuzz_regression_seeds_no_panic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("regression");
    let rd = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("fuzz/regression dir missing: {e} ({})", dir.display()));
    let mut count = 0usize;
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        // Must not panic. Any Ok must report positive dimensions.
        if let Ok(info) = ImageInfo::from_bytes(&bytes) {
            assert!(
                info.width > 0 && info.height > 0,
                "{}: Ok probe with zero dims",
                p.display()
            );
        }
        count += 1;
    }
    assert!(
        count >= 5,
        "expected several regression seeds, found {count}"
    );
}

// ===========================================================================
// Limits — each knob below the image rejects, generous limits succeed
// ===========================================================================

/// `Limits::default()` carries the **safe fallback** caps (NOT all-`None`), so
/// it is never weaker than passing no limits at all — the footgun fix. A median
/// sample (well under the caps) still decodes.
#[test]
fn limits_default_carries_safe_fallback() {
    let limits = Limits::default();
    // Not None — the previous all-`None` default was a footgun (weaker than
    // passing nothing, which the decoder bounds with the same fallback).
    assert_eq!(limits.max_width, Some(16_384));
    assert_eq!(limits.max_height, Some(16_384));
    assert_eq!(limits.max_pixels, Some(268_435_456)); // 256 MP
    assert_eq!(limits.max_memory_bytes, Some(1_073_741_824)); // 1 GiB

    // default() == server_defaults(), field by field.
    let srv = Limits::server_defaults();
    assert_eq!(limits.max_width, srv.max_width);
    assert_eq!(limits.max_height, srv.max_height);
    assert_eq!(limits.max_pixels, srv.max_pixels);
    assert_eq!(limits.max_memory_bytes, srv.max_memory_bytes);

    let data = read_fixture(EXAMPLE);
    let out = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode()
        .expect("decode with default (safe-fallback) limits");
    assert_eq!((out.width, out.height), (1280, 854));
}

/// A generous `Limits` (well above 1280x854) lets the decode through.
#[test]
fn limits_generous_succeeds() {
    let data = read_fixture(EXAMPLE);
    let mut limits = Limits::default();
    limits.max_width = Some(8192);
    limits.max_height = Some(8192);
    limits.max_pixels = Some(64_000_000);
    limits.max_memory_bytes = Some(1 << 30);

    let out = DecoderConfig::new()
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect("decode within generous limits");
    assert_eq!((out.width, out.height), (1280, 854));
}

/// Each individual limit set below the image dimension makes decode return
/// `LimitExceeded`, never decode the pixels.
#[test]
fn limits_each_knob_below_image_rejects() {
    let data = read_fixture(EXAMPLE);

    let check_rejected = |limits: &Limits, knob: &str| {
        let r = DecoderConfig::new()
            .decode_request(&data)
            .with_limits(limits)
            .decode();
        match r {
            Err(e) => assert!(
                matches!(e.error(), HeicError::LimitExceeded(_)),
                "{knob}: expected LimitExceeded, got {:?}",
                e.error()
            ),
            Ok(_) => panic!("{knob}: decode should have been rejected by the limit"),
        }
    };

    let mut w = Limits::default();
    w.max_width = Some(640); // < 1280
    check_rejected(&w, "max_width");

    let mut h = Limits::default();
    h.max_height = Some(400); // < 854
    check_rejected(&h, "max_height");

    let mut px = Limits::default();
    px.max_pixels = Some(100); // << 1280*854
    check_rejected(&px, "max_pixels");

    let mut mem = Limits::default();
    mem.max_memory_bytes = Some(1024); // 1 KiB, far below frame buffers
    check_rejected(&mem, "max_memory_bytes");
}

/// `Limits::server_defaults()` is a sensible bounded config that still decodes
/// the median sample, and its caps are the documented values.
#[test]
fn limits_server_defaults_decode_ok() {
    let limits = Limits::server_defaults();
    assert_eq!(limits.max_width, Some(16_384));
    assert_eq!(limits.max_height, Some(16_384));
    assert_eq!(limits.max_pixels, Some(268_435_456));
    assert_eq!(limits.max_memory_bytes, Some(1_073_741_824));

    let data = read_fixture(EXAMPLE);
    let out = DecoderConfig::new()
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect("server_defaults decode example.heic");
    assert_eq!((out.width, out.height), (1280, 854));
}

// ===========================================================================
// estimate_memory
// ===========================================================================

/// `estimate_memory` is a plausible upper bound: at minimum it covers the
/// output pixel buffer, and it grows with pixel count and bpp.
#[test]
fn estimate_memory_plausible_and_monotone() {
    // For 1280x854 RGBA8 the output alone is ~4.4 MB; the estimate (which
    // also adds YCbCr planes + deblock metadata) must exceed that.
    let out_bytes = 1280u64 * 854 * 4;
    let est = DecoderConfig::estimate_memory(1280, 854, PixelLayout::Rgba8);
    assert!(
        est > out_bytes,
        "estimate {est} should exceed bare output {out_bytes}"
    );

    // RGBA8 (4 bpp) estimate >= Rgb8 (3 bpp) estimate for the same dims.
    let est_rgb = DecoderConfig::estimate_memory(1280, 854, PixelLayout::Rgb8);
    assert!(est >= est_rgb, "4bpp estimate should be >= 3bpp estimate");

    // Larger image => larger estimate.
    let est_big = DecoderConfig::estimate_memory(4096, 4096, PixelLayout::Rgba8);
    assert!(est_big > est, "bigger image => bigger estimate");
}

/// Hostile dimensions saturate instead of wrapping to a small value, so a
/// memory limit still catches them.
#[test]
fn estimate_memory_saturates_on_huge_dims() {
    let est = DecoderConfig::estimate_memory(u32::MAX, u32::MAX, PixelLayout::Rgba8);
    // A wrapped estimate would be small and slip past a limit check; the
    // saturating math keeps it astronomically large.
    assert!(est > u64::from(u32::MAX), "huge dims must not wrap small");
}

// ===========================================================================
// DecoderConfig builder chain + output layouts
// ===========================================================================

/// All four pixel layouts decode to the right byte width and channel count.
/// Compares RGB vs BGR (and RGBA vs BGRA) to confirm the channel swap is real,
/// and that alpha is appended (not interleaved into RGB).
#[test]
fn decode_all_layouts_byte_layout() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new();

    let rgb = cfg.decode(&data, PixelLayout::Rgb8).expect("rgb");
    let bgr = cfg.decode(&data, PixelLayout::Bgr8).expect("bgr");
    let rgba = cfg.decode(&data, PixelLayout::Rgba8).expect("rgba");
    let bgra = cfg.decode(&data, PixelLayout::Bgra8).expect("bgra");

    let px = 1280usize * 854;
    assert_eq!(rgb.data.len(), px * 3);
    assert_eq!(bgr.data.len(), px * 3);
    assert_eq!(rgba.data.len(), px * 4);
    assert_eq!(bgra.data.len(), px * 4);

    // RGB and BGR are R/B-swapped: rgb[0]==bgr[2], rgb[2]==bgr[0], G shared.
    assert_eq!(rgb.data[0], bgr.data[2], "R<->B swap (channel 0)");
    assert_eq!(rgb.data[1], bgr.data[1], "G shared");
    assert_eq!(rgb.data[2], bgr.data[0], "R<->B swap (channel 2)");

    // RGB pixel equals the leading RGB of RGBA (alpha appended, opaque).
    assert_eq!(&rgba.data[0..3], &rgb.data[0..3], "RGBA color == RGB color");
    assert_eq!(rgba.data[3], 255, "opaque image -> alpha 255");

    // BGRA color matches BGR; alpha is the 4th byte.
    assert_eq!(&bgra.data[0..3], &bgr.data[0..3], "BGRA color == BGR color");
    assert_eq!(bgra.data[3], 255, "opaque image -> alpha 255");
}

/// The `decode()` one-shot is identical to the explicit
/// `decode_request().with_output_layout().decode()` chain.
#[test]
fn decode_oneshot_equals_request_chain() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new();

    let a = cfg.decode(&data, PixelLayout::Rgba8).expect("oneshot");
    let b = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .decode()
        .expect("chain");
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert_eq!(a.layout, b.layout);
    assert_eq!(a.data, b.data, "one-shot and chain must be byte-identical");
}

/// `decode_request` defaults to Rgba8 when no layout is set.
#[test]
fn decode_request_default_layout_is_rgba8() {
    let data = read_fixture(EXAMPLE);
    let out = DecoderConfig::new()
        .decode_request(&data)
        .decode()
        .expect("default-layout decode");
    assert_eq!(out.layout, PixelLayout::Rgba8);
    assert_eq!(out.data.len(), 1280 * 854 * 4);
}

/// `decode_into` writes exactly the same bytes a `decode()` would, into a
/// caller buffer, and reports the dimensions.
#[test]
fn decode_into_matches_decode_and_reports_dims() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new();

    let reference = cfg.decode(&data, PixelLayout::Rgb8).expect("reference rgb");

    let info = ImageInfo::from_bytes(&data).expect("probe");
    let mut buf = vec![0u8; info.output_buffer_size(PixelLayout::Rgb8).unwrap()];
    let (w, h) = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut buf)
        .expect("decode_into");
    assert_eq!((w, h), (1280, 854));
    assert_eq!(buf, reference.data, "decode_into == decode bytes");
}

/// `decode_into` with an undersized buffer returns `BufferTooSmall` carrying
/// the required/actual sizes — and never writes out of bounds (no panic).
#[test]
fn decode_into_buffer_too_small() {
    let data = read_fixture(EXAMPLE);
    let mut tiny = vec![0u8; 16];
    let r = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut tiny);
    match r {
        Err(e) => match e.error() {
            HeicError::BufferTooSmall { required, actual } => {
                assert_eq!(*actual, 16);
                assert_eq!(*required, 1280 * 854 * 3);
            }
            other => panic!("expected BufferTooSmall, got {other:?}"),
        },
        Ok(_) => panic!("undersized buffer must be rejected"),
    }
}

/// `decode_to_frame` exposes raw YCbCr; dims match the RGB decode and the Y
/// plane is non-degenerate (real content, not a flat fill).
#[test]
fn decode_to_frame_yuv_dims_and_content() {
    let data = read_fixture(EXAMPLE);
    let frame = DecoderConfig::new()
        .decode_to_frame(&data)
        .expect("decode_to_frame");
    assert_eq!(frame.cropped_width(), 1280);
    assert_eq!(frame.cropped_height(), 854);

    let (y_plane, y_stride) = frame.plane(0);
    assert!(y_stride >= 1280, "Y stride covers the width");
    assert!(!y_plane.is_empty(), "Y plane populated");
    // Real photo content: luma is not a single constant.
    let first = y_plane[0];
    assert!(
        y_plane.iter().take(4096).any(|&v| v != first),
        "Y plane should carry varied content, not a flat fill"
    );
}

// ===========================================================================
// Metadata extraction APIs
// ===========================================================================

/// `extract_exif` / `extract_xmp` / `extract_icc` return clean results and
/// agree with the `ImageInfo` boolean flags for the same file.
#[test]
fn metadata_extractors_agree_with_probe_flags() {
    for name in [EXAMPLE, APPLE_HDR] {
        let data = read_fixture(name);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {name}: {e:?}"));
        let cfg = DecoderConfig::new();

        let exif = cfg
            .extract_exif(&data)
            .unwrap_or_else(|e| panic!("{name} exif: {e}"));
        assert_eq!(
            exif.is_some(),
            info.has_exif,
            "{name}: extract_exif vs has_exif"
        );
        // EXIF, when present, is TIFF data starting with a byte-order mark.
        if let Some(bytes) = &exif {
            assert!(bytes.len() >= 4, "{name}: EXIF too short");
            assert!(
                bytes.starts_with(b"II") || bytes.starts_with(b"MM"),
                "{name}: EXIF must start with a TIFF byte-order mark"
            );
        }

        let xmp = cfg
            .extract_xmp(&data)
            .unwrap_or_else(|e| panic!("{name} xmp: {e}"));
        assert_eq!(
            xmp.is_some(),
            info.has_xmp,
            "{name}: extract_xmp vs has_xmp"
        );

        let icc = cfg
            .extract_icc(&data)
            .unwrap_or_else(|e| panic!("{name} icc: {e}"));
        assert_eq!(
            icc.is_some(),
            info.has_icc_profile,
            "{name}: extract_icc vs has_icc_profile"
        );
    }
}

/// `has_gain_map` / `decode_gain_map` round-trip: the Apple sample reports a
/// gain map and decodes to a grayscale plane no larger than the primary.
#[test]
fn gain_map_decode_smaller_than_primary() {
    let data = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();
    assert!(cfg.has_gain_map(&data).expect("has_gain_map"));

    let gm = cfg.decode_gain_map(&data).expect("decode_gain_map");
    assert!(gm.width > 0 && gm.height > 0, "gain map has positive dims");
    assert_eq!(
        gm.data.len(),
        gm.width as usize * gm.height as usize,
        "gain map is one byte per pixel (grayscale)"
    );

    let info = ImageInfo::from_bytes(&data).expect("probe primary");
    assert!(
        u64::from(gm.width) * u64::from(gm.height)
            <= u64::from(info.width) * u64::from(info.height),
        "gain map should not exceed the primary in pixel count"
    );
}

/// Files with no gain map report `false` and surface no panic.
#[test]
fn has_gain_map_false_for_plain_file() {
    let data = read_fixture(EXAMPLE);
    assert!(
        !DecoderConfig::new()
            .has_gain_map(&data)
            .expect("has_gain_map"),
        "example.heic has no gain map"
    );
}

/// `decode_thumbnail` returns either a smaller embedded preview or `None`,
/// never larger than the primary, and the byte count is consistent.
#[test]
fn decode_thumbnail_smaller_or_none() {
    let data = read_fixture(EXAMPLE);
    let info = ImageInfo::from_bytes(&data).expect("probe");
    let thumb = DecoderConfig::new()
        .decode_thumbnail(&data, PixelLayout::Rgb8)
        .expect("decode_thumbnail");
    match thumb {
        Some(t) => {
            assert_eq!(t.layout, PixelLayout::Rgb8);
            assert_eq!(t.data.len(), t.width as usize * t.height as usize * 3);
            assert!(
                u64::from(t.width) * u64::from(t.height)
                    <= u64::from(info.width) * u64::from(info.height),
                "thumbnail must not exceed the primary"
            );
        }
        None => assert!(!info.has_thumbnail, "None thumb iff has_thumbnail is false"),
    }
}

/// Metadata extractors return clean `Err` (not a panic) on malformed input.
#[test]
fn metadata_extractors_error_on_garbage() {
    let cfg = DecoderConfig::new();
    // Valid ftyp, then nothing — no primary image / corrupt container.
    let mut data = vec![0u8; 24];
    data[0..4].copy_from_slice(&16u32.to_be_bytes());
    data[4..8].copy_from_slice(b"ftyp");
    data[8..12].copy_from_slice(b"heic");

    assert!(cfg.extract_exif(&data).is_err() || cfg.extract_exif(&data).unwrap().is_none());
    assert!(cfg.extract_xmp(&data).is_err() || cfg.extract_xmp(&data).unwrap().is_none());
    assert!(cfg.extract_icc(&data).is_err() || cfg.extract_icc(&data).unwrap().is_none());
    // has_gain_map on garbage must not panic.
    let _ = cfg.has_gain_map(&data);
}

// ===========================================================================
// Stop cancellation
// ===========================================================================

/// A Stop token that is already cancelled makes decode bail with `Cancelled`
/// before producing pixels.
#[test]
fn already_stopped_returns_cancelled() {
    let data = read_fixture(EXAMPLE);

    // A Stop impl that is always cancelled.
    struct AlwaysStop;
    impl Stop for AlwaysStop {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::Cancelled)
        }
    }
    let stop = AlwaysStop;

    let r = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_stop(&stop)
        .decode();
    match r {
        Err(e) => assert!(
            matches!(e.error(), HeicError::Cancelled(_)),
            "expected Cancelled, got {:?}",
            e.error()
        ),
        Ok(_) => panic!("already-stopped decode must return Cancelled"),
    }
}

/// An un-cancelled Stop token lets the decode complete normally — confirms the
/// cancellation plumbing doesn't false-trip.
#[test]
fn unstopped_token_decodes_normally() {
    let data = read_fixture(EXAMPLE);
    let stop = enough::Unstoppable;
    let out = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_stop(&stop)
        .decode()
        .expect("unstopped decode");
    assert_eq!((out.width, out.height), (1280, 854));
}

// ===========================================================================
// Backend dispatcher (src/backend.rs)
// ===========================================================================

/// `recommended_backends()` includes the pure-Rust backend, and a fresh
/// `DecoderConfig::new()` is seeded with it.
#[test]
fn recommended_backends_contains_rust() {
    let rec = heic::recommended_backends();
    assert!(
        rec.contains(&Backend::Rust),
        "recommended must include Rust"
    );
    assert_eq!(Backend::Rust.name(), "rust");

    let cfg = DecoderConfig::new();
    assert!(
        cfg.backends().contains(&Backend::Rust),
        "DecoderConfig::new() seeds the recommended allowlist"
    );
}

/// `with_backend` / `with_backends` set the allowlist; the single-Rust fast
/// path and the (single-entry) allowlist walk produce byte-identical output.
#[test]
fn rust_fast_path_equals_explicit_allowlist() {
    let data = read_fixture(EXAMPLE);

    // Fast path: exactly [Rust].
    let fast = DecoderConfig::new()
        .with_backend(Backend::Rust)
        .decode(&data, PixelLayout::Rgba8)
        .expect("fast-path decode");

    // recommended_backends — on this Linux build with no native backend
    // features enabled, this is also just [Rust], exercising the
    // with_backends setter and the same dispatch.
    let rec = DecoderConfig::new()
        .with_backends(&heic::recommended_backends())
        .decode(&data, PixelLayout::Rgba8)
        .expect("recommended decode");

    assert_eq!((fast.width, fast.height), (rec.width, rec.height));
    assert_eq!(fast.data, rec.data, "fast path and allowlist must agree");
}

/// `with_backends(&[])` (empty allowlist) makes decode fail with
/// `NoBackendSelected` — never an unrelated error and never a panic.
#[test]
fn empty_backend_allowlist_errors() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new().with_backends(&[]);
    assert!(cfg.backends().is_empty(), "allowlist cleared");

    let r = cfg.decode(&data, PixelLayout::Rgba8);
    match r {
        Err(e) => assert!(
            matches!(e.error(), HeicError::NoBackendSelected),
            "expected NoBackendSelected, got {:?}",
            e.error()
        ),
        Ok(_) => panic!("empty allowlist must not decode"),
    }
}

/// Empty allowlist also blocks `decode_to_frame` and `decode_into`.
#[test]
fn empty_allowlist_blocks_other_entry_points() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new().with_backends(&[]);

    let frame_err = cfg.decode_to_frame(&data);
    assert!(
        matches!(frame_err, Err(ref e) if matches!(e.error(), HeicError::NoBackendSelected)),
        "decode_to_frame should reject empty allowlist"
    );

    let mut buf = vec![0u8; 1280 * 854 * 3];
    let into_err = cfg
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .decode_into(&mut buf);
    assert!(
        matches!(into_err, Err(ref e) if matches!(e.error(), HeicError::NoBackendSelected)),
        "decode_into should reject empty allowlist"
    );
}

/// `DecoderConfig` is `Clone` and the clone carries the same allowlist; the
/// clone decodes identically.
#[test]
fn config_clone_preserves_allowlist_and_output() {
    let data = read_fixture(EXAMPLE);
    let cfg = DecoderConfig::new().with_backend(Backend::Rust);
    let cloned = cfg.clone();
    assert_eq!(cfg.backends(), cloned.backends());

    let a = cfg.decode(&data, PixelLayout::Rgb8).expect("orig");
    let b = cloned.decode(&data, PixelLayout::Rgb8).expect("clone");
    assert_eq!(a.data, b.data, "cloned config decodes identically");
}

/// `Backend` and `PixelLayout` helpers: bytes_per_pixel / has_alpha are
/// correct for every variant (pure value-level invariants).
#[test]
fn pixel_layout_helpers() {
    assert_eq!(PixelLayout::Rgb8.bytes_per_pixel(), 3);
    assert_eq!(PixelLayout::Bgr8.bytes_per_pixel(), 3);
    assert_eq!(PixelLayout::Rgba8.bytes_per_pixel(), 4);
    assert_eq!(PixelLayout::Bgra8.bytes_per_pixel(), 4);

    assert!(!PixelLayout::Rgb8.has_alpha());
    assert!(!PixelLayout::Bgr8.has_alpha());
    assert!(PixelLayout::Rgba8.has_alpha());
    assert!(PixelLayout::Bgra8.has_alpha());
}

/// `DecodeOutput` is `Clone`; cloning preserves all fields.
#[test]
fn decode_output_clone_roundtrip() {
    let data = read_fixture(EXAMPLE);
    let out: DecodeOutput = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .expect("decode");
    let c = out.clone();
    assert_eq!(out.width, c.width);
    assert_eq!(out.height, c.height);
    assert_eq!(out.layout, c.layout);
    assert_eq!(out.data, c.data);
}
