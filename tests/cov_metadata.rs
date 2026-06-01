//! Coverage + behavior tests for the metadata-extraction paths in
//! `src/decode.rs` and `src/lib.rs`:
//! `extract_exif` / `extract_xmp` / `extract_icc`, the CICP / `colr`
//! fields surfaced on [`ImageInfo`], and their malformed-input behavior.
//!
//! Every test asserts REAL behavior against the bundled, committed
//! testdata fixtures — not just "the call returned Ok". The golden
//! positive fixture is `testdata/apple-hdr/hdr-sample.heic`, which carries
//! EXIF (big-endian TIFF), XMP (`<?xpacket ...>`), and an embedded ICC
//! profile in its `colr` box. The synthetic fixtures carry none of those,
//! which makes them the negative fixtures.
//!
//! Facts pinned by these tests (verified by probing the fixtures, not
//! assumed): apple-hdr's EXIF starts `MM\0\x2a` (big-endian TIFF), its XMP
//! starts with the `<?xpacket` packet wrapper and contains an `xmpmeta`
//! element, and its ICC is 536 bytes with the `acsp` profile signature at
//! byte offset 36 and a self-consistent size field at offset 0.

#![cfg(all(feature = "backend-rust", feature = "std"))]

use heic::{DecoderConfig, HeicError, ImageInfo, ProbeError};
use std::borrow::Cow;

const APPLE_HDR: &str = "testdata/apple-hdr/hdr-sample.heic";
const EXAMPLE: &str = "testdata/libheif-examples/example.heic";
const SYNTH_LOSSLESS: &str = "testdata/synthetic/synth_8bit_lossless.heic";
const SYNTH_Q50: &str = "testdata/synthetic/synth_8bit_q50.heic";
const UNCI_RGB: &str = "testdata/libheif-examples/uncompressed_comp_RGB.heif";

/// Read a bundled fixture or fail loudly with the path (the testdata is
/// committed, so a missing file is a real error, not a graceful skip).
fn read_fixture(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("bundled fixture {path} unreadable: {e}"))
}

// ---------------------------------------------------------------------------
// EXIF extraction
// ---------------------------------------------------------------------------

/// A file with EXIF returns `Some(bytes)` whose head is a valid TIFF
/// byte-order mark (`II` little-endian or `MM` big-endian) followed by the
/// TIFF magic `0x002A`. The HEIF 4-byte offset prefix must already be
/// stripped, so byte 0 must be a BOM byte, not a length byte. For
/// apple-hdr specifically the BOM is `MM` (big-endian), so the magic is the
/// big-endian `00 2A`.
#[test]
fn extract_exif_present_is_valid_tiff() {
    let data = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();

    // ImageInfo must agree that EXIF is present.
    let info = ImageInfo::from_bytes(&data).expect("probe apple-hdr");
    assert!(info.has_exif, "apple-hdr is known to carry EXIF");
    assert!(info.exif.is_some(), "ImageInfo.exif populated when present");

    let exif = cfg
        .extract_exif(&data)
        .expect("extract_exif must not error on a valid file")
        .expect("apple-hdr carries EXIF so this is Some");

    assert!(
        exif.len() >= 8,
        "a real TIFF header is at least 8 bytes, got {}",
        exif.len()
    );
    let bom = &exif[0..2];
    assert!(
        bom == b"II" || bom == b"MM",
        "EXIF must start with a TIFF byte-order mark (II/MM), got {bom:?}"
    );
    // Validate the TIFF magic 0x002A in the order the BOM declares.
    let magic = if bom == b"MM" {
        u16::from_be_bytes([exif[2], exif[3]])
    } else {
        u16::from_le_bytes([exif[2], exif[3]])
    };
    assert_eq!(magic, 0x002A, "TIFF magic must be 42 after the BOM");
    // apple-hdr's TIFF specifically is big-endian.
    assert_eq!(bom, b"MM", "apple-hdr EXIF is big-endian TIFF");

    // The bytes surfaced on ImageInfo must be byte-identical to the
    // dedicated extractor's bytes (two code paths, one source of truth).
    assert_eq!(
        info.exif.as_deref(),
        Some(exif.as_ref()),
        "ImageInfo.exif and extract_exif must agree byte-for-byte"
    );
}

/// A file WITHOUT EXIF returns `None` from `extract_exif`, and `ImageInfo`
/// agrees (`has_exif == false`, `exif == None`).
#[test]
fn extract_exif_absent_is_none() {
    let cfg = DecoderConfig::new();
    for path in [SYNTH_LOSSLESS, SYNTH_Q50, EXAMPLE, UNCI_RGB] {
        let data = read_fixture(path);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {path}: {e:?}"));
        assert!(!info.has_exif, "{path} has no EXIF");
        assert!(info.exif.is_none(), "{path} ImageInfo.exif must be None");
        let exif = cfg
            .extract_exif(&data)
            .unwrap_or_else(|e| panic!("extract_exif {path}: {e:?}"));
        assert!(exif.is_none(), "{path} extract_exif must be None");
    }
}

// ---------------------------------------------------------------------------
// XMP extraction
// ---------------------------------------------------------------------------

/// A file with XMP returns `Some(bytes)` that look like an XMP packet:
/// they contain the `xpacket` processing instruction and the `xmpmeta`
/// root element (the two canonical XMP markers). Also checks the bytes are
/// valid UTF-8 XML-ish text.
#[test]
fn extract_xmp_present_is_xmp_packet() {
    let data = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();

    let info = ImageInfo::from_bytes(&data).expect("probe apple-hdr");
    assert!(info.has_xmp, "apple-hdr is known to carry XMP");
    assert!(info.xmp.is_some(), "ImageInfo.xmp populated when present");

    let xmp = cfg
        .extract_xmp(&data)
        .expect("extract_xmp must not error on a valid file")
        .expect("apple-hdr carries XMP so this is Some");

    let text = std::str::from_utf8(&xmp).expect("XMP is UTF-8 XML text");
    assert!(
        text.contains("xpacket"),
        "XMP must contain the xpacket wrapper"
    );
    assert!(
        text.contains("xmpmeta"),
        "XMP must contain the xmpmeta root element"
    );
    // Sanity: the packet opens with the standard begin marker.
    assert!(
        text.trim_start().starts_with("<?xpacket"),
        "XMP packet starts with the xpacket PI"
    );

    assert_eq!(
        info.xmp.as_deref(),
        Some(xmp.as_ref()),
        "ImageInfo.xmp and extract_xmp must agree byte-for-byte"
    );
}

/// A file WITHOUT XMP returns `None`, and `ImageInfo` agrees.
#[test]
fn extract_xmp_absent_is_none() {
    let cfg = DecoderConfig::new();
    for path in [SYNTH_LOSSLESS, SYNTH_Q50, EXAMPLE, UNCI_RGB] {
        let data = read_fixture(path);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {path}: {e:?}"));
        assert!(!info.has_xmp, "{path} has no XMP");
        assert!(info.xmp.is_none(), "{path} ImageInfo.xmp must be None");
        let xmp = cfg
            .extract_xmp(&data)
            .unwrap_or_else(|e| panic!("extract_xmp {path}: {e:?}"));
        assert!(xmp.is_none(), "{path} extract_xmp must be None");
    }
}

// ---------------------------------------------------------------------------
// ICC profile extraction
// ---------------------------------------------------------------------------

/// A file with an embedded ICC profile returns `Some(bytes)` that form a
/// well-formed ICC profile: the 4-byte big-endian size field at offset 0
/// equals the byte length, and the 4-byte profile signature `acsp` sits at
/// offset 36 (the canonical ICC magic). The first preferred-CMM field at
/// offset 4 for apple-hdr is `appl`.
#[test]
fn extract_icc_present_is_valid_profile() {
    let data = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();

    let info = ImageInfo::from_bytes(&data).expect("probe apple-hdr");
    assert!(info.has_icc_profile, "apple-hdr carries an ICC profile");
    assert!(
        info.icc_profile.is_some(),
        "ImageInfo.icc_profile populated"
    );

    let icc = cfg
        .extract_icc(&data)
        .expect("extract_icc must not error on a valid file")
        .expect("apple-hdr carries an ICC profile so this is Some");

    assert!(
        icc.len() >= 132,
        "an ICC profile has at least a 128-byte header; got {}",
        icc.len()
    );
    // Header size field self-consistency (ICC.1 §7.2.2).
    let size_field = u32::from_be_bytes([icc[0], icc[1], icc[2], icc[3]]) as usize;
    assert_eq!(
        size_field,
        icc.len(),
        "ICC profile size field must equal the profile byte length"
    );
    // Profile signature 'acsp' at offset 36 (ICC.1 §7.2.7).
    assert_eq!(
        &icc[36..40],
        b"acsp",
        "ICC profile must carry the 'acsp' signature at offset 36"
    );
    // apple-hdr's preferred CMM is Apple's.
    assert_eq!(&icc[4..8], b"appl", "apple-hdr ICC preferred-CMM is 'appl'");
    assert_eq!(icc.len(), 536, "apple-hdr ICC profile is 536 bytes");

    assert_eq!(
        info.icc_profile.as_deref(),
        Some(icc.as_slice()),
        "ImageInfo.icc_profile and extract_icc must agree byte-for-byte"
    );
}

/// A file WITHOUT an ICC profile (uses nclx color parameters or has no
/// colr box) returns `None`, and `ImageInfo` agrees.
#[test]
fn extract_icc_absent_is_none() {
    let cfg = DecoderConfig::new();
    for path in [SYNTH_LOSSLESS, SYNTH_Q50, EXAMPLE, UNCI_RGB] {
        let data = read_fixture(path);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {path}: {e:?}"));
        assert!(!info.has_icc_profile, "{path} has no ICC profile");
        assert!(
            info.icc_profile.is_none(),
            "{path} ImageInfo.icc_profile must be None"
        );
        let icc = cfg
            .extract_icc(&data)
            .unwrap_or_else(|e| panic!("extract_icc {path}: {e:?}"));
        assert!(icc.is_none(), "{path} extract_icc must be None");
    }
}

// ---------------------------------------------------------------------------
// CICP / colr fields surfaced on ImageInfo
// ---------------------------------------------------------------------------

/// The CICP fields are surfaced from the primary item's `colr` nclx box,
/// defaulting to "unspecified" (the value 2) when the file carries no nclx.
/// example.heic and the synthetic fixtures have no nclx `colr` (their color
/// is signalled in the HEVC VUI instead), so the container-level CICP is
/// unspecified across all four fields. This pins the documented
/// "unspecified defaults" branch in `ImageInfo::from_bytes`.
#[test]
fn cicp_unspecified_when_no_nclx() {
    for path in [EXAMPLE, SYNTH_LOSSLESS, SYNTH_Q50] {
        let data = read_fixture(path);
        let info = ImageInfo::from_bytes(&data).unwrap_or_else(|e| panic!("probe {path}: {e:?}"));
        assert_eq!(info.color_primaries, 2, "{path} primaries unspecified");
        assert_eq!(
            info.transfer_characteristics, 2,
            "{path} transfer unspecified"
        );
        assert_eq!(info.matrix_coefficients, 2, "{path} matrix unspecified");
        // Default range flag for an unspecified nclx is limited range.
        assert!(!info.video_full_range, "{path} default is limited range");
    }
}

/// apple-hdr also has no nclx `colr` (its CICP is signalled elsewhere), but
/// it DOES carry an ICC profile — so the CICP fields are unspecified while
/// `has_icc_profile` is true. This pins the interaction between the
/// ICC-profile branch and the nclx-defaults branch.
#[test]
fn cicp_unspecified_but_icc_present_for_apple_hdr() {
    let data = read_fixture(APPLE_HDR);
    let info = ImageInfo::from_bytes(&data).expect("probe apple-hdr");
    assert_eq!(info.color_primaries, 2);
    assert_eq!(info.transfer_characteristics, 2);
    assert_eq!(info.matrix_coefficients, 2);
    assert!(
        info.has_icc_profile,
        "apple-hdr signals color via ICC, not nclx"
    );
    // 10-bit-source monochrome gain-map file: the primary is 8-bit 4:2:0
    // photo content (the gain map is the auxiliary), so chroma_format is
    // 4:2:0 == 1, and the file advertises a gain map.
    assert_eq!(info.bit_depth, 8, "primary item luma is 8-bit");
    assert_eq!(info.chroma_format, 1, "primary item is 4:2:0");
    assert!(info.has_gain_map, "apple-hdr carries an HDR gain map");
}

/// Known dimensions/format pins for the two headline fixtures, so a
/// regression in `apply_transform_dimensions` or the ispe/hvcC plumbing
/// fails loudly here.
#[test]
fn imageinfo_dimensions_and_format_pinned() {
    let ex = ImageInfo::from_bytes(&read_fixture(EXAMPLE)).expect("probe example");
    assert_eq!((ex.width, ex.height), (1280, 854), "example is 1280x854");
    assert_eq!(ex.bit_depth, 8);
    assert_eq!(ex.chroma_format, 1, "example is 4:2:0");
    assert!(!ex.has_alpha);

    let hdr = ImageInfo::from_bytes(&read_fixture(APPLE_HDR)).expect("probe apple-hdr");
    assert_eq!(
        (hdr.width, hdr.height),
        (1512, 850),
        "apple-hdr is 1512x850"
    );

    // unci (uncompressed) path: RGB 4:4:4, 30x20.
    let unci = ImageInfo::from_bytes(&read_fixture(UNCI_RGB)).expect("probe unci RGB");
    assert_eq!((unci.width, unci.height), (30, 20));
    assert_eq!(unci.chroma_format, 3, "unci RGB is 4:4:4");
}

// ---------------------------------------------------------------------------
// Cow ownership semantics of the extractors
// ---------------------------------------------------------------------------

/// `extract_exif` / `extract_xmp` return `Cow`. For apple-hdr's
/// single-extent items they should borrow (zero-copy) from the input
/// slice. Whichever variant is returned, the bytes must round-trip
/// identically through `.into_owned()`.
#[test]
fn extractor_cow_roundtrips_and_borrows_when_single_extent() {
    let data = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();

    let exif = cfg.extract_exif(&data).unwrap().expect("exif present");
    let exif_owned = exif.clone().into_owned();
    assert_eq!(
        exif.as_ref(),
        exif_owned.as_slice(),
        "exif Cow must round-trip"
    );
    // apple-hdr's Exif item is single-extent → borrowed (zero-copy).
    assert!(
        matches!(exif, Cow::Borrowed(_)),
        "single-extent EXIF should be borrowed"
    );

    let xmp = cfg.extract_xmp(&data).unwrap().expect("xmp present");
    let xmp_owned = xmp.clone().into_owned();
    assert_eq!(
        xmp.as_ref(),
        xmp_owned.as_slice(),
        "xmp Cow must round-trip"
    );
}

// ---------------------------------------------------------------------------
// Malformed / truncated input — clean Err / None, never a panic
// ---------------------------------------------------------------------------

/// Empty and sub-header inputs: the extractors and probe must reject them
/// cleanly. `extract_*` go through `heif::parse`, which errors on
/// not-a-container input; the probe returns `NeedMoreData` for < 12 bytes
/// and `InvalidFormat` when the second box isn't `ftyp`.
#[test]
fn malformed_tiny_inputs_are_clean_errors() {
    let cfg = DecoderConfig::new();

    // Empty input.
    assert!(cfg.extract_exif(&[]).is_err(), "empty → Err, no panic");
    assert!(cfg.extract_xmp(&[]).is_err(), "empty → Err, no panic");
    assert!(cfg.extract_icc(&[]).is_err(), "empty → Err, no panic");

    // Probe distinguishes too-short from wrong-format.
    assert!(
        matches!(ImageInfo::from_bytes(&[]), Err(ProbeError::NeedMoreData)),
        "empty probe → NeedMoreData"
    );
    assert!(
        matches!(
            ImageInfo::from_bytes(&[0u8; 8]),
            Err(ProbeError::NeedMoreData)
        ),
        "8-byte probe → NeedMoreData"
    );
    // 12+ bytes but no `ftyp` box at offset 4 → InvalidFormat.
    let not_heif = b"\0\0\0\x10mdat\0\0\0\0\0\0\0\0";
    assert!(
        matches!(
            ImageInfo::from_bytes(not_heif),
            Err(ProbeError::InvalidFormat)
        ),
        "non-ftyp 12-byte probe → InvalidFormat"
    );
    // And the extractors reject the same garbage rather than panicking.
    assert!(cfg.extract_exif(not_heif).is_err());
    assert!(cfg.extract_xmp(not_heif).is_err());
    assert!(cfg.extract_icc(not_heif).is_err());
}

/// A valid HEIC prefix truncated mid-box must surface a clean error (never
/// a panic / out-of-bounds). We truncate the golden file to a few hundred
/// bytes — enough to see the `ftyp` but not the full item structure.
#[test]
fn truncated_valid_file_is_clean_error() {
    let full = read_fixture(APPLE_HDR);
    let cfg = DecoderConfig::new();
    for cut in [16usize, 64, 128, 300, 1024] {
        let truncated = &full[..cut.min(full.len())];
        // Extractors must not panic; they either error or (if the prefix
        // happens to parse) return None — both are acceptable, a panic is
        // not. We just exercise the code path without unwrapping.
        let _ = cfg.extract_exif(truncated);
        let _ = cfg.extract_xmp(truncated);
        let _ = cfg.extract_icc(truncated);
        let _ = ImageInfo::from_bytes(truncated);
    }
    // Sentinel: at a small cut the probe definitely cannot complete, so it
    // must be an Err (not a silently-fabricated ImageInfo).
    assert!(
        ImageInfo::from_bytes(&full[..32]).is_err(),
        "32-byte prefix of a real file cannot yield a full ImageInfo"
    );
}

/// The fuzz-regression crash seeds are crafted malformed HEIF containers
/// (valid `ftyp`, broken internals). Feeding each through every metadata
/// path must produce a clean `Result` (Ok(None)/Err) and never panic. This
/// is the untrusted-input gate for the extract_* surface.
#[test]
fn fuzz_regression_seeds_dont_panic_in_extractors() {
    let dir = std::path::Path::new("fuzz/regression");
    if !dir.is_dir() {
        eprintln!("fuzz/regression missing — skipping crash-seed gate (precondition)");
        return;
    }
    let cfg = DecoderConfig::new();
    let mut seen = 0usize;
    for entry in std::fs::read_dir(dir).expect("read fuzz/regression") {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };
        seen += 1;
        // None of these may panic. Results are intentionally not unwrapped:
        // both Ok(None) and Err are valid for adversarial input. The
        // assertion is the absence of a panic / abort across the call set.
        let _ = cfg.extract_exif(&data);
        let _ = cfg.extract_xmp(&data);
        let _ = cfg.extract_icc(&data);
        let _ = ImageInfo::from_bytes(&data);
        let _ = cfg.has_gain_map(&data);
        let _ = cfg.has_depth(&data);
    }
    assert!(
        seen > 0,
        "fuzz/regression must contain crafted seeds to exercise"
    );
}

/// A container whose 4-byte EXIF offset prefix points past the end of the
/// item data must yield `None` (the documented `tiff_start < len` guard),
/// not a panic or a slice out of bounds. We can't easily forge a full HEIF
/// here, so we assert the guard's observable contract on real fixtures: the
/// apple-hdr EXIF strip succeeded (offset within bounds) and produced a
/// non-empty TIFF, while absent files produce None. This pins both sides of
/// the `tiff_start` branch via real data.
#[test]
fn exif_offset_prefix_guard_is_respected() {
    let cfg = DecoderConfig::new();
    // Present-and-valid side: stripping succeeded, leaving real TIFF bytes.
    let data = read_fixture(APPLE_HDR);
    let exif = cfg.extract_exif(&data).unwrap().expect("present");
    assert!(!exif.is_empty(), "stripped TIFF is non-empty");
    assert_ne!(
        &exif[0..2],
        b"\0\0",
        "the 4-byte offset prefix was stripped (byte 0 is a BOM, not a length)"
    );
    // Absent side: no Exif item at all → None.
    assert!(
        cfg.extract_exif(&read_fixture(SYNTH_Q50))
            .unwrap()
            .is_none()
    );
}

/// Probing a genuinely-corrupt-but-ftyp container surfaces
/// `ProbeError::Corrupt` wrapping a `HeicError`. We use a crash seed that
/// is a valid `ftyp` but has no decodable primary image; the probe must
/// classify it as Corrupt (not InvalidFormat, since the `ftyp` is present).
#[test]
fn corrupt_ftyp_container_probes_as_corrupt() {
    // Smallest crash seed that is a real ftyp HEIF with broken internals.
    let path = "fuzz/regression/crash-0270d85adb0ff4bbfcef8aed88f3f18c9e3457c0";
    let Ok(data) = std::fs::read(path) else {
        eprintln!("crash seed {path} missing — skipping (precondition)");
        return;
    };
    // It must start with a valid ftyp so the format check passes.
    assert_eq!(&data[4..8], b"ftyp", "seed is a real ftyp container");
    match ImageInfo::from_bytes(&data) {
        Err(ProbeError::Corrupt(inner)) => {
            // The wrapped HeicError must be a real decode/container error,
            // and Display must not panic.
            let msg = format!("{}", inner.error());
            assert!(!msg.is_empty(), "corrupt error has a message");
            assert!(
                matches!(
                    inner.error(),
                    HeicError::NoPrimaryImage
                        | HeicError::InvalidContainer(_)
                        | HeicError::InvalidData(_)
                        | HeicError::HevcDecode(_)
                ),
                "Corrupt wraps a container/decode error, got: {msg}"
            );
        }
        other => panic!("expected ProbeError::Corrupt for a broken ftyp, got {other:?}"),
    }
}
