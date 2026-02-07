//! HEIF container parser

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::str;

use super::boxes::{
    Box, BoxIterator, ColorInfo, FourCC, HevcDecoderConfig, ImageSpatialExtents, ItemInfo,
    ItemLocation, ItemProperty, ItemReference, PropertyAssociation,
};
use crate::error::{HeicError, Result};

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
    /// Image spatial extents (indexed by property index) - DEPRECATED, use properties
    pub image_extents: Vec<ImageSpatialExtents>,
    /// HEVC decoder configs (indexed by property index) - DEPRECATED, use properties
    pub hevc_configs: Vec<HevcDecoderConfig>,
    /// Color info (indexed by property index) - DEPRECATED, use properties
    pub color_infos: Vec<ColorInfo>,
    /// Property associations
    pub property_associations: Vec<PropertyAssociation>,
    /// Item references (from iref box)
    pub item_references: Vec<ItemReference>,
    /// Media data offset
    mdat_offset: Option<usize>,
    /// Media data length
    mdat_length: Option<usize>,
    /// Item data (idat) offset within meta box content
    idat_offset: Option<usize>,
    /// Item data (idat) length
    idat_length: Option<usize>,
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

        if let Some(assoc) = assoc {
            for &(prop_idx, _essential) in &assoc.properties {
                let idx = prop_idx as usize - 1; // 1-based index in ipma
                if let Some(prop) = self.properties.get(idx) {
                    match prop {
                        ItemProperty::ImageExtents(ext) => {
                            dimensions = Some((ext.width, ext.height));
                        }
                        ItemProperty::HevcConfig(config) => {
                            hevc_config = Some(config.clone());
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
        })
    }

    /// Get raw data for an item
    pub fn get_item_data(&self, item_id: u32) -> Option<&[u8]> {
        let loc = self.item_locations.iter().find(|l| l.item_id == item_id)?;

        if loc.extents.is_empty() {
            return None;
        }

        // For single-extent items, return a direct slice
        if loc.extents.len() == 1 {
            let (offset, length) = loc.extents[0];
            let length = length as usize;

            match loc.construction_method {
                0 => {
                    // File-based: offset is absolute within file
                    let abs_offset = (loc.base_offset + offset) as usize;
                    if abs_offset + length <= self.data.len() {
                        return Some(&self.data[abs_offset..abs_offset + length]);
                    }
                }
                1 => {
                    // idat-based: offset is within idat box content
                    if let Some(idat_off) = self.idat_offset {
                        let abs_offset = idat_off + (loc.base_offset + offset) as usize;
                        if abs_offset + length <= self.data.len() {
                            return Some(&self.data[abs_offset..abs_offset + length]);
                        }
                    }
                }
                _ => {} // construction_method=2 (item) not supported yet
            }
            return None;
        }

        // For multi-extent items, use get_item_data_owned
        None
    }

    /// Get raw data for an item, concatenating multiple extents if needed
    pub fn get_item_data_owned(&self, item_id: u32) -> Option<Vec<u8>> {
        let loc = self.item_locations.iter().find(|l| l.item_id == item_id)?;

        if loc.extents.is_empty() {
            return None;
        }

        let mut result = Vec::new();
        for &(offset, length) in &loc.extents {
            let len = length as usize;
            let abs_offset = match loc.construction_method {
                0 => (loc.base_offset + offset) as usize,
                1 => {
                    self.idat_offset? + (loc.base_offset + offset) as usize
                }
                _ => return None,
            };
            if abs_offset + len > self.data.len() {
                return None;
            }
            result.extend_from_slice(&self.data[abs_offset..abs_offset + len]);
        }

        Some(result)
    }

    /// Get tile item IDs for a grid item (from iref 'dimg' references)
    pub fn get_tile_item_ids(&self, grid_item_id: u32) -> Option<Vec<u32>> {
        self.item_references.iter()
            .find(|r| r.from_item_id == grid_item_id && r.ref_type == FourCC::DIMG)
            .map(|r| r.to_item_ids.clone())
    }
}

/// Parse a HEIF container
pub fn parse(data: &[u8]) -> Result<HeifContainer<'_>> {
    let mut container = HeifContainer {
        data,
        brand: FourCC(*b"    "),
        compatible_brands: Vec::new(),
        primary_item_id: 0,
        item_locations: Vec::new(),
        item_infos: Vec::new(),
        properties: Vec::new(),
        image_extents: Vec::new(),
        hevc_configs: Vec::new(),
        color_infos: Vec::new(),
        property_associations: Vec::new(),
        item_references: Vec::new(),
        mdat_offset: None,
        mdat_length: None,
        idat_offset: None,
        idat_length: None,
    };

    // Parse top-level boxes
    for top_box in BoxIterator::new(data) {
        match top_box.box_type() {
            FourCC::FTYP => parse_ftyp(&top_box, &mut container)?,
            FourCC::META => parse_meta(&top_box, &mut container)?,
            FourCC::MDAT => {
                container.mdat_offset = Some(top_box.header.content_offset);
                container.mdat_length = Some(top_box.content.len());
            }
            _ => {} // Ignore unknown boxes
        }
    }

    // Verify we have required boxes
    if container.brand.0 == *b"    " {
        return Err(HeicError::InvalidContainer("missing ftyp box"));
    }

    Ok(container)
}

fn parse_ftyp(ftyp: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = ftyp.content;
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("ftyp too short"));
    }

    container.brand = FourCC::from_bytes(&content[0..4]).unwrap();

    // Skip minor version (4 bytes)
    let mut offset = 8;
    while offset + 4 <= content.len() {
        if let Some(brand) = FourCC::from_bytes(&content[offset..]) {
            container.compatible_brands.push(brand);
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
        return Err(HeicError::InvalidContainer("not a HEIF file"));
    }

    Ok(())
}

fn parse_meta(meta: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    // Meta is a full box - skip version/flags
    if meta.content.len() < 4 {
        return Err(HeicError::InvalidContainer("meta box too short"));
    }

    let content = &meta.content[4..];
    // Base offset for child boxes within this meta content (for idat resolution)
    let meta_content_base = meta.header.content_offset + 4;

    for child in BoxIterator::new(content) {
        match child.box_type() {
            FourCC::PITM => parse_pitm(&child, container)?,
            FourCC::ILOC => parse_iloc(&child, container)?,
            FourCC::IINF => parse_iinf(&child, container)?,
            FourCC::IPRP => parse_iprp(&child, container)?,
            FourCC::IREF => parse_iref(&child, container)?,
            FourCC::IDAT => {
                // Store absolute file offset for idat content
                container.idat_offset = Some(meta_content_base + child.header.content_offset);
                container.idat_length = Some(child.content.len());
            }
            _ => {} // hdlr, etc.
        }
    }

    Ok(())
}

fn parse_pitm(pitm: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = pitm.content;
    if content.len() < 4 {
        return Err(HeicError::InvalidContainer("pitm too short"));
    }

    let version = content[0];
    if version == 0 {
        if content.len() < 6 {
            return Err(HeicError::InvalidContainer("pitm v0 too short"));
        }
        container.primary_item_id = u16::from_be_bytes([content[4], content[5]]) as u32;
    } else {
        if content.len() < 8 {
            return Err(HeicError::InvalidContainer("pitm v1 too short"));
        }
        container.primary_item_id =
            u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    }

    Ok(())
}

fn parse_iloc(iloc: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = iloc.content;
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("iloc too short"));
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

    for _ in 0..item_count {
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
            let method = content[pos + 1] & 0xF;
            pos += 2;
            method
        } else {
            0
        };

        // Data reference index (2 bytes) - skip
        pos += 2;

        let base_offset = read_sized_int(content, &mut pos, base_offset_size as usize);

        let extent_count = u16::from_be_bytes([content[pos], content[pos + 1]]);
        pos += 2;

        let mut extents = Vec::with_capacity(extent_count as usize);
        for _ in 0..extent_count {
            if version >= 1 && index_size > 0 {
                // Extent index - skip
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

fn parse_iinf(iinf: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = iinf.content;
    if content.len() < 6 {
        return Err(HeicError::InvalidContainer("iinf too short"));
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

    // Parse infe boxes
    let remaining = &content[pos..];
    let mut infe_count = 0;
    for child in BoxIterator::new(remaining) {
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
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("infe too short"));
    }

    let version = content[0];
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
    let name_end = content[pos..].iter().position(|&b| b == 0).unwrap_or(0);
    let item_name = str::from_utf8(&content[pos..pos + name_end])
        .unwrap_or("")
        .to_string();
    pos += name_end + 1;

    // Content type (null-terminated string, optional)
    let content_type = if pos < content.len() {
        let ct_end = content[pos..].iter().position(|&b| b == 0).unwrap_or(0);
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

fn parse_iprp(iprp: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    for child in BoxIterator::new(iprp.content) {
        match child.box_type() {
            FourCC::IPCO => parse_ipco(&child, container)?,
            FourCC::IPMA => parse_ipma(&child, container)?,
            _ => {}
        }
    }
    Ok(())
}

fn parse_ipco(ipco: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    // Properties are stored in order - index is implicit (1-based in ipma, 0-based here)
    for child in BoxIterator::new(ipco.content) {
        let prop = match child.box_type() {
            FourCC::ISPE => {
                if let Ok(ext) = parse_ispe(&child) {
                    container.image_extents.push(ext); // Keep deprecated for now
                    ItemProperty::ImageExtents(ext)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::HVCC => {
                if let Ok(config) = parse_hvcc(&child) {
                    container.hevc_configs.push(config.clone()); // Keep deprecated for now
                    ItemProperty::HevcConfig(config)
                } else {
                    ItemProperty::Unknown
                }
            }
            FourCC::COLR => {
                if let Ok(color) = parse_colr(&child) {
                    container.color_infos.push(color.clone()); // Keep deprecated for now
                    ItemProperty::ColorInfo(color)
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

fn parse_ispe(ispe: &Box<'_>) -> Result<ImageSpatialExtents> {
    let content = ispe.content;
    if content.len() < 12 {
        return Err(HeicError::InvalidContainer("ispe too short"));
    }

    // Skip version/flags (4 bytes)
    let width = u32::from_be_bytes([content[4], content[5], content[6], content[7]]);
    let height = u32::from_be_bytes([content[8], content[9], content[10], content[11]]);

    Ok(ImageSpatialExtents { width, height })
}

fn parse_hvcc(hvcc: &Box<'_>) -> Result<HevcDecoderConfig> {
    let content = hvcc.content;
    if content.len() < 23 {
        return Err(HeicError::InvalidContainer("hvcC too short"));
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
        return Err(HeicError::InvalidContainer("colr too short"));
    }

    let color_type = FourCC::from_bytes(&content[0..4]).unwrap();

    match &color_type.0 {
        b"nclx" => {
            if content.len() < 11 {
                return Err(HeicError::InvalidContainer("nclx colr too short"));
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
            Ok(ColorInfo::IccProfile(content[4..].to_vec()))
        }
        _ => Err(HeicError::InvalidContainer("unknown color type")),
    }
}

fn parse_ipma(ipma: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = ipma.content;
    if content.len() < 8 {
        return Err(HeicError::InvalidContainer("ipma too short"));
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

    for _ in 0..entry_count {
        if pos + 2 > content.len() {
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

fn parse_iref(iref: &Box<'_>, container: &mut HeifContainer<'_>) -> Result<()> {
    let content = iref.content;
    if content.len() < 4 {
        return Err(HeicError::InvalidContainer("iref too short"));
    }

    let version = content[0];
    let mut pos = 4;

    while pos + 8 <= content.len() {
        // Each reference is a box-like structure: size(4) + type(4) + data
        let ref_size = u32::from_be_bytes([
            content[pos], content[pos + 1], content[pos + 2], content[pos + 3],
        ]) as usize;
        
        if ref_size < 14 || pos + ref_size > content.len() {
            break;
        }

        let ref_type = FourCC::from_bytes(&content[pos + 4..pos + 8])
            .unwrap_or(FourCC(*b"    "));

        let mut rpos = pos + 8;

        // from_item_id
        let from_item_id = if version == 0 {
            let id = u16::from_be_bytes([content[rpos], content[rpos + 1]]) as u32;
            rpos += 2;
            id
        } else {
            let id = u32::from_be_bytes([
                content[rpos], content[rpos + 1], content[rpos + 2], content[rpos + 3],
            ]);
            rpos += 4;
            id
        };

        // reference_count
        if rpos + 2 > pos + ref_size {
            pos += ref_size;
            continue;
        }
        let ref_count = u16::from_be_bytes([content[rpos], content[rpos + 1]]) as usize;
        rpos += 2;

        let mut to_item_ids = Vec::with_capacity(ref_count);
        for _ in 0..ref_count {
            let to_id = if version == 0 {
                if rpos + 2 > pos + ref_size {
                    break;
                }
                let id = u16::from_be_bytes([content[rpos], content[rpos + 1]]) as u32;
                rpos += 2;
                id
            } else {
                if rpos + 4 > pos + ref_size {
                    break;
                }
                let id = u32::from_be_bytes([
                    content[rpos], content[rpos + 1], content[rpos + 2], content[rpos + 3],
                ]);
                rpos += 4;
                id
            };
            to_item_ids.push(to_id);
        }

        container.item_references.push(ItemReference {
            ref_type,
            from_item_id,
            to_item_ids,
        });

        pos += ref_size;
    }

    Ok(())
}

/// Parsed image grid configuration (ISO/IEC 23008-12, A.2.3.2)
#[derive(Debug, Clone)]
pub struct ImageGrid {
    /// Number of tile columns
    pub columns: u32,
    /// Number of tile rows
    pub rows: u32,
    /// Output width in pixels
    pub output_width: u32,
    /// Output height in pixels
    pub output_height: u32,
}

/// Parse grid item data into ImageGrid configuration
pub fn parse_grid_config(data: &[u8]) -> core::result::Result<ImageGrid, HeicError> {
    // ImageGrid: version(1) + flags(1) + rows_minus1(1) + cols_minus1(1) + output_width + output_height
    if data.len() < 8 {
        return Err(HeicError::InvalidData("grid data too short"));
    }

    let _version = data[0];
    let flags = data[1];
    let rows = data[2] as u32 + 1;
    let columns = data[3] as u32 + 1;

    // fields_length: 0 = 16-bit, 1 = 32-bit dimensions
    let large_fields = (flags & 1) != 0;

    let (output_width, output_height) = if large_fields {
        if data.len() < 12 {
            return Err(HeicError::InvalidData("grid data too short for 32-bit dims"));
        }
        (
            u32::from_be_bytes([data[4], data[5], data[6], data[7]]),
            u32::from_be_bytes([data[8], data[9], data[10], data[11]]),
        )
    } else {
        (
            u16::from_be_bytes([data[4], data[5]]) as u32,
            u16::from_be_bytes([data[6], data[7]]) as u32,
        )
    };

    Ok(ImageGrid {
        columns,
        rows,
        output_width,
        output_height,
    })
}
