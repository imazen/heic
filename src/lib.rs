//! Pure Rust HEIC/HEIF image decoder
//!
//! This crate provides a safe, sandboxed HEIC image decoder without
//! any C/C++ dependencies.
//!
//! # Example
//!
//! ```ignore
//! use heic_decoder::HeicDecoder;
//!
//! let data = std::fs::read("image.heic")?;
//! let decoder = HeicDecoder::new();
//! let image = decoder.decode(&data)?;
//! println!("Decoded {}x{} image", image.width, image.height);
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Allow dead code during development - will be removed when decoder is complete
#![allow(dead_code)]

extern crate alloc;

mod error;
pub mod heif;
pub mod hevc;

pub use error::{HeicError, Result};

use alloc::vec::Vec;
use heif::{FourCC, ItemType};

/// Decoded image data
#[derive(Debug, Clone)]
pub struct DecodedImage {
    /// Raw pixel data in RGB or RGBA format
    pub data: Vec<u8>,
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Whether the image has an alpha channel
    pub has_alpha: bool,
}

/// Image metadata without full decode
#[derive(Debug, Clone, Copy)]
pub struct ImageInfo {
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Whether the image has an alpha channel
    pub has_alpha: bool,
}

/// HEIC image decoder
#[derive(Debug, Default)]
pub struct HeicDecoder {
    _private: (),
}

impl HeicDecoder {
    /// Create a new HEIC decoder
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
    }

    /// Decode HEIC data to raw pixels
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format
    /// or if decoding fails.
    pub fn decode(&self, data: &[u8]) -> Result<DecodedImage> {
        let frame = self.decode_to_frame(data)?;

        Ok(DecodedImage {
            data: frame.to_rgb(),
            width: frame.cropped_width(),
            height: frame.cropped_height(),
            has_alpha: false, // TODO: handle alpha plane
        })
    }

    /// Decode HEIC data to raw YCbCr frame (for debugging)
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format.
    pub fn decode_to_frame(&self, data: &[u8]) -> Result<hevc::DecodedFrame> {
        let container = heif::parse(data)?;
        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

        if primary_item.item_type == ItemType::Grid {
            return self.decode_grid(&container, &primary_item);
        }

        let image_data = container
            .get_item_data(primary_item.id)
            .ok_or(HeicError::InvalidData("Missing image data"))?;

        if let Some(ref config) = primary_item.hevc_config {
            Ok(hevc::decode_with_config(config, image_data)?)
        } else {
            Ok(hevc::decode(image_data)?)
        }
    }

    /// Decode a grid-based HEIC image
    fn decode_grid(
        &self,
        container: &heif::HeifContainer<'_>,
        grid_item: &heif::Item,
    ) -> Result<hevc::DecodedFrame> {
        // Parse grid descriptor
        let grid_data = container
            .get_item_data(grid_item.id)
            .ok_or(HeicError::InvalidData("Missing grid descriptor"))?;

        if grid_data.len() < 8 {
            return Err(HeicError::InvalidData("Grid descriptor too short"));
        }

        let flags = grid_data[1];
        let rows = grid_data[2] as u32 + 1;
        let cols = grid_data[3] as u32 + 1;
        let (output_width, output_height) = if (flags & 1) != 0 {
            if grid_data.len() < 12 {
                return Err(HeicError::InvalidData("Grid descriptor too short for 32-bit dims"));
            }
            (
                u32::from_be_bytes([grid_data[4], grid_data[5], grid_data[6], grid_data[7]]),
                u32::from_be_bytes([grid_data[8], grid_data[9], grid_data[10], grid_data[11]]),
            )
        } else {
            (
                u16::from_be_bytes([grid_data[4], grid_data[5]]) as u32,
                u16::from_be_bytes([grid_data[6], grid_data[7]]) as u32,
            )
        };

        // Get tile item IDs from iref
        let tile_ids = container.get_item_references(grid_item.id, FourCC::DIMG);
        let expected_tiles = (rows * cols) as usize;
        if tile_ids.len() != expected_tiles {
            return Err(HeicError::InvalidData("Grid tile count mismatch"));
        }

        // Get hvcC config from the first tile item
        let first_tile = container
            .get_item(tile_ids[0])
            .ok_or(HeicError::InvalidData("Missing tile item"))?;
        let tile_config = first_tile
            .hevc_config
            .as_ref()
            .ok_or(HeicError::InvalidData("Missing tile hvcC config"))?;

        // Get tile dimensions from ispe
        let (tile_width, tile_height) = first_tile
            .dimensions
            .ok_or(HeicError::InvalidData("Missing tile dimensions"))?;

        // Create output frame at the grid's output dimensions
        // Use the tile's bit_depth and chroma_format
        let bit_depth = tile_config.bit_depth_luma_minus8 + 8;
        let chroma_format = tile_config.chroma_format;
        let mut output = hevc::DecodedFrame::with_params(
            output_width,
            output_height,
            bit_depth,
            chroma_format,
        );

        // Decode each tile and copy into the output frame
        for (tile_idx, &tile_id) in tile_ids.iter().enumerate() {
            let tile_row = tile_idx as u32 / cols;
            let tile_col = tile_idx as u32 % cols;

            let tile_data = container
                .get_item_data(tile_id)
                .ok_or(HeicError::InvalidData("Missing tile data"))?;

            // Decode tile
            let tile_frame = hevc::decode_with_config(tile_config, tile_data)?;

            // Copy tile into output frame
            let dst_x = tile_col * tile_width;
            let dst_y = tile_row * tile_height;

            // Luma: copy visible portion (clamp to output dimensions)
            let copy_w = tile_frame.cropped_width().min(output_width.saturating_sub(dst_x));
            let copy_h = tile_frame.cropped_height().min(output_height.saturating_sub(dst_y));

            let src_y_start = tile_frame.crop_top;
            let src_x_start = tile_frame.crop_left;

            for row in 0..copy_h {
                let src_row = (src_y_start + row) as usize;
                let dst_row = (dst_y + row) as usize;
                for col in 0..copy_w {
                    let src_col = (src_x_start + col) as usize;
                    let dst_col = (dst_x + col) as usize;

                    let src_idx = src_row * tile_frame.y_stride() + src_col;
                    let dst_idx = dst_row * output.y_stride() + dst_col;
                    output.y_plane[dst_idx] = tile_frame.y_plane[src_idx];
                }
            }

            // Chroma: copy with subsampling
            if chroma_format > 0 {
                let (sub_x, sub_y) = match chroma_format {
                    1 => (2u32, 2u32), // 4:2:0
                    2 => (2, 1),       // 4:2:2
                    3 => (1, 1),       // 4:4:4
                    _ => (2, 2),
                };
                let c_copy_w = copy_w.div_ceil(sub_x);
                let c_copy_h = copy_h.div_ceil(sub_y);
                let c_dst_x = dst_x / sub_x;
                let c_dst_y = dst_y / sub_y;
                let c_src_x = src_x_start / sub_x;
                let c_src_y = src_y_start / sub_y;

                let src_c_stride = tile_frame.c_stride();
                let dst_c_stride = output.c_stride();

                for row in 0..c_copy_h {
                    let src_row = (c_src_y + row) as usize;
                    let dst_row = (c_dst_y + row) as usize;
                    for col in 0..c_copy_w {
                        let src_col = (c_src_x + col) as usize;
                        let dst_col = (c_dst_x + col) as usize;

                        let src_idx = src_row * src_c_stride + src_col;
                        let dst_idx = dst_row * dst_c_stride + dst_col;
                        if src_idx < tile_frame.cb_plane.len() && dst_idx < output.cb_plane.len() {
                            output.cb_plane[dst_idx] = tile_frame.cb_plane[src_idx];
                            output.cr_plane[dst_idx] = tile_frame.cr_plane[src_idx];
                        }
                    }
                }
            }
        }

        Ok(output)
    }

    /// Get image info without full decoding
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format.
    pub fn get_info(&self, data: &[u8]) -> Result<ImageInfo> {
        let container = heif::parse(data)?;

        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

        // Try to get info from HEVC config first (faster, no mdat access needed)
        if let Some(ref config) = primary_item.hevc_config
            && let Ok(info) = hevc::get_info_from_config(config)
        {
            return Ok(ImageInfo {
                width: info.width,
                height: info.height,
                has_alpha: false,
            });
        }

        // Fallback to reading image data
        let image_data = container
            .get_item_data(primary_item.id)
            .ok_or(HeicError::InvalidData("Missing image data"))?;

        let info = hevc::get_info(image_data)?;

        Ok(ImageInfo {
            width: info.width,
            height: info.height,
            has_alpha: false,
        })
    }
}
