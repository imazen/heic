//! HEIF container parser

#![allow(dead_code)]

use alloc::borrow::Cow;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::str;

use enough::Stop;

use super::boxes::{
    Av1DecoderConfig, Box, BoxIterator, CleanAperture, ColorInfo, CompressionConfig,
    ContentLightLevelBox, FourCC, HevcDecoderConfig, ImageMirror, ImageRotation,
    ImageSpatialExtents, ItemInfo, ItemLocation, ItemProperty, ItemReference, MasteringDisplayBox,
    PropertyAssociation, Transform, UncompressedComponent, UncompressedConfig,
};
use whereat::at;

use crate::error::{HeicError, Result, check_stop};

// ---------------------------------------------------------------------------
// Resource limits — prevent unbounded allocation from adversarial input
// ---------------------------------------------------------------------------

const MAX_ITEMS: u32 = 65_536;
const MAX_PROPERTIES: u32 = 65_536;
const MAX_EXTENTS_PER_ITEM: u32 = 1_024;
const MAX_REFERENCES: u32 = 65_536;
const MAX_REFS_PER_ENTRY: u32 = 4_096;
const MAX_COMPATIBLE_BRANDS: usize = 256;
const MAX_STRING_LENGTH: usize = 4_096;
const MAX_NAL_UNIT_SIZE: usize = 16 * 1024 * 1024; // 16 MiB
const MAX_ICC_PROFILE_SIZE: usize = 4 * 1024 * 1024; // 4 MiB

/// Parsed HEIF container
#[derive(Debug)]
pub struct HeifContainer<'a> {
    /// Raw file data
    data: &'a [u8],
    /// File type brand
    pub brand: FourCC,
    /// Compatible brands
    pub compatible_brands: Vec<FourCC>,
    /// Primary item ID
    pub primary_item_id: u32,
    /// Item locations
    pub item_locations: Vec<ItemLocation>,
    /// Item info entries
    pub item_infos: Vec<ItemInfo>,
    /// Item properties in order (1-based indexing in ipma, 0-based here)
    pub properties: Vec<ItemProperty>,
    /// Property associations
    pub property_associations: Vec<PropertyAssociation>,
    /// Item references (from iref box)
    pub item_references: Vec<ItemReference>,
    /// Item data (from idat box inside meta)
    idat_data: Option<&'a [u8]>,
    /// Media data offset
    mdat_offset: Option<usize>,
    /// Media data length
    mdat_length: Option<usize>,
}

/// Item type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ItemType {
    /// HEVC coded image
    Hvc1,
    /// AV1 coded image
    Av01,
    /// H.264/AVC coded image
    Avc1,
    /// JPEG coded image
    Jpeg,
    /// Uncompressed/generic compressed (ISO 23001-17)
    Unci,
    /// Image grid
    Grid,
    /// Image overlay
    Iovl,
    /// Identity transform
    Iden,
    /// EXIF metadata
    Exif,
    /// MIME data
    Mime,
    /// Unknown type
    Unknown(FourCC),
}

impl From<FourCC> for ItemType {
    fn from(fourcc: FourCC) -> Self {
        match &fourcc.0 {
            b"hvc1" => Self::Hvc1,
            b"av01" => Self::Av01,
            b"avc1" => Self::Avc1,
            b"jpeg" => Self::Jpeg,
            b"unci" => Self::Unci,
            b"grid" => Self::Grid,
            b"iovl" => Self::Iovl,
            b"iden" => Self::Iden,
            b"Exif" => Self::Exif,
            b"mime" => Self::Mime,
            _ => Self::Unknown(fourcc),
        }
    }
}

/// Parsed item with resolved properties
#[derive(Debug)]
pub struct Item {
    /// Item ID
    pub id: u32,
    /// Item type
    pub item_type: ItemType,
    /// Item name
    pub name: String,
    /// Image dimensions (if available)
    pub dimensions: Option<(u32, u32)>,
    /// HEVC config (if available)
    pub hevc_config: Option<HevcDecoderConfig>,
    /// AV1 config (if available)
    pub av1_config: Option<Av1DecoderConfig>,
    /// Uncompressed codec config (if available)
    pub uncompressed_config: Option<UncompressedConfig>,
    /// Generic compression config (if available)
    pub compression_config: Option<CompressionConfig>,
    /// Clean aperture crop (if available)
    pub clean_aperture: Option<CleanAperture>,
    /// Image rotation (if available)
    pub rotation: Option<ImageRotation>,
    /// Image mirror (if available)
    pub mirror: Option<ImageMirror>,
    /// Ordered transformative properties (clap, imir, irot) in ipma order.
    /// HEIF spec requires these be applied in listing order.
    pub transforms: Vec<Transform>,
    /// Color info from colr box (nclx or ICC)
    pub color_info: Option<ColorInfo>,
    /// Auxiliary type property (from auxC box), containing the URN string
    /// and optional subtype data (e.g., depth representation info).
    pub auxiliary_type_property: Option<super::boxes::AuxiliaryTypeProperty>,
    /// Content Light Level Information (from cLLi box).
    pub content_light_level: Option<super::boxes::ContentLightLevelBox>,
    /// Mastering Display Colour Volume (from mDCv box).
    pub mastering_display: Option<super::boxes::MasteringDisplayBox>,
}

impl<'a> HeifContainer<'a> {
    /// Get the primary item
    pub fn primary_item(&self) -> Option<Item> {
        self.get_item(self.primary_item_id)
    }

    /// Get an item by ID
    pub fn get_item(&self, item_id: u32) -> Option<Item> {
        let info = self.item_infos.iter().find(|i| i.item_id == item_id)?;

        // Find property associations for this item
        let assoc = self
            .property_associations
            .iter()
            .find(|a| a.item_id == item_id);

        let mut dimensions = None;
        let mut hevc_config = None;
        let mut av1_config = None;
        let mut uncompressed_config = None;
        let mut compression_config = None;
        let mut clean_aperture = None;
        let mut rotation = None;
        let mut mirror = None;
        let mut transforms = Vec::new();
        let mut color_info = None;
        let mut auxiliary_type_property = None;
        let mut content_light_level = None;
        let mut mastering_display = None;

        if let Some(assoc) = assoc {
            for &(prop_idx, _essential) in &assoc.properties {
                if prop_idx == 0 {
                    continue; // invalid 0-index in ipma
                }
                let idx = prop_idx as usize - 1; // 1-based index in ipma
                if let Some(prop) = self.properties.get(idx) {
                    match prop {
                        ItemProperty::ImageExtents(ext) => {
                            dimensions = Some((ext.width, ext.height));
                        }
                        ItemProperty::HevcConfig(config) => {
                            hevc_config = Some(config.clone());
                        }
                        ItemProperty::Av1Config(config) => {
                            av1_config = Some(config.clone());
                        }
                        ItemProperty::UncompressedConfig(config) => {
                            uncompressed_config = Some(config.clone());
                        }
                        ItemProperty::CompressionConfig(config) => {
                            compression_config = Some(config.clone());
                        }
                        ItemProperty::CleanAperture(clap) => {
                            clean_aperture = Some(*clap);
                            transforms.push(Transform::CleanAperture(*clap));
                        }
                        ItemProperty::Rotation(rot) => {
                            rotation = Some(*rot);
                            transforms.push(Transform::Rotation(*rot));
                        }
                        ItemProperty::Mirror(m) => {
                            mirror = Some(*m);
                            transforms.push(Transform::Mirror(*m));
                        }
                        ItemProperty::ColorInfo(ci) => {
                            color_info = Some(ci.clone());
                        }
                        ItemProperty::AuxiliaryType(atp) => {
                            auxiliary_type_property = Some(atp.clone());
                        }
                        ItemProperty::ContentLightLevel(clli) => {
                            content_light_level = Some(*clli);
                        }
                        ItemProperty::MasteringDisplay(mdcv) => {
                            mastering_display = Some(*mdcv);
                        }
                        _ => {}
                    }
                }
            }
        }

        Some(Item {
            id: item_id,
            item_type: info.item_type.into(),
            name: info.item_name.clone(),
            dimensions,
            hevc_config,
            av1_config,
            uncompressed_config,
            compression_config,
            clean_aperture,
            rotation,
            mirror,
            transforms,
            color_info,
            auxiliary_type_property,
            content_light_level,
            mastering_display,
        })
    }

    /// Get raw data for an item.
    ///
    /// Returns `Cow::Borrowed` for single-extent items (zero-copy),
    /// and `Cow::Owned` for multi-extent items (concatenated).
    pub fn get_item_data(&self, item_id: u32) -> Result<Cow<'a, [u8]>> {
        let loc = self
            .item_locations
            .iter()
            .find(|l| l.item_id == item_id)
            .ok_or_else(|| at!(HeicError::InvalidData("item not found in iloc")))?;

        if loc.extents.is_empty() {
            return Err(at!(HeicError::InvalidData("item has no extents")));
        }

        let source = match loc.construction_method {
            0 => self.data, // File offset (typically into mdat)
            1 => self
                .idat_data
                .ok_or_else(|| at!(HeicError::InvalidData("construction_method=1 but no idat")))?,
            _ => return Err(at!(HeicError::Unsupported("construction_method >= 2"))),
        };

        if loc.extents.len() == 1 {
            // Single extent: return slice directly (zero-copy)
            let (offset, length) = loc.extents[0];
            let offset = usize::try_from(
                loc.base_offset
                    .checked_add(offset)
                    .ok_or_else(|| at!(HeicError::InvalidData("extent offset overflow")))?,
            )
            .map_err(|_| at!(HeicError::InvalidData("extent offset too large")))?;
            let length = usize::try_from(length)
                .map_err(|_| at!(HeicError::InvalidData("extent length too large")))?;
            let end = offset
                .checked_add(length)
                .ok_or_else(|| at!(HeicError::InvalidData("extent end overflow")))?;
            if end > source.len() {
                return Err(at!(HeicError::InvalidData(
                    "extent extends past end of data"
                )));
            }
            return Ok(Cow::Borrowed(&source[offset..end]));
        }

        // Multiple extents: concatenate into owned buffer
        let total_len: u64 = loc
            .extents
            .iter()
            .map(|&(_, len)| len)
            .try_fold(0u64, |acc, len| acc.checked_add(len))
            .ok_or_else(|| at!(HeicError::InvalidData("multi-extent total length overflow")))?;
        // Sanity: total declared extent size cannot exceed the source data
        if total_len > source.len() as u64 {
            return Err(at!(HeicError::InvalidData(
                "multi-extent total exceeds source data"
            )));
        }
        let total_len = usize::try_from(total_len)
            .map_err(|_| at!(HeicError::InvalidData("multi-extent total too large")))?;

        let mut buf = Vec::new();
        buf.try_reserve(total_len)
            .map_err(|_| at!(HeicError::OutOfMemory))?;

        for &(extent_offset, extent_length) in &loc.extents {
            let offset = usize::try_from(
                loc.base_offset
                    .checked_add(extent_offset)
                    .ok_or_else(|| at!(HeicError::InvalidData("extent offset overflow")))?,
            )
            .map_err(|_| at!(HeicError::InvalidData("extent offset too large")))?;
            let length = usize::try_from(extent_length)
                .map_err(|_| at!(HeicError::InvalidData("extent length too large")))?;
            let end = offset
                .checked_add(length)
                .ok_or_else(|| at!(HeicError::InvalidData("extent end overflow")))?;
            if end > source.len() {
                return Err(at!(HeicError::InvalidData(
                    "extent extends past end of data"
                )));
            }
            buf.extend_from_slice(&source[offset..end]);
        }

        Ok(Cow::Owned(buf))
    }

    /// Find auxiliary items that reference a given target item, filtered by aux type prefix.
    ///
    /// `auxl` references point FROM the auxiliary item TO the primary item.
    /// This searches for items whose `auxl` reference targets `target_item_id`
    /// and whose `auxiliary_type` starts with `aux_type_prefix`.
    pub fn find_auxiliary_items(&self, target_item_id: u32, aux_type_prefix: &str) -> Vec<u32> {
        self.item_references
            .iter()
            .filter(|r| r.reference_type == FourCC::AUXL && r.to_item_ids.contains(&target_item_id))
            .filter_map(|r| {
                let item = self.get_item(r.from_item_id)?;
                if let Some(ref atp) = item.auxiliary_type_property
                    && atp.aux_type.starts_with(aux_type_prefix)
                {
                    return Some(r.from_item_id);
                }
                None
            })
            .collect()
    }

    /// Get item references of a given type from a source item
    pub fn get_item_references(&self, from_item_id: u32, ref_type: FourCC) -> Vec<u32> {
        self.item_references
            .iter()
            .filter(|r| r.from_item_id == from_item_id && r.reference_type == ref_type)
            .flat_map(|r| r.to_item_ids.iter().copied())
            .collect()
    }

    /// Find all auxiliary items that reference a given target item.
    ///
    /// Returns `(item_id, auxiliary_type_urn)` pairs for all items linked
    /// via `auxl` references to `target_item_id`.
    pub fn find_all_auxiliary_items(&self, target_item_id: u32) -> Vec<(u32, String)> {
        self.item_references
            .iter()
            .filter(|r| r.reference_type == FourCC::AUXL && r.to_item_ids.contains(&target_item_id))
            .filter_map(|r| {
                let item = self.get_item(r.from_item_id)?;
                let urn = item.auxiliary_type_property.as_ref()?.aux_type.clone();
                Some((r.from_item_id, urn))
            })
            .collect()
    }

    /// Find thumbnail items for a given target item.
    ///
    /// Returns item IDs of thumbnails linked via `thmb` references.
    /// `thmb` references point FROM the thumbnail TO the target item.
    pub fn find_thumbnails(&self, target_item_id: u32) -> Vec<u32> {
        self.item_references
            .iter()
            .filter(|r| r.reference_type == FourCC::THMB && r.to_item_ids.contains(&target_item_id))
            .map(|r| r.from_item_id)
            .collect()
    }

    /// Find XMP metadata items that describe a specific item.
    ///
    /// In HEIC files, XMP metadata for auxiliary images (like gain maps) is
    /// stored as a separate `mime` item linked via a `cdsc` (content describes)
    /// reference pointing to the target item.
    ///
    /// Returns the raw XMP bytes if found, `None` otherwise.
    pub fn find_xmp_for_item(&self, target_item_id: u32) -> Option<Cow<'a, [u8]>> {
        // Find mime/XMP items that have a cdsc reference to the target item
        for r in &self.item_references {
            if r.reference_type != FourCC::CDSC {
                continue;
            }
            if !r.to_item_ids.contains(&target_item_id) {
                continue;
            }
            // r.from_item_id is the metadata item describing target_item_id
            let Some(info) = self.item_infos.iter().find(|i| i.item_id == r.from_item_id) else {
                continue;
            };
            if info.item_type != FourCC(*b"mime") {
                continue;
            }
            if !info.content_type.contains("xmp") && !info.content_type.contains("rdf+xml") {
                continue;
            }
            if let Ok(data) = self.get_item_data(r.from_item_id) {
                return Some(data);
            }
        }
        None
    }
}

/// Parse a HEIF container
pub fn parse<'a>(data: &'a [u8], stop: &dyn Stop) -> Result<HeifContainer<'a>> {
    let mut container = HeifContainer {
        data,
        brand: FourCC(*b"    "),
        compatible_brands: Vec::new(),
        primary_item_id: 0,
        item_locations: Vec::new(),
        item_infos: Vec::new(),
        properties: Vec::new(),
        property_associations: Vec::new(),
        item_references: Vec::new(),
        idat_data: None,
        mdat_offset: None,
        mdat_length: None,
    };

    // Track whether we found a meta box (still image) or moov box (image sequence)
    let mut has_meta = false;
    let mut moov_box: Option<Box<'a>> = None;

    // Parse top-level boxes
    for top_box in BoxIterator::new(data) {
        check_stop(stop)?;
        match top_box.box_type() {
            FourCC::FTYP => parse_ftyp(&top_box, &mut container)?,
            FourCC::META => {
                parse_meta(&top_box, &mut container, stop)?;
                has_meta = true;
            }
            FourCC::MOOV => {
                // Defer moov parsing — meta takes priority if both present
                moov_box = Some(top_box);
            }
            FourCC::MDAT => {
                container.mdat_offset = Some(top_box.header.content_offset);
                container.mdat_length = Some(top_box.content.len());
            }
            _ => {} // Ignore unknown boxes
        }
    }

    // Verify we have required boxes
    if container.brand.0 == *b"    " {
        return Err(at!(HeicError::InvalidContainer("missing ftyp box")));
    }

    // If no meta box but we have a moov box (image sequence), parse it to
    // create synthetic items from the first I-frame of the first pict track.
    if !has_meta && let Some(ref moov) = moov_box {
        parse_moov(moov, data, &mut container, stop)?;
    }

    Ok(container)
}

fn parse_ftyp(ftyp: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = ftyp.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("ftyp too short")));
    }

    container.brand = FourCC::from_bytes(&content[0..4]).unwrap();

    // Skip minor version (4 bytes)
    let mut offset = 8;
    let mut brand_count = 0usize;
    while offset + 4 <= content.len() {
        if brand_count >= MAX_COMPATIBLE_BRANDS {
            break;
        }
        if let Some(brand) = FourCC::from_bytes(&content[offset..]) {
            container.compatible_brands.push(brand);
            brand_count += 1;
        }
        offset += 4;
    }

    // Verify this is a HEIF/HEIC file
    let valid_brands = [
        FourCC(*b"heic"),
        FourCC(*b"heix"),
        FourCC(*b"hevc"),
        FourCC(*b"hevx"),
        FourCC(*b"mif1"),
        FourCC(*b"msf1"),
        FourCC(*b"mif2"), // HEIF v2
        FourCC(*b"mif3"), // HEIF v3
        FourCC(*b"avif"), // AV1 Image File Format
        FourCC(*b"avis"), // AV1 Image Sequence
    ];

    let is_heif = valid_brands.contains(&container.brand)
        || container
            .compatible_brands
            .iter()
            .any(|b| valid_brands.contains(b));

    if !is_heif {
        return Err(at!(HeicError::InvalidContainer("not a HEIF file")));
    }

    Ok(())
}

fn parse_meta<'a>(
    meta: &Box<'a>,
    container: &mut HeifContainer<'a>,
    stop: &dyn Stop,
) -> Result<()> {
    // Meta is a full box - skip version/flags
    if meta.content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("meta box too short")));
    }

    let content = &meta.content[4..];

    for child in BoxIterator::new(content) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::PITM => parse_pitm(&child, container)?,
            FourCC::ILOC => parse_iloc(&child, container, stop)?,
            FourCC::IINF => parse_iinf(&child, container, stop)?,
            FourCC::IPRP => parse_iprp(&child, container, stop)?,
            FourCC::IREF => parse_iref(&child, container, stop)?,
            FourCC::IDAT => {
                container.idat_data = Some(child.content);
            }
            _ => {} // hdlr, etc.
        }
    }

    Ok(())
}

fn parse_pitm(pitm: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = pitm.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("pitm too short")));
    }

    let version = content[0];
    if version == 0 {
        if content.len() < 6 {
            return Err(at!(HeicError::InvalidContainer("pitm v0 too short")));
        }
        container.primary_item_id = u16::from_be_bytes([content[4], content[5]]) as u32;
    } else {
        if content.len() < 8 {
            return Err(at!(HeicError::InvalidContainer("pitm v1 too short")));
        }
        container.primary_item_id =
            u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    }

    Ok(())
}

fn parse_iloc(iloc: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = iloc.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("iloc too short")));
    }

    let version = content[0];
    let offset_size = (content[4] >> 4) & 0xF;
    let length_size = content[4] & 0xF;
    let base_offset_size = (content[5] >> 4) & 0xF;
    let index_size = if version >= 1 { content[5] & 0xF } else { 0 };

    let mut pos = 6;

    let item_count = if version < 2 {
        let count = u16::from_be_bytes([content[pos], content[pos + 1]]) as u32;
        pos += 2;
        count
    } else {
        let count = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        count
    };

    if item_count > MAX_ITEMS {
        return Err(at!(HeicError::LimitExceeded(
            "iloc item count exceeds limit"
        )));
    }

    container
        .item_locations
        .try_reserve(item_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    for _ in 0..item_count {
        check_stop(stop)?;

        let id_size = if version < 2 { 2usize } else { 4 };
        if pos + id_size > content.len() {
            break;
        }
        let item_id = if version < 2 {
            let id = u16::from_be_bytes([content[pos], content[pos + 1]]) as u32;
            pos += 2;
            id
        } else {
            let id = u32::from_be_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            id
        };

        let construction_method = if version >= 1 {
            if pos + 2 > content.len() {
                break;
            }
            let method = content[pos + 1] & 0xF;
            pos += 2;
            method
        } else {
            0
        };

        // Data reference index (2 bytes) - skip
        if pos + 2 > content.len() {
            break;
        }
        pos += 2;

        let base_offset = read_sized_int(content, &mut pos, base_offset_size as usize);

        if pos + 2 > content.len() {
            break;
        }
        let extent_count = u16::from_be_bytes([content[pos], content[pos + 1]]);
        pos += 2;

        if u32::from(extent_count) > MAX_EXTENTS_PER_ITEM {
            return Err(at!(HeicError::LimitExceeded(
                "extent count per item exceeds limit"
            )));
        }

        let mut extents = Vec::new();
        extents
            .try_reserve(extent_count as usize)
            .map_err(|_| at!(HeicError::OutOfMemory))?;
        for _ in 0..extent_count {
            if version >= 1 && index_size > 0 {
                // Extent index - skip
                if pos + index_size as usize > content.len() {
                    break;
                }
                pos += index_size as usize;
            }

            let extent_offset = read_sized_int(content, &mut pos, offset_size as usize);
            let extent_length = read_sized_int(content, &mut pos, length_size as usize);
            extents.push((extent_offset, extent_length));
        }

        container.item_locations.push(ItemLocation {
            item_id,
            construction_method,
            base_offset,
            extents,
        });
    }

    Ok(())
}

fn read_sized_int(data: &[u8], pos: &mut usize, size: usize) -> u64 {
    if size == 0 || *pos + size > data.len() {
        return 0;
    }

    let mut value = 0u64;
    for i in 0..size {
        value = (value << 8) | data[*pos + i] as u64;
    }
    *pos += size;
    value
}

fn parse_iinf(iinf: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = iinf.content;
    if content.len() < 6 {
        return Err(at!(HeicError::InvalidContainer("iinf too short")));
    }

    let version = content[0];
    let mut pos = 4;

    let entry_count = if version == 0 {
        let count = u16::from_be_bytes([content[pos], content[pos + 1]]) as u32;
        pos += 2;
        count
    } else {
        let count = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        count
    };

    if entry_count > MAX_ITEMS {
        return Err(at!(HeicError::LimitExceeded(
            "iinf entry count exceeds limit"
        )));
    }

    container
        .item_infos
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    // Parse infe boxes
    let remaining = &content[pos..];
    let mut infe_count = 0;
    for child in BoxIterator::new(remaining) {
        check_stop(stop)?;
        if child.box_type() == FourCC::INFE
            && let Ok(info) = parse_infe(&child)
        {
            container.item_infos.push(info);
            infe_count += 1;
            if infe_count >= entry_count {
                break;
            }
        }
    }

    Ok(())
}

fn parse_infe(infe: &Box<'_>) -> Result<ItemInfo> {
    let content = infe.content;

    let version = *content
        .first()
        .ok_or_else(|| at!(HeicError::InvalidContainer("infe too short")))?;
    // Minimum: 4 (ver+flags) + id (2 or 4) + 2 (protection) + type (4 if v>=2)
    let min_len = match version {
        0..=1 => 4 + 2 + 2, // 8
        2 => 4 + 2 + 2 + 4, // 12
        _ => 4 + 4 + 2 + 4, // 14 (version >= 3 uses 4-byte item_id)
    };
    if content.len() < min_len {
        return Err(at!(HeicError::InvalidContainer("infe too short")));
    }

    let flags = u32::from_be_bytes([0, content[1], content[2], content[3]]);
    let hidden = (flags & 1) != 0;

    let mut pos = 4;

    let item_id = if version < 3 {
        let id = u16::from_be_bytes([content[pos], content[pos + 1]]) as u32;
        pos += 2;
        id
    } else {
        let id = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        id
    };

    // Item protection index (2 bytes) - skip
    pos += 2;

    let item_type = if version >= 2 {
        let ft = FourCC::from_bytes(&content[pos..]).unwrap_or(FourCC(*b"    "));
        pos += 4;
        ft
    } else {
        FourCC(*b"    ")
    };

    // Item name (null-terminated string)
    if pos >= content.len() {
        return Ok(ItemInfo {
            item_id,
            item_type,
            item_name: String::new(),
            content_type: String::new(),
            hidden,
        });
    }
    let name_end = content[pos..].iter().position(|&b| b == 0).unwrap_or(0);
    if name_end > MAX_STRING_LENGTH {
        return Err(at!(HeicError::InvalidContainer("item name too long")));
    }
    let item_name = str::from_utf8(&content[pos..pos + name_end])
        .unwrap_or("")
        .to_string();
    pos += name_end + 1;

    // Content type (null-terminated string, optional)
    let content_type = if pos < content.len() {
        let ct_end = content[pos..].iter().position(|&b| b == 0).unwrap_or(0);
        if ct_end > MAX_STRING_LENGTH {
            return Err(at!(HeicError::InvalidContainer("content type too long")));
        }
        str::from_utf8(&content[pos..pos + ct_end])
            .unwrap_or("")
            .to_string()
    } else {
        String::new()
    };

    Ok(ItemInfo {
        item_id,
        item_type,
        item_name,
        content_type,
        hidden,
    })
}

fn parse_iprp(iprp: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    for child in BoxIterator::new(iprp.content) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::IPCO => parse_ipco(&child, container, stop)?,
            FourCC::IPMA => parse_ipma(&child, container, stop)?,
            _ => {}
        }
    }
    Ok(())
}

fn parse_ipco(ipco: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    // Properties are stored in order - index is implicit (1-based in ipma, 0-based here)
    for (prop_count, child) in BoxIterator::new(ipco.content).enumerate() {
        check_stop(stop)?;
        if prop_count >= MAX_PROPERTIES as usize {
            return Err(at!(HeicError::LimitExceeded(
                "property count exceeds limit"
            )));
        }
        let prop = match child.box_type() {
            FourCC::ISPE => {
                if let Ok(ext) = parse_ispe(&child) {
                    ItemProperty::ImageExtents(ext)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::HVCC => {
                if let Ok(config) = parse_hvcc(&child) {
                    ItemProperty::HevcConfig(config)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::COLR => {
                if let Ok(color) = parse_colr(&child) {
                    ItemProperty::ColorInfo(color)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::CLAP => {
                if let Ok(clap) = parse_clap(&child) {
                    ItemProperty::CleanAperture(clap)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::IROT => {
                if let Ok(rot) = parse_irot(&child) {
                    ItemProperty::Rotation(rot)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::IMIR => {
                if let Ok(mirror) = parse_imir(&child) {
                    ItemProperty::Mirror(mirror)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::AUXC => {
                if let Ok(aux_type) = parse_auxc(&child) {
                    ItemProperty::AuxiliaryType(aux_type)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::CLLI => {
                if let Ok(clli) = parse_clli(&child) {
                    ItemProperty::ContentLightLevel(clli)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::MDCV => {
                if let Ok(mdcv) = parse_mdcv(&child) {
                    ItemProperty::MasteringDisplay(mdcv)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::AV1C => {
                if let Ok(config) = parse_av1c(&child) {
                    ItemProperty::Av1Config(config)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::UNCC => {
                if let Ok(config) = parse_uncc(&child) {
                    ItemProperty::UncompressedConfig(config)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::CMPC => {
                if let Ok(config) = parse_cmpc(&child) {
                    ItemProperty::CompressionConfig(config)
                } else {
                    ItemProperty::Unknown
                }
            }
            _ => ItemProperty::Unknown,
        };
        container.properties.push(prop);
    }

    Ok(())
}

fn parse_clap(clap: &Box<'_>) -> Result<CleanAperture> {
    let content = clap.content;
    // clap box: 8 fields of 4 bytes each = 32 bytes (no version/flags)
    if content.len() < 32 {
        return Err(at!(HeicError::InvalidContainer("clap too short")));
    }

    let width_n = u32::from_be_bytes([content[0], content[1], content[2], content[3]]);
    let width_d = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let height_n = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);
    let height_d = u32::from_be_bytes([content[12], content[13], content[14], content[15]]);
    let horiz_off_n = i32::from_be_bytes([content[16], content[17], content[18], content[19]]);
    let horiz_off_d = u32::from_be_bytes([content[20], content[21], content[22], content[23]]);
    let vert_off_n = i32::from_be_bytes([content[24], content[25], content[26], content[27]]);
    let vert_off_d = u32::from_be_bytes([content[28], content[29], content[30], content[31]]);

    // Validate denominators are non-zero
    if width_d == 0 || height_d == 0 || horiz_off_d == 0 || vert_off_d == 0 {
        return Err(at!(HeicError::InvalidContainer(
            "clap has zero denominator"
        )));
    }

    Ok(CleanAperture {
        width_n,
        width_d,
        height_n,
        height_d,
        horiz_off_n,
        horiz_off_d,
        vert_off_n,
        vert_off_d,
    })
}

fn parse_irot(irot: &Box<'_>) -> Result<ImageRotation> {
    let content = irot.content;
    // irot box: 1 byte angle (0=0°, 1=90°CCW, 2=180°, 3=270°CCW)
    if content.is_empty() {
        return Err(at!(HeicError::InvalidContainer("irot too short")));
    }
    let angle = match content[0] & 0x03 {
        0 => 0,
        1 => 270, // HEIF irot: 1 = 90° CCW = 270° CW
        2 => 180,
        3 => 90, // HEIF irot: 3 = 270° CCW = 90° CW
        _ => 0,
    };
    Ok(ImageRotation { angle })
}

fn parse_imir(imir: &Box<'_>) -> Result<ImageMirror> {
    let content = imir.content;
    // imir box: 1 byte (7 bits reserved, 1 bit axis)
    if content.is_empty() {
        return Err(at!(HeicError::InvalidContainer("imir too short")));
    }
    Ok(ImageMirror {
        axis: content[0] & 0x01,
    })
}

fn parse_auxc(auxc: &Box<'_>) -> Result<super::boxes::AuxiliaryTypeProperty> {
    let content = auxc.content;
    // auxC is a full box: version/flags (4 bytes) + null-terminated UTF-8 aux_type string
    // + optional subtype data bytes after the null terminator
    if content.len() < 5 {
        return Err(at!(HeicError::InvalidContainer("auxC too short")));
    }

    // Skip version/flags (4 bytes)
    let data = &content[4..];
    // Find null terminator
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    if end > MAX_STRING_LENGTH {
        return Err(at!(HeicError::InvalidContainer("auxC string too long")));
    }
    let aux_type = str::from_utf8(&data[..end]).unwrap_or("").to_string();

    // Subtype data is everything after the null terminator
    let subtype_data = if end < data.len() {
        data[end + 1..].to_vec()
    } else {
        Vec::new()
    };

    Ok(super::boxes::AuxiliaryTypeProperty {
        aux_type,
        subtype_data,
    })
}

/// Parse a Content Light Level Information property box (cLLi).
///
/// NOT a FullBox — payload is 4 bytes: two u16 BE values.
/// See ISO 14496-12 § 12.1.5.
fn parse_clli(clli: &Box<'_>) -> Result<ContentLightLevelBox> {
    let content = clli.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("clli too short")));
    }
    let max_content_light_level = u16::from_be_bytes([content[0], content[1]]);
    let max_frame_average_light_level = u16::from_be_bytes([content[2], content[3]]);
    Ok(ContentLightLevelBox {
        max_content_light_level,
        max_frame_average_light_level,
    })
}

/// Parse a Mastering Display Colour Volume property box (mDCv).
///
/// NOT a FullBox — payload is 24 bytes.
/// See ISO 14496-12 § 12.1.5 / SMPTE ST 2086.
fn parse_mdcv(mdcv: &Box<'_>) -> Result<MasteringDisplayBox> {
    let content = mdcv.content;
    if content.len() < 24 {
        return Err(at!(HeicError::InvalidContainer("mdcv too short")));
    }
    let mut pos = 0;
    let mut primaries = [(0u16, 0u16); 3];
    for p in &mut primaries {
        let x = u16::from_be_bytes([content[pos], content[pos + 1]]);
        let y = u16::from_be_bytes([content[pos + 2], content[pos + 3]]);
        *p = (x, y);
        pos += 4;
    }
    let wp_x = u16::from_be_bytes([content[pos], content[pos + 1]]);
    let wp_y = u16::from_be_bytes([content[pos + 2], content[pos + 3]]);
    pos += 4;
    let max_luminance = u32::from_be_bytes([
        content[pos],
        content[pos + 1],
        content[pos + 2],
        content[pos + 3],
    ]);
    pos += 4;
    let min_luminance = u32::from_be_bytes([
        content[pos],
        content[pos + 1],
        content[pos + 2],
        content[pos + 3],
    ]);
    Ok(MasteringDisplayBox {
        primaries_xy: primaries,
        white_point_xy: (wp_x, wp_y),
        max_luminance,
        min_luminance,
    })
}

fn parse_ispe(ispe: &Box<'_>) -> Result<ImageSpatialExtents> {
    let content = ispe.content;
    if content.len() < 12 {
        return Err(at!(HeicError::InvalidContainer("ispe too short")));
    }

    // Skip version/flags (4 bytes)
    let width = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let height = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);

    // Validate dimensions are non-zero and reasonable
    if width == 0 || height == 0 {
        return Err(at!(HeicError::InvalidContainer("ispe has zero dimension")));
    }
    if width > (1 << 30) || height > (1 << 30) {
        return Err(at!(HeicError::InvalidContainer(
            "ispe dimensions too large"
        )));
    }

    Ok(ImageSpatialExtents { width, height })
}

fn parse_hvcc(hvcc: &Box<'_>) -> Result<HevcDecoderConfig> {
    let content = hvcc.content;
    if content.len() < 23 {
        return Err(at!(HeicError::InvalidContainer("hvcC too short")));
    }

    let config_version = content[0];
    let general_profile_space = (content[1] >> 6) & 0x3;
    let general_tier_flag = (content[1] >> 5) & 0x1 != 0;
    let general_profile_idc = content[1] & 0x1F;
    let general_profile_compatibility_flags =
        u32::from_be_bytes([content[2], content[3], content[4], content[5]]);
    let general_constraint_indicator_flags = u64::from_be_bytes([
        content[6],
        content[7],
        content[8],
        content[9],
        content[10],
        content[11],
        0,
        0,
    ]) >> 16;
    let general_level_idc = content[12];
    // Skip min_spatial_segmentation_idc (2 bytes)
    // Skip parallelismType (1 byte)
    let chroma_format = content[16] & 0x3;
    let bit_depth_luma_minus8 = content[17] & 0x7;
    let bit_depth_chroma_minus8 = content[18] & 0x7;
    // Skip avgFrameRate (2 bytes)
    let length_size_minus_one = content[21] & 0x3;

    // Validate length_size_minus_one: valid values are 0, 1, 3 (sizes 1, 2, 4 bytes)
    if length_size_minus_one == 2 {
        return Err(at!(HeicError::InvalidContainer(
            "hvcC invalid length_size_minus_one=2"
        )));
    }

    let num_arrays = content[22];
    let mut pos = 23;
    let mut nal_units = Vec::new();

    for _ in 0..num_arrays {
        if pos + 3 > content.len() {
            break;
        }

        // array_completeness and nal_unit_type
        let _nal_type = content[pos] & 0x3F;
        pos += 1;

        let num_nalus = u16::from_be_bytes([content[pos], content[pos + 1]]);
        pos += 2;

        for _ in 0..num_nalus {
            if pos + 2 > content.len() {
                break;
            }

            let nalu_len = u16::from_be_bytes([content[pos], content[pos + 1]]) as usize;
            pos += 2;

            if nalu_len > MAX_NAL_UNIT_SIZE {
                return Err(at!(HeicError::LimitExceeded("NAL unit too large")));
            }

            if pos + nalu_len > content.len() {
                break;
            }

            nal_units.push(content[pos..pos + nalu_len].to_vec());
            pos += nalu_len;
        }
    }

    Ok(HevcDecoderConfig {
        config_version,
        general_profile_space,
        general_tier_flag,
        general_profile_idc,
        general_profile_compatibility_flags,
        general_constraint_indicator_flags,
        general_level_idc,
        chroma_format,
        bit_depth_luma_minus8,
        bit_depth_chroma_minus8,
        length_size_minus_one,
        nal_units,
    })
}

fn parse_colr(colr: &Box<'_>) -> Result<ColorInfo> {
    let content = colr.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("colr too short")));
    }

    let color_type = FourCC::from_bytes(&content[0..4]).unwrap();

    match &color_type.0 {
        b"nclx" => {
            if content.len() < 11 {
                return Err(at!(HeicError::InvalidContainer("nclx colr too short")));
            }
            Ok(ColorInfo::Nclx {
                color_primaries: u16::from_be_bytes([content[4], content[5]]),
                transfer_characteristics: u16::from_be_bytes([content[6], content[7]]),
                matrix_coefficients: u16::from_be_bytes([content[8], content[9]]),
                full_range: (content[10] >> 7) != 0,
            })
        }
        b"prof" | b"ricc" => {
            // ICC profile
            let icc_data = &content[4..];
            if icc_data.len() > MAX_ICC_PROFILE_SIZE {
                return Err(at!(HeicError::LimitExceeded("ICC profile too large")));
            }
            Ok(ColorInfo::IccProfile(icc_data.to_vec()))
        }
        _ => Err(at!(HeicError::InvalidContainer("unknown color type"))),
    }
}

fn parse_iref(iref: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = iref.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("iref too short")));
    }

    let version = content[0];
    let remaining = &content[4..];

    let mut total_refs = 0u32;

    // iref contains child boxes, each is a reference type
    for child in BoxIterator::new(remaining) {
        check_stop(stop)?;
        let ref_type = child.box_type();
        let data = child.content;
        let mut pos = 0;

        while pos < data.len() {
            if total_refs >= MAX_REFERENCES {
                return Err(at!(HeicError::LimitExceeded(
                    "reference count exceeds limit"
                )));
            }

            let (from_id, id_size) = if version == 0 {
                if pos + 2 > data.len() {
                    break;
                }
                (
                    u16::from_be_bytes([data[pos], data[pos + 1]]) as u32,
                    2usize,
                )
            } else {
                if pos + 4 > data.len() {
                    break;
                }
                (
                    u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]]),
                    4usize,
                )
            };
            pos += id_size;

            if pos + 2 > data.len() {
                break;
            }
            let ref_count = u16::from_be_bytes([data[pos], data[pos + 1]]);
            pos += 2;

            if u32::from(ref_count) > MAX_REFS_PER_ENTRY {
                return Err(at!(HeicError::LimitExceeded(
                    "refs per entry exceeds limit"
                )));
            }

            let mut to_ids = Vec::new();
            to_ids
                .try_reserve(ref_count as usize)
                .map_err(|_| at!(HeicError::OutOfMemory))?;
            for _ in 0..ref_count {
                if pos + id_size > data.len() {
                    break;
                }
                let to_id = if version == 0 {
                    u16::from_be_bytes([data[pos], data[pos + 1]]) as u32
                } else {
                    u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                };
                pos += id_size;
                to_ids.push(to_id);
            }

            container.item_references.push(ItemReference {
                reference_type: ref_type,
                from_item_id: from_id,
                to_item_ids: to_ids,
            });
            total_refs += 1;
        }
    }

    Ok(())
}

fn parse_ipma(ipma: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = ipma.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("ipma too short")));
    }

    let version = content[0];
    let flags = u32::from_be_bytes([0, content[1], content[2], content[3]]);
    let mut pos = 4;

    let entry_count = u32::from_be_bytes([
        content[pos],
        content[pos + 1],
        content[pos + 2],
        content[pos + 3],
    ]);
    pos += 4;

    if entry_count > MAX_ITEMS {
        return Err(at!(HeicError::LimitExceeded(
            "ipma entry count exceeds limit"
        )));
    }

    container
        .property_associations
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    for _ in 0..entry_count {
        check_stop(stop)?;

        let id_size = if version < 1 { 2 } else { 4 };
        if pos + id_size > content.len() {
            break;
        }

        let item_id = if version < 1 {
            let id = u16::from_be_bytes([content[pos], content[pos + 1]]) as u32;
            pos += 2;
            id
        } else {
            let id = u32::from_be_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            pos += 4;
            id
        };

        if pos >= content.len() {
            break;
        }

        let assoc_count = content[pos];
        pos += 1;

        let mut properties = Vec::with_capacity(assoc_count as usize);

        for _ in 0..assoc_count {
            if pos >= content.len() {
                break;
            }

            let (essential, prop_idx) = if (flags & 1) != 0 {
                // 16-bit property index
                if pos + 2 > content.len() {
                    break;
                }
                let val = u16::from_be_bytes([content[pos], content[pos + 1]]);
                pos += 2;
                ((val >> 15) != 0, val & 0x7FFF)
            } else {
                // 8-bit property index
                let val = content[pos];
                pos += 1;
                ((val >> 7) != 0, (val & 0x7F) as u16)
            };

            properties.push((prop_idx, essential));
        }

        container.property_associations.push(PropertyAssociation {
            item_id,
            properties,
        });
    }

    Ok(())
}

/// Parse an AV1 Codec Configuration Box (av1C).
///
/// Format (AV1 Codec ISO Media File Format, section 2.3.3):
/// - 1 byte: `marker(1) | version(7)` — must be 0x81
/// - 1 byte: `seq_profile(3) | seq_level_idx_0(5)`
/// - 1 byte: `seq_tier_0(1) | high_bitdepth(1) | twelve_bit(1) | monochrome(1)
///   | chroma_subsampling_x(1) | chroma_subsampling_y(1) | chroma_sample_position(2)`
/// - 1 byte: `reserved(3) | initial_presentation_delay_present(1) | ipd_or_reserved(4)`
/// - remaining: configOBUs
fn parse_av1c(av1c: &Box<'_>) -> Result<Av1DecoderConfig> {
    let content = av1c.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("av1C too short")));
    }

    // Validate marker and version byte
    if content[0] != 0x81 {
        return Err(at!(HeicError::InvalidContainer(
            "av1C invalid marker/version byte"
        )));
    }

    let seq_profile = (content[1] >> 5) & 0x07;
    let seq_level_idx_0 = content[1] & 0x1F;

    let byte2 = content[2];
    let high_bitdepth = (byte2 >> 6) & 1 != 0;
    let twelve_bit = (byte2 >> 5) & 1 != 0;
    let monochrome = (byte2 >> 4) & 1 != 0;
    let chroma_subsampling_x = (byte2 >> 3) & 1 != 0;
    let chroma_subsampling_y = (byte2 >> 2) & 1 != 0;

    // Remaining bytes are configOBUs
    let config_obus = if content.len() > 4 {
        content[4..].to_vec()
    } else {
        Vec::new()
    };

    Ok(Av1DecoderConfig {
        seq_profile,
        seq_level_idx_0,
        high_bitdepth,
        twelve_bit,
        monochrome,
        chroma_subsampling_x,
        chroma_subsampling_y,
        config_obus,
    })
}

/// Maximum number of components in an uncompressed codec configuration.
const MAX_UNCI_COMPONENTS: u32 = 256;

/// Parse an Uncompressed Codec Configuration Box (uncC, ISO 23001-17).
///
/// This is a FullBox: version(1) + flags(3) + profile(4) + component_count(2) + ...
fn parse_uncc(uncc: &Box<'_>) -> Result<UncompressedConfig> {
    let content = uncc.content;
    // Minimum: 4 (ver/flags) + 4 (profile) + 4 (component_count as u32 for v1, or u16)
    if content.len() < 10 {
        return Err(at!(HeicError::InvalidContainer("uncC too short")));
    }

    let _version = content[0];
    let mut pos = 4; // skip version + flags

    let profile = u32::from_be_bytes([
        content[pos],
        content[pos + 1],
        content[pos + 2],
        content[pos + 3],
    ]);
    pos += 4;

    if pos + 4 > content.len() {
        return Err(at!(HeicError::InvalidContainer(
            "uncC truncated at component_count"
        )));
    }

    // ISO 23001-17 uncC uses a u32 component count for all versions
    let component_count = u32::from_be_bytes([
        content[pos],
        content[pos + 1],
        content[pos + 2],
        content[pos + 3],
    ]);
    pos += 4;

    if component_count > MAX_UNCI_COMPONENTS {
        return Err(at!(HeicError::LimitExceeded(
            "uncC component count exceeds limit"
        )));
    }

    let mut components = Vec::new();
    components
        .try_reserve(component_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    for _ in 0..component_count {
        // Each component: component_index(2) + bit_depth_minus_one(1) + format(1) + align_size(1) = 5 bytes
        if pos + 5 > content.len() {
            return Err(at!(HeicError::InvalidContainer(
                "uncC truncated at component"
            )));
        }
        let component_index = u16::from_be_bytes([content[pos], content[pos + 1]]);
        pos += 2;
        let component_bit_depth_minus_one = content[pos];
        pos += 1;
        let component_format = content[pos];
        pos += 1;
        let component_align_size = content[pos];
        pos += 1;

        components.push(UncompressedComponent {
            component_index,
            component_bit_depth_minus_one,
            component_format,
            component_align_size,
        });
    }

    // Remaining fixed fields
    if pos + 8 > content.len() {
        // If remaining data is short, return defaults for optional fields
        return Ok(UncompressedConfig {
            profile,
            components,
            sampling_type: 0,
            interleave_type: 0,
            block_size: 0,
            components_little_endian: false,
            block_pad_lsb: false,
            block_little_endian: false,
            block_reversed: false,
            pad_unknown: false,
            pixel_size: 0,
            row_align_size: 0,
            tile_align_size: 0,
            num_tile_cols_minus_one: 0,
            num_tile_rows_minus_one: 0,
        });
    }

    let sampling_type = content[pos];
    pos += 1;
    let interleave_type = content[pos];
    pos += 1;
    let block_size = content[pos];
    pos += 1;

    // Flags byte
    let flags_byte = content[pos];
    pos += 1;
    let components_little_endian = (flags_byte >> 7) & 1 != 0;
    let block_pad_lsb = (flags_byte >> 6) & 1 != 0;
    let block_little_endian = (flags_byte >> 5) & 1 != 0;
    let block_reversed = (flags_byte >> 4) & 1 != 0;
    let pad_unknown = (flags_byte >> 3) & 1 != 0;

    let pixel_size = content[pos];
    pos += 1;

    let row_align_size = if pos + 4 <= content.len() {
        let val = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        val
    } else {
        0
    };

    let tile_align_size = if pos + 4 <= content.len() {
        let val = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        val
    } else {
        0
    };

    let num_tile_cols_minus_one = if pos + 4 <= content.len() {
        let val = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        pos += 4;
        val
    } else {
        0
    };

    let num_tile_rows_minus_one = if pos + 4 <= content.len() {
        let val = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        let _ = pos; // suppress unused assignment warning
        val
    } else {
        0
    };

    Ok(UncompressedConfig {
        profile,
        components,
        sampling_type,
        interleave_type,
        block_size,
        components_little_endian,
        block_pad_lsb,
        block_little_endian,
        block_reversed,
        pad_unknown,
        pixel_size,
        row_align_size,
        tile_align_size,
        num_tile_cols_minus_one,
        num_tile_rows_minus_one,
    })
}

/// Parse a Generic Compression Configuration Box (cmpC, ISO 23001-17).
///
/// FullBox: version(1) + flags(3) + compression_type(4)
fn parse_cmpc(cmpc: &Box<'_>) -> Result<CompressionConfig> {
    let content = cmpc.content;
    // 4 (version + flags) + 4 (compression_type) = 8 bytes minimum
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("cmpC too short")));
    }

    let compression_type = FourCC::from_bytes(&content[4..8])
        .ok_or_else(|| at!(HeicError::InvalidContainer("cmpC invalid compression type")))?;

    Ok(CompressionConfig { compression_type })
}

// ---------------------------------------------------------------------------
// Image sequence (moov/trak) parsing
// ---------------------------------------------------------------------------

/// Maximum number of samples in an image sequence track
const MAX_SAMPLES: u32 = 1_000_000;
/// Maximum number of chunks in an image sequence track
const MAX_CHUNKS: u32 = 1_000_000;
/// Maximum number of stsc entries
const MAX_STSC_ENTRIES: u32 = 1_000_000;
/// Maximum number of sync samples
const MAX_SYNC_SAMPLES: u32 = 1_000_000;

/// Parsed data from a single track in a moov box
struct TrackInfo {
    /// Track ID from tkhd
    track_id: u32,
    /// Display width from tkhd (integer part of 16.16 fixed point)
    width: u32,
    /// Display height from tkhd (integer part of 16.16 fixed point)
    height: u32,
    /// Handler type from hdlr (e.g. "pict", "vide")
    handler_type: FourCC,
    /// HEVC decoder config from stsd hvc1/hev1 entry
    hevc_config: Option<HevcDecoderConfig>,
    /// Color info from colr box nested in stsd entry
    color_info: Option<ColorInfo>,
    /// Per-sample sizes (empty if all same size)
    sample_sizes: Vec<u32>,
    /// Uniform sample size (0 means use per-sample sizes)
    uniform_sample_size: u32,
    /// Total number of samples
    sample_count: u32,
    /// Chunk offsets (from stco or co64)
    chunk_offsets: Vec<u64>,
    /// Sample-to-chunk map entries: (first_chunk, samples_per_chunk, sample_desc_idx)
    sample_to_chunk: Vec<(u32, u32, u32)>,
    /// Sync sample numbers (1-based). Empty means all samples are sync.
    sync_samples: Vec<u32>,
}

/// Parse a moov box and populate the container with synthetic items for the
/// first I-frame of the first `pict` handler track.
fn parse_moov<'a>(
    moov: &Box<'a>,
    file_data: &'a [u8],
    container: &mut HeifContainer<'a>,
    stop: &dyn Stop,
) -> Result<()> {
    let mut tracks: Vec<TrackInfo> = Vec::new();

    for child in BoxIterator::new(moov.content) {
        check_stop(stop)?;
        if child.box_type() == FourCC::TRAK
            && let Ok(track) = parse_trak(&child, stop)
        {
            tracks.push(track);
        }
    }

    // Find the first track with handler_type = "pict" (image sequence)
    // If none, try "vide" as fallback
    let primary_track = tracks
        .iter()
        .find(|t| t.handler_type == FourCC(*b"pict"))
        .or_else(|| tracks.iter().find(|t| t.handler_type == FourCC(*b"vide")));

    let Some(track) = primary_track else {
        return Ok(()); // No suitable track found
    };

    // Determine the first sync sample (1-based index)
    let first_sync = if track.sync_samples.is_empty() {
        // No stss box means all samples are sync — use sample 1
        1u32
    } else {
        *track
            .sync_samples
            .first()
            .ok_or_else(|| at!(HeicError::InvalidData("empty sync sample table")))?
    };

    if first_sync == 0 || first_sync > track.sample_count {
        return Err(at!(HeicError::InvalidData(
            "sync sample index out of range"
        )));
    }

    // Get the sample's size
    let sample_size = if track.uniform_sample_size > 0 {
        track.uniform_sample_size
    } else {
        let idx = (first_sync - 1) as usize;
        if idx >= track.sample_sizes.len() {
            return Err(at!(HeicError::InvalidData("sample index out of range")));
        }
        track.sample_sizes[idx]
    };

    // Resolve sample → file offset using stco + stsc tables
    let sample_offset = resolve_sample_offset(track, first_sync, file_data.len() as u64, stop)?;

    let sample_end = sample_offset
        .checked_add(sample_size as u64)
        .ok_or_else(|| at!(HeicError::InvalidData("sample offset+size overflow")))?;
    if sample_end > file_data.len() as u64 {
        return Err(at!(HeicError::InvalidData(
            "sample extends past end of file"
        )));
    }

    // Build synthetic item ID 1 for the primary image
    let synth_id: u32 = 1;
    container.primary_item_id = synth_id;

    // Create item info
    container.item_infos.push(ItemInfo {
        item_id: synth_id,
        item_type: FourCC::HVC1,
        item_name: String::new(),
        content_type: String::new(),
        hidden: false,
    });

    // Create item location pointing directly at the sample data in the file
    container.item_locations.push(ItemLocation {
        item_id: synth_id,
        construction_method: 0, // file offset
        base_offset: 0,
        extents: alloc::vec![(sample_offset, sample_size as u64)],
    });

    // Create properties: ispe (dimensions) and hvcC (decoder config)
    let prop_start = container.properties.len();

    container
        .properties
        .push(ItemProperty::ImageExtents(ImageSpatialExtents {
            width: track.width,
            height: track.height,
        }));

    if let Some(ref config) = track.hevc_config {
        container
            .properties
            .push(ItemProperty::HevcConfig(config.clone()));
    }

    if let Some(ref color) = track.color_info {
        container
            .properties
            .push(ItemProperty::ColorInfo(color.clone()));
    }

    // Associate properties with our synthetic item (1-based indices)
    let prop_count = container.properties.len() - prop_start;
    let mut associations = Vec::new();
    for i in 0..prop_count {
        associations.push(((prop_start + i + 1) as u16, true));
    }
    container.property_associations.push(PropertyAssociation {
        item_id: synth_id,
        properties: associations,
    });

    // Check for thumbnail track: second pict track with tref, or smaller dimensions
    for other_track in &tracks {
        if other_track.track_id == track.track_id {
            continue;
        }
        if other_track.handler_type != FourCC(*b"pict") {
            continue;
        }
        // Smaller track is likely a thumbnail
        if other_track.width < track.width || other_track.height < track.height {
            let thumb_id: u32 = synth_id + 1;

            let thumb_sync = if other_track.sync_samples.is_empty() {
                1u32
            } else {
                match other_track.sync_samples.first() {
                    Some(&s) if s > 0 && s <= other_track.sample_count => s,
                    _ => continue,
                }
            };

            let thumb_size = if other_track.uniform_sample_size > 0 {
                other_track.uniform_sample_size
            } else {
                let idx = (thumb_sync - 1) as usize;
                if idx >= other_track.sample_sizes.len() {
                    continue;
                }
                other_track.sample_sizes[idx]
            };

            let Ok(thumb_offset) =
                resolve_sample_offset(other_track, thumb_sync, file_data.len() as u64, stop)
            else {
                continue;
            };

            let Some(thumb_end) = thumb_offset.checked_add(thumb_size as u64) else {
                continue;
            };
            if thumb_end > file_data.len() as u64 {
                continue;
            }

            container.item_infos.push(ItemInfo {
                item_id: thumb_id,
                item_type: FourCC::HVC1,
                item_name: String::new(),
                content_type: String::new(),
                hidden: false,
            });
            container.item_locations.push(ItemLocation {
                item_id: thumb_id,
                construction_method: 0,
                base_offset: 0,
                extents: alloc::vec![(thumb_offset, thumb_size as u64)],
            });

            let tp_start = container.properties.len();
            container
                .properties
                .push(ItemProperty::ImageExtents(ImageSpatialExtents {
                    width: other_track.width,
                    height: other_track.height,
                }));
            if let Some(ref config) = other_track.hevc_config {
                container
                    .properties
                    .push(ItemProperty::HevcConfig(config.clone()));
            }
            if let Some(ref color) = other_track.color_info {
                container
                    .properties
                    .push(ItemProperty::ColorInfo(color.clone()));
            }

            let tp_count = container.properties.len() - tp_start;
            let mut thumb_assocs = Vec::new();
            for i in 0..tp_count {
                thumb_assocs.push(((tp_start + i + 1) as u16, true));
            }
            container.property_associations.push(PropertyAssociation {
                item_id: thumb_id,
                properties: thumb_assocs,
            });

            // Link thumbnail to primary via thmb reference
            container.item_references.push(ItemReference {
                reference_type: FourCC::THMB,
                from_item_id: thumb_id,
                to_item_ids: alloc::vec![synth_id],
            });

            break; // Only first thumbnail track
        }
    }

    Ok(())
}

/// Parse a trak box into a TrackInfo
fn parse_trak(trak: &Box<'_>, stop: &dyn Stop) -> Result<TrackInfo> {
    let mut info = TrackInfo {
        track_id: 0,
        width: 0,
        height: 0,
        handler_type: FourCC(*b"    "),
        hevc_config: None,
        color_info: None,
        sample_sizes: Vec::new(),
        uniform_sample_size: 0,
        sample_count: 0,
        chunk_offsets: Vec::new(),
        sample_to_chunk: Vec::new(),
        sync_samples: Vec::new(),
    };

    for child in BoxIterator::new(trak.content) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::TKHD => parse_tkhd(&child, &mut info)?,
            FourCC::MDIA => parse_mdia(&child, &mut info, stop)?,
            _ => {}
        }
    }

    Ok(info)
}

/// Parse tkhd (track header) for track_id, width, height
fn parse_tkhd(tkhd: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = tkhd.content;
    if content.len() < 4 {
        return Err(at!(HeicError::InvalidContainer("tkhd too short")));
    }

    let version = content[0];

    // Track ID and dimensions depend on version
    if version == 0 {
        // v0 layout (84 bytes total after box header):
        //   [0-3]: version(1)+flags(3)
        //   [4-7]: creation_time(4)
        //   [8-11]: modification_time(4)
        //   [12-15]: track_ID(4)
        //   [16-19]: reserved(4)
        //   [20-23]: duration(4)
        //   [24-31]: reserved(8)
        //   [32-33]: layer(2)
        //   [34-35]: alt_group(2)
        //   [36-37]: volume(2)
        //   [38-39]: reserved(2)
        //   [40-75]: matrix(36)
        //   [76-79]: width(4, 16.16 fixed point)
        //   [80-83]: height(4, 16.16 fixed point)
        if content.len() < 84 {
            return Err(at!(HeicError::InvalidContainer("tkhd v0 too short")));
        }
        info.track_id = u32::from_be_bytes([content[12], content[13], content[14], content[15]]);
        let w_fixed = u32::from_be_bytes([content[76], content[77], content[78], content[79]]);
        let h_fixed = u32::from_be_bytes([content[80], content[81], content[82], content[83]]);
        info.width = w_fixed >> 16;
        info.height = h_fixed >> 16;
    } else {
        // v1 layout (96 bytes total after box header):
        //   [0-3]: version(1)+flags(3)
        //   [4-11]: creation_time(8)
        //   [12-19]: modification_time(8)
        //   [20-23]: track_ID(4)
        //   [24-27]: reserved(4)
        //   [28-35]: duration(8)
        //   [36-43]: reserved(8)
        //   [44-45]: layer(2)
        //   [46-47]: alt_group(2)
        //   [48-49]: volume(2)
        //   [50-51]: reserved(2)
        //   [52-87]: matrix(36)
        //   [88-91]: width(4, 16.16 fixed point)
        //   [92-95]: height(4, 16.16 fixed point)
        if content.len() < 96 {
            return Err(at!(HeicError::InvalidContainer("tkhd v1 too short")));
        }
        info.track_id = u32::from_be_bytes([content[20], content[21], content[22], content[23]]);
        let w_fixed = u32::from_be_bytes([content[88], content[89], content[90], content[91]]);
        let h_fixed = u32::from_be_bytes([content[92], content[93], content[94], content[95]]);
        info.width = w_fixed >> 16;
        info.height = h_fixed >> 16;
    }

    Ok(())
}

/// Parse mdia box: hdlr + minf
fn parse_mdia(mdia: &Box<'_>, info: &mut TrackInfo, stop: &dyn Stop) -> Result<()> {
    for child in BoxIterator::new(mdia.content) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::HDLR => parse_hdlr_track(&child, info)?,
            FourCC::MINF => parse_minf(&child, info, stop)?,
            _ => {}
        }
    }
    Ok(())
}

/// Parse hdlr for handler_type
fn parse_hdlr_track(hdlr: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = hdlr.content;
    // Full box: version(1)+flags(3) + pre_defined(4) + handler_type(4)
    if content.len() < 12 {
        return Err(at!(HeicError::InvalidContainer("hdlr too short")));
    }
    info.handler_type = FourCC::from_bytes(&content[8..12])
        .ok_or_else(|| at!(HeicError::InvalidContainer("hdlr handler_type")))?;
    Ok(())
}

/// Parse minf → stbl
fn parse_minf(minf: &Box<'_>, info: &mut TrackInfo, stop: &dyn Stop) -> Result<()> {
    for child in BoxIterator::new(minf.content) {
        check_stop(stop)?;
        if child.box_type() == FourCC::STBL {
            parse_stbl(&child, info, stop)?;
        }
    }
    Ok(())
}

/// Parse stbl (sample table) — the core of track sample access
fn parse_stbl(stbl: &Box<'_>, info: &mut TrackInfo, stop: &dyn Stop) -> Result<()> {
    for child in BoxIterator::new(stbl.content) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::STSD => parse_stsd_track(&child, info, stop)?,
            FourCC::STSZ => parse_stsz(&child, info)?,
            FourCC::STCO => parse_stco(&child, info)?,
            FourCC::CO64 => parse_co64(&child, info)?,
            FourCC::STSC => parse_stsc(&child, info)?,
            FourCC::STSS => parse_stss(&child, info)?,
            _ => {}
        }
    }
    Ok(())
}

/// Parse stsd (sample description) for HEVC entries
fn parse_stsd_track(stsd: &Box<'_>, info: &mut TrackInfo, stop: &dyn Stop) -> Result<()> {
    let content = stsd.content;
    // Full box: version(1)+flags(3) + entry_count(4)
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("stsd too short")));
    }
    let entry_count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    if entry_count == 0 {
        return Ok(());
    }

    // Parse entries sequentially
    let mut pos = 8usize;
    for _ in 0..entry_count.min(16) {
        check_stop(stop)?;
        if pos + 8 > content.len() {
            break;
        }
        let entry_size = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]) as usize;
        let entry_type = FourCC::from_bytes(&content[pos + 4..pos + 8])
            .ok_or_else(|| at!(HeicError::InvalidContainer("stsd entry type")))?;

        if entry_size < 8 || pos + entry_size > content.len() {
            break;
        }

        if entry_type == FourCC::HVC1 || entry_type == FourCC::HEV1 {
            parse_visual_sample_entry(&content[pos..pos + entry_size], info, stop)?;
            break; // Use first HEVC entry
        }

        pos += entry_size;
    }

    Ok(())
}

/// Parse a visual sample entry (hvc1/hev1) to extract hvcC and optional colr
fn parse_visual_sample_entry(
    entry_data: &[u8],
    info: &mut TrackInfo,
    stop: &dyn Stop,
) -> Result<()> {
    // Visual sample entry fixed fields:
    //   size(4) + type(4) + reserved(6) + data_ref_idx(2) + pre_defined(2)
    //   + reserved(2) + pre_defined(12) + width(2) + height(2)
    //   + horiz_resolution(4) + vert_resolution(4) + reserved(4)
    //   + frame_count(2) + compressor_name(32) + depth(2) + pre_defined(2)
    // Total fixed: 86 bytes
    let min_visual_entry = 86;
    if entry_data.len() < min_visual_entry {
        return Err(at!(HeicError::InvalidContainer(
            "visual sample entry too short"
        )));
    }

    // Parse nested boxes after the fixed-size visual sample entry header
    let nested_data = &entry_data[min_visual_entry..];
    for child in BoxIterator::new(nested_data) {
        check_stop(stop)?;
        match child.box_type() {
            FourCC::HVCC => {
                info.hevc_config = Some(parse_hvcc(&child)?);
            }
            FourCC::COLR => {
                if let Ok(color) = parse_colr(&child) {
                    info.color_info = Some(color);
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Parse stsz (sample size) box
fn parse_stsz(stsz: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = stsz.content;
    // Full box: version(1)+flags(3) + sample_size(4) + sample_count(4)
    if content.len() < 12 {
        return Err(at!(HeicError::InvalidContainer("stsz too short")));
    }
    let sample_size = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let sample_count = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);

    if sample_count > MAX_SAMPLES {
        return Err(at!(HeicError::LimitExceeded("stsz sample count too large")));
    }

    info.sample_count = sample_count;
    info.uniform_sample_size = sample_size;

    if sample_size == 0 {
        // Per-sample sizes follow
        let needed = 12usize.saturating_add(sample_count as usize * 4);
        if content.len() < needed {
            return Err(at!(HeicError::InvalidContainer(
                "stsz per-sample data truncated"
            )));
        }
        info.sample_sizes
            .try_reserve(sample_count as usize)
            .map_err(|_| at!(HeicError::OutOfMemory))?;
        let mut pos = 12;
        for _ in 0..sample_count {
            let sz = u32::from_be_bytes([
                content[pos],
                content[pos + 1],
                content[pos + 2],
                content[pos + 3],
            ]);
            info.sample_sizes.push(sz);
            pos += 4;
        }
    }

    Ok(())
}

/// Parse stco (chunk offset, 32-bit) box
fn parse_stco(stco: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = stco.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("stco too short")));
    }
    let entry_count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    if entry_count > MAX_CHUNKS {
        return Err(at!(HeicError::LimitExceeded("stco chunk count too large")));
    }
    let needed = 8usize.saturating_add(entry_count as usize * 4);
    if content.len() < needed {
        return Err(at!(HeicError::InvalidContainer("stco data truncated")));
    }
    info.chunk_offsets
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;
    let mut pos = 8;
    for _ in 0..entry_count {
        let offset = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]) as u64;
        info.chunk_offsets.push(offset);
        pos += 4;
    }
    Ok(())
}

/// Parse co64 (chunk offset, 64-bit) box
fn parse_co64(co64: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = co64.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("co64 too short")));
    }
    let entry_count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    if entry_count > MAX_CHUNKS {
        return Err(at!(HeicError::LimitExceeded("co64 chunk count too large")));
    }
    let needed = 8usize.saturating_add(entry_count as usize * 8);
    if content.len() < needed {
        return Err(at!(HeicError::InvalidContainer("co64 data truncated")));
    }
    info.chunk_offsets
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;
    let mut pos = 8;
    for _ in 0..entry_count {
        let offset = u64::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
            content[pos + 4],
            content[pos + 5],
            content[pos + 6],
            content[pos + 7],
        ]);
        info.chunk_offsets.push(offset);
        pos += 8;
    }
    Ok(())
}

/// Parse stsc (sample-to-chunk) box
fn parse_stsc(stsc: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = stsc.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("stsc too short")));
    }
    let entry_count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    if entry_count > MAX_STSC_ENTRIES {
        return Err(at!(HeicError::LimitExceeded("stsc entry count too large")));
    }
    let needed = 8usize.saturating_add(entry_count as usize * 12);
    if content.len() < needed {
        return Err(at!(HeicError::InvalidContainer("stsc data truncated")));
    }
    info.sample_to_chunk
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;
    let mut pos = 8;
    for _ in 0..entry_count {
        let first_chunk = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        let samples_per_chunk = u32::from_be_bytes([
            content[pos + 4],
            content[pos + 5],
            content[pos + 6],
            content[pos + 7],
        ]);
        let sample_desc_idx = u32::from_be_bytes([
            content[pos + 8],
            content[pos + 9],
            content[pos + 10],
            content[pos + 11],
        ]);
        info.sample_to_chunk
            .push((first_chunk, samples_per_chunk, sample_desc_idx));
        pos += 12;
    }
    Ok(())
}

/// Parse stss (sync sample) box
fn parse_stss(stss: &Box<'_>, info: &mut TrackInfo) -> Result<()> {
    let content = stss.content;
    if content.len() < 8 {
        return Err(at!(HeicError::InvalidContainer("stss too short")));
    }
    let entry_count = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    if entry_count > MAX_SYNC_SAMPLES {
        return Err(at!(HeicError::LimitExceeded(
            "stss sync sample count too large"
        )));
    }
    let needed = 8usize.saturating_add(entry_count as usize * 4);
    if content.len() < needed {
        return Err(at!(HeicError::InvalidContainer("stss data truncated")));
    }
    info.sync_samples
        .try_reserve(entry_count as usize)
        .map_err(|_| at!(HeicError::OutOfMemory))?;
    let mut pos = 8;
    for _ in 0..entry_count {
        let sample = u32::from_be_bytes([
            content[pos],
            content[pos + 1],
            content[pos + 2],
            content[pos + 3],
        ]);
        info.sync_samples.push(sample);
        pos += 4;
    }
    Ok(())
}

/// Resolve a 1-based sample number to a file byte offset using stco + stsc tables.
///
/// The stsc table maps runs of chunks to their sample counts. For each chunk,
/// we know how many samples it contains and the chunk's starting byte offset
/// from stco/co64. We walk through chunks counting samples until we find the
/// chunk containing our target sample, then add up preceding sample sizes
/// within that chunk.
fn resolve_sample_offset(
    track: &TrackInfo,
    sample_number: u32,
    file_len: u64,
    stop: &dyn Stop,
) -> Result<u64> {
    if sample_number == 0 || sample_number > track.sample_count {
        return Err(at!(HeicError::InvalidData("sample number out of range")));
    }
    if track.chunk_offsets.is_empty() {
        return Err(at!(HeicError::InvalidData("no chunk offsets")));
    }
    if track.sample_to_chunk.is_empty() {
        return Err(at!(HeicError::InvalidData("no sample-to-chunk mapping")));
    }

    let num_chunks = track.chunk_offsets.len() as u32;
    let target = sample_number; // 1-based

    // Walk through stsc entries to find which chunk contains the target sample
    let mut current_sample: u32 = 1; // 1-based sample counter
    let stsc = &track.sample_to_chunk;

    for (entry_idx, &(first_chunk, samples_per_chunk, _desc_idx)) in stsc.iter().enumerate() {
        check_stop(stop)?;
        // first_chunk is 1-based
        let next_first_chunk = if entry_idx + 1 < stsc.len() {
            stsc[entry_idx + 1].0
        } else {
            num_chunks + 1 // extends to end
        };

        if first_chunk > num_chunks || next_first_chunk < first_chunk {
            return Err(at!(HeicError::InvalidData("invalid stsc first_chunk")));
        }

        // Iterate through chunks in this stsc run
        for chunk_1based in first_chunk..next_first_chunk {
            let chunk_end_sample = current_sample
                .checked_add(samples_per_chunk)
                .ok_or_else(|| at!(HeicError::InvalidData("sample count overflow")))?;

            if target >= current_sample && target < chunk_end_sample {
                // Target sample is in this chunk
                let chunk_idx = (chunk_1based - 1) as usize;
                if chunk_idx >= track.chunk_offsets.len() {
                    return Err(at!(HeicError::InvalidData("chunk index out of range")));
                }
                let chunk_offset = track.chunk_offsets[chunk_idx];

                // Add up sizes of preceding samples in this chunk
                let samples_before = target - current_sample;
                let first_sample_in_chunk = current_sample;
                let mut byte_offset = chunk_offset;
                for s in 0..samples_before {
                    let sample_idx = (first_sample_in_chunk + s - 1) as usize; // 0-based
                    let sz = if track.uniform_sample_size > 0 {
                        track.uniform_sample_size as u64
                    } else if sample_idx < track.sample_sizes.len() {
                        track.sample_sizes[sample_idx] as u64
                    } else {
                        return Err(at!(HeicError::InvalidData(
                            "sample size index out of range"
                        )));
                    };
                    byte_offset = byte_offset
                        .checked_add(sz)
                        .ok_or_else(|| at!(HeicError::InvalidData("byte offset overflow")))?;
                }

                if byte_offset >= file_len {
                    return Err(at!(HeicError::InvalidData(
                        "resolved sample offset past end of file"
                    )));
                }
                return Ok(byte_offset);
            }

            current_sample = chunk_end_sample;
        }
    }

    Err(at!(HeicError::InvalidData(
        "sample not found in stsc mapping"
    )))
}
