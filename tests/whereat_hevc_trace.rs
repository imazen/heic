//! #25: an HEVC decode failure reaching the public `At<HeicError>` carries the
//! line inside `src/hevc/` that detected it, followed by the module boundary,
//! instead of a trace that starts at the boundary.
//!
//! The fixtures are `testdata/features/single.heic` with bytes corrupted in
//! place: the SPS payload inside `hvcC` (fails in the parameter-set parser)
//! and the slice data inside `mdat` (fails in the CTU / CABAC decode or the
//! decode-completeness guard). Offsets were read from the file's box layout
//! (`hvcC` at 176, SPS NAL payload at 236 for 42 bytes, `mdat` payload at 416
//! for 109 bytes) and are re-derived here from the box tags so a regenerated
//! fixture still works.

use heic::{DecoderConfig, HeicError, PixelLayout};
use std::path::PathBuf;

fn single_heic() -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("testdata/features/single.heic");
    std::fs::read(&p).unwrap_or_else(|e| panic!("missing committed fixture {p:?}: {e}"))
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{needle:?} not found"))
}

/// Byte range of the first SPS NAL payload inside `hvcC`.
fn sps_payload_range(data: &[u8]) -> std::ops::Range<usize> {
    let cfg = find(data, b"hvcC") + 4; // HEVCDecoderConfigurationRecord
    let num_arrays = data[cfg + 22] as usize;
    let mut q = cfg + 23;
    for _ in 0..num_arrays {
        let nal_type = data[q] & 0x3f;
        let count = u16::from_be_bytes([data[q + 1], data[q + 2]]) as usize;
        q += 3;
        for _ in 0..count {
            let len = u16::from_be_bytes([data[q], data[q + 1]]) as usize;
            if nal_type == 33 {
                // skip the 2-byte NAL header so the corruption lands in the RBSP
                return q + 2 + 2..q + 2 + len;
            }
            q += 2 + len;
        }
    }
    panic!("no SPS in hvcC");
}

fn files(err: &heic::At<HeicError>) -> Vec<String> {
    err.frames()
        .filter_map(|f| f.location())
        .map(|l| l.file().replace('\\', "/"))
        .collect()
}

fn assert_origin_inside_hevc(label: &str, data: &[u8]) {
    let err = match DecoderConfig::new().decode(data, PixelLayout::Rgb8) {
        Err(e) => e,
        Ok(out) => panic!(
            "{label}: corrupted file decoded Ok ({}x{})",
            out.width, out.height
        ),
    };
    assert!(
        matches!(err.error(), HeicError::HevcDecode(_)),
        "{label}: expected an HEVC decode error, got {err}"
    );
    let files = files(&err);
    assert!(
        !files.is_empty(),
        "{label}: error carries no located frame: {err:?}"
    );
    assert!(
        files[0].contains("src/hevc/"),
        "{label}: first frame should be the decoder origin, got {files:?}"
    );
    // The boundary frame is the `.map_err(hevc_at).at()` in the container /
    // backend code — not `core/src/ops/function.rs`, which is what a
    // `#[track_caller]` helper passed by name to `map_err` would record.
    assert!(
        files
            .iter()
            .any(|f| f.ends_with("src/backend.rs") || f.ends_with("src/decode.rs")),
        "{label}: trace should also record the module boundary, got {files:?}"
    );
}

#[test]
fn corrupted_sps_trace_starts_in_the_parameter_set_parser() {
    let mut data = single_heic();
    let range = sps_payload_range(&data);
    for b in &mut data[range] {
        *b = 0xff;
    }
    assert_origin_inside_hevc("sps", &data);
}

/// The raw HEVC entry points return `At<HevcError>` directly: the trace
/// starts in the parameter-set parser, with no location-less hop at the
/// module boundary.
#[test]
fn public_hevc_get_info_trace_starts_in_the_parameter_set_parser() {
    // Annex B start code + SPS NAL header (type 33) + garbage payload.
    let mut data = vec![0, 0, 0, 1, 0x42, 0x01];
    data.extend_from_slice(&[0xff; 40]);
    let err = heic::hevc::get_info(&data).expect_err("garbage SPS must not parse");
    let files: Vec<String> = err
        .frames()
        .filter_map(|f| f.location())
        .map(|l| l.file().replace('\\', "/"))
        .collect();
    assert!(
        files.first().is_some_and(|f| f.contains("src/hevc/")),
        "hevc::get_info origin should be inside the decoder, got {files:?}"
    );
}

/// `ImageInfo::from_bytes` returns `At<ProbeError>`: a `Corrupt` probe keeps
/// the container parser's origin frame and records the probe boundary.
#[test]
fn corrupt_probe_trace_starts_in_the_container_parser() {
    // A well-framed `ftyp` box with a non-HEIF brand ("qt  "): passes the
    // 12-byte / `ftyp` format gate, then the container parser's brand check
    // (`src/heif/parser.rs`) rejects it.
    let mut data = Vec::new();
    data.extend_from_slice(&20u32.to_be_bytes());
    data.extend_from_slice(b"ftyp");
    data.extend_from_slice(b"qt  ");
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(b"qt  ");
    let err = heic::ImageInfo::from_bytes(&data).expect_err("foreign brand must not probe");
    assert!(
        matches!(err.error(), heic::ProbeError::Corrupt(_)),
        "expected ProbeError::Corrupt, got {err}"
    );
    let files: Vec<String> = err
        .frames()
        .filter_map(|f| f.location())
        .map(|l| l.file().replace('\\', "/"))
        .collect();
    assert!(
        files.first().is_some_and(|f| f.contains("src/heif/")),
        "probe origin should be inside the container parser, got {files:?}"
    );
    assert!(
        files.iter().any(|f| f.ends_with("src/lib.rs")),
        "probe trace should record the ImageInfo::from_bytes boundary, got {files:?}"
    );
}

#[test]
fn corrupted_slice_data_trace_starts_in_the_decoder() {
    let mut data = single_heic();
    let mdat = find(&data, b"mdat") + 4;
    // Keep the length prefix + NAL header + slice header start intact, then
    // zero the rest of the slice data so CABAC runs off the rails.
    for b in &mut data[mdat + 16..] {
        *b = 0;
    }
    assert_origin_inside_hevc("slice", &data);
}
