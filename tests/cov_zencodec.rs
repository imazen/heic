//! Coverage + behavior tests for the zencodec adapter in `src/codec.rs`.
//!
//! These exercise the public zencodec trait surface (`HeicDecoderConfig`,
//! `HeicDecodeJob`, `HeicZenDecoder`, `HeicStreamDecoder`) against the
//! committed `testdata/` corpus, asserting REAL behavior:
//!
//! - format detection / probe + probe_full agree and reject non-HEIC bytes
//! - one-shot decode through the adapter matches the native decode pixel-exact
//! - streaming (`next_batch`) reassembles to the same image as one-shot
//! - `push_decoder` (the RowSinkAdapter path) reassembles to the same image
//! - `ResourceLimits` → native `Limits` conversion enforces tight limits
//! - malformed / truncated / crafted bytes return a clean `Err` (no panic)
//!
//! Requires the `zencodec` feature.

#![cfg(feature = "zencodec")]

use std::path::{Path, PathBuf};

use heic::{HeicDecoderConfig, HeicStreamDecoder, HeicZenDecoder};

use zencodec::decode::{
    Decode, DecodeJob, DecodeRowSink, DecoderConfig, StreamingDecode, negotiate_pixel_format,
};
use zencodec::{ImageFormat, ResourceLimits, ThreadingPolicy};
use zenpixels::{PixelDescriptor, PixelSliceMut};

use std::borrow::Cow;

// ── Fixtures ────────────────────────────────────────────────────────────────

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

/// The bundled 1280×854 grid image (6× 512×512 HEVC tiles, BT.709 limited).
const EXAMPLE_REL: &str = "libheif-examples/example.heic";
const EXAMPLE_W: u32 = 1280;
const EXAMPLE_H: u32 = 854;

/// A small synthetic single-image HEIC (non-grid path).
const SYNTH_REL: &str = "synthetic/synth_8bit_q95.heic";

// NOTE: the adapter's 16-bit decode branch (RGB16/RGBA16 via `to_rgb16`/
// `to_rgba16`) is NOT exercised here because the committed corpus has no
// 16-bit primary image that this build can decode: the `uncompressed_*_16*`
// fixtures need 16-bit unci component support (currently "only 8-bit unsigned
// integer components supported"), and the apple-hdr sample's *primary* image
// is 8-bit HEVC (the 10-bit data lives in the gain-map auxiliary). The 16-bit
// path is therefore a documented coverage gap pending a 10-bit HEVC primary
// fixture. The 16-bit descriptors are still asserted as *advertised* in
// `formats_and_descriptors_advertised`.

fn read_fixture(rel: &str) -> Vec<u8> {
    let path = testdata_dir().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| {
        panic!(
            "committed fixture {} must be present (testdata is checked in): {e}",
            path.display()
        )
    })
}

/// Native one-shot decode for cross-checking the adapter output.
fn native_decode(data: &[u8], layout: heic::PixelLayout) -> heic::DecodeOutput {
    heic::DecoderConfig::new()
        .decode_request(data)
        .with_output_layout(layout)
        .decode()
        .expect("native decode of committed fixture should succeed")
}

// ── A collecting DecodeRowSink for the push_decoder path ─────────────────────

/// Sink that records `begin()` dims and concatenates every strip into a single
/// tightly-packed RGB buffer, so we can compare against the one-shot decode.
struct CollectSink {
    begin_w: u32,
    begin_h: u32,
    begin_desc: Option<PixelDescriptor>,
    /// Full image buffer, sized at `begin()`.
    buf: Vec<u8>,
    width: u32,
    bpp: usize,
    began: bool,
    finished: bool,
    strips: u32,
}

impl CollectSink {
    fn new() -> Self {
        Self {
            begin_w: 0,
            begin_h: 0,
            begin_desc: None,
            buf: Vec::new(),
            width: 0,
            bpp: 0,
            began: false,
            finished: false,
            strips: 0,
        }
    }
}

impl DecodeRowSink for CollectSink {
    fn begin(
        &mut self,
        width: u32,
        height: u32,
        descriptor: PixelDescriptor,
    ) -> Result<(), zencodec::decode::SinkError> {
        self.begin_w = width;
        self.begin_h = height;
        self.begin_desc = Some(descriptor);
        self.width = width;
        self.bpp = descriptor.bytes_per_pixel();
        self.buf = vec![0u8; width as usize * height as usize * self.bpp];
        self.began = true;
        Ok(())
    }

    fn provide_next_buffer(
        &mut self,
        y: u32,
        height: u32,
        width: u32,
        descriptor: PixelDescriptor,
    ) -> Result<PixelSliceMut<'_>, zencodec::decode::SinkError> {
        self.strips += 1;
        let bpp = descriptor.bytes_per_pixel();
        let stride = width as usize * bpp;
        let start = y as usize * self.width as usize * self.bpp;
        let len = height as usize * stride;
        let end = start + len;
        let region = &mut self.buf[start..end];
        Ok(
            PixelSliceMut::new(region, width, height, stride, descriptor)
                .expect("strip region sized correctly"),
        )
    }

    fn finish(&mut self) -> Result<(), zencodec::decode::SinkError> {
        self.finished = true;
        Ok(())
    }
}

// ── Format detection / probe ────────────────────────────────────────────────

#[test]
fn formats_and_descriptors_advertised() {
    assert_eq!(
        <HeicDecoderConfig as DecoderConfig>::formats(),
        &[ImageFormat::Heic]
    );
    let descs = <HeicDecoderConfig as DecoderConfig>::supported_descriptors();
    assert!(descs.contains(&PixelDescriptor::RGB8_SRGB));
    assert!(descs.contains(&PixelDescriptor::RGBA8_SRGB));
    assert!(descs.contains(&PixelDescriptor::BGRA8_SRGB));
    // 16-bit native formats must be present (capabilities advertise native_16bit).
    assert!(descs.contains(&PixelDescriptor::RGB16_SRGB));
    assert!(descs.contains(&PixelDescriptor::RGBA16_SRGB));

    let caps = <HeicDecoderConfig as DecoderConfig>::capabilities();
    assert!(caps.streaming(), "adapter advertises streaming");
    assert!(caps.decode_into(), "adapter advertises decode_into / push");
    assert!(caps.gain_map(), "adapter advertises gain_map support");
}

#[test]
fn probe_detects_heic_and_reports_dims() {
    let data = read_fixture(EXAMPLE_REL);
    let job = HeicDecoderConfig::new().job();
    let info = job.probe(&data).expect("probe of example.heic");
    assert_eq!(info.format, ImageFormat::Heic);
    assert_eq!(info.width, EXAMPLE_W);
    assert_eq!(info.height, EXAMPLE_H);
    assert_eq!(info.frame_count(), Some(1));
}

#[test]
fn probe_rejects_non_heic_bytes() {
    // A plausible-looking but non-HEIC byte string must produce a clean Err.
    let job = HeicDecoderConfig::new().job();
    for junk in [
        &b"not a heic file at all, just ascii"[..],
        &b"\x89PNG\r\n\x1a\n"[..], // PNG signature
        &b"\xFF\xD8\xFF\xE0"[..],  // JPEG SOI
        &[][..],                   // empty
    ] {
        let result = job.probe(junk);
        assert!(
            result.is_err(),
            "probe must reject non-HEIC bytes: {junk:?}"
        );
    }
}

#[test]
fn probe_full_extracts_more_than_lightweight_probe() {
    // example.heic carries no EXIF/XMP, so use it to confirm dims agree and
    // that probe() stays lightweight (no metadata) while probe_full() runs the
    // full container parse path without diverging on geometry.
    let data = read_fixture(EXAMPLE_REL);
    let cfg = HeicDecoderConfig::new();
    let light = cfg.clone().job().probe(&data).expect("probe");
    let full = cfg.job().probe_full(&data).expect("probe_full");

    assert_eq!(light.width, full.width);
    assert_eq!(light.height, full.height);
    assert_eq!(light.format, full.format);
    assert_eq!(light.frame_count(), full.frame_count());
    // Lightweight probe never touches the container metadata extraction path.
    assert!(light.embedded_metadata.exif.is_none());
    assert!(light.embedded_metadata.xmp.is_none());
    assert!(light.source_color.icc_profile.is_none());
}

#[test]
fn output_info_matches_probe_dims_and_buffer_size() {
    let data = read_fixture(EXAMPLE_REL);
    let job = HeicDecoderConfig::new().job();
    let oi = job.output_info(&data).expect("output_info");
    assert_eq!(oi.width, EXAMPLE_W);
    assert_eq!(oi.height, EXAMPLE_H);
    // example.heic has no alpha → native format is an RGB (3-byte) descriptor.
    assert!(!oi.has_alpha);
    assert_eq!(oi.native_format.bytes_per_pixel(), 3);
    assert_eq!(
        oi.buffer_size(),
        u64::from(EXAMPLE_W) * u64::from(EXAMPLE_H) * 3
    );
    assert_eq!(
        oi.pixel_count(),
        u64::from(EXAMPLE_W) * u64::from(EXAMPLE_H)
    );
}

// ── One-shot decode through the adapter ─────────────────────────────────────

#[test]
fn adapter_decode_grid_matches_native_rgb() {
    let data = read_fixture(EXAMPLE_REL);
    let job = HeicDecoderConfig::new().job();
    let decoder: HeicZenDecoder<'_> = job
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder creation");
    let output = decoder.decode().expect("adapter decode");

    let info = output.info();
    assert_eq!(info.width, EXAMPLE_W);
    assert_eq!(info.height, EXAMPLE_H);

    let pixels = output.pixels();
    assert_eq!(pixels.width(), EXAMPLE_W);
    assert_eq!(pixels.rows(), EXAMPLE_H);
    assert_eq!(pixels.descriptor().bytes_per_pixel(), 3);
    // Non-degenerate: a real photo has variation, not a flat buffer.
    let first = pixels.row(0);
    assert!(
        first.iter().any(|&b| b != 0),
        "decoded row must be non-zero"
    );
    assert!(
        first.iter().any(|&b| b != first[0]),
        "decoded row must not be a single flat value"
    );

    // Cross-check against the native decode: the adapter must produce the
    // SAME pixels (it wraps the same decoder). Compare row-by-row because the
    // PixelBuffer may carry stride padding.
    let native = native_decode(&data, heic::PixelLayout::Rgb8);
    assert_eq!(native.width, EXAMPLE_W);
    assert_eq!(native.height, EXAMPLE_H);
    let row_bytes = EXAMPLE_W as usize * 3;
    for y in 0..EXAMPLE_H {
        let nat = &native.data[y as usize * row_bytes..(y as usize + 1) * row_bytes];
        assert_eq!(pixels.row(y), nat, "adapter vs native mismatch on row {y}");
    }
}

#[test]
fn adapter_decode_rgba_negotiation_adds_opaque_alpha() {
    // Prefer RGBA8: the negotiator must honor it; the (alpha-less) grid image
    // gets a synthesized opaque alpha plane.
    let data = read_fixture(EXAMPLE_REL);
    let job = HeicDecoderConfig::new().job();
    let decoder = job
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGBA8_SRGB])
        .expect("decoder creation");
    let output = decoder.decode().expect("adapter rgba decode");
    let pixels = output.pixels();
    assert_eq!(pixels.width(), EXAMPLE_W);
    assert_eq!(pixels.rows(), EXAMPLE_H);
    assert_eq!(pixels.descriptor().bytes_per_pixel(), 4);
    // Every 4th byte (alpha) should be fully opaque for a no-alpha source.
    let row = pixels.row(10);
    assert!(
        row.chunks_exact(4).all(|px| px[3] == 255),
        "synthesized alpha must be opaque (255)"
    );
}

#[test]
fn adapter_decode_synthetic_single_image() {
    // The synthetic file is a plain single-image HEIC (non-grid path through
    // the adapter). Confirm it decodes to a sane, non-degenerate image whose
    // dims agree with probe.
    let data = read_fixture(SYNTH_REL);
    let job = HeicDecoderConfig::new().job();
    let probe = job.probe(&data).expect("probe synthetic");
    let (pw, ph) = (probe.width, probe.height);
    assert!(pw > 0 && ph > 0);

    let decoder = HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder");
    let output = decoder.decode().expect("decode synthetic");
    assert_eq!(output.info().width, pw);
    assert_eq!(output.info().height, ph);
    let pixels = output.pixels();
    assert!(pixels.row(0).iter().any(|&b| b != 0));
}

// ── Streaming decode (HeicStreamDecoder / next_batch) ───────────────────────

#[test]
fn streaming_grid_reassembles_to_one_shot() {
    let data = read_fixture(EXAMPLE_REL);

    // One-shot reference.
    let native = native_decode(&data, heic::PixelLayout::Rgb8);
    let row_bytes = EXAMPLE_W as usize * 3;
    assert_eq!(native.data.len(), row_bytes * EXAMPLE_H as usize);

    // Streaming: assemble strips into a full-frame buffer.
    let mut stream: HeicStreamDecoder = HeicDecoderConfig::new()
        .job()
        .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("streaming decoder");

    // The trait info() must report the full image geometry.
    assert_eq!(stream.info().width, EXAMPLE_W);
    assert_eq!(stream.info().height, EXAMPLE_H);

    let mut assembled = vec![0u8; row_bytes * EXAMPLE_H as usize];
    let mut max_y_seen = 0u32;
    let mut batch_count = 0u32;
    let mut total_rows = 0u32;
    while let Some((y, slice)) = stream.next_batch().expect("next_batch") {
        batch_count += 1;
        assert_eq!(
            slice.width(),
            EXAMPLE_W,
            "strip width must equal image width"
        );
        assert_eq!(slice.descriptor().bytes_per_pixel(), 3);
        let h = slice.rows();
        assert!(h > 0, "a strip must contain at least one row");
        assert!(y >= max_y_seen, "strips must arrive top-to-bottom");
        max_y_seen = y;
        for r in 0..h {
            let dst = ((y + r) as usize) * row_bytes;
            assembled[dst..dst + row_bytes].copy_from_slice(&slice.row(r)[..row_bytes]);
        }
        total_rows += h;
    }
    assert!(
        batch_count >= 2,
        "a 6-tile grid should stream as multiple strips, got {batch_count}"
    );
    assert_eq!(
        total_rows, EXAMPLE_H,
        "all rows must be emitted exactly once"
    );

    // The streamed grid path and the one-shot decode must agree pixel-exact.
    assert_eq!(
        assembled, native.data,
        "streamed grid reassembly must equal one-shot decode"
    );
}

#[test]
fn streaming_nongrid_fallback_emits_full_image() {
    // The synthetic single-image file takes the non-grid streaming fallback
    // (full decode upfront, emitted in fixed-height strips).
    let data = read_fixture(SYNTH_REL);
    let probe = HeicDecoderConfig::new().job().probe(&data).expect("probe");
    let (w, h) = (probe.width, probe.height);

    let mut stream = HeicDecoderConfig::new()
        .job()
        .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("streaming decoder");
    assert_eq!(stream.info().width, w);
    assert_eq!(stream.info().height, h);

    let mut rows_seen = 0u32;
    let mut prev_end = 0u32;
    while let Some((y, slice)) = stream.next_batch().expect("next_batch") {
        assert_eq!(slice.width(), w);
        assert_eq!(y, prev_end, "non-grid strips must be contiguous");
        prev_end = y + slice.rows();
        rows_seen += slice.rows();
        // Sanity: each row carries the advertised number of bytes.
        assert!(slice.row(0).len() >= w as usize * slice.descriptor().bytes_per_pixel());
    }
    assert_eq!(rows_seen, h, "non-grid fallback must emit every row once");
}

// ── push_decoder (RowSinkAdapter path) ──────────────────────────────────────

#[test]
fn push_decoder_grid_reassembles_to_one_shot() {
    let data = read_fixture(EXAMPLE_REL);
    let native = native_decode(&data, heic::PixelLayout::Rgb8);
    let row_bytes = EXAMPLE_W as usize * 3;

    let mut sink = CollectSink::new();
    let out_info = HeicDecoderConfig::new()
        .job()
        .push_decoder(
            Cow::Borrowed(&data),
            &mut sink,
            &[PixelDescriptor::RGB8_SRGB],
        )
        .expect("push_decoder");

    assert_eq!(out_info.width, EXAMPLE_W);
    assert_eq!(out_info.height, EXAMPLE_H);
    assert!(sink.began, "sink.begin() must be called");
    assert!(sink.finished, "sink.finish() must be called");
    assert_eq!(sink.begin_w, EXAMPLE_W);
    assert_eq!(sink.begin_h, EXAMPLE_H);
    assert!(sink.strips >= 1, "at least one strip should be provided");
    assert_eq!(sink.begin_desc.unwrap().bytes_per_pixel(), 3);

    // Compare the collected buffer row-by-row against the one-shot decode.
    for y in 0..EXAMPLE_H {
        let s = y as usize * row_bytes;
        let collected = &sink.buf[s..s + row_bytes];
        let nat = &native.data[s..s + row_bytes];
        assert_eq!(collected, nat, "push_decoder vs native mismatch on row {y}");
    }
}

// ── ResourceLimits → native Limits ──────────────────────────────────────────

#[test]
fn tight_max_input_bytes_rejected_at_decoder() {
    let data = read_fixture(EXAMPLE_REL);
    let limits = ResourceLimits::none().with_max_input_bytes(64);
    let job = HeicDecoderConfig::new().job().with_limits(limits);
    // HeicZenDecoder has no Debug impl, so match instead of expect_err.
    match job.decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB]) {
        Ok(_) => panic!("64-byte input cap must reject a multi-KB file"),
        Err(err) => assert!(
            matches!(err.error(), heic::HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        ),
    }
}

#[test]
fn tight_max_pixels_propagates_to_native_decode() {
    // max_pixels below the image area must surface as an error from the wrapped
    // native decoder, proving ResourceLimits → crate::Limits conversion runs.
    let data = read_fixture(EXAMPLE_REL);
    let limits = ResourceLimits::none().with_max_pixels(1024); // far below 1280*854
    let decoder = HeicDecoderConfig::new()
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder construction passes the input-size gate");
    let result = decoder.decode();
    assert!(
        result.is_err(),
        "max_pixels=1024 must reject a {EXAMPLE_W}x{EXAMPLE_H} image"
    );
}

#[test]
fn tight_dimension_limit_rejected_by_streaming_grid_setup() {
    // The grid streaming setup enforces the dimension ceiling. A max_width
    // below the image width must be rejected.
    let data = read_fixture(EXAMPLE_REL);
    let limits = ResourceLimits::none().with_max_width(16);
    let result = HeicDecoderConfig::new()
        .job()
        .with_limits(limits)
        .streaming_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB]);
    assert!(
        result.is_err(),
        "max_width=16 must reject the 1280-wide grid in streaming setup"
    );
}

#[test]
fn generous_limits_allow_decode() {
    // A limit set strictly above the image must NOT block decode — confirms the
    // conversion isn't spuriously rejecting valid inputs.
    let data = read_fixture(EXAMPLE_REL);
    let limits = ResourceLimits::none()
        .with_max_input_bytes(data.len() as u64 + 1)
        .with_max_pixels(u64::from(EXAMPLE_W) * u64::from(EXAMPLE_H))
        .with_max_width(EXAMPLE_W)
        .with_max_height(EXAMPLE_H);
    let decoder = HeicDecoderConfig::new()
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("generous limits should permit the decoder");
    let output = decoder.decode().expect("generous limits decode");
    assert_eq!(output.info().width, EXAMPLE_W);
    assert_eq!(output.info().height, EXAMPLE_H);
}

#[test]
fn sequential_threading_decodes_same_pixels_as_default() {
    // ThreadingPolicy::Sequential drives policy_to_threads → 1, forcing the
    // single-thread native path. Output must be identical to the default decode.
    let data = read_fixture(EXAMPLE_REL);
    let native = native_decode(&data, heic::PixelLayout::Rgb8);
    let row_bytes = EXAMPLE_W as usize * 3;

    let limits = ResourceLimits::none().with_threading(ThreadingPolicy::Sequential);
    let decoder = HeicDecoderConfig::new()
        .job()
        .with_limits(limits)
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("sequential decoder");
    let output = decoder.decode().expect("sequential decode");
    let pixels = output.pixels();
    for y in 0..EXAMPLE_H {
        let nat = &native.data[y as usize * row_bytes..(y as usize + 1) * row_bytes];
        assert_eq!(pixels.row(y), nat, "sequential decode diverged on row {y}");
    }
}

// ── Negotiation helper (re-exported zencodec fn, exercised via adapter descs) ─

#[test]
fn negotiation_honors_bgra_preference_for_grid() {
    // Decoding the grid with a BGRA preference must produce a 4-byte BGRA
    // buffer whose RGB channels are the byte-swap of the RGB decode.
    let data = read_fixture(EXAMPLE_REL);
    let rgb = native_decode(&data, heic::PixelLayout::Rgb8);

    let decoder = HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::BGRA8_SRGB])
        .expect("decoder");
    let output = decoder.decode().expect("bgra decode");
    let pixels = output.pixels();
    assert_eq!(pixels.descriptor().bytes_per_pixel(), 4);

    // Spot-check: BGRA pixel 0 must mirror RGB pixel 0 with B/R swapped and
    // alpha opaque.
    let rgb_row0 = &rgb.data[0..3];
    let bgra_row0 = pixels.row(0);
    assert_eq!(bgra_row0[0], rgb_row0[2], "B == R(rgb)");
    assert_eq!(bgra_row0[1], rgb_row0[1], "G == G");
    assert_eq!(bgra_row0[2], rgb_row0[0], "R == B(rgb)");
    assert_eq!(bgra_row0[3], 255, "alpha opaque");

    // Sanity: the re-exported negotiator returns BGRA when the source has no
    // alpha and BGRA is preferred and available.
    let chosen = negotiate_pixel_format(
        &[PixelDescriptor::BGRA8_SRGB],
        &[
            PixelDescriptor::RGB8_SRGB,
            PixelDescriptor::RGBA8_SRGB,
            PixelDescriptor::BGRA8_SRGB,
        ],
    );
    assert_eq!(chosen, Some(PixelDescriptor::BGRA8_SRGB));
}

// ── Malformed / truncated / crafted input → clean Err, never panic ──────────

#[test]
fn truncated_example_returns_err_not_panic() {
    let full = read_fixture(EXAMPLE_REL);
    // Prefixes too small to hold example.heic's container header (which spans
    // ~1 KB) can't yield a primary image → must be a clean Err on probe. (A
    // partial-but-large truncation may legitimately still parse the header and
    // decode the early grid tiles, so we only require *small* prefixes to fail;
    // all prefixes must avoid panicking.)
    let small_cuts = [0usize, 4, 16, 64, 256, 512, 768];
    for &cut in &small_cuts {
        let cut = cut.min(full.len());
        let data = &full[..cut];
        let job = HeicDecoderConfig::new().job();
        assert!(
            job.probe(data).is_err(),
            "probe of a {cut}-byte prefix must fail cleanly"
        );
        match HeicDecoderConfig::new()
            .job()
            .decoder(Cow::Borrowed(data), &[PixelDescriptor::RGB8_SRGB])
        {
            Ok(dec) => assert!(
                dec.decode().is_err(),
                "{cut}-byte prefix must not decode successfully"
            ),
            Err(_) => { /* clean rejection at construction */ }
        }
    }

    // Larger truncations: the only guarantee is no panic / no uncapped alloc.
    let limits = ResourceLimits::none().with_max_pixels(8_000_000);
    for frac in [3usize, 2, 4, 8] {
        let cut = full.len() * (frac - 1) / frac;
        let data = &full[..cut];
        let _ = HeicDecoderConfig::new().job().probe(data);
        if let Ok(dec) = HeicDecoderConfig::new()
            .job()
            .with_limits(limits)
            .decoder(Cow::Borrowed(data), &[PixelDescriptor::RGB8_SRGB])
        {
            // Result is don't-care; the point is it must return, not panic.
            let _ = dec.decode();
        }
    }
}

#[test]
fn fuzz_regression_seeds_never_panic_through_adapter() {
    // The fuzz/regression crash seeds are crafted malformed inputs. Run each one
    // through probe + decoder + streaming setup; they must all return Err (or a
    // valid-but-bounded result) without panicking or OOMing.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fuzz/regression");
    let Ok(rd) = std::fs::read_dir(&dir) else {
        eprintln!(
            "fuzz/regression not present at {} — skipping crafted-seed gate",
            dir.display()
        );
        return;
    };
    let mut count = 0u32;
    // Cap input via limits so a malformed-but-large descriptor can't blow memory.
    let limits = ResourceLimits::none()
        .with_max_pixels(8_000_000)
        .with_max_memory(256 * 1024 * 1024);
    for entry in rd.flatten() {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let Ok(data) = std::fs::read(&p) else {
            continue;
        };
        count += 1;

        let job = HeicDecoderConfig::new().job().with_limits(limits);
        // probe must not panic
        let _ = job.probe(&data);

        // decoder construction + decode must not panic
        if let Ok(dec) = HeicDecoderConfig::new()
            .job()
            .with_limits(limits)
            .decoder(Cow::Owned(data.clone()), &[PixelDescriptor::RGB8_SRGB])
        {
            let _ = dec.decode();
        }

        // streaming setup must not panic
        let _ = HeicDecoderConfig::new()
            .job()
            .with_limits(limits)
            .streaming_decoder(Cow::Owned(data), &[PixelDescriptor::RGB8_SRGB]);
    }
    assert!(count > 0, "expected at least one fuzz/regression seed");
    eprintln!("ran {count} crafted seeds through the adapter without panic");
}

#[test]
fn crafted_grid_descriptor_garbage_rejected() {
    // A ftyp+meta-shaped HEIC header is too small to parse as a real grid.
    // Feed a short, malformed buffer that passes the brand sniff but cannot
    // yield a primary image: the adapter must reject it, never panic.
    //
    // 'ftyp' box claiming 'heic' brand, then nothing usable.
    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&[0, 0, 0, 0x14]); // box size 20
    data.extend_from_slice(b"ftyp");
    data.extend_from_slice(b"heic"); // major brand
    data.extend_from_slice(&[0, 0, 0, 0]); // minor version
    data.extend_from_slice(b"heic"); // compatible brand
    // No 'meta' box at all.

    let job = HeicDecoderConfig::new().job();
    // Probe must error (no primary image / corrupt), not panic.
    assert!(
        job.probe(&data).is_err(),
        "headerless grid must be rejected"
    );

    // Decoder path likewise.
    match HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
    {
        Ok(dec) => assert!(dec.decode().is_err()),
        Err(_) => { /* rejected at construction — also fine */ }
    }
}

#[test]
fn animation_unsupported() {
    // HEIC has no animation; the adapter must report Unsupported, not panic.
    let data = read_fixture(EXAMPLE_REL);
    let result = HeicDecoderConfig::new()
        .job()
        .animation_frame_decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB]);
    assert!(result.is_err(), "animation must be unsupported");
    let err = result.err().unwrap();
    assert!(
        matches!(err.error(), heic::HeicError::Unsupported(_)),
        "expected Unsupported, got {err:?}"
    );
}

// ── Orientation hint: Preserve (default) vs Correct ─────────────────────────
//
// The zencodec adapter honors `OrientationHint` the same way zenjpeg does, so
// the two codecs report orientation consistently:
//   - Preserve (default): pixels stay in stored orientation; `ImageInfo` reports
//     the stored (coded) dims + the intrinsic `Orientation` tag.
//   - Correct: the decoder bakes the image upright; `ImageInfo` reports the
//     display dims + `Orientation::Identity`.
// Either way `display_width()`/`display_height()` yield the upright dims.
//
// `irot90.heic` is a single HEVC item with an `irot` 90° rotation: stored
// (coded) dims 64×40, display dims 40×64.

const IROT90_REL: &str = "features/irot90.heic";
const IROT90_STORED: (u32, u32) = (64, 40);
const IROT90_DISPLAY: (u32, u32) = (40, 64);

#[test]
fn orientation_preserve_default_reports_stored_dims_and_tag() {
    let data = read_fixture(IROT90_REL);
    // Default config == OrientationHint::Preserve.
    let info = HeicDecoderConfig::new().job().probe(&data).expect("probe");
    assert_eq!(
        (info.width, info.height),
        IROT90_STORED,
        "Preserve must report stored (coded, pre-rotation) dims"
    );
    assert!(
        info.orientation.swaps_axes() && !info.orientation.is_identity(),
        "Preserve must report the intrinsic 90/270 orientation tag, got {:?}",
        info.orientation
    );
    assert_eq!(
        (info.display_width(), info.display_height()),
        IROT90_DISPLAY,
        "display_width/height must yield the upright dims under Preserve"
    );
    // probe_full must agree with the lightweight probe.
    let full = HeicDecoderConfig::new()
        .job()
        .probe_full(&data)
        .expect("probe_full");
    assert_eq!((full.width, full.height), IROT90_STORED);
    assert_eq!(full.orientation, info.orientation);
}

#[test]
fn orientation_correct_reports_display_dims_and_identity() {
    let data = read_fixture(IROT90_REL);
    let info = HeicDecoderConfig::new()
        .with_orientation(zencodec::OrientationHint::Correct)
        .job()
        .probe(&data)
        .expect("probe");
    assert_eq!(
        (info.width, info.height),
        IROT90_DISPLAY,
        "Correct must report display (post-rotation) dims"
    );
    assert_eq!(
        info.orientation,
        zenpixels::Orientation::Identity,
        "Correct must report Identity — orientation is baked into the pixels"
    );
    assert_eq!(
        (info.display_width(), info.display_height()),
        IROT90_DISPLAY,
    );
}

#[test]
fn orientation_decode_dims_match_probe_for_both_hints() {
    let data = read_fixture(IROT90_REL);

    // Preserve (default): decoded pixels stay in stored orientation, and the
    // output ImageInfo dims match the pixels.
    let preserve = HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder")
        .decode()
        .expect("decode");
    assert_eq!(
        (preserve.width(), preserve.height()),
        IROT90_STORED,
        "Preserve decode must output stored-orientation pixels"
    );
    assert_eq!(
        (preserve.info().width, preserve.info().height),
        IROT90_STORED,
        "Preserve decode ImageInfo dims must match the decoded pixels"
    );
    assert!(
        preserve.info().orientation.swaps_axes(),
        "Preserve decode must tag the intrinsic orientation"
    );
    assert_eq!(
        (
            preserve.info().display_width(),
            preserve.info().display_height()
        ),
        IROT90_DISPLAY,
    );

    // Correct: decoded pixels are baked upright.
    let correct = HeicDecoderConfig::new()
        .with_orientation(zencodec::OrientationHint::Correct)
        .job()
        .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
        .expect("decoder")
        .decode()
        .expect("decode");
    assert_eq!(
        (correct.width(), correct.height()),
        IROT90_DISPLAY,
        "Correct decode must output display-orientation (upright) pixels"
    );
    assert_eq!(correct.info().orientation, zenpixels::Orientation::Identity,);
}

#[test]
fn orientation_mirror_preserve_reports_flip_tag_without_swapping_dims() {
    // `imir_h.heic` is a left↔right mirror (no axis swap): stored dims == display
    // dims, but Preserve must still report a non-identity flip orientation.
    let data = read_fixture("features/imir_h.heic");
    let info = HeicDecoderConfig::new().job().probe(&data).expect("probe");
    assert!(
        !info.orientation.is_identity() && !info.orientation.swaps_axes(),
        "imir Preserve must report a pure flip (non-identity, non-swapping), got {:?}",
        info.orientation
    );
    // A mirror does not swap dims, so display dims equal stored dims.
    assert_eq!(info.display_width(), info.width);
    assert_eq!(info.display_height(), info.height);
}

// ── GainMapRender modes (Apple HDR gain map) ────────────────────────────────

/// Default (`BaseOnly`): no gain-map extras of either kind.
#[test]
fn gain_map_render_base_only_attaches_nothing() {
    let data = read_fixture("apple-hdr/hdr-sample.heic");
    let out = HeicDecoderConfig::new()
        .job()
        .decoder(Cow::Borrowed(&data), &[])
        .expect("decoder")
        .decode()
        .expect("decode");
    assert!(out.extras::<zencodec::decode::DecodedGainMap>().is_none());
    assert!(out.extras::<heic::HdrGainMap>().is_none());
}

/// `Components` surfaces the decoded gain map both as the canonical
/// `zencodec::decode::DecodedGainMap` (gray8 pixels + ISO 21496-1 params from
/// the gain-map item's XMP) and as the native `HdrGainMap`.
#[test]
fn gain_map_render_components_surfaces_decoded_gain_map() {
    let data = read_fixture("apple-hdr/hdr-sample.heic");
    let out = HeicDecoderConfig::new()
        .job()
        .with_gain_map_render(zencodec::GainMapRender::Components)
        .decoder(Cow::Borrowed(&data), &[])
        .expect("decoder")
        .decode()
        .expect("decode");
    let dgm = out
        .extras::<zencodec::decode::DecodedGainMap>()
        .expect("Components must surface the DecodedGainMap");
    assert!(dgm.pixels.width() > 0 && dgm.pixels.height() > 0);
    assert_eq!(dgm.metadata.channels, 1, "Apple gain maps are luma-only");
    assert!(out.extras::<heic::HdrGainMap>().is_some());
}

/// heic applies gain maps natively (`reconstructs_hdr()` is true):
/// `ReconstructHdr` returns linear f32 HDR pixels brighter than SDR white,
/// with the content-light-level / mastering-display envelope populated.
#[test]
fn gain_map_render_reconstruct_applies_gain_map() {
    assert!(<HeicDecoderConfig as DecoderConfig>::capabilities().reconstructs_hdr());
    let data = read_fixture("apple-hdr/hdr-sample.heic");
    let out = HeicDecoderConfig::new()
        .job()
        .with_gain_map_render(zencodec::GainMapRender::ReconstructHdr {
            target_headroom: None,
        })
        .decoder(Cow::Borrowed(&data), &[])
        .expect("decoder")
        .decode()
        .expect("decode");

    let desc = out.pixels().descriptor();
    assert_eq!(desc.channel_type(), zenpixels::ChannelType::F32);
    assert_eq!(desc.transfer(), zenpixels::TransferFunction::Linear);

    // The HDR rendition must actually exceed SDR white (1.0 in linear)
    // somewhere — this fixture has highlights with real headroom.
    let max = out
        .pixels()
        .contiguous_bytes()
        .chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .fold(0.0f32, f32::max);
    assert!(
        max > 1.0,
        "expected HDR headroom above SDR white, max={max}"
    );

    let info = out.info();
    let cll = info
        .source_color
        .content_light_level
        .expect("ReconstructHdr must populate the content light level");
    assert!(cll.max_content_light_level > 203);
    assert!(info.source_color.mastering_display.is_some());

    // Reconstruction consumed the gain map; no Components extras attach.
    assert!(out.extras::<zencodec::decode::DecodedGainMap>().is_none());
}

/// `ReconstructHdr { target_headroom: Some(1.0) }` clamps the boost to
/// SDR — output is linear but stays at (or barely above) SDR white.
#[test]
fn gain_map_render_reconstruct_honors_target_headroom() {
    let data = read_fixture("apple-hdr/hdr-sample.heic");
    let out = HeicDecoderConfig::new()
        .job()
        .with_gain_map_render(zencodec::GainMapRender::ReconstructHdr {
            target_headroom: Some(1.0),
        })
        .decoder(Cow::Borrowed(&data), &[])
        .expect("decoder")
        .decode()
        .expect("decode");
    let max = out
        .pixels()
        .contiguous_bytes()
        .chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .fold(0.0f32, f32::max);
    assert!(
        max <= 1.01,
        "boost clamped to 1.0 must not exceed SDR white, max={max}"
    );
}
