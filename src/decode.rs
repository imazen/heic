//! Internal decode pipeline: grid assembly, overlay compositing,
//! alpha plane extraction, metadata extraction, and gain map decoding.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use enough::{Stop, Unstoppable};

use crate::error::check_stop;
use crate::heif::{self, CleanAperture, ColorInfo, FourCC, ItemType, Transform};
use crate::{
    DecodeOutput, DecoderConfig, HdrGainMap, HeicError, Limits, PixelLayout, Result, floor_f64,
    round_f64,
};

#[cfg(feature = "parallel")]
use rayon::prelude::*;

/// Decode a collection of tile data in parallel or sequentially depending on
/// `max_threads` and the `parallel` feature.
///
/// When `parallel` is enabled and `max_threads != Some(1)`, uses rayon
/// parallelism scoped to the requested thread count. Otherwise falls back
/// to sequential iteration.
#[cfg(feature = "parallel")]
fn decode_tiles_parallel(
    tile_data_list: &[Cow<'_, [u8]>],
    tile_config: &crate::heif::HevcDecoderConfig,
    max_threads: Option<usize>,
) -> Result<Vec<crate::hevc::DecodedFrame>> {
    match max_threads {
        Some(1) => {
            // Forced single-threaded
            tile_data_list
                .iter()
                .map(|tile_data| {
                    crate::hevc::decode_with_config(tile_config, tile_data).map_err(Into::into)
                })
                .collect::<Result<_>>()
        }
        Some(n) if n > 1 => {
            // Limited thread pool
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n)
                .build()
                .map_err(|_| HeicError::InvalidData("failed to create thread pool"))?;
            pool.install(|| {
                tile_data_list
                    .par_iter()
                    .map(|tile_data| {
                        crate::hevc::decode_with_config(tile_config, tile_data).map_err(Into::into)
                    })
                    .collect::<Result<_>>()
            })
        }
        _ => {
            // Unlimited: use global rayon pool
            tile_data_list
                .par_iter()
                .map(|tile_data| {
                    crate::hevc::decode_with_config(tile_config, tile_data).map_err(Into::into)
                })
                .collect::<Result<_>>()
        }
    }
}

/// Sentinel for no limits
static NO_LIMITS: Limits = Limits {
    max_width: None,
    max_height: None,
    max_pixels: None,
    max_memory_bytes: None,
};

/// Core decode-to-frame implementation shared by all entry points.
pub(crate) fn decode_to_frame(
    data: &[u8],
    limits: Option<&Limits>,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    let limits = limits.unwrap_or(&NO_LIMITS);

    check_stop(stop)?;

    let container = heif::parse(data, stop)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    // Check limits on primary item dimensions if available from ispe
    if let Some((w, h)) = primary_item.dimensions {
        limits.check_dimensions(w, h)?;
        // Estimate memory before allocating frames
        let estimated = DecoderConfig::estimate_memory(w, h, PixelLayout::Rgba8);
        limits.check_memory(estimated)?;
    }

    check_stop(stop)?;

    let mut frame = decode_item(&container, &primary_item, 0, limits, stop, max_threads)?;

    check_stop(stop)?;

    // Try to decode alpha plane from auxiliary image.
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
    if let Some(alpha_id) = alpha_id
        && let Some(alpha_plane) = decode_alpha_plane(&container, alpha_id, &frame, limits)
    {
        frame.alpha_plane = Some(alpha_plane);
    }

    Ok(frame)
}

/// Decode an item, handling derived image types (iden, grid, iovl).
/// Applies the item's own transforms (clap, irot, imir) after decoding.
fn decode_item(
    container: &heif::HeifContainer<'_>,
    item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    if depth > 8 {
        return Err(HeicError::InvalidData("Derived image reference chain too deep").into());
    }

    check_stop(stop)?;

    let mut frame = match item.item_type {
        ItemType::Grid => decode_grid(container, item, limits, stop, max_threads)?,
        ItemType::Iden => decode_iden(container, item, depth, limits, stop, max_threads)?,
        ItemType::Iovl => decode_iovl(container, item, depth, limits, stop, max_threads)?,
        _ => {
            let image_data = container.get_item_data(item.id)?;

            if let Some(ref config) = item.hevc_config {
                crate::hevc::decode_with_config(config, &image_data)?
            } else {
                crate::hevc::decode(&image_data)?
            }
        }
    };

    // Set color conversion parameters from colr nclx box if present.
    if let Some(ColorInfo::Nclx {
        full_range,
        matrix_coefficients,
        color_primaries,
        transfer_characteristics,
    }) = &item.color_info
    {
        frame.full_range = *full_range;
        frame.matrix_coeffs = *matrix_coefficients as u8;
        frame.color_primaries = *color_primaries as u8;
        frame.transfer_characteristics = *transfer_characteristics as u8;
    }

    // Apply transformative properties in ipma listing order (HEIF spec requirement)
    for transform in &item.transforms {
        match transform {
            Transform::CleanAperture(clap) => {
                apply_clean_aperture(&mut frame, clap);
            }
            Transform::Mirror(mirror) => {
                frame = match mirror.axis {
                    0 => frame.mirror_vertical(),
                    1 => frame.mirror_horizontal(),
                    _ => frame,
                };
            }
            Transform::Rotation(rotation) => {
                frame = match rotation.angle {
                    90 => frame.rotate_90_cw(),
                    180 => frame.rotate_180(),
                    270 => frame.rotate_270_cw(),
                    _ => frame,
                };
            }
        }
    }

    Ok(frame)
}

/// Decode an identity-derived image by following dimg references.
fn decode_iden(
    container: &heif::HeifContainer<'_>,
    iden_item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    let source_ids = container.get_item_references(iden_item.id, FourCC::DIMG);
    let source_id = source_ids
        .first()
        .ok_or(HeicError::InvalidData("iden item has no dimg reference"))?;

    let source_item = container
        .get_item(*source_id)
        .ok_or(HeicError::InvalidData("iden dimg target item not found"))?;

    decode_item(
        container,
        &source_item,
        depth + 1,
        limits,
        stop,
        max_threads,
    )
}

/// Decode an image overlay (iovl) by compositing referenced tiles onto a canvas.
fn decode_iovl(
    container: &heif::HeifContainer<'_>,
    iovl_item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    let iovl_data = container.get_item_data(iovl_item.id)?;

    // Parse iovl descriptor:
    // - version (1 byte) + flags (3 bytes)
    // - canvas_fill_value: 2 bytes * num_channels (flags & 0x01 determines 32-bit offsets)
    if iovl_data.len() < 6 {
        return Err(HeicError::InvalidData("Overlay descriptor too short").into());
    }

    let flags = iovl_data[1];
    let large = (flags & 1) != 0;

    let tile_ids = container.get_item_references(iovl_item.id, FourCC::DIMG);
    if tile_ids.is_empty() {
        return Err(HeicError::InvalidData("Overlay has no tile references").into());
    }

    // Calculate expected layout
    let off_size = if large { 4usize } else { 2 };
    let per_tile = 2 * off_size;
    let fixed_end = 4 + 2 * off_size; // version/flags + width/height
    let tile_data_size = tile_ids.len() * per_tile;
    let fill_bytes = iovl_data
        .len()
        .checked_sub(fixed_end + tile_data_size)
        .ok_or(HeicError::InvalidData(
            "Overlay descriptor too short for tiles",
        ))?;

    // Parse canvas fill values (16-bit per channel)
    let num_fill_channels = fill_bytes / 2;
    let mut fill_values = [0u16; 4];
    for i in 0..num_fill_channels.min(4) {
        fill_values[i] = u16::from_be_bytes([iovl_data[4 + i * 2], iovl_data[4 + i * 2 + 1]]);
    }

    let mut pos = 4 + fill_bytes;

    // Read canvas dimensions
    let (canvas_width, canvas_height) = if large {
        if pos + 8 > iovl_data.len() {
            return Err(HeicError::InvalidData("Overlay descriptor truncated").into());
        }
        let w = u32::from_be_bytes([
            iovl_data[pos],
            iovl_data[pos + 1],
            iovl_data[pos + 2],
            iovl_data[pos + 3],
        ]);
        let h = u32::from_be_bytes([
            iovl_data[pos + 4],
            iovl_data[pos + 5],
            iovl_data[pos + 6],
            iovl_data[pos + 7],
        ]);
        pos += 8;
        (w, h)
    } else {
        if pos + 4 > iovl_data.len() {
            return Err(HeicError::InvalidData("Overlay descriptor truncated").into());
        }
        let w = u16::from_be_bytes([iovl_data[pos], iovl_data[pos + 1]]) as u32;
        let h = u16::from_be_bytes([iovl_data[pos + 2], iovl_data[pos + 3]]) as u32;
        pos += 4;
        (w, h)
    };

    // Check canvas dimensions against limits
    limits.check_dimensions(canvas_width, canvas_height)?;

    // Read per-tile offsets
    let mut offsets = Vec::with_capacity(tile_ids.len());
    for _ in 0..tile_ids.len() {
        let (x, y) = if large {
            if pos + 8 > iovl_data.len() {
                return Err(HeicError::InvalidData("Overlay offset data truncated").into());
            }
            let x = i32::from_be_bytes([
                iovl_data[pos],
                iovl_data[pos + 1],
                iovl_data[pos + 2],
                iovl_data[pos + 3],
            ]);
            let y = i32::from_be_bytes([
                iovl_data[pos + 4],
                iovl_data[pos + 5],
                iovl_data[pos + 6],
                iovl_data[pos + 7],
            ]);
            pos += 8;
            (x, y)
        } else {
            if pos + 4 > iovl_data.len() {
                return Err(HeicError::InvalidData("Overlay offset data truncated").into());
            }
            let x = i16::from_be_bytes([iovl_data[pos], iovl_data[pos + 1]]) as i32;
            let y = i16::from_be_bytes([iovl_data[pos + 2], iovl_data[pos + 3]]) as i32;
            pos += 4;
            (x, y)
        };
        offsets.push((x, y));
    }

    // Decode first tile to get format info
    let first_tile_item = container
        .get_item(tile_ids[0])
        .ok_or(HeicError::InvalidData("Missing overlay tile item"))?;
    let first_tile_config = first_tile_item
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing overlay tile hvcC"))?;

    let bit_depth = first_tile_config.bit_depth_luma_minus8 + 8;
    let chroma_format = first_tile_config.chroma_format;

    let mut output = crate::hevc::DecodedFrame::with_params(
        canvas_width,
        canvas_height,
        bit_depth,
        chroma_format,
    );

    // Apply canvas fill values (16-bit values scaled to bit depth)
    let fill_shift = 16u32.saturating_sub(bit_depth as u32);
    let y_fill = fill_values[0] >> fill_shift;
    let cb_fill = if num_fill_channels > 1 {
        fill_values[1] >> fill_shift
    } else {
        1u16 << (bit_depth - 1) // neutral chroma
    };
    let cr_fill = if num_fill_channels > 2 {
        fill_values[2] >> fill_shift
    } else {
        1u16 << (bit_depth - 1) // neutral chroma
    };
    output.y_plane.fill(y_fill);
    output.cb_plane.fill(cb_fill);
    output.cr_plane.fill(cr_fill);

    // Decode each tile and composite onto the canvas
    for (idx, &tile_id) in tile_ids.iter().enumerate() {
        check_stop(stop)?;

        let tile_item = container
            .get_item(tile_id)
            .ok_or(HeicError::InvalidData("Missing overlay tile"))?;

        let tile_frame = decode_item(container, &tile_item, depth + 1, limits, stop, max_threads)?;

        // Propagate color conversion settings from first tile
        if idx == 0 {
            output.full_range = tile_frame.full_range;
            output.matrix_coeffs = tile_frame.matrix_coeffs;
        }

        let (off_x, off_y) = offsets[idx];
        let dst_x = off_x.max(0) as u32;
        let dst_y = off_y.max(0) as u32;
        let tile_w = tile_frame.cropped_width();
        let tile_h = tile_frame.cropped_height();

        // Copy luma
        let copy_w = tile_w.min(canvas_width.saturating_sub(dst_x));
        let copy_h = tile_h.min(canvas_height.saturating_sub(dst_y));

        for row in 0..copy_h {
            let src_row = (tile_frame.crop_top + row) as usize;
            let dst_row = (dst_y + row) as usize;
            for col in 0..copy_w {
                let src_col = (tile_frame.crop_left + col) as usize;
                let dst_col = (dst_x + col) as usize;
                let src_idx = src_row * tile_frame.y_stride() + src_col;
                let dst_idx = dst_row * output.y_stride() + dst_col;
                if src_idx < tile_frame.y_plane.len() && dst_idx < output.y_plane.len() {
                    output.y_plane[dst_idx] = tile_frame.y_plane[src_idx];
                }
            }
        }

        // Copy chroma
        if chroma_format > 0 {
            let (sub_x, sub_y) = match chroma_format {
                1 => (2u32, 2u32),
                2 => (2, 1),
                3 => (1, 1),
                _ => (2, 2),
            };
            let c_copy_w = copy_w.div_ceil(sub_x);
            let c_copy_h = copy_h.div_ceil(sub_y);
            let c_dst_x = dst_x / sub_x;
            let c_dst_y = dst_y / sub_y;
            let c_src_x = tile_frame.crop_left / sub_x;
            let c_src_y = tile_frame.crop_top / sub_y;

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

/// Decode a grid-based HEIC image
fn decode_grid(
    container: &heif::HeifContainer<'_>,
    grid_item: &heif::Item,
    limits: &Limits,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    // Parse grid descriptor
    let grid_data = container.get_item_data(grid_item.id)?;

    if grid_data.len() < 8 {
        return Err(HeicError::InvalidData("Grid descriptor too short").into());
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(HeicError::InvalidData("Grid descriptor too short for 32-bit dims").into());
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

    // Check grid output dimensions against limits
    limits.check_dimensions(output_width, output_height)?;

    // Get tile item IDs from iref
    let tile_ids = container.get_item_references(grid_item.id, FourCC::DIMG);
    let expected_tiles = (rows * cols) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(HeicError::InvalidData("Grid tile count mismatch").into());
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
    let bit_depth = tile_config.bit_depth_luma_minus8 + 8;
    let chroma_format = tile_config.chroma_format;
    let mut output = crate::hevc::DecodedFrame::with_params(
        output_width,
        output_height,
        bit_depth,
        chroma_format,
    );

    // Streaming decode: decode tiles and blit immediately, dropping each tile
    // (or row of tiles) before decoding the next. This keeps peak memory at
    // output + 1 tile (sequential) or output + 1 row of tiles (parallel).
    check_stop(stop)?;
    let tile_data_list: Vec<Cow<'_, [u8]>> = tile_ids
        .iter()
        .map(|&tid| container.get_item_data(tid))
        .collect::<core::result::Result<_, _>>()?;

    #[cfg(feature = "parallel")]
    {
        // Parallel: decode tiles concurrently (respecting max_threads), then blit.
        let all_tiles = decode_tiles_parallel(&tile_data_list, tile_config, max_threads)?;

        for (tile_idx, tile_frame) in all_tiles.iter().enumerate() {
            if tile_idx == 0 {
                output.full_range = tile_frame.full_range;
                output.matrix_coeffs = tile_frame.matrix_coeffs;
            }
            blit_tile_to_grid(
                &mut output,
                tile_frame,
                tile_idx,
                cols,
                tile_width,
                tile_height,
                output_width,
                output_height,
                chroma_format,
            );
        }
    }

    #[cfg(not(feature = "parallel"))]
    {
        let _ = max_threads; // unused without parallel feature
        // Sequential: decode one tile, blit, drop — only 1 tile in memory at a time.
        for (tile_idx, tile_data) in tile_data_list.iter().enumerate() {
            check_stop(stop)?;
            let tile_frame = crate::hevc::decode_with_config(tile_config, tile_data)?;
            if tile_idx == 0 {
                output.full_range = tile_frame.full_range;
                output.matrix_coeffs = tile_frame.matrix_coeffs;
            }
            blit_tile_to_grid(
                &mut output,
                &tile_frame,
                tile_idx,
                cols,
                tile_width,
                tile_height,
                output_width,
                output_height,
                chroma_format,
            );
            // tile_frame dropped here
        }
    }

    Ok(output)
}

/// Copy a single decoded tile into the correct position in the output grid frame.
#[allow(clippy::too_many_arguments)]
fn blit_tile_to_grid(
    output: &mut crate::hevc::DecodedFrame,
    tile: &crate::hevc::DecodedFrame,
    tile_idx: usize,
    cols: u32,
    tile_width: u32,
    tile_height: u32,
    output_width: u32,
    output_height: u32,
    chroma_format: u8,
) {
    let tile_row = tile_idx as u32 / cols;
    let tile_col = tile_idx as u32 % cols;
    let dst_x = tile_col * tile_width;
    let dst_y = tile_row * tile_height;

    // Luma: copy visible portion (clamp to output dimensions)
    let copy_w = tile.cropped_width().min(output_width.saturating_sub(dst_x));
    let copy_h = tile
        .cropped_height()
        .min(output_height.saturating_sub(dst_y));

    let src_y_start = tile.crop_top;
    let src_x_start = tile.crop_left;

    for row in 0..copy_h {
        let src_row = (src_y_start + row) as usize;
        let dst_row = (dst_y + row) as usize;
        for col in 0..copy_w {
            let src_col = (src_x_start + col) as usize;
            let dst_col = (dst_x + col) as usize;

            let src_idx = src_row * tile.y_stride() + src_col;
            let dst_idx = dst_row * output.y_stride() + dst_col;
            output.y_plane[dst_idx] = tile.y_plane[src_idx];
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

        let src_c_stride = tile.c_stride();
        let dst_c_stride = output.c_stride();

        for row in 0..c_copy_h {
            let src_row = (c_src_y + row) as usize;
            let dst_row = (c_dst_y + row) as usize;
            for col in 0..c_copy_w {
                let src_col = (c_src_x + col) as usize;
                let dst_col = (c_dst_x + col) as usize;

                let src_idx = src_row * src_c_stride + src_col;
                let dst_idx = dst_row * dst_c_stride + dst_col;
                if src_idx < tile.cb_plane.len() && dst_idx < output.cb_plane.len() {
                    output.cb_plane[dst_idx] = tile.cb_plane[src_idx];
                    output.cr_plane[dst_idx] = tile.cr_plane[src_idx];
                }
            }
        }
    }
}

/// Try to decode a grid image directly into an RGB output buffer,
/// bypassing intermediate full-frame YCbCr assembly.
///
/// Returns `Ok(None)` if the image is not eligible for streaming
/// (not a grid, has transforms, has alpha). Returns `Ok(Some((w, h)))`
/// on success with the streaming path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_decode_grid_streaming(
    data: &[u8],
    limits: Option<&Limits>,
    stop: &dyn Stop,
    layout: PixelLayout,
    output: &mut [u8],
    max_threads: Option<usize>,
) -> Result<Option<(u32, u32)>> {
    let limits = limits.unwrap_or(&NO_LIMITS);

    check_stop(stop)?;

    let container = heif::parse(data, stop)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    // Eligibility: must be a grid with no transforms and no alpha
    if primary_item.item_type != ItemType::Grid {
        return Ok(None);
    }
    if !primary_item.transforms.is_empty() {
        return Ok(None);
    }

    // Check for alpha auxiliary image
    let has_alpha = !container
        .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
        .is_empty()
        || !container
            .find_auxiliary_items(
                primary_item.id,
                "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
            )
            .is_empty();
    if has_alpha {
        return Ok(None);
    }

    // Parse grid descriptor
    let grid_data = container.get_item_data(primary_item.id)?;

    if grid_data.len() < 8 {
        return Err(HeicError::InvalidData("Grid descriptor too short").into());
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(HeicError::InvalidData("Grid descriptor too short for 32-bit dims").into());
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

    limits.check_dimensions(output_width, output_height)?;

    // Check output buffer size
    let bpp = layout.bytes_per_pixel();
    let required = (output_width as usize)
        .checked_mul(output_height as usize)
        .and_then(|n| n.checked_mul(bpp))
        .ok_or(HeicError::LimitExceeded(
            "output buffer size overflows usize",
        ))?;
    if output.len() < required {
        return Err(HeicError::BufferTooSmall {
            required,
            actual: output.len(),
        }
        .into());
    }

    // Get tile info
    let tile_ids = container.get_item_references(primary_item.id, FourCC::DIMG);
    let expected_tiles = (rows * cols) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(HeicError::InvalidData("Grid tile count mismatch").into());
    }

    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or(HeicError::InvalidData("Missing tile item"))?;
    let tile_config = first_tile
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing tile hvcC config"))?;
    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or(HeicError::InvalidData("Missing tile dimensions"))?;

    // Determine color conversion overrides from grid item's colr nclx
    let color_override = match &primary_item.color_info {
        Some(ColorInfo::Nclx {
            full_range,
            matrix_coefficients,
            ..
        }) => Some((*full_range, *matrix_coefficients as u8)),
        _ => None,
    };

    // Collect tile data
    check_stop(stop)?;
    let tile_data_list: Vec<Cow<'_, [u8]>> = tile_ids
        .iter()
        .map(|&tid| container.get_item_data(tid))
        .collect::<core::result::Result<_, _>>()?;

    // Stream tiles: decode, color-convert directly to output, drop
    #[cfg(feature = "parallel")]
    {
        let cols_usize = cols as usize;
        for row in 0..rows {
            let row_start = row as usize * cols_usize;
            let row_end = row_start + cols_usize;
            let row_tiles = decode_tiles_parallel(
                &tile_data_list[row_start..row_end],
                tile_config,
                max_threads,
            )?;

            for (col, mut tile_frame) in row_tiles.into_iter().enumerate() {
                let tile_idx = row as usize * cols_usize + col;
                if let Some((fr, mc)) = color_override {
                    tile_frame.full_range = fr;
                    tile_frame.matrix_coeffs = mc;
                }
                let dst_x = col as u32 * tile_width;
                let dst_y = row * tile_height;
                let copy_w = tile_frame
                    .cropped_width()
                    .min(output_width.saturating_sub(dst_x));
                let copy_h = tile_frame
                    .cropped_height()
                    .min(output_height.saturating_sub(dst_y));
                convert_tile_to_output(
                    &tile_frame,
                    output,
                    layout,
                    dst_x,
                    dst_y,
                    copy_w,
                    copy_h,
                    output_width,
                );
                let _ = tile_idx; // suppress unused warning
            }
        }
    }

    #[cfg(not(feature = "parallel"))]
    {
        let _ = max_threads; // unused without parallel feature
        for (tile_idx, tile_data) in tile_data_list.iter().enumerate() {
            check_stop(stop)?;
            let mut tile_frame = crate::hevc::decode_with_config(tile_config, tile_data)?;
            if let Some((fr, mc)) = color_override {
                tile_frame.full_range = fr;
                tile_frame.matrix_coeffs = mc;
            }
            let tile_col = tile_idx as u32 % cols;
            let tile_row = tile_idx as u32 / cols;
            let dst_x = tile_col * tile_width;
            let dst_y = tile_row * tile_height;
            let copy_w = tile_frame
                .cropped_width()
                .min(output_width.saturating_sub(dst_x));
            let copy_h = tile_frame
                .cropped_height()
                .min(output_height.saturating_sub(dst_y));
            convert_tile_to_output(
                &tile_frame,
                output,
                layout,
                dst_x,
                dst_y,
                copy_w,
                copy_h,
                output_width,
            );
        }
    }

    Ok(Some((output_width, output_height)))
}

/// Try to decode a grid image with row-level streaming to a sink.
///
/// Calls [`RowSink::demand()`](crate::RowSink::demand) for each tile-row and
/// writes color-converted pixels directly. Returns `Ok(None)` if the image
/// is not eligible for streaming (not a grid, has transforms, has alpha).
#[allow(clippy::too_many_arguments)]
pub(crate) fn try_decode_grid_to_sink(
    data: &[u8],
    limits: Option<&Limits>,
    stop: &dyn Stop,
    layout: PixelLayout,
    sink: &mut dyn crate::RowSink,
    max_threads: Option<usize>,
) -> Result<Option<(u32, u32)>> {
    let limits = limits.unwrap_or(&NO_LIMITS);

    check_stop(stop)?;

    let container = heif::parse(data, stop)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    // Eligibility: must be a grid with no transforms and no alpha
    if primary_item.item_type != ItemType::Grid {
        return Ok(None);
    }
    if !primary_item.transforms.is_empty() {
        return Ok(None);
    }

    // Check for alpha auxiliary image
    let has_alpha = !container
        .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
        .is_empty()
        || !container
            .find_auxiliary_items(
                primary_item.id,
                "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
            )
            .is_empty();
    if has_alpha {
        return Ok(None);
    }

    // Parse grid descriptor
    let grid_data = container.get_item_data(primary_item.id)?;

    if grid_data.len() < 8 {
        return Err(HeicError::InvalidData("Grid descriptor too short").into());
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(HeicError::InvalidData("Grid descriptor too short for 32-bit dims").into());
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

    limits.check_dimensions(output_width, output_height)?;

    // Get tile info
    let tile_ids = container.get_item_references(primary_item.id, FourCC::DIMG);
    let expected_tiles = (rows * cols) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(HeicError::InvalidData("Grid tile count mismatch").into());
    }

    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or(HeicError::InvalidData("Missing tile item"))?;
    let tile_config = first_tile
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing tile hvcC config"))?;
    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or(HeicError::InvalidData("Missing tile dimensions"))?;

    // Determine color conversion overrides from grid item's colr nclx
    let color_override = match &primary_item.color_info {
        Some(ColorInfo::Nclx {
            full_range,
            matrix_coefficients,
            ..
        }) => Some((*full_range, *matrix_coefficients as u8)),
        _ => None,
    };

    // Collect tile data
    check_stop(stop)?;
    let tile_data_list: Vec<Cow<'_, [u8]>> = tile_ids
        .iter()
        .map(|&tid| container.get_item_data(tid))
        .collect::<core::result::Result<_, _>>()?;

    let bpp = layout.bytes_per_pixel();

    // Stream tile-rows: decode one row at a time, write to sink, drop
    for row in 0..rows {
        check_stop(stop)?;

        let row_start = row as usize * cols as usize;
        let row_end = row_start + cols as usize;

        // Calculate strip height (last row may be clipped)
        let strip_h = tile_height.min(output_height.saturating_sub(row * tile_height));
        if strip_h == 0 {
            break;
        }

        let y_offset = row * tile_height;
        let min_bytes = output_width as usize * strip_h as usize * bpp;
        let strip_buf = sink.demand(y_offset, strip_h, min_bytes);

        // Decode tiles for this row
        #[cfg(feature = "parallel")]
        let row_tiles: Vec<crate::hevc::DecodedFrame> = decode_tiles_parallel(
            &tile_data_list[row_start..row_end],
            tile_config,
            max_threads,
        )?;

        #[cfg(not(feature = "parallel"))]
        let row_tiles: Vec<crate::hevc::DecodedFrame> = {
            let _ = max_threads; // unused without parallel feature
            tile_data_list[row_start..row_end]
                .iter()
                .map(|tile_data| {
                    crate::hevc::decode_with_config(tile_config, tile_data).map_err(Into::into)
                })
                .collect::<Result<_>>()?
        };

        // Color-convert each tile directly into the strip buffer
        for (col, mut tile_frame) in row_tiles.into_iter().enumerate() {
            if let Some((fr, mc)) = color_override {
                tile_frame.full_range = fr;
                tile_frame.matrix_coeffs = mc;
            }
            let dst_x = col as u32 * tile_width;
            let copy_w = tile_frame
                .cropped_width()
                .min(output_width.saturating_sub(dst_x));
            let copy_h = tile_frame.cropped_height().min(strip_h);

            // Write into the strip buffer (y=0 within the strip)
            convert_tile_to_output(
                &tile_frame,
                strip_buf,
                layout,
                dst_x,
                0, // relative to strip, not to full image
                copy_w,
                copy_h,
                output_width,
            );
        }
    }

    Ok(Some((output_width, output_height)))
}

/// Color-convert a single decoded tile directly into the correct region
/// of the output RGB/RGBA/BGR/BGRA buffer.
#[allow(clippy::too_many_arguments)]
pub(crate) fn convert_tile_to_output(
    tile: &crate::hevc::DecodedFrame,
    output: &mut [u8],
    layout: PixelLayout,
    dst_x: u32,
    dst_y: u32,
    copy_w: u32,
    copy_h: u32,
    output_width: u32,
) {
    let bpp = layout.bytes_per_pixel();
    let shift = tile.bit_depth - 8;
    let src_x_start = tile.crop_left;
    let src_y_start = tile.crop_top;

    // Fast path: 4:2:0 + Rgb8 uses SIMD-accelerated conversion
    if tile.chroma_format == 1 && layout == PixelLayout::Rgb8 {
        let y_stride = tile.y_stride();
        let c_stride = tile.c_stride();

        for r in 0..copy_h {
            let src_row = src_y_start + r;
            let out_offset = ((dst_y + r) as usize * output_width as usize + dst_x as usize) * 3;
            let row_bytes = copy_w as usize * 3;
            crate::hevc::color_convert::convert_420_to_rgb(
                &tile.y_plane,
                &tile.cb_plane,
                &tile.cr_plane,
                y_stride,
                c_stride,
                src_row,
                src_row + 1,
                src_x_start,
                src_x_start + copy_w,
                shift as u32,
                tile.full_range,
                tile.matrix_coeffs,
                &mut output[out_offset..out_offset + row_bytes],
            );
        }
        return;
    }

    // Scalar fallback for other layouts and chroma formats
    let (cr_r, cb_g, cr_g, cb_b, y_bias, y_scale, rnd, shr) = if tile.full_range {
        let (cr_r, cb_g, cr_g, cb_b) = match tile.matrix_coeffs {
            1 => (403i32, -48, -120, 475), // BT.709
            9 => (377, -42, -146, 482),    // BT.2020
            _ => (359i32, -88, -183, 454), // BT.601
        };
        (cr_r, cb_g, cr_g, cb_b, 0i32, 256i32, 128i32, 8i32)
    } else {
        let (cr_r, cb_g, cr_g, cb_b) = match tile.matrix_coeffs {
            1 => (14744i32, -1754, -4383, 17373), // BT.709
            9 => (13806, -1541, -5349, 17615),    // BT.2020
            _ => (13126i32, -3222, -6686, 16591), // BT.601
        };
        (cr_r, cb_g, cr_g, cb_b, 16i32, 9576i32, 4096i32, 13i32)
    };

    let y_stride = tile.y_stride();
    let c_stride = tile.c_stride();

    for r in 0..copy_h {
        let src_y = src_y_start + r;
        let out_row_start = ((dst_y + r) as usize * output_width as usize + dst_x as usize) * bpp;

        for c in 0..copy_w {
            let src_x = src_x_start + c;
            let y_idx = src_y as usize * y_stride + src_x as usize;
            let y_val = (tile.y_plane[y_idx] >> shift) as i32;

            // Get chroma values based on chroma format
            let (cb_val, cr_val) = match tile.chroma_format {
                0 => (128i32, 128i32),
                1 => {
                    let c_idx = (src_y / 2) as usize * c_stride + (src_x / 2) as usize;
                    (
                        (tile.cb_plane[c_idx] >> shift) as i32,
                        (tile.cr_plane[c_idx] >> shift) as i32,
                    )
                }
                2 => {
                    let c_idx = src_y as usize * c_stride + (src_x / 2) as usize;
                    (
                        (tile.cb_plane[c_idx] >> shift) as i32,
                        (tile.cr_plane[c_idx] >> shift) as i32,
                    )
                }
                3 => {
                    let c_idx = src_y as usize * c_stride + src_x as usize;
                    (
                        (tile.cb_plane[c_idx] >> shift) as i32,
                        (tile.cr_plane[c_idx] >> shift) as i32,
                    )
                }
                _ => (128, 128),
            };

            let cb = cb_val - 128;
            let cr = cr_val - 128;
            let yv = (y_val - y_bias) * y_scale;
            let red = ((yv + cr_r * cr + rnd) >> shr).clamp(0, 255) as u8;
            let green = ((yv + cb_g * cb + cr_g * cr + rnd) >> shr).clamp(0, 255) as u8;
            let blue = ((yv + cb_b * cb + rnd) >> shr).clamp(0, 255) as u8;

            let out_offset = out_row_start + c as usize * bpp;
            match layout {
                PixelLayout::Rgb8 => {
                    output[out_offset] = red;
                    output[out_offset + 1] = green;
                    output[out_offset + 2] = blue;
                }
                PixelLayout::Rgba8 => {
                    output[out_offset] = red;
                    output[out_offset + 1] = green;
                    output[out_offset + 2] = blue;
                    output[out_offset + 3] = 255;
                }
                PixelLayout::Bgr8 => {
                    output[out_offset] = blue;
                    output[out_offset + 1] = green;
                    output[out_offset + 2] = red;
                }
                PixelLayout::Bgra8 => {
                    output[out_offset] = blue;
                    output[out_offset + 1] = green;
                    output[out_offset + 2] = red;
                    output[out_offset + 3] = 255;
                }
            }
        }
    }
}

/// Decode an auxiliary alpha plane and return it sized to match the primary frame.
///
/// Returns the alpha plane as a Vec<u16> with one value per cropped pixel,
/// or None if decoding fails.
fn decode_alpha_plane(
    container: &heif::HeifContainer<'_>,
    alpha_id: u32,
    primary_frame: &crate::hevc::DecodedFrame,
    limits: &Limits,
) -> Option<Vec<u16>> {
    let alpha_item = container.get_item(alpha_id)?;
    let alpha_data = container.get_item_data(alpha_id).ok()?;
    let alpha_config = alpha_item.hevc_config.as_ref()?;

    // Check limits on alpha image dimensions before decoding
    if let Some((w, h)) = alpha_item.dimensions {
        limits.check_dimensions(w, h).ok()?;
        let estimated = DecoderConfig::estimate_memory(w, h, PixelLayout::Rgba8);
        limits.check_memory(estimated).ok()?;
    }

    let alpha_frame = crate::hevc::decode_with_config(alpha_config, &alpha_data).ok()?;

    let primary_w = primary_frame.cropped_width();
    let primary_h = primary_frame.cropped_height();
    let alpha_w = alpha_frame.cropped_width();
    let alpha_h = alpha_frame.cropped_height();

    // Use u64 arithmetic to avoid u32 overflow
    let total_pixels = usize::try_from((primary_w as u64).checked_mul(primary_h as u64)?).ok()?;
    let mut alpha_plane = Vec::with_capacity(total_pixels);

    if alpha_w == primary_w && alpha_h == primary_h {
        // Same dimensions — direct copy of Y plane from cropped region
        let y_start = alpha_frame.crop_top;
        let x_start = alpha_frame.crop_left;
        for y in 0..primary_h {
            for x in 0..primary_w {
                let src_idx = ((y_start + y) * alpha_frame.width + (x_start + x)) as usize;
                alpha_plane.push(alpha_frame.y_plane[src_idx]);
            }
        }
    } else {
        // Different dimensions — bilinear resize
        for dy in 0..primary_h {
            for dx in 0..primary_w {
                let sx = (dx as f64) * (alpha_w as f64 - 1.0) / (primary_w as f64 - 1.0).max(1.0);
                let sy = (dy as f64) * (alpha_h as f64 - 1.0) / (primary_h as f64 - 1.0).max(1.0);

                let x0 = floor_f64(sx) as u32;
                let y0 = floor_f64(sy) as u32;
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

                alpha_plane.push(round_f64(val) as u16);
            }
        }
    }

    Some(alpha_plane)
}

/// Decode gain map from Apple HDR HEIC
pub(crate) fn decode_gain_map(data: &[u8]) -> Result<HdrGainMap> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    let gainmap_ids =
        container.find_auxiliary_items(primary_item.id, "urn:com:apple:photo:2020:aux:hdrgainmap");

    let &gainmap_id = gainmap_ids
        .first()
        .ok_or(HeicError::InvalidData("No HDR gain map found"))?;

    let gainmap_item = container
        .get_item(gainmap_id)
        .ok_or(HeicError::InvalidData("Missing gain map item"))?;

    // Use decode_item to handle grids, iden, and plain HEVC gain maps
    let frame = decode_item(
        &container,
        &gainmap_item,
        0,
        &Limits::default(),
        &Unstoppable,
        None,
    )?;

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

/// Apply clean aperture (clap box) crop to a decoded frame
fn apply_clean_aperture(frame: &mut crate::hevc::DecodedFrame, clap: &CleanAperture) {
    let conf_width = frame.cropped_width();
    let conf_height = frame.cropped_height();

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

    if clean_width >= conf_width && clean_height >= conf_height {
        return;
    }

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
        round_f64((conf_width as f64 - clean_width as f64) / 2.0 + horiz_off_pixels) as u32;
    let extra_top =
        round_f64((conf_height as f64 - clean_height as f64) / 2.0 + vert_off_pixels) as u32;
    let extra_right = conf_width
        .saturating_sub(clean_width)
        .saturating_sub(extra_left);
    let extra_bottom = conf_height
        .saturating_sub(clean_height)
        .saturating_sub(extra_top);

    frame.crop_left += extra_left;
    frame.crop_right += extra_right;
    frame.crop_top += extra_top;
    frame.crop_bottom += extra_bottom;
}

/// Extract EXIF TIFF data from HEIC container
pub(crate) fn extract_exif<'a>(data: &'a [u8]) -> Result<Option<Cow<'a, [u8]>>> {
    let container = heif::parse(data, &Unstoppable)?;

    // Find Exif item(s)
    for info in &container.item_infos {
        if info.item_type != FourCC(*b"Exif") {
            continue;
        }
        let Ok(exif_data) = container.get_item_data(info.item_id) else {
            continue;
        };
        // HEIF EXIF format: 4 bytes big-endian offset to TIFF header, then data.
        // The offset is from byte 4 (after the 4-byte offset field itself).
        // Typically 0, meaning TIFF data starts at byte 4.
        if exif_data.len() < 4 {
            continue;
        }
        let tiff_offset =
            u32::from_be_bytes([exif_data[0], exif_data[1], exif_data[2], exif_data[3]]) as usize;
        let tiff_start = 4 + tiff_offset;
        if tiff_start < exif_data.len() {
            return Ok(Some(match exif_data {
                Cow::Borrowed(b) => Cow::Borrowed(&b[tiff_start..]),
                Cow::Owned(v) => Cow::Owned(v[tiff_start..].to_vec()),
            }));
        }
    }

    Ok(None)
}

/// Decode thumbnail image from HEIC container
pub(crate) fn decode_thumbnail(data: &[u8], layout: PixelLayout) -> Result<Option<DecodeOutput>> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    let thumb_ids = container.find_thumbnails(primary_item.id);
    let Some(&thumb_id) = thumb_ids.first() else {
        return Ok(None);
    };

    let thumb_item = container
        .get_item(thumb_id)
        .ok_or(HeicError::InvalidData("Thumbnail item not found"))?;

    let stop: &dyn Stop = &Unstoppable;
    let frame = decode_item(&container, &thumb_item, 0, &NO_LIMITS, stop, None)?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();

    let pixels = match layout {
        PixelLayout::Rgb8 => frame.to_rgb(),
        PixelLayout::Rgba8 => frame.to_rgba(),
        PixelLayout::Bgr8 => frame.to_bgr(),
        PixelLayout::Bgra8 => frame.to_bgra(),
    };

    Ok(Some(DecodeOutput {
        data: pixels,
        width,
        height,
        layout,
    }))
}

/// Extract XMP XML data from HEIC container
pub(crate) fn extract_xmp<'a>(data: &'a [u8]) -> Result<Option<Cow<'a, [u8]>>> {
    let container = heif::parse(data, &Unstoppable)?;

    // Find mime items with XMP content type
    for info in &container.item_infos {
        if info.item_type == FourCC(*b"mime")
            && (info.content_type.contains("xmp")
                || info.content_type.contains("rdf+xml")
                || info.content_type == "application/rdf+xml")
            && let Ok(xmp_data) = container.get_item_data(info.item_id)
        {
            return Ok(Some(xmp_data));
        }
    }

    Ok(None)
}
