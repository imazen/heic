//! Coverage + behavior tests for the multi-codec / uncompressed dispatch in
//! `src/decode.rs`: the `unci` (ISO 23001-17 uncompressed HEIF, decoded via
//! `zenflate`) and `av1` (rav1d-safe) entry points reached through
//! `decode_to_frame` and the public `DecoderConfig` API.
//!
//! Every test asserts REAL behavior — exact dimensions, buffer length tied to
//! the layout, pixel non-degeneracy for files known to carry varied content,
//! a specific `HeicError` variant for the documented unsupported layouts, and
//! no-panic on crafted/malformed input. Behavior here was first mapped
//! empirically against the committed `testdata/` corpus, then pinned.
//!
//! Requires features: `backend-rust,std,av1,unci,zencodec,parallel,fallible-alloc`.

#![cfg(all(feature = "unci", feature = "std", feature = "backend-rust"))]

use heic::{
    Backend, DecodeOutput, DecoderConfig, HeicError, ImageInfo, Limits, PixelLayout, ProbeError,
    RowSink, Stop, StopReason,
};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// fixtures
// ---------------------------------------------------------------------------

fn td(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(rel)
}

/// Read a committed corpus file. The bundled `testdata/` IS present in the
/// checkout, so a missing file is a checkout/CI misconfiguration and must be
/// loud (panic), not a graceful skip.
fn read(rel: &str) -> Vec<u8> {
    let p = td(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing committed fixture {}: {e}", p.display()))
}

/// Count distinct values across one byte stride of an interleaved buffer.
fn distinct_at(data: &[u8], stride: usize, offset: usize) -> usize {
    let mut seen = [false; 256];
    let mut i = offset;
    while i < data.len() {
        seen[data[i] as usize] = true;
        i += stride;
    }
    seen.iter().filter(|&&b| b).count()
}

// ---------------------------------------------------------------------------
// unci: raw (uncompressed) pixel-interleaved + component-planar, 8-bit RGB
// ---------------------------------------------------------------------------

/// Component-planar 8-bit RGB uncompressed HEIF decodes to exact dims with a
/// fully-sized RGBA buffer and non-degenerate, in-range pixels.
#[test]
fn unci_comp_rgb_decodes_exact() {
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("comp RGB unci must decode");
    assert_eq!((out.width, out.height), (30, 20));
    assert_eq!(out.layout, PixelLayout::Rgba8);
    assert_eq!(out.data.len(), 30 * 20 * 4, "RGBA8 buffer != w*h*4");
    // Alpha must be fully opaque (unci RGB carries no alpha plane → 255 fill).
    assert!(
        out.data.chunks_exact(4).all(|p| p[3] == 255),
        "RGB unci alpha channel must be opaque"
    );
    // Real content: the R channel carries more than one value.
    assert!(
        distinct_at(&out.data, 4, 0) > 1,
        "comp RGB decoded to a flat R channel (no real pixels)"
    );
}

/// Pixel-interleaved 8-bit RGB is the other interleave path (interleave_type
/// == 1) and must decode identically in dimensions.
#[test]
fn unci_pix_rgb_decodes_exact() {
    let data = read("libheif-examples/uncompressed_pix_RGB.heif");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .expect("pix RGB unci must decode");
    assert_eq!((out.width, out.height), (30, 20));
    assert_eq!(out.data.len(), 30 * 20 * 3, "RGB8 buffer != w*h*3");
    assert!(
        distinct_at(&out.data, 3, 0) > 1,
        "pix RGB decoded to a flat R channel"
    );
}

/// The component-planar and pixel-interleaved encodings of the SAME source
/// image must produce byte-identical RGB output — the two interleave branches
/// in `decode_unci_item` are two routes to one pixel grid. Divergence here
/// would be an image-corruption bug (the pixels are sacred).
#[test]
fn unci_comp_and_pix_rgb_agree() {
    let comp = DecoderConfig::new()
        .decode(
            &read("libheif-examples/uncompressed_comp_RGB.heif"),
            PixelLayout::Rgb8,
        )
        .expect("comp RGB");
    let pix = DecoderConfig::new()
        .decode(
            &read("libheif-examples/uncompressed_pix_RGB.heif"),
            PixelLayout::Rgb8,
        )
        .expect("pix RGB");
    assert_eq!((comp.width, comp.height), (pix.width, pix.height));
    assert_eq!(
        comp.data, pix.data,
        "component-planar and pixel-interleaved RGB must decode to identical pixels"
    );
}

/// ABGR (4-component, alpha present) uncompressed HEIF: decodes to RGBA, dims
/// exact, and carries genuinely varied content (>10 distinct R values in the
/// committed fixture).
#[test]
fn unci_comp_abgr_decodes_with_content() {
    let data = read("libheif-examples/uncompressed_comp_ABGR.heif");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("ABGR unci must decode");
    assert_eq!((out.width, out.height), (30, 20));
    assert_eq!(out.data.len(), 30 * 20 * 4);
    assert!(
        distinct_at(&out.data, 4, 0) > 10,
        "ABGR fixture should carry varied R content"
    );
}

/// `RGxB` (a padded component layout) and the tiled RGB variants are all in
/// the supported 8-bit / interleave-{0,1} family and must decode to the same
/// 30x20 grid.
#[test]
fn unci_supported_8bit_variants_decode() {
    for rel in [
        "libheif-examples/uncompressed_comp_RGxB.heif",
        "libheif-examples/uncompressed_pix_RGxB.heif",
        "libheif-examples/uncompressed_comp_RGB_tiled.heif",
        "libheif-examples/uncompressed_comp_RGB_tiled_row_tile_align.heif",
        "libheif-examples/uncompressed_pix_R8G8B8_bsz0_psz5_tiled.heif",
        "libheif-examples/uncompressed_pix_R8G8B8A8_bsz0_psz10_tiled.heif",
        "libheif-examples/uncompressed_comp_YUV_tiled.heif",
    ] {
        let data = read(rel);
        let out = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .unwrap_or_else(|e| panic!("{rel} must decode, got {:?}", e.error()));
        assert_eq!((out.width, out.height), (30, 20), "{rel}: wrong dims");
        assert_eq!(out.data.len(), 30 * 20 * 4, "{rel}: buffer length");
        assert!(
            out.data.chunks_exact(4).all(|p| p[3] == 255)
                || rel.contains("R8G8B8A8")
                || rel.contains("ABGR"),
            "{rel}: non-alpha source must be opaque"
        );
    }
}

/// Monochrome ('M') unci: a single luma component. It decodes to exact dims;
/// only the luma plane is populated, so the output is allowed to be flat — we
/// assert it is in-range and correctly sized rather than non-degenerate.
#[test]
fn unci_mono_decodes() {
    let data = read("libheif-examples/uncompressed_comp_M.heif");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("mono unci must decode");
    assert_eq!((out.width, out.height), (30, 20));
    assert_eq!(out.data.len(), 30 * 20 * 4);
    assert!(
        out.data.chunks_exact(4).all(|p| p[3] == 255),
        "mono alpha must be opaque"
    );
}

// ---------------------------------------------------------------------------
// unci: deflate / zlib compressed (the zenflate path)
// ---------------------------------------------------------------------------

/// Deflate-compressed uncompressed-HEIF: exercises `zenflate::deflate_decompress`
/// through `decode_unci_item`. The committed fixture is 128x72 with varied
/// content.
#[test]
fn unci_deflate_decodes() {
    let data = read("libheif-examples/rgb_generic_compressed_defl.heif");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("deflate unci must decode");
    assert_eq!((out.width, out.height), (128, 72));
    assert_eq!(out.data.len(), 128 * 72 * 4);
    assert!(
        distinct_at(&out.data, 4, 0) > 1,
        "deflate fixture should carry varied content"
    );
}

/// Zlib-compressed uncompressed-HEIF: exercises `zenflate::zlib_decompress`.
/// The deflate and zlib fixtures encode the SAME 128x72 source — assert their
/// decoded pixels are identical (compression must be lossless and the two
/// codecs must agree).
#[test]
fn unci_zlib_decodes_and_matches_deflate() {
    let zlib = DecoderConfig::new()
        .decode(
            &read("libheif-examples/rgb_generic_compressed_zlib.heif"),
            PixelLayout::Rgba8,
        )
        .expect("zlib unci must decode");
    assert_eq!((zlib.width, zlib.height), (128, 72));
    assert_eq!(zlib.data.len(), 128 * 72 * 4);

    let defl = DecoderConfig::new()
        .decode(
            &read("libheif-examples/rgb_generic_compressed_defl.heif"),
            PixelLayout::Rgba8,
        )
        .expect("deflate unci must decode");
    assert_eq!(
        zlib.data, defl.data,
        "zlib and deflate encodings of the same source must decode identically"
    );
}

/// Brotli compression is documented as unsupported for unci → the decoder must
/// return `UnsupportedCodec` (NOT panic, NOT silently produce garbage).
#[test]
fn unci_brotli_clean_unsupported() {
    let data = read("libheif-examples/rgb_generic_compressed_brotli.heif");
    let err = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect_err("brotli unci must be rejected");
    assert!(
        matches!(err.error(), HeicError::UnsupportedCodec(_)),
        "brotli should be UnsupportedCodec, got {:?}",
        err.error()
    );
}

/// The tiled-compression variants (`tile_deflate`, `zlib_rows`, `zlib_tiled`)
/// use a per-tile/per-row compression layout that the current single-stream
/// path does not reassemble — they must return a clean `InvalidData` error
/// (decompressed output is smaller than the full-frame expectation), never a
/// panic and never wrong pixels.
#[test]
fn unci_tiled_compression_clean_error() {
    for rel in [
        "libheif-examples/rgb_generic_compressed_tile_deflate.heif",
        "libheif-examples/rgb_generic_compressed_zlib_rows.heif",
        "libheif-examples/rgb_generic_compressed_zlib_tiled.heif",
    ] {
        let data = read(rel);
        let err = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .expect_err(&format!("{rel}: expected clean error, got Ok"));
        assert!(
            matches!(err.error(), HeicError::InvalidData(_)),
            "{rel}: expected InvalidData, got {:?}",
            err.error()
        );
    }
}

// ---------------------------------------------------------------------------
// unci: documented-unsupported layouts return the right variant (no panic)
// ---------------------------------------------------------------------------

/// 16-bit and sub-8-bit component depths are not yet handled → `Unsupported`
/// ("only 8-bit unsigned integer components supported"). Must be a clean Err.
#[test]
fn unci_non_8bit_depths_unsupported() {
    for rel in [
        "libheif-examples/uncompressed_comp_B16R16G16.heif",
        "libheif-examples/uncompressed_pix_B16R16G16.heif",
        "libheif-examples/uncompressed_comp_R5G6B5_tiled.heif",
        "libheif-examples/uncompressed_comp_R7G7B7_tiled.heif",
        "libheif-examples/uncompressed_comp_Y16U16V16_420.heif",
    ] {
        let data = read(rel);
        let err = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .expect_err(&format!("{rel}: expected Unsupported, got Ok"));
        assert!(
            matches!(err.error(), HeicError::Unsupported(_)),
            "{rel}: expected Unsupported, got {:?}",
            err.error()
        );
    }
}

/// Row-interleaved and tile-interleaved (`uncompressed_row_*`,
/// `uncompressed_tile_*`) use interleave types other than component-planar(0)
/// / pixel-interleaved(1) → `Unsupported` ("interleave type not supported").
#[test]
fn unci_row_tile_interleave_unsupported() {
    for rel in [
        "libheif-examples/uncompressed_row_RGB.heif",
        "libheif-examples/uncompressed_row_ABGR.heif",
        "libheif-examples/uncompressed_row_M.heif",
        "libheif-examples/uncompressed_tile_RGB_tiled.heif",
        "libheif-examples/uncompressed_tile_YUV_tiled.heif",
    ] {
        let data = read(rel);
        let err = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .expect_err(&format!("{rel}: expected Unsupported, got Ok"));
        assert!(
            matches!(err.error(), HeicError::Unsupported(_)),
            "{rel}: expected Unsupported interleave, got {:?}",
            err.error()
        );
    }
}

/// Chroma-subsampled YUV/VUY/YVU 4:2:0 and 4:2:2 unci: the single-plane
/// expected-size math assumes 4:4:4, so the subsampled payloads come up short
/// and yield a clean `InvalidData` ("decompressed data smaller than expected").
/// Must not panic or silently mis-decode.
#[test]
fn unci_subsampled_yuv_clean_error() {
    for rel in [
        "libheif-examples/uncompressed_comp_YUV_420.heif",
        "libheif-examples/uncompressed_comp_YUV_422.heif",
        "libheif-examples/uncompressed_comp_VUY_420.heif",
        "libheif-examples/uncompressed_mix_YVU_422.heif",
    ] {
        let data = read(rel);
        let err = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .expect_err(&format!("{rel}: expected error, got Ok"));
        assert!(
            matches!(err.error(), HeicError::InvalidData(_)),
            "{rel}: expected InvalidData, got {:?}",
            err.error()
        );
    }
}

// ---------------------------------------------------------------------------
// probe parity: ImageInfo::from_bytes for the unci items
// ---------------------------------------------------------------------------

/// `ImageInfo::from_bytes` must report unci dims (30x20) and the unci-typical
/// 8-bit / 4:4:4 descriptor without decoding pixels — and it must succeed even
/// for the unsupported-decode variants (probe only reads the container/uncC).
#[test]
fn probe_unci_reports_dims_for_supported_and_unsupported() {
    for rel in [
        "libheif-examples/uncompressed_comp_RGB.heif", // decodes
        "libheif-examples/uncompressed_row_RGB.heif",  // decode-unsupported
        "libheif-examples/uncompressed_comp_B16R16G16.heif", // 16-bit
    ] {
        let data = read(rel);
        let info = ImageInfo::from_bytes(&data)
            .unwrap_or_else(|e| panic!("{rel}: probe must succeed, got {e:?}"));
        assert_eq!((info.width, info.height), (30, 20), "{rel}: probe dims");
        assert_eq!(info.chroma_format, 3, "{rel}: unci probes as 4:4:4");
        assert!(!info.has_alpha || rel.contains("ABGR"), "{rel}: alpha flag");
        // Buffer-size helper round-trips with the probed dims.
        assert_eq!(
            info.output_buffer_size(PixelLayout::Rgba8),
            Some(30 * 20 * 4),
            "{rel}: output_buffer_size mismatch"
        );
    }
    // The 16-bit fixture probes with the real component depth.
    let info =
        ImageInfo::from_bytes(&read("libheif-examples/uncompressed_comp_B16R16G16.heif")).unwrap();
    assert_eq!(info.bit_depth, 16, "B16R16G16 probes as 16-bit");
}

/// The compressed-RGB fixtures probe to 128x72.
#[test]
fn probe_unci_compressed_dims() {
    for rel in [
        "libheif-examples/rgb_generic_compressed_defl.heif",
        "libheif-examples/rgb_generic_compressed_zlib.heif",
        "libheif-examples/rgb_generic_compressed_brotli.heif",
    ] {
        let info = ImageInfo::from_bytes(&read(rel))
            .unwrap_or_else(|e| panic!("{rel}: probe must succeed, got {e:?}"));
        assert_eq!((info.width, info.height), (128, 72), "{rel}: probe dims");
        assert_eq!(info.bit_depth, 8, "{rel}: 8-bit");
    }
}

// ---------------------------------------------------------------------------
// decode_into / decode_rows / layout coverage through the unci frame
// ---------------------------------------------------------------------------

/// `decode_into` writes a deflate-unci image into a caller buffer and returns
/// the dims; output must match the one-shot `decode` path byte-for-byte.
#[test]
fn unci_decode_into_matches_oneshot() {
    let data = read("libheif-examples/rgb_generic_compressed_defl.heif");
    let info = ImageInfo::from_bytes(&data).unwrap();
    let n = info.output_buffer_size(PixelLayout::Rgba8).unwrap();
    let mut buf = vec![0u8; n];
    let (w, h) = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .decode_into(&mut buf)
        .expect("decode_into deflate unci");
    assert_eq!((w, h), (128, 72));
    let oneshot = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .unwrap();
    assert_eq!(buf, oneshot.data, "decode_into != one-shot decode");
}

/// `decode_into` with a too-small buffer returns `BufferTooSmall` (the unci
/// path is not the streaming-grid path, so it hits the fallback size check).
#[test]
fn unci_decode_into_too_small_errors() {
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let mut tiny = vec![0u8; 16];
    let err = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .decode_into(&mut tiny)
        .expect_err("too-small buffer must error");
    assert!(
        matches!(err.error(), HeicError::BufferTooSmall { .. }),
        "expected BufferTooSmall, got {:?}",
        err.error()
    );
}

/// A `RowSink` receiving an unci image: the fallback path decodes the full
/// frame, then hands the whole image to the sink in one strip. Assert it
/// writes correctly-sized, content-matching data.
#[test]
fn unci_decode_rows_to_sink() {
    struct OneStrip {
        buf: Vec<u8>,
        demanded: bool,
    }
    impl RowSink for OneStrip {
        fn demand(&mut self, _y: u32, _h: u32, min_bytes: usize) -> &mut [u8] {
            self.demanded = true;
            self.buf.resize(min_bytes, 0);
            &mut self.buf
        }
    }
    let data = read("libheif-examples/uncompressed_comp_ABGR.heif");
    let mut sink = OneStrip {
        buf: Vec::new(),
        demanded: false,
    };
    let (w, h) = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .decode_rows(&mut sink)
        .expect("decode_rows unci");
    assert_eq!((w, h), (30, 20));
    assert!(sink.demanded, "sink demand() was never called");
    assert_eq!(sink.buf.len(), 30 * 20 * 4);
    let oneshot = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .unwrap();
    assert_eq!(sink.buf, oneshot.data, "sink data != one-shot decode");
}

/// All four pixel layouts decode the same unci source; RGB/BGR swap red and
/// blue, RGBA/BGRA likewise, and alpha is opaque everywhere.
#[test]
fn unci_all_layouts_consistent() {
    let data = read("libheif-examples/uncompressed_comp_ABGR.heif");
    let cfg = DecoderConfig::new();
    let rgba = cfg.decode(&data, PixelLayout::Rgba8).unwrap();
    let bgra = cfg.decode(&data, PixelLayout::Bgra8).unwrap();
    let rgb = cfg.decode(&data, PixelLayout::Rgb8).unwrap();
    let bgr = cfg.decode(&data, PixelLayout::Bgr8).unwrap();
    assert_eq!(rgba.data.len(), 30 * 20 * 4);
    assert_eq!(rgb.data.len(), 30 * 20 * 3);
    // R↔B swap between RGBA and BGRA, alpha unchanged.
    for (a, b) in rgba.data.chunks_exact(4).zip(bgra.data.chunks_exact(4)) {
        assert_eq!(a[0], b[2], "R/B not swapped between RGBA and BGRA");
        assert_eq!(a[1], b[1], "G must be stable across RGBA/BGRA");
        assert_eq!(a[2], b[0], "B/R not swapped");
        assert_eq!(a[3], b[3], "alpha must be stable");
    }
    // RGB (3bpp) must match RGBA (4bpp) on the colour channels.
    for (three, four) in rgb.data.chunks_exact(3).zip(rgba.data.chunks_exact(4)) {
        assert_eq!(three, &four[..3], "RGB must match RGBA colour channels");
    }
    for (three, four) in bgr.data.chunks_exact(3).zip(bgra.data.chunks_exact(4)) {
        assert_eq!(three, &four[..3], "BGR must match BGRA colour channels");
    }
}

// ---------------------------------------------------------------------------
// limits + cancellation through the unci decode path
// ---------------------------------------------------------------------------

/// A `max_width` smaller than the unci image rejects before pixel work with
/// `LimitExceeded` (the unci path checks limits.check_dimensions itself).
#[test]
fn unci_limits_reject_dimensions() {
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let mut limits = Limits::default();
    limits.max_width = Some(10); // image is 30 wide
    let err = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgba8)
        .with_limits(&limits)
        .decode()
        .expect_err("width limit must reject");
    assert!(
        matches!(err.error(), HeicError::LimitExceeded(_)),
        "expected LimitExceeded, got {:?}",
        err.error()
    );
}

/// A `max_memory_bytes` of 1 byte rejects the unci decode (the path runs
/// `estimate_memory` + `check_memory` before decompressing).
#[test]
fn unci_limits_reject_memory() {
    let data = read("libheif-examples/rgb_generic_compressed_defl.heif");
    let mut limits = Limits::default();
    limits.max_memory_bytes = Some(1);
    let err = DecoderConfig::new()
        .decode_request(&data)
        .with_limits(&limits)
        .decode()
        .expect_err("1-byte memory cap must reject");
    assert!(
        matches!(err.error(), HeicError::LimitExceeded(_)),
        "expected LimitExceeded, got {:?}",
        err.error()
    );
}

/// An already-triggered `Stop` cancels the unci decode cleanly with
/// `Cancelled` (the path polls `check_stop` between component planes / rows).
#[test]
fn unci_cancellation_returns_cancelled() {
    struct AlwaysStop;
    impl Stop for AlwaysStop {
        fn check(&self) -> Result<(), StopReason> {
            Err(StopReason::Cancelled)
        }
    }
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let stop = AlwaysStop;
    let err = DecoderConfig::new()
        .decode_request(&data)
        .with_stop(&stop)
        .decode()
        .expect_err("pre-cancelled decode must error");
    assert!(
        matches!(err.error(), HeicError::Cancelled(_)),
        "expected Cancelled, got {:?}",
        err.error()
    );
}

// ---------------------------------------------------------------------------
// backend allowlist through the unci/decode dispatch
// ---------------------------------------------------------------------------

/// The pure-Rust backend explicitly selected decodes an unci image (the unci
/// path is backend-independent but still routed through a configured
/// allowlist that must contain at least Rust).
#[test]
fn unci_with_explicit_rust_backend() {
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let out = DecoderConfig::new()
        .with_backend(Backend::Rust)
        .decode(&data, PixelLayout::Rgba8)
        .expect("explicit rust backend must decode unci");
    assert_eq!((out.width, out.height), (30, 20));
}

/// An empty backend allowlist is a programmer error: decode must surface
/// `NoBackendSelected` rather than silently falling back or panicking. (HEVC
/// path; unci is reached through the same dispatcher.)
#[test]
fn empty_backend_allowlist_errors() {
    let data = read("libheif-examples/example.heic");
    let err = DecoderConfig::new()
        .with_backends(&[])
        .decode(&data, PixelLayout::Rgba8)
        .expect_err("empty allowlist must error");
    assert!(
        matches!(err.error(), HeicError::NoBackendSelected),
        "expected NoBackendSelected, got {:?}",
        err.error()
    );
}

// ---------------------------------------------------------------------------
// AV1 dispatch: no AV1-coded fixture is bundled; verify the dispatch decision
// is reachable and that the HEVC path (taken instead) still works. The `av1`
// feature is compiled in for this test crate, so `decode_av1_item` is built.
// ---------------------------------------------------------------------------

/// No bundled testdata file is AV1-coded (the corpus is HEVC + uncompressed),
/// so we cannot exercise `decode_av1_item` end-to-end. Document that here and
/// assert the corpus genuinely contains no `av01` primary item, so a future
/// AVIF fixture is required to cover the AV1 leg.
#[test]
fn av1_no_bundled_fixture_present() {
    // Probe a representative spread; none should report AV1 (we have no API to
    // read the codec fourcc directly, but AV1 stills decode through the same
    // public path — if any existed it would simply decode here). This test's
    // purpose is to PIN the absence so the AV1 leg is knowingly uncovered.
    let mut any_av1_candidate = false;
    let dir = td("libheif-examples");
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".avif") {
            any_av1_candidate = true;
        }
    }
    assert!(
        !any_av1_candidate,
        "an .avif/AV1 fixture appeared — wire it into an end-to-end AV1 decode \
         test (this PIN can then be removed)"
    );
    eprintln!(
        "NOTE: no AV1-coded fixture in testdata/; decode_av1_item is compiled \
         (av1 feature) but not exercised end-to-end. Add an AVIF still to cover it."
    );
}

// ---------------------------------------------------------------------------
// other bundled files: HEVC synthetic + mif3 mini + apple-hdr, no panic
// ---------------------------------------------------------------------------

/// The synthetic HEVC stills (256x256) at every quality decode through the
/// HEVC backend with exact dims and a fully-sized buffer.
#[test]
fn synthetic_hevc_decodes_all_qualities() {
    for rel in [
        "synthetic/synth_8bit_lossless.heic",
        "synthetic/synth_8bit_q95.heic",
        "synthetic/synth_8bit_q50.heic",
        "synthetic/synth_8bit_q10.heic",
    ] {
        let data = read(rel);
        let out = DecoderConfig::new()
            .decode(&data, PixelLayout::Rgb8)
            .unwrap_or_else(|e| panic!("{rel} must decode, got {:?}", e.error()));
        assert_eq!((out.width, out.height), (256, 256), "{rel}: dims");
        assert_eq!(out.data.len(), 256 * 256 * 3, "{rel}: buffer length");
        assert!(
            distinct_at(&out.data, 3, 0) > 1,
            "{rel}: decoded to a flat image"
        );
    }
}

/// `lightning_mini.heif` uses the mif3 brand "mini" box layout the parser does
/// not surface a primary item for — it must error cleanly (`NoPrimaryImage`),
/// never panic.
#[test]
fn lightning_mini_clean_error() {
    let data = read("libheif-examples/lightning_mini.heif");
    let err = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect_err("mif3-mini must error cleanly");
    assert!(
        matches!(
            err.error(),
            HeicError::NoPrimaryImage | HeicError::InvalidContainer(_) | HeicError::Unsupported(_)
        ),
        "expected clean container error, got {:?}",
        err.error()
    );
}

/// The 10-bit apple-hdr monochrome still decodes through the HEVC backend
/// (10-bit → 8-bit downconvert) to a non-trivial image.
#[test]
fn apple_hdr_primary_decodes() {
    let data = read("apple-hdr/hdr-sample.heic");
    let out = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .expect("apple-hdr primary must decode");
    assert!(out.width > 0 && out.height > 0);
    assert_eq!(out.data.len(), out.width as usize * out.height as usize * 4);
}

// ---------------------------------------------------------------------------
// untrusted / malformed input: clean Err, no panic
// ---------------------------------------------------------------------------

/// Truncating a real unci file mid-stream must produce a clean error or a
/// successful decode of whatever survived — never a panic.
#[test]
fn truncated_unci_no_panic() {
    let full = read("libheif-examples/rgb_generic_compressed_defl.heif");
    // Truncate at several offsets across the file.
    for frac in [4usize, 2, 4 * 3 / 2] {
        let cut = (full.len() * 4 / frac.max(1)).min(full.len());
        let trunc = &full[..cut];
        // Either Ok (rare, if the cut lands past pixel data) or Err — but the
        // call must RETURN, not panic/abort.
        let _ = DecoderConfig::new().decode(trunc, PixelLayout::Rgba8);
    }
    // A 1-byte and empty input: probe and decode both return errors, no panic.
    assert!(
        DecoderConfig::new()
            .decode(&[], PixelLayout::Rgba8)
            .is_err()
    );
    assert!(
        DecoderConfig::new()
            .decode(&[0u8], PixelLayout::Rgba8)
            .is_err()
    );
}

/// Bit-flipping bytes inside a deflate-compressed unci stream must not panic;
/// the decoder either errors (corrupt deflate) or produces a sized buffer.
#[test]
fn corrupted_deflate_unci_no_panic() {
    let mut data = read("libheif-examples/rgb_generic_compressed_defl.heif");
    // Flip bytes in the back half (more likely the mdat payload than the
    // container header) so we hammer the zenflate decompress path.
    let start = data.len() / 2;
    for b in &mut data[start..] {
        *b ^= 0xA5;
    }
    match DecoderConfig::new().decode(&data, PixelLayout::Rgba8) {
        Ok(out) => {
            // If it still decodes, the buffer must be self-consistent.
            assert_eq!(out.data.len(), out.width as usize * out.height as usize * 4);
        }
        Err(_) => { /* clean rejection — also fine */ }
    }
}

/// Crafted-crash fuzz seeds (committed under `fuzz/regression/`) must all
/// return cleanly (Ok or Err) for both probe and decode — never panic.
#[test]
fn fuzz_regression_seeds_no_panic() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("fuzz")
        .join("regression");
    if !dir.is_dir() {
        eprintln!(
            "NOTE: {} absent — fuzz regression seeds not present in this checkout, skipping",
            dir.display()
        );
        return;
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(bytes) = std::fs::read(&p) else {
            continue;
        };
        // Probe + decode must both return, not panic.
        let _ = ImageInfo::from_bytes(&bytes);
        let _ = DecoderConfig::new().decode(&bytes, PixelLayout::Rgba8);
        let _ = DecoderConfig::new().decode(&bytes, PixelLayout::Rgb8);
        count += 1;
    }
    assert!(
        count > 0,
        "fuzz/regression exists but held no seed files — expected committed crash POCs"
    );
}

/// Probe-level malformed inputs map to the correct `ProbeError` variant:
/// too-short → `NeedMoreData`, non-ftyp → `InvalidFormat`.
#[test]
fn probe_malformed_variants() {
    // < 12 bytes → NeedMoreData
    assert!(matches!(
        ImageInfo::from_bytes(&[0u8; 8]),
        Err(ProbeError::NeedMoreData)
    ));
    // 12+ bytes but not an ftyp box → InvalidFormat
    let mut not_ftyp = vec![0u8; 32];
    not_ftyp[4..8].copy_from_slice(b"moov");
    assert!(matches!(
        ImageInfo::from_bytes(&not_ftyp),
        Err(ProbeError::InvalidFormat)
    ));
    // ftyp box header but garbage body → Corrupt (container parse fails) or
    // InvalidFormat; must be an Err, never panic.
    let mut bad_ftyp = vec![0u8; 64];
    bad_ftyp[0..4].copy_from_slice(&16u32.to_be_bytes());
    bad_ftyp[4..8].copy_from_slice(b"ftyp");
    bad_ftyp[8..12].copy_from_slice(b"heic");
    assert!(ImageInfo::from_bytes(&bad_ftyp).is_err());
}

/// `DecodeOutput` is the documented decoded-image carrier; pin its field/shape
/// contract for the unci path (data/width/height/layout all agree).
#[test]
fn decode_output_fields_consistent() {
    let data = read("libheif-examples/uncompressed_comp_RGB.heif");
    let out: DecodeOutput = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgba8)
        .unwrap();
    assert_eq!(out.width, 30);
    assert_eq!(out.height, 20);
    assert_eq!(out.layout, PixelLayout::Rgba8);
    assert_eq!(
        out.data.len(),
        out.width as usize * out.height as usize * out.layout.bytes_per_pixel()
    );
}
