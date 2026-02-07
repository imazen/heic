//! HEIC grid image decoder
//!
//! Implements ISO/IEC 23008-12 grid-based image composition.
//! Smartphone cameras (Android/iPhone) commonly encode photos as grids
//! of HEVC tiles (e.g., 8160×6120 split into 192 tiles of 512×512).
//! This module decodes each tile and stitches them into a full image.

use crate::error::HeicError;
use crate::heif::{HeifContainer, ImageGrid, ItemType};
use crate::hevc::DecodedFrame;
use crate::hevc::{decode_with_config, decode};

/// Decode a grid image from a HEIF container
///
/// Parses the grid configuration, decodes each HEVC tile, and stitches
/// them into a single output frame at the grid's output dimensions.
pub fn decode_grid<'a>(
    container: &HeifContainer<'a>,
    grid_item_id: u32,
    grid_config: &ImageGrid,
) -> Result<DecodedFrame, HeicError> {
    // Get tile item IDs from iref 'dimg' reference
    let tile_ids = container
        .get_tile_item_ids(grid_item_id)
        .ok_or(HeicError::InvalidData("Grid has no dimg references in iref"))?;

    let expected_tiles = (grid_config.rows * grid_config.columns) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(HeicError::InvalidData(
            "Grid tile count mismatch with iref references",
        ));
    }

    // Decode the first tile to determine tile dimensions and bit depth
    let first_tile = decode_tile(container, tile_ids[0])?;
    let tile_width = first_tile.cropped_width();
    let tile_height = first_tile.cropped_height();
    let bit_depth = first_tile.bit_depth;
    let chroma_format = first_tile.chroma_format;

    // Create output frame at the grid's output dimensions
    let out_width = grid_config.output_width;
    let out_height = grid_config.output_height;
    let mut output = DecodedFrame::with_params(out_width, out_height, bit_depth, chroma_format);

    // Stitch first tile
    stitch_tile(&first_tile, &mut output, 0, 0, tile_width, tile_height);

    // Decode and stitch remaining tiles
    for (idx, &tile_id) in tile_ids.iter().enumerate().skip(1) {
        let row = idx as u32 / grid_config.columns;
        let col = idx as u32 % grid_config.columns;
        let dst_x = col * tile_width;
        let dst_y = row * tile_height;

        let tile = decode_tile(container, tile_id)?;
        stitch_tile(&tile, &mut output, dst_x, dst_y, tile_width, tile_height);
    }

    Ok(output)
}

/// Decode a single tile item
fn decode_tile(container: &HeifContainer<'_>, tile_id: u32) -> Result<DecodedFrame, HeicError> {
    let item = container
        .get_item(tile_id)
        .ok_or(HeicError::InvalidData("Tile item not found"))?;

    if item.item_type != ItemType::Hvc1 {
        return Err(HeicError::InvalidData("Tile is not HEVC coded"));
    }

    // Try single-extent first, fall back to multi-extent
    let frame = if let Some(image_data) = container.get_item_data(tile_id) {
        if let Some(ref config) = item.hevc_config {
            decode_with_config(config, image_data).map_err(HeicError::HevcDecode)?
        } else {
            decode(image_data).map_err(HeicError::HevcDecode)?
        }
    } else if let Some(image_data) = container.get_item_data_owned(tile_id) {
        if let Some(ref config) = item.hevc_config {
            decode_with_config(config, &image_data).map_err(HeicError::HevcDecode)?
        } else {
            decode(&image_data).map_err(HeicError::HevcDecode)?
        }
    } else {
        return Err(HeicError::InvalidData("Missing tile image data"));
    };

    Ok(frame)
}

/// Copy a decoded tile into the output frame at (dst_x, dst_y)
///
/// Handles edge tiles that may extend beyond the grid output dimensions
/// by only copying the portion that fits within output bounds.
fn stitch_tile(
    tile: &DecodedFrame,
    output: &mut DecodedFrame,
    dst_x: u32,
    dst_y: u32,
    _tile_width: u32,
    _tile_height: u32,
) {
    let out_width = output.width;
    let out_height = output.height;

    // Use cropped tile dimensions (conformance window)
    let src_x_start = tile.crop_left;
    let src_y_start = tile.crop_top;
    let src_width = tile.cropped_width();
    let src_height = tile.cropped_height();

    // Clamp to output bounds (edge tiles may extend past grid output size)
    let copy_width = src_width.min(out_width.saturating_sub(dst_x));
    let copy_height = src_height.min(out_height.saturating_sub(dst_y));

    if copy_width == 0 || copy_height == 0 {
        return;
    }

    // Copy luma plane
    let src_y_stride = tile.width as usize;
    let dst_y_stride = out_width as usize;

    for row in 0..copy_height {
        let src_row = (src_y_start + row) as usize;
        let dst_row = (dst_y + row) as usize;
        let src_start = src_row * src_y_stride + src_x_start as usize;
        let dst_start = dst_row * dst_y_stride + dst_x as usize;

        output.y_plane[dst_start..dst_start + copy_width as usize]
            .copy_from_slice(&tile.y_plane[src_start..src_start + copy_width as usize]);
    }

    // Copy chroma planes (4:2:0 subsampled)
    if tile.chroma_format >= 1 {
        let (c_sub_x, c_sub_y) = match tile.chroma_format {
            1 => (2u32, 2u32), // 4:2:0
            2 => (2, 1),       // 4:2:2
            3 => (1, 1),       // 4:4:4
            _ => (2, 2),
        };

        let src_c_stride = tile.c_stride();
        let dst_c_stride = output.c_stride();

        let c_src_x = src_x_start / c_sub_x;
        let c_src_y = src_y_start / c_sub_y;
        let c_dst_x = dst_x / c_sub_x;
        let c_dst_y = dst_y / c_sub_y;
        let c_copy_w = copy_width / c_sub_x;
        let c_copy_h = copy_height / c_sub_y;

        for row in 0..c_copy_h {
            let src_row = (c_src_y + row) as usize;
            let dst_row = (c_dst_y + row) as usize;
            let src_start = src_row * src_c_stride + c_src_x as usize;
            let dst_start = dst_row * dst_c_stride + c_dst_x as usize;

            if dst_start + c_copy_w as usize <= output.cb_plane.len()
                && src_start + c_copy_w as usize <= tile.cb_plane.len()
            {
                output.cb_plane[dst_start..dst_start + c_copy_w as usize]
                    .copy_from_slice(&tile.cb_plane[src_start..src_start + c_copy_w as usize]);
                output.cr_plane[dst_start..dst_start + c_copy_w as usize]
                    .copy_from_slice(&tile.cr_plane[src_start..src_start + c_copy_w as usize]);
            }
        }
    }
}
