//! Structural inventory, review round 1 (imazen/heic#57): edits to real and
//! synthetic files that leave the decode output unchanged, each checking a
//! disposition in both directions. Bytes the decode never reads must sit in
//! unconsumed leaves; bytes it reads must sit in consumed leaves.
//!
//! Every test first checks that the edit does not change what the decode
//! returns, so the expected disposition follows from heic's behaviour, not
//! from the inventory's model of it.

#![cfg(all(feature = "zencodec", feature = "backend-rust"))]

use std::borrow::Cow;
use std::ops::Range;
use std::path::Path;

use heic::HeicDecoderConfig;
use zencodec::decode::{Decode, DecodeJob, DecoderConfig};
use zencodec::inventory::{Disposition, Inventory, MetadataKind, PartKind, PartTag};

fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("testdata")
            .join(name),
    )
    .unwrap()
}

fn inv_of(cfg: HeicDecoderConfig, data: &[u8]) -> Inventory {
    let inv = cfg.job().inventory(data).unwrap().unwrap();
    inv.validate()
        .unwrap_or_else(|e| panic!("invalid inventory: {e}\n{inv}"));
    inv
}

fn pixels(cfg: HeicDecoderConfig, data: &[u8]) -> Result<Vec<u8>, String> {
    cfg.job()
        .decoder(Cow::Borrowed(data), &[])
        .and_then(|d| d.decode())
        .map(|o| o.pixels().contiguous_bytes().to_vec())
        .map_err(|e| e.to_string())
}

fn same_pixels(a: &[u8], b: &[u8]) {
    assert_eq!(
        pixels(HeicDecoderConfig::new(), a).unwrap(),
        pixels(HeicDecoderConfig::new(), b).unwrap(),
        "the edit changed the decode output"
    );
}

/// The deepest part covering byte `at`: `(index, disposition, description)`.
fn leaf_at(inv: &Inventory, at: u64) -> (usize, Disposition, String) {
    let covers = |p: &zencodec::inventory::Part| p.range.start <= at && at < p.range.end;
    let mut has_child = vec![false; inv.parts().len()];
    for p in inv.parts() {
        if let Some(q) = p.parent
            && covers(p)
        {
            has_child[q.index()] = true;
        }
    }
    for (i, p) in inv.parts().iter().enumerate() {
        if !has_child[i] && covers(p) {
            return (
                i,
                p.disposition,
                format!(
                    "{} {} {}..{} {}",
                    p.kind.name(),
                    p.tag,
                    p.range.start,
                    p.range.end,
                    p.detail.clone().unwrap_or_default()
                ),
            );
        }
    }
    panic!("no part covers {at}");
}

/// Every byte of `r` sits in an unconsumed leaf.
fn assert_unconsumed(inv: &Inventory, r: Range<u64>, what: &str) {
    for at in r.clone() {
        let (_, d, desc) = leaf_at(inv, at);
        assert!(
            !d.is_consumed(),
            "{what}: byte {at} (of {r:?}) is in a leaf reported {d}: {desc}\n{inv}"
        );
    }
}

/// Every byte of `r` sits in a consumed leaf.
fn assert_consumed(inv: &Inventory, r: Range<u64>, what: &str) {
    for at in r.clone() {
        let (_, d, desc) = leaf_at(inv, at);
        assert!(
            d.is_consumed(),
            "{what}: byte {at} (of {r:?}) is in a leaf reported {d}: {desc}\n{inv}"
        );
    }
}

fn find(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{:?} not found", String::from_utf8_lossy(needle)))
}

// ── Minimal BMFF rewriter ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
struct Bx {
    start: usize,
    size: usize,
    hdr: usize,
    typ: [u8; 4],
}

fn boxes_in(d: &[u8], start: usize, end: usize) -> Vec<Bx> {
    let mut out = Vec::new();
    let mut p = start;
    while p + 8 <= end {
        let s32 = u32::from_be_bytes(d[p..p + 4].try_into().unwrap()) as usize;
        let typ: [u8; 4] = d[p + 4..p + 8].try_into().unwrap();
        let (size, hdr) = match s32 {
            1 => (
                u64::from_be_bytes(d[p + 8..p + 16].try_into().unwrap()) as usize,
                16,
            ),
            0 => (end - p, 8),
            n => (n, 8),
        };
        if size < hdr || p + size > end {
            break;
        }
        out.push(Bx {
            start: p,
            size,
            hdr,
            typ,
        });
        p += size;
    }
    out
}

fn child_start(d: &[u8], b: &Bx) -> usize {
    let c = b.start + b.hdr;
    match &b.typ {
        b"meta" | b"iref" => c + 4,
        b"iinf" => c + 4 + if d[c] == 0 { 2 } else { 4 },
        _ => c,
    }
}

/// The chain of boxes along `names`; `(name, n)` picks the n-th match.
fn path(d: &[u8], names: &[(&[u8; 4], usize)]) -> Vec<Bx> {
    let mut chain = Vec::new();
    let (mut s, mut e) = (0, d.len());
    for (n, k) in names {
        let b = *boxes_in(d, s, e)
            .iter()
            .filter(|b| &&b.typ == n)
            .nth(*k)
            .unwrap_or_else(|| panic!("no {}", String::from_utf8_lossy(*n)));
        s = child_start(d, &b);
        e = b.start + b.size;
        chain.push(b);
    }
    chain
}

struct Ext {
    item: u32,
    method: u8,
    base_pos: usize,
    base_size: usize,
    off_pos: usize,
    off_size: usize,
    len_pos: usize,
    len_size: usize,
}

fn rd(d: &[u8], p: usize, n: usize) -> u64 {
    d[p..p + n]
        .iter()
        .fold(0u64, |a, &b| (a << 8) | u64::from(b))
}

fn wr(d: &mut [u8], p: usize, n: usize, v: u64) {
    for i in 0..n {
        d[p + i] = (v >> (8 * (n - 1 - i))) as u8;
    }
}

fn iloc_exts(d: &[u8]) -> Vec<Ext> {
    let chain = path(d, &[(b"meta", 0), (b"iloc", 0)]);
    let b = chain[1];
    let mut p = b.start + b.hdr;
    let v = d[p];
    p += 4;
    let (off_size, len_size) = ((d[p] >> 4) as usize, (d[p] & 15) as usize);
    let base_size = (d[p + 1] >> 4) as usize;
    let idx_size = if v >= 1 { (d[p + 1] & 15) as usize } else { 0 };
    p += 2;
    let n = if v < 2 {
        let n = rd(d, p, 2);
        p += 2;
        n
    } else {
        let n = rd(d, p, 4);
        p += 4;
        n
    };
    let mut out = Vec::new();
    for _ in 0..n {
        let id_size = if v < 2 { 2 } else { 4 };
        let item = rd(d, p, id_size) as u32;
        p += id_size;
        let mut method = 0;
        if v >= 1 {
            method = d[p + 1] & 15;
            p += 2;
        }
        p += 2;
        let base_pos = p;
        p += base_size;
        let cnt = rd(d, p, 2);
        p += 2;
        for _ in 0..cnt {
            p += idx_size;
            let off_pos = p;
            p += off_size;
            let len_pos = p;
            p += len_size;
            out.push(Ext {
                item,
                method,
                base_pos,
                base_size,
                off_pos,
                off_size,
                len_pos,
                len_size,
            });
        }
    }
    out
}

/// Insert `payload` at absolute `at`, growing every box in `chain` (which
/// must all contain `at`) and shifting `iloc` file offsets at or past `at`.
fn insert(d: &[u8], chain: &[Bx], at: usize, payload: &[u8]) -> Vec<u8> {
    let mut v = d.to_vec();
    for b in chain {
        assert!(
            b.start < at && at <= b.start + b.size,
            "{b:?} does not hold {at}"
        );
        assert_eq!(b.hdr, 8);
        wr(&mut v, b.start, 4, (b.size + payload.len()) as u64);
    }
    v.splice(at..at, payload.iter().copied());
    let delta = payload.len() as u64;
    let at = at as u64;
    for e in iloc_exts(&v) {
        if e.method != 0 {
            continue;
        }
        let base = rd(&v, e.base_pos, e.base_size);
        let off = rd(&v, e.off_pos, e.off_size);
        if e.base_size > 0 && base >= at {
            continue;
        }
        if base + off >= at {
            assert!(e.off_size > 0);
            wr(&mut v, e.off_pos, e.off_size, off + delta);
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    for e in iloc_exts(&v) {
        if e.method == 0 && e.base_size > 0 && seen.insert(e.base_pos) {
            let base = rd(&v, e.base_pos, e.base_size);
            if base >= at {
                wr(&mut v, e.base_pos, e.base_size, base + delta);
            }
        }
    }
    v
}

fn set_ext_len(d: &mut [u8], item: u32, len: u64) {
    let e = iloc_exts(d).into_iter().find(|e| e.item == item).unwrap();
    wr(d, e.len_pos, e.len_size, len);
}

/// Point an item's (single) method-0 extent at absolute `start`.
fn set_ext_start(d: &mut [u8], item: u32, start: u64) {
    let e = iloc_exts(d).into_iter().find(|e| e.item == item).unwrap();
    if e.base_size > 0 {
        let off = rd(d, e.off_pos, e.off_size);
        wr(d, e.base_pos, e.base_size, start - off);
    } else {
        wr(d, e.off_pos, e.off_size, start);
    }
}

/// File range of an item's (single) method-0 extent.
fn ext_range(d: &[u8], item: u32) -> Range<u64> {
    let e = iloc_exts(d).into_iter().find(|e| e.item == item).unwrap();
    let s = rd(d, e.base_pos, e.base_size) + rd(d, e.off_pos, e.off_size);
    s..s + rd(d, e.len_pos, e.len_size)
}

fn primary_id(d: &[u8]) -> u32 {
    let p = path(d, &[(b"meta", 0), (b"pitm", 0)])[1];
    let c = p.start + p.hdr;
    if d[c] == 0 {
        rd(d, c + 4, 2) as u32
    } else {
        rd(d, c + 4, 4) as u32
    }
}

/// `(start, payload)` of each NAL unit of an `hvcC` box.
fn hvcc_nals(d: &[u8], hvcc: &Bx) -> Vec<(usize, Vec<u8>)> {
    let c = hvcc.start + hvcc.hdr;
    let mut p = c + 23;
    let mut out = Vec::new();
    for _ in 0..d[c + 22] {
        let n = rd(d, p + 1, 2);
        p += 3;
        for _ in 0..n {
            let l = rd(d, p, 2) as usize;
            out.push((p + 2, d[p + 2..p + 2 + l].to_vec()));
            p += 2 + l;
        }
    }
    out
}

fn nal_type(nal: &[u8]) -> u8 {
    (nal[0] >> 1) & 0x3F
}

// ── F1/F2: hvcC NAL units ───────────────────────────────────────────────────

fn single_hvcc() -> (Vec<u8>, Vec<Bx>) {
    let orig = fixture("features/single.heic");
    let chain = path(
        &orig,
        &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0), (b"hvcC", 0)],
    );
    (orig, chain)
}

/// heic types each hvcC NAL unit by its own header (`parse_single_nal` →
/// `decode_nal_units`), not by the array's declared type: an SEI NAL unit in
/// an array declared SPS is ignored.
#[test]
fn hvcc_sei_nal_in_sps_typed_array_is_not_consumed() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let mut array = vec![0x21u8, 0x00, 0x01];
    let nal = [&[0x4Eu8, 0x01][..], b"SECRET-SERIAL-0123456789"].concat();
    array.extend_from_slice(&(nal.len() as u16).to_be_bytes());
    array.extend_from_slice(&nal);
    let mut data = insert(&orig, &chain, hvcc.start + hvcc.size, &array);
    data[hvcc.start + hvcc.hdr + 22] += 1;
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, b"SECRET-SERIAL") as u64;
    assert_unconsumed(&inv, s..s + 24, "SEI NAL in an SPS-typed hvcC array");
}

/// The converse: the SPS, in an array whose declared type says SEI, is
/// still decoded (wiping it breaks the decode).
#[test]
fn hvcc_sps_in_sei_typed_array_is_consumed() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let c = hvcc.start + hvcc.hdr;
    let mut p = c + 23;
    let mut sps_hdr = None;
    for _ in 0..orig[c + 22] {
        let n = rd(&orig, p + 1, 2);
        if orig[p] & 0x3F == 33 {
            sps_hdr = Some(p);
        }
        p += 3;
        for _ in 0..n {
            p += 2 + rd(&orig, p, 2) as usize;
        }
    }
    let sps_hdr = sps_hdr.expect("SPS array");
    let mut data = orig.clone();
    data[sps_hdr] = (data[sps_hdr] & 0xC0) | 39;
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let r = (sps_hdr + 5) as u64..(sps_hdr + 5) as u64 + rd(&data, sps_hdr + 3, 2);
    let mut wiped = data.clone();
    wiped[r.start as usize..r.end as usize].fill(0);
    let wiped_px = pixels(HeicDecoderConfig::new(), &wiped).ok();
    assert!(
        wiped_px != Some(pixels(HeicDecoderConfig::new(), &data).unwrap()),
        "wiping the SPS must change the decode"
    );
    assert_consumed(&inv, r, "SPS NAL in an SEI-typed hvcC array");
}

/// NAL units `parse_single_nal` rejects (forbidden_zero_bit set,
/// nuh_temporal_id_plus1 = 0, shorter than the 2-byte header) are skipped,
/// whatever array holds them.
#[test]
fn hvcc_nal_units_heic_rejects_are_dropped() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let rejected: [&[u8]; 3] = [
        b"\xC0\x01FORBIDDEN-BIT", // forbidden_zero_bit set, type 32
        b"\x40\x00TEMPORAL-ID-0", // VPS header with nuh_temporal_id_plus1 = 0
        b"\x42",                  // one byte
    ];
    let mut array = vec![0x20u8, 0x00, rejected.len() as u8];
    for nal in rejected {
        array.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        array.extend_from_slice(nal);
    }
    let mut data = insert(&orig, &chain, hvcc.start + hvcc.size, &array);
    data[hvcc.start + hvcc.hdr + 22] += 1;
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let arr = hvcc.start + hvcc.size;
    for nal in rejected {
        let s = (find(&data[arr..], nal) + arr) as u64;
        assert_unconsumed(&inv, s..s + nal.len() as u64, "NAL unit heic rejects");
    }
}

/// A NAL length that runs past the hvcC: parse_hvcc skips the length field,
/// abandons that array and reads the next array header right after it, so
/// the real parameter sets that follow are still decoded.
#[test]
fn hvcc_overrunning_length_resumes_at_the_next_array() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let c = hvcc.start + hvcc.hdr;
    let at = c + 23;
    let mut data = insert(&orig, &chain, at, &[0x27, 0x00, 0x01, 0xFF, 0xFF]);
    data[c + 22] += 1;
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    for (start, nal) in hvcc_nals(&orig, &hvcc) {
        if matches!(nal_type(&nal), 33 | 34) {
            let s = (start + 5) as u64;
            assert_consumed(
                &inv,
                s..s + nal.len() as u64,
                "parameter set after the overrun",
            );
        }
    }
    let (_, d, desc) = leaf_at(&inv, (at + 3) as u64);
    assert_eq!(d, Disposition::Malformed, "{desc}");
}

/// `decode_nal_units` keeps the last SPS and PPS. With copies of them at the
/// start of the item data, the hvcC's are parsed and replaced.
#[test]
fn hvcc_parameter_sets_replaced_by_item_data_are_dropped() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let nals = hvcc_nals(&orig, &hvcc);
    let mut planted = Vec::new();
    for (_, nal) in nals.iter().filter(|(_, n)| matches!(nal_type(n), 33 | 34)) {
        planted.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        planted.extend_from_slice(nal);
    }
    let img = ext_range(&orig, 1);
    let mdat = path(&orig, &[(b"mdat", 0)]);
    let mut data = insert(&orig, &mdat, img.start as usize, &planted);
    // Point the extent back at the planted units and lengthen it.
    set_ext_start(&mut data, 1, img.start);
    set_ext_len(&mut data, 1, img.end - img.start + planted.len() as u64);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    for (start, nal) in &nals {
        let r = *start as u64..(start + nal.len()) as u64;
        match nal_type(nal) {
            33 | 34 => assert_unconsumed(&inv, r, "hvcC parameter set the item data replaces"),
            _ => assert_consumed(&inv, r, "hvcC NAL unit"),
        }
    }
    assert_consumed(
        &inv,
        img.start..img.start + planted.len() as u64,
        "parameter sets in the item data",
    );
}

/// No cap on listing ignored hvcC NAL units.
#[test]
fn hvcc_more_than_24_sei_nals_all_split() {
    let (orig, chain) = single_hvcc();
    let hvcc = *chain.last().unwrap();
    let mut array = vec![0x27u8];
    array.extend_from_slice(&30u16.to_be_bytes());
    for i in 0..30u8 {
        let nal = [&[0x4Eu8, 0x01][..], format!("PII-{i:02}").as_bytes()].concat();
        array.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        array.extend_from_slice(&nal);
    }
    let mut data = insert(&orig, &chain, hvcc.start + hvcc.size, &array);
    data[hvcc.start + hvcc.hdr + 22] += 1;
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    for i in 0..30 {
        let s = find(&data, format!("PII-{i:02}").as_bytes()) as u64;
        assert_unconsumed(&inv, s..s + 6, "SEI NAL in hvcC");
    }
}

// ── F3: item data the decode does not read ──────────────────────────────────

/// Append `tail` to the (single) extent of `item`: at the end of its
/// `idat` (method 1, the extent must end the idat) or inside the `mdat`
/// right after it (method 0).
fn grow_item(orig: &[u8], item: u32, tail: &[u8]) -> Vec<u8> {
    let e = iloc_exts(orig)
        .into_iter()
        .find(|e| e.item == item)
        .unwrap();
    let (base, off, len) = (
        rd(orig, e.base_pos, e.base_size),
        rd(orig, e.off_pos, e.off_size),
        rd(orig, e.len_pos, e.len_size),
    );
    let mut data = if e.method == 1 {
        let chain = path(orig, &[(b"meta", 0), (b"idat", 0)]);
        let idat = chain[1];
        assert_eq!(
            (base + off + len) as usize,
            idat.size - idat.hdr,
            "the item's extent ends the idat"
        );
        insert(orig, &chain, idat.start + idat.size, tail)
    } else {
        let mdat = path(orig, &[(b"mdat", 0)]);
        insert(orig, &mdat, (base + off + len) as usize, tail)
    };
    set_ext_len(&mut data, item, len + tail.len() as u64);
    data
}

/// decode.rs `decode_grid` reads 8 (or 12) descriptor bytes.
#[test]
fn grid_descriptor_tail_is_not_consumed() {
    let orig = fixture("features/grid.heic");
    let tail = b"GRID-TAIL-SECRET";
    let data = grow_item(&orig, primary_id(&orig), tail);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, tail) as u64;
    assert_unconsumed(
        &inv,
        s..s + tail.len() as u64,
        "bytes after the grid descriptor",
    );
    assert_consumed(&inv, s - 8..s, "the grid descriptor");
}

/// decode.rs `decode_iovl` reads its fields and one offset pair per input.
#[test]
fn iovl_descriptor_tail_is_not_consumed() {
    let orig = fixture("features/iovl.heic");
    let tail = b"IOVL-TAIL-SECRET";
    let data = grow_item(&orig, primary_id(&orig), tail);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, tail) as u64;
    assert_unconsumed(
        &inv,
        s..s + tail.len() as u64,
        "bytes after the overlay descriptor",
    );
    assert_consumed(&inv, s - 4..s, "the overlay descriptor's last offset");
}

/// decode.rs `decode_iden` follows `dimg` and never reads the iden item's
/// own data.
#[test]
fn iden_item_extent_is_not_consumed() {
    let orig = fixture("features/iden_rot90.heic");
    let secret = b"IDEN-ITEM-SECRET";
    let mdat = path(&orig, &[(b"mdat", 0)]);
    let data = insert(&orig, &mdat, orig.len(), secret);
    let sec_at = (data.len() - secret.len()) as u32;
    let chain = path(&data, &[(b"meta", 0), (b"iloc", 0)]);
    let iloc = chain[1];
    let c = iloc.start + iloc.hdr;
    // v0, 4-byte offsets/lengths, items 1 then 2; item 2's extent count is
    // the last two bytes of the box.
    assert_eq!(&data[c..c + 6], &[0, 0, 0, 0, 0x44, 0x00]);
    assert_eq!(
        &data[iloc.start + iloc.size - 6..iloc.start + iloc.size],
        &[0, 2, 0, 0, 0, 0]
    );
    let mut d2 = data.clone();
    d2[iloc.start + iloc.size - 1] = 1;
    let mut ext = sec_at.to_be_bytes().to_vec();
    ext.extend_from_slice(&(secret.len() as u32).to_be_bytes());
    let data = insert(&d2, &chain, iloc.start + iloc.size, &ext);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, secret) as u64;
    assert_unconsumed(&inv, s..s + secret.len() as u64, "extent of the iden item");
}

// ── F4: extents outside every mdat/idat ─────────────────────────────────────

/// heic reads method-0 extents at absolute file offsets
/// (`get_item_data`), wherever they lie.
#[test]
fn extents_outside_mdat_keep_their_item_disposition() {
    let orig = fixture("features/exif.heic");
    let mut data = orig.clone();
    let mdat = path(&orig, &[(b"mdat", 0)])[0];
    data[mdat.start + 4..mdat.start + 8].copy_from_slice(b"free");
    same_pixels(&orig, &data);
    let info = HeicDecoderConfig::new().job().probe_full(&data).unwrap();
    assert!(info.embedded_metadata.exif.is_some());
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    assert_consumed(
        &inv,
        ext_range(&data, 1),
        "primary image bytes in a free box",
    );
    zencodec_testkit::check_inventory(HeicDecoderConfig::new(), &data).unwrap();
}

/// An `mdat` whose size runs past the end of the file: heic stops reading
/// boxes there, but reads the extents inside.
#[test]
fn mdat_size_past_eof_still_lists_its_extents() {
    let orig = fixture("features/single.heic");
    let mut data = orig.clone();
    let m = path(&orig, &[(b"mdat", 0)])[0];
    data[m.start..m.start + 4].copy_from_slice(&0xFFFF_FFF0u32.to_be_bytes());
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    assert_consumed(
        &inv,
        ext_range(&data, 1),
        "primary image in an overrunning mdat",
    );
}

// ── F5: tails the caller still receives ─────────────────────────────────────

/// `extract_xmp_from_container` returns the whole item, so bytes after the
/// packet trailer reach the caller. hdr-sample.heic has some.
#[test]
fn xmp_bytes_reported_by_probe_full_are_consumed() {
    let data = fixture("apple-hdr/hdr-sample.heic");
    let info = HeicDecoderConfig::new().job().probe_full(&data).unwrap();
    let xmp = info.embedded_metadata.xmp.clone().unwrap();
    let s = find(&data, &xmp[..64.min(xmp.len())]) as u64;
    assert_eq!(&data[s as usize..s as usize + xmp.len()], &xmp[..]);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    assert_consumed(
        &inv,
        s..s + xmp.len() as u64,
        "XMP bytes probe_full reports",
    );
}

/// `parse_colr` copies everything after the colour type into the ICC the
/// caller receives, including bytes past the profile's declared size.
#[test]
fn icc_bytes_reported_by_probe_full_are_consumed() {
    let orig = fixture("apple-hdr/hdr-sample.heic");
    let chain = path(
        &orig,
        &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0), (b"colr", 0)],
    );
    let colr = *chain.last().unwrap();
    let tail = b"ICC-TAIL-SECRET";
    let data = insert(&orig, &chain, colr.start + colr.size, tail);
    let info = HeicDecoderConfig::new().job().probe_full(&data).unwrap();
    let icc = info.source_color.icc_profile.clone().unwrap();
    assert!(icc.ends_with(tail), "probe_full ICC carries the tail");
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, tail) as u64;
    assert_consumed(
        &inv,
        s..s + tail.len() as u64,
        "ICC bytes probe_full reports",
    );
    let (_, d, _) = leaf_at(&inv, s);
    assert_eq!(d, Disposition::Metadata(MetadataKind::Icc));
}

// ── F6: overlapping extents ─────────────────────────────────────────────────

#[test]
fn overlapping_extent_does_not_turn_image_data_unreferenced() {
    let orig = fixture("features/xmp.heic");
    let xmp = ext_range(&orig, 2);
    let img = ext_range(&orig, 1);
    assert_eq!(xmp.end, img.start, "layout assumption: XMP then image");
    let mut data = orig.clone();
    set_ext_len(&mut data, 2, xmp.end - xmp.start + 10);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    assert_consumed(
        &inv,
        img,
        "primary image extent overlapped by the XMP extent",
    );
    assert_consumed(&inv, xmp, "XMP extent");
}

// ── F7: bounded work ────────────────────────────────────────────────────────

/// `n` one-byte extents in the trailing bytes after `n_boxes` empty `free`
/// boxes: each extent lies outside every mdat.
fn many_extents_file(n_boxes: usize, n_extents: usize) -> Vec<u8> {
    fn bx(t: &[u8; 4], p: &[u8]) -> Vec<u8> {
        let mut v = ((p.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(t);
        v.extend_from_slice(p);
        v
    }
    fn full(t: &[u8; 4], ver: u8, p: &[u8]) -> Vec<u8> {
        let mut q = vec![ver, 0, 0, 0];
        q.extend_from_slice(p);
        bx(t, &q)
    }
    let per_item = 1024usize;
    let items = n_extents.div_ceil(per_item);
    let build = |trail_at: u32| -> Vec<u8> {
        let mut iloc = vec![0x01u8, 0x40];
        iloc.extend_from_slice(&(items as u16).to_be_bytes());
        let mut left = n_extents;
        for i in 0..items {
            let k = left.min(per_item);
            left -= k;
            iloc.extend_from_slice(&((i + 1) as u16).to_be_bytes());
            iloc.extend_from_slice(&[0, 0, 0, 0]);
            iloc.extend_from_slice(&trail_at.to_be_bytes());
            iloc.extend_from_slice(&(k as u16).to_be_bytes());
            iloc.extend(std::iter::repeat_n(1u8, k));
        }
        let mut infe_p = 1u16.to_be_bytes().to_vec();
        infe_p.extend_from_slice(&[0, 0]);
        infe_p.extend_from_slice(b"hvc1\0");
        let iinf = full(
            b"iinf",
            0,
            &[&1u16.to_be_bytes()[..], &full(b"infe", 2, &infe_p)].concat(),
        );
        let meta = full(
            b"meta",
            0,
            &[
                full(b"hdlr", 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0"),
                full(b"pitm", 0, &1u16.to_be_bytes()),
                full(b"iloc", 1, &iloc),
                iinf,
            ]
            .concat(),
        );
        let mut f = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
        f.extend(meta);
        for _ in 0..n_boxes {
            f.extend(bx(b"free", b""));
        }
        f.extend_from_slice(b"TRAILINGBYTES");
        f
    };
    let draft = build(0);
    let at = (draft.len() - 13) as u32;
    build(at)
}

struct Flag(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl enough::Stop for Flag {
    fn check(&self) -> Result<(), enough::StopReason> {
        if self.0.load(std::sync::atomic::Ordering::Relaxed) {
            Err(enough::StopReason::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// The reviewer's quadratic input (160,000 extents outside every mdat,
/// 160,000 boxes): placement was O(boxes × extents) and ignored the stop
/// token, taking 37 s in a release build. It must now finish, or honour a
/// cancellation half a second in, within a few seconds even in a debug
/// build on a loaded machine.
#[test]
fn many_extents_outside_mdat_return_promptly() {
    let n = 160_000;
    let data = many_extents_file(n, n);
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f2 = flag.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        f2.store(true, std::sync::atomic::Ordering::Relaxed);
    });
    let t = std::time::Instant::now();
    let r = HeicDecoderConfig::new()
        .job()
        .with_stop(zencodec::StopToken::new(Flag(flag)))
        .inventory(&data);
    let el = t.elapsed();
    eprintln!(
        "n={n}: inventory returned after {el:?}: {}",
        match &r {
            Ok(Some(i)) => format!("Ok, {} parts", i.parts().len()),
            Ok(None) => "None".into(),
            Err(e) => format!("Err {e}"),
        }
    );
    assert!(el.as_secs() < 30, "took {el:?}");
    // Without cancellation it completes. Every extent covers the same byte
    // of the trailing bytes (which read as a box header that overruns the
    // file, so a Malformed part), so it is listed once, under the primary
    // image (the highest-ranked use), inside that part.
    let small = many_extents_file(2_000, 2_000);
    let inv = inv_of(HeicDecoderConfig::new(), &small);
    let extents: Vec<_> = inv
        .parts()
        .iter()
        .filter(|p| p.kind == PartKind::Extent)
        .collect();
    assert_eq!(extents.len(), 1, "{inv}");
    assert_eq!(extents[0].tag, PartTag::Code(1));
    assert_eq!(extents[0].disposition, Disposition::ImageData);
    let host = inv.get(extents[0].parent.expect("nested")).unwrap();
    assert_eq!(host.disposition, Disposition::Malformed, "{inv}");
}

// ── F8/F9: properties and item entries ──────────────────────────────────────

/// Only auxiliary items' auxC is read (`find_auxiliary_items`, depth). An
/// auxC associated with the primary is parsed into `Item` and never used.
#[test]
fn primary_auxc_is_not_consumed() {
    let orig = fixture("features/single.heic");
    let ipma_chain = path(&orig, &[(b"meta", 0), (b"iprp", 0), (b"ipma", 0)]);
    let ipma = *ipma_chain.last().unwrap();
    let c = ipma.start + ipma.hdr;
    assert_eq!(rd(&orig, c + 4, 4), 1, "one ipma entry");
    assert_eq!(orig[c], 0);
    assert_eq!(orig[c + 3] & 1, 0);
    let ipco = path(&orig, &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0)])[2];
    let nprops = boxes_in(&orig, ipco.start + 8, ipco.start + ipco.size).len();
    let mut data = insert(
        &orig,
        &ipma_chain,
        ipma.start + ipma.size,
        &[(nprops + 1) as u8],
    );
    data[c + 8 + 2] += 1;
    let ipco_chain = path(&data, &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0)]);
    let ipco = *ipco_chain.last().unwrap();
    let mut auxc = vec![0u8, 0, 0, 0];
    auxc.extend_from_slice(b"urn:example:SECRET-AUXC\0PII-SUBTYPE");
    let mut auxc_box = ((auxc.len() + 8) as u32).to_be_bytes().to_vec();
    auxc_box.extend_from_slice(b"auxC");
    auxc_box.extend_from_slice(&auxc);
    let data = insert(&data, &ipco_chain, ipco.start + ipco.size, &auxc_box);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, b"urn:example:SECRET") as u64;
    assert_unconsumed(&inv, s..s + 35, "the primary's auxC");
}

/// An auxiliary item's auxC: heic matches the URN; the subtype bytes after
/// it are read only for a decoded depth map.
#[test]
fn auxc_subtype_of_an_aux_item_is_not_consumed() {
    let orig = fixture("apple-hdr/hdr-sample.heic");
    let ipco = path(&orig, &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0)]);
    let urn = b"urn:com:apple:photo:2020:aux:hdrgainmap\0";
    let auxc = boxes_in(&orig, ipco[2].start + 8, ipco[2].start + ipco[2].size)
        .into_iter()
        .find(|b| &b.typ == b"auxC" && orig[b.start..b.start + b.size].ends_with(urn))
        .expect("gain map auxC ending at its URN");
    let chain = [ipco[0], ipco[1], ipco[2], auxc];
    let secret = b"AUXC-SUBTYPE-SECRET";
    let data = insert(&orig, &chain, auxc.start + auxc.size, secret);
    same_pixels(&orig, &data);
    let probe = |d: &[u8]| {
        format!(
            "{:?}",
            HeicDecoderConfig::new().job().probe_full(d).unwrap()
        )
    };
    assert_eq!(probe(&orig), probe(&data));
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, secret) as u64;
    assert_unconsumed(&inv, s..s + secret.len() as u64, "auxC subtype");
    let u = find(&data, urn) as u64;
    assert_consumed(&inv, u..u + urn.len() as u64, "auxC URN");
}

/// `parse_infe` reads a content type only up to a NUL; without one it
/// reads an empty string.
#[test]
fn infe_content_type_without_nul_is_not_consumed() {
    let orig = fixture("features/single.heic");
    let chain = path(&orig, &[(b"meta", 0), (b"iinf", 0), (b"infe", 0)]);
    let infe = *chain.last().unwrap();
    let junk = b"SECRET-CONTENT-TYPE-NO-NUL";
    let data = insert(&orig, &chain, infe.start + infe.size, junk);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let s = find(&data, junk) as u64;
    assert_unconsumed(
        &inv,
        s..s + junk.len() as u64,
        "infe content type heic reads as empty",
    );
}

// ── F10/F11: gain-map settings ──────────────────────────────────────────────

/// With `extract_gain_map` under `ReconstructHdr`, `decode_inner` still
/// attaches the `HdrGainMap` with the gain map's XMP.
#[test]
fn reconstruct_plus_extract_attaches_gain_map_xmp() {
    let data = fixture("apple-hdr/hdr-sample.heic");
    let cfg = HeicDecoderConfig::new().with_extract_gain_map(true);
    let render = zencodec::GainMapRender::ReconstructHdr {
        target_headroom: None,
    };
    let out = cfg
        .clone()
        .job()
        .with_gain_map_render(render)
        .decoder(Cow::Borrowed(&data), &[])
        .unwrap()
        .decode()
        .unwrap();
    let gm = out
        .extras::<heic::HdrGainMap>()
        .expect("HdrGainMap attached");
    let xmp = gm.xmp.clone().expect("gain-map XMP attached");
    let s = find(&data, &xmp[..64]) as u64;
    let inv = cfg
        .job()
        .with_gain_map_render(render)
        .inventory(&data)
        .unwrap()
        .unwrap();
    inv.validate().unwrap();
    assert_consumed(
        &inv,
        s..s + xmp.len() as u64,
        "gain-map XMP attached to the output",
    );
    // ReconstructHdr alone reads that XMP and discards it.
    let alone = HeicDecoderConfig::new()
        .job()
        .with_gain_map_render(render)
        .inventory(&data)
        .unwrap()
        .unwrap();
    assert_unconsumed(
        &alone,
        s..s + xmp.len() as u64,
        "gain-map XMP under ReconstructHdr alone",
    );
}

/// The Apple gain-map parameters come from the EXIF MakerNote
/// (`apple_gain_map_params`), whatever the DecodePolicy.
#[test]
fn policy_stripped_exif_still_feeds_gain_map_params() {
    let data = fixture("apple-hdr/hdr-sample.heic");
    let policy = zencodec::decode::DecodePolicy::none().with_allow_exif(false);
    let info = HeicDecoderConfig::new()
        .job()
        .with_policy(policy)
        .probe_full(&data)
        .unwrap();
    assert!(info.embedded_metadata.exif.is_none());
    assert!(
        matches!(info.gain_map, zencodec::GainMapPresence::Available(_)),
        "gain map params reported: {:?}",
        info.gain_map
    );
    let inv = HeicDecoderConfig::new()
        .job()
        .with_policy(policy)
        .inventory(&data)
        .unwrap()
        .unwrap();
    let exif = inv
        .parts()
        .iter()
        .find(|p| p.kind == PartKind::Extent && p.label.as_deref() == Some("Exif"))
        .unwrap();
    assert_eq!(
        exif.disposition,
        Disposition::Metadata(MetadataKind::GainMap),
        "{inv}"
    );
}

// ── Synthetic files built from single.heic's HEVC data ──────────────────────

fn bx(t: &[u8; 4], p: &[u8]) -> Vec<u8> {
    let mut v = ((p.len() + 8) as u32).to_be_bytes().to_vec();
    v.extend_from_slice(t);
    v.extend_from_slice(p);
    v
}

fn full(t: &[u8; 4], ver: u8, flags: u32, p: &[u8]) -> Vec<u8> {
    let mut q = vec![ver];
    q.extend_from_slice(&flags.to_be_bytes()[1..]);
    q.extend_from_slice(p);
    bx(t, &q)
}

/// single.heic's `hvcC` box, `ispe` box and primary image data.
fn single_parts() -> (Vec<u8>, Vec<u8>, Vec<u8>, (u32, u32)) {
    let orig = fixture("features/single.heic");
    let ipco = path(&orig, &[(b"meta", 0), (b"iprp", 0), (b"ipco", 0)])[2];
    let props = boxes_in(&orig, ipco.start + 8, ipco.start + ipco.size);
    let get = |t: &[u8; 4]| {
        let b = props.iter().find(|b| &b.typ == t).unwrap();
        orig[b.start..b.start + b.size].to_vec()
    };
    let (hvcc, ispe) = (get(b"hvcC"), get(b"ispe"));
    let w = rd(&ispe, 12, 4) as u32;
    let h = rd(&ispe, 16, 4) as u32;
    let r = ext_range(&orig, 1);
    (
        hvcc,
        ispe,
        orig[r.start as usize..r.end as usize].to_vec(),
        (w, h),
    )
}

/// An image sequence: `ftyp`, then `moov` boxes (each `trak` built by
/// `traks(sample_offset)`), then an `mdat` holding the sample.
fn sequence(moovs: impl Fn(u32) -> Vec<Vec<u8>>, sample: &[u8]) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"msf1\0\0\0\0msf1hevc");
    let build = |off: u32| -> Vec<u8> {
        let mut f = ftyp.clone();
        for m in moovs(off) {
            f.extend(m);
        }
        f.extend(bx(b"mdat", sample));
        f
    };
    let draft = build(0);
    build((draft.len() - sample.len()) as u32)
}

/// One `pict` track whose single sample (`size` bytes at `off`) is an HEVC
/// image, with `entry_extra` boxes after the hvcC in its sample entry.
fn pict_trak(
    id: u32,
    (w, h): (u32, u32),
    hvcc: &[u8],
    entry_extra: &[u8],
    off: u32,
    size: u32,
) -> Vec<u8> {
    let mut tkhd = vec![0u8; 80];
    tkhd[8..12].copy_from_slice(&id.to_be_bytes());
    tkhd[72..76].copy_from_slice(&(w << 16).to_be_bytes());
    tkhd[76..80].copy_from_slice(&(h << 16).to_be_bytes());
    let mut entry = vec![0u8; 78];
    entry[6..8].copy_from_slice(&1u16.to_be_bytes());
    entry[24..26].copy_from_slice(&(w as u16).to_be_bytes());
    entry[26..28].copy_from_slice(&(h as u16).to_be_bytes());
    entry.extend_from_slice(hvcc);
    entry.extend_from_slice(entry_extra);
    let stsd = full(
        b"stsd",
        0,
        0,
        &[&1u32.to_be_bytes()[..], &bx(b"hvc1", &entry)].concat(),
    );
    let stsz = full(
        b"stsz",
        0,
        0,
        &[size.to_be_bytes(), 1u32.to_be_bytes()].concat(),
    );
    let stco = full(
        b"stco",
        0,
        0,
        &[1u32.to_be_bytes(), off.to_be_bytes()].concat(),
    );
    let stsc = full(
        b"stsc",
        0,
        0,
        &[1u32, 1, 1, 1]
            .iter()
            .flat_map(|v| v.to_be_bytes())
            .collect::<Vec<_>>(),
    );
    let stbl = bx(b"stbl", &[stsd, stsz, stco, stsc].concat());
    let minf = bx(b"minf", &stbl);
    let hdlr = full(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0");
    let mdia = bx(b"mdia", &[hdlr, minf].concat());
    bx(b"trak", &[full(b"tkhd", 0, 0, &tkhd), mdia].concat())
}

/// parser.rs `parse`: the last top-level `moov` wins. A first `moov` whose
/// hvcC is wiped does not change the decode.
#[test]
fn last_moov_wins() {
    let (hvcc, _, sample, dims) = single_parts();
    let n = sample.len() as u32;
    let mut wiped = hvcc.clone();
    wiped[8 + 23..].fill(0);
    let one = sequence(
        |off| vec![bx(b"moov", &pict_trak(1, dims, &hvcc, &[], off, n))],
        &sample,
    );
    let two = sequence(
        |off| {
            vec![
                bx(b"moov", &pict_trak(1, dims, &wiped, &[], off, n)),
                bx(b"moov", &pict_trak(1, dims, &hvcc, &[], off, n)),
            ]
        },
        &sample,
    );
    same_pixels(&one, &two);
    let inv = inv_of(HeicDecoderConfig::new(), &two);
    let first = find(&two, b"moov") - 4;
    let second = first + 8 + find(&two[first + 8..], b"moov") - 4;
    let first_hvcc = first + find(&two[first..], b"hvcC") - 4;
    let second_hvcc = second + find(&two[second..], b"hvcC") - 4;
    assert_unconsumed(
        &inv,
        (first_hvcc + 8) as u64..(first_hvcc + hvcc.len()) as u64,
        "the replaced moov's hvcC",
    );
    assert_consumed(
        &inv,
        (second_hvcc + 8) as u64..(second_hvcc + 8 + 23) as u64,
        "the last moov's hvcC",
    );
    zencodec_testkit::check_inventory(HeicDecoderConfig::new(), &two).unwrap();
}

/// parser.rs `parse_moov` reads at most 16 tracks; later ones are never
/// parsed, and tracks it parses but does not decode reach no caller.
#[test]
fn tracks_past_sixteen_are_not_read() {
    let (hvcc, _, sample, dims) = single_parts();
    let n = sample.len() as u32;
    let other = bx(
        b"trak",
        &full(
            b"tkhd",
            0,
            0,
            &[b"OTHER-TRACK-TKHD".as_slice(), &[0u8; 64]].concat(),
        ),
    );
    let late = bx(
        b"trak",
        &full(
            b"tkhd",
            0,
            0,
            &[b"LATE-TRACK-TKHD!".as_slice(), &[0u8; 64]].concat(),
        ),
    );
    let data = sequence(
        |off| {
            let mut m = pict_trak(1, dims, &hvcc, &[], off, n);
            for _ in 0..15 {
                m.extend_from_slice(&other);
            }
            m.extend_from_slice(&late);
            vec![bx(b"moov", &m)]
        },
        &sample,
    );
    let plain = sequence(
        |off| vec![bx(b"moov", &pict_trak(1, dims, &hvcc, &[], off, n))],
        &sample,
    );
    same_pixels(&plain, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let late_at = find(&data, b"LATE-TRACK-TKHD!") as u64;
    let (_, d, desc) = leaf_at(&inv, late_at);
    assert_eq!(d, Disposition::Skipped, "{desc}");
    let other_at = find(&data, b"OTHER-TRACK-TKHD") as u64;
    let (_, d, desc) = leaf_at(&inv, other_at);
    assert_eq!(d, Disposition::Dropped, "{desc}");
}

/// The decoded sample entry's `colr` is the primary's colour (codec.rs
/// reports it as CICP); an earlier `colr` there is replaced.
#[test]
fn sample_entry_colr_is_the_reported_colour() {
    let (hvcc, _, sample, dims) = single_parts();
    let n = sample.len() as u32;
    let early = bx(b"colr", b"nclx\0\x09\0\x10\0\x09\x80");
    let late = bx(b"colr", b"nclx\0\x01\0\x0d\0\x01\x80");
    let extra = [early.clone(), late.clone()].concat();
    let data = sequence(
        |off| vec![bx(b"moov", &pict_trak(1, dims, &hvcc, &extra, off, n))],
        &sample,
    );
    let info = HeicDecoderConfig::new().job().probe_full(&data).unwrap();
    let cicp = info.source_color.cicp.expect("CICP reported");
    assert_eq!(cicp.color_primaries, 1, "{info:?}");
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let colrs: Vec<_> = inv
        .parts()
        .iter()
        .filter(|p| p.tag == PartTag::FourCc(*b"colr"))
        .map(|p| p.disposition)
        .collect();
    assert_eq!(
        colrs,
        [
            Disposition::Dropped,
            Disposition::Metadata(MetadataKind::Cicp)
        ],
        "{inv}"
    );
}

/// A `tmap` primary over `[base, gain map]`, both single.heic's image, with
/// `tail` after the ISO 21496-1 payload.
fn tmap_file(tail: &[u8]) -> Vec<u8> {
    let (hvcc, ispe, sample, _) = single_parts();
    let mut params = zencodec::GainMapParams::default();
    params.alternate_hdr_headroom = 2.0;
    for ch in &mut params.channels {
        ch.max = 2.0;
    }
    let iso = zencodec::gainmap::serialize_iso21496_fmt(
        &params,
        zencodec::gainmap::Iso21496Format::AvifTmap,
    );
    assert!(
        zencodec::gainmap::parse_iso21496_fmt(&iso, zencodec::gainmap::Iso21496Format::AvifTmap)
            .is_ok()
    );
    let tmap = [iso.as_slice(), tail].concat();
    let infe = |id: u16, t: &[u8; 4]| {
        full(
            b"infe",
            2,
            0,
            &[&id.to_be_bytes()[..], &[0, 0], t, b"\0"].concat(),
        )
    };
    let iinf = full(
        b"iinf",
        0,
        0,
        &[
            3u16.to_be_bytes().to_vec(),
            infe(1, b"hvc1"),
            infe(2, b"hvc1"),
            infe(3, b"tmap"),
        ]
        .concat(),
    );
    let mut dimg = 3u16.to_be_bytes().to_vec();
    dimg.extend_from_slice(&2u16.to_be_bytes());
    dimg.extend_from_slice(&1u16.to_be_bytes());
    dimg.extend_from_slice(&2u16.to_be_bytes());
    let iref = full(b"iref", 0, 0, &bx(b"dimg", &dimg));
    let ipco = bx(b"ipco", &[hvcc, ispe].concat());
    let ipma = full(
        b"ipma",
        0,
        0,
        &[
            3u32.to_be_bytes().to_vec(),
            vec![0, 1, 2, 0x81, 2],
            vec![0, 2, 2, 0x81, 2],
            vec![0, 3, 1, 2],
        ]
        .concat(),
    );
    let iprp = bx(b"iprp", &[ipco, ipma].concat());
    let build = |d: u32| -> Vec<u8> {
        let s = sample.len() as u32;
        let mut iloc = vec![0x44u8, 0x00];
        iloc.extend_from_slice(&3u16.to_be_bytes());
        for (id, off, len) in [(1u16, d, s), (2, d, s), (3, d + s, tmap.len() as u32)] {
            iloc.extend_from_slice(&id.to_be_bytes());
            iloc.extend_from_slice(&[0, 0, 0, 0]);
            iloc.extend_from_slice(&1u16.to_be_bytes());
            iloc.extend_from_slice(&off.to_be_bytes());
            iloc.extend_from_slice(&len.to_be_bytes());
        }
        let meta = full(
            b"meta",
            0,
            0,
            &[
                full(b"hdlr", 0, 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0"),
                full(b"pitm", 0, 0, &3u16.to_be_bytes()),
                full(b"iloc", 1, 0, &iloc),
                iinf.clone(),
                iref.clone(),
                iprp.clone(),
            ]
            .concat(),
        );
        let mut f = bx(b"ftyp", b"heic\0\0\0\0mif1heictmap");
        f.extend(meta);
        f.extend(bx(b"mdat", &[sample.as_slice(), &tmap].concat()));
        f
    };
    let draft = build(0);
    let mdat = draft.len() - sample.len() - tmap.len();
    build(mdat as u32)
}

/// The `tmap` payload past the ISO 21496-1 metadata: attached whole as
/// `HdrGainMap::iso21496` with `extract_gain_map`, dropped otherwise.
#[test]
fn tmap_tail_reaches_the_caller_only_when_attached() {
    let tail = b"TMAP-TAIL-SECRET";
    let data = tmap_file(tail);
    let s = find(&data, tail) as u64;
    let r = s..s + tail.len() as u64;

    let plain = inv_of(HeicDecoderConfig::new(), &data);
    assert_unconsumed(&plain, r.clone(), "tmap tail, gain map only described");
    let info = HeicDecoderConfig::new().job().probe_full(&data).unwrap();
    assert!(
        matches!(info.gain_map, zencodec::GainMapPresence::Available(_)),
        "{:?}",
        info.gain_map
    );

    let cfg = HeicDecoderConfig::new().with_extract_gain_map(true);
    let out = cfg
        .clone()
        .job()
        .decoder(Cow::Borrowed(&data), &[])
        .unwrap()
        .decode()
        .unwrap();
    let gm = out
        .extras::<heic::HdrGainMap>()
        .expect("HdrGainMap attached");
    assert!(
        gm.iso21496.as_deref().is_some_and(|b| b.ends_with(tail)),
        "the attached payload carries the tail"
    );
    let attached = inv_of(cfg, &data);
    assert_consumed(
        &attached,
        r.clone(),
        "tmap tail attached as HdrGainMap::iso21496",
    );
    let (_, d, _) = leaf_at(&attached, s);
    assert_eq!(d, Disposition::Metadata(MetadataKind::GainMap));
}

// ── F14: superseded and unused structure ────────────────────────────────────

/// heic parses every top-level meta and appends; a second meta that only
/// repeats item 1 is superseded entry by entry (`get_item`/`get_item_data`
/// use the first).
#[test]
fn second_meta_repeating_items_is_dropped() {
    let orig = fixture("features/single.heic");
    let meta = path(&orig, &[(b"meta", 0)])[0];
    let data = [&orig[..], &orig[meta.start..meta.start + meta.size]].concat();
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let second = (orig.len()) as u64;
    for typ in [b"ipma", b"iloc"] {
        let p = inv
            .parts()
            .iter()
            .find(|p| p.tag == PartTag::FourCc(*typ) && p.range.start >= second)
            .unwrap();
        assert_eq!(p.disposition, Disposition::Dropped, "{p:?}");
    }
}

/// An `iref` with a 64-bit size field and version 1 (4-byte item IDs): the
/// version byte follows the 16-byte header.
#[test]
fn iref_version_after_a_64_bit_header() {
    let orig = fixture("features/grid.heic");
    let chain = path(&orig, &[(b"meta", 0), (b"iref", 0)]);
    let iref = chain[1];
    assert_eq!(orig[iref.start + iref.hdr], 0, "version 0 iref");
    // Rebuild every entry with 4-byte IDs.
    let mut entries = Vec::new();
    for child in boxes_in(&orig, iref.start + iref.hdr + 4, iref.start + iref.size) {
        let c = &orig[child.start + 8..child.start + child.size];
        let mut p = 0;
        let mut v1 = Vec::new();
        while p + 4 <= c.len() {
            v1.extend_from_slice(&(rd(c, p, 2) as u32).to_be_bytes());
            let n = rd(c, p + 2, 2) as usize;
            v1.extend_from_slice(&(n as u16).to_be_bytes());
            p += 4;
            for _ in 0..n {
                v1.extend_from_slice(&(rd(c, p, 2) as u32).to_be_bytes());
                p += 2;
            }
        }
        // Three bytes too short for another entry: the tail split depends
        // on reading the version (and so the ID width) right.
        if &child.typ == b"dimg" {
            v1.extend_from_slice(b"XYZ");
        }
        entries.extend(bx(&child.typ, &v1));
    }
    let payload = [&[1u8, 0, 0, 0][..], &entries].concat();
    let mut big = 1u32.to_be_bytes().to_vec();
    big.extend_from_slice(b"iref");
    big.extend_from_slice(&((payload.len() + 16) as u64).to_be_bytes());
    big.extend_from_slice(&payload);
    // The old iref becomes padding; the new one follows it.
    let mut data = insert(&orig, &chain[..1], iref.start + iref.size, &big);
    data[iref.start + 4..iref.start + 8].copy_from_slice(b"free");
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let dimg = inv
        .parts()
        .iter()
        .enumerate()
        .find(|(_, p)| p.tag == PartTag::FourCc(*b"dimg") && p.range.start > iref.start as u64)
        .unwrap();
    assert_eq!(dimg.1.disposition, Disposition::Structure, "{inv}");
    let tail: Vec<_> = inv
        .parts()
        .iter()
        .filter(|p| p.parent.is_some_and(|q| q.index() == dimg.0))
        .map(|p| (p.range.clone(), p.disposition))
        .collect();
    let end = dimg.1.range.end;
    assert_eq!(
        tail,
        [(end - 3..end, Disposition::Dropped)],
        "only the 3 bytes after the last whole entry are unread\n{inv}"
    );
}

/// A `dimg` reference from an item nothing decodes is parsed and unused.
#[test]
fn dimg_from_an_unused_item_is_dropped() {
    let orig = fixture("features/grid.heic");
    let chain = path(&orig, &[(b"meta", 0), (b"iref", 0)]);
    let iref = chain[1];
    assert_eq!(orig[iref.start + iref.hdr], 0, "version 0 iref");
    // From item 0x7777 (no such item) to item 1.
    let entry = bx(b"dimg", &[0x77, 0x77, 0, 1, 0, 1]);
    let data = insert(&orig, &chain, iref.start + iref.size, &entry);
    same_pixels(&orig, &data);
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    let at = find(&data[iref.start..], &entry) + iref.start;
    let (_, d, desc) = leaf_at(&inv, at as u64 + 8);
    assert_eq!(d, Disposition::Dropped, "{desc}");
}

/// Peak-memory probe (F13): `HEIC_INVENTORY_MEM_BOXES` empty top-level
/// `free` boxes (default 10,000). `just inventory-memory` runs it in release
/// under `/usr/bin/time -v` with 900,000 boxes, the review's input.
#[test]
fn many_boxes_inventory() {
    let n: usize = std::env::var("HEIC_INVENTORY_MEM_BOXES")
        .map(|v| v.parse().unwrap())
        .unwrap_or(10_000);
    let data = many_extents_file(n, 0);
    let t = std::time::Instant::now();
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    eprintln!(
        "n={n}: {} bytes -> {} parts in {:?}",
        data.len(),
        inv.parts().len(),
        t.elapsed()
    );
    assert!(inv.parts().len() > n);
}

/// Timing probe (F7): the review's input with `HEIC_INVENTORY_EXTENTS`
/// boxes and extents (default 2,000), no cancellation. `just
/// inventory-extents` runs it in release with 160,000.
#[test]
fn many_extents_inventory() {
    let n: usize = std::env::var("HEIC_INVENTORY_EXTENTS")
        .map(|v| v.parse().unwrap())
        .unwrap_or(2_000);
    let data = many_extents_file(n, n);
    let t = std::time::Instant::now();
    let inv = inv_of(HeicDecoderConfig::new(), &data);
    eprintln!(
        "n={n}: {} bytes -> {} parts in {:?}",
        data.len(),
        inv.parts().len(),
        t.elapsed()
    );
}

/// Round 2 (N1, from the round-2 review): parser.rs `parse_meta` returns at
/// the first child-parser error, so when `parse_iloc` rejects the file (1,025
/// extents on one item, one past heic's cap) the `iinf`, `iprp`, `ipco` and
/// `ipma` after the `iloc` are never read. Overwriting the `ipma` payload
/// leaves decode and probe unchanged (both still reject), so nothing after
/// the `iloc` may be reported consumed.
#[test]
fn r2_boxes_after_a_rejecting_iloc_are_not_read() {
    let orig = fixture("features/single.heic");
    let chain = path(&orig, &[(b"meta", 0), (b"iloc", 0)]);
    let p = chain[1].start + chain[1].hdr;
    let v = orig[p];
    let idx_size = if v >= 1 {
        (orig[p + 5] & 15) as usize
    } else {
        0
    };
    let e = iloc_exts(&orig).into_iter().find(|e| e.item == 1).unwrap();
    let entry = idx_size + e.off_size + e.len_size;
    let mut d = orig.clone();
    wr(&mut d, e.off_pos - idx_size - 2, 2, 1025);
    let d = insert(&d, &chain, e.len_pos + e.len_size, &vec![0u8; entry * 1024]);
    let ipma = path(&d, &[(b"meta", 0), (b"iprp", 0), (b"ipma", 0)])[2];
    let mut m = d.clone();
    let body = ipma.start + ipma.hdr..ipma.start + ipma.size;
    let secret = b"IPMA-SECRET-PII-0123456789";
    for (i, b) in m[body.clone()].iter_mut().enumerate() {
        *b = secret[i % secret.len()];
    }
    let err = |x: &[u8]| pixels(HeicDecoderConfig::new(), x).unwrap_err();
    assert_eq!(err(&d), err(&m), "decode unchanged");
    let probe = |x: &[u8]| {
        HeicDecoderConfig::new()
            .job()
            .probe_full(x)
            .map(|i| format!("{i:?}"))
            .map_err(|e| e.to_string())
    };
    assert_eq!(probe(&d), probe(&m), "probe_full unchanged");
    let inv = inv_of(HeicDecoderConfig::new(), &m);
    let r = body.start as u64..body.end as u64;
    assert_unconsumed(&inv, r, "ipma payload heic never reads");
    // Every part after the iloc is unconsumed; the iloc itself is where
    // heic stops.
    let iloc_end = path(&m, &[(b"meta", 0), (b"iloc", 0)])[1];
    let iloc_end = (iloc_end.start + iloc_end.size) as u64;
    for p in inv.parts() {
        if p.range.start >= iloc_end {
            assert!(
                !p.disposition.is_consumed(),
                "part after the rejecting iloc reported {}: {p:?}\n{inv}",
                p.disposition
            );
        }
    }
}
