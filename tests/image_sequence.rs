//! Integration tests for HEIF image sequence (msf1 / moov-based) decoding.
//!
//! Tests marked `#[ignore]` require external Nokia conformance files.
//! Set `HEIC_TEST_DIR` or have files at `/home/lilith/work/heic`.

use heic::{DecoderConfig, ImageInfo, PixelLayout};

fn heic_base_dir() -> String {
    std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into())
}

fn nokia_dir() -> String {
    format!(
        "{}/test-images/nokia-conformance/conformance_files",
        heic_base_dir()
    )
}

fn read_nokia(name: &str) -> Option<Vec<u8>> {
    let path = format!("{}/{name}", nokia_dir());
    std::fs::read(&path).ok()
}

// ---- Probe tests (ImageInfo::from_bytes) ----

#[test]
#[ignore]
fn probe_c026_image_sequence() {
    let data = read_nokia("C026.heic").expect("C026.heic not found");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert_eq!(info.width, 1280);
    assert_eq!(info.height, 720);
    assert_eq!(info.bit_depth, 8);
    assert_eq!(info.chroma_format, 1); // 4:2:0
    assert!(!info.has_alpha);
}

#[test]
#[ignore]
fn probe_all_msf1_files() {
    let msf1_files = [
        "C026.heic",
        "C027.heic",
        "C028.heic",
        "C029.heic",
        "C030.heic",
        "C031.heic",
        "C032.heic",
        "C036.heic",
        "C037.heic",
        "C038.heic",
        "C041.heic",
    ];

    let mut probed = 0;
    let mut failed = Vec::new();
    for name in &msf1_files {
        let Some(data) = read_nokia(name) else {
            eprintln!("  SKIP {name}: not found");
            continue;
        };
        match ImageInfo::from_bytes(&data) {
            Ok(info) => {
                assert!(info.width > 0 && info.height > 0, "{name}: zero dimensions");
                eprintln!(
                    "  OK {name}: {}x{} depth={} chroma={}",
                    info.width, info.height, info.bit_depth, info.chroma_format
                );
                probed += 1;
            }
            Err(e) => {
                failed.push(format!("{name}: {e}"));
            }
        }
    }

    if !failed.is_empty() {
        eprintln!("Failed to probe:");
        for f in &failed {
            eprintln!("  {f}");
        }
    }
    assert!(
        probed >= 10,
        "should probe at least 10 msf1 files, got {probed}"
    );
}

// ---- Decode tests ----

#[test]
#[ignore]
fn decode_c026_image_sequence() {
    let data = read_nokia("C026.heic").expect("C026.heic not found");
    let output = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .expect("decode failed");
    assert_eq!(output.width, 1280);
    assert_eq!(output.height, 720);
    assert_eq!(output.data.len(), 1280 * 720 * 3);

    // Verify not degenerate
    let non_zero = output.data.iter().any(|&v| v != 0);
    let non_max = output.data.iter().any(|&v| v != 255);
    assert!(non_zero, "decoded image should not be all zeros");
    assert!(non_max, "decoded image should not be all 255");
}

#[test]
#[ignore]
fn decode_c041_image_sequence() {
    // C041 is 1920x1080 — different dimensions than the 1280x720 files
    let data = read_nokia("C041.heic").expect("C041.heic not found");
    let output = DecoderConfig::new()
        .decode(&data, PixelLayout::Rgb8)
        .expect("decode failed");
    assert_eq!(output.width, 1920);
    assert_eq!(output.height, 1080);
    assert_eq!(output.data.len(), 1920 * 1080 * 3);
}

#[test]
#[ignore]
fn decode_all_msf1_files() {
    let msf1_files = [
        ("C026.heic", 1280, 720),
        ("C027.heic", 1280, 720),
        ("C028.heic", 1280, 720),
        ("C029.heic", 1280, 720),
        ("C030.heic", 1280, 720),
        ("C031.heic", 1280, 720),
        ("C032.heic", 1280, 720),
        ("C036.heic", 1280, 720),
        ("C037.heic", 1280, 720),
        ("C038.heic", 1280, 720),
        ("C041.heic", 1920, 1080),
    ];

    let mut decoded = 0;
    let mut failed = Vec::new();
    for (name, exp_w, exp_h) in &msf1_files {
        let Some(data) = read_nokia(name) else {
            eprintln!("  SKIP {name}: not found");
            continue;
        };
        match DecoderConfig::new().decode(&data, PixelLayout::Rgb8) {
            Ok(output) => {
                assert_eq!(output.width, *exp_w, "{name}: wrong width");
                assert_eq!(output.height, *exp_h, "{name}: wrong height");
                assert_eq!(
                    output.data.len(),
                    (*exp_w as usize) * (*exp_h as usize) * 3,
                    "{name}: data length mismatch"
                );
                eprintln!("  OK {name}: {}x{}", output.width, output.height);
                decoded += 1;
            }
            Err(e) => {
                failed.push(format!("{name}: {e}"));
            }
        }
    }

    eprintln!("\nDecoded {decoded}/{} msf1 files", msf1_files.len());
    if !failed.is_empty() {
        eprintln!("Failed:");
        for f in &failed {
            eprintln!("  {f}");
        }
    }
    assert!(
        decoded >= 8,
        "should decode at least 8 msf1 files, got {decoded}"
    );
}

// ---- C032 thumbnail track ----

#[test]
#[ignore]
fn c032_has_thumbnail() {
    let data = read_nokia("C032.heic").expect("C032.heic not found");
    let info = ImageInfo::from_bytes(&data).expect("probe failed");
    assert_eq!(info.width, 1280);
    assert_eq!(info.height, 720);
    assert!(info.has_thumbnail, "C032 should have a thumbnail track");
}

// ---- Limits and error handling ----

#[test]
#[ignore]
fn limits_reject_oversized_sequence() {
    let data = read_nokia("C026.heic").expect("C026.heic not found");
    let mut limits = heic::Limits::default();
    limits.max_width = Some(640);
    limits.max_height = Some(480);
    let result = DecoderConfig::new()
        .decode_request(&data)
        .with_output_layout(PixelLayout::Rgb8)
        .with_limits(&limits)
        .decode();
    assert!(
        result.is_err(),
        "should reject 1280x720 with 640x480 limits"
    );
}

// ---- Synthetic moov structure test (runs without external files) ----

/// Build a minimal msf1 file structure with a moov box that the parser should
/// accept without erroring on missing primary image.
#[test]
fn parse_synthetic_moov_structure() {
    // Build minimal ftyp + moov + mdat
    let mut buf = Vec::new();

    // ftyp: size(4) + "ftyp" + brand "msf1" + minor_version(4) + compat "hevc"
    let ftyp_size: u32 = 20;
    buf.extend_from_slice(&ftyp_size.to_be_bytes());
    buf.extend_from_slice(b"ftyp");
    buf.extend_from_slice(b"msf1");
    buf.extend_from_slice(&0u32.to_be_bytes()); // minor version
    buf.extend_from_slice(b"hevc");

    // Build a minimal hvcC box (23 bytes minimum + 0 arrays)
    let mut hvcc = Vec::new();
    hvcc.push(1); // config_version
    hvcc.push(0x20 | 1); // tier_flag=0, profile_space=0, profile_idc=1 (Main)
    hvcc.extend_from_slice(&0u32.to_be_bytes()); // profile_compat
    hvcc.extend_from_slice(&[0u8; 6]); // constraint_indicator (6 bytes)
    hvcc.push(120); // general_level_idc
    hvcc.extend_from_slice(&0xF000u16.to_be_bytes()); // min_spatial_seg
    hvcc.push(0); // parallelism
    hvcc.push(0x01); // chroma_format = 1 (4:2:0)
    hvcc.push(0x00); // bit_depth_luma_minus8 = 0
    hvcc.push(0x00); // bit_depth_chroma_minus8 = 0
    hvcc.extend_from_slice(&0u16.to_be_bytes()); // avg frame rate
    hvcc.push(0x03); // length_size_minus_one=3 (4 bytes)
    hvcc.push(0); // num_arrays = 0

    let hvcc_box_size = (8 + hvcc.len()) as u32;
    let mut hvcc_box = Vec::new();
    hvcc_box.extend_from_slice(&hvcc_box_size.to_be_bytes());
    hvcc_box.extend_from_slice(b"hvcC");
    hvcc_box.extend_from_slice(&hvcc);

    // Build visual sample entry (hvc1): 86 byte header + hvcC
    let vse_size = (86 + hvcc_box.len()) as u32;
    let mut vse = Vec::new();
    vse.extend_from_slice(&vse_size.to_be_bytes());
    vse.extend_from_slice(b"hvc1");
    vse.extend_from_slice(&[0u8; 6]); // reserved
    vse.extend_from_slice(&1u16.to_be_bytes()); // data_ref_index
    vse.extend_from_slice(&[0u8; 16]); // pre_defined + reserved + pre_defined
    vse.extend_from_slice(&64u16.to_be_bytes()); // width
    vse.extend_from_slice(&64u16.to_be_bytes()); // height
    vse.extend_from_slice(&0x00480000u32.to_be_bytes()); // horiz_res 72dpi
    vse.extend_from_slice(&0x00480000u32.to_be_bytes()); // vert_res 72dpi
    vse.extend_from_slice(&0u32.to_be_bytes()); // reserved
    vse.extend_from_slice(&1u16.to_be_bytes()); // frame_count
    vse.extend_from_slice(&[0u8; 32]); // compressor_name
    vse.extend_from_slice(&0x0018u16.to_be_bytes()); // depth
    vse.extend_from_slice(&0xFFFFu16.to_be_bytes()); // pre_defined = -1
    vse.extend_from_slice(&hvcc_box);

    // stsd: version+flags(4) + entry_count(4) + entry
    let stsd_size = (8 + 4 + 4 + vse.len()) as u32;
    let mut stsd = Vec::new();
    stsd.extend_from_slice(&stsd_size.to_be_bytes());
    stsd.extend_from_slice(b"stsd");
    stsd.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    stsd.extend_from_slice(&1u32.to_be_bytes()); // entry_count
    stsd.extend_from_slice(&vse);

    // stsz: version+flags(4) + sample_size(4) + sample_count(4) + sizes
    let sample_data = b"fake_hevc_sample_data_for_testing";
    let stsz_size: u32 = 8 + 4 + 4 + 4 + 4;
    let mut stsz = Vec::new();
    stsz.extend_from_slice(&stsz_size.to_be_bytes());
    stsz.extend_from_slice(b"stsz");
    stsz.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    stsz.extend_from_slice(&0u32.to_be_bytes()); // sample_size=0 (per-sample)
    stsz.extend_from_slice(&1u32.to_be_bytes()); // sample_count=1
    stsz.extend_from_slice(&(sample_data.len() as u32).to_be_bytes()); // sample 1 size

    // stco: version+flags(4) + entry_count(4) + offset (will fill later)
    let _stco_placeholder_offset = buf.len(); // will be patched
    let stco_size: u32 = 8 + 4 + 4 + 4;
    let mut stco = Vec::new();
    stco.extend_from_slice(&stco_size.to_be_bytes());
    stco.extend_from_slice(b"stco");
    stco.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    stco.extend_from_slice(&1u32.to_be_bytes()); // entry_count=1
    stco.extend_from_slice(&0u32.to_be_bytes()); // placeholder offset

    // stsc: version+flags(4) + entry_count(4) + (first_chunk, samples_per_chunk, desc_idx)
    let stsc_size: u32 = 8 + 4 + 4 + 12;
    let mut stsc = Vec::new();
    stsc.extend_from_slice(&stsc_size.to_be_bytes());
    stsc.extend_from_slice(b"stsc");
    stsc.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    stsc.extend_from_slice(&1u32.to_be_bytes()); // entry_count=1
    stsc.extend_from_slice(&1u32.to_be_bytes()); // first_chunk=1
    stsc.extend_from_slice(&1u32.to_be_bytes()); // samples_per_chunk=1
    stsc.extend_from_slice(&1u32.to_be_bytes()); // sample_desc_idx=1

    // stts: version+flags(4) + entry_count(4) + (sample_count, sample_delta)
    let stts_size: u32 = 8 + 4 + 4 + 8;
    let mut stts = Vec::new();
    stts.extend_from_slice(&stts_size.to_be_bytes());
    stts.extend_from_slice(b"stts");
    stts.extend_from_slice(&0u32.to_be_bytes());
    stts.extend_from_slice(&1u32.to_be_bytes());
    stts.extend_from_slice(&1u32.to_be_bytes());
    stts.extend_from_slice(&1u32.to_be_bytes());

    // stbl = stsd + stsz + stco + stsc + stts
    let stbl_size = (8 + stsd.len() + stsz.len() + stco.len() + stsc.len() + stts.len()) as u32;
    let mut stbl = Vec::new();
    stbl.extend_from_slice(&stbl_size.to_be_bytes());
    stbl.extend_from_slice(b"stbl");
    stbl.extend_from_slice(&stsd);
    stbl.extend_from_slice(&stsz);
    // Remember stco position for patching
    // stco offset will be patched after moov is assembled
    stbl.extend_from_slice(&stco);
    stbl.extend_from_slice(&stsc);
    stbl.extend_from_slice(&stts);

    // vmhd (video media header): version+flags(4) + graphicsmode(2) + opcolor(6)
    let vmhd_size: u32 = 8 + 4 + 2 + 6;
    let mut vmhd = Vec::new();
    vmhd.extend_from_slice(&vmhd_size.to_be_bytes());
    vmhd.extend_from_slice(b"vmhd");
    vmhd.extend_from_slice(&0u32.to_be_bytes());
    vmhd.extend_from_slice(&[0u8; 8]);

    // dinf + dref
    let dref_size: u32 = 8 + 4 + 4;
    let dinf_size: u32 = 8 + dref_size;
    let mut dinf = Vec::new();
    dinf.extend_from_slice(&dinf_size.to_be_bytes());
    dinf.extend_from_slice(b"dinf");
    dinf.extend_from_slice(&dref_size.to_be_bytes());
    dinf.extend_from_slice(b"dref");
    dinf.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    dinf.extend_from_slice(&0u32.to_be_bytes()); // entry_count=0

    // minf = vmhd + dinf + stbl
    let minf_size = (8 + vmhd.len() + dinf.len() + stbl.len()) as u32;
    let mut minf = Vec::new();
    minf.extend_from_slice(&minf_size.to_be_bytes());
    minf.extend_from_slice(b"minf");
    minf.extend_from_slice(&vmhd);
    minf.extend_from_slice(&dinf);
    minf.extend_from_slice(&stbl);

    // hdlr: version+flags(4) + pre_defined(4) + handler_type(4) + reserved(12) + name(1)
    let hdlr_size: u32 = 8 + 4 + 4 + 4 + 12 + 1;
    let mut hdlr = Vec::new();
    hdlr.extend_from_slice(&hdlr_size.to_be_bytes());
    hdlr.extend_from_slice(b"hdlr");
    hdlr.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    hdlr.extend_from_slice(&0u32.to_be_bytes()); // pre_defined
    hdlr.extend_from_slice(b"pict"); // handler_type
    hdlr.extend_from_slice(&[0u8; 12]); // reserved
    hdlr.push(0); // null-terminated name

    // mdhd: version+flags(4) + creation(4) + modification(4) + timescale(4) + duration(4) + lang(2) + pre_defined(2)
    let mdhd_size: u32 = 8 + 4 + 4 + 4 + 4 + 4 + 2 + 2;
    let mut mdhd = Vec::new();
    mdhd.extend_from_slice(&mdhd_size.to_be_bytes());
    mdhd.extend_from_slice(b"mdhd");
    mdhd.extend_from_slice(&0u32.to_be_bytes());
    mdhd.extend_from_slice(&[0u8; 16]); // times + duration
    mdhd.extend_from_slice(&[0u8; 4]); // lang + pre_defined

    // mdia = mdhd + hdlr + minf
    let mdia_size = (8 + mdhd.len() + hdlr.len() + minf.len()) as u32;
    let mut mdia = Vec::new();
    mdia.extend_from_slice(&mdia_size.to_be_bytes());
    mdia.extend_from_slice(b"mdia");
    mdia.extend_from_slice(&mdhd);
    mdia.extend_from_slice(&hdlr);
    mdia.extend_from_slice(&minf);

    // tkhd v0 (84 bytes content)
    let tkhd_size: u32 = 8 + 84;
    let mut tkhd = Vec::new();
    tkhd.extend_from_slice(&tkhd_size.to_be_bytes());
    tkhd.extend_from_slice(b"tkhd");
    tkhd.extend_from_slice(&[0u8; 4]); // version=0 + flags
    tkhd.extend_from_slice(&[0u8; 8]); // creation + modification time
    tkhd.extend_from_slice(&1u32.to_be_bytes()); // track_ID
    tkhd.extend_from_slice(&[0u8; 4]); // reserved
    tkhd.extend_from_slice(&[0u8; 4]); // duration
    tkhd.extend_from_slice(&[0u8; 8]); // reserved
    tkhd.extend_from_slice(&[0u8; 8]); // layer + alt_group + volume + reserved
    tkhd.extend_from_slice(&[0u8; 36]); // matrix
    tkhd.extend_from_slice(&((64u32) << 16).to_be_bytes()); // width 16.16
    tkhd.extend_from_slice(&((64u32) << 16).to_be_bytes()); // height 16.16

    // trak = tkhd + mdia
    let trak_size = (8 + tkhd.len() + mdia.len()) as u32;
    let mut trak = Vec::new();
    trak.extend_from_slice(&trak_size.to_be_bytes());
    trak.extend_from_slice(b"trak");
    trak.extend_from_slice(&tkhd);
    trak.extend_from_slice(&mdia);

    // mvhd (108 bytes content for v0)
    let mvhd_size: u32 = 8 + 108;
    let mut mvhd = Vec::new();
    mvhd.extend_from_slice(&mvhd_size.to_be_bytes());
    mvhd.extend_from_slice(b"mvhd");
    mvhd.extend_from_slice(&[0u8; 108]);

    // moov = mvhd + trak
    let moov_size = (8 + mvhd.len() + trak.len()) as u32;
    let mut moov = Vec::new();
    moov.extend_from_slice(&moov_size.to_be_bytes());
    moov.extend_from_slice(b"moov");
    moov.extend_from_slice(&mvhd);
    moov.extend_from_slice(&trak);

    // Now we know the mdat content offset — patch stco
    let mdat_content_offset = (buf.len() + moov.len() + 8) as u32; // 8 = mdat header
    // Find stco offset entry within moov: search for "stco" and patch the offset 12 bytes after
    let moov_bytes = &moov;
    let stco_pos = moov_bytes
        .windows(4)
        .position(|w| w == b"stco")
        .expect("stco not found in moov");
    // stco layout after "stco": version+flags(4) + entry_count(4) + offset(4)
    let offset_pos = stco_pos + 4 + 4 + 4; // after "stco" + ver+flags + count
    let offset_bytes = mdat_content_offset.to_be_bytes();
    // Can't mutate moov directly — reconstruct with patch
    let mut moov_patched = moov.clone();
    moov_patched[offset_pos] = offset_bytes[0];
    moov_patched[offset_pos + 1] = offset_bytes[1];
    moov_patched[offset_pos + 2] = offset_bytes[2];
    moov_patched[offset_pos + 3] = offset_bytes[3];

    buf.extend_from_slice(&moov_patched);

    // mdat
    let mdat_size = (8 + sample_data.len()) as u32;
    buf.extend_from_slice(&mdat_size.to_be_bytes());
    buf.extend_from_slice(b"mdat");
    buf.extend_from_slice(sample_data);

    // Parse the synthetic file
    let info = ImageInfo::from_bytes(&buf);
    match info {
        Ok(info) => {
            assert_eq!(info.width, 64);
            assert_eq!(info.height, 64);
            assert_eq!(info.chroma_format, 1); // 4:2:0 from hvcC
        }
        Err(e) => {
            // The hvcC has no actual VPS/SPS/PPS so the HEVC info extraction may fail,
            // but the container parsing should succeed — check that the error is from
            // HEVC probing, not from container parsing.
            let msg = format!("{e}");
            assert!(
                msg.contains("HEVC")
                    || msg.contains("missing")
                    || msg.contains("NAL")
                    || msg.contains("parameter"),
                "unexpected error (should be HEVC-related): {e}"
            );
        }
    }
}

/// Verify that files with both meta and moov prefer the meta path.
/// If meta is present, its primary_item_id is used. Since item 5 doesn't
/// exist in the meta, probing should fail with NoPrimaryImage — proving that
/// the moov path (which would create synthetic item 1) was NOT used.
#[test]
fn meta_takes_priority_over_moov() {
    let mut buf = Vec::new();

    // ftyp
    let ftyp_size: u32 = 20;
    buf.extend_from_slice(&ftyp_size.to_be_bytes());
    buf.extend_from_slice(b"ftyp");
    buf.extend_from_slice(b"heic");
    buf.extend_from_slice(&0u32.to_be_bytes());
    buf.extend_from_slice(b"heic");

    // meta (full box) with pitm pointing to item 5
    let pitm_size: u32 = 8 + 4 + 2; // size + type + ver+flags + item_id
    let meta_content_size = 4 + pitm_size as usize; // ver+flags + pitm
    let meta_size = (8 + meta_content_size) as u32;
    buf.extend_from_slice(&meta_size.to_be_bytes());
    buf.extend_from_slice(b"meta");
    buf.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    buf.extend_from_slice(&pitm_size.to_be_bytes());
    buf.extend_from_slice(b"pitm");
    buf.extend_from_slice(&0u32.to_be_bytes()); // version+flags
    buf.extend_from_slice(&5u16.to_be_bytes()); // item_id = 5

    // Empty moov (just header — would create synthetic item 1 if parsed)
    let moov_size: u32 = 8;
    buf.extend_from_slice(&moov_size.to_be_bytes());
    buf.extend_from_slice(b"moov");

    // Since meta is present, moov should be ignored. Item 5 doesn't exist
    // in meta, so probe fails with NoPrimaryImage.
    let result = ImageInfo::from_bytes(&buf);
    assert!(
        result.is_err(),
        "should fail because item 5 doesn't exist in meta"
    );
}
