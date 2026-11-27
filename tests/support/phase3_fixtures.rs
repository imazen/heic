//! Assemble small HEIF containers from independently authored native sample
//! patterns. The checked-in text NALs were encoded by FFmpeg/libx265 in
//! lossless, one-frame, one-thread mode with encoder information disabled.
//! FFmpeg decoded all five streams back to their original raw sample words.

#[path = "phase3_nals.rs"]
mod nals;
pub use nals::*;

pub const W: u32 = 32;
pub const H: u32 = 16;

pub struct Item {
    pub kind: [u8; 4],
    pub data: Vec<u8>,
    pub properties: Vec<Vec<u8>>,
}

pub fn bx(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    [(data.len() as u32 + 8).to_be_bytes().as_slice(), kind, data].concat()
}

fn full(kind: &[u8; 4], version: u8, data: &[u8]) -> Vec<u8> {
    bx(kind, &[&[version, 0, 0, 0], data].concat())
}

fn hex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

pub fn image(nals: [&str; 4], w: u32, h: u32, bits: u8, chroma: u8) -> Item {
    let [vps, sps, pps, slice] = nals.map(hex);
    let mut config = vec![0u8; 23];
    config[0] = 1;
    config[16] = 0xfc | chroma;
    config[17] = 0xf8 | (bits - 8);
    config[18] = config[17];
    config[21] = 3;
    config[22] = 3;
    for nal in [vps, sps, pps] {
        config.extend([0x80 | (nal[0] >> 1), 0, 1]);
        config.extend((nal.len() as u16).to_be_bytes());
        config.extend(nal);
    }
    let mut data = (slice.len() as u32).to_be_bytes().to_vec();
    data.extend(slice);
    Item {
        kind: *b"hvc1",
        data,
        properties: vec![bx(b"hvcC", &config), ispe(w, h)],
    }
}

pub fn ispe(w: u32, h: u32) -> Vec<u8> {
    full(b"ispe", 0, &[w.to_be_bytes(), h.to_be_bytes()].concat())
}

pub fn nclx(p: u16, t: u16, m: u16, full_range: bool) -> Vec<u8> {
    bx(
        b"colr",
        &[
            b"nclx".as_slice(),
            &p.to_be_bytes(),
            &t.to_be_bytes(),
            &m.to_be_bytes(),
            &[u8::from(full_range) << 7],
        ]
        .concat(),
    )
}

pub fn auxc(urn: &[u8]) -> Vec<u8> {
    full(b"auxC", 0, &[urn, b"\0"].concat())
}

pub fn hdr_props() -> [Vec<u8>; 2] {
    let coords = [34000u16, 16000, 13250, 34500, 7500, 3000, 15635, 16450];
    let mut mastering: Vec<u8> = coords.into_iter().flat_map(u16::to_be_bytes).collect();
    mastering.extend(10_000_000u32.to_be_bytes());
    mastering.extend(50u32.to_be_bytes());
    [bx(b"clli", &[3, 232, 1, 144]), bx(b"mdcv", &mastering)]
}

pub fn aperture_x1() -> Vec<u8> {
    let values = [W - 1, 1, H, 1, 1, 2, 0, 1];
    bx(
        b"clap",
        &values
            .into_iter()
            .flat_map(u32::to_be_bytes)
            .collect::<Vec<_>>(),
    )
}

pub fn iso(multi: bool, alternate: bool, reverse: bool) -> Vec<u8> {
    let frac = |num: i32, den: u32| [num.to_be_bytes(), den.to_be_bytes()].concat();
    let flags = (u8::from(multi) << 7) | (u8::from(!alternate) << 6) | (u8::from(reverse) << 2);
    let mut data = vec![0, 0, 0, 0, 0, flags];
    let (base, alt) = if reverse { (2, 0) } else { (0, 2) };
    data.extend(frac(base, 1));
    data.extend(frac(alt, 1));
    for ch in 0..if multi { 3 } else { 1 } {
        for (num, den) in [(0, 1), (2 + ch * 2, 2), (1, 1), (0, 1), (0, 1)] {
            data.extend(frac(num, den));
        }
    }
    data
}

pub fn heif(items: &[Item], refs: &[(&[u8; 4], u16, &[u16])]) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let mut infos = (items.len() as u16).to_be_bytes().to_vec();
    let mut properties = Vec::new();
    let mut associations = (items.len() as u32).to_be_bytes().to_vec();
    let mut index = 0u8;
    for (i, item) in (1u16..).zip(items) {
        let id = i.to_be_bytes();
        infos.extend(full(
            b"infe",
            2,
            &[id.as_slice(), &[0, 0], &item.kind, &[0]].concat(),
        ));
        associations.extend(id);
        associations.push(item.properties.len() as u8);
        for prop in &item.properties {
            index += 1;
            associations.push(index | if &prop[4..8] == b"hvcC" { 0x80 } else { 0 });
            properties.extend_from_slice(prop);
        }
    }
    let iinf = full(b"iinf", 0, &infos);
    let iprp = bx(
        b"iprp",
        &[bx(b"ipco", &properties), full(b"ipma", 0, &associations)].concat(),
    );
    let iref = refs
        .iter()
        .flat_map(|(kind, from, to)| {
            let mut bytes = from.to_be_bytes().to_vec();
            bytes.extend((to.len() as u16).to_be_bytes());
            bytes.extend(to.iter().flat_map(|id| id.to_be_bytes()));
            bx(kind, &bytes)
        })
        .collect::<Vec<_>>();
    let iref = full(b"iref", 0, &iref);
    let header = |mut offset: u32| {
        let mut loc = vec![0x44, 0];
        loc.extend((items.len() as u16).to_be_bytes());
        for (id, item) in (1u16..).zip(items) {
            loc.extend(id.to_be_bytes());
            loc.extend([0, 0, 0, 0, 0, 1]);
            loc.extend(offset.to_be_bytes());
            loc.extend((item.data.len() as u32).to_be_bytes());
            offset += item.data.len() as u32;
        }
        let meta = full(
            b"meta",
            0,
            &[
                full(b"hdlr", 0, b"\0\0\0\0pict\0\0\0\0\0\0\0\0\0\0\0\0\0"),
                full(b"pitm", 0, &[0, 1]),
                full(b"iloc", 1, &loc),
                iinf.clone(),
                iref.clone(),
                iprp.clone(),
            ]
            .concat(),
        );
        [ftyp.as_slice(), &meta].concat()
    };
    let mut out = header((header(0).len() + 8) as u32);
    let data = items
        .iter()
        .flat_map(|item| item.data.iter().copied())
        .collect::<Vec<_>>();
    out.extend(bx(b"mdat", &data));
    out
}
