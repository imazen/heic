//! HEIF/ISOBMFF container parser
//!
//! This module parses the ISO Base Media File Format (ISOBMFF) container
//! used by HEIF/HEIC files. The container consists of nested "boxes" that
//! describe the file structure and contain image data.

mod boxes;
mod parser;

pub use boxes::{
    AuxiliaryTypeProperty, Av1DecoderConfig, CleanAperture, ColorInfo, CompressionConfig,
    ContentLightLevelBox, FourCC, HevcDecoderConfig, ImageMirror, ImageRotation,
    ImageSpatialExtents, ItemProperty, MasteringDisplayBox, Transform, UncompressedComponent,
    UncompressedConfig,
};
pub use parser::{HeifContainer, Item, ItemType, parse};

// Crate-internal access for the structural inventory (`crate::inventory`).
#[cfg(feature = "zencodec")]
pub(crate) use boxes::{Box as BmffBox, BoxHeader, ItemInfo, ItemLocation};
#[cfg(feature = "zencodec")]
pub(crate) use parser::{
    TrackRole, iloc_entries, iloc_layout, moov_track_roles, parse_infe, parse_property,
};
