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

/// HDR gain map data extracted from an auxiliary image.
///
/// The gain map can be used with the Apple HDR formula to reconstruct HDR:
/// ```text
/// sdr_linear = sRGB_EOTF(sdr_pixel)
/// gainmap_linear = sRGB_EOTF(gainmap_pixel)
/// scale = 1.0 + (headroom - 1.0) * gainmap_linear
/// hdr_linear = sdr_linear * scale
/// ```
/// Where `headroom` comes from EXIF maker notes (tags 0x0021 and 0x0030).
#[derive(Debug, Clone)]
pub struct HdrGainMap {
    /// Gain map pixel data normalized to 0.0-1.0
    pub data: Vec<f32>,
    /// Gain map width in pixels
    pub width: u32,
    /// Gain map height in pixels
    pub height: u32,
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

    /// Decode HEIC data to raw pixels.
    ///
    /// Returns RGB data normally, or RGBA data when the image has an alpha plane.
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format
    /// or if decoding fails.
    pub fn decode(&self, data: &[u8]) -> Result<DecodedImage> {
        let frame = self.decode_to_frame(data)?;

        if frame.alpha_plane.is_some() {
            Ok(DecodedImage {
                data: frame.to_rgba(),
                width: frame.cropped_width(),
                height: frame.cropped_height(),
                has_alpha: true,
            })
        } else {
            Ok(DecodedImage {
                data: frame.to_rgb(),
                width: frame.cropped_width(),
                height: frame.cropped_height(),
                has_alpha: false,
            })
        }
    }

    /// Decode HEIC data to raw YCbCr frame (for debugging)
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format.
    pub fn decode_to_frame(&self, data: &[u8]) -> Result<hevc::DecodedFrame> {
        let container = heif::parse(data)?;
        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

        let mut frame = if primary_item.item_type == ItemType::Grid {
            self.decode_grid(&container, &primary_item)?
        } else {
            let image_data = container
                .get_item_data(primary_item.id)
                .ok_or(HeicError::InvalidData("Missing image data"))?;

            if let Some(ref config) = primary_item.hevc_config {
                hevc::decode_with_config(config, image_data)?
            } else {
                hevc::decode(image_data)?
            }
        };

        // Try to decode alpha plane from auxiliary image.
        // Two known alpha URIs:
        //   "urn:mpeg:hevc:2015:auxid:1" — HEVC alpha (older)
        //   "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha" — MPEG CICP alpha (newer)
        let alpha_id = container
            .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
            .first()
            .copied()
            .or_else(|| {
                container
                    .find_auxiliary_items(
                        primary_item.id,
                        "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
                    )
                    .first()
                    .copied()
            });
        if let Some(alpha_id) = alpha_id {
            if let Some(alpha_plane) = self.decode_alpha_plane(&container, alpha_id, &frame) {
                frame.alpha_plane = Some(alpha_plane);
            }
        }

        // Apply clean aperture crop (clap box) if present
        // The clap box specifies the true image dimensions within the
        // conformance-window-cropped frame
        if let Some(clap) = primary_item.clean_aperture {
            apply_clean_aperture(&mut frame, &clap);
        }

        // Apply image rotation (irot box) if present
        if let Some(rotation) = primary_item.rotation {
            frame = match rotation.angle {
                90 => frame.rotate_90_cw(),
                180 => frame.rotate_180(),
                270 => frame.rotate_270_cw(),
                _ => frame,
            };
        }

        Ok(frame)
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

    /// Decode an auxiliary alpha plane and return it sized to match the primary frame.
    ///
    /// Returns the alpha plane as a Vec<u16> with one value per cropped pixel,
    /// or None if decoding fails.
    fn decode_alpha_plane(
        &self,
        container: &heif::HeifContainer<'_>,
        alpha_id: u32,
        primary_frame: &hevc::DecodedFrame,
    ) -> Option<Vec<u16>> {
        let alpha_item = container.get_item(alpha_id)?;
        let alpha_data = container.get_item_data(alpha_id)?;
        let alpha_config = alpha_item.hevc_config.as_ref()?;

        let alpha_frame = hevc::decode_with_config(alpha_config, alpha_data).ok()?;

        let primary_w = primary_frame.cropped_width();
        let primary_h = primary_frame.cropped_height();
        let alpha_w = alpha_frame.cropped_width();
        let alpha_h = alpha_frame.cropped_height();

        let total_pixels = (primary_w * primary_h) as usize;
        let mut alpha_plane = Vec::with_capacity(total_pixels);

        if alpha_w == primary_w && alpha_h == primary_h {
            // Same dimensions — direct copy of Y plane from cropped region
            let y_start = alpha_frame.crop_top;
            let x_start = alpha_frame.crop_left;
            for y in 0..primary_h {
                for x in 0..primary_w {
                    let src_idx =
                        ((y_start + y) * alpha_frame.width + (x_start + x)) as usize;
                    alpha_plane.push(alpha_frame.y_plane[src_idx]);
                }
            }
        } else {
            // Different dimensions — bilinear resize
            for dy in 0..primary_h {
                for dx in 0..primary_w {
                    // Map destination pixel to source coordinates
                    let sx = (dx as f64) * (alpha_w as f64 - 1.0) / (primary_w as f64 - 1.0).max(1.0);
                    let sy = (dy as f64) * (alpha_h as f64 - 1.0) / (primary_h as f64 - 1.0).max(1.0);

                    let x0 = sx.floor() as u32;
                    let y0 = sy.floor() as u32;
                    let x1 = (x0 + 1).min(alpha_w - 1);
                    let y1 = (y0 + 1).min(alpha_h - 1);
                    let fx = sx - x0 as f64;
                    let fy = sy - y0 as f64;

                    let stride = alpha_frame.width;
                    let off_y = alpha_frame.crop_top;
                    let off_x = alpha_frame.crop_left;

                    let get = |px: u32, py: u32| -> f64 {
                        let idx = ((off_y + py) * stride + (off_x + px)) as usize;
                        alpha_frame.y_plane.get(idx).copied().unwrap_or(0) as f64
                    };

                    let v00 = get(x0, y0);
                    let v10 = get(x1, y0);
                    let v01 = get(x0, y1);
                    let v11 = get(x1, y1);

                    let val = v00 * (1.0 - fx) * (1.0 - fy)
                        + v10 * fx * (1.0 - fy)
                        + v01 * (1.0 - fx) * fy
                        + v11 * fx * fy;

                    alpha_plane.push(val.round() as u16);
                }
            }
        }

        Some(alpha_plane)
    }

    /// Get image info without full decoding
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format.
    pub fn get_info(&self, data: &[u8]) -> Result<ImageInfo> {
        let container = heif::parse(data)?;

        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

        // Check for alpha auxiliary image (two known URIs)
        let has_alpha = !container
            .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
            .is_empty()
            || !container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
                )
                .is_empty();

        // Try to get info from HEVC config first (faster, no mdat access needed)
        if let Some(ref config) = primary_item.hevc_config
            && let Ok(info) = hevc::get_info_from_config(config)
        {
            return Ok(ImageInfo {
                width: info.width,
                height: info.height,
                has_alpha,
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
            has_alpha,
        })
    }

    /// Decode the HDR gain map from an Apple HDR HEIC file.
    ///
    /// Returns the raw gain map pixel data normalized to 0.0-1.0.
    /// The gain map is typically lower resolution than the primary image.
    ///
    /// # Errors
    ///
    /// Returns an error if the file has no gain map or decoding fails.
    pub fn decode_gain_map(&self, data: &[u8]) -> Result<HdrGainMap> {
        let container = heif::parse(data)?;
        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

        let gainmap_ids = container.find_auxiliary_items(
            primary_item.id,
            "urn:com:apple:photo:2020:aux:hdrgainmap",
        );

        let &gainmap_id = gainmap_ids
            .first()
            .ok_or(HeicError::InvalidData("No HDR gain map found"))?;

        let gainmap_item = container
            .get_item(gainmap_id)
            .ok_or(HeicError::InvalidData("Missing gain map item"))?;
        let gainmap_data = container
            .get_item_data(gainmap_id)
            .ok_or(HeicError::InvalidData("Missing gain map data"))?;
        let gainmap_config = gainmap_item
            .hevc_config
            .as_ref()
            .ok_or(HeicError::InvalidData("Missing gain map hvcC config"))?;

        let frame = hevc::decode_with_config(gainmap_config, gainmap_data)?;

        let width = frame.cropped_width();
        let height = frame.cropped_height();
        let max_val = ((1u32 << frame.bit_depth) - 1) as f32;

        let mut float_data = Vec::with_capacity((width * height) as usize);
        let y_start = frame.crop_top;
        let x_start = frame.crop_left;

        for y in 0..height {
            for x in 0..width {
                let src_idx = ((y_start + y) * frame.width + (x_start + x)) as usize;
                let raw = frame.y_plane[src_idx] as f32;
                float_data.push(raw / max_val);
            }
        }

        Ok(HdrGainMap {
            data: float_data,
            width,
            height,
        })
    }
}

/// Apply clean aperture (clap box) crop to a decoded frame
///
/// The clap box specifies the clean aperture within the conformance-window-cropped
/// image. This adjusts the frame's crop values to include the additional clap crop.
///
/// Per ISO 14496-12:
///   crop_left = (coded_width - clean_width) / 2 + horiz_off
///   crop_top  = (coded_height - clean_height) / 2 + vert_off
fn apply_clean_aperture(frame: &mut hevc::DecodedFrame, clap: &heif::CleanAperture) {
    // Get current cropped dimensions (after conformance window)
    let conf_width = frame.cropped_width();
    let conf_height = frame.cropped_height();

    // Compute clean aperture dimensions (integer division for rational)
    let clean_width = if clap.width_d > 0 {
        clap.width_n / clap.width_d
    } else {
        conf_width
    };
    let clean_height = if clap.height_d > 0 {
        clap.height_n / clap.height_d
    } else {
        conf_height
    };

    // Only apply if clap actually further constrains the image
    if clean_width >= conf_width && clean_height >= conf_height {
        return;
    }

    // Compute offsets: how many pixels to crop from top-left
    // offset = (coded - clean) / 2 + rational_offset
    // We use integer arithmetic with rounding
    let horiz_off_pixels = if clap.horiz_off_d > 0 {
        (clap.horiz_off_n as f64) / (clap.horiz_off_d as f64)
    } else {
        0.0
    };
    let vert_off_pixels = if clap.vert_off_d > 0 {
        (clap.vert_off_n as f64) / (clap.vert_off_d as f64)
    } else {
        0.0
    };

    let extra_left =
        ((conf_width as f64 - clean_width as f64) / 2.0 + horiz_off_pixels).round() as u32;
    let extra_top =
        ((conf_height as f64 - clean_height as f64) / 2.0 + vert_off_pixels).round() as u32;
    let extra_right = conf_width.saturating_sub(clean_width).saturating_sub(extra_left);
    let extra_bottom = conf_height.saturating_sub(clean_height).saturating_sub(extra_top);

    // Add the clap crop on top of existing conformance window crop
    frame.crop_left += extra_left;
    frame.crop_right += extra_right;
    frame.crop_top += extra_top;
    frame.crop_bottom += extra_bottom;
}
