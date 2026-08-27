//! 4:2:2 chroma (`chroma_format_idc == 2`) decode coverage — issue #48.
//!
//! Fixtures come from `scripts/gen_422_fixtures.py` (deterministic synthetic
//! content, 80x56, multiple partial CTUs):
//!
//! * `testdata/hevc422/x265_422_{8,10}bit_lossless.hevc` — x265 `--lossless`
//!   raw Annex-B streams coded straight from the committed i422 sources
//!   `src_422_{8,10}bit_80x56.yuv`. A correct decode reproduces the source
//!   planes bit-exactly (dec265 does), so the oracle is the source itself.
//!   Exercises the 4:2:2 transform-tree syntax (two cbf_cb/cbf_cr per node),
//!   stacked chroma TBs, Table 8-3 chroma-mode remap and intra prediction
//!   order — under `cu_transquant_bypass`, so no dequant / loop filters.
//! * `testdata/features/yuv422{,_10bit}.heic` — heif-enc (libheif + x265,
//!   `-p chroma=422`, lossy, q=35 / q=60) with libde265's `dec265` decode of
//!   the coded bitstream as `testdata/hevc422/*.ref.yuv` (80x64 coded, `clap`
//!   crops the bottom 8 rows). Exercises dequant/transform, the
//!   ChromaArrayType != 1 chroma QP derivation, 4:2:2 deblocking and SAO.
//!
//! Every comparison is sample-exact on the YCbCr planes. Per this project's
//! "false positives are the highest-severity bug" rule, nothing here passes
//! on "the decoder accepted the bytes".

use heic::hevc::DecodedFrame;
use heic::{DecoderConfig, ImageInfo, PixelLayout};
use std::path::PathBuf;

const W: u32 = 80;
const H: u32 = 56;

fn fixture(rel: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("testdata")
        .join(rel);
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing committed fixture {p:?}: {e}"))
}

/// Planar i422 reference: Y is `w x h`, Cb/Cr are `w/2 x h`.
struct Planes {
    w: u32,
    h: u32,
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
}

fn load_i422(bytes: &[u8], w: u32, h: u32, depth: u8) -> Planes {
    let n = (w * h) as usize;
    let cn = (w / 2 * h) as usize;
    let samples: Vec<u16> = if depth == 8 {
        assert_eq!(bytes.len(), n + 2 * cn, "i422 8-bit size for {w}x{h}");
        bytes.iter().map(|&b| u16::from(b)).collect()
    } else {
        assert_eq!(
            bytes.len(),
            2 * (n + 2 * cn),
            "i422 16-bit size for {w}x{h}"
        );
        bytes
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    };
    Planes {
        w,
        h,
        y: samples[..n].to_vec(),
        cb: samples[n..n + cn].to_vec(),
        cr: samples[n + cn..].to_vec(),
    }
}

/// Compare one plane over the frame's visible window, sample-exact.
///
/// `ours` is indexed with `stride`; `expected` is the full coded picture
/// (`exp_w` wide), so both are addressed by absolute coded coordinates.
#[allow(clippy::too_many_arguments)]
fn assert_plane_exact(
    label: &str,
    plane: &str,
    ours: &[u16],
    stride: usize,
    expected: &[u16],
    exp_w: usize,
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
) {
    let mut mismatches = 0usize;
    let mut max_diff = 0i32;
    let mut first: Option<(u32, u32, u16, u16)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            let o = ours[y as usize * stride + x as usize];
            let e = expected[y as usize * exp_w + x as usize];
            if o != e {
                mismatches += 1;
                max_diff = max_diff.max((i32::from(o) - i32::from(e)).abs());
                if first.is_none() {
                    first = Some((x, y, o, e));
                }
            }
        }
    }
    assert_eq!(
        mismatches,
        0,
        "{label}: {plane} plane differs in {mismatches} of {} samples (max |diff| {max_diff}); \
         first at ({},{}) ours={} expected={}",
        (x1 - x0) * (y1 - y0),
        first.unwrap().0,
        first.unwrap().1,
        first.unwrap().2,
        first.unwrap().3
    );
}

fn assert_frame_matches(label: &str, frame: &DecodedFrame, expected: &Planes) {
    assert_eq!(frame.chroma_format, 2, "{label}: chroma_format");
    assert_eq!(frame.width, expected.w, "{label}: coded width");
    assert_eq!(frame.height, expected.h, "{label}: coded height");
    assert_eq!(
        (frame.cropped_width(), frame.cropped_height()),
        (W, H),
        "{label}: visible size"
    );
    let (x0, x1) = (frame.crop_left, frame.width - frame.crop_right);
    let (y0, y1) = (frame.crop_top, frame.height - frame.crop_bottom);
    let max = (1u32 << frame.bit_depth) - 1;
    for (name, p) in [
        ("Y", &frame.y_plane),
        ("Cb", &frame.cb_plane),
        ("Cr", &frame.cr_plane),
    ] {
        let over = p.iter().filter(|&&s| u32::from(s) > max).count();
        assert_eq!(
            over, 0,
            "{label}: {over} {name} samples exceed {max} (bit depth {})",
            frame.bit_depth
        );
    }
    let exp_w = expected.w as usize;
    assert_plane_exact(
        label,
        "Y",
        &frame.y_plane,
        frame.y_stride(),
        &expected.y,
        exp_w,
        x0,
        x1,
        y0,
        y1,
    );
    // 4:2:2: half width, full height.
    assert_plane_exact(
        label,
        "Cb",
        &frame.cb_plane,
        frame.c_stride(),
        &expected.cb,
        exp_w / 2,
        x0 / 2,
        x1 / 2,
        y0,
        y1,
    );
    assert_plane_exact(
        label,
        "Cr",
        &frame.cr_plane,
        frame.c_stride(),
        &expected.cr,
        exp_w / 2,
        x0 / 2,
        x1 / 2,
        y0,
        y1,
    );
}

fn check_raw_lossless(depth: u8) {
    let stream = format!("hevc422/x265_422_{depth}bit_lossless.hevc");
    let source = load_i422(
        &fixture(&format!("hevc422/src_422_{depth}bit_80x56.yuv")),
        W,
        H,
        depth,
    );
    let frame = heic::hevc::decode(&fixture(&stream)).unwrap_or_else(|e| panic!("{stream}: {e}"));
    assert_eq!(frame.bit_depth, depth, "{stream}: bit depth");
    assert_frame_matches(&stream, &frame, &source);
}

fn check_heic_vs_libde265(name: &str, depth: u8) {
    let heic_rel = format!("features/{name}.heic");
    // dec265 writes the full coded picture (80x64); libheif's clap crops it to 80x56.
    let reference = load_i422(&fixture(&format!("hevc422/{name}.ref.yuv")), W, 64, depth);
    let frame = DecoderConfig::new()
        .decode_to_frame(&fixture(&heic_rel))
        .unwrap_or_else(|e| panic!("{heic_rel}: {e}"));
    assert_eq!(frame.bit_depth, depth, "{heic_rel}: bit depth");
    assert_frame_matches(&heic_rel, &frame, &reference);
}

#[test]
fn x265_422_8bit_lossless_raw_decodes_source_exact() {
    check_raw_lossless(8);
}

/// The #48 "returns Ok with chroma ~17x out of range" path.
#[test]
fn x265_422_10bit_lossless_raw_decodes_source_exact() {
    check_raw_lossless(10);
}

#[test]
fn heif_enc_422_8bit_matches_libde265() {
    check_heic_vs_libde265("yuv422", 8);
}

#[test]
fn heif_enc_422_10bit_matches_libde265() {
    check_heic_vs_libde265("yuv422_10bit", 10);
}

#[test]
fn image_info_reports_422() {
    for name in ["yuv422", "yuv422_10bit"] {
        let info = ImageInfo::from_bytes(&fixture(&format!("features/{name}.heic")))
            .unwrap_or_else(|e| panic!("{name}: probe failed: {e}"));
        assert_eq!((info.width, info.height), (W, H), "{name}: dimensions");
        assert_eq!(info.chroma_format, 2, "{name}: chroma_format");
    }
}

/// Coarse end-to-end RGB check on the synthetic content (colour conversion
/// goes through the generic 4:2:2 chroma fetch, not the 4:2:0 SIMD path).
#[test]
fn heic_422_rgb8_output_has_expected_colours() {
    let out = DecoderConfig::new()
        .decode(&fixture("features/yuv422.heic"), PixelLayout::Rgb8)
        .expect("yuv422.heic → Rgb8");
    assert_eq!((out.width, out.height), (W, H));
    let px = |x: u32, y: u32| {
        let i = ((y * W + x) * 3) as usize;
        (out.data[i], out.data[i + 1], out.data[i + 2])
    };
    // Saturated red square spans x 12..34, y 10..30 in the generator.
    let (r, g, b) = px(22, 20);
    assert!(
        r > 180 && g < 90 && b < 90,
        "red square pixel came out ({r},{g},{b})"
    );
    // Top-left of the base gradient is (0, 0, 1) → blue.
    let (r, g, b) = px(2, 2);
    assert!(
        b > 150 && r < 90 && g < 90,
        "top-left pixel came out ({r},{g},{b})"
    );
    // Bottom-right of the base gradient is (1, 1, 0) → yellow.
    let (r, g, b) = px(78, 54);
    assert!(
        r > 150 && g > 150 && b < 100,
        "bottom-right pixel came out ({r},{g},{b})"
    );
}
