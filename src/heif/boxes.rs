//! ISOBMFF box definitions and parsing

use alloc::string::String;
use alloc::vec::Vec;

/// Four-character code identifying a box type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FourCC(pub [u8; 4]);

impl FourCC {
    pub const FTYP: Self = Self(*b"ftyp");
    pub const META: Self = Self(*b"meta");
    pub const HDLR: Self = Self(*b"hdlr");
    pub const PITM: Self = Self(*b"pitm");
    pub const ILOC: Self = Self(*b"iloc");
    pub const IINF: Self = Self(*b"iinf");
    pub const INFE: Self = Self(*b"infe");
    pub const IPRP: Self = Self(*b"iprp");
    pub const IPCO: Self = Self(*b"ipco");
    pub const IPMA: Self = Self(*b"ipma");
    pub const MDAT: Self = Self(*b"mdat");
    pub const ISPE: Self = Self(*b"ispe");
    pub const HVCB: Self = Self(*b"hvcB");
    pub const HVCC: Self = Self(*b"hvcC");
    pub const COLR: Self = Self(*b"colr");
    pub const PIXI: Self = Self(*b"pixi");
    pub const IREF: Self = Self(*b"iref");
    pub const AUXC: Self = Self(*b"auxC");
    pub const DIMG: Self = Self(*b"dimg");
    pub const THMB: Self = Self(*b"thmb");
    pub const IDAT: Self = Self(*b"idat");

    /// Create from bytes
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() >= 4 {
            Some(Self([bytes[0], bytes[1], bytes[2], bytes[3]]))
        } else {
            None
        }
    }

    /// Convert to string for debugging
    pub fn as_str(&self) -> &str {
        core::str::from_utf8(&self.0).unwrap_or("????")
    }
}

impl core::fmt::Display for FourCC {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Raw box header
#[derive(Debug, Clone, Copy)]
pub struct BoxHeader {
    /// Box type
    pub box_type: FourCC,
    /// Total box size including header
    pub size: u64,
    /// Offset to box content (after header)
    pub content_offset: usize,
}

/// A parsed ISOBMFF box
#[derive(Debug)]
pub struct Box<'a> {
    /// Box header
    pub header: BoxHeader,
    /// Box content (excluding header)
    pub content: &'a [u8],
}

impl<'a> Box<'a> {
    /// Get box type
    pub fn box_type(&self) -> FourCC {
        self.header.box_type
    }

    /// Get full box version and flags (for full boxes)
    pub fn version_flags(&self) -> Option<(u8, u32)> {
        if self.content.len() >= 4 {
            let version = self.content[0];
            let flags = u32::from_be_bytes([0, self.content[1], self.content[2], self.content[3]]);
            Some((version, flags))
        } else {
            None
        }
    }
}

/// Box iterator for parsing sequential boxes
pub struct BoxIterator<'a> {
    data: &'a [u8],
    offset: usize,
}

impl<'a> BoxIterator<'a> {
    /// Create a new box iterator
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0 }
    }
}

impl<'a> Iterator for BoxIterator<'a> {
    type Item = Box<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset + 8 > self.data.len() {
            return None;
        }

        let data = &self.data[self.offset..];

        // Read size (4 bytes, big-endian)
        let size_32 = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
        let box_type = FourCC::from_bytes(&data[4..8])?;

        let (size, header_size): (u64, usize) = if size_32 == 1 {
            // Extended size (64-bit)
            if data.len() < 16 {
                return None;
            }
            let ext_size = u64::from_be_bytes([
                data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
            ]);
            (ext_size, 16)
        } else if size_32 == 0 {
            // Box extends to end of file
            ((self.data.len() - self.offset) as u64, 8)
        } else {
            (size_32 as u64, 8)
        };

        let content_len = size as usize - header_size;
        if self.offset + header_size + content_len > self.data.len() {
            return None;
        }

        let content = &data[header_size..size as usize];

        let box_item = Box {
            header: BoxHeader {
                box_type,
                size,
                content_offset: self.offset + header_size,
            },
            content,
        };

        self.offset += size as usize;
        Some(box_item)
    }
}

/// Item location entry from iloc box
#[derive(Debug, Clone)]
pub struct ItemLocation {
    /// Item ID
    pub item_id: u32,
    /// Construction method (0=file, 1=idat, 2=item)
    pub construction_method: u8,
    /// Base offset
    pub base_offset: u64,
    /// Extents (offset, length pairs)
    pub extents: Vec<(u64, u64)>,
}

/// Item info entry from iinf/infe boxes
#[derive(Debug, Clone)]
pub struct ItemInfo {
    /// Item ID
    pub item_id: u32,
    /// Item type (e.g., "hvc1", "grid", "Exif")
    pub item_type: FourCC,
    /// Item name (optional)
    pub item_name: String,
    /// Content type (optional)
    pub content_type: String,
    /// Hidden flag
    pub hidden: bool,
}

/// Image spatial extents from ispe box
#[derive(Debug, Clone, Copy)]
pub struct ImageSpatialExtents {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
}

/// HEVC decoder configuration from hvcC box
#[derive(Debug, Clone)]
pub struct HevcDecoderConfig {
    /// Configuration version
    pub config_version: u8,
    /// General profile space
    pub general_profile_space: u8,
    /// General tier flag
    pub general_tier_flag: bool,
    /// General profile IDC
    pub general_profile_idc: u8,
    /// General profile compatibility flags
    pub general_profile_compatibility_flags: u32,
    /// General constraint indicator flags
    pub general_constraint_indicator_flags: u64,
    /// General level IDC
    pub general_level_idc: u8,
    /// Chroma format
    pub chroma_format: u8,
    /// Bit depth luma minus 8
    pub bit_depth_luma_minus8: u8,
    /// Bit depth chroma minus 8
    pub bit_depth_chroma_minus8: u8,
    /// Length size minus one
    pub length_size_minus_one: u8,
    /// NAL units (VPS, SPS, PPS, etc.)
    pub nal_units: Vec<Vec<u8>>,
}

/// Color information from colr box
#[derive(Debug, Clone)]
pub enum ColorInfo {
    /// ICC profile
    IccProfile(Vec<u8>),
    /// nclx color parameters
    Nclx {
        /// Color primaries
        color_primaries: u16,
        /// Transfer characteristics
        transfer_characteristics: u16,
        /// Matrix coefficients
        matrix_coefficients: u16,
        /// Full range flag
        full_range: bool,
    },
}

/// Item property (indexed in ipco)
#[derive(Debug, Clone)]
pub enum ItemProperty {
    /// Image spatial extents (ispe)
    ImageExtents(ImageSpatialExtents),
    /// HEVC decoder config (hvcC)
    HevcConfig(HevcDecoderConfig),
    /// Color info (colr)
    ColorInfo(ColorInfo),
    /// Unknown property
    Unknown,
}

/// Item property association
#[derive(Debug, Clone)]
pub struct PropertyAssociation {
    /// Item ID
    pub item_id: u32,
    /// Property indices (1-based, essential flag)
    pub properties: Vec<(u16, bool)>,
}

/// Item reference from iref box
#[derive(Debug, Clone)]
pub struct ItemReference {
    /// Reference type (e.g., "dimg" for derived image, "thmb" for thumbnail)
    pub ref_type: FourCC,
    /// Source item ID (the item that references others)
    pub from_item_id: u32,
    /// Referenced item IDs
    pub to_item_ids: Vec<u32>,
}
