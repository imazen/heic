//! Coverage + robustness tests for the HEIF container parser.
//!
//! Target: `src/heif/parser.rs` and `src/heif/boxes.rs`, driven through the
//! public API (`ImageInfo::from_bytes`, `DecoderConfig::decode`, `extract_*`)
//! since the `heif` module is crate-internal.
//!
//! Two thrusts:
//!  1. ROBUSTNESS — every committed fuzz/regression seed plus a battery of
//!     hand-crafted malformed ISOBMFF byte strings must produce a clean `Err`
//!     (or `Ok`) and NEVER panic / hang. Where a specific error variant is
//!     reachable we assert on it, not just "is_err()".
//!  2. VALID SWEEP — every committed `.heic`/`.heif` is fed through the probe
//!     path; we assert a meaningful number parse and that the canonical
//!     reference file probes to its known dimensions (so the sweep harness is
//!     itself exercised against a known-good input, per the project's
//!     "test the test infrastructure" rule).
//!
//! These tests don't relax any expectation: a malformed input that the parser
//! is documented to reject is asserted to be rejected with the right variant.

use heic::{DecoderConfig, ImageInfo, PixelLayout, ProbeError};
use std::path::{Path, PathBuf};

fn manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn testdata_dir() -> PathBuf {
    manifest_dir().join("testdata")
}

/// Recursively collect files under `dir` whose name passes `keep`.
fn collect(dir: &Path, keep: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, keep, out);
        } else if keep(&p) {
            out.push(p);
        }
    }
}

// ---------------------------------------------------------------------------
// 1. Fuzz / regression seeds — must never panic through the public API.
// ---------------------------------------------------------------------------

/// Every committed crash/oom seed under `fuzz/regression/` (recursively,
/// including the `fuzz_hevc_raw/` subdir of raw HEVC NAL streams) is fed
/// through both the probe and full-decode entry points. The decoder is
/// allowed to return any `Result` — the only requirement is that it does not
/// panic, abort, or hang. These are known crashing inputs from the fuzzer;
/// the contract is "clean rejection, no UB".
#[test]
fn fuzz_regression_seeds_never_panic() {
    let reg = manifest_dir().join("fuzz").join("regression");
    let mut seeds = Vec::new();
    collect(
        &reg,
        &|p| {
            // Skip directory-marker / non-seed files; every committed seed is
            // a plain binary blob (crash-*, oom-*). Take them all.
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("crash-") || n.starts_with("oom-"))
        },
        &mut seeds,
    );
    seeds.sort();

    assert!(
        seeds.len() >= 15,
        "expected the committed fuzz/regression corpus (>=15 seeds), found {} under {} \
         — a thin checkout would silently weaken this gate",
        seeds.len(),
        reg.display()
    );

    let config = DecoderConfig::new();
    for seed in &seeds {
        let data = std::fs::read(seed)
            .unwrap_or_else(|e| panic!("failed to read committed seed {}: {e}", seed.display()));

        // Probe path: parser-only. Any Result is fine; no panic.
        let probe = ImageInfo::from_bytes(&data);
        // Full decode path: container parse + HEVC. Any Result is fine.
        let decode = config.decode(&data, PixelLayout::Rgba8);
        // Metadata extraction paths also walk the container.
        let _ = config.extract_exif(&data);
        let _ = config.extract_xmp(&data);
        let _ = config.extract_icc(&data);

        // If a seed *does* probe successfully, a probed size must be
        // self-consistent (non-zero) — a probe returning Ok with a zero
        // dimension would be a silent corruption bug.
        if let Ok(info) = probe {
            assert!(
                info.width > 0 && info.height > 0,
                "seed {} probed Ok but reported degenerate {}x{}",
                seed.display(),
                info.width,
                info.height
            );
        }

        // If decode succeeds, the buffer length must match the reported
        // geometry exactly (4 bytes/px for Rgba8). A short/long buffer would
        // be an out-of-bounds hazard for callers.
        if let Ok(out) = decode {
            assert_eq!(out.layout, PixelLayout::Rgba8);
            assert_eq!(
                out.data.len(),
                out.width as usize * out.height as usize * 4,
                "seed {} decoded with inconsistent buffer length",
                seed.display()
            );
        }
    }
    eprintln!(
        "fuzz_regression_seeds_never_panic: swept {} seeds",
        seeds.len()
    );
}

// ---------------------------------------------------------------------------
// 2. Hand-crafted malformed ISOBMFF — assert specific error variants.
// ---------------------------------------------------------------------------

/// Build a minimal `ftyp` box with the given major brand and compatible
/// brands. Returns the full box bytes (size + "ftyp" + brand + minor + compat).
fn ftyp(major: &[u8; 4], compatible: &[&[u8; 4]]) -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(major);
    content.extend_from_slice(&0u32.to_be_bytes()); // minor version
    for c in compatible {
        content.extend_from_slice(*c);
    }
    let mut out = Vec::new();
    let size = (8 + content.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(b"ftyp");
    out.extend_from_slice(&content);
    out
}

/// A buffer shorter than the 12-byte minimum must be reported as
/// `NeedMoreData`, distinguishing "give me more bytes" from "this isn't HEIC".
#[test]
fn probe_empty_and_tiny_is_need_more_data() {
    assert!(matches!(
        ImageInfo::from_bytes(&[]),
        Err(ProbeError::NeedMoreData)
    ));
    // 11 bytes: still under the 12-byte threshold even though it starts
    // plausibly.
    let mut almost = Vec::from(*b"\x00\x00\x00\x18ftyp");
    almost.truncate(11);
    assert!(matches!(
        ImageInfo::from_bytes(&almost),
        Err(ProbeError::NeedMoreData)
    ));
}

/// A 12+ byte buffer whose box-type field at offset 4 is not `ftyp` is a fast
/// `InvalidFormat` rejection — the probe must not try to parse it as a
/// container.
#[test]
fn probe_non_ftyp_magic_is_invalid_format() {
    // Looks like a box ("size" + "mdat") but no ftyp up front.
    let data = b"\x00\x00\x00\x10mdatHELLOWORLD";
    assert!(matches!(
        ImageInfo::from_bytes(data),
        Err(ProbeError::InvalidFormat)
    ));
    // Pure noise, long enough to pass the length gate.
    let noise = [0xABu8; 64];
    assert!(matches!(
        ImageInfo::from_bytes(&noise),
        Err(ProbeError::InvalidFormat)
    ));
}

/// A real `ftyp` box whose brand is not in the HEIF allowlist must parse far
/// enough to reach the brand check and then be reported as a corrupt /
/// non-HEIF container — NOT accepted, NOT panicking.
#[test]
fn probe_ftyp_with_foreign_brand_is_corrupt() {
    // Valid box framing, brand "qt  " (QuickTime) + no HEIF compatible brand.
    let data = ftyp(b"qt  ", &[b"qt  "]);
    match ImageInfo::from_bytes(&data) {
        Err(ProbeError::Corrupt(_)) => {}
        other => panic!("expected Corrupt for foreign brand, got {other:?}"),
    }
    // The full decode path routes through heif::parse too and must also reject.
    assert!(
        DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err(),
        "foreign-brand ftyp must not decode"
    );
}

/// An `ftyp` box whose declared content is shorter than the 8-byte
/// brand+minor minimum must be reported corrupt (hits the
/// `"ftyp too short"` branch in `parse_ftyp`).
#[test]
fn probe_truncated_ftyp_content_is_corrupt() {
    // size=12 box "ftyp" with only 4 content bytes (a brand, no minor version).
    let mut data = Vec::new();
    data.extend_from_slice(&12u32.to_be_bytes());
    data.extend_from_slice(b"ftyp");
    data.extend_from_slice(b"heic");
    // Pad to >= 12 total so the probe length-gate passes and we reach parse.
    assert!(data.len() >= 12);
    match ImageInfo::from_bytes(&data) {
        Err(ProbeError::Corrupt(_)) => {}
        // A NeedMoreData here would also be acceptable, but the box has a
        // self-consistent (short) size so the parser reaches the content
        // check — assert it's the corrupt branch.
        other => panic!("expected Corrupt for short ftyp content, got {other:?}"),
    }
}

/// A valid HEIF `ftyp` with NO `meta` box at all: the container parses but has
/// no primary item, so the probe reports a corrupt header (NoPrimaryImage),
/// and decode fails — neither panics.
#[test]
fn probe_valid_ftyp_no_meta_has_no_primary() {
    let data = ftyp(b"heic", &[b"mif1", b"heic"]);
    match ImageInfo::from_bytes(&data) {
        Err(ProbeError::Corrupt(_)) => {}
        other => panic!("expected Corrupt(NoPrimaryImage) for ftyp-only, got {other:?}"),
    }
    assert!(
        DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err()
    );
}

/// A `meta` box declaring a size far larger than the bytes that follow. The
/// box iterator must reject the over-long box (box_end > data.len()) and stop
/// rather than reading out of bounds — the net effect is "no primary item",
/// reported as a clean Err with no panic and no hang.
#[test]
fn probe_meta_with_oversized_declared_size_no_panic() {
    let mut data = ftyp(b"heic", &[b"mif1", b"heic"]);
    // meta box header claiming 0x7FFF_FFFF bytes but only a few real bytes.
    data.extend_from_slice(&0x7FFF_FFFFu32.to_be_bytes());
    data.extend_from_slice(b"meta");
    data.extend_from_slice(&[0u8; 8]); // far fewer than declared
    // Must not panic, must not hang, must be a clean rejection.
    let r = ImageInfo::from_bytes(&data);
    assert!(r.is_err(), "oversized meta should not yield a usable image");
    assert!(
        DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err()
    );
}

/// A `meta` box using the 64-bit extended-size form (`size32 == 1`) but
/// truncated before the 8-byte large-size field can be read. The box iterator
/// must bail (returns None) instead of indexing past the end.
#[test]
fn probe_extended_size_meta_truncated_no_panic() {
    let mut data = ftyp(b"heic", &[b"mif1", b"heic"]);
    data.extend_from_slice(&1u32.to_be_bytes()); // size32 == 1 -> 64-bit size
    data.extend_from_slice(b"meta");
    data.extend_from_slice(&[0u8; 4]); // only 4 of the needed 8 large-size bytes
    let r = ImageInfo::from_bytes(&data);
    assert!(r.is_err());
    // decode path walks the same iterator.
    assert!(
        DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err()
    );
}

/// A box whose 32-bit size field is `0` ("extends to end of file"). The
/// iterator treats it as running to EOF; with no useful content after the
/// ftyp this yields no primary item. Must be a clean Err, no panic.
#[test]
fn probe_zero_size_box_runs_to_eof_no_panic() {
    let mut data = ftyp(b"heic", &[b"mif1", b"heic"]);
    // A trailing box with size32 == 0: consumes the rest of the file.
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(b"free");
    data.extend_from_slice(&[0xCDu8; 16]);
    let r = ImageInfo::from_bytes(&data);
    assert!(r.is_err());
}

/// An `iloc` box inside `meta` declaring an enormous item_count while the box
/// content holds none of those entries. The parser bounds item_count against
/// `MAX_ITEMS` (and the per-entry reads break at end-of-content), so this must
/// be a clean Err — never an attempt to allocate/iterate billions of entries.
#[test]
fn probe_iloc_overclaimed_item_count_is_bounded() {
    // Assemble: ftyp + meta { pitm(primary=1) + iloc(item_count = u16::MAX) }.
    // The iloc body is otherwise empty, so per-entry reads break early.
    let mut iloc_content = Vec::new();
    iloc_content.push(0u8); // version 0
    iloc_content.extend_from_slice(&[0u8; 3]); // flags
    iloc_content.push(0u8); // offset_size<<4 | length_size
    iloc_content.push(0u8); // base_offset_size<<4 | index_size
    iloc_content.extend_from_slice(&u16::MAX.to_be_bytes()); // item_count = 65535
    // (no entries follow)
    let iloc = box_with(b"iloc", &iloc_content);

    let mut pitm_content = Vec::new();
    pitm_content.extend_from_slice(&[0u8; 4]); // version 0 + flags
    pitm_content.extend_from_slice(&1u16.to_be_bytes()); // primary item id = 1
    let pitm = box_with(b"pitm", &pitm_content);

    // meta is a FullBox: 4 bytes version+flags, then child boxes.
    let mut meta_content = Vec::new();
    meta_content.extend_from_slice(&[0u8; 4]); // version+flags
    meta_content.extend_from_slice(&pitm);
    meta_content.extend_from_slice(&iloc);
    let meta = box_with(b"meta", &meta_content);

    let mut data = ftyp(b"heic", &[b"mif1", b"heic"]);
    data.extend_from_slice(&meta);

    // 65535 <= MAX_ITEMS, so the count itself doesn't trip the limit; the body
    // is empty so no entries are read. The result is a container with a
    // primary id that resolves to no item info => no primary image.
    let r = ImageInfo::from_bytes(&data);
    assert!(
        r.is_err(),
        "iloc with overclaimed count + empty body must not yield an image"
    );
    // And it must complete quickly (no hang) — implicit: the test returns.
    assert!(
        DecoderConfig::new()
            .decode(&data, PixelLayout::Rgba8)
            .is_err()
    );
}

/// Truncate the canonical reference file at a series of progressively shorter
/// lengths and feed each prefix through the probe + decode paths. Real files
/// cut short mid-box are the most common malformed input in the wild; none of
/// these prefixes may panic.
#[test]
fn truncated_prefixes_of_real_file_never_panic() {
    let path = testdata_dir().join("libheif-examples").join("example.heic");
    let Ok(full) = std::fs::read(&path) else {
        eprintln!(
            "SKIP truncated_prefixes: reference file absent at {} (testdata should be committed)",
            path.display()
        );
        return;
    };
    assert!(full.len() > 1000, "reference file unexpectedly tiny");

    let config = DecoderConfig::new();
    // A spread of cut points: header region, mid-meta, mid-mdat, near-end.
    let cuts = [
        0usize,
        8,
        12,
        16,
        24,
        32,
        64,
        100,
        256,
        512,
        1024,
        full.len() / 2,
        full.len() - 1,
    ];
    for &cut in &cuts {
        let prefix = &full[..cut.min(full.len())];
        // No panic on either path; result value is irrelevant.
        let _ = ImageInfo::from_bytes(prefix);
        let _ = config.decode(prefix, PixelLayout::Rgba8);
        let _ = config.extract_exif(prefix);
        let _ = config.extract_xmp(prefix);
    }
}

/// A single byte flipped at every position in the first 256 bytes of the real
/// file (header + box structure region). Bit-rot / transmission corruption of
/// the container header must always be a clean Err-or-Ok, never a panic.
#[test]
fn single_byte_corruption_in_header_never_panics() {
    let path = testdata_dir().join("libheif-examples").join("example.heic");
    let Ok(full) = std::fs::read(&path) else {
        eprintln!("SKIP single_byte_corruption: reference file absent");
        return;
    };
    let config = DecoderConfig::new();
    let span = full.len().min(256);
    for i in 0..span {
        let mut corrupt = full.clone();
        corrupt[i] ^= 0xFF;
        // Probe the header region of a header-corrupted file — runs the parser
        // over every single-byte flip in the box-structure region.
        let _ = ImageInfo::from_bytes(&corrupt);
        // Full decode is far more expensive (a real grid image); exercise it on
        // a stride so a desynced box header still reaches the HEVC path without
        // re-decoding 256 full images. The contract on both paths is "no panic".
        if i % 8 == 0 {
            let _ = config.decode(&corrupt, PixelLayout::Rgba8);
        }
    }
}

// ---------------------------------------------------------------------------
// 3. Valid multi-file sweep — exercise the parser on every committed image.
// ---------------------------------------------------------------------------

/// Probe every committed `.heic`/`.heif` file. We assert (a) no panic, (b) a
/// meaningful number of files exist and probe successfully, and (c) the
/// canonical reference file probes to its KNOWN dimensions — the last check
/// proves the sweep harness exercises a real parse rather than swallowing
/// everything (testing the test infrastructure).
#[test]
fn probe_sweep_over_all_committed_files() {
    let mut files = Vec::new();
    collect(
        &testdata_dir(),
        &|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("heic") || e.eq_ignore_ascii_case("heif"))
        },
        &mut files,
    );
    files.sort();

    assert!(
        files.len() >= 50,
        "committed testdata corpus missing/thin ({} files under {}); the parser \
         sweep needs the full corpus (no graceful skip)",
        files.len(),
        testdata_dir().display()
    );

    let mut probed_ok = 0usize;
    let mut found_reference = false;
    for path in &files {
        let data = std::fs::read(path)
            .unwrap_or_else(|e| panic!("failed to read committed file {}: {e}", path.display()));
        match ImageInfo::from_bytes(&data) {
            Ok(info) => {
                // Any successful probe must report a non-degenerate image and a
                // plausible bit depth / chroma format.
                assert!(
                    info.width > 0 && info.height > 0,
                    "{} probed Ok with degenerate {}x{}",
                    path.display(),
                    info.width,
                    info.height
                );
                // Component bit depth: HEVC-coded items are 8/10/12; some
                // uncompressed HEIF packed formats (R5G6B5, R7G7B7, ...) carry
                // sub-byte component depths reported faithfully by the probe.
                assert!(
                    (1..=16).contains(&info.bit_depth),
                    "{} reported implausible bit_depth {}",
                    path.display(),
                    info.bit_depth
                );
                assert!(
                    info.chroma_format <= 3,
                    "{} reported implausible chroma_format {}",
                    path.display(),
                    info.chroma_format
                );
                probed_ok += 1;

                if path.ends_with("example.heic") {
                    found_reference = true;
                    assert_eq!(info.width, 1280, "reference probe width regressed");
                    assert_eq!(info.height, 854, "reference probe height regressed");
                    assert_eq!(info.bit_depth, 8);
                    assert_eq!(info.chroma_format, 1, "reference is 4:2:0");
                    assert!(!info.has_alpha);
                }
            }
            Err(e) => {
                // Some committed files (brotli-compressed unci, AV1 without the
                // feature, etc.) legitimately don't probe — they must report a
                // clean ProbeError, not panic. The match above already
                // guarantees no panic; record nothing extra.
                eprintln!("probe Err (acceptable) for {}: {e}", path.display());
            }
        }
    }

    assert!(
        found_reference,
        "canonical example.heic not found in committed corpus — harness would be untested"
    );
    assert!(
        probed_ok >= 30,
        "only {probed_ok} of {} committed files probed successfully; the parser \
         sweep should resolve dimensions for most of the corpus",
        files.len()
    );
    eprintln!(
        "probe_sweep_over_all_committed_files: {probed_ok}/{} files probed",
        files.len()
    );
}

/// Build an arbitrary box: 8-byte header (size + 4cc) wrapping `content`.
fn box_with(fourcc: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let size = (8 + content.len()) as u32;
    out.extend_from_slice(&size.to_be_bytes());
    out.extend_from_slice(fourcc);
    out.extend_from_slice(content);
    out
}
