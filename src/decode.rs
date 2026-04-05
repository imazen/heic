//! Internal decode pipeline: grid assembly, overlay compositing,
//! alpha plane extraction, metadata extraction, and gain map decoding.

use alloc::borrow::Cow;
use alloc::vec::Vec;

use enough::{Stop, Unstoppable};

use whereat::at;

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
                .map_err(|_| at!(HeicError::InvalidData("failed to create thread pool")))?;
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
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

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
        && let Some(alpha_plane) = decode_alpha_plane(&container, alpha_id, &frame, limits, stop)
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
        return Err(at!(HeicError::InvalidData(
            "Derived image reference chain too deep"
        )));
    }

    check_stop(stop)?;

    let mut frame = match item.item_type {
        ItemType::Grid => decode_grid(container, item, depth, limits, stop, max_threads)?,
        ItemType::Iden => decode_iden(container, item, depth, limits, stop, max_threads)?,
        ItemType::Iovl => decode_iovl(container, item, depth, limits, stop, max_threads)?,
        ItemType::Hvc1 | ItemType::Unknown(_) => {
            // HEVC path — Unknown falls through to HEVC for backwards compat
            let image_data = container.get_item_data(item.id)?;
            if let Some(ref config) = item.hevc_config {
                crate::hevc::decode_with_config(config, &image_data)?
            } else if item.item_type == ItemType::Hvc1 {
                crate::hevc::decode(&image_data)?
            } else {
                return Err(at!(HeicError::UnsupportedCodec(
                    "unknown item type with no decoder config"
                )));
            }
        }
        ItemType::Av01 => {
            #[cfg(feature = "av1")]
            {
                let image_data = container.get_item_data(item.id)?;
                decode_av1_item(item, &image_data, limits, stop)?
            }
            #[cfg(not(feature = "av1"))]
            {
                return Err(at!(HeicError::UnsupportedCodec(
                    "AV1 codec requires the 'av1' feature"
                )));
            }
        }
        ItemType::Unci => {
            #[cfg(feature = "unci")]
            {
                let image_data = container.get_item_data(item.id)?;
                decode_unci_item(item, &image_data, limits, stop)?
            }
            #[cfg(not(feature = "unci"))]
            {
                return Err(at!(HeicError::UnsupportedCodec(
                    "uncompressed HEIF requires the 'unci' feature"
                )));
            }
        }
        ItemType::Avc1 => {
            return Err(at!(HeicError::UnsupportedCodec(
                "H.264/AVC codec not supported"
            )));
        }
        ItemType::Jpeg => {
            return Err(at!(HeicError::UnsupportedCodec("JPEG codec not supported")));
        }
        ItemType::Exif | ItemType::Mime => {
            return Err(at!(HeicError::InvalidData(
                "metadata item type cannot be decoded as image"
            )));
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
        .ok_or_else(|| at!(HeicError::InvalidData("iden item has no dimg reference")))?;

    let source_item = container
        .get_item(*source_id)
        .ok_or_else(|| at!(HeicError::InvalidData("iden dimg target item not found")))?;

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
        return Err(at!(HeicError::InvalidData("Overlay descriptor too short")));
    }

    let flags = iovl_data[1];
    let large = (flags & 1) != 0;

    let tile_ids = container.get_item_references(iovl_item.id, FourCC::DIMG);
    if tile_ids.is_empty() {
        return Err(at!(HeicError::InvalidData(
            "Overlay has no tile references"
        )));
    }

    // Calculate expected layout
    let off_size = if large { 4usize } else { 2 };
    let per_tile = 2 * off_size;
    let fixed_end = 4 + 2 * off_size; // version/flags + width/height
    let tile_data_size = tile_ids.len() * per_tile;
    let fill_bytes = iovl_data
        .len()
        .checked_sub(fixed_end + tile_data_size)
        .ok_or_else(|| {
            at!(HeicError::InvalidData(
                "Overlay descriptor too short for tiles",
            ))
        })?;

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
            return Err(at!(HeicError::InvalidData("Overlay descriptor truncated")));
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
            return Err(at!(HeicError::InvalidData("Overlay descriptor truncated")));
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
                return Err(at!(HeicError::InvalidData("Overlay offset data truncated")));
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
                return Err(at!(HeicError::InvalidData("Overlay offset data truncated")));
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
        .ok_or_else(|| at!(HeicError::InvalidData("Missing overlay tile item")))?;

    let (bit_depth, chroma_format) = if let Some(ref config) = first_tile_item.hevc_config {
        (config.bit_depth_luma_minus8 + 8, config.chroma_format)
    } else if let Some(ref config) = first_tile_item.av1_config {
        (config.bit_depth(), config.chroma_format())
    } else {
        // Default to 8-bit 4:2:0 for unknown codecs
        (8u8, 1u8)
    };

    let mut output = crate::hevc::DecodedFrame::with_params(
        canvas_width,
        canvas_height,
        bit_depth,
        chroma_format,
    )?;

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
            .ok_or_else(|| at!(HeicError::InvalidData("Missing overlay tile")))?;

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

/// Decode an AV1-coded image item using rav1d-safe.
///
/// Prepends the av1C configOBUs to the image data, feeds the combined OBU
/// stream to the rav1d decoder, and converts the resulting frame to a
/// `DecodedFrame` with Y/Cb/Cr planes.
#[cfg(feature = "av1")]
fn decode_av1_item(
    item: &heif::Item,
    image_data: &[u8],
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<crate::hevc::DecodedFrame> {
    use rav1d_safe::src::managed::{Decoder, Planes, Settings};

    let config = item
        .av1_config
        .as_ref()
        .ok_or_else(|| at!(HeicError::InvalidData("AV1 item has no av1C config")))?;

    // Build combined OBU data: config_obus + image_data
    let total_len = config
        .config_obus
        .len()
        .checked_add(image_data.len())
        .ok_or_else(|| at!(HeicError::LimitExceeded("AV1 OBU data size overflow")))?;
    if total_len > 256 * 1024 * 1024 {
        return Err(at!(HeicError::LimitExceeded(
            "AV1 OBU data exceeds 256 MiB"
        )));
    }

    let mut obu_data = Vec::new();
    obu_data
        .try_reserve(total_len)
        .map_err(|_| at!(HeicError::OutOfMemory))?;
    obu_data.extend_from_slice(&config.config_obus);
    obu_data.extend_from_slice(image_data);

    check_stop(stop)?;

    // Create decoder with default settings (single-threaded for still images)
    let mut decoder = Decoder::with_settings(Settings::default()).map_err(|e| {
        at!(HeicError::InvalidData(match e {
            rav1d_safe::src::managed::Error::OutOfMemory => "AV1 decoder init: out of memory",
            _ => "AV1 decoder initialization failed",
        }))
    })?;

    // Feed the OBU data
    let frame_opt = decoder
        .decode(&obu_data)
        .map_err(|_| at!(HeicError::InvalidData("AV1 decode failed")))?;

    // Get the frame — it may come from decode() or flush()
    let frame = if let Some(f) = frame_opt {
        f
    } else {
        // Try flushing to get buffered frames
        let flushed = decoder
            .flush()
            .map_err(|_| at!(HeicError::InvalidData("AV1 flush failed")))?;
        flushed
            .into_iter()
            .next()
            .ok_or_else(|| at!(HeicError::InvalidData("AV1 decoder produced no frames")))?
    };

    let width = frame.width();
    let height = frame.height();
    let bit_depth = frame.bit_depth();

    // Check limits on decoded frame dimensions
    limits.check_dimensions(width, height)?;
    let estimated = DecoderConfig::estimate_memory(width, height, PixelLayout::Rgba8);
    limits.check_memory(estimated)?;

    check_stop(stop)?;

    // Map rav1d PixelLayout to our chroma_format
    let chroma_format = match frame.pixel_layout() {
        rav1d_safe::src::managed::PixelLayout::I400 => 0u8,
        rav1d_safe::src::managed::PixelLayout::I420 => 1,
        rav1d_safe::src::managed::PixelLayout::I422 => 2,
        rav1d_safe::src::managed::PixelLayout::I444 => 3,
    };

    let mut output =
        crate::hevc::DecodedFrame::with_params(width, height, bit_depth, chroma_format)?;

    // Copy planes from rav1d frame to our DecodedFrame
    match frame.planes() {
        Planes::Depth8(planes) => {
            // Copy Y plane
            let y_view = planes.y();
            for y in 0..height as usize {
                let src_row = y_view.row(y);
                let dst_start = y * output.y_stride();
                for (i, &val) in src_row.iter().take(width as usize).enumerate() {
                    output.y_plane[dst_start + i] = val as u16;
                }
            }

            // Copy Cb/Cr planes if not monochrome
            if chroma_format > 0
                && let (Some(cb_view), Some(cr_view)) = (planes.u(), planes.v())
            {
                let c_height = cb_view.height();
                let c_width = cb_view.width();
                for y in 0..c_height {
                    let cb_row = cb_view.row(y);
                    let cr_row = cr_view.row(y);
                    let dst_start = y * output.c_stride();
                    for (i, (&cb, &cr)) in cb_row
                        .iter()
                        .take(c_width)
                        .zip(cr_row.iter().take(c_width))
                        .enumerate()
                    {
                        output.cb_plane[dst_start + i] = cb as u16;
                        output.cr_plane[dst_start + i] = cr as u16;
                    }
                }
            }
        }
        Planes::Depth16(planes) => {
            // Copy Y plane (16-bit)
            let y_view = planes.y();
            for y in 0..height as usize {
                let src_row = y_view.row(y);
                let dst_start = y * output.y_stride();
                for (i, &val) in src_row.iter().take(width as usize).enumerate() {
                    output.y_plane[dst_start + i] = val;
                }
            }

            // Copy Cb/Cr planes if not monochrome
            if chroma_format > 0
                && let (Some(cb_view), Some(cr_view)) = (planes.u(), planes.v())
            {
                let c_height = cb_view.height();
                let c_width = cb_view.width();
                for y in 0..c_height {
                    let cb_row = cb_view.row(y);
                    let cr_row = cr_view.row(y);
                    let dst_start = y * output.c_stride();
                    for (i, (&cb, &cr)) in cb_row
                        .iter()
                        .take(c_width)
                        .zip(cr_row.iter().take(c_width))
                        .enumerate()
                    {
                        output.cb_plane[dst_start + i] = cb;
                        output.cr_plane[dst_start + i] = cr;
                    }
                }
            }
        }
    }

    // Set color info from the AV1 frame
    let color_info = frame.color_info();
    output.full_range = color_info.color_range == rav1d_safe::src::managed::ColorRange::Full;
    output.matrix_coeffs = color_info.matrix_coefficients as u8;
    output.color_primaries = color_info.primaries as u8;
    output.transfer_characteristics = color_info.transfer_characteristics as u8;

    Ok(output)
}

/// Decode an uncompressed HEIF (unci) image item.
///
/// Handles both compressed (deflate/zlib via zenflate) and raw uncompressed
/// pixel data as defined in ISO 23001-17. Supports pixel-interleaved and
/// component-planar layouts for 8-bit unsigned integer components.
#[cfg(feature = "unci")]
fn decode_unci_item(
    item: &heif::Item,
    image_data: &[u8],
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<crate::hevc::DecodedFrame> {
    let unc_config = item
        .uncompressed_config
        .as_ref()
        .ok_or_else(|| at!(HeicError::InvalidData("unci item has no uncC config")))?;

    let (width, height) = item
        .dimensions
        .ok_or_else(|| at!(HeicError::InvalidData("unci item has no dimensions")))?;

    if width == 0 || height == 0 {
        return Err(at!(HeicError::InvalidData("unci item has zero dimensions")));
    }

    // Check limits on unci dimensions before allocating
    limits.check_dimensions(width, height)?;
    let estimated = DecoderConfig::estimate_memory(width, height, PixelLayout::Rgba8);
    limits.check_memory(estimated)?;

    let num_components = unc_config.components.len();
    if num_components == 0 {
        return Err(at!(HeicError::InvalidData("unci item has no components")));
    }

    // Calculate expected decompressed size with overflow checks
    let bits_per_pixel: u32 = unc_config
        .components
        .iter()
        .try_fold(0u32, |acc, c| {
            acc.checked_add(c.component_bit_depth_minus_one as u32 + 1)
        })
        .ok_or_else(|| at!(HeicError::InvalidData("unci bit depth overflow")))?;

    // For now, only support 8-bit unsigned integer components
    let all_8bit = unc_config
        .components
        .iter()
        .all(|c| c.component_bit_depth_minus_one == 7 && c.component_format == 0);
    if !all_8bit {
        return Err(at!(HeicError::Unsupported(
            "unci: only 8-bit unsigned integer components supported"
        )));
    }

    let bytes_per_pixel = bits_per_pixel.div_ceil(8);
    let expected_size = (width as u64)
        .checked_mul(height as u64)
        .and_then(|n| n.checked_mul(bytes_per_pixel as u64))
        .ok_or_else(|| at!(HeicError::LimitExceeded("unci decompressed size overflow")))?;

    // Security: limit decompressed size to min(512 MiB, limits.max_memory_bytes)
    let decompress_cap = limits
        .max_memory_bytes
        .map_or(512 * 1024 * 1024, |m| m.min(512 * 1024 * 1024));
    if expected_size > decompress_cap {
        return Err(at!(HeicError::LimitExceeded(
            "unci decompressed size exceeds limit"
        )));
    }
    let expected_size = expected_size as usize;

    check_stop(stop)?;

    // Decompress if compression config is present
    let pixel_data: alloc::borrow::Cow<'_, [u8]> =
        if let Some(ref cmp_config) = item.compression_config {
            let mut decompressed = Vec::new();
            decompressed
                .try_reserve(expected_size)
                .map_err(|_| at!(HeicError::OutOfMemory))?;
            decompressed.resize(expected_size, 0);

            let mut decompressor = zenflate::Decompressor::new();
            let result = match &cmp_config.compression_type.0 {
                b"defl" => decompressor
                    .deflate_decompress(image_data, &mut decompressed, stop)
                    .map_err(|_| at!(HeicError::InvalidData("unci deflate decompression failed"))),
                b"zlib" => decompressor
                    .zlib_decompress(image_data, &mut decompressed, stop)
                    .map_err(|_| at!(HeicError::InvalidData("unci zlib decompression failed"))),
                _ => {
                    return Err(at!(HeicError::UnsupportedCodec(
                        "unci compression type not supported (only deflate and zlib)"
                    )));
                }
            }?;

            decompressed.truncate(result.output_written);
            alloc::borrow::Cow::Owned(decompressed)
        } else {
            // No compression — use raw data
            alloc::borrow::Cow::Borrowed(image_data)
        };

    if pixel_data.len() < expected_size {
        return Err(at!(HeicError::InvalidData(
            "unci decompressed data smaller than expected"
        )));
    }

    // Create output frame — use RGB (chroma_format=3 = 4:4:4) for unci
    let mut output = crate::hevc::DecodedFrame::with_params(width, height, 8, 3)?;

    // Set full-range since unci pixels are typically full-range
    output.full_range = true;

    // Determine component layout → map component indices to R, G, B channels
    // ISO 23001-17 component_index: 0=Y/R, 1=Cb/G, 2=Cr/B, 3=A, 4=R, 5=G, 6=B
    let interleave = unc_config.interleave_type;

    match interleave {
        0 => {
            // Component-planar: each component stored as a complete plane
            let plane_size = (width as usize) * (height as usize);
            for (comp_idx, comp) in unc_config.components.iter().enumerate() {
                check_stop(stop)?;
                let plane_offset = comp_idx * plane_size;
                if plane_offset + plane_size > pixel_data.len() {
                    return Err(at!(HeicError::InvalidData(
                        "unci component plane extends past data"
                    )));
                }
                let plane_data = &pixel_data[plane_offset..plane_offset + plane_size];

                // Map component_index to Y/Cb/Cr plane
                let target = match comp.component_index {
                    0 | 4 => Some(&mut output.y_plane),  // Y or R
                    1 | 5 => Some(&mut output.cb_plane), // Cb or G
                    2 | 6 => Some(&mut output.cr_plane), // Cr or B
                    _ => None,                           // alpha or unknown — skip
                };

                if let Some(target_plane) = target {
                    for (i, &val) in plane_data.iter().enumerate() {
                        if i < target_plane.len() {
                            target_plane[i] = val as u16;
                        }
                    }
                }
            }
        }
        1 => {
            // Pixel-interleaved: R,G,B,R,G,B,...
            let stride = num_components;
            let mut comp_to_plane: [Option<u8>; 8] = [None; 8];
            for (i, comp) in unc_config.components.iter().enumerate() {
                if i < 8 {
                    comp_to_plane[i] = match comp.component_index {
                        0 | 4 => Some(0), // Y/R
                        1 | 5 => Some(1), // Cb/G
                        2 | 6 => Some(2), // Cr/B
                        _ => None,
                    };
                }
            }

            for y in 0..height as usize {
                check_stop(stop)?;
                for x in 0..width as usize {
                    let pixel_offset = (y * width as usize + x) * stride;
                    if pixel_offset + stride > pixel_data.len() {
                        return Err(at!(HeicError::InvalidData("unci pixel data truncated")));
                    }
                    let dst_idx = y * output.y_stride() + x;
                    for (c, &mapping) in comp_to_plane.iter().enumerate().take(num_components) {
                        if let Some(plane_id) = mapping {
                            let val = pixel_data[pixel_offset + c] as u16;
                            match plane_id {
                                0 => {
                                    if dst_idx < output.y_plane.len() {
                                        output.y_plane[dst_idx] = val;
                                    }
                                }
                                1 => {
                                    if dst_idx < output.cb_plane.len() {
                                        output.cb_plane[dst_idx] = val;
                                    }
                                }
                                2 => {
                                    if dst_idx < output.cr_plane.len() {
                                        output.cr_plane[dst_idx] = val;
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        _ => {
            return Err(at!(HeicError::Unsupported(
                "unci interleave type not supported (only component-planar and pixel-interleaved)"
            )));
        }
    }

    // For unci RGB data, set identity matrix (no YCbCr conversion needed)
    // The output planes contain R, G, B directly when component indices are 4, 5, 6
    // or Y, Cb, Cr when indices are 0, 1, 2
    let has_rgb_indices = unc_config
        .components
        .iter()
        .any(|c| c.component_index >= 4 && c.component_index <= 6);

    if has_rgb_indices {
        // Direct RGB in Y/Cb/Cr planes — use identity matrix (0)
        output.matrix_coeffs = 0;
    }

    Ok(output)
}

/// Decode a grid-based HEIC image
fn decode_grid(
    container: &heif::HeifContainer<'_>,
    grid_item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
    max_threads: Option<usize>,
) -> Result<crate::hevc::DecodedFrame> {
    // Parse grid descriptor
    let grid_data = container.get_item_data(grid_item.id)?;

    if grid_data.len() < 8 {
        return Err(at!(HeicError::InvalidData("Grid descriptor too short")));
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(at!(HeicError::InvalidData(
                "Grid descriptor too short for 32-bit dims"
            )));
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
        return Err(at!(HeicError::InvalidData("Grid tile count mismatch")));
    }

    // Get config from the first tile item — supports HEVC, AV1, and unci tiles
    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile item")))?;

    // Determine bit depth and chroma format from the tile's codec config
    let (bit_depth, chroma_format) = if let Some(ref config) = first_tile.hevc_config {
        (config.bit_depth_luma_minus8 + 8, config.chroma_format)
    } else if let Some(ref config) = first_tile.av1_config {
        (config.bit_depth(), config.chroma_format())
    } else if first_tile.uncompressed_config.is_some() {
        // unci tiles: assume 8-bit RGB (chroma_format 3 = 4:4:4)
        let bd = first_tile
            .uncompressed_config
            .as_ref()
            .and_then(|c| c.components.first())
            .map(|c| c.component_bit_depth_minus_one + 1)
            .unwrap_or(8);
        (bd, 3)
    } else {
        return Err(at!(HeicError::InvalidData(
            "Missing tile decoder config (no hvcC, av1C, or uncC)"
        )));
    };

    // Get tile dimensions from ispe
    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile dimensions")))?;
    let mut output = crate::hevc::DecodedFrame::with_params(
        output_width,
        output_height,
        bit_depth,
        chroma_format,
    )?;

    // Streaming decode: decode tiles and blit immediately, dropping each tile
    // (or row of tiles) before decoding the next. This keeps peak memory at
    // output + 1 tile (sequential) or output + 1 row of tiles (parallel).
    check_stop(stop)?;
    let tile_data_list: Vec<Cow<'_, [u8]>> = tile_ids
        .iter()
        .map(|&tid| container.get_item_data(tid))
        .collect::<core::result::Result<_, _>>()?;

    // For HEVC grids, use the parallel decode path when available
    let hevc_tile_config = first_tile.hevc_config.as_ref();

    #[cfg(feature = "parallel")]
    {
        if let Some(tile_config) = hevc_tile_config {
            // Parallel HEVC: decode tiles concurrently (respecting max_threads), then blit.
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
        } else {
            // Non-HEVC tiles: sequential decode via decode_item per tile
            for (tile_idx, &tile_id) in tile_ids.iter().enumerate() {
                check_stop(stop)?;
                let tile_item = container
                    .get_item(tile_id)
                    .ok_or_else(|| at!(HeicError::InvalidData("Missing grid tile")))?;
                let tile_frame =
                    decode_item(container, &tile_item, depth + 1, limits, stop, max_threads)?;
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
            }
        }
    }

    #[cfg(not(feature = "parallel"))]
    {
        let _ = max_threads; // unused without parallel feature
        // Sequential: decode one tile, blit, drop — only 1 tile in memory at a time.
        for (tile_idx, tile_data) in tile_data_list.iter().enumerate() {
            check_stop(stop)?;
            let tile_frame = if let Some(tile_config) = hevc_tile_config {
                crate::hevc::decode_with_config(tile_config, tile_data)?
            } else {
                // Non-HEVC tiles: look up the tile item and dispatch
                let tile_id = tile_ids[tile_idx];
                let tile_item = container
                    .get_item(tile_id)
                    .ok_or_else(|| at!(HeicError::InvalidData("Missing grid tile")))?;
                decode_item(container, &tile_item, depth + 1, limits, stop, None)?
            };
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
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

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
        return Err(at!(HeicError::InvalidData("Grid descriptor too short")));
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(at!(HeicError::InvalidData(
                "Grid descriptor too short for 32-bit dims"
            )));
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
        .ok_or_else(|| {
            at!(HeicError::LimitExceeded(
                "output buffer size overflows usize",
            ))
        })?;
    if output.len() < required {
        return Err(at!(HeicError::BufferTooSmall {
            required,
            actual: output.len(),
        }));
    }

    // Get tile info
    let tile_ids = container.get_item_references(primary_item.id, FourCC::DIMG);
    let expected_tiles = (rows * cols) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(at!(HeicError::InvalidData("Grid tile count mismatch")));
    }

    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile item")))?;

    // Streaming grid path only supports HEVC tiles
    let tile_config = match first_tile.hevc_config.as_ref() {
        Some(config) => config,
        None => return Ok(None),
    };

    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile dimensions")))?;

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
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

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
        return Err(at!(HeicError::InvalidData("Grid descriptor too short")));
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(at!(HeicError::InvalidData(
                "Grid descriptor too short for 32-bit dims"
            )));
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
        return Err(at!(HeicError::InvalidData("Grid tile count mismatch")));
    }

    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile item")))?;

    // Streaming grid path only supports HEVC tiles
    let tile_config = match first_tile.hevc_config.as_ref() {
        Some(config) => config,
        None => return Ok(None),
    };

    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or_else(|| at!(HeicError::InvalidData("Missing tile dimensions")))?;

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
    stop: &dyn Stop,
) -> Option<Vec<u16>> {
    let alpha_item = container.get_item(alpha_id)?;
    let alpha_data = container.get_item_data(alpha_id).ok()?;

    // Check limits on alpha image dimensions before decoding
    if let Some((w, h)) = alpha_item.dimensions {
        limits.check_dimensions(w, h).ok()?;
        let estimated = DecoderConfig::estimate_memory(w, h, PixelLayout::Rgba8);
        limits.check_memory(estimated).ok()?;
    }

    check_stop(stop).ok()?;

    // Multi-codec dispatch: try HEVC first, then AV1
    let alpha_frame = if let Some(ref config) = alpha_item.hevc_config {
        crate::hevc::decode_with_config(config, &alpha_data).ok()?
    } else {
        #[cfg(feature = "av1")]
        {
            if alpha_item.av1_config.is_some() {
                decode_av1_item(&alpha_item, &alpha_data, limits, stop).ok()?
            } else {
                return None;
            }
        }
        #[cfg(not(feature = "av1"))]
        {
            return None;
        }
    };

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

/// Decode gain map from Apple HDR HEIC.
///
/// Returns the grayscale gain map pixels scaled to 8-bit, along with
/// the source bit depth and any XMP metadata associated with the gain map item.
pub(crate) fn decode_gain_map(data: &[u8]) -> Result<HdrGainMap> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let gainmap_ids =
        container.find_auxiliary_items(primary_item.id, "urn:com:apple:photo:2020:aux:hdrgainmap");

    let &gainmap_id = gainmap_ids
        .first()
        .ok_or_else(|| at!(HeicError::InvalidData("No HDR gain map found")))?;

    let gainmap_item = container
        .get_item(gainmap_id)
        .ok_or_else(|| at!(HeicError::InvalidData("Missing gain map item")))?;

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
    let bit_depth = frame.bit_depth;

    // Extract Y plane as grayscale, scale to 8-bit if needed
    let total_pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| at!(HeicError::LimitExceeded("gain map dimensions overflow")))?;

    let max_val = ((1u32 << bit_depth) - 1) as u32;
    let y_start = frame.crop_top;
    let x_start = frame.crop_left;

    let mut grayscale = Vec::new();
    grayscale
        .try_reserve(total_pixels)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    for y in 0..height {
        for x in 0..width {
            let src_idx = ((y_start + y) * frame.width + (x_start + x)) as usize;
            let raw = frame.y_plane[src_idx] as u32;
            let val = if bit_depth == 8 {
                raw as u8
            } else {
                ((raw * 255 + max_val / 2) / max_val) as u8
            };
            grayscale.push(val);
        }
    }

    // Extract XMP metadata associated with the gain map item (if any)
    let xmp = container
        .find_xmp_for_item(gainmap_id)
        .map(|cow| cow.into_owned());

    Ok(HdrGainMap {
        data: grayscale,
        width,
        height,
        bit_depth,
        xmp,
    })
}

/// Check if the primary image has an HDR gain map auxiliary image.
pub(crate) fn has_gain_map(data: &[u8]) -> Result<bool> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let gainmap_ids =
        container.find_auxiliary_items(primary_item.id, "urn:com:apple:photo:2020:aux:hdrgainmap");
    Ok(!gainmap_ids.is_empty())
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
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let thumb_ids = container.find_thumbnails(primary_item.id);
    let Some(&thumb_id) = thumb_ids.first() else {
        return Ok(None);
    };

    let thumb_item = container
        .get_item(thumb_id)
        .ok_or_else(|| at!(HeicError::InvalidData("Thumbnail item not found")))?;

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

/// List all auxiliary images linked to the primary item.
pub(crate) fn list_auxiliary_images(data: &[u8]) -> Result<Vec<crate::AuxiliaryImageDescriptor>> {
    use crate::auxiliary::AuxiliaryImageType;

    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let aux_items = container.find_all_auxiliary_items(primary_item.id);

    let mut result = Vec::new();
    for (item_id, urn) in aux_items {
        let aux_type = AuxiliaryImageType::from_urn(&urn);
        let item = container.get_item(item_id);
        let dimensions = item.and_then(|it| it.dimensions);
        result.push(crate::AuxiliaryImageDescriptor {
            aux_type,
            item_id,
            dimensions,
        });
    }

    Ok(result)
}

/// Check if the primary image has a depth auxiliary image.
pub(crate) fn has_depth(data: &[u8]) -> Result<bool> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let depth_ids = container.find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:2");
    if !depth_ids.is_empty() {
        return Ok(true);
    }
    let depth_ids = container.find_auxiliary_items(
        primary_item.id,
        "urn:mpeg:mpegB:cicp:systems:auxiliary:depth",
    );
    Ok(!depth_ids.is_empty())
}

/// Decode the depth map auxiliary image.
pub(crate) fn decode_depth(data: &[u8]) -> Result<crate::DepthMap> {
    use crate::auxiliary::{AuxiliaryImageType, parse_depth_representation_info};

    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    // Find depth auxiliary item (try MPEG URN first, then CICP URN)
    let depth_id = container
        .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:2")
        .first()
        .copied()
        .or_else(|| {
            container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:depth",
                )
                .first()
                .copied()
        })
        .ok_or_else(|| at!(HeicError::InvalidData("no depth auxiliary image found")))?;

    let depth_item = container
        .get_item(depth_id)
        .ok_or_else(|| at!(HeicError::InvalidData("depth item not found")))?;

    // Parse depth representation info from auxC subtype data
    let depth_info = depth_item
        .auxiliary_type_property
        .as_ref()
        .map(|atp| {
            let _ = AuxiliaryImageType::from_urn(&atp.aux_type); // validates it's depth
            parse_depth_representation_info(&atp.subtype_data)
        })
        .unwrap_or_default();

    // Decode the depth image using the same item decode pipeline
    let frame = decode_item(
        &container,
        &depth_item,
        0,
        &Limits::default(),
        &Unstoppable,
        None,
    )?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();
    let bit_depth = frame.bit_depth;

    // Extract the Y (luma) plane as the grayscale depth data
    let total_pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| at!(HeicError::LimitExceeded("depth map dimensions overflow")))?;

    let mut depth_data = Vec::new();
    depth_data
        .try_reserve(total_pixels)
        .map_err(|_| at!(HeicError::OutOfMemory))?;

    let y_start = frame.crop_top;
    let x_start = frame.crop_left;
    for y in 0..height {
        for x in 0..width {
            let src_idx = ((y_start + y) * frame.width + (x_start + x)) as usize;
            depth_data.push(frame.y_plane[src_idx]);
        }
    }

    Ok(crate::DepthMap {
        data: depth_data,
        width,
        height,
        bit_depth,
        depth_info,
    })
}

/// Decode a specific auxiliary image by item ID to grayscale u16 pixels.
///
/// This is a general-purpose decoder for any auxiliary image item,
/// returning the luma plane as u16 samples.
pub(crate) fn decode_auxiliary_item(
    data: &[u8],
    item_id: u32,
    layout: PixelLayout,
) -> Result<DecodeOutput> {
    let container = heif::parse(data, &Unstoppable)?;
    let item = container
        .get_item(item_id)
        .ok_or_else(|| at!(HeicError::InvalidData("auxiliary item not found")))?;

    let frame = decode_item(&container, &item, 0, &Limits::default(), &Unstoppable, None)?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();

    let pixels = match layout {
        PixelLayout::Rgb8 => frame.to_rgb(),
        PixelLayout::Rgba8 => frame.to_rgba(),
        PixelLayout::Bgr8 => frame.to_bgr(),
        PixelLayout::Bgra8 => frame.to_bgra(),
    };

    Ok(DecodeOutput {
        data: pixels,
        width,
        height,
        layout,
    })
}

/// Decode a single auxiliary item to grayscale 8-bit pixels.
///
/// The Y plane of the decoded HEVC frame is extracted and scaled
/// to 8-bit if the source bit depth is greater than 8.
fn decode_aux_to_grayscale(
    container: &heif::HeifContainer<'_>,
    item_id: u32,
) -> Result<(Vec<u8>, u32, u32)> {
    let item = container
        .get_item(item_id)
        .ok_or_else(|| at!(HeicError::InvalidData("auxiliary item not found")))?;

    let frame = decode_item(container, &item, 0, &Limits::default(), &Unstoppable, None)?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();
    let max_val = ((1u32 << frame.bit_depth) - 1) as u32;
    let y_start = frame.crop_top;
    let x_start = frame.crop_left;

    let mut grayscale = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        for x in 0..width {
            let src_idx = ((y_start + y) * frame.width + (x_start + x)) as usize;
            let raw = frame.y_plane[src_idx] as u32;
            // Scale to 8-bit
            let val = if frame.bit_depth == 8 {
                raw as u8
            } else {
                ((raw * 255 + max_val / 2) / max_val) as u8
            };
            grayscale.push(val);
        }
    }

    Ok((grayscale, width, height))
}

/// Decode all segmentation mattes from a HEIC file.
///
/// Looks for all known matte auxiliary types (portrait, skin, hair, teeth,
/// glasses) and decodes each to an 8-bit grayscale matte.
pub(crate) fn decode_mattes(data: &[u8]) -> Result<Vec<crate::SegmentationMatte>> {
    use crate::auxiliary::AuxiliaryImageType;

    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let matte_urns: &[(AuxiliaryImageType, &str)] = &[
        (
            AuxiliaryImageType::PortraitMatte,
            AuxiliaryImageType::PortraitMatte.urn(),
        ),
        (
            AuxiliaryImageType::SkinMatte,
            AuxiliaryImageType::SkinMatte.urn(),
        ),
        (
            AuxiliaryImageType::HairMatte,
            AuxiliaryImageType::HairMatte.urn(),
        ),
        (
            AuxiliaryImageType::TeethMatte,
            AuxiliaryImageType::TeethMatte.urn(),
        ),
        (
            AuxiliaryImageType::GlassesMatte,
            AuxiliaryImageType::GlassesMatte.urn(),
        ),
    ];

    let mut mattes = Vec::new();

    for (aux_type, urn) in matte_urns {
        let aux_ids = container.find_auxiliary_items(primary_item.id, urn);
        if let Some(&aux_id) = aux_ids.first() {
            let (pixels, width, height) = decode_aux_to_grayscale(&container, aux_id)?;
            mattes.push(crate::SegmentationMatte {
                data: pixels,
                width,
                height,
                matte_type: aux_type.clone(),
            });
        }
    }

    Ok(mattes)
}

/// Decode a specific segmentation matte type from a HEIC file.
///
/// Returns `None` if the requested matte type is not present.
pub(crate) fn decode_matte(
    data: &[u8],
    matte_type: &crate::auxiliary::AuxiliaryImageType,
) -> Result<Option<crate::SegmentationMatte>> {
    let container = heif::parse(data, &Unstoppable)?;
    let primary_item = container
        .primary_item()
        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

    let aux_ids = container.find_auxiliary_items(primary_item.id, matte_type.urn());
    let Some(&aux_id) = aux_ids.first() else {
        return Ok(None);
    };

    let (pixels, width, height) = decode_aux_to_grayscale(&container, aux_id)?;
    Ok(Some(crate::SegmentationMatte {
        data: pixels,
        width,
        height,
        matte_type: matte_type.clone(),
    }))
}
