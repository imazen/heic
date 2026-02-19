//! HEIF/ISOBMFF container parser
//!
//! This module parses the ISO Base Media File Format (ISOBMFF) container
//! used by HEIF/HEIC files. The container consists of nested "boxes" that
//! describe the file structure and contain image data.

mod boxes;
mod parser;

pub use boxes::{
    CleanAperture, FourCC, HevcDecoderConfig, ImageRotation, ImageSpatialExtents, ItemProperty,
};
pub use parser::{parse, HeifContainer, Item, ItemType};
