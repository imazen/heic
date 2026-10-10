//! Structural inventory of a HEIF/HEIC file, for zencodec's
//! `DecodeJob::inventory`.
//!
//! [`inventory`] walks every ISOBMFF box without decoding pixels and records
//! what the zencodec decode path (`codec.rs`, `HeicDecoder::decode_inner`)
//! does with each byte. Box headers are read here. The contents of `infe`,
//! `iloc` and every `ipco` property go through the same parser functions
//! `heif::parse` uses, and the rules for which items reach the caller
//! replicate the decode path; each rule cites the code it mirrors.
//!
//! Shape of the result:
//! - every box is a `Box` part, recursively for container boxes (`meta`,
//!   `iinf`, `iprp`, `ipco`, `iref`, `dinf`, `grpl`, `moov`/`trak`/…), and
//!   each container declares a `body` its children tile;
//! - every `infe` is an `Item` part tagged with its item ID;
//! - every `ipco` child is a `Property` part;
//! - every `iloc` extent is an `Extent` part inside the `mdat` or `idat` that
//!   holds it, and the other `mdat`/`idat` bytes are gaps. Extents that
//!   overlap, have zero length or point outside every `mdat`/`idat` are listed
//!   in the `iloc` part's detail instead;
//! - bytes after the last box are `Trailing`; a box that runs past its
//!   container, or bytes inside a container too short for a box header, are
//!   `Malformed`.
//!
//! Dispositions describe what the decode reads when it runs. The inventory
//! does not predict whether the decode succeeds; when `heif::parse` rejects
//! the file outright, nothing reaches the caller and every item is `Dropped`.

use alloc::borrow::Cow;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::ops::Range;

use enough::Stop;
use whereat::{At, at};
use zencodec::ImageFormat;
use zencodec::inventory::{
    DEFAULT_MAX_PARTS, Disposition, Inventory, InventoryError, MetadataKind, Part, PartId,
    PartKind, PartTag,
};

use crate::error::{HeicError, check_stop};
use crate::heif::{
    self, BmffBox, BoxHeader, ColorInfo, FourCC, HeifContainer, ItemInfo, ItemLocation,
    ItemProperty, ItemType,
};

/// Containers nested deeper than this are reported whole, not walked.
const MAX_DEPTH: usize = 32;
/// Labels are copied from the file, at most this many bytes.
const MAX_LABEL: usize = 64;
/// Remarks kept per part; the rest are counted.
const MAX_NOTES: usize = 24;
/// The remark on an image-data extent whose coded units are not listed.
const NOT_WALKED: &str = "coded-unit framing inside is not walked: bytes after the last NAL/OBU unit are not distinguished";

const APPLE_GAIN_MAP_URN: &str = "urn:com:apple:photo:2020:aux:hdrgainmap";
const ALPHA_URNS: [&str; 2] = [
    "urn:mpeg:hevc:2015:auxid:1",
    "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
];
const DEPTH_URNS: [&str; 2] = [
    "urn:mpeg:hevc:2015:auxid:2",
    "urn:mpeg:mpegB:cicp:systems:auxiliary:depth",
];

/// The parts of the job's configuration that change what the decode reads.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Options {
    /// `GainMapRender::ReconstructHdr`: the gain map is decoded and applied
    /// to the base (codec.rs `reconstruct_hdr_base`).
    pub(crate) apply_gain_map: bool,
    /// `extract_gain_map` or `GainMapRender::Components`: the decoded gain
    /// map is attached to the output as `HdrGainMap`, with its XMP and the
    /// whole `tmap` payload (codec.rs `decode_inner`). Independent of
    /// `apply_gain_map`: both can hold at once.
    pub(crate) attach_gain_map: bool,
    /// `HeicDecodeJob::extract_depth`.
    pub(crate) decode_depth: bool,
    /// `DecodePolicy` keeps these (codec.rs `apply_policy`).
    pub(crate) keep_icc: bool,
    pub(crate) keep_exif: bool,
    pub(crate) keep_xmp: bool,
}

impl Options {
    /// The gain-map image is decoded (applied, attached or both).
    fn decode_gain_map(&self) -> bool {
        self.apply_gain_map || self.attach_gain_map
    }
}

impl Default for Options {
    fn default() -> Self {
        Self {
            apply_gain_map: false,
            attach_gain_map: false,
            decode_depth: false,
            keep_icc: true,
            keep_exif: true,
            keep_xmp: true,
        }
    }
}

/// Map every structural part of `data`. Errors only on cancellation or when
/// the part cap is reached.
pub(crate) fn inventory(
    data: &[u8],
    opts: &Options,
    stop: &dyn Stop,
) -> Result<Inventory, At<HeicError>> {
    let mut w = Walker::new(data, stop)?;
    w.walk(None, 0, data.len() as u64, Ctx::Top, 0)?;

    let parsed = match heif::parse(data, stop) {
        Ok(c) => Ok(c),
        Err(e) if matches!(e.error(), HeicError::Cancelled(_)) => return Err(e),
        Err(e) => Err(e.error().to_string()),
    };
    match &parsed {
        Ok(c) => {
            let model = Model::build(c, opts);
            w.resolve(Some((c, &model)))?;
        }
        Err(why) => {
            let why = format!("heic::heif::parse rejects this file ({why}); nothing is decoded");
            let target = w
                .top_metas
                .first()
                .copied()
                .or(w.ftyp)
                .or(if w.nodes.is_empty() { None } else { Some(0) });
            if let Some(t) = target {
                w.note(t, why);
            }
            w.resolve(None)?;
        }
    }
    w.emit()
}

// ── Box walk ─────────────────────────────────────────────────────────────

/// One part, before it is pushed into the [`Inventory`].
struct Node {
    parent: Option<usize>,
    kind: PartKind,
    tag: PartTag,
    label: Option<String>,
    range: Range<u64>,
    body: Option<Range<u64>>,
    disp: Disposition,
    notes: Vec<String>,
    dropped_notes: usize,
    /// For `mdat`/`idat`: the disposition of body bytes no extent covers.
    gap_fill: Option<Disposition>,
    /// Sub-ranges past an internal end (a tail after the fields its parser
    /// reads, bytes past a declared size), added as child parts when the
    /// part ends up consumed. `None`: the bytes still reach the caller with
    /// the part, so the child takes the part's disposition.
    inner: Vec<Inner>,
}

/// A child range of a consumed leaf: `(range, disposition, remark)`, with
/// `None` for "the part's own disposition".
type Inner = (Range<u64>, Option<Disposition>, String);

/// Where a box sits, which decides how heic treats it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ctx {
    Top,
    /// Children of a top-level `meta`; `heif::parse` reads these
    /// (parser.rs `parse_meta`).
    Meta,
    Iinf,
    Iprp,
    Ipco,
    Iref,
    /// A `moov` subtree (parser.rs `parse_moov`).
    Moov {
        track: MoovUse,
    },
    Stsd {
        used: bool,
    },
    SampleEntry {
        used: bool,
    },
    /// A subtree heic never reads.
    Unread,
}

/// What heic does with the boxes of a `moov` subtree. heic reads only the
/// last top-level `moov`, and only when the file has no top-level `meta`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MoovUse {
    /// Directly inside the `moov` heic reads: only `trak` children are read.
    Top,
    /// Inside the track whose first sync sample heic decodes.
    Primary,
    /// Inside a track `parse_trak` reads, but nothing of it reaches the
    /// caller (another track, the thumbnail track, or one it rejects).
    Parsed,
    /// Never read.
    Unread,
}

/// How a box's children are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Layout {
    Leaf,
    /// Children start right after the header.
    Plain,
    /// FullBox: 4 bytes of version and flags, then children.
    Full,
    /// FullBox plus a `u32` entry count (`dref`, `stsd`).
    FullCount,
    /// `iinf`: FullBox plus a `u16` (version 0) or `u32` entry count.
    Iinf,
    /// A visual sample entry: 78 bytes of fixed fields, then children.
    VisualSampleEntry,
    /// An audio sample entry: 28 bytes of fixed fields, then children.
    AudioSampleEntry,
    /// A `meta` outside the top level: QuickTime-style (no version/flags)
    /// when `hdlr` follows the header directly, ISO FullBox otherwise.
    NestedMeta,
    /// `mdat`/`idat`: the payload holds item extents.
    Data,
}

struct Hdr {
    typ: [u8; 4],
    /// 8, or 16 with a 64-bit size: what heic's `BoxIterator` skips.
    base_header: u64,
    /// `base_header` plus 16 for a `uuid` usertype.
    header: u64,
    end: u64,
    usertype: Option<[u8; 16]>,
}

enum HdrOutcome {
    Ok(Hdr),
    /// Fewer bytes than a box header needs.
    Short,
    /// The size field is smaller than the header.
    BadSize {
        typ: [u8; 4],
        size: u64,
    },
    /// The box runs past its container.
    Overrun {
        typ: [u8; 4],
        declared: u64,
    },
}

fn be_u32(data: &[u8], at: u64) -> Option<u32> {
    let at = usize::try_from(at).ok()?;
    let b = data.get(at..at.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

fn be_u16(data: &[u8], at: u64) -> Option<u16> {
    let at = usize::try_from(at).ok()?;
    let b = data.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

fn fourcc_at(data: &[u8], at: u64) -> Option<[u8; 4]> {
    let at = usize::try_from(at).ok()?;
    let b = data.get(at..at.checked_add(4)?)?;
    Some([b[0], b[1], b[2], b[3]])
}

fn slice(data: &[u8], r: Range<u64>) -> &[u8] {
    let (Ok(s), Ok(e)) = (usize::try_from(r.start), usize::try_from(r.end)) else {
        return &[];
    };
    data.get(s..e).unwrap_or(&[])
}

/// Same size rules as heic's `BoxIterator` (heif/boxes.rs), plus the `uuid`
/// usertype, which heic does not read.
fn read_header(data: &[u8], pos: u64, limit: u64) -> HdrOutcome {
    let avail = limit.saturating_sub(pos);
    if avail < 8 {
        return HdrOutcome::Short;
    }
    let (Some(size32), Some(typ)) = (be_u32(data, pos), fourcc_at(data, pos + 4)) else {
        return HdrOutcome::Short;
    };
    let (size, base_header) = match size32 {
        1 => {
            if avail < 16 {
                return HdrOutcome::Short;
            }
            let (Some(hi), Some(lo)) = (be_u32(data, pos + 8), be_u32(data, pos + 12)) else {
                return HdrOutcome::Short;
            };
            ((u64::from(hi) << 32) | u64::from(lo), 16)
        }
        0 => (avail, 8),
        n => (u64::from(n), 8),
    };
    if size < base_header {
        return HdrOutcome::BadSize { typ, size };
    }
    if size > avail {
        return HdrOutcome::Overrun {
            typ,
            declared: size,
        };
    }
    let mut header = base_header;
    let mut usertype = None;
    if &typ == b"uuid" && size >= base_header + 16 {
        let u = slice(data, pos + base_header..pos + base_header + 16);
        let mut out = [0u8; 16];
        if u.len() == 16 {
            out.copy_from_slice(u);
            usertype = Some(out);
            header += 16;
        }
    }
    HdrOutcome::Ok(Hdr {
        typ,
        base_header,
        header,
        end: pos + size,
        usertype,
    })
}

fn fourcc_str(cc: &[u8; 4]) -> String {
    if cc.iter().all(|b| (0x20..0x7f).contains(b)) {
        cc.iter().map(|&b| b as char).collect()
    } else {
        format!("0x{:08X}", u32::from_be_bytes(*cc))
    }
}

/// A label copied from the file: at most [`MAX_LABEL`] bytes, up to the
/// first NUL, invalid UTF-8 replaced.
fn label_from(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    let b = &bytes[..end.min(MAX_LABEL)];
    if b.is_empty() {
        return None;
    }
    Some(String::from_utf8_lossy(b).into_owned())
}

fn uuid_str(u: &[u8; 16]) -> String {
    let mut s = String::with_capacity(36);
    for (i, b) in u.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            s.push('-');
        }
        s.push_str(&format!("{b:02x}"));
    }
    s
}

struct InfeRec {
    node: usize,
    info: Option<ItemInfo>,
    /// `parse_iinf` accepted it (it stops after `entry_count` entries).
    taken: bool,
}

struct IlocRec {
    node: usize,
    /// Box size and payload range, to re-parse in the second pass.
    size: u64,
    content: Range<u64>,
}

struct PropRec {
    node: usize,
    /// 0-based index in `HeifContainer::properties` (ipma index − 1).
    index: usize,
    typ: [u8; 4],
    prop: ItemProperty,
    /// The `colr` colour type, when the property is a `colr`.
    colour_type: Option<[u8; 4]>,
}

struct Walker<'a> {
    data: &'a [u8],
    stop: &'a dyn Stop,
    nodes: Vec<Node>,
    max_nodes: usize,
    has_top_meta: bool,
    ftyp: Option<usize>,
    top_metas: Vec<usize>,
    moovs: Vec<usize>,
    mdats: Vec<usize>,
    /// `(node, inside a top-level meta)`.
    idats: Vec<(usize, bool)>,
    pitms: Vec<usize>,
    infes: Vec<InfeRec>,
    ilocs: Vec<IlocRec>,
    props: Vec<PropRec>,
    /// `iref` children: node, reference type, the `iref` version.
    irefs: Vec<(usize, [u8; 4], u8)>,
    /// `ipma` boxes `heif::parse` reads, in file order.
    ipmas: Vec<usize>,
    /// The first `hvc1`/`hev1` entry of an `stsd` (parser.rs
    /// `parse_stsd_track` uses only that one).
    stsd_hevc_taken: bool,
    /// Start of the top-level `moov` heic reads: the last one, when the
    /// file has no top-level `meta` (parser.rs `parse`).
    used_moov_at: Option<u64>,
    /// parser.rs `parse_moov`'s verdict on each `trak` of that `moov`.
    track_roles: Vec<heif::TrackRole>,
    traks_seen: usize,
    /// `hvcC` and `colr` boxes of the decoded sample entry, in order.
    entry_hvcc: Vec<usize>,
    entry_colr: Vec<usize>,
    /// Leaves that hold an item extent as a child.
    hosted: BTreeSet<usize>,
}

impl<'a> Walker<'a> {
    fn new(data: &'a [u8], stop: &'a dyn Stop) -> Result<Self, At<HeicError>> {
        // parser.rs `parse`: `moov` is read only when no top-level `meta`
        // exists, and then the last one; look ahead for both.
        let mut has_top_meta = false;
        let mut last_moov: Option<(u64, Hdr)> = None;
        let mut pos = 0u64;
        let len = data.len() as u64;
        while let HdrOutcome::Ok(h) = read_header(data, pos, len) {
            check_stop(stop)?;
            if &h.typ == b"meta" {
                has_top_meta = true;
            }
            let end = h.end;
            if &h.typ == b"moov" {
                last_moov = Some((pos, h));
            }
            pos = end;
        }
        let mut used_moov_at = None;
        let mut track_roles = Vec::new();
        if !has_top_meta && let Some((at, h)) = last_moov {
            let content = at + h.base_header..h.end;
            let bmff = BmffBox {
                header: BoxHeader {
                    box_type: FourCC(h.typ),
                    size: h.end - at,
                    content_offset: usize::try_from(content.start).unwrap_or(usize::MAX),
                },
                content: slice(data, content),
            };
            used_moov_at = Some(at);
            track_roles = heif::moov_track_roles(&bmff, len, stop)?;
        }
        Ok(Self {
            data,
            stop,
            nodes: Vec::new(),
            max_nodes: DEFAULT_MAX_PARTS as usize,
            has_top_meta,
            ftyp: None,
            top_metas: Vec::new(),
            moovs: Vec::new(),
            mdats: Vec::new(),
            idats: Vec::new(),
            pitms: Vec::new(),
            infes: Vec::new(),
            ilocs: Vec::new(),
            props: Vec::new(),
            irefs: Vec::new(),
            ipmas: Vec::new(),
            stsd_hevc_taken: false,
            used_moov_at,
            track_roles,
            traks_seen: 0,
            entry_hvcc: Vec::new(),
            entry_colr: Vec::new(),
            hosted: BTreeSet::new(),
        })
    }

    fn add(&mut self, node: Node) -> Result<usize, At<HeicError>> {
        if self.nodes.len() >= self.max_nodes {
            return Err(at!(HeicError::LimitExceeded(
                "inventory exceeds the zencodec part cap"
            )));
        }
        self.nodes.push(node);
        Ok(self.nodes.len() - 1)
    }

    fn note(&mut self, node: usize, note: impl Into<String>) {
        if let Some(n) = self.nodes.get_mut(node) {
            if n.notes.len() < MAX_NOTES {
                n.notes.push(note.into());
            } else {
                n.dropped_notes += 1;
            }
        }
    }

    fn leaf(
        parent: Option<usize>,
        kind: PartKind,
        tag: PartTag,
        range: Range<u64>,
        disp: Disposition,
    ) -> Node {
        Node {
            parent,
            kind,
            tag,
            label: None,
            range,
            body: None,
            disp,
            notes: Vec::new(),
            dropped_notes: 0,
            gap_fill: None,
            inner: Vec::new(),
        }
    }

    fn malformed(
        &mut self,
        parent: Option<usize>,
        tag: PartTag,
        range: Range<u64>,
        why: String,
    ) -> Result<(), At<HeicError>> {
        let kind = if tag == PartTag::None {
            PartKind::Gap
        } else {
            PartKind::Box
        };
        let mut n = Self::leaf(parent, kind, tag, range, Disposition::Malformed);
        n.notes.push(why);
        self.add(n)?;
        Ok(())
    }

    /// Walk the boxes in `start..end`, which the caller guarantees is within
    /// the input.
    fn walk(
        &mut self,
        parent: Option<usize>,
        start: u64,
        end: u64,
        ctx: Ctx,
        depth: usize,
    ) -> Result<(), At<HeicError>> {
        self.walk_counted(parent, start, end, ctx, depth, None)
    }

    fn walk_counted(
        &mut self,
        parent: Option<usize>,
        start: u64,
        end: u64,
        ctx: Ctx,
        depth: usize,
        // `iinf` entry count: parser.rs `parse_iinf` takes parsed `infe`
        // boxes until it has `entry_count` of them (at least one).
        mut iinf_left: Option<(u32, u32)>,
    ) -> Result<(), At<HeicError>> {
        let mut pos = start;
        while pos < end {
            check_stop(self.stop)?;
            match read_header(self.data, pos, end) {
                HdrOutcome::Short => {
                    if ctx != Ctx::Top {
                        self.malformed(
                            parent,
                            PartTag::None,
                            pos..end,
                            format!("{} bytes, too short for a box header", end - pos),
                        )?;
                    }
                    // At the top level, `fill_gaps(None, Trailing)` covers it.
                    return Ok(());
                }
                HdrOutcome::BadSize { typ, size } => {
                    self.malformed(
                        parent,
                        PartTag::FourCc(typ),
                        pos..end,
                        format!("size field {size} is smaller than the box header"),
                    )?;
                    return Ok(());
                }
                HdrOutcome::Overrun { typ, declared } if ctx == Ctx::Top && &typ == b"mdat" => {
                    // heic stops reading boxes here, but reads item extents
                    // at absolute offsets wherever they lie (parser.rs
                    // `get_item_data`): clip the body to the end of the file
                    // so the extents inside are still listed.
                    let header = if be_u32(self.data, pos) == Some(1) {
                        16
                    } else {
                        8
                    };
                    let mut n = Self::leaf(
                        parent,
                        PartKind::Box,
                        PartTag::FourCc(typ),
                        pos..end,
                        Disposition::Malformed,
                    );
                    n.notes.push(format!(
                        "declares {declared} bytes, the file has {} left; heic stops reading boxes here, but reads item extents inside it (the body is clipped to the end of the file)",
                        end - pos
                    ));
                    if pos + header <= end {
                        n.body = Some(pos + header..end);
                        n.gap_fill = Some(Disposition::Unreferenced);
                    }
                    let id = self.add(n)?;
                    self.mdats.push(id);
                    return Ok(());
                }
                HdrOutcome::Overrun { typ, declared } => {
                    let room = if ctx == Ctx::Top {
                        "the file"
                    } else {
                        "its container"
                    };
                    self.malformed(
                        parent,
                        PartTag::FourCc(typ),
                        pos..end,
                        format!(
                            "declares {declared} bytes, {room} has {} left; heic stops reading here",
                            end - pos
                        ),
                    )?;
                    return Ok(());
                }
                HdrOutcome::Ok(h) => {
                    self.visit(parent, pos, &h, ctx, depth, &mut iinf_left)?;
                    pos = h.end;
                }
            }
        }
        Ok(())
    }

    fn visit(
        &mut self,
        parent: Option<usize>,
        pos: u64,
        h: &Hdr,
        ctx: Ctx,
        depth: usize,
        iinf_left: &mut Option<(u32, u32)>,
    ) -> Result<(), At<HeicError>> {
        let range = pos..h.end;
        let (disp, mut layout, child_ctx, why) = self.classify(ctx, &h.typ, pos, parent);
        let mut kind = match ctx {
            Ctx::Ipco => PartKind::Property,
            _ => PartKind::Box,
        };
        let mut tag = PartTag::FourCc(h.typ);
        let mut label = h.usertype.as_ref().map(uuid_str);
        let mut notes: Vec<String> = Vec::new();
        if let Some(why) = why {
            notes.push(why.to_string());
        }
        if &h.typ == b"uuid" && h.usertype.is_none() {
            notes.push("uuid box too short for its 16-byte usertype".to_string());
        }
        if depth >= MAX_DEPTH && layout != Layout::Leaf {
            layout = Layout::Leaf;
            notes.push(format!("nested deeper than {MAX_DEPTH} boxes; not walked"));
        }

        let content = pos + h.base_header..h.end;
        let bmff = BmffBox {
            header: BoxHeader {
                box_type: FourCC(h.typ),
                size: h.end - pos,
                content_offset: usize::try_from(content.start).unwrap_or(usize::MAX),
            },
            content: slice(self.data, content.clone()),
        };

        // Contents interpreted by heic's own parsers.
        let mut infe: Option<InfeRec> = None;
        let mut iinf_count: Option<u32> = None;
        match (ctx, &h.typ) {
            (Ctx::Top, b"ftyp") => {
                label = label_from(slice(
                    self.data,
                    content.start..content.end.min(content.start + 4),
                ));
                let brands: Vec<String> = slice(self.data, content.start + 8..content.end)
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .take(16)
                    .map(fourcc_str)
                    .collect();
                if !brands.is_empty() {
                    notes.push(format!("compatible brands {}", brands.join(",")));
                }
            }
            (_, b"hdlr") => {
                // version/flags(4) pre_defined(4) handler_type(4)
                label = label_from(slice(
                    self.data,
                    content.start + 8..(content.start + 12).min(content.end),
                ));
            }
            (Ctx::Iinf, b"infe") => {
                let parsed = heif::parse_infe(&bmff).ok();
                let mut taken = false;
                if let Some(info) = &parsed {
                    kind = PartKind::Item;
                    tag = PartTag::Code(info.item_id);
                    label = if info.item_name.is_empty() {
                        Some(fourcc_str(&info.item_type.0))
                    } else {
                        label_from(info.item_name.as_bytes())
                    };
                    notes.push(format!("type {}", fourcc_str(&info.item_type.0)));
                    if !info.content_type.is_empty() {
                        notes.push(format!(
                            "content type {:?}",
                            label_from(info.content_type.as_bytes()).unwrap_or_default()
                        ));
                    }
                    if info.hidden {
                        notes.push("hidden".to_string());
                    }
                    if let Some((wanted, got)) = iinf_left.as_mut() {
                        if *got == u32::MAX {
                            // parse_iinf already stopped.
                        } else {
                            taken = true;
                            *got += 1;
                            if *got >= *wanted {
                                *got = u32::MAX;
                            }
                        }
                    }
                } else {
                    notes.push("heic's infe parser rejects this entry".to_string());
                }
                infe = Some(InfeRec {
                    node: usize::MAX,
                    info: parsed,
                    taken,
                });
            }
            _ => {}
        }

        let body = match layout {
            Layout::Leaf => None,
            Layout::Plain | Layout::Data => Some(pos + h.header..h.end),
            Layout::Full => Some(pos + h.header + 4..h.end),
            Layout::FullCount => Some(pos + h.header + 8..h.end),
            Layout::VisualSampleEntry => Some(pos + h.header + 78..h.end),
            Layout::AudioSampleEntry => Some(pos + h.header + 28..h.end),
            Layout::NestedMeta => {
                let qt = fourcc_at(self.data, pos + h.header + 4) == Some(*b"hdlr");
                if qt {
                    notes.push("QuickTime-style meta (no version/flags)".to_string());
                    Some(pos + h.header..h.end)
                } else {
                    Some(pos + h.header + 4..h.end)
                }
            }
            Layout::Iinf => {
                let version = slice(self.data, pos + h.header..h.end).first().copied();
                let count_at = pos + h.header + 4;
                match version {
                    Some(0) => be_u16(self.data, count_at)
                        .filter(|_| count_at + 2 <= h.end)
                        .map(|c| {
                            iinf_count = Some(u32::from(c));
                            count_at + 2..h.end
                        }),
                    Some(_) => be_u32(self.data, count_at)
                        .filter(|_| count_at + 4 <= h.end)
                        .map(|c| {
                            iinf_count = Some(c);
                            count_at + 4..h.end
                        }),
                    None => None,
                }
            }
        };
        let body = match body {
            Some(b) if b.start <= b.end && b.end <= h.end => Some(b),
            Some(_) => {
                notes.push("too short for its fixed fields; contents not walked".to_string());
                None
            }
            None if layout != Layout::Leaf => {
                notes.push("too short for its fixed fields; contents not walked".to_string());
                None
            }
            None => None,
        };

        let mut node = Self::leaf(parent, kind, tag, range, disp);
        node.label = label;
        node.notes = notes;
        node.body = body.clone();
        // The `iref` version byte follows the parent's box header (8 or 16
        // bytes); it sets the item ID width of every reference entry.
        let iref_version = match (ctx, parent.and_then(|p| self.nodes.get(p))) {
            (Ctx::Iref, Some(p)) => match read_header(self.data, p.range.start, p.range.end) {
                HdrOutcome::Ok(ph) => slice(self.data, p.range.clone())
                    .get(usize::try_from(ph.base_header).unwrap_or(usize::MAX))
                    .copied()
                    .unwrap_or(0),
                _ => 0,
            },
            _ => 0,
        };
        if layout == Layout::Leaf {
            node.inner = inner_parts(
                ctx,
                &h.typ,
                content.clone(),
                slice(self.data, content.clone()),
                iref_version,
            );
        }
        if layout == Layout::Data && body.is_some() {
            node.gap_fill = Some(Disposition::Unreferenced);
        }
        let id = self.add(node)?;

        // Records for the second pass.
        match (ctx, &h.typ) {
            (Ctx::Top, b"ftyp") => {
                if self.ftyp.is_none() {
                    self.ftyp = Some(id);
                }
            }
            (Ctx::Top, b"meta") => self.top_metas.push(id),
            (Ctx::Top, b"moov") => self.moovs.push(id),
            (Ctx::Top, b"mdat") => self.mdats.push(id),
            (Ctx::Meta, b"pitm") => self.pitms.push(id),
            // Parsed in the second pass, after `heif::parse`, so the walk
            // never holds a second copy of heic's item locations.
            (Ctx::Meta, b"iloc") => self.ilocs.push(IlocRec {
                node: id,
                size: h.end - pos,
                content: content.clone(),
            }),
            (Ctx::Meta, b"idat") => self.idats.push((id, true)),
            (_, b"idat") => self.idats.push((id, false)),
            (Ctx::Iref, typ) => self.irefs.push((id, *typ, iref_version)),
            (Ctx::Iprp, b"ipma") => self.ipmas.push(id),
            (Ctx::SampleEntry { used: true }, b"hvcC") => self.entry_hvcc.push(id),
            (Ctx::SampleEntry { used: true }, b"colr") => self.entry_colr.push(id),
            (Ctx::Ipco, typ) => {
                let index = self.props.len();
                let prop = heif::parse_property(&bmff);
                let colour_type = (typ == b"colr")
                    .then(|| {
                        fourcc_at(self.data, content.start)
                            .filter(|_| content.end - content.start >= 4)
                    })
                    .flatten();
                if let Some(ct) = colour_type {
                    self.nodes[id].label = Some(fourcc_str(&ct));
                }
                if typ == b"auxC"
                    && let ItemProperty::AuxiliaryType(a) = &prop
                {
                    self.nodes[id].label = label_from(a.aux_type.as_bytes());
                }
                if typ == b"hvcC" {
                    let nals = hvcc_nal_summary(slice(self.data, content.clone()));
                    if !nals.is_empty() {
                        self.note(
                            id,
                            format!(
                                "NAL arrays {nals}; heic reads VPS/SPS/PPS only (hevc/mod.rs decode_nal_units)"
                            ),
                        );
                    }
                }
                self.props.push(PropRec {
                    node: id,
                    index,
                    typ: *typ,
                    prop,
                    colour_type,
                });
            }
            _ => {}
        }
        if let Some(mut rec) = infe {
            rec.node = id;
            self.infes.push(rec);
        }

        if let Some(b) = body
            && layout != Layout::Data
        {
            let iinf = iinf_count.map(|c| (c, 0u32));
            self.walk_counted(Some(id), b.start, b.end, child_ctx, depth + 1, iinf)?;
        }
        Ok(())
    }

    /// heic's treatment of a box type in a context:
    /// `(disposition, layout, context for its children, remark)`.
    fn classify(
        &mut self,
        ctx: Ctx,
        typ: &[u8; 4],
        pos: u64,
        parent: Option<usize>,
    ) -> (Disposition, Layout, Ctx, Option<&'static str>) {
        use Disposition as D;
        match ctx {
            // parser.rs `parse` top-level match.
            Ctx::Top => match typ {
                b"ftyp" => (D::Structure, Layout::Leaf, ctx, None),
                b"meta" => (D::Structure, Layout::Full, Ctx::Meta, None),
                b"moov" if self.has_top_meta => (
                    D::Skipped,
                    Layout::Plain,
                    Ctx::Moov {
                        track: MoovUse::Unread,
                    },
                    Some("heic reads moov only when the file has no top-level meta"),
                ),
                b"moov" if self.used_moov_at != Some(pos) => (
                    D::Skipped,
                    Layout::Plain,
                    Ctx::Moov {
                        track: MoovUse::Unread,
                    },
                    Some("a later top-level moov replaces it (parser.rs parse keeps the last)"),
                ),
                b"moov" => (
                    D::Structure,
                    Layout::Plain,
                    Ctx::Moov {
                        track: MoovUse::Top,
                    },
                    Some(
                        "no top-level meta: heic decodes the first sync sample of a pict/vide track",
                    ),
                ),
                b"mdat" => (D::Structure, Layout::Data, ctx, None),
                b"free" | b"skip" | b"wide" => (D::Padding, Layout::Leaf, ctx, None),
                b"uuid" => (D::Unknown, Layout::Leaf, ctx, None),
                b"pdin" | b"moof" | b"mfra" | b"styp" | b"sidx" | b"ssix" | b"prft" | b"meco"
                | b"emsg" | b"jumb" => (
                    D::Skipped,
                    Layout::Leaf,
                    ctx,
                    Some("heic does not read this box"),
                ),
                _ => (D::Unknown, Layout::Leaf, ctx, None),
            },
            // parser.rs `parse_meta`.
            Ctx::Meta => match typ {
                b"hdlr" => (
                    D::Skipped,
                    Layout::Leaf,
                    ctx,
                    Some("heic does not check the handler"),
                ),
                b"pitm" | b"iloc" => (D::Structure, Layout::Leaf, ctx, None),
                b"iinf" => (D::Structure, Layout::Iinf, Ctx::Iinf, None),
                b"iprp" => (D::Structure, Layout::Plain, Ctx::Iprp, None),
                b"iref" => (D::Structure, Layout::Full, Ctx::Iref, None),
                b"idat" => (D::Structure, Layout::Data, ctx, None),
                b"dinf" | b"grpl" => (
                    D::Skipped,
                    Layout::Plain,
                    Ctx::Unread,
                    Some("heic does not read this box"),
                ),
                b"free" | b"skip" => (D::Padding, Layout::Leaf, ctx, None),
                b"uuid" => (D::Unknown, Layout::Leaf, ctx, None),
                b"ipro" | b"fiin" | b"ipmc" | b"xml " | b"bxml" => (
                    D::Skipped,
                    Layout::Leaf,
                    ctx,
                    Some("heic does not read this box"),
                ),
                _ => (D::Unknown, Layout::Leaf, ctx, None),
            },
            // parser.rs `parse_iinf`: only `infe` children; the item's
            // disposition is set in the second pass.
            Ctx::Iinf => match typ {
                b"infe" => (D::Structure, Layout::Leaf, ctx, None),
                _ => (
                    D::Unknown,
                    Layout::Leaf,
                    ctx,
                    Some("heic reads only infe children of iinf"),
                ),
            },
            // parser.rs `parse_iprp`.
            Ctx::Iprp => match typ {
                b"ipco" => (D::Structure, Layout::Plain, Ctx::Ipco, None),
                b"ipma" => (D::Structure, Layout::Leaf, ctx, None),
                _ => (
                    D::Unknown,
                    Layout::Leaf,
                    ctx,
                    Some("heic reads only ipco and ipma children of iprp"),
                ),
            },
            // Properties: decided in the second pass.
            Ctx::Ipco => (D::Unknown, Layout::Leaf, ctx, None),
            // parser.rs `parse_iref` parses every reference type; the
            // second pass decides which ones the decode consults.
            Ctx::Iref => (D::Dropped, Layout::Leaf, ctx, None),
            Ctx::Moov { track } => {
                let layout = generic_layout(typ);
                let unread = Ctx::Moov {
                    track: MoovUse::Unread,
                };
                match typ {
                    b"free" | b"skip" => return (D::Padding, Layout::Leaf, ctx, None),
                    b"uuid" => return (D::Unknown, Layout::Leaf, ctx, None),
                    _ => {}
                }
                if track == MoovUse::Top {
                    // parser.rs `parse_moov` reads only `trak` children.
                    if typ != b"trak" {
                        return (
                            D::Skipped,
                            layout,
                            unread,
                            Some("parse_moov reads only trak boxes"),
                        );
                    }
                    let role = self.track_roles.get(self.traks_seen).copied();
                    self.traks_seen += 1;
                    let parsed = Ctx::Moov {
                        track: MoovUse::Parsed,
                    };
                    return match role {
                        Some(heif::TrackRole::Primary) => (
                            D::Structure,
                            layout,
                            Ctx::Moov {
                                track: MoovUse::Primary,
                            },
                            Some(
                                "heic decodes this track's first sync sample (parser.rs parse_moov)",
                            ),
                        ),
                        Some(heif::TrackRole::Thumbnail) => (
                            D::Dropped,
                            layout,
                            parsed,
                            Some(
                                "thumbnail track: parsed into item 2, which only the native decode_thumbnail reads",
                            ),
                        ),
                        Some(heif::TrackRole::Parsed) => (
                            D::Dropped,
                            layout,
                            parsed,
                            Some("parsed; heic decodes another track"),
                        ),
                        Some(heif::TrackRole::Rejected) => (
                            D::Dropped,
                            layout,
                            parsed,
                            Some("parse_trak rejects this track"),
                        ),
                        Some(heif::TrackRole::NotRead) | None => (
                            D::Skipped,
                            layout,
                            unread,
                            Some("after the 16 tracks heic reads (parser.rs MAX_TRACKS)"),
                        ),
                    };
                }
                // parser.rs `parse_trak`/`parse_mdia`/`parse_minf`/
                // `parse_stbl`: each reads these children only.
                let parent_typ = parent.and_then(|p| match self.nodes.get(p)?.tag {
                    PartTag::FourCc(t) => Some(t),
                    _ => None,
                });
                let read = matches!(
                    (parent_typ.as_ref(), typ),
                    (Some(b"trak"), b"tkhd" | b"mdia")
                        | (Some(b"mdia"), b"hdlr" | b"minf")
                        | (Some(b"minf"), b"stbl")
                        | (
                            Some(b"stbl"),
                            b"stsd" | b"stsz" | b"stco" | b"co64" | b"stsc" | b"stss"
                        )
                );
                let child = match typ {
                    b"stsd" => Ctx::Stsd {
                        used: track == MoovUse::Primary,
                    },
                    _ if read => ctx,
                    _ => unread,
                };
                match track {
                    MoovUse::Primary if read => (D::Structure, layout, child, None),
                    MoovUse::Parsed if read => (
                        D::Dropped,
                        layout,
                        child,
                        Some("parsed; nothing of this track reaches the caller"),
                    ),
                    _ => (D::Skipped, layout, child, None),
                }
            }
            Ctx::Stsd { used } => {
                let visual = matches!(
                    typ,
                    b"hvc1"
                        | b"hev1"
                        | b"av01"
                        | b"avc1"
                        | b"avc3"
                        | b"jpeg"
                        | b"mp4v"
                        | b"encv"
                        | b"vvc1"
                        | b"vvi1"
                        | b"j2ki"
                        | b"uncv"
                );
                let hevc = matches!(typ, b"hvc1" | b"hev1");
                let chosen = used && hevc && !self.stsd_hevc_taken;
                if chosen {
                    self.stsd_hevc_taken = true;
                }
                let layout = if visual {
                    Layout::VisualSampleEntry
                } else if is_audio_sample_entry(typ) {
                    Layout::AudioSampleEntry
                } else {
                    Layout::Leaf
                };
                if chosen {
                    (
                        D::Structure,
                        layout,
                        Ctx::SampleEntry { used: true },
                        Some("heic uses the first HEVC sample entry of a track"),
                    )
                } else {
                    (D::Skipped, layout, Ctx::SampleEntry { used: false }, None)
                }
            }
            // parser.rs `parse_visual_sample_entry`: hvcC and colr.
            Ctx::SampleEntry { used } => match typ {
                b"hvcC" | b"colr" if used => (D::Structure, Layout::Leaf, ctx, None),
                b"free" | b"skip" => (D::Padding, Layout::Leaf, ctx, None),
                b"uuid" => (D::Unknown, Layout::Leaf, ctx, None),
                _ => (D::Skipped, generic_layout(typ), Ctx::Unread, None),
            },
            Ctx::Unread => match typ {
                b"free" | b"skip" => (D::Padding, Layout::Leaf, ctx, None),
                b"uuid" => (D::Unknown, Layout::Leaf, ctx, None),
                b"stsd" => (
                    D::Skipped,
                    Layout::FullCount,
                    Ctx::Stsd { used: false },
                    None,
                ),
                _ => (D::Skipped, generic_layout(typ), ctx, None),
            },
        }
    }
}

/// Container layouts used to enumerate subtrees heic itself does not read.
fn generic_layout(typ: &[u8; 4]) -> Layout {
    match typ {
        b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"edts" | b"udta" | b"mvex" | b"dinf"
        | b"mfra" | b"moof" | b"traf" | b"tref" | b"grpl" | b"sinf" | b"schi" | b"rinf"
        | b"meco" | b"iprp" | b"ipco" | b"strk" | b"strd" => Layout::Plain,
        b"dref" | b"stsd" => Layout::FullCount,
        b"meta" => Layout::NestedMeta,
        b"iinf" => Layout::Iinf,
        b"iref" => Layout::Full,
        _ => Layout::Leaf,
    }
}

/// Audio sample entries (ISO 14496-12 `AudioSampleEntry`), walked so their
/// child boxes (`esds`, `dOps`, …) are listed.
fn is_audio_sample_entry(typ: &[u8; 4]) -> bool {
    matches!(
        typ,
        b"mp4a"
            | b"Opus"
            | b"fLaC"
            | b"ac-3"
            | b"ec-3"
            | b"alac"
            | b"ipcm"
            | b"fpcm"
            | b"mha1"
            | b"mhm1"
            | b"enca"
    )
}

fn be32(c: &[u8], at: usize) -> Option<u64> {
    let b = c.get(at..at.checked_add(4)?)?;
    Some(u64::from(u32::from_be_bytes([b[0], b[1], b[2], b[3]])))
}

/// Bytes of a leaf box's payload that heic never reads, as child parts to add
/// when the box is consumed. `content` is the payload heic's parsers see
/// (after the 8- or 16-byte header). Read lengths mirror parser.rs.
fn inner_parts(
    ctx: Ctx,
    typ: &[u8; 4],
    content: Range<u64>,
    c: &[u8],
    iref_version: u8,
) -> Vec<Inner> {
    let mut out = Vec::new();
    let len = c.len();
    let tail = |out: &mut Vec<Inner>, read: usize, why: &str| {
        if read < len {
            out.push((
                content.start + read as u64..content.end,
                Some(Disposition::Dropped),
                why.to_string(),
            ));
        }
    };
    let moov_used = ctx
        == Ctx::Moov {
            track: MoovUse::Primary,
        };
    match (ctx, typ) {
        // `parse_ftyp`: brands in whole 4-byte units, at most 256.
        (Ctx::Top, b"ftyp") => {
            let read = 8 + (len.saturating_sub(8) / 4).min(256) * 4;
            tail(
                &mut out,
                read,
                "not a whole brand, or past the 256 brands heic reads",
            );
        }
        (Ctx::Meta, b"pitm") => {
            let read = if c.first() == Some(&0) { 6 } else { 8 };
            tail(&mut out, read, "after the item ID heic's pitm parser reads");
        }
        (Ctx::Iprp, b"ipma") => {
            if let Some(read) = ipma_read_len(c) {
                tail(
                    &mut out,
                    read,
                    "after the last association heic's ipma parser reads",
                );
            }
        }
        (Ctx::Iinf, b"infe") => {
            if let Some(read) = infe_read_len(c) {
                tail(
                    &mut out,
                    read,
                    "after the name and content type heic reads (content encoding, extensions)",
                );
            }
        }
        (Ctx::Iref, _) => {
            let read = iref_entry_read_len(c, iref_version);
            tail(
                &mut out,
                read,
                "after the last whole reference entry heic parses",
            );
        }
        (Ctx::Ipco, b"ispe") => tail(&mut out, 12, "after the width and height heic reads"),
        (Ctx::Ipco, b"clap") => tail(&mut out, 32, "after the 8 clap fields heic reads"),
        (Ctx::Ipco, b"irot" | b"imir") => tail(&mut out, 1, "after the one byte heic reads"),
        (Ctx::Ipco, b"clli") => tail(&mut out, 4, "after the two light levels heic reads"),
        (Ctx::Ipco, b"mdcv") => tail(&mut out, 24, "after the 24 bytes heic reads"),
        (Ctx::Ipco, b"colr") | (Ctx::SampleEntry { used: true }, b"colr") => match c.get(..4) {
            Some(b"nclx") => tail(&mut out, 11, "after the nclx fields heic reads"),
            Some(b"prof" | b"ricc") => {
                // The profile's own header declares its size.
                if let Some(declared) = be32(c, 4) {
                    let end = 4u64.saturating_add(declared);
                    if end < len as u64 {
                        // parser.rs `parse_colr` keeps everything after the
                        // colour type, so these bytes reach the caller with
                        // the profile.
                        out.push((
                            content.start + end..content.end,
                            None,
                            format!(
                                "after the ICC profile's declared {declared} bytes, but heic hands them to the caller with the profile (parser.rs parse_colr keeps the whole payload)"
                            ),
                        ));
                    }
                }
            }
            _ => {}
        },
        (Ctx::Ipco, b"hvcC") | (Ctx::SampleEntry { used: true }, b"hvcC") => {
            // The NAL units themselves become coded-unit parts later
            // (`add_coded_units`).
            if let Some((_, read)) = hvcc_units(c, usize::MAX) {
                tail(
                    &mut out,
                    read,
                    "after the NAL arrays heic's hvcC parser reads",
                );
            }
        }
        (Ctx::Moov { .. }, b"tkhd") if moov_used => {
            let read = if c.first() == Some(&0) { 84 } else { 96 };
            tail(&mut out, read, "after the fields heic's tkhd parser reads");
        }
        (Ctx::Moov { .. }, b"hdlr") if moov_used => {
            tail(
                &mut out,
                12,
                "reserved fields and handler name: heic reads only the handler type",
            );
        }
        (Ctx::Moov { .. }, b"stsz") if moov_used => {
            let read = match (be32(c, 4), be32(c, 8)) {
                (Some(0), Some(n)) => n.saturating_mul(4).saturating_add(12),
                _ => 12,
            };
            tail(
                &mut out,
                usize::try_from(read).unwrap_or(usize::MAX),
                "after the sample sizes heic reads",
            );
        }
        (Ctx::Moov { .. }, b"stco" | b"co64" | b"stsc" | b"stss") if moov_used => {
            let per: u64 = match typ {
                b"co64" => 8,
                b"stsc" => 12,
                _ => 4,
            };
            if let Some(n) = be32(c, 4) {
                let read = n.saturating_mul(per).saturating_add(8);
                tail(
                    &mut out,
                    usize::try_from(read).unwrap_or(usize::MAX),
                    "after the entries heic reads",
                );
            }
        }
        _ => {}
    }
    out
}

/// parser.rs `parse_ipma`: the bytes its entry loop reads.
fn ipma_read_len(c: &[u8]) -> Option<usize> {
    let version = *c.first()?;
    let wide = c.get(3)? & 1 != 0;
    let count = be32(c, 4)?;
    let mut pos = 8usize;
    let id = if version < 1 { 2 } else { 4 };
    for _ in 0..count {
        if pos + id > c.len() {
            break;
        }
        pos += id;
        if pos >= c.len() {
            break;
        }
        let n = c[pos];
        pos += 1;
        for _ in 0..n {
            let w = if wide { 2 } else { 1 };
            if pos + w > c.len() {
                break;
            }
            pos += w;
        }
    }
    Some(pos)
}

/// parser.rs `parse_infe`: version/flags, ID, protection index, type, then
/// the item name and content type up to their NULs.
fn infe_read_len(c: &[u8]) -> Option<usize> {
    let version = *c.first()?;
    let mut pos = match version {
        0..=1 => 8,
        2 => 12,
        _ => 14,
    };
    if pos >= c.len() {
        return Some(pos.min(c.len()));
    }
    // `parse_infe`: a string without a NUL reads as empty (`unwrap_or(0)`),
    // so its bytes are not read; the name still advances the cursor by one.
    let name_end = c[pos..].iter().position(|&b| b == 0).unwrap_or(0);
    let mut read = pos;
    pos += name_end + 1;
    if name_end > 0 || c.get(pos - 1) == Some(&0) {
        read = pos;
    }
    if pos < c.len()
        && let Some(e) = c[pos..].iter().position(|&b| b == 0)
    {
        pos += e + 1;
        read = pos;
    }
    Some(read.min(c.len()))
}

/// parser.rs `parse_iref`: one reference box's entries.
fn iref_entry_read_len(c: &[u8], version: u8) -> usize {
    let id = if version == 0 { 2 } else { 4 };
    let mut pos = 0usize;
    while pos < c.len() {
        if pos + id > c.len() {
            break;
        }
        pos += id;
        if pos + 2 > c.len() {
            break;
        }
        let n = u16::from_be_bytes([c[pos], c[pos + 1]]);
        pos += 2;
        for _ in 0..n {
            if pos + id > c.len() {
                break;
            }
            pos += id;
        }
    }
    pos
}

/// `"VPS×1, SPS×1, PPS×1, SEI(prefix)×1"` for an `hvcC` payload.
fn hvcc_nal_summary(content: &[u8]) -> String {
    let Some(&num_arrays) = content.get(22) else {
        return String::new();
    };
    let mut pos = 23usize;
    let mut parts = Vec::new();
    for _ in 0..num_arrays {
        let (Some(&t), Some(n)) = (content.get(pos), content.get(pos + 1..pos + 3)) else {
            break;
        };
        let nal_type = t & 0x3F;
        let count = u16::from_be_bytes([n[0], n[1]]);
        pos += 3;
        for _ in 0..count {
            let Some(l) = content.get(pos..pos + 2) else {
                break;
            };
            pos += 2 + usize::from(u16::from_be_bytes([l[0], l[1]]));
        }
        let name = match nal_type {
            32 => "VPS".to_string(),
            33 => "SPS".to_string(),
            34 => "PPS".to_string(),
            39 => "SEI(prefix)".to_string(),
            40 => "SEI(suffix)".to_string(),
            other => format!("type {other}"),
        };
        parts.push(format!("{name}×{count}"));
        if parts.len() >= 16 {
            break;
        }
    }
    parts.join(", ")
}

// ── What the decode reads ────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Use {
    disp: Disposition,
    why: String,
}

fn rank(d: Disposition) -> u8 {
    match d {
        Disposition::ImageData => 9,
        Disposition::Metadata(_) => 8,
        Disposition::Structure => 7,
        Disposition::Dropped => 4,
        Disposition::Skipped => 3,
        Disposition::Unknown => 2,
        Disposition::Unreferenced => 1,
        _ => 0,
    }
}

/// The role an item's properties play, which decides each property's
/// disposition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    /// The primary item: its colour, orientation and HDR metadata are
    /// reported (codec.rs `build_image_info_full`).
    Primary,
    /// An item decoded as part of another image (derived-image input).
    /// `nclx`: its nclx reaches the decoded frame. A deriving item's own
    /// nclx overwrites all four fields after the inputs decode
    /// (decode.rs `decode_item`), and grids and overlays keep only the
    /// first input's range and matrix (`decode_grid`, `decode_iovl`).
    Component { nclx: bool },
    /// A tile of an HEVC grid: only the first tile's `hvcC` and `ispe` are
    /// consulted (decode.rs `decode_grid`).
    HevcGridTile { first: bool },
    /// The alpha plane (decode.rs `decode_alpha_plane`).
    Alpha,
    /// The gain-map item when only its dimensions are reported.
    GainMapDims,
    /// The gain-map item when it is decoded.
    GainMapDecoded,
    /// The depth item when it is decoded.
    DepthDecoded,
    /// An auxiliary item whose type is reported in `HeicAuxiliaryInfo`.
    AuxInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Ispe,
    Codec,
    Colr,
    Clap,
    Irot,
    Imir,
    AuxC,
    Clli,
    Mdcv,
}

/// How the decode reads an item's data, when that differs from the item's
/// own disposition (a descriptor read only up to its fields, an item whose
/// data is never read).
#[derive(Clone, Debug)]
struct DataUse {
    disp: Disposition,
    why: String,
    /// Only the first this many bytes of the item's data are read; the rest
    /// is `Dropped`. `None`: all of it.
    prefix: Option<u64>,
}

#[derive(Default)]
struct Model {
    opts: Options,
    /// The item's declaration (`infe`), and its data unless `data` says
    /// otherwise.
    items: BTreeMap<u32, Use>,
    data: BTreeMap<u32, DataUse>,
    props: BTreeMap<usize, Use>,
    /// `auxC` bytes after the URN's NUL (the subtype), per property.
    auxc_tail: BTreeMap<usize, Use>,
    visited: BTreeSet<(u32, u8)>,
    /// HEVC grid tile → the tile whose `hvcC` decodes it (decode.rs
    /// `decode_grid` uses the first tile's configuration for all of them).
    hevc_cfg: BTreeMap<u32, u32>,
    /// Items whose `dimg` references the decode follows.
    dimg_from: BTreeSet<u32>,
    /// The primary item: `auxl` references to it are consulted
    /// (`find_all_auxiliary_items`).
    primary: Option<u32>,
    /// `cdsc` references to this item are consulted for the gain map's XMP
    /// (`find_xmp_for_item`); `true` when that XMP reaches the caller.
    cdsc_target: Option<(u32, bool)>,
    /// The `tmap` payload is attached whole (`HdrGainMap::iso21496`).
    tmap_attached: Option<u32>,
    /// The first usable EXIF item feeds Apple gain-map parameters
    /// (codec.rs `apple_gain_map_params`).
    exif_feeds_gain_map: bool,
}

impl Model {
    fn set_item(&mut self, id: u32, disp: Disposition, why: impl Into<String>) {
        set(&mut self.items, id, disp, why.into());
    }

    fn set_prop(&mut self, idx: usize, disp: Disposition, why: String) {
        set(&mut self.props, idx, disp, why);
    }

    /// Record how the decode reads `id`'s data. The highest-ranked use wins;
    /// at equal rank, reading more wins.
    fn set_data(
        &mut self,
        id: u32,
        disp: Disposition,
        why: impl Into<String>,
        prefix: Option<u64>,
    ) {
        let new = DataUse {
            disp,
            why: why.into(),
            prefix,
        };
        match self.data.get(&id) {
            Some(old) if rank(old.disp) > rank(disp) => {}
            Some(old)
                if rank(old.disp) == rank(disp)
                    && match (old.prefix, prefix) {
                        (None, _) => true,
                        (Some(_), None) => false,
                        (Some(a), Some(b)) => a >= b,
                    } => {}
            _ => {
                self.data.insert(id, new);
            }
        }
    }

    fn build(c: &HeifContainer<'_>, opts: &Options) -> Self {
        let mut m = Self {
            opts: *opts,
            ..Self::default()
        };
        let Some(primary) = c.primary_item() else {
            return m;
        };
        let pid = primary.id;
        m.primary = Some(pid);
        // decode.rs `decode_to_frame` → `decode_item(primary)`.
        m.visit_image(
            c,
            pid,
            0,
            Role::Primary,
            false,
            Disposition::ImageData,
            "primary image",
        );

        // decode.rs `decode_to_frame`: the first alpha auxiliary item.
        let alpha = ALPHA_URNS
            .iter()
            .find_map(|urn| c.find_auxiliary_items(pid, urn).first().copied());
        if let Some(a) = alpha
            && let Some(item) = c.get_item(a)
        {
            let decodable =
                item.hevc_config.is_some() || (cfg!(feature = "av1") && item.av1_config.is_some());
            if decodable {
                m.set_item(
                    a,
                    Disposition::ImageData,
                    "alpha plane of the primary image",
                );
                m.props_of(c, a, Role::Alpha);
            } else {
                m.set_item(
                    a,
                    Disposition::Skipped,
                    "alpha item without hvcC/av1C; decode_alpha_plane does not decode it",
                );
            }
        }

        // Gain map: lib.rs `ImageInfo::from_bytes` (has_gain_map), codec.rs
        // `extract_gain_map_info` (GainMapPresence::Available) and
        // `decode_inner` (pixel decode), decode.rs `decode_gain_map`.
        let apple: Vec<u32> = c.find_auxiliary_items(pid, APPLE_GAIN_MAP_URN);
        let has_gain_map = !apple.is_empty()
            || c.items().any(|i| {
                i.item_type == ItemType::Tmap
                    && c.get_item_references(i.id, FourCC(*b"dimg")).len() >= 2
            });
        if has_gain_map {
            let tmap = crate::decode::find_tmap_gain_map(c);
            let gm_dims = |id: u32| c.get_item(id).and_then(|i| i.dimensions).is_some();
            let iso_ok = tmap.as_ref().is_some_and(|(_, _, iso)| {
                zencodec::gainmap::parse_iso21496_fmt(
                    iso,
                    zencodec::gainmap::Iso21496Format::AvifTmap,
                )
                .is_ok()
            });
            let apple_params = crate::codec::apple_gain_map_params(c).is_some();
            // codec.rs `extract_gain_map_info` (every probe and decode).
            let mut exif_feeds = false;
            if let Some((tmap_id, gm_id, _)) = &tmap
                && iso_ok
                && gm_dims(*gm_id)
            {
                m.set_item(
                    *tmap_id,
                    Disposition::Metadata(MetadataKind::GainMap),
                    "ISO 21496-1 gain-map parameters, reported in ImageInfo::gain_map",
                );
                m.props_of(c, *gm_id, Role::GainMapDims);
            } else {
                if let Some(&gm) = apple.first()
                    && gm_dims(gm)
                    && apple_params
                {
                    m.props_of(c, gm, Role::GainMapDims);
                    exif_feeds = true;
                }
                if let Some((tmap_id, _, _)) = &tmap {
                    m.set_item(
                        *tmap_id,
                        Disposition::Dropped,
                        "read by find_tmap_gain_map; parse_iso21496_fmt rejects it or the gain-map item has no ispe, so no gain map is reported from it",
                    );
                }
            }
            let chosen = tmap
                .as_ref()
                .map(|(t, g, _)| (Some(*t), *g))
                .or_else(|| apple.first().map(|&g| (None, g)));
            if let Some((tmap_id, gm)) = chosen {
                if opts.decode_gain_map() {
                    // codec.rs `gain_map_params_from`: the tmap payload's
                    // parameters, else the Apple MakerNote headroom.
                    if !(tmap_id.is_some() && iso_ok) && apple_params {
                        exif_feeds = true;
                    }
                    let data_disp = if opts.apply_gain_map {
                        Disposition::ImageData
                    } else {
                        Disposition::Metadata(MetadataKind::GainMap)
                    };
                    if let Some(t) = tmap_id {
                        if opts.attach_gain_map {
                            m.set_item(
                                t,
                                Disposition::Metadata(MetadataKind::GainMap),
                                "ISO 21496-1 gain-map metadata, attached whole as HdrGainMap::iso21496",
                            );
                            m.tmap_attached = Some(t);
                        } else if iso_ok {
                            m.set_item(
                                t,
                                Disposition::Metadata(MetadataKind::GainMap),
                                "ISO 21496-1 gain-map parameters, applied by ReconstructHdr",
                            );
                        }
                        m.set_data(
                            t,
                            Disposition::Metadata(MetadataKind::GainMap),
                            "the tmap payload (decode.rs find_tmap_gain_map reads the whole item)",
                            None,
                        );
                    }
                    m.visit_image(
                        c,
                        gm,
                        0,
                        Role::GainMapDecoded,
                        false,
                        data_disp,
                        "gain-map image",
                    );
                    let xmp_of = tmap_id.unwrap_or(gm);
                    m.cdsc_target = Some((xmp_of, opts.attach_gain_map));
                    if let Some(x) = xmp_item_for(c, xmp_of) {
                        if opts.attach_gain_map {
                            m.set_item(
                                x,
                                Disposition::Metadata(MetadataKind::GainMap),
                                "gain-map XMP, attached as HdrGainMap::xmp",
                            );
                        } else {
                            m.set_item(
                                x,
                                Disposition::Dropped,
                                "gain-map XMP: decode_gain_map reads it, ReconstructHdr discards it",
                            );
                        }
                    }
                } else {
                    m.set_item(
                        gm,
                        Disposition::Skipped,
                        "gain-map image: decoded only with extract_gain_map or GainMapRender::Components/ReconstructHdr",
                    );
                }
            }
            m.exif_feeds_gain_map = exif_feeds;
        }

        // Depth: decode.rs `decode_depth`, codec.rs `decode_inner`.
        let depth = DEPTH_URNS
            .iter()
            .find_map(|urn| c.find_auxiliary_items(pid, urn).first().copied());
        if let Some(d) = depth {
            if opts.decode_depth {
                m.visit_image(
                    c,
                    d,
                    0,
                    Role::DepthDecoded,
                    false,
                    Disposition::Metadata(MetadataKind::Supplement),
                    "depth map",
                );
            } else {
                m.set_item(
                    d,
                    Disposition::Skipped,
                    "depth image: decoded only with extract_depth",
                );
            }
        }

        // codec.rs `decode_inner`: every auxiliary item's URN is reported in
        // the `HeicAuxiliaryInfo` extension.
        for (id, urn) in c.find_all_auxiliary_items(pid) {
            m.props_of(c, id, Role::AuxInfo);
            m.set_item(
                id,
                Disposition::Skipped,
                format!(
                    "auxiliary image {:?}; only its type reaches HeicAuxiliaryInfo",
                    label_from(urn.as_bytes()).unwrap_or_default()
                ),
            );
        }

        for t in c.find_thumbnails(pid) {
            m.set_item(
                t,
                Disposition::Skipped,
                "thumbnail of the primary image; native decode_thumbnail only",
            );
        }

        // codec.rs `extract_exif_from_container`: the first usable Exif item,
        // whatever it describes.
        if let Some(id) = exif_item(c) {
            if opts.keep_exif {
                m.set_item(
                    id,
                    Disposition::Metadata(MetadataKind::Exif),
                    "EXIF, reported in ImageInfo (first usable Exif item; no cdsc check)",
                );
            } else if m.exif_feeds_gain_map {
                // codec.rs `apple_gain_map_params` reads the MakerNote
                // whatever the policy; `apply_policy` clears only
                // `embedded_metadata.exif`.
                m.set_item(
                    id,
                    Disposition::Metadata(MetadataKind::GainMap),
                    "EXIF removed by the DecodePolicy, but its Apple MakerNote HDR headroom gives the reported gain-map parameters (codec.rs apple_gain_map_params)",
                );
            } else {
                m.set_item(id, Disposition::Dropped, "EXIF removed by the DecodePolicy");
            }
        }
        // Exif items `extract_exif_from_container` cannot use.
        for info in c
            .item_infos
            .iter()
            .filter(|i| i.item_type == FourCC(*b"Exif"))
        {
            if !exif_usable(c, info.item_id) {
                m.set_item(
                    info.item_id,
                    Disposition::Dropped,
                    "the 4-byte TIFF-header offset is missing or points past the item, so heic drops this EXIF",
                );
            }
        }
        // codec.rs `extract_xmp_from_container`: the first XMP mime item.
        if let Some(id) = xmp_item(c) {
            let (d, why) = if opts.keep_xmp {
                (
                    Disposition::Metadata(MetadataKind::Xmp),
                    "XMP, reported in ImageInfo (first XMP mime item; no cdsc check)",
                )
            } else {
                (Disposition::Dropped, "XMP removed by the DecodePolicy")
            };
            m.set_item(id, d, why);
        }
        m
    }

    /// decode.rs `decode_item`: an image item and the items it derives from.
    /// `overridden`: an item deriving from this one carries an nclx, which
    /// replaces this one's colour fields in the decoded frame.
    #[allow(clippy::too_many_arguments)]
    fn visit_image(
        &mut self,
        c: &HeifContainer<'_>,
        id: u32,
        depth: u32,
        role: Role,
        overridden: bool,
        data_disp: Disposition,
        why: &str,
    ) {
        if depth > crate::decode::MAX_DERIVED_DEPTH {
            self.set_item(
                id,
                Disposition::Skipped,
                "derived-image chain deeper than heic's limit; decode fails",
            );
            return;
        }
        let role_key = match role {
            Role::Primary => 0,
            Role::GainMapDecoded => 1,
            Role::DepthDecoded => 2,
            Role::Component { nclx: true } => 3,
            _ => 4,
        };
        if !self.visited.insert((id, role_key)) {
            return;
        }
        let Some(item) = c.get_item(id) else {
            return;
        };
        let dimg = FourCC(*b"dimg");
        // This item's nclx replaces its inputs' colour fields.
        let has_nclx = matches!(item.color_info, Some(ColorInfo::Nclx { .. }));
        let below = overridden || has_nclx;
        match item.item_type {
            ItemType::Grid => {
                self.set_item(id, Disposition::Structure, format!("grid ({why})"));
                // decode.rs `decode_grid` (and codec.rs's streaming grid
                // path): 8 descriptor bytes, 12 with 32-bit output sizes.
                let read = c
                    .get_item_data(id)
                    .ok()
                    .and_then(|d| d.get(1).copied())
                    .map_or(8, |flags| if flags & 1 != 0 { 12 } else { 8 });
                self.set_data(
                    id,
                    Disposition::Structure,
                    format!("grid descriptor ({why}); decode_grid reads {read} bytes"),
                    Some(read),
                );
                self.props_of(c, id, role);
                self.dimg_from.insert(id);
                let tiles = c.get_item_references(id, dimg);
                let hevc = tiles
                    .first()
                    .and_then(|&t| c.get_item(t))
                    .is_some_and(|t| t.hevc_config.is_some());
                for (i, &t) in tiles.iter().enumerate() {
                    if hevc {
                        // decode.rs `decode_grid`: HEVC tiles decode from
                        // their bytes with the first tile's configuration.
                        self.set_item(t, data_disp, format!("HEVC grid tile of item {id}"));
                        self.props_of(c, t, Role::HevcGridTile { first: i == 0 });
                        self.hevc_cfg.entry(t).or_insert(tiles[0]);
                    } else {
                        self.visit_image(
                            c,
                            t,
                            depth + 1,
                            Role::Component {
                                nclx: i == 0 && !below,
                            },
                            below || i > 0,
                            data_disp,
                            "grid tile",
                        );
                    }
                }
            }
            ItemType::Iden => {
                self.set_item(
                    id,
                    Disposition::Structure,
                    format!("identity-derived image ({why})"),
                );
                self.set_data(
                    id,
                    Disposition::Dropped,
                    "decode.rs decode_iden follows dimg and never reads the iden item's own data",
                    None,
                );
                self.props_of(c, id, role);
                self.dimg_from.insert(id);
                if let Some(&src) = c.get_item_references(id, dimg).first() {
                    self.visit_image(
                        c,
                        src,
                        depth + 1,
                        Role::Component { nclx: !below },
                        below,
                        data_disp,
                        "iden source",
                    );
                }
            }
            ItemType::Iovl => {
                self.set_item(id, Disposition::Structure, format!("overlay ({why})"));
                let inputs = c.get_item_references(id, dimg);
                // decode.rs `decode_iovl`: version/flags, four fill values,
                // the canvas size and one offset pair per input.
                let read = c.get_item_data(id).ok().and_then(|d| {
                    let off: u64 = if d.get(1)? & 1 != 0 { 4 } else { 2 };
                    let n = inputs.len() as u64;
                    Some(2 + 8 + 2 * off + n.saturating_mul(2 * off))
                });
                self.set_data(
                    id,
                    Disposition::Structure,
                    format!("overlay descriptor ({why}); decode_iovl reads its fields"),
                    read,
                );
                self.props_of(c, id, role);
                self.dimg_from.insert(id);
                for (i, src) in inputs.into_iter().enumerate() {
                    self.visit_image(
                        c,
                        src,
                        depth + 1,
                        Role::Component {
                            nclx: i == 0 && !below,
                        },
                        below || i > 0,
                        data_disp,
                        "overlay input",
                    );
                }
            }
            ItemType::Tmap => {
                self.set_item(
                    id,
                    Disposition::Structure,
                    format!("tone-map derived image, decoded as its base ({why})"),
                );
                self.set_data(
                    id,
                    Disposition::Dropped,
                    "decode.rs decode_tmap decodes the base image; the tmap payload is read only for the gain map",
                    None,
                );
                self.props_of(c, id, role);
                self.dimg_from.insert(id);
                if let Some(&base) = c.get_item_references(id, dimg).first() {
                    self.visit_image(
                        c,
                        base,
                        depth + 1,
                        Role::Component { nclx: !below },
                        below,
                        data_disp,
                        "tmap base",
                    );
                }
            }
            ItemType::Hvc1 | ItemType::Unknown(_) => {
                if item.hevc_config.is_some() || item.item_type == ItemType::Hvc1 {
                    self.set_item(id, data_disp, why);
                    self.props_of(c, id, role);
                } else {
                    self.set_item(
                        id,
                        Disposition::Skipped,
                        "item type without a decoder configuration; heic cannot decode it",
                    );
                }
            }
            ItemType::Av01 => {
                if cfg!(feature = "av1") {
                    self.set_item(id, data_disp, why);
                    self.props_of(c, id, role);
                } else {
                    self.set_item(
                        id,
                        Disposition::Skipped,
                        "AV1 item; needs heic's `av1` feature",
                    );
                }
            }
            ItemType::Unci => {
                if cfg!(feature = "unci") {
                    self.set_item(id, data_disp, why);
                    self.props_of(c, id, role);
                } else {
                    self.set_item(
                        id,
                        Disposition::Skipped,
                        "uncompressed item; needs heic's `unci` feature",
                    );
                }
            }
            ItemType::Avc1 | ItemType::Jpeg => {
                self.set_item(id, Disposition::Skipped, "heic does not decode this codec");
            }
            ItemType::Exif | ItemType::Mime => {
                self.set_item(
                    id,
                    Disposition::Skipped,
                    "metadata item referenced as an image; decode fails",
                );
            }
        }
    }

    /// parser.rs `HeifContainer::get_item`: the item's first ipma entry, in
    /// order; for each kind of property the last one wins, except `clap`,
    /// `irot` and `imir`, which all apply in order.
    fn props_of(&mut self, c: &HeifContainer<'_>, id: u32, role: Role) {
        let Some(assoc) = c.property_associations.iter().find(|a| a.item_id == id) else {
            return;
        };
        let mut slots: BTreeMap<Slot, Vec<usize>> = BTreeMap::new();
        for &(pidx, _essential) in &assoc.properties {
            if pidx == 0 {
                continue;
            }
            let i = usize::from(pidx) - 1;
            let Some(p) = c.properties.get(i) else {
                continue;
            };
            let slot = match p {
                ItemProperty::ImageExtents(_) => Slot::Ispe,
                ItemProperty::HevcConfig(_)
                | ItemProperty::Av1Config(_)
                | ItemProperty::UncompressedConfig(_)
                | ItemProperty::CompressionConfig(_) => Slot::Codec,
                ItemProperty::ColorInfo(_) => Slot::Colr,
                ItemProperty::CleanAperture(_) => Slot::Clap,
                ItemProperty::Rotation(_) => Slot::Irot,
                ItemProperty::Mirror(_) => Slot::Imir,
                ItemProperty::AuxiliaryType(_) => Slot::AuxC,
                ItemProperty::ContentLightLevel(_) => Slot::Clli,
                ItemProperty::MasteringDisplay(_) => Slot::Mdcv,
                ItemProperty::Unknown => continue,
            };
            slots.entry(slot).or_default().push(i);
        }
        for (slot, idxs) in slots {
            let all = matches!(slot, Slot::Clap | Slot::Irot | Slot::Imir);
            // Codec configs of different codecs land in different fields;
            // treat each codec field separately.
            let winners: Vec<usize> = if all {
                idxs.clone()
            } else if slot == Slot::Codec {
                let mut last: BTreeMap<u8, usize> = BTreeMap::new();
                for &i in &idxs {
                    let k = match &c.properties[i] {
                        ItemProperty::HevcConfig(_) => 0,
                        ItemProperty::Av1Config(_) => 1,
                        ItemProperty::UncompressedConfig(_) => 2,
                        _ => 3,
                    };
                    last.insert(k, i);
                }
                last.into_values().collect()
            } else {
                idxs.last().copied().into_iter().collect()
            };
            for &i in &idxs {
                if !winners.contains(&i) {
                    let last = winners.last().copied().unwrap_or(i);
                    self.set_prop(
                        i,
                        Disposition::Dropped,
                        format!(
                            "superseded for item {id} by property #{} of the same kind",
                            last + 1
                        ),
                    );
                    continue;
                }
                if let Some((d, why)) = prop_use(slot, role, &c.properties[i], &self.opts) {
                    self.set_prop(i, d, format!("item {id}: {why}"));
                }
                if slot == Slot::AuxC {
                    let (d, why) = if role == Role::DepthDecoded {
                        (
                            Disposition::Metadata(MetadataKind::Supplement),
                            "depth representation info, parsed into DepthMap::depth_info (decode.rs decode_depth)",
                        )
                    } else {
                        (
                            Disposition::Dropped,
                            "auxC subtype: heic matches only the URN for this item",
                        )
                    };
                    set(&mut self.auxc_tail, i, d, format!("item {id}: {why}"));
                }
            }
        }
    }
}

fn set<K: Ord>(map: &mut BTreeMap<K, Use>, key: K, disp: Disposition, why: String) {
    match map.get(&key) {
        Some(old) if rank(old.disp) >= rank(disp) => {}
        _ => {
            map.insert(key, Use { disp, why });
        }
    }
}

/// What a property decides for an item in `role`, or `None` when that role
/// does not consult it.
fn prop_use(
    slot: Slot,
    role: Role,
    prop: &ItemProperty,
    opts: &Options,
) -> Option<(Disposition, &'static str)> {
    use Disposition as D;
    use MetadataKind as K;
    let icc = matches!(prop, ItemProperty::ColorInfo(ColorInfo::IccProfile(_)));
    // parser.rs `find_auxiliary_items` reads auxC only for items that
    // `auxl`-reference the primary; nothing reads it for an image's own
    // decode.
    const AUXC_UNREAD: (Disposition, &str) = (
        D::Dropped,
        "auxC parsed into the item; heic reads it only for auxiliary items of the primary",
    );
    match role {
        // codec.rs `build_image_info_full` and decode.rs `decode_item`.
        Role::Primary => Some(match slot {
            Slot::Ispe => (D::Structure, "image extents"),
            Slot::Codec => (D::Structure, "decoder configuration"),
            Slot::Colr if icc && opts.keep_icc => {
                (D::Metadata(K::Icc), "ICC profile, reported in SourceColor")
            }
            Slot::Colr if icc => (D::Dropped, "ICC profile removed by the DecodePolicy"),
            Slot::Colr => (
                D::Metadata(K::Cicp),
                "nclx colour: YCbCr conversion and the reported CICP",
            ),
            Slot::Clap => (D::Structure, "clean-aperture crop applied to the pixels"),
            Slot::Irot | Slot::Imir => (
                D::Metadata(K::Orientation),
                "orientation, reported in ImageInfo (applied under OrientationHint::Correct)",
            ),
            Slot::AuxC => AUXC_UNREAD,
            Slot::Clli | Slot::Mdcv => (
                D::Metadata(K::HdrStatic),
                "HDR static metadata, reported in SourceColor",
            ),
        }),
        Role::Component { .. } | Role::GainMapDecoded | Role::DepthDecoded => Some(match slot {
            Slot::Ispe if role == Role::GainMapDecoded => {
                (D::Metadata(K::GainMap), "gain-map dimensions")
            }
            Slot::Ispe => (D::Structure, "image extents"),
            Slot::Codec => (D::Structure, "decoder configuration"),
            Slot::Colr if icc => (
                D::Dropped,
                "ICC profile of a non-primary item is not reported",
            ),
            Slot::Colr if role == Role::Component { nclx: false } => (
                D::Dropped,
                "nclx of a derived-image input that the deriving image's nclx replaces, or of an input after the first (decode_grid/decode_iovl keep the first input's range and matrix)",
            ),
            Slot::Colr => (D::Structure, "nclx colour: this item's YCbCr conversion"),
            Slot::Clap | Slot::Irot | Slot::Imir => {
                (D::Structure, "transform applied to this item's pixels")
            }
            Slot::AuxC if role == Role::DepthDecoded => {
                (D::Structure, "identifies the depth map (URN)")
            }
            Slot::AuxC if role == Role::GainMapDecoded => {
                (D::Structure, "identifies the gain map (URN)")
            }
            Slot::AuxC => AUXC_UNREAD,
            Slot::Clli | Slot::Mdcv => (
                D::Dropped,
                "HDR metadata of a non-primary item is not reported",
            ),
        }),
        // decode.rs `decode_grid`.
        Role::HevcGridTile { first } => Some(match slot {
            Slot::Ispe | Slot::Codec if first => (
                D::Structure,
                "first HEVC grid tile: its configuration and size apply to every tile",
            ),
            _ => (
                D::Dropped,
                "HEVC grid tiles decode with the first tile's configuration; this property is not consulted",
            ),
        }),
        // decode.rs `decode_alpha_plane`.
        Role::Alpha => Some(match slot {
            Slot::Ispe => (D::Structure, "alpha plane extents"),
            Slot::Codec => (D::Structure, "alpha plane decoder configuration"),
            Slot::AuxC => (D::Structure, "identifies the alpha plane (URN)"),
            _ => (D::Dropped, "not applied to the alpha plane"),
        }),
        // codec.rs `extract_gain_map_info`.
        Role::GainMapDims => match slot {
            Slot::Ispe => Some((
                D::Metadata(K::GainMap),
                "gain-map dimensions, reported in ImageInfo::gain_map",
            )),
            Slot::AuxC => Some((D::Structure, "identifies the gain map (URN)")),
            _ => None,
        },
        // codec.rs `decode_inner` → `HeicAuxiliaryInfo::auxiliary_types`.
        Role::AuxInfo => match slot {
            Slot::AuxC => Some((
                D::Structure,
                "auxiliary type (URN), reported in HeicAuxiliaryInfo",
            )),
            _ => None,
        },
    }
}

/// codec.rs `extract_exif_from_container`, returning the item it picks.
fn exif_item(c: &HeifContainer<'_>) -> Option<u32> {
    c.item_infos
        .iter()
        .find(|i| i.item_type == FourCC(*b"Exif") && exif_usable(c, i.item_id))
        .map(|i| i.item_id)
}

/// Whether `extract_exif_from_container` can use this Exif item: its data
/// resolves and its 4-byte TIFF-header offset points inside it.
fn exif_usable(c: &HeifContainer<'_>, id: u32) -> bool {
    let Ok(data) = c.get_item_data(id) else {
        return false;
    };
    if data.len() < 4 {
        return false;
    }
    let tiff_offset = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    4usize.saturating_add(tiff_offset) < data.len()
}

fn is_xmp(info: &ItemInfo) -> bool {
    info.item_type == FourCC(*b"mime")
        && (info.content_type.contains("xmp") || info.content_type.contains("rdf+xml"))
}

/// codec.rs `extract_xmp_from_container`, returning the item it picks.
fn xmp_item(c: &HeifContainer<'_>) -> Option<u32> {
    c.item_infos
        .iter()
        .find(|i| is_xmp(i) && c.get_item_data(i.item_id).is_ok())
        .map(|i| i.item_id)
}

/// parser.rs `HeifContainer::find_xmp_for_item`, returning the item it picks.
fn xmp_item_for(c: &HeifContainer<'_>, target: u32) -> Option<u32> {
    for r in &c.item_references {
        if r.reference_type != FourCC(*b"cdsc") || !r.to_item_ids.contains(&target) {
            continue;
        }
        let Some(info) = c.item_infos.iter().find(|i| i.item_id == r.from_item_id) else {
            continue;
        };
        if is_xmp(info) && c.get_item_data(r.from_item_id).is_ok() {
            return Some(r.from_item_id);
        }
    }
    None
}

/// An item no decode rule picked.
fn default_use(info: &ItemInfo) -> Use {
    let t = &info.item_type.0;
    let (disp, why) = match t {
        b"Exif" => (
            Disposition::Skipped,
            "Exif item not reported (only the first usable Exif item is)".to_string(),
        ),
        b"mime" if is_xmp(info) => (
            Disposition::Skipped,
            "XMP item not reported (only the first XMP mime item is)".to_string(),
        ),
        b"mime" => (
            Disposition::Skipped,
            "mime item; the zencodec path reads only XMP".to_string(),
        ),
        b"uri " => (
            Disposition::Skipped,
            "uri item; heic does not read it".to_string(),
        ),
        b"hvc1" | b"av01" | b"grid" | b"iovl" | b"iden" | b"tmap" | b"unci" | b"avc1" | b"jpeg"
        | b"hvt1" | b"lhv1" | b"j2k1" | b"vvc1" => (
            Disposition::Skipped,
            "image item the zencodec decode does not use".to_string(),
        ),
        _ => (
            Disposition::Unknown,
            format!("item type {} heic does not recognise", fourcc_str(t)),
        ),
    };
    Use { disp, why }
}

fn rejected_use() -> Use {
    Use {
        disp: Disposition::Dropped,
        why: "heic rejects the file, so nothing is decoded".to_string(),
    }
}

struct Cand {
    item: u32,
    k: usize,
    n: usize,
    range: Range<u64>,
    /// Offset of the extent's first byte in the item's data.
    at: u64,
    method: u8,
    disp: Disposition,
    why: String,
    label: Option<String>,
    note_to: usize,
    /// From the item's first `iloc` entry, the one heic reads.
    first_entry: bool,
}

/// Where an extent is listed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Host {
    /// Between top-level boxes.
    Top,
    Node(usize),
}

/// The children of every node at the end of the box walk, in file order.
struct Kids {
    top: Vec<u32>,
    start: Vec<u32>,
    list: Vec<u32>,
}

impl Kids {
    fn new(nodes: &[Node]) -> Self {
        let n = nodes.len();
        let mut start = vec![0u32; n + 1];
        let mut top = Vec::new();
        for (i, node) in nodes.iter().enumerate() {
            match node.parent {
                Some(p) => start[p + 1] += 1,
                None => top.push(i as u32),
            }
        }
        for i in 0..n {
            start[i + 1] += start[i];
        }
        let mut cursor = start.clone();
        let mut list = vec![0u32; start[n] as usize];
        for (i, node) in nodes.iter().enumerate() {
            if let Some(p) = node.parent {
                list[cursor[p] as usize] = i as u32;
                cursor[p] += 1;
            }
        }
        Self { top, start, list }
    }

    fn of(&self, i: usize) -> &[u32] {
        &self.list[self.start[i] as usize..self.start[i + 1] as usize]
    }
}

/// Extents that share bytes: every byte goes to the highest-ranked extent
/// covering it (the earliest-starting one on a tie), so bytes the decoder
/// reads are never listed as unreferenced. `list` is sorted by start;
/// returns `(index into list, range)` in file order.
fn sweep(list: &[Cand]) -> Vec<(usize, Range<u64>)> {
    use alloc::collections::BinaryHeap;
    use core::cmp::Reverse;
    let mut bounds: Vec<u64> = Vec::with_capacity(list.len().saturating_mul(2));
    for c in list {
        bounds.push(c.range.start);
        bounds.push(c.range.end);
    }
    bounds.sort_unstable();
    bounds.dedup();
    let mut heap: BinaryHeap<(u8, Reverse<usize>)> = BinaryHeap::new();
    let mut next = 0usize;
    let mut out: Vec<(usize, Range<u64>)> = Vec::with_capacity(list.len());
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        while next < list.len() && list[next].range.start <= a {
            heap.push((rank(list[next].disp), Reverse(next)));
            next += 1;
        }
        while let Some(&(_, Reverse(i))) = heap.peek() {
            if list[i].range.end <= a {
                heap.pop();
            } else {
                break;
            }
        }
        if let Some(&(_, Reverse(i))) = heap.peek() {
            match out.last_mut() {
                Some((j, r)) if *j == i && r.end == a => r.end = b,
                _ => out.push((i, a..b)),
            }
        }
    }
    out
}

/// parser.rs `parse_iref` over one reference box's payload: calls `f` with
/// each entry's from-ID and to-IDs. Returns how many entries `f` accepted,
/// out of how many.
fn iref_entries(c: &[u8], version: u8, mut f: impl FnMut(u32, &[u32]) -> bool) -> (usize, usize) {
    let id = if version == 0 { 2 } else { 4 };
    let read_id = |at: usize| -> u32 {
        if id == 2 {
            u32::from(u16::from_be_bytes([c[at], c[at + 1]]))
        } else {
            u32::from_be_bytes([c[at], c[at + 1], c[at + 2], c[at + 3]])
        }
    };
    let (mut yes, mut all) = (0usize, 0usize);
    let mut to: Vec<u32> = Vec::new();
    let mut pos = 0usize;
    while pos < c.len() {
        if pos + id > c.len() {
            break;
        }
        let from = read_id(pos);
        pos += id;
        if pos + 2 > c.len() {
            break;
        }
        let n = u16::from_be_bytes([c[pos], c[pos + 1]]);
        pos += 2;
        to.clear();
        for _ in 0..n {
            if pos + id > c.len() {
                break;
            }
            to.push(read_id(pos));
            pos += id;
        }
        all += 1;
        if f(from, &to) {
            yes += 1;
        }
    }
    (yes, all)
}

/// parser.rs `parse_ipma`: the item ID of every entry it records.
fn ipma_items(c: &[u8], mut f: impl FnMut(u32)) {
    let (Some(&version), Some(&flags), Some(count)) = (c.first(), c.get(3), be32(c, 4)) else {
        return;
    };
    let wide = flags & 1 != 0;
    let id = if version < 1 { 2 } else { 4 };
    let mut pos = 8usize;
    for _ in 0..count {
        if pos + id > c.len() {
            break;
        }
        let item = if id == 2 {
            u32::from(u16::from_be_bytes([c[pos], c[pos + 1]]))
        } else {
            u32::from_be_bytes([c[pos], c[pos + 1], c[pos + 2], c[pos + 3]])
        };
        pos += id;
        if pos >= c.len() {
            break;
        }
        let n = c[pos];
        pos += 1;
        for _ in 0..n {
            let w = if wide { 2 } else { 1 };
            if pos >= c.len() || pos + w > c.len() {
                break;
            }
            pos += w;
        }
        f(item);
    }
}

/// The winning `hvcC` of an item: the last `HevcConfig` of its first `ipma`
/// entry (parser.rs `get_item`), as a property index.
fn hvcc_prop(c: &HeifContainer<'_>, item: u32) -> Option<usize> {
    let assoc = c.property_associations.iter().find(|a| a.item_id == item)?;
    assoc
        .properties
        .iter()
        .filter(|(p, _)| *p > 0)
        .map(|(p, _)| usize::from(*p) - 1)
        .rfind(|&i| matches!(c.properties.get(i), Some(ItemProperty::HevcConfig(_))))
}

impl Walker<'_> {
    /// A remark built only when the part has room for it.
    fn note_with(&mut self, node: usize, f: impl FnOnce() -> String) {
        if let Some(n) = self.nodes.get_mut(node) {
            if n.notes.len() < MAX_NOTES {
                n.notes.push(f());
            } else {
                n.dropped_notes += 1;
            }
        }
    }

    /// Payload range of a box part, after its header.
    fn content_of(&self, node: usize) -> Option<Range<u64>> {
        let r = self.nodes.get(node)?.range.clone();
        match read_header(self.data, r.start, r.end) {
            HdrOutcome::Ok(h) => Some(r.start + h.base_header..r.end),
            _ => None,
        }
    }

    /// The part an extent is listed under: the `mdat`/`idat` holding it,
    /// else (parser.rs `get_item_data` reads absolute file offsets wherever
    /// they lie) the deepest leaf holding it, or the top level when it lies
    /// between top-level boxes. `None` when it straddles parts or lies in a
    /// container's header.
    fn host_of(
        &self,
        kids: &Kids,
        r: &Range<u64>,
        method: u8,
        active_idat: Option<usize>,
    ) -> Option<Host> {
        let mut level: &[u32] = &kids.top;
        let mut host = Host::Top;
        loop {
            let i = level.partition_point(|&n| self.nodes[n as usize].range.start <= r.start);
            match i.checked_sub(1).map(|j| level[j] as usize) {
                Some(n) if r.end <= self.nodes[n].range.end => {
                    let node = &self.nodes[n];
                    if node.gap_fill.is_some() {
                        let body = node.body.as_ref()?;
                        let allowed = method == 0 || Some(n) == active_idat;
                        return (allowed && body.start <= r.start && r.end <= body.end)
                            .then_some(Host::Node(n));
                    }
                    let ks = kids.of(n);
                    if ks.is_empty() {
                        return node.body.is_none().then_some(Host::Node(n));
                    }
                    level = ks;
                    host = Host::Node(n);
                }
                Some(n) if r.start < self.nodes[n].range.end => return None,
                _ => {
                    // In a gap at this level: the next part must not overlap.
                    if level
                        .get(i)
                        .is_some_and(|&n| self.nodes[n as usize].range.start < r.end)
                    {
                        return None;
                    }
                    return match host {
                        Host::Top => Some(Host::Top),
                        Host::Node(h) => self.nodes[h].body.is_none().then_some(Host::Node(h)),
                    };
                }
            }
        }
    }

    /// Second pass: dispositions that depend on the whole file.
    fn resolve(
        &mut self,
        parsed: Option<(&HeifContainer<'_>, &Model)>,
    ) -> Result<(), At<HeicError>> {
        let item_use = |id: u32, info: Option<&ItemInfo>| -> Use {
            match parsed {
                None => rejected_use(),
                Some((_, m)) => match m.items.get(&id) {
                    Some(u) => u.clone(),
                    None => match info {
                        Some(i) => default_use(i),
                        None => Use {
                            disp: Disposition::Unknown,
                            why: "no infe entry declares this item".to_string(),
                        },
                    },
                },
            }
        };

        // Item declarations (parser.rs `parse_iinf`, `HeifContainer::get_item`).
        let infes = core::mem::take(&mut self.infes);
        let mut declared: BTreeMap<u32, (Option<String>, ItemInfo)> = BTreeMap::new();
        for rec in &infes {
            let Some(info) = &rec.info else {
                self.nodes[rec.node].disp = Disposition::Malformed;
                continue;
            };
            let (disp, why) = if !rec.taken {
                (
                    Disposition::Dropped,
                    "past the iinf entry count; heic stops reading entries here".to_string(),
                )
            } else if let alloc::collections::btree_map::Entry::Vacant(slot) =
                declared.entry(info.item_id)
            {
                slot.insert((self.nodes[rec.node].label.clone(), info.clone()));
                let u = item_use(info.item_id, Some(info));
                if u.disp.is_consumed() {
                    (Disposition::Structure, u.why)
                } else {
                    (u.disp, u.why)
                }
            } else {
                (
                    Disposition::Dropped,
                    "repeats an earlier item ID; heic uses the first entry".to_string(),
                )
            };
            self.nodes[rec.node].disp = disp;
            self.note(rec.node, why);
        }
        drop(infes);

        // pitm: the last one wins (parser.rs `parse_pitm`).
        if let Some((_, earlier)) = self.pitms.clone().split_last() {
            for &p in earlier {
                self.nodes[p].disp = Disposition::Dropped;
                self.note(p, "superseded by a later pitm");
            }
        }

        // iref children (parser.rs `parse_iref` parses every type). An
        // entry is consulted when the decode follows it: `dimg` from a
        // decoded derived image, `auxl` to the primary
        // (`find_all_auxiliary_items`), `cdsc` to the attached gain map
        // (`find_xmp_for_item`).
        for (node, typ, version) in core::mem::take(&mut self.irefs) {
            let Some(content) = self.content_of(node) else {
                continue;
            };
            let c = slice(self.data, content);
            let (disp, why, counts): (Disposition, &str, Option<(usize, usize)>) = match parsed {
                None => (Disposition::Dropped, "heic rejects the file", None),
                Some((_, m)) => {
                    let (yes, all) = iref_entries(c, version, |from, to| match &typ {
                        b"dimg" => m.dimg_from.contains(&from),
                        b"auxl" => m.primary.is_some_and(|p| to.contains(&p)),
                        b"cdsc" => m
                            .cdsc_target
                            .is_some_and(|(t, attached)| attached && to.contains(&t)),
                        _ => false,
                    });
                    let (d, why) = match &typ {
                        b"dimg" if yes > 0 => (
                            Disposition::Structure,
                            "the decode follows these derived-image references",
                        ),
                        b"dimg" => (
                            Disposition::Dropped,
                            "parsed; no image the decode reads derives from the items it lists",
                        ),
                        b"auxl" if yes > 0 => (
                            Disposition::Structure,
                            "auxiliary images of the primary (find_auxiliary_items)",
                        ),
                        b"auxl" => (
                            Disposition::Dropped,
                            "parsed; it does not point at the primary item, so no auxiliary lookup matches it",
                        ),
                        b"cdsc" if yes > 0 => (
                            Disposition::Structure,
                            "consulted to find the attached gain map's XMP",
                        ),
                        b"cdsc" => (
                            Disposition::Dropped,
                            "parsed; the zencodec path picks EXIF/XMP without consulting cdsc",
                        ),
                        b"thmb" => (
                            Disposition::Dropped,
                            "parsed; thumbnails are not reported through zencodec",
                        ),
                        _ => (
                            Disposition::Dropped,
                            "parsed; heic never consults this reference type",
                        ),
                    };
                    (d, why, Some((yes, all)))
                }
            };
            self.nodes[node].disp = disp;
            self.note(node, why);
            if let Some((yes, all)) = counts
                && yes > 0
                && yes < all
            {
                self.note_with(node, || {
                    format!(
                        "{yes} of its {all} entries are consulted; the rest are parsed and unused"
                    )
                });
            }
        }

        // ipma: only an item's first entry counts (parser.rs `get_item`).
        if let Some((c, _)) = parsed {
            let mut seen: BTreeSet<u32> = BTreeSet::new();
            for node in core::mem::take(&mut self.ipmas) {
                check_stop(self.stop)?;
                let Some(content) = self.content_of(node) else {
                    continue;
                };
                let (mut entries, mut repeats) = (0usize, 0usize);
                ipma_items(slice(self.data, content), |item| {
                    entries += 1;
                    if !seen.insert(item) {
                        repeats += 1;
                    }
                });
                if entries > 0 && repeats == entries {
                    self.nodes[node].disp = Disposition::Dropped;
                    self.note(
                        node,
                        "every entry repeats an item an earlier ipma entry covers; heic uses the first",
                    );
                } else if repeats > 0 {
                    self.note_with(node, || {
                        format!("{repeats} of {entries} entries repeat an item an earlier entry covers; heic uses the first")
                    });
                }
            }
            debug_assert!(seen.len() <= c.property_associations.len());
        }

        // Properties (parser.rs `parse_property`, `HeifContainer::get_item`).
        let props = core::mem::take(&mut self.props);
        let prop_nodes: Vec<usize> = props.iter().map(|r| r.node).collect();
        let associated: BTreeSet<usize> = parsed
            .map(|(c, _)| {
                c.property_associations
                    .iter()
                    .flat_map(|a| a.properties.iter())
                    .filter(|(p, _)| *p > 0)
                    .map(|(p, _)| usize::from(*p) - 1)
                    .collect()
            })
            .unwrap_or_default();
        for rec in &props {
            self.note(rec.node, format!("property #{}", rec.index + 1));
            let recognised = matches!(
                &rec.typ,
                b"ispe"
                    | b"hvcC"
                    | b"colr"
                    | b"clap"
                    | b"irot"
                    | b"imir"
                    | b"auxC"
                    | b"clli"
                    | b"mdcv"
                    | b"av1C"
                    | b"uncC"
                    | b"cmpC"
            );
            let (disp, why): (Disposition, String) = if parsed.is_none() {
                (Disposition::Dropped, "heic rejects the file".to_string())
            } else if matches!(rec.prop, ItemProperty::Unknown) {
                if &rec.typ == b"colr" && rec.colour_type == Some(*b"rICC") {
                    (
                        Disposition::Unknown,
                        "colour type rICC: heic matches lowercase `ricc` (parser.rs parse_colr), so it treats this ICC profile as unknown".to_string(),
                    )
                } else if &rec.typ == b"colr" {
                    (
                        Disposition::Unknown,
                        "colour type heic does not recognise".to_string(),
                    )
                } else if recognised {
                    (
                        Disposition::Malformed,
                        "heic's parser rejects this property, so the decoder treats it as unknown"
                            .to_string(),
                    )
                } else {
                    (
                        Disposition::Unknown,
                        "heic has no parser for this property".to_string(),
                    )
                }
            } else if let Some(u) = parsed.and_then(|(_, m)| m.props.get(&rec.index)) {
                (u.disp, u.why.clone())
            } else if associated.contains(&rec.index) {
                (
                    Disposition::Skipped,
                    "associated only with items or roles the zencodec decode does not use"
                        .to_string(),
                )
            } else {
                (
                    Disposition::Unreferenced,
                    "no ipma entry references this property".to_string(),
                )
            };
            self.nodes[rec.node].disp = disp;
            self.note(rec.node, why);
            // auxC: version/flags, the URN up to its NUL, then subtype bytes
            // (parser.rs `parse_auxc`).
            if &rec.typ == b"auxC"
                && disp.is_consumed()
                && let Some(content) = self.content_of(rec.node)
            {
                let c = slice(self.data, content.clone());
                if let Some(nul) = c.get(4..).and_then(|u| u.iter().position(|&b| b == 0)) {
                    let urn_end = 4 + nul as u64 + 1;
                    if content.start + urn_end < content.end {
                        let (d, why) = parsed
                            .and_then(|(_, m)| m.auxc_tail.get(&rec.index))
                            .map(|u| (u.disp, u.why.clone()))
                            .unwrap_or((
                                Disposition::Dropped,
                                "auxC subtype: heic matches only the URN".to_string(),
                            ));
                        self.nodes[rec.node].inner.push((
                            content.start + urn_end..content.end,
                            Some(d),
                            why,
                        ));
                    }
                }
            }
        }
        drop(props);

        // The decoded sample entry of an image sequence (parser.rs
        // `parse_visual_sample_entry`): the last `hvcC` and the last `colr`
        // heic can parse win; that `colr` is the primary item's colour.
        if parsed.is_some() {
            if let Some((_, earlier)) = self.entry_hvcc.clone().split_last() {
                for &n in earlier {
                    self.nodes[n].disp = Disposition::Dropped;
                    self.note(
                        n,
                        "replaced by a later hvcC in the sample entry (parse_visual_sample_entry keeps the last)",
                    );
                }
            }
            let colrs = core::mem::take(&mut self.entry_colr);
            let mut winner: Option<(usize, ItemProperty)> = None;
            for &n in &colrs {
                let Some(content) = self.content_of(n) else {
                    continue;
                };
                let bmff = BmffBox {
                    header: BoxHeader {
                        box_type: FourCC(*b"colr"),
                        size: self.nodes[n].range.end - self.nodes[n].range.start,
                        content_offset: usize::try_from(content.start).unwrap_or(usize::MAX),
                    },
                    content: slice(self.data, content),
                };
                match heif::parse_property(&bmff) {
                    p @ ItemProperty::ColorInfo(_) => {
                        if let Some((old, _)) = winner.replace((n, p)) {
                            self.nodes[old].disp = Disposition::Dropped;
                            self.note(
                                old,
                                "replaced by a later colr in the sample entry (parse_visual_sample_entry keeps the last)",
                            );
                        }
                    }
                    _ => {
                        self.nodes[n].disp = Disposition::Unknown;
                        self.note(
                            n,
                            "heic's colr parser rejects it, so parse_visual_sample_entry skips it",
                        );
                    }
                }
            }
            if let (Some((n, p)), Some((_, m))) = (winner, parsed) {
                let (d, why) = match p {
                    ItemProperty::ColorInfo(ColorInfo::IccProfile(_)) if m.opts.keep_icc => (
                        Disposition::Metadata(MetadataKind::Icc),
                        "ICC profile of the decoded track, reported in SourceColor",
                    ),
                    ItemProperty::ColorInfo(ColorInfo::IccProfile(_)) => (
                        Disposition::Dropped,
                        "ICC profile removed by the DecodePolicy",
                    ),
                    _ => (
                        Disposition::Metadata(MetadataKind::Cicp),
                        "nclx colour of the decoded track: YCbCr conversion and the reported CICP",
                    ),
                };
                self.nodes[n].disp = d;
                self.note(n, why);
            }
        }

        // idat: heic keeps the last one of the top-level meta boxes
        // (parser.rs `parse_meta`).
        let active_idat = self
            .idats
            .iter()
            .rev()
            .find(|(_, top)| *top)
            .map(|&(n, _)| n);
        for (n, top) in self.idats.clone() {
            if top && Some(n) != active_idat {
                self.nodes[n].disp = Disposition::Dropped;
                self.note(n, "superseded: heic reads only the last idat");
            }
        }
        let active_idat_body = active_idat.and_then(|n| self.nodes[n].body.clone());

        // Extents.
        let file_len = self.data.len() as u64;
        // When `heif::parse` succeeded, each iloc box's locations are the
        // next run of `HeifContainer::item_locations` (parse_meta appends in
        // box order); `iloc_layout` counts them without storing anything.
        // Otherwise parse each box on its own, after heic's parse has freed
        // its copy.
        let mut sources: Vec<(usize, bool, Cow<'_, [ItemLocation]>)> = Vec::new();
        let mut next = 0usize;
        for rec in core::mem::take(&mut self.ilocs) {
            check_stop(self.stop)?;
            let data = self.data;
            let bmff = BmffBox {
                header: BoxHeader {
                    box_type: FourCC(*b"iloc"),
                    size: rec.size,
                    content_offset: usize::try_from(rec.content.start).unwrap_or(usize::MAX),
                },
                content: slice(data, rec.content.clone()),
            };
            let laid_out = match parsed {
                Some((c, _)) => heif::iloc_layout(&bmff, self.stop).map(|(n, read)| {
                    let end = next.saturating_add(n).min(c.item_locations.len());
                    let run = Cow::Borrowed(&c.item_locations[next.min(end)..end]);
                    next = end;
                    (run, read)
                }),
                None => heif::iloc_entries(&bmff, self.stop).map(|(v, read)| (Cow::Owned(v), read)),
            };
            match laid_out {
                Ok((locs, read)) => {
                    let read_end = rec.content.start.saturating_add(read as u64);
                    if read_end < rec.content.end {
                        self.nodes[rec.node].inner.push((
                            read_end..rec.content.end,
                            Some(Disposition::Dropped),
                            "after the last entry heic's iloc parser reads".to_string(),
                        ));
                    }
                    sources.push((rec.node, true, locs));
                }
                Err(e) if matches!(e.error(), HeicError::Cancelled(_)) => return Err(e),
                Err(e) => self.note(
                    rec.node,
                    format!("heic's iloc parser rejects this box: {}", e.error()),
                ),
            }
        }
        if !self.has_top_meta
            && let Some((c, _)) = parsed
            && let Some(&moov) = self
                .moovs
                .iter()
                .find(|&&n| Some(self.nodes[n].range.start) == self.used_moov_at)
        {
            // parser.rs `parse_moov`: synthetic items for the decoded sample.
            sources.push((moov, false, Cow::Borrowed(&c.item_locations[..])));
        }
        let mut cands: Vec<Cand> = Vec::new();
        let mut located: BTreeSet<u32> = BTreeSet::new();
        // Items with an extent that is not listed whole as a part: their
        // data cannot be walked.
        let mut incomplete: BTreeSet<u32> = BTreeSet::new();
        // Items whose data the decode reads only in part: bytes read.
        let mut prefixes: BTreeMap<u32, u64> = BTreeMap::new();
        for (note_to, is_iloc, locs) in sources {
            let n_ext: usize = locs.iter().map(|l| l.extents.len()).sum();
            self.note(
                note_to,
                format!("{} item locations, {n_ext} extents", locs.len()),
            );
            let mut repeats = 0usize;
            for loc in locs.iter() {
                check_stop(self.stop)?;
                let id = loc.item_id;
                let info = declared.get(&id);
                let first_entry = located.insert(id);
                let u = if !first_entry {
                    repeats += 1;
                    Use {
                        disp: Disposition::Dropped,
                        why: "repeats an earlier iloc entry for this item; heic reads the first"
                            .to_string(),
                    }
                } else if let Some(d) = parsed.and_then(|(_, m)| m.data.get(&id)) {
                    if let Some(p) = d.prefix {
                        prefixes.insert(id, p);
                    }
                    Use {
                        disp: d.disp,
                        why: d.why.clone(),
                    }
                } else {
                    item_use(id, info.map(|(_, i)| i))
                };
                let label = info.and_then(|(l, _)| l.clone()).or_else(|| {
                    parsed.and_then(|(c, _)| {
                        c.item_infos
                            .iter()
                            .find(|i| i.item_id == id)
                            .map(|i| fourcc_str(&i.item_type.0))
                    })
                });
                let src = match loc.construction_method {
                    0 => Some(0..file_len),
                    1 => active_idat_body.clone(),
                    m => {
                        self.note_with(note_to, || {
                            format!("item {id}: construction method {m} is not supported by heic; extents not placed")
                        });
                        continue;
                    }
                };
                let Some(src) = src else {
                    self.note_with(note_to, || {
                        format!("item {id}: construction method 1 but no idat for heic to read")
                    });
                    continue;
                };
                let n = loc.extents.len();
                let mut at = 0u64;
                for (k, &(off, len)) in loc.extents.iter().enumerate() {
                    let k = k + 1;
                    let here = at;
                    at = at.saturating_add(len);
                    if len == 0 {
                        self.note_with(note_to, || {
                            format!("item {id} extent {k}/{n} has length 0: heic reads nothing (ISO 14496-12 reads to the end of the source)")
                        });
                        continue;
                    }
                    let start = src
                        .start
                        .checked_add(loc.base_offset)
                        .and_then(|v| v.checked_add(off));
                    let end = start.and_then(|s| s.checked_add(len));
                    let (Some(start), Some(end)) = (start, end) else {
                        if first_entry {
                            incomplete.insert(id);
                        }
                        self.note_with(note_to, || {
                            format!("item {id} extent {k}/{n}: offset overflows")
                        });
                        continue;
                    };
                    if end > src.end {
                        if first_entry {
                            incomplete.insert(id);
                        }
                        let what = if loc.construction_method == 1 {
                            "idat"
                        } else {
                            "file"
                        };
                        self.note_with(note_to, || {
                            format!("item {id} extent {k}/{n} at {start}..{end} runs past the end of the {what} ({})", src.end)
                        });
                        continue;
                    }
                    // Every candidate becomes a part: refuse before the list
                    // outgrows the part cap (heic accepts up to 65,536 items
                    // of 1,024 extents each).
                    if cands.len() >= self.max_nodes {
                        return Err(Self::cap());
                    }
                    cands.push(Cand {
                        item: id,
                        k,
                        n,
                        range: start..end,
                        at: here,
                        method: loc.construction_method,
                        disp: u.disp,
                        why: u.why.clone(),
                        label: label.clone(),
                        note_to,
                        first_entry,
                    });
                }
            }
            if is_iloc && parsed.is_some() && repeats > 0 {
                if repeats == locs.len() {
                    self.nodes[note_to].disp = Disposition::Dropped;
                    self.note(
                        note_to,
                        "every entry repeats an item an earlier iloc entry locates; heic reads the first",
                    );
                } else {
                    self.note_with(note_to, || {
                        format!("{repeats} of {} entries repeat an item an earlier entry locates; heic reads the first", locs.len())
                    });
                }
            }
        }

        // Place each extent: binary search down the part tree.
        let mut by_host: BTreeMap<Host, Vec<Cand>> = BTreeMap::new();
        {
            let kids = Kids::new(&self.nodes);
            for cand in cands {
                check_stop(self.stop)?;
                match self.host_of(&kids, &cand.range, cand.method, active_idat) {
                    Some(h) => by_host.entry(h).or_default().push(cand),
                    None => {
                        if cand.first_entry {
                            incomplete.insert(cand.item);
                        }
                        self.note_with(cand.note_to, || {
                            format!(
                                "item {} extent {}/{} at {}..{} crosses part boundaries outside every mdat/idat; not listed as a part",
                                cand.item, cand.k, cand.n, cand.range.start, cand.range.end
                            )
                        });
                    }
                }
            }
        }

        // Consumed items' whole extents: item → (k, node, offset in the
        // item's data).
        let mut item_exts: BTreeMap<u32, Vec<(usize, usize, u64)>> = BTreeMap::new();
        for (host, mut list) in by_host {
            check_stop(self.stop)?;
            list.sort_by_key(|c| c.range.start);
            let pieces = sweep(&list);
            let mut count = vec![0u32; list.len()];
            for (i, _) in &pieces {
                count[*i] += 1;
            }
            let mut whole: Vec<bool> = count.iter().map(|&c| c == 1).collect();
            for (i, r) in &pieces {
                if *r != list[*i].range {
                    whole[*i] = false;
                }
            }
            let parent = match host {
                Host::Top => None,
                Host::Node(h) => Some(h),
            };
            let outside = parent.filter(|&h| self.nodes[h].gap_fill.is_none());
            if let Some(h) = outside {
                self.hosted.insert(h);
            }
            for (i, r) in &pieces {
                let cand = &list[*i];
                let whole = whole[*i];
                let mut node = Self::leaf(
                    parent,
                    PartKind::Extent,
                    PartTag::Code(cand.item),
                    r.clone(),
                    cand.disp,
                );
                node.label = cand.label.clone();
                node.notes.push(format!("extent {}/{}", cand.k, cand.n));
                if !whole {
                    node.notes.push(format!(
                        "bytes {}..{} of this extent at {}..{}: it overlaps another extent, and the bytes they share are listed once, under the higher-ranked use",
                        r.start, r.end, cand.range.start, cand.range.end
                    ));
                }
                node.notes.push(cand.why.clone());
                match outside {
                    Some(h) => node.notes.push(format!(
                        "outside every mdat/idat, inside {} at {}; heic reads item data at absolute offsets (parser.rs get_item_data)",
                        self.nodes[h].tag, self.nodes[h].range.start
                    )),
                    None if parent.is_none() => node.notes.push(
                        "between top-level boxes, outside every mdat/idat; heic reads item data at absolute offsets (parser.rs get_item_data)"
                            .to_string(),
                    ),
                    None => {}
                }
                if whole && cand.disp == Disposition::ImageData {
                    node.notes.push(NOT_WALKED.to_string());
                }
                let at = self.add(node)?;
                if whole && cand.first_entry && cand.disp.is_consumed() {
                    item_exts
                        .entry(cand.item)
                        .or_default()
                        .push((cand.k, at, cand.at));
                }
            }
            // Extents not listed whole: note where their bytes went.
            for (i, cand) in list.iter().enumerate() {
                if whole[i] {
                    continue;
                }
                if cand.first_entry {
                    incomplete.insert(cand.item);
                }
                // The piece holding this extent's first byte.
                let k = pieces.partition_point(|(_, r)| r.end <= cand.range.start);
                let owner = pieces.get(k).map(|(j, _)| list[*j].item);
                self.note_with(cand.note_to, || {
                    format!(
                        "item {} extent {}/{} at {}..{} overlaps another extent (item {}); the shared bytes are listed once, under the higher-ranked use",
                        cand.item,
                        cand.k,
                        cand.n,
                        cand.range.start,
                        cand.range.end,
                        owner.map_or_else(|| "?".to_string(), |o| o.to_string())
                    )
                });
            }
        }

        self.split_item_data(parsed, &declared, &item_exts, &incomplete, &prefixes)?;
        self.add_inner_parts()?;
        self.add_coded_units(parsed, &item_exts, &incomplete, &prop_nodes)?;

        if !self.moovs.is_empty() {
            for m in self.mdats.clone() {
                if self.nodes[m].body.is_some() {
                    self.nodes[m].gap_fill = Some(Disposition::Skipped);
                    self.note(
                        m,
                        "the file has a moov: bytes outside item extents may be track samples heic does not decode (sample tables are not enumerated)",
                    );
                }
            }
        }
        Ok(())
    }

    /// Child parts for bytes inside consumed leaves that heic never reads.
    fn add_inner_parts(&mut self) -> Result<(), At<HeicError>> {
        let mut has_child = vec![false; self.nodes.len()];
        for n in &self.nodes {
            if let Some(p) = n.parent {
                has_child[p] = true;
            }
        }
        for (i, &has_children) in has_child.iter().enumerate() {
            // A part with a body gets inner parts only when they tile it
            // (the EXIF split); other containers are never split here.
            if self.nodes[i].inner.is_empty() || !self.nodes[i].disp.is_consumed() || has_children {
                continue;
            }
            let parent_disp = self.nodes[i].disp;
            let inner = core::mem::take(&mut self.nodes[i].inner);
            for (r, d, why) in inner {
                let mut n = Self::leaf(
                    Some(i),
                    PartKind::Gap,
                    PartTag::None,
                    r,
                    d.unwrap_or(parent_disp),
                );
                n.notes.push(why);
                self.add(n)?;
            }
        }
        Ok(())
    }

    /// The bytes of an item's whole extents, in order.
    fn item_bytes(&self, spans: &[(usize, u64)]) -> Cow<'_, [u8]> {
        if let [(n, _)] = spans {
            return Cow::Borrowed(slice(self.data, self.nodes[*n].range.clone()));
        }
        let mut v = Vec::new();
        for (n, _) in spans {
            v.extend_from_slice(slice(self.data, self.nodes[*n].range.clone()));
        }
        Cow::Owned(v)
    }

    /// Split item data at internal ends, across however many extents it
    /// spans: descriptor bytes the decode reads (grid, iovl), the EXIF offset
    /// field and the bytes it skips, the XMP packet trailer, the ISO 21496-1
    /// payload length.
    fn split_item_data(
        &mut self,
        parsed: Option<(&HeifContainer<'_>, &Model)>,
        declared: &BTreeMap<u32, (Option<String>, ItemInfo)>,
        item_exts: &BTreeMap<u32, Vec<(usize, usize, u64)>>,
        incomplete: &BTreeSet<u32>,
        prefixes: &BTreeMap<u32, u64>,
    ) -> Result<(), At<HeicError>> {
        let Some((_, m)) = parsed else {
            return Ok(());
        };
        for (&item, exts) in item_exts {
            check_stop(self.stop)?;
            if incomplete.contains(&item) {
                continue;
            }
            let mut exts = exts.clone();
            exts.sort_unstable();
            // (node, offset in the item's data)
            let spans: Vec<(usize, u64)> = exts.iter().map(|&(_, n, at)| (n, at)).collect();
            let total = exts.last().map_or(0, |&(_, n, at)| {
                at + (self.nodes[n].range.end - self.nodes[n].range.start)
            });
            let disp = self.nodes[spans[0].0].disp;
            let typ = declared.get(&item).map(|(_, i)| i.item_type.0);
            let mut segs: Vec<(Range<u64>, Option<Disposition>, String)> = Vec::new();
            let mut tile = false;
            if let Some(&p) = prefixes.get(&item) {
                if p < total {
                    segs.push((
                        p..total,
                        Some(Disposition::Dropped),
                        format!("after the {p} descriptor bytes the decode reads"),
                    ));
                }
            } else {
                match (typ.as_ref(), disp) {
                    // codec.rs `extract_exif_from_container` reports the
                    // bytes from 4 + offset on.
                    (Some(b"Exif"), Disposition::Metadata(kind)) => {
                        let d = self.item_bytes(&spans);
                        if let Some(off) = be32(&d, 0) {
                            let tiff = 4u64.saturating_add(off);
                            if tiff < total {
                                segs.push((
                                    0..4,
                                    Some(Disposition::Structure),
                                    "exif_tiff_header_offset".to_string(),
                                ));
                                if off > 0 {
                                    segs.push((
                                        4..tiff,
                                        Some(Disposition::Dropped),
                                        "bytes the TIFF-header offset skips".to_string(),
                                    ));
                                }
                                let why = if kind == MetadataKind::Exif {
                                    "TIFF data, reported in ImageInfo"
                                } else {
                                    "TIFF data: heic reads its Apple MakerNote HDR headroom for the gain-map parameters; the EXIF itself is not reported"
                                };
                                segs.push((tiff..total, Some(disp), why.to_string()));
                                tile = true;
                            }
                        }
                    }
                    // codec.rs `extract_xmp_from_container` and decode.rs
                    // `decode_gain_map` hand over the whole item.
                    (Some(b"mime"), Disposition::Metadata(_)) => {
                        let d = self.item_bytes(&spans);
                        if let Some(end) = xmp_packet_end(&d)
                            && (end as u64) < total
                        {
                            segs.push((
                                end as u64..total,
                                None,
                                "after the XMP packet trailer, but heic hands the whole item to the caller".to_string(),
                            ));
                        }
                    }
                    (Some(b"tmap"), Disposition::Metadata(MetadataKind::GainMap)) => {
                        let d = self.item_bytes(&spans);
                        if let Some(end) = iso21496_avif_len(&d)
                            && (end as u64) < total
                        {
                            let (d, why) = if m.tmap_attached == Some(item) {
                                (
                                    None,
                                    "after the ISO 21496-1 gain-map metadata, but the whole payload is attached as HdrGainMap::iso21496",
                                )
                            } else {
                                (
                                    Some(Disposition::Dropped),
                                    "after the ISO 21496-1 gain-map metadata",
                                )
                            };
                            segs.push((end as u64..total, d, why.to_string()));
                        }
                    }
                    _ => {}
                }
            }
            if segs.is_empty() {
                continue;
            }
            for (r, d, why) in segs {
                for (k, &(n, at)) in spans.iter().enumerate() {
                    let e = spans.get(k + 1).map_or(total, |x| x.1);
                    let (a, b) = (r.start.max(at), r.end.min(e));
                    if a < b {
                        let file = self.nodes[n].range.start;
                        self.nodes[n].inner.push((
                            file + (a - at)..file + (b - at),
                            d,
                            why.clone(),
                        ));
                    }
                }
            }
            if tile {
                for &(n, _) in &spans {
                    self.nodes[n].body = Some(self.nodes[n].range.clone());
                }
            }
        }
        Ok(())
    }

    fn cap() -> At<HeicError> {
        at!(HeicError::LimitExceeded(
            "inventory exceeds the zencodec part cap"
        ))
    }

    /// Room left under the part cap.
    fn budget(&self) -> usize {
        self.max_nodes.saturating_sub(self.nodes.len())
    }

    /// Coded-unit framing: the NAL units of every `hvcC`, and of every HEVC
    /// item the decode reads, with each SEI NAL unit's messages under it.
    fn add_coded_units(
        &mut self,
        parsed: Option<(&HeifContainer<'_>, &Model)>,
        item_exts: &BTreeMap<u32, Vec<(usize, usize, u64)>>,
        incomplete: &BTreeSet<u32>,
        prop_nodes: &[usize],
    ) -> Result<(), At<HeicError>> {
        // hevc/mod.rs `decode_with_config_stop` puts the hvcC's NAL units
        // ahead of the item's, and `decode_nal_units` keeps the last SPS and
        // the last PPS: per hvcC part, (items using it, of which carry an
        // SPS, of which carry a PPS in their own data).
        let mut stream_params: BTreeMap<usize, (usize, usize, usize)> = BTreeMap::new();
        if let Some((c, m)) = parsed {
            for (&item, exts) in item_exts {
                check_stop(self.stop)?;
                let mut exts = exts.clone();
                exts.sort_unstable();
                let nodes: Vec<usize> = exts.iter().map(|&(_, n, _)| n).collect();
                let Some(info) = c.get_item(item) else {
                    continue;
                };
                let unwalked = |w: &mut Self, why: &str| {
                    for &n in &nodes {
                        w.nodes[n].notes.retain(|x| x != NOT_WALKED);
                        w.note(n, why);
                    }
                };
                if incomplete.contains(&item) {
                    unwalked(
                        self,
                        "coded units not listed: an extent of this item is not listed whole",
                    );
                    continue;
                }
                // decode.rs `decode_item`: an `hvcC` (the first tile's, for
                // an HEVC grid tile) means length-prefixed NAL units; an
                // `hvc1` item without one goes to hevc/mod.rs `decode`, which
                // takes Annex B or 4-byte lengths.
                let cfg_item = m.hevc_cfg.get(&item).copied().unwrap_or(item);
                let cfg = if cfg_item == item {
                    info.hevc_config.clone()
                } else {
                    c.get_item(cfg_item).and_then(|t| t.hevc_config)
                };
                // The hvcC part that configuration came from: an ipco
                // property, or the decoded sample entry's (synthetic items
                // of an image sequence have no ipco property).
                let cfg_node = match hvcc_prop(c, cfg_item) {
                    Some(p) => prop_nodes.get(p).copied(),
                    None if cfg.is_some() => self.entry_hvcc.last().copied(),
                    None => None,
                };
                let stream: Cow<'_, [u8]> = if let [n] = nodes[..] {
                    Cow::Borrowed(slice(self.data, self.nodes[n].range.clone()))
                } else {
                    let mut v = Vec::new();
                    for &n in &nodes {
                        v.extend_from_slice(slice(self.data, self.nodes[n].range.clone()));
                    }
                    Cow::Owned(v)
                };
                let length_size = match (&cfg, info.item_type) {
                    (Some(cfg), _) => usize::from(cfg.length_size_minus_one) + 1,
                    (None, ItemType::Hvc1)
                        if stream.starts_with(&[0, 0, 1]) || stream.starts_with(&[0, 0, 0, 1]) =>
                    {
                        unwalked(
                            self,
                            "Annex B stream (hevc/mod.rs decode): coded units not listed",
                        );
                        continue;
                    }
                    (None, ItemType::Hvc1) => 4,
                    (None, ItemType::Av01) => {
                        unwalked(
                            self,
                            "AV1 item: OBU framing is not walked by heic's inventory",
                        );
                        continue;
                    }
                    _ => {
                        for &n in &nodes {
                            self.nodes[n].notes.retain(|x| x != NOT_WALKED);
                        }
                        continue;
                    }
                };
                let slice_disp = self.nodes[nodes[0]].disp;
                let (units, end) = length_prefixed_units(&stream, length_size, self.budget())
                    .ok_or_else(Self::cap)?;
                let last = last_parameter_sets(&units);
                if let Some(h) = cfg_node {
                    let e = stream_params.entry(h).or_default();
                    e.0 += 1;
                    e.1 += usize::from(last.sps.is_some());
                    e.2 += usize::from(last.pps.is_some());
                }
                // Where each extent sits in the item's byte stream.
                let mut spans: Vec<(usize, usize, u64)> = Vec::with_capacity(nodes.len());
                let mut at = 0usize;
                for &n in &nodes {
                    let r = self.nodes[n].range.clone();
                    spans.push((n, at, r.start));
                    at += (r.end - r.start) as usize;
                    self.nodes[n].body = Some(r);
                    self.nodes[n].notes.retain(|x| x != NOT_WALKED);
                    let how =
                        format!("{length_size}-byte NAL unit lengths (hvcC lengthSizeMinusOne)");
                    self.note(n, how);
                }
                let pieces_of = |range: Range<usize>| -> Vec<(usize, Range<u64>)> {
                    let mut out = Vec::new();
                    for (k, &(n, s, file)) in spans.iter().enumerate() {
                        let e = spans.get(k + 1).map_or(stream.len(), |x| x.1);
                        let (a, b) = (range.start.max(s), range.end.min(e));
                        if a < b {
                            out.push((n, file + (a - s) as u64..file + (b - s) as u64));
                        }
                    }
                    out
                };
                for (j, rec) in units.iter().enumerate() {
                    let (disp, why) =
                        nal_disposition(rec.shape, slice_disp, last.superseded(j, rec));
                    let pieces = pieces_of(rec.range.clone());
                    self.add_nal(&stream, rec, &pieces, disp, why)?;
                }
                if end < stream.len() {
                    for (n, r) in pieces_of(end..stream.len()) {
                        let mut g = Self::leaf(
                            Some(n),
                            PartKind::Gap,
                            PartTag::None,
                            r,
                            Disposition::Unreferenced,
                        );
                        g.notes.push(format!(
                            "after the last NAL unit: fewer bytes than a {length_size}-byte length, which heic does not read"
                        ));
                        self.add(g)?;
                    }
                }
            }
        }

        // `hvcC` payloads: parser.rs `parse_hvcc` keeps every NAL unit, and
        // hevc/mod.rs `decode_with_config_stop` hands them all to the
        // decoder ahead of the item data. A part that holds an item extent
        // (one lying outside every mdat/idat) is not split further.
        let hvccs: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| {
                let n = &self.nodes[i];
                n.tag == PartTag::FourCc(*b"hvcC")
                    && matches!(n.kind, PartKind::Property | PartKind::Box)
                    && n.disp != Disposition::Malformed
                    && !self.hosted.contains(&i)
            })
            .collect();
        for i in hvccs {
            check_stop(self.stop)?;
            let r = self.nodes[i].range.clone();
            let HdrOutcome::Ok(h) = read_header(self.data, r.start, r.end) else {
                continue;
            };
            let base = r.start + h.header;
            let c = slice(self.data, base..r.end);
            let (units, _) = hvcc_units(c, self.budget()).ok_or_else(Self::cap)?;
            let parent_disp = self.nodes[i].disp;
            let last = last_parameter_sets(&units);
            // Replaced for every item that uses this hvcC.
            let (users, with_sps, with_pps) = stream_params.get(&i).copied().unwrap_or_default();
            let sps_replaced = users > 0 && with_sps == users;
            let pps_replaced = users > 0 && with_pps == users;
            for (j, rec) in units.iter().enumerate() {
                let (disp, why) = if !parent_disp.is_consumed() {
                    (
                        parent_disp,
                        "in an hvcC the decode does not use".to_string(),
                    )
                } else if let NalShape::Overrun { declared } = rec.shape {
                    (
                        Disposition::Malformed,
                        format!(
                            "length {declared} runs past the hvcC; parse_hvcc skips the rest of this array and reads the next array header right after this length"
                        ),
                    )
                } else {
                    let by_item = match rec.shape {
                        NalShape::Unit { typ: 33, .. } => sps_replaced,
                        NalShape::Unit { typ: 34, .. } => pps_replaced,
                        _ => false,
                    };
                    let (d, why) = nal_disposition(
                        rec.shape,
                        Disposition::ImageData,
                        by_item || last.superseded(j, rec),
                    );
                    if by_item {
                        (
                            d,
                            "parsed, then replaced by a later one of the same type in the item data (heic keeps the last)".to_string(),
                        )
                    } else {
                        (d, why)
                    }
                };
                let pieces = [(
                    i,
                    base + rec.range.start as u64..base + rec.range.end as u64,
                )];
                self.add_nal(c, rec, &pieces, disp, why)?;
            }
        }
        Ok(())
    }

    /// One NAL unit as a `CodedUnit` part per extent it lies in (`pieces`:
    /// parent node and file range), and a SEI unit's messages under it.
    /// `d` holds the bytes `rec` indexes.
    fn add_nal(
        &mut self,
        d: &[u8],
        rec: &NalRec,
        pieces: &[(usize, Range<u64>)],
        disp: Disposition,
        why: String,
    ) -> Result<(), At<HeicError>> {
        let nal = &d[(rec.range.start + rec.prefix).min(rec.range.end)..rec.range.end];
        let (tag, label) = match rec.shape {
            NalShape::Unit { typ, .. } | NalShape::BadHeader { typ } => (
                PartTag::Code(u32::from(typ)),
                Some(nal_name(typ).to_string()),
            ),
            NalShape::Overrun { .. } if nal.len() >= 2 => {
                let typ = (nal[0] >> 1) & 0x3F;
                (
                    PartTag::Code(u32::from(typ)),
                    Some(nal_name(typ).to_string()),
                )
            }
            _ => (PartTag::None, None),
        };
        let size = match rec.shape {
            NalShape::Overrun { declared } => format!(
                "{}-byte length field declaring {declared} bytes",
                rec.prefix
            ),
            _ => format!("{}-byte length + {}-byte NAL unit", rec.prefix, nal.len()),
        };
        let layer = match rec.shape {
            NalShape::Unit { layer, .. } if layer != 0 => format!("; nuh_layer_id {layer}"),
            _ => String::new(),
        };
        let split = pieces.len() > 1;
        let mut first = None;
        for (k, (parent, r)) in pieces.iter().enumerate() {
            let mut n = Self::leaf(
                Some(*parent),
                PartKind::CodedUnit,
                tag.clone(),
                r.clone(),
                disp,
            );
            n.label = label.clone();
            n.notes.push(format!("{size}{layer}; {why}"));
            if split {
                n.notes.push(format!(
                    "piece {}/{} of a NAL unit that spans item extents",
                    k + 1,
                    pieces.len()
                ));
            }
            let at = self.add(n)?;
            first.get_or_insert(at);
        }
        let sei = matches!(rec.shape, NalShape::Unit { typ: 39 | 40, .. });
        if let (true, false, Some(parent)) = (sei, split, first) {
            let base = pieces[0].1.start + rec.prefix as u64;
            let msgs = sei_messages(nal, self.budget()).ok_or_else(Self::cap)?;
            for msg in msgs {
                let r = base + msg.range.start as u64..base + msg.range.end as u64;
                if r.start >= r.end {
                    continue;
                }
                let name = sei_name(msg.payload_type);
                let (d, what) = if msg.overrun {
                    (
                        Disposition::Malformed,
                        format!(
                            "sei_message payloadType {} payloadSize {} runs past the NAL unit",
                            msg.payload_type, msg.payload_size
                        ),
                    )
                } else {
                    (
                        disp,
                        format!(
                            "sei_message payloadType {} ({}), payloadSize {}",
                            msg.payload_type,
                            name.unwrap_or("unnamed"),
                            msg.payload_size
                        ),
                    )
                };
                let mut n = Self::leaf(
                    Some(parent),
                    PartKind::CodedUnit,
                    PartTag::Code(u32::try_from(msg.payload_type).unwrap_or(u32::MAX)),
                    r,
                    d,
                );
                n.label = msg
                    .label
                    .or_else(|| name.map(str::to_string))
                    .or_else(|| Some(format!("sei_payload_{}", msg.payload_type)));
                n.notes.push(what);
                self.add(n)?;
            }
        }
        Ok(())
    }

    fn emit(self) -> Result<Inventory, At<HeicError>> {
        fn cap(_: InventoryError) -> At<HeicError> {
            at!(HeicError::LimitExceeded(
                "inventory exceeds the zencodec part cap"
            ))
        }
        let mut inv = Inventory::new(ImageFormat::Heic, self.data.len() as u64);
        let mut ids: Vec<PartId> = Vec::with_capacity(self.nodes.len());
        let mut fills: Vec<(PartId, Disposition)> = Vec::new();
        // Consume the nodes, so each one's strings move into its part (or
        // are freed) instead of being copied: the walk never holds two full
        // copies of the part list.
        for n in self.nodes {
            let mut part = Part::new(n.kind, n.tag, n.range, n.disp);
            if let Some(l) = n.label {
                part = part.with_label(l);
            }
            let has_body = n.body.is_some();
            if let Some(b) = n.body {
                part = part.with_body(b);
            }
            if !n.notes.is_empty() || n.dropped_notes > 0 {
                let mut d = n.notes.join("; ");
                if n.dropped_notes > 0 {
                    d.push_str(&format!("; {} more remarks", n.dropped_notes));
                }
                part = part.with_detail(d);
            }
            let parent = n.parent.map(|p| ids[p]);
            let id = inv.push(parent, part).map_err(cap)?;
            ids.push(id);
            if let Some(d) = n.gap_fill
                && has_body
            {
                fills.push((id, d));
            }
        }
        drop(ids);
        for (id, d) in fills {
            inv.fill_gaps(Some(id), d).map_err(cap)?;
        }
        inv.fill_gaps(None, Disposition::Trailing).map_err(cap)?;
        Ok(inv)
    }
}

/// End of an XMP packet: just past `?>` of the `<?xpacket end=…?>` trailer.
fn xmp_packet_end(d: &[u8]) -> Option<usize> {
    let marker = b"<?xpacket end=";
    let at = d.windows(marker.len()).rposition(|w| w == marker)?;
    let close = d[at..].windows(2).position(|w| w == b"?>")?;
    Some(at + close + 2)
}

/// Length of an ISO 21496-1 payload in the AVIF `tmap` form (version byte
/// first), as zencodec's `parse_iso21496_fmt(.., AvifTmap)` reads it.
fn iso21496_avif_len(d: &[u8]) -> Option<usize> {
    // version(1) minimum_version(2) writer_version(2) flags(1)
    let flags = *d.get(5)?;
    let channels = if flags & 0x80 != 0 { 3 } else { 1 };
    let common_denominator = flags & 0x08 != 0;
    let body = if common_denominator {
        4 + 8 + 20 * channels
    } else {
        16 + 40 * channels
    };
    Some(6 + body)
}

// ── HEVC coded units ─────────────────────────────────────────────────────

/// What heic makes of one length-prefixed unit (bitstream.rs
/// `parse_length_prefixed_ext` and `parse_nal_header`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NalShape {
    /// A NAL unit whose header `parse_nal_header` accepts.
    Unit { typ: u8, layer: u8 },
    /// Shorter than the 2-byte NAL unit header: skipped without a word.
    Short,
    /// `parse_nal_header` rejects the header (forbidden_zero_bit set, or
    /// nuh_temporal_id_plus1 = 0): skipped without a word.
    BadHeader { typ: u8 },
    /// The length runs past the data: the decode fails here.
    Overrun { declared: u64 },
}

/// One length-prefixed NAL unit, offsets relative to the start of the data
/// it was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
struct NalRec {
    /// The length field and the NAL unit.
    range: Range<usize>,
    /// Bytes of length field at the start of `range`.
    prefix: usize,
    shape: NalShape,
}

fn nal_shape(nal: &[u8]) -> NalShape {
    let (Some(&b0), Some(&b1)) = (nal.first(), nal.get(1)) else {
        return NalShape::Short;
    };
    let typ = (b0 >> 1) & 0x3F;
    if b0 & 0x80 != 0 || b1 & 0x07 == 0 {
        return NalShape::BadHeader { typ };
    }
    NalShape::Unit {
        typ,
        layer: ((b0 & 1) << 5) | (b1 >> 3),
    }
}

/// bitstream.rs `parse_length_prefixed_ext`: the units it reads and where it
/// stops (fewer bytes than a length field remain, or a length overruns).
/// `None` when there are more than `limit` units.
fn length_prefixed_units(
    d: &[u8],
    length_size: usize,
    limit: usize,
) -> Option<(Vec<NalRec>, usize)> {
    let mut out = Vec::new();
    if !(1..=4).contains(&length_size) {
        return Some((out, 0));
    }
    let mut i = 0usize;
    while d.len() - i >= length_size {
        if out.len() >= limit {
            return None;
        }
        let declared = d[i..i + length_size]
            .iter()
            .fold(0u64, |n, &b| (n << 8) | u64::from(b));
        let body = i + length_size;
        match usize::try_from(declared)
            .ok()
            .and_then(|n| body.checked_add(n))
            .filter(|&e| e <= d.len())
        {
            Some(end) => {
                out.push(NalRec {
                    range: i..end,
                    prefix: length_size,
                    shape: nal_shape(&d[body..end]),
                });
                i = end;
            }
            None => {
                out.push(NalRec {
                    range: i..d.len(),
                    prefix: length_size,
                    shape: NalShape::Overrun { declared },
                });
                return Some((out, d.len()));
            }
        }
    }
    Some((out, i))
}

/// parser.rs `parse_hvcc`: the NAL units of the `hvcC` arrays (each with its
/// 2-byte length) and where its reading stops, relative to the box payload.
/// heic keeps every unit and hands them to the decoder ahead of the item
/// data. `None` when there are more than `limit` units.
fn hvcc_units(c: &[u8], limit: usize) -> Option<(Vec<NalRec>, usize)> {
    let mut out = Vec::new();
    let Some(&num_arrays) = c.get(22) else {
        return Some((out, c.len()));
    };
    let mut pos = 23usize;
    'arrays: for _ in 0..num_arrays {
        if pos + 3 > c.len() {
            break;
        }
        let n = u16::from_be_bytes([c[pos + 1], c[pos + 2]]);
        pos += 3;
        for _ in 0..n {
            if pos + 2 > c.len() {
                break;
            }
            if out.len() >= limit {
                return None;
            }
            let l = usize::from(u16::from_be_bytes([c[pos], c[pos + 1]]));
            if pos + 2 + l > c.len() {
                // parse_hvcc skips the length field, abandons this array and
                // reads the next array header right after the length.
                out.push(NalRec {
                    range: pos..pos + 2,
                    prefix: 2,
                    shape: NalShape::Overrun { declared: l as u64 },
                });
                pos += 2;
                continue 'arrays;
            }
            out.push(NalRec {
                range: pos..pos + 2 + l,
                prefix: 2,
                shape: nal_shape(&c[pos + 2..pos + 2 + l]),
            });
            pos += 2 + l;
        }
    }
    Some((out, pos))
}

/// The last SPS and PPS of a NAL unit list: heic's `decode_nal_units`
/// keeps the last of each, whatever its ID.
struct LastParams {
    sps: Option<usize>,
    pps: Option<usize>,
}

impl LastParams {
    fn superseded(&self, j: usize, rec: &NalRec) -> bool {
        match rec.shape {
            NalShape::Unit { typ: 33, .. } => self.sps.is_some_and(|l| j < l),
            NalShape::Unit { typ: 34, .. } => self.pps.is_some_and(|l| j < l),
            _ => false,
        }
    }
}

fn last_parameter_sets(units: &[NalRec]) -> LastParams {
    let last = |t: u8| {
        units
            .iter()
            .rposition(|r| matches!(r.shape, NalShape::Unit { typ, .. } if typ == t))
    };
    LastParams {
        sps: last(33),
        pps: last(34),
    }
}

/// The disposition of one NAL unit, following hevc/mod.rs
/// `decode_with_config_stop` / `decode_nal_units` (the pure-Rust decoder).
/// `slice_disp` is what the decoded picture is (image data, a gain map, a
/// depth map); `superseded` marks a parameter set a later one of the same
/// type replaces.
fn nal_disposition(
    shape: NalShape,
    slice_disp: Disposition,
    superseded: bool,
) -> (Disposition, String) {
    use Disposition as D;
    let (d, why): (Disposition, &str) = match shape {
        NalShape::Overrun { declared } => {
            return (
                D::Malformed,
                format!(
                    "length {declared} runs past the data; heic's decode fails here (bitstream.rs parse_length_prefixed_ext)"
                ),
            );
        }
        NalShape::Short => (D::Dropped, "shorter than a NAL unit header; heic skips it"),
        NalShape::BadHeader { .. } => (
            D::Dropped,
            "forbidden_zero_bit set or nuh_temporal_id_plus1 = 0; heic skips it",
        ),
        NalShape::Unit { typ, layer } => match typ {
            0..=9 | 16..=21 if layer == 0 => (slice_disp, "slice segment heic decodes"),
            0..=9 | 16..=21 => (
                D::Skipped,
                "slice of an enhancement layer; heic decodes layer 0 only",
            ),
            10..=15 | 22..=31 => (D::Unknown, "reserved VCL NAL unit type; heic ignores it"),
            32 => (
                D::Structure,
                "parsed (a malformed VPS fails the decode); heic uses none of its values",
            ),
            33 | 34 if superseded => (
                D::Dropped,
                "parsed, then replaced by a later one of the same type (heic keeps the last)",
            ),
            33 | 34 => (D::Structure, "parameter set the decode uses"),
            35..=37 => (D::Skipped, "heic ignores it"),
            38 => (D::Padding, "filler data"),
            39 | 40 => (D::Skipped, "heic does not read SEI messages"),
            _ => (
                D::Unknown,
                "reserved or unspecified NAL unit type; heic ignores it",
            ),
        },
    };
    (d, why.to_string())
}

/// H.265 Table 7-1 names.
fn nal_name(typ: u8) -> &'static str {
    const NAMES: [&str; 64] = [
        "TRAIL_N",
        "TRAIL_R",
        "TSA_N",
        "TSA_R",
        "STSA_N",
        "STSA_R",
        "RADL_N",
        "RADL_R",
        "RASL_N",
        "RASL_R",
        "RSV_VCL_N10",
        "RSV_VCL_R11",
        "RSV_VCL_N12",
        "RSV_VCL_R13",
        "RSV_VCL_N14",
        "RSV_VCL_R15",
        "BLA_W_LP",
        "BLA_W_RADL",
        "BLA_N_LP",
        "IDR_W_RADL",
        "IDR_N_LP",
        "CRA_NUT",
        "RSV_IRAP_VCL22",
        "RSV_IRAP_VCL23",
        "RSV_VCL24",
        "RSV_VCL25",
        "RSV_VCL26",
        "RSV_VCL27",
        "RSV_VCL28",
        "RSV_VCL29",
        "RSV_VCL30",
        "RSV_VCL31",
        "VPS_NUT",
        "SPS_NUT",
        "PPS_NUT",
        "AUD_NUT",
        "EOS_NUT",
        "EOB_NUT",
        "FD_NUT",
        "PREFIX_SEI_NUT",
        "SUFFIX_SEI_NUT",
        "RSV_NVCL41",
        "RSV_NVCL42",
        "RSV_NVCL43",
        "RSV_NVCL44",
        "RSV_NVCL45",
        "RSV_NVCL46",
        "RSV_NVCL47",
        "UNSPEC48",
        "UNSPEC49",
        "UNSPEC50",
        "UNSPEC51",
        "UNSPEC52",
        "UNSPEC53",
        "UNSPEC54",
        "UNSPEC55",
        "UNSPEC56",
        "UNSPEC57",
        "UNSPEC58",
        "UNSPEC59",
        "UNSPEC60",
        "UNSPEC61",
        "UNSPEC62",
        "UNSPEC63",
    ];
    NAMES[usize::from(typ & 0x3F)]
}

/// H.265 Annex D `sei_payload` names for the types an auditor is likely to
/// meet.
fn sei_name(t: u64) -> Option<&'static str> {
    Some(match t {
        0 => "buffering_period",
        1 => "pic_timing",
        2 => "pan_scan_rect",
        3 => "filler_payload",
        4 => "user_data_registered_itu_t_t35",
        5 => "user_data_unregistered",
        6 => "recovery_point",
        9 => "scene_info",
        15 => "picture_snapshot",
        16 => "progressive_refinement_segment_start",
        17 => "progressive_refinement_segment_end",
        19 => "film_grain_characteristics",
        22 => "post_filter_hint",
        23 => "tone_mapping_info",
        45 => "frame_packing_arrangement",
        47 => "display_orientation",
        56 => "green_metadata",
        128 => "structure_of_pictures_info",
        129 => "active_parameter_sets",
        130 => "decoding_unit_info",
        131 => "temporal_sub_layer_zero_idx",
        132 => "decoded_picture_hash",
        133 => "scalable_nesting",
        134 => "region_refresh_info",
        135 => "no_display",
        136 => "time_code",
        137 => "mastering_display_colour_volume",
        138 => "segmented_rect_frame_packing_arrangement",
        139 => "temporal_motion_constrained_tile_sets",
        140 => "chroma_resampling_filter_hint",
        141 => "knee_function_info",
        142 => "colour_remapping_info",
        143 => "deinterlaced_field_identification",
        144 => "content_light_level_info",
        145 => "dependent_rap_indication",
        146 => "coded_region_completion",
        147 => "alternative_transfer_characteristics",
        148 => "ambient_viewing_environment",
        149 => "content_colour_volume",
        150 => "equirectangular_projection",
        151 => "cubemap_projection",
        152 => "fisheye_video_info",
        154 => "sphere_rotation",
        155 => "regionwise_packing",
        156 => "omni_viewport",
        157 => "regional_nesting",
        165 => "alpha_channel_info",
        166 => "overlay_info",
        177 => "depth_representation_info",
        200 => "sei_manifest",
        201 => "sei_prefix_indication",
        202 => "annotated_regions",
        205 => "shutter_interval_info",
        _ => return None,
    })
}

/// One `sei_message` of a SEI NAL unit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SeiMsg {
    /// Raw (escaped) bytes of the message within the NAL unit, header
    /// included in the offsets.
    range: Range<usize>,
    payload_type: u64,
    payload_size: u64,
    /// The payload or its size fields run past the NAL unit.
    overrun: bool,
    /// The UUID of `user_data_unregistered`, the country and provider code
    /// of `user_data_registered_itu_t_t35`.
    label: Option<String>,
}

/// H.265 7.3.2.4 `sei_rbsp` (7.3.5 `sei_message`) over a SEI NAL unit,
/// 2-byte header included. heic reads none of this; it is for the
/// inventory only. `None` when there are more than `limit` messages.
fn sei_messages(nal: &[u8], limit: usize) -> Option<Vec<SeiMsg>> {
    // Emulation prevention, as bitstream.rs `remove_emulation_prevention`
    // removes it; `at[i]` is the raw index of RBSP byte `i`.
    let raw = nal.get(2..).unwrap_or(&[]);
    let mut rbsp = Vec::with_capacity(raw.len());
    let mut at = Vec::with_capacity(raw.len());
    let mut i = 0usize;
    while i < raw.len() {
        if i + 2 < raw.len() && raw[i] == 0 && raw[i + 1] == 0 && raw[i + 2] == 3 {
            rbsp.extend_from_slice(&[0, 0]);
            at.extend_from_slice(&[i, i + 1]);
            i += 3;
        } else {
            rbsp.push(raw[i]);
            at.push(i);
            i += 1;
        }
    }
    let raw_end = |r: usize| -> usize { 2 + at.get(r).map_or(raw.len(), |&a| a) };
    // The rbsp_stop_one_bit is the last set bit.
    let Some(last) = rbsp.iter().rposition(|&b| b != 0) else {
        return Some(Vec::new());
    };
    let ff = |p: &mut usize| -> Option<u64> {
        let mut v = 0u64;
        loop {
            let b = *rbsp.get(*p)?;
            *p += 1;
            v = v.saturating_add(u64::from(b));
            if b != 0xFF {
                return Some(v);
            }
        }
    };
    let mut out = Vec::new();
    let mut p = 0usize;
    // more_rbsp_data(): more than the stop bit and its alignment zeros.
    while p < last || (p == last && rbsp[p] != 0x80) {
        if out.len() >= limit {
            return None;
        }
        let start = p;
        let (Some(payload_type), Some(payload_size)) = (ff(&mut p), ff(&mut p)) else {
            out.push(SeiMsg {
                range: raw_end(start)..nal.len(),
                payload_type: 0,
                payload_size: 0,
                overrun: true,
                label: None,
            });
            break;
        };
        let body = p;
        let end = usize::try_from(payload_size)
            .ok()
            .and_then(|n| body.checked_add(n))
            .filter(|&e| e <= rbsp.len());
        let Some(end) = end else {
            out.push(SeiMsg {
                range: raw_end(start)..nal.len(),
                payload_type,
                payload_size,
                overrun: true,
                label: None,
            });
            break;
        };
        let payload = &rbsp[body..end];
        let label = match payload_type {
            5 => payload
                .get(..16)
                .and_then(|u| <[u8; 16]>::try_from(u).ok())
                .map(|u| uuid_str(&u)),
            4 => payload.first().map(|&cc| {
                let rest = if cc == 0xFF {
                    &payload[1.min(payload.len())..]
                } else {
                    payload
                };
                let ext = if cc == 0xFF {
                    payload.get(1).copied()
                } else {
                    None
                };
                let provider = rest
                    .get(1..3)
                    .map(|b| format!(" provider {:#06x}", u16::from_be_bytes([b[0], b[1]])))
                    .unwrap_or_default();
                match ext {
                    Some(x) => format!("itu_t_t35 country 0xff{x:02x}{provider}"),
                    None => format!("itu_t_t35 country {cc:#04x}{provider}"),
                }
            }),
            _ => None,
        };
        // The last payload byte's raw position, plus one.
        let raw_stop = if end > start {
            raw_end(end - 1) + 1
        } else {
            raw_end(start)
        };
        out.push(SeiMsg {
            range: raw_end(start)..raw_stop,
            payload_type,
            payload_size,
            overrun: false,
            label,
        });
        p = end;
    }
    Some(out)
}
