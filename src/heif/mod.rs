//! HEIF/ISOBMFF container parser
//!
//! This module parses the ISO Base Media File Format (ISOBMFF) container
//! used by HEIF/HEIC files. The container consists of nested "boxes" that
//! describe the file structure and contain image data.

mod boxes;
mod parser;

pub use boxes::{HevcDecoderConfig, ItemProperty};
pub use parser::{HeifContainer, ImageGrid, Item, ItemType, parse, parse_grid_config};
