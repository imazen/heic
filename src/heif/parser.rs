//! HEIF container parser

#![allow(dead_code)]

use alloc::borrow::Cow;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::str;

use enough::Stop;

use super::boxes::{
    Box, BoxIterator, CleanAperture, ColorInfo, FourCC, HevcDecoderConfig, ImageMirror,
    ImageRotation, ImageSpatialExtents, ItemInfo, ItemLocation, ItemProperty, ItemReference,
    PropertyAssociation, Transform,
};
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
pub enum ItemType {
    /// HEVC coded image
    Hvc1,
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
    /// Auxiliary type URI (from auxC property, e.g. "urn:mpeg:hevc:2015:auxid:1" for alpha)
    pub auxiliary_type: Option<String>,
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
        let mut clean_aperture = None;
        let mut rotation = None;
        let mut mirror = None;
        let mut transforms = Vec::new();
        let mut color_info = None;
        let mut auxiliary_type = None;

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
                        ItemProperty::AuxiliaryType(s) => {
                            auxiliary_type = Some(s.clone());
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
            clean_aperture,
            rotation,
            mirror,
            transforms,
            color_info,
            auxiliary_type,
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
            .ok_or(HeicError::InvalidData("item not found in iloc"))?;

        if loc.extents.is_empty() {
            return Err(HeicError::InvalidData("item has no extents").into());
        }

        let source = match loc.construction_method {
            0 => self.data, // File offset (typically into mdat)
            1 => self
                .idat_data
                .ok_or(HeicError::InvalidData("construction_method=1 but no idat"))?,
            _ => return Err(HeicError::Unsupported("construction_method >= 2").into()),
        };

        if loc.extents.len() == 1 {
            // Single extent: return slice directly (zero-copy)
            let (offset, length) = loc.extents[0];
            let offset = usize::try_from(
                loc.base_offset
                    .checked_add(offset)
                    .ok_or(HeicError::InvalidData("extent offset overflow"))?,
            )
            .map_err(|_| HeicError::InvalidData("extent offset too large"))?;
            let length = usize::try_from(length)
                .map_err(|_| HeicError::InvalidData("extent length too large"))?;
            let end = offset
                .checked_add(length)
                .ok_or(HeicError::InvalidData("extent end overflow"))?;
            if end > source.len() {
                return Err(HeicError::InvalidData("extent extends past end of data").into());
            }
            return Ok(Cow::Borrowed(&source[offset..end]));
        }

        // Multiple extents: concatenate into owned buffer
        let total_len: u64 = loc
            .extents
            .iter()
            .map(|&(_, len)| len)
            .try_fold(0u64, |acc, len| acc.checked_add(len))
            .ok_or(HeicError::InvalidData("multi-extent total length overflow"))?;
        let total_len = usize::try_from(total_len)
            .map_err(|_| HeicError::InvalidData("multi-extent total too large"))?;

        let mut buf = Vec::new();
        buf.try_reserve(total_len)
            .map_err(|_| HeicError::OutOfMemory)?;

        for &(extent_offset, extent_length) in &loc.extents {
            let offset = usize::try_from(
                loc.base_offset
                    .checked_add(extent_offset)
                    .ok_or(HeicError::InvalidData("extent offset overflow"))?,
            )
            .map_err(|_| HeicError::InvalidData("extent offset too large"))?;
            let length = usize::try_from(extent_length)
                .map_err(|_| HeicError::InvalidData("extent length too large"))?;
            let end = offset
                .checked_add(length)
                .ok_or(HeicError::InvalidData("extent end overflow"))?;
            if end > source.len() {
                return Err(HeicError::InvalidData("extent extends past end of data").into());
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
                if let Some(ref aux_type) = item.auxiliary_type
                    && aux_type.starts_with(aux_type_prefix)
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

    // Parse top-level boxes
    for top_box in BoxIterator::new(data) {
        check_stop(stop)?;
        match top_box.box_type() {
            FourCC::FTYP => parse_ftyp(&top_box, &mut container)?,
            FourCC::META => parse_meta(&top_box, &mut container, stop)?,
            FourCC::MDAT => {
                container.mdat_offset = Some(top_box.header.content_offset);
                container.mdat_length = Some(top_box.content.len());
            }
            _ => {} // Ignore unknown boxes
        }
    }

    // Verify we have required boxes
    if container.brand.0 == *b"    " {
        return Err(HeicError::InvalidContainer("missing ftyp box").into());
    }

    Ok(container)
}

fn parse_ftyp(ftyp: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = ftyp.content;
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("ftyp too short").into());
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
    ];

    let is_heif = valid_brands.contains(&container.brand)
        || container
            .compatible_brands
            .iter()
            .any(|b| valid_brands.contains(b));

    if !is_heif {
        return Err(HeicError::InvalidContainer("not a HEIF file").into());
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
        return Err(HeicError::InvalidContainer("meta box too short").into());
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
        return Err(HeicError::InvalidContainer("pitm too short").into());
    }

    let version = content[0];
    if version == 0 {
        if content.len() < 6 {
            return Err(HeicError::InvalidContainer("pitm v0 too short").into());
        }
        container.primary_item_id = u16::from_be_bytes([content[4], content[5]]) as u32;
    } else {
        if content.len() < 8 {
            return Err(HeicError::InvalidContainer("pitm v1 too short").into());
        }
        container.primary_item_id =
            u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    }

    Ok(())
}

fn parse_iloc(iloc: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = iloc.content;
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("iloc too short").into());
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
        return Err(HeicError::LimitExceeded("iloc item count exceeds limit").into());
    }

    container
        .item_locations
        .try_reserve(item_count as usize)
        .map_err(|_| HeicError::OutOfMemory)?;

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
            return Err(HeicError::LimitExceeded("extent count per item exceeds limit").into());
        }

        let mut extents = Vec::new();
        extents
            .try_reserve(extent_count as usize)
            .map_err(|_| HeicError::OutOfMemory)?;
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
        return Err(HeicError::InvalidContainer("iinf too short").into());
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
        return Err(HeicError::LimitExceeded("iinf entry count exceeds limit").into());
    }

    container
        .item_infos
        .try_reserve(entry_count as usize)
        .map_err(|_| HeicError::OutOfMemory)?;

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
        .ok_or(HeicError::InvalidContainer("infe too short"))?;
    // Minimum: 4 (ver+flags) + id (2 or 4) + 2 (protection) + type (4 if v>=2)
    let min_len = match version {
        0..=1 => 4 + 2 + 2, // 8
        2 => 4 + 2 + 2 + 4, // 12
        _ => 4 + 4 + 2 + 4, // 14 (version >= 3 uses 4-byte item_id)
    };
    if content.len() < min_len {
        return Err(HeicError::InvalidContainer("infe too short").into());
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
        return Err(HeicError::InvalidContainer("item name too long").into());
    }
    let item_name = str::from_utf8(&content[pos..pos + name_end])
        .unwrap_or("")
        .to_string();
    pos += name_end + 1;

    // Content type (null-terminated string, optional)
    let content_type = if pos < content.len() {
        let ct_end = content[pos..].iter().position(|&b| b == 0).unwrap_or(0);
        if ct_end > MAX_STRING_LENGTH {
            return Err(HeicError::InvalidContainer("content type too long").into());
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
            return Err(HeicError::LimitExceeded("property count exceeds limit").into());
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
        return Err(HeicError::InvalidContainer("clap too short").into());
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
        return Err(HeicError::InvalidContainer("clap has zero denominator").into());
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
        return Err(HeicError::InvalidContainer("irot too short").into());
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
        return Err(HeicError::InvalidContainer("imir too short").into());
    }
    Ok(ImageMirror {
        axis: content[0] & 0x01,
    })
}

fn parse_auxc(auxc: &Box<'_>) -> Result<String> {
    let content = auxc.content;
    // auxC is a full box: version/flags (4 bytes) + null-terminated UTF-8 aux_type string
    if content.len() < 5 {
        return Err(HeicError::InvalidContainer("auxC too short").into());
    }

    // Skip version/flags (4 bytes)
    let data = &content[4..];
    // Find null terminator
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    if end > MAX_STRING_LENGTH {
        return Err(HeicError::InvalidContainer("auxC string too long").into());
    }
    let aux_type = str::from_utf8(&data[..end]).unwrap_or("").to_string();
    Ok(aux_type)
}

fn parse_ispe(ispe: &Box<'_>) -> Result<ImageSpatialExtents> {
    let content = ispe.content;
    if content.len() < 12 {
        return Err(HeicError::InvalidContainer("ispe too short").into());
    }

    // Skip version/flags (4 bytes)
    let width = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let height = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);

    // Validate dimensions are non-zero and reasonable
    if width == 0 || height == 0 {
        return Err(HeicError::InvalidContainer("ispe has zero dimension").into());
    }
    if width > (1 << 30) || height > (1 << 30) {
        return Err(HeicError::InvalidContainer("ispe dimensions too large").into());
    }

    Ok(ImageSpatialExtents { width, height })
}

fn parse_hvcc(hvcc: &Box<'_>) -> Result<HevcDecoderConfig> {
    let content = hvcc.content;
    if content.len() < 23 {
        return Err(HeicError::InvalidContainer("hvcC too short").into());
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
        return Err(HeicError::InvalidContainer("hvcC invalid length_size_minus_one=2").into());
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
                return Err(HeicError::LimitExceeded("NAL unit too large").into());
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
        return Err(HeicError::InvalidContainer("colr too short").into());
    }

    let color_type = FourCC::from_bytes(&content[0..4]).unwrap();

    match &color_type.0 {
        b"nclx" => {
            if content.len() < 11 {
                return Err(HeicError::InvalidContainer("nclx colr too short").into());
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
                return Err(HeicError::LimitExceeded("ICC profile too large").into());
            }
            Ok(ColorInfo::IccProfile(icc_data.to_vec()))
        }
        _ => Err(HeicError::InvalidContainer("unknown color type").into()),
    }
}

fn parse_iref(iref: &Box<'_>, container: &mut HeifContainer<'_>, stop: &dyn Stop) -> Result<()> {
    let content = iref.content;
    if content.len() < 4 {
        return Err(HeicError::InvalidContainer("iref too short").into());
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
                return Err(HeicError::LimitExceeded("reference count exceeds limit").into());
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
                return Err(HeicError::LimitExceeded("refs per entry exceeds limit").into());
            }

            let mut to_ids = Vec::new();
            to_ids
                .try_reserve(ref_count as usize)
                .map_err(|_| HeicError::OutOfMemory)?;
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
        return Err(HeicError::InvalidContainer("ipma too short").into());
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
        return Err(HeicError::LimitExceeded("ipma entry count exceeds limit").into());
    }

    container
        .property_associations
        .try_reserve(entry_count as usize)
        .map_err(|_| HeicError::OutOfMemory)?;

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
