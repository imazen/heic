//! Structural inventory (`DecodeJob::inventory`, `src/inventory.rs`).
//!
//! - Every committed `testdata/` HEIC/HEIF passes
//!   `zencodec_testkit::check_inventory` (whole file, file + junk, every
//!   truncation), under the default job and under a job that decodes the gain
//!   map and depth map.
//! - The bytes the inventory attributes to EXIF, XMP and ICC are the bytes
//!   `probe_full` reports, so the dispositions describe the real decode path.
//! - A synthetic file holding every box and item kind heic handles, plus
//!   private units and trailing bytes, has a pinned part list.
//! - `HEIC_INVENTORY_CORPUS=<dir>` runs `check_inventory` over a corpus
//!   directory (`just inventory-corpus` points it at codec-corpus
//!   `heic-conformance`).
//! - `INVENTORY_ORACLE_EXIFTOOL=<exiftool>` with `HEIC_INVENTORY_CORPUS`
//!   cross-checks every box exiftool lists against the inventory
//!   (`just inventory-oracle`).
//! - `INVENTORY_ORACLE_FFMPEG=<ffmpeg>` (and `ffprobe` beside it) with
//!   `HEIC_INVENTORY_CORPUS` cross-checks every NAL unit ffmpeg reads
//!   against the coded-unit parts (`just inventory-oracle-ffmpeg`).
//!
//! The env-gated tests are decided by the caller (justfile / CI), not by the
//! test: with the variable unset they do nothing and say so.

#![cfg(all(feature = "zencodec", feature = "backend-rust"))]

use std::path::{Path, PathBuf};

use heic::HeicDecoderConfig;
use zencodec::decode::{DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, Part, PartKind, PartTag};

fn testdata_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("testdata")
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "heic" | "heif" | "avif" | "hif"
            )
        }) {
            out.push(p);
        }
    }
}

fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect(dir, &mut out);
    out.sort();
    out
}

fn inventory_of(config: HeicDecoderConfig, data: &[u8]) -> Inventory {
    let inv = config
        .job()
        .inventory(data)
        .expect("inventory errs only on cancellation or limits")
        .expect("heic declares the inventory capability");
    inv.validate()
        .unwrap_or_else(|e| panic!("invalid inventory: {e}\n{inv}"));
    inv
}

fn bytes<'a>(data: &'a [u8], p: &Part) -> &'a [u8] {
    &data[p.range.start as usize..p.range.end as usize]
}

// ── Committed corpus ────────────────────────────────────────────────────────

#[test]
fn committed_corpus_inventories_conform() {
    let files = files_under(&testdata_dir());
    assert!(
        files.len() >= 50,
        "testdata corpus missing or thin ({} files)",
        files.len()
    );
    let configs = [
        HeicDecoderConfig::new(),
        HeicDecoderConfig::new()
            .with_extract_gain_map(true)
            .with_extract_depth(true),
    ];
    let mut undecodable = 0;
    for path in &files {
        let data = std::fs::read(path).unwrap();
        for config in &configs {
            check_file(config, &data, &path.display().to_string(), &mut undecodable);
        }
    }
    eprintln!(
        "{} files, {undecodable} config/file pairs without image data",
        files.len()
    );
}

/// `check_inventory` for a file the inventory says the decoder turns into
/// pixels; for one it says nothing decodes, the same guarantees minus "some
/// part is image data", plus proof that the decode really fails.
fn check_file(config: &HeicDecoderConfig, data: &[u8], name: &str, undecodable: &mut usize) {
    let inv = inventory_of(config.clone(), data);
    if inv
        .parts()
        .iter()
        .any(|p| p.disposition == Disposition::ImageData)
    {
        zencodec_testkit::check_inventory(config.clone(), data)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        return;
    }
    *undecodable += 1;
    let decoded = config
        .clone()
        .job()
        .decoder(std::borrow::Cow::Borrowed(data), &[])
        .and_then(zencodec::decode::Decode::decode);
    assert!(
        decoded.is_err(),
        "{name}: the inventory attributes no bytes to image data, but the decode succeeds\n{inv}"
    );
    let mut junked = data.to_vec();
    junked.extend((0..37u8).map(|i| i.wrapping_mul(97) ^ 0x5A));
    let inv = inventory_of(config.clone(), &junked);
    let mut has_child = vec![false; inv.parts().len()];
    for p in inv.parts() {
        if let Some(q) = p.parent {
            has_child[q.index()] = true;
        }
    }
    for (i, p) in inv.parts().iter().enumerate() {
        if !has_child[i] && p.range.end > data.len() as u64 {
            assert!(
                !p.disposition.is_consumed(),
                "{name}: appended junk reported as {}\n{inv}",
                p.disposition
            );
        }
    }
    let len = data.len();
    let mut cuts: Vec<usize> = vec![0, 1, 2, 3, 4, 8, 16];
    for (num, den) in [(1, 8), (1, 4), (3, 8), (1, 2), (5, 8), (3, 4), (7, 8)] {
        cuts.push(len * num / den);
    }
    cuts.push(len.saturating_sub(1));
    cuts.retain(|&n| n < len);
    for n in cuts {
        let inv = inventory_of(config.clone(), &data[..n]);
        assert_eq!(inv.input_len(), n as u64, "{name}");
    }
}

/// The EXIF, XMP and ICC bytes the inventory marks as reported are exactly
/// what `probe_full` reports, and nothing else is marked.
#[test]
fn reported_metadata_matches_probe_full() {
    let files = files_under(&testdata_dir());
    let mut checked = [0usize; 3];
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let job = HeicDecoderConfig::new().job();
        let Ok(info) = job.probe_full(&data) else {
            continue;
        };
        let inv = inventory_of(HeicDecoderConfig::new(), &data);
        // EXIF: the leaf (a split extent's TIFF child is what is reported).
        // XMP and ICC: the whole extent or property, trailer included.
        let mut has_child = vec![false; inv.parts().len()];
        for p in inv.parts() {
            if let Some(q) = p.parent {
                has_child[q.index()] = true;
            }
        }
        let of_kind = |k: MetadataKind| -> Vec<&Part> {
            inv.parts()
                .iter()
                .enumerate()
                .filter(|(i, p)| {
                    p.disposition == Disposition::Metadata(k)
                        && (k != MetadataKind::Exif || !has_child[*i])
                })
                .map(|(_, p)| p)
                .collect()
        };
        let name = path.display();

        let exif = of_kind(MetadataKind::Exif);
        match &info.embedded_metadata.exif {
            Some(want) => {
                assert_eq!(exif.len(), 1, "{name}: one EXIF part expected\n{inv}");
                // The TIFF child of a split extent is exactly what is reported.
                assert_eq!(
                    bytes(&data, exif[0]),
                    &want[..],
                    "{name}: EXIF bytes differ\n{inv}"
                );
                checked[0] += 1;
            }
            None => assert!(
                exif.is_empty(),
                "{name}: EXIF marked but not reported\n{inv}"
            ),
        }

        let xmp = of_kind(MetadataKind::Xmp);
        match &info.embedded_metadata.xmp {
            Some(want) => {
                let got: Vec<u8> = xmp.iter().flat_map(|p| bytes(&data, p).to_vec()).collect();
                assert_eq!(&got[..], &want[..], "{name}: XMP bytes differ\n{inv}");
                checked[1] += 1;
            }
            None => assert!(xmp.is_empty(), "{name}: XMP marked but not reported\n{inv}"),
        }

        let icc = of_kind(MetadataKind::Icc);
        match &info.source_color.icc_profile {
            Some(want) => {
                assert_eq!(icc.len(), 1, "{name}: one ICC property expected\n{inv}");
                // colr header (8) + colour type (4).
                assert_eq!(
                    &bytes(&data, icc[0])[12..],
                    &want[..],
                    "{name}: ICC bytes differ"
                );
                checked[2] += 1;
            }
            None => assert!(icc.is_empty(), "{name}: ICC marked but not reported\n{inv}"),
        }
    }
    eprintln!(
        "cross-checked EXIF in {}, XMP in {}, ICC in {} files",
        checked[0], checked[1], checked[2]
    );
    assert!(
        checked.iter().all(|&n| n > 0),
        "the corpus must exercise EXIF, XMP and ICC: {checked:?}"
    );
}

/// The default job leaves the gain-map image undecoded; a job that extracts
/// it consumes it.
#[test]
fn gain_map_disposition_follows_the_job() {
    let data = std::fs::read(testdata_dir().join("apple-hdr/hdr-sample.heic")).unwrap();
    let gain_map_extents = |inv: &Inventory| -> Vec<Disposition> {
        inv.parts()
            .iter()
            .filter(|p| p.kind == PartKind::Extent && p.tag == PartTag::Code(10))
            .map(|p| p.disposition)
            .collect()
    };
    let plain = inventory_of(HeicDecoderConfig::new(), &data);
    assert_eq!(gain_map_extents(&plain), [Disposition::Skipped], "{plain}");
    let extracting = inventory_of(HeicDecoderConfig::new().with_extract_gain_map(true), &data);
    assert_eq!(
        gain_map_extents(&extracting),
        [Disposition::Metadata(MetadataKind::GainMap)],
        "{extracting}"
    );
}

// ── Synthetic fixture ───────────────────────────────────────────────────────

fn bx(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut v = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(typ);
    v.extend_from_slice(payload);
    v
}

fn full(typ: &[u8; 4], version: u8, flags: u32, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![version];
    p.extend_from_slice(&flags.to_be_bytes()[1..]);
    p.extend_from_slice(payload);
    bx(typ, &p)
}

fn cat(parts: &[Vec<u8>]) -> Vec<u8> {
    parts.concat()
}

fn infe(id: u16, typ: &[u8; 4], name: &str, content_type: Option<&str>, hidden: bool) -> Vec<u8> {
    let mut p = id.to_be_bytes().to_vec();
    p.extend_from_slice(&[0, 0]);
    p.extend_from_slice(typ);
    p.extend_from_slice(name.as_bytes());
    p.push(0);
    if let Some(ct) = content_type {
        p.extend_from_slice(ct.as_bytes());
        p.push(0);
    }
    full(b"infe", 2, u32::from(hidden), &p)
}

fn iref_entry(typ: &[u8; 4], from: u16, to: &[u16]) -> Vec<u8> {
    let mut p = from.to_be_bytes().to_vec();
    p.extend_from_slice(&(to.len() as u16).to_be_bytes());
    for t in to {
        p.extend_from_slice(&t.to_be_bytes());
    }
    bx(typ, &p)
}

/// One `iloc` v1 entry: `(item, construction method, [(offset, length)])`,
/// 4-byte offsets and lengths, no base offset.
type Loc = (u16, u8, Vec<(u32, u32)>);

fn iloc(locs: &[Loc]) -> Vec<u8> {
    let mut p = vec![0x44, 0x00]; // offset_size 4, length_size 4, base 0, index 0
    p.extend_from_slice(&(locs.len() as u16).to_be_bytes());
    for (id, method, extents) in locs {
        p.extend_from_slice(&id.to_be_bytes());
        p.extend_from_slice(&[0, *method]);
        p.extend_from_slice(&[0, 0]); // data reference index
        p.extend_from_slice(&(extents.len() as u16).to_be_bytes());
        for (o, l) in extents {
            p.extend_from_slice(&o.to_be_bytes());
            p.extend_from_slice(&l.to_be_bytes());
        }
    }
    full(b"iloc", 1, 0, &p)
}

/// A HEIC holding every box and item kind heic handles, private units, an
/// unreferenced property, slack in `mdat` and `idat`, an extent outside every
/// `mdat`, a zero-length extent and trailing bytes.
/// UUID planted in the synthetic `user_data_unregistered` SEI message.
const PLANTED_UUID: [u8; 16] = *b"\x9d\x41\x7a\x52\x3b\x60\x4f\x1e\x8c\x2a\x71\x45\x0e\x93\xd6\xb8";
const PLANTED_UUID_STR: &str = "9d417a52-3b60-4f1e-8c2a-71450e93d6b8";

/// A NAL unit with a 4-byte length.
fn nal4(nal: &[u8]) -> Vec<u8> {
    cat(&[(nal.len() as u32).to_be_bytes().to_vec(), nal.to_vec()])
}

/// A prefix SEI NAL unit holding one `user_data_unregistered` message: the
/// planted UUID, then an identifying string whose RBSP contains `00 00 01`
/// (escaped as `00 00 03 01`).
fn planted_sei() -> Vec<u8> {
    let user = b"encoder=planted-id-7\0\0\x01!";
    let mut nal = vec![0x4E, 0x01, 5, (16 + user.len()) as u8];
    nal.extend_from_slice(&PLANTED_UUID);
    nal.extend_from_slice(b"encoder=planted-id-7\0\0\x03\x01!");
    nal.push(0x80);
    nal
}

/// A filler-data NAL unit.
fn planted_fd() -> Vec<u8> {
    vec![0x4C, 0x01, 0xFF, 0xFF, 0xFF, 0x80]
}

fn planted_nal_units() -> Vec<u8> {
    cat(&[
        nal4(&planted_sei()),
        nal4(&[0x46, 0x01, 0x50]),             // AUD
        nal4(&[0x26, 0x01, b's', b'l', b'c']), // IDR_W_RADL slice
        nal4(&[0x02, 0x09, b'e', b'l']),       // TRAIL_R, nuh_layer_id 1
        nal4(&[0x52, 0x01, b'r']),             // reserved type 41
        nal4(&[]),                             // zero-length unit
        nal4(&planted_fd()),
        // Suffix SEI: user_data_registered_itu_t_t35, country 0xB5,
        // provider 0x003C.
        nal4(&[0x50, 0x01, 4, 5, 0xB5, 0x00, 0x3C, 0x00, 0x01, 0x80]),
        b"JNK".to_vec(),
    ])
}

fn synthetic_heic() -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
    let dinf = bx(
        b"dinf",
        &full(
            b"dref",
            0,
            0,
            &cat(&[1u32.to_be_bytes().to_vec(), full(b"url ", 0, 1, b"")]),
        ),
    );
    let pitm = full(b"pitm", 0, 0, &1u16.to_be_bytes());
    let iinf = full(
        b"iinf",
        0,
        0,
        &cat(&[
            7u16.to_be_bytes().to_vec(),
            infe(1, b"hvc1", "Primary", None, false),
            infe(2, b"Exif", "", None, true),
            infe(3, b"mime", "XMP", Some("application/rdf+xml"), true),
            infe(4, b"mime", "notes", Some("text/plain"), true),
            infe(5, b"zzzz", "private", None, true),
            infe(6, b"grid", "", None, true),
            infe(7, b"hvc1", "Thumb", None, false),
        ]),
    );
    let iref = full(
        b"iref",
        0,
        0,
        &cat(&[
            iref_entry(b"cdsc", 2, &[1]),
            iref_entry(b"thmb", 7, &[1]),
            iref_entry(b"zRef", 5, &[1]),
        ]),
    );
    // hvcC: 23 fixed bytes, lengthSizeMinusOne = 3, an empty PPS array and a
    // prefix-SEI array (heic ignores SEI) holding one 4-byte NAL unit.
    let mut hvcc = vec![
        1u8, 1, 0x60, 0, 0, 0, 0x90, 0, 0, 0, 0, 0, 60, 0xF0, 0, 0xFC, 0xFD, 0xF8, 0xF8, 0, 0,
        0x0F, 2,
    ];
    hvcc.extend_from_slice(&[0x22, 0, 0]);
    hvcc.extend_from_slice(&[0x27, 0, 1, 0, 4, 0x4E, 0x01, 0x05, 0x80]);
    let ipco = bx(
        b"ipco",
        &cat(&[
            bx(b"hvcC", &hvcc), // 1
            full(
                b"ispe",
                0,
                0,
                &cat(&[
                    64u32.to_be_bytes().to_vec(),
                    64u32.to_be_bytes().to_vec(),
                    b"xyz".to_vec(), // 3 bytes heic never reads
                ]),
            ), // 2
            bx(b"colr", b"nclx\0\x01\0\x0d\0\x06\x80"), // 3
            bx(b"colr", b"rICCfake-icc-profile"), // 4
            bx(b"irot", &[1]),  // 5
            full(b"pixi", 0, 0, &[3, 8, 8, 8]), // 6
            bx(b"zPrv", b"private property"), // 7
            bx(b"clli", &[0x03, 0xE8, 0x00, 0xC8]), // 8
            bx(b"uuid", b"0123456789abcdef-payload"), // 9
            full(
                b"ispe",
                0,
                0,
                &cat(&[16u32.to_be_bytes().to_vec(), 16u32.to_be_bytes().to_vec()]),
            ), // 10
            // 11: an ICC profile declaring 20 bytes, followed by 5 more.
            bx(
                b"colr",
                &cat(&[
                    b"prof".to_vec(),
                    20u32.to_be_bytes().to_vec(),
                    b"sixteen-icc-body".to_vec(),
                    b"after".to_vec(),
                ]),
            ),
        ]),
    );
    // item 1: hvcC, ispe, colr nclx, colr rICC, irot, pixi, clli, colr prof
    // (the later colr supersedes the nclx); item 7: hvcC, ispe 16.
    let ipma = full(
        b"ipma",
        0,
        0,
        &cat(&[
            2u32.to_be_bytes().to_vec(),
            vec![0, 1, 8, 0x81, 2, 3, 4, 0x85, 6, 8, 11],
            vec![0, 7, 2, 0x81, 10],
        ]),
    );
    let iprp = bx(b"iprp", &cat(&[ipco, ipma]));
    let grid_descriptor = [0u8, 0, 0, 0, 0, 64, 0, 64];
    let idat = bx(b"idat", &cat(&[grid_descriptor.to_vec(), b"idt".to_vec()]));
    let grpl = bx(
        b"grpl",
        &full(
            b"altr",
            0,
            0,
            &cat(&[
                9u32.to_be_bytes().to_vec(),
                2u32.to_be_bytes().to_vec(),
                1u32.to_be_bytes().to_vec(),
                7u32.to_be_bytes().to_vec(),
            ]),
        ),
    );
    let zmet = bx(b"zMet", b"private meta child");
    let free = bx(b"free", b"stale editor data");
    let uuid = bx(
        b"uuid",
        b"\xbe\x7a\xcf\xcb\x97\xa9\x42\xe8\x9c\x71\x99\x94\x91\xe3\xaf\xacprivate payload",
    );

    // mdat payload. The primary item's data is 4-byte length-prefixed NAL
    // units: SEI carrying an identifying string, decoded and ignored types,
    // filler, and 3 bytes too short for a length field.
    let primary = planted_nal_units();
    let slack = b"slack".to_vec();
    // EXIF with a 2-byte gap before its TIFF header; XMP with bytes after
    // its packet trailer.
    let exif = b"\0\0\0\x02PPMM\0*\0\0\0\x08\0\0".to_vec();
    let xmp = b"<?xpacket begin?><x:xmpmeta/><?xpacket end=\"w\"?>TAIL".to_vec();
    let notes = b"free text!".to_vec();
    let private = b"PRIVATE!".to_vec();
    let thumb = b"THUMB-HEVC".to_vec();
    let mdat_payload = cat(&[
        primary.clone(),
        slack.clone(),
        exif.clone(),
        xmp.clone(),
        notes.clone(),
        private.clone(),
        thumb.clone(),
    ]);

    let build = |mdat_at: u32, free_at: u32| -> Vec<u8> {
        let d = mdat_at + 8;
        let p = d;
        let e = p + (primary.len() + slack.len()) as u32;
        let x = e + exif.len() as u32;
        let n = x + xmp.len() as u32;
        let v = n + notes.len() as u32;
        let t = v + private.len() as u32;
        let iloc = iloc(&[
            (1, 0, vec![(p, primary.len() as u32)]),
            (2, 0, vec![(e, exif.len() as u32)]),
            (3, 0, vec![(x, xmp.len() as u32)]),
            (4, 0, vec![(n, notes.len() as u32), (n, 0)]),
            (5, 0, vec![(v, private.len() as u32), (free_at + 8, 5)]),
            (6, 1, vec![(0, grid_descriptor.len() as u32)]),
            (7, 0, vec![(t, thumb.len() as u32)]),
        ]);
        let meta = full(
            b"meta",
            0,
            0,
            &cat(&[
                hdlr.clone(),
                dinf.clone(),
                pitm.clone(),
                iloc,
                iinf.clone(),
                iref.clone(),
                iprp.clone(),
                idat.clone(),
                grpl.clone(),
                zmet.clone(),
            ]),
        );
        cat(&[
            ftyp.clone(),
            meta,
            free.clone(),
            uuid.clone(),
            bx(b"mdat", &mdat_payload),
            b"TRAILER".to_vec(),
        ])
    };
    let draft = build(0, 0);
    let free_at = draft.windows(4).position(|w| w == b"free").unwrap() as u32 - 4;
    let mdat_at = draft.windows(4).position(|w| w == b"mdat").unwrap() as u32 - 4;
    build(mdat_at, free_at)
}

#[test]
fn synthetic_fixture_conforms() {
    let data = synthetic_heic();
    zencodec_testkit::check_inventory(HeicDecoderConfig::new(), &data).unwrap();
}

/// The exact part list of the synthetic fixture, in file order.
#[test]
fn synthetic_fixture_part_list_is_pinned() {
    let data = synthetic_heic();
    let inv = inventory_of(HeicDecoderConfig::new(), &data);
    let mut got = Vec::new();
    fn walk(
        inv: &Inventory,
        parent: Option<zencodec::inventory::PartId>,
        depth: usize,
        out: &mut Vec<String>,
    ) {
        for id in inv.children(parent) {
            let p = inv.get(id).unwrap();
            out.push(format!(
                "{}{} {} {}..{} {}{}",
                "  ".repeat(depth),
                p.kind.name(),
                p.tag,
                p.range.start,
                p.range.end,
                p.disposition,
                p.label
                    .as_deref()
                    .map(|l| format!(" {l:?}"))
                    .unwrap_or_default()
            ));
            walk(inv, Some(id), depth + 1, out);
        }
    }
    walk(&inv, None, 0, &mut got);
    let expected = EXPECTED_SYNTHETIC
        .trim_matches('\n')
        .lines()
        .collect::<Vec<_>>();
    let got_ref: Vec<&str> = got.iter().map(String::as_str).collect();
    assert_eq!(got_ref, expected, "\n{}\n{inv}", got.join("\n"));
}

const EXPECTED_SYNTHETIC: &str = "
box ftyp 0..24 structure \"heic\"
box meta 24..928 structure
  box hdlr 36..69 skipped \"pict\"
  box dinf 69..105 skipped
    box dref 77..105 skipped
      box url  93..105 skipped
  box pitm 105..119 structure
  box iloc 119..263 structure
  box iinf 263..482 structure
    item 0x1 277..305 structure \"Primary\"
    item 0x2 305..326 structure \"Exif\"
    item 0x3 326..370 structure \"XMP\"
    item 0x4 370..407 skipped \"notes\"
    item 0x5 407..435 unknown \"private\"
    item 0x6 435..456 skipped \"grid\"
    item 0x7 456..482 skipped \"Thumb\"
  box iref 482..536 structure
    box cdsc 494..508 dropped
    box thmb 508..522 dropped
    box zRef 522..536 dropped
  box iprp 536..847 structure
    box ipco 544..815 structure
      property hvcC 552..595 structure
        coded-unit 0x27 589..595 skipped \"PREFIX_SEI_NUT\"
          coded-unit 0x5 593..595 malformed \"user_data_unregistered\"
      property ispe 595..618 structure
        gap - 615..618 dropped
      property colr 618..637 dropped \"nclx\"
      property colr 637..665 unknown \"rICC\"
      property irot 665..674 metadata(orientation)
      property pixi 674..690 unknown
      property zPrv 690..714 unknown
      property clli 714..726 metadata(hdr-static)
      property uuid 726..758 unknown \"30313233-3435-3637-3839-616263646566\"
      property ispe 758..778 skipped
      property colr 778..815 metadata(icc) \"prof\"
        gap - 810..815 unreferenced
    box ipma 815..847 structure
  box idat 847..866 structure
    extent 0x6 855..863 skipped \"grid\"
    gap - 863..866 unreferenced
  box grpl 866..902 skipped
    box altr 874..902 skipped
  box zMet 902..928 unknown
box free 928..953 padding
box uuid 953..992 unknown \"be7acfcb-97a9-42e8-9c71-999491e3afac\"
box mdat 992..1213 structure
  extent 0x1 1000..1112 image-data \"Primary\"
    coded-unit 0x27 1000..1050 skipped \"PREFIX_SEI_NUT\"
      coded-unit 0x5 1006..1049 skipped \"9d417a52-3b60-4f1e-8c2a-71450e93d6b8\"
    coded-unit 0x23 1050..1057 skipped \"AUD_NUT\"
    coded-unit 0x13 1057..1066 image-data \"IDR_W_RADL\"
    coded-unit 0x1 1066..1074 skipped \"TRAIL_R\"
    coded-unit 0x29 1074..1081 unknown \"RSV_NVCL41\"
    coded-unit - 1081..1085 dropped
    coded-unit 0x26 1085..1095 padding \"FD_NUT\"
    coded-unit 0x28 1095..1109 skipped \"SUFFIX_SEI_NUT\"
      coded-unit 0x4 1101..1108 skipped \"itu_t_t35 country 0xb5 provider 0x003c\"
    gap - 1109..1112 unreferenced
  gap - 1112..1117 unreferenced
  extent 0x2 1117..1133 metadata(exif) \"Exif\"
    gap - 1117..1121 structure
    gap - 1121..1123 dropped
    gap - 1123..1133 metadata(exif)
  extent 0x3 1133..1185 metadata(xmp) \"XMP\"
    gap - 1181..1185 unreferenced
  extent 0x4 1185..1195 skipped \"notes\"
  extent 0x5 1195..1203 unknown \"private\"
  extent 0x7 1203..1213 skipped \"Thumb\"
gap - 1213..1220 trailing
";

/// Insert `prefix` and `suffix` around the data of a file whose only `iloc`
/// extent is the last thing in the file, inside a final `mdat`: patches the
/// extent length and the `mdat` size. Returns the new file and the extent's
/// new range.
fn plant(file: &[u8], prefix: &[u8], suffix: &[u8]) -> (Vec<u8>, std::ops::Range<usize>) {
    let be = |b: &[u8]| b.iter().fold(0u64, |n, &x| (n << 8) | u64::from(x));
    // Top-level boxes: meta's iloc, and the last box (mdat).
    let (mut pos, mut meta, mut last) = (0usize, None, 0usize);
    while pos + 8 <= file.len() {
        let size = be(&file[pos..pos + 4]) as usize;
        if &file[pos + 4..pos + 8] == b"meta" {
            meta = Some(pos..pos + size);
        }
        last = pos;
        pos += size;
    }
    assert_eq!(pos, file.len());
    assert_eq!(&file[last + 4..last + 8], b"mdat");
    let meta = meta.unwrap();
    let mut q = meta.start + 12;
    let iloc = loop {
        let size = be(&file[q..q + 4]) as usize;
        if &file[q + 4..q + 8] == b"iloc" {
            break q;
        }
        q += size;
    };
    let version = file[iloc + 8];
    assert!(version <= 1);
    let (off_sz, len_sz) = (
        usize::from(file[iloc + 12] >> 4),
        usize::from(file[iloc + 12] & 15),
    );
    let (base_sz, idx_sz) = (
        usize::from(file[iloc + 13] >> 4),
        usize::from(file[iloc + 13] & 15),
    );
    assert_eq!(be(&file[iloc + 14..iloc + 16]), 1, "one item");
    let mut f = iloc + 16 + 2 + if version == 1 { 2 } else { 0 } + 2;
    let base = be(&file[f..f + base_sz]) as usize;
    f += base_sz;
    assert_eq!(be(&file[f..f + 2]), 1, "one extent");
    f += 2 + if version == 1 { idx_sz } else { 0 };
    let start = base + be(&file[f..f + off_sz]) as usize;
    let len_at = f + off_sz;
    let len = be(&file[len_at..len_at + len_sz]) as usize;
    assert_eq!(start + len, file.len(), "the extent ends the file");
    let new_len = len + prefix.len() + suffix.len();
    let mut out = file.to_vec();
    out[len_at..len_at + len_sz].copy_from_slice(&(new_len as u64).to_be_bytes()[8 - len_sz..]);
    let mdat_size = be(&file[last..last + 4]) as usize + prefix.len() + suffix.len();
    out[last..last + 4].copy_from_slice(&(mdat_size as u32).to_be_bytes());
    out.splice(start..start, prefix.iter().copied());
    out.extend_from_slice(suffix);
    (out, start..start + new_len)
}

/// SEI with an identifying string, filler and junk planted in a real
/// image's data change nothing the decode returns, and the inventory lists
/// each as unconsumed.
#[test]
fn planted_units_change_nothing_and_are_unconsumed() {
    let orig = std::fs::read(testdata_dir().join("features/single.heic")).unwrap();
    let sei = nal4(&planted_sei());
    let fd = nal4(&planted_fd());
    let suffix = cat(&[fd.clone(), b"JNK".to_vec()]);
    let (planted, ext) = plant(&orig, &sei, &suffix);

    let decode = |d: &[u8]| {
        heic::DecoderConfig::new()
            .decode(d, heic::PixelLayout::Rgba8)
            .unwrap()
    };
    let (a, b) = (decode(&orig), decode(&planted));
    assert_eq!((a.width, a.height), (b.width, b.height));
    assert!(a.data == b.data, "planted units changed the pixels");
    let probe = |d: &[u8]| HeicDecoderConfig::new().job().probe_full(d).unwrap();
    assert_eq!(
        format!("{:?}", probe(&orig)),
        format!("{:?}", probe(&planted))
    );

    zencodec_testkit::check_inventory(HeicDecoderConfig::new(), &planted).unwrap();
    let inv = inventory_of(HeicDecoderConfig::new(), &planted);
    let at = |start: usize, len: usize| {
        inv.parts()
            .iter()
            .find(|p| {
                p.range == (start as u64..(start + len) as u64) && p.kind == PartKind::CodedUnit
            })
            .unwrap_or_else(|| panic!("no coded unit at {start}+{len}\n{inv}"))
    };
    let sei_part = at(ext.start, sei.len());
    assert_eq!(sei_part.disposition, Disposition::Skipped);
    assert_eq!(sei_part.label.as_deref(), Some("PREFIX_SEI_NUT"));
    let msg = inv
        .parts()
        .iter()
        .find(|p| p.label.as_deref() == Some(PLANTED_UUID_STR))
        .unwrap_or_else(|| panic!("no part labelled with the planted UUID\n{inv}"));
    assert_eq!(msg.disposition, Disposition::Skipped);
    assert!(
        bytes(&planted, msg)
            .windows(20)
            .any(|w| w == b"encoder=planted-id-7")
    );
    let fd_at = ext.end - suffix.len();
    assert_eq!(at(fd_at, fd.len()).disposition, Disposition::Padding);
    let junk = inv
        .parts()
        .iter()
        .find(|p| p.range == ((ext.end - 3) as u64..ext.end as u64))
        .unwrap_or_else(|| panic!("no part for the junk\n{inv}"));
    assert_eq!(junk.disposition, Disposition::Unreferenced);
    // No consumed leaf covers a planted byte.
    let planted_ranges = [ext.start..ext.start + sei.len(), fd_at..ext.end];
    let mut has_child = vec![false; inv.parts().len()];
    for p in inv.parts() {
        if let Some(q) = p.parent {
            has_child[q.index()] = true;
        }
    }
    for (i, p) in inv.parts().iter().enumerate() {
        if has_child[i] || !p.disposition.is_consumed() {
            continue;
        }
        for r in &planted_ranges {
            assert!(
                p.range.end <= r.start as u64 || p.range.start >= r.end as u64,
                "consumed part {p:?} covers planted bytes {r:?}"
            );
        }
    }
}

// ── Caller-gated corpus and oracle runs ─────────────────────────────────────

fn corpus_dir() -> Option<PathBuf> {
    std::env::var_os("HEIC_INVENTORY_CORPUS").map(PathBuf::from)
}

/// `HEIC_INVENTORY_CORPUS=<dir>`: every HEIF/AVIF file under it.
#[test]
fn corpus_inventories_conform() {
    let Some(dir) = corpus_dir() else {
        eprintln!("HEIC_INVENTORY_CORPUS unset: corpus run not requested (just inventory-corpus)");
        return;
    };
    let files = files_under(&dir);
    assert!(!files.is_empty(), "no HEIF files under {}", dir.display());
    let mut undecodable = 0;
    let mut totals: std::collections::BTreeMap<&'static str, u64> = Default::default();
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let name = path.display().to_string();
        check_file(&HeicDecoderConfig::new(), &data, &name, &mut undecodable);
        let inv = inventory_of(HeicDecoderConfig::new(), &data);
        for (d, n) in inv.bytes_by_disposition() {
            *totals.entry(d).or_default() += n;
        }
    }
    eprintln!(
        "{} files checked ({undecodable} without image data); bytes by disposition: {totals:?}",
        files.len()
    );
}

/// One unit `exiftool -v3` lists: its type and payload position.
#[derive(Debug)]
struct OracleUnit {
    fourcc: String,
    payload_at: u64,
    payload_len: u64,
}

/// `- Tag 'ftyp' (20 bytes):` followed by `0008: 68 65 …`: four-character box
/// names with the absolute offset of their payload.
fn exiftool_units(exiftool: &Path, file: &Path) -> Vec<OracleUnit> {
    let out = std::process::Command::new(exiftool)
        .arg("-v3")
        .arg(file)
        .output()
        .unwrap_or_else(|e| panic!("running {}: {e}", exiftool.display()));
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().collect();
    let mut units = Vec::new();
    // Nesting depth = the number of `|` columns before the line's content.
    let depth = |l: &str| {
        l.chars()
            .take_while(|c| *c == ' ' || *c == '|')
            .filter(|c| *c == '|')
            .count()
    };
    // exiftool also walks inside an embedded ICC profile, whose tags are
    // four-character signatures but not boxes.
    let mut inside_icc: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let d = depth(line);
        if let Some(icc) = inside_icc {
            if d > icc {
                continue;
            }
            inside_icc = None;
        }
        if line.contains("+ [ICC_Profile directory") {
            inside_icc = Some(d);
            continue;
        }
        let Some(at) = line.find("- Tag '") else {
            continue;
        };
        let rest = &line[at + 7..];
        let Some(q) = rest.find('\'') else {
            continue;
        };
        let name = &rest[..q];
        if name.chars().count() != 4 {
            continue;
        }
        let Some(len) = rest[q..]
            .strip_prefix("' (")
            .and_then(|r| r.split(' ').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        if len == 0 {
            continue;
        }
        let Some(hex) = lines.get(i + 1).and_then(|l| {
            let t = l.trim_start_matches([' ', '|']);
            let colon = t.find(':')?;
            u64::from_str_radix(&t[..colon], 16).ok()
        }) else {
            continue;
        };
        units.push(OracleUnit {
            fourcc: name.to_string(),
            payload_at: hex,
            payload_len: len,
        });
    }
    units
}

/// Header length of the box at `start`: 8, 16 with a 64-bit size, plus 16
/// for a `uuid` usertype when exiftool counts it as header.
fn header_len(data: &[u8], start: usize) -> u64 {
    let size32 = u32::from_be_bytes(data[start..start + 4].try_into().unwrap());
    if size32 == 1 { 16 } else { 8 }
}

/// `INVENTORY_ORACLE_EXIFTOOL=<exiftool>` + `HEIC_INVENTORY_CORPUS=<dir>`:
/// every box exiftool lists appears in the inventory with the same payload
/// offset and length.
#[test]
fn exiftool_oracle_agrees() {
    let Some(tool) = std::env::var_os("INVENTORY_ORACLE_EXIFTOOL").map(PathBuf::from) else {
        eprintln!(
            "INVENTORY_ORACLE_EXIFTOOL unset: oracle run not requested (just inventory-oracle)"
        );
        return;
    };
    let dir = corpus_dir().expect("INVENTORY_ORACLE_EXIFTOOL needs HEIC_INVENTORY_CORPUS");
    let files = files_under(&dir);
    assert!(
        files.len() >= 20,
        "the oracle run needs at least 20 files, found {}",
        files.len()
    );
    let mut rows = Vec::new();
    let mut mismatches = Vec::new();
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let inv = inventory_of(HeicDecoderConfig::new(), &data);
        let units = exiftool_units(&tool, path);
        let mut matched = 0;
        for u in &units {
            let hit = inv.parts().iter().any(|p| {
                let tag_ok = match (&p.kind, &p.tag) {
                    (PartKind::Item, _) => u.fourcc == "infe",
                    (_, PartTag::FourCc(cc)) => cc.as_slice() == u.fourcc.as_bytes(),
                    _ => false,
                };
                if !tag_ok {
                    return false;
                }
                let start = p.range.start as usize;
                let h = header_len(&data, start);
                let mut ok = p.range.start + h == u.payload_at
                    && p.range.end - u.payload_at == u.payload_len;
                if !ok && u.fourcc == "uuid" {
                    ok = p.range.start + h + 16 == u.payload_at
                        && p.range.end - u.payload_at == u.payload_len;
                }
                ok
            });
            if hit {
                matched += 1;
            } else {
                mismatches.push(format!(
                    "{}: exiftool '{}' payload {}+{} has no matching part",
                    path.display(),
                    u.fourcc,
                    u.payload_at,
                    u.payload_len
                ));
            }
        }
        let name = path
            .strip_prefix(&dir)
            .unwrap_or(path)
            .display()
            .to_string();
        rows.push(format!(
            "| {name} | {} | {matched} | {} |",
            units.len(),
            inv.parts().len()
        ));
    }
    eprintln!("| file | exiftool units | matched | inventory parts |\n|---|---|---|---|");
    for r in &rows {
        eprintln!("{r}");
    }
    eprintln!("{} mismatches", mismatches.len());
    for m in &mismatches {
        eprintln!("{m}");
    }
    assert!(
        mismatches.is_empty(),
        "{} exiftool units without a matching part",
        mismatches.len()
    );
}

// ── ffmpeg coded-unit oracle ────────────────────────────────────────────────

/// Split ffmpeg's Annex B output at start codes. NAL units never end in a
/// zero byte (the stop bit, or `cabac_zero_word`'s emulation byte), so
/// stripping zeros keeps a 4-byte start code's leading zero out of the unit.
fn annexb_units(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(d.len(), |&n| n - 3);
        while e > s && d[e - 1] == 0 {
            e -= 1;
        }
        out.push(&d[s..e]);
    }
    out
}

/// `trace_headers`: the `nal_unit_type` of every unit it decomposes, per
/// section (`Extradata`, then one per `Packet`).
fn trace_types(log: &str) -> Vec<Vec<u32>> {
    let mut out: Vec<Vec<u32>> = Vec::new();
    for line in log.lines() {
        let Some(rest) = line.split_once("] ").map(|(_, r)| r) else {
            continue;
        };
        if rest.starts_with("Extradata") || rest.starts_with("Packet:") {
            out.push(Vec::new());
        } else if rest.split_whitespace().nth(1) == Some("nal_unit_type")
            && let (Some(v), Some(cur)) = (rest.rsplit_once("= "), out.last_mut())
        {
            cur.push(v.1.trim().parse().unwrap());
        }
    }
    out
}

/// `INVENTORY_ORACLE_FFMPEG=<ffmpeg>` + `HEIC_INVENTORY_CORPUS=<dir>`: for
/// every HEVC packet ffmpeg demuxes from an item the decode reads, ffprobe's
/// packet position and size equal an extent, every NAL unit of ffmpeg's
/// Annex B output sits at a coded-unit part with the same payload offset and
/// length, and `trace_headers`' NAL unit types appear in the same order.
#[test]
fn ffmpeg_oracle_agrees() {
    use std::process::Command;
    let Some(ffmpeg) = std::env::var_os("INVENTORY_ORACLE_FFMPEG").map(PathBuf::from) else {
        eprintln!(
            "INVENTORY_ORACLE_FFMPEG unset: oracle run not requested (just inventory-oracle-ffmpeg)"
        );
        return;
    };
    let ffprobe = ffmpeg.with_file_name("ffprobe");
    let dir = corpus_dir().expect("INVENTORY_ORACLE_FFMPEG needs HEIC_INVENTORY_CORPUS");
    let files = files_under(&dir);
    let (mut opened, mut rejected, mut packets, mut other_packets) = (0, 0, 0, 0);
    let (mut nals, mut matched, mut extradata, mut extradata_matched) = (0, 0, 0, 0);
    let (mut traced, mut ours, mut from_lhvc) = (0usize, 0usize, 0usize);
    let mut failures = Vec::new();
    for path in &files {
        let data = std::fs::read(path).unwrap();
        let name = path.display().to_string();
        let inv = inventory_of(HeicDecoderConfig::new(), &data);
        let probe = Command::new(&ffprobe)
            .args(["-v", "error", "-select_streams", "v", "-show_entries"])
            .args(["packet=stream_index,pos,size", "-of", "compact=p=0"])
            .arg(path)
            .output()
            .unwrap();
        let listing = String::from_utf8_lossy(&probe.stdout);
        if !probe.status.success() || listing.trim().is_empty() {
            rejected += 1;
            continue;
        }
        opened += 1;
        // Coded-unit parts directly under extents and hvcC boxes (SEI
        // messages excluded): (range, tag).
        let unit_parts: Vec<(std::ops::Range<u64>, u32, Option<usize>)> = inv
            .parts()
            .iter()
            .filter(|p| p.kind == PartKind::CodedUnit)
            .filter(|p| {
                p.parent
                    .and_then(|q| inv.get(q))
                    .is_some_and(|q| q.kind != PartKind::CodedUnit)
            })
            .map(|p| {
                let tag = match p.tag {
                    PartTag::Code(t) => t,
                    _ => u32::MAX,
                };
                (p.range.clone(), tag, p.parent.map(|q| q.index()))
            })
            .collect();
        // A NAL unit's payload at o..o+len is a part ending there whose
        // length field (1, 2 or 4 bytes) precedes it.
        let find = |o: u64, len: u64| {
            unit_parts.iter().position(|(r, _, _)| {
                r.end == o + len && matches!(o.checked_sub(r.start), Some(1 | 2 | 4))
            })
        };
        // Packets per stream, in order: `stream_index=0|pos=5901|size=106458`
        // (ffprobe's own field order).
        let mut streams: std::collections::BTreeMap<u64, Vec<(u64, u64)>> = Default::default();
        for line in listing.lines() {
            let field = |k: &str| {
                line.split('|')
                    .find_map(|kv| kv.strip_prefix(k)?.strip_prefix('='))
                    .and_then(|v| v.parse::<u64>().ok())
            };
            if let (Some(stream), Some(pos), Some(size)) =
                (field("stream_index"), field("pos"), field("size"))
            {
                streams.entry(stream).or_default().push((pos, size));
            }
        }
        for (stream, pkts) in streams {
            // A packet is checked when it is a walked extent (one with coded
            // units): the item data the decode reads.
            let walked: Vec<Option<usize>> = pkts
                .iter()
                .map(|&(pos, size)| {
                    inv.parts()
                        .iter()
                        .position(|p| p.kind == PartKind::Extent && p.range == (pos..pos + size))
                        .filter(|&e| unit_parts.iter().any(|u| u.2 == Some(e)))
                })
                .collect();
            if walked.iter().all(Option::is_none) {
                other_packets += pkts.len();
                continue;
            }
            other_packets += walked.iter().filter(|w| w.is_none()).count();
            // `hevc_mp4toannexb` explicitly: the hevc muxer skips it when a
            // packet happens to start with bytes that look like a start code
            // (a 4-byte length of 0x0000013x).
            let out = Command::new(&ffmpeg)
                .args(["-hide_banner", "-nostats", "-v", "info", "-i"])
                .arg(path)
                .args(["-map", &format!("0:{stream}"), "-c", "copy"])
                .args([
                    "-bsf:v",
                    "trace_headers,hevc_mp4toannexb",
                    "-f",
                    "hevc",
                    "-",
                ])
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}: ffmpeg failed on stream {stream}"
            );
            let mut found: Vec<Vec<u32>> = vec![Vec::new(); pkts.len()];
            let (mut pi, mut cursor) = (0usize, 0usize);
            for nal in annexb_units(&out.stdout) {
                // The current packet from the cursor, else the next one.
                let mut hit = None;
                for (q, &(pos, size)) in pkts.iter().enumerate().skip(pi).take(2) {
                    let from = if q == pi { cursor } else { 0 };
                    let region = &data[(pos as usize + from)..(pos + size) as usize];
                    if let Some(k) = region.windows(nal.len()).position(|w| w == nal) {
                        hit = Some((q, from + k));
                        break;
                    }
                }
                match hit {
                    Some((q, k)) => {
                        (pi, cursor) = (q, k + nal.len());
                        if walked[q].is_none() {
                            continue;
                        }
                        nals += 1;
                        let o = pkts[q].0 + k as u64;
                        match find(o, nal.len() as u64) {
                            Some(i) => {
                                matched += 1;
                                found[q].push(unit_parts[i].1);
                            }
                            None => failures.push(format!(
                                "{name}: stream {stream}: NAL unit at {o}+{} has no coded-unit part",
                                nal.len()
                            )),
                        }
                    }
                    // Parameter sets ffmpeg inserts from the hvcC.
                    None => {
                        extradata += 1;
                        let at_hvcc = unit_parts.iter().any(|(r, _, _)| {
                            let (s, e) = (r.start as usize, r.end as usize);
                            e - s > nal.len()
                                && matches!(e - s - nal.len(), 1 | 2 | 4)
                                && data[e - nal.len()..e] == *nal
                        });
                        // ffmpeg also merges an `lhvC` (layered HEVC)
                        // configuration into its extradata; heic does not
                        // read lhvC, so it is one Unknown property.
                        let in_lhvc = || {
                            inv.parts().iter().any(|p| {
                                p.tag == PartTag::FourCc(*b"lhvC")
                                    && bytes(&data, p).windows(nal.len()).any(|w| w == nal)
                            })
                        };
                        if at_hvcc {
                            extradata_matched += 1;
                        } else if in_lhvc() {
                            from_lhvc += 1;
                        } else {
                            failures.push(format!(
                                "{name}: stream {stream}: inserted NAL unit ({} bytes) not found as a part",
                                nal.len()
                            ));
                        }
                    }
                }
            }
            let sections = trace_types(&String::from_utf8_lossy(&out.stderr));
            for (q, e) in walked.iter().enumerate() {
                let Some(e) = *e else { continue };
                packets += 1;
                // Every coded unit of the extent is one ffmpeg found.
                let listed = unit_parts.iter().filter(|u| u.2 == Some(e)).count();
                if listed != found[q].len() {
                    failures.push(format!(
                        "{name}: stream {stream} packet {q}: {listed} coded-unit parts, ffmpeg found {}",
                        found[q].len()
                    ));
                }
                // trace_headers' types (section 0 is the extradata) are a
                // subsequence of ours.
                let traced_types = sections.get(q + 1).cloned().unwrap_or_default();
                traced += traced_types.len();
                ours += found[q].len();
                let mut it = found[q].iter();
                if !traced_types.iter().all(|t| it.any(|x| x == t)) {
                    failures.push(format!(
                        "{name}: stream {stream} packet {q}: trace_headers types {traced_types:?}, inventory {:?}",
                        found[q]
                    ));
                }
            }
        }
    }
    eprintln!(
        "ffmpeg read {opened} of {} files ({rejected} rejected); {packets} packets of walked extents ({other_packets} other packets); \
         {matched}/{nals} packet NAL units match a coded-unit part, {extradata_matched}/{extradata} inserted parameter sets match an hvcC part ({from_lhvc} come from an lhvC property heic does not read); \
         trace_headers decomposed {traced} of {ours} units",
        files.len()
    );
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        packets >= 20,
        "the oracle needs at least 20 packets, got {packets}"
    );
}
