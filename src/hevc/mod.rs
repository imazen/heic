//! HEVC/H.265 decoder
//!
//! This module implements the HEVC (High Efficiency Video Coding) decoder
//! for decoding HEIC still images.

mod availability;
pub mod bitstream;
mod cabac;
mod ctu;
pub mod debug;
mod deblock;
mod intra;
pub mod params;
mod picture;
mod residual;
pub mod slice;
mod transform;

pub use picture::DecodedFrame;

use crate::error::HevcError;
use crate::heif::HevcDecoderConfig;
use alloc::vec::Vec;

use ctu::{SaoComponentParams, SaoEoClass, SaoParams, SaoType};

type Result<T> = core::result::Result<T, HevcError>;

/// Decode HEVC bitstream to pixels (Annex B or raw format)
pub fn decode(data: &[u8]) -> Result<DecodedFrame> {
    // Parse NAL units
    let nal_units = bitstream::parse_nal_units(data)?;
    decode_nal_units(&nal_units)
}

/// Decode HEVC from HEIC container (config + image data)
///
/// This is the preferred method for HEIC files where parameter sets
/// are stored separately in the hvcC box.
pub fn decode_with_config(config: &HevcDecoderConfig, image_data: &[u8]) -> Result<DecodedFrame> {
    let mut nal_units = Vec::new();

    // Parse parameter sets from hvcC
    for nal_data in &config.nal_units {
        if let Ok(nal) = bitstream::parse_single_nal(nal_data) {
            nal_units.push(nal);
        }
    }

    // Parse slice data with correct length size
    let length_size = (config.length_size_minus_one + 1) as usize;
    let mut slice_nals = bitstream::parse_length_prefixed_ext(image_data, length_size)?;
    nal_units.append(&mut slice_nals);

    decode_nal_units(&nal_units)
}

/// Get image info from HEIC config
pub fn get_info_from_config(config: &HevcDecoderConfig) -> Result<ImageInfo> {
    for nal_data in &config.nal_units {
        if let Ok(nal) = bitstream::parse_single_nal(nal_data)
            && nal.nal_type == bitstream::NalType::SpsNut
        {
            let sps = params::parse_sps(&nal.payload)?;
            let (width, height) = get_cropped_dimensions(&sps);
            return Ok(ImageInfo { width, height });
        }
    }
    Err(HevcError::MissingParameterSet("SPS"))
}

/// Internal: decode from parsed NAL units
fn decode_nal_units(nal_units: &[bitstream::NalUnit<'_>]) -> Result<DecodedFrame> {
    // Find and parse parameter sets
    let mut _vps = None;
    let mut sps = None;
    let mut pps = None;

    for nal in nal_units {
        match nal.nal_type {
            bitstream::NalType::VpsNut => {
                _vps = Some(params::parse_vps(&nal.payload)?);
            }
            bitstream::NalType::SpsNut => {
                sps = Some(params::parse_sps(&nal.payload)?);
            }
            bitstream::NalType::PpsNut => {
                pps = Some(params::parse_pps(&nal.payload)?);
            }
            _ => {}
        }
    }

    let sps = sps.ok_or(HevcError::MissingParameterSet("SPS"))?;
    let pps = pps.ok_or(HevcError::MissingParameterSet("PPS"))?;

    // Create frame buffer
    let mut frame = DecodedFrame::new(
        sps.pic_width_in_luma_samples,
        sps.pic_height_in_luma_samples,
    );

    // Set conformance window cropping from SPS
    // Offsets are in units of SubWidthC/SubHeightC, need to convert to luma samples
    if sps.conformance_window_flag {
        let (sub_width_c, sub_height_c) = match sps.chroma_format_idc {
            0 => (1, 1), // Monochrome
            1 => (2, 2), // 4:2:0
            2 => (2, 1), // 4:2:2
            3 => (1, 1), // 4:4:4
            _ => (2, 2), // Default to 4:2:0
        };
        frame.set_crop(
            sps.conf_win_offset.0 * sub_width_c,  // left
            sps.conf_win_offset.1 * sub_width_c,  // right
            sps.conf_win_offset.2 * sub_height_c, // top
            sps.conf_win_offset.3 * sub_height_c, // bottom
        );
    }

    // Decode slice data
    for nal in nal_units {
        if nal.nal_type.is_slice() {
            decode_slice(nal, &sps, &pps, &mut frame)?;
        }
    }

    Ok(frame)
}

/// Get image info without full decoding
pub fn get_info(data: &[u8]) -> Result<ImageInfo> {
    let nal_units = bitstream::parse_nal_units(data)?;

    for nal in &nal_units {
        if nal.nal_type == bitstream::NalType::SpsNut {
            let sps = params::parse_sps(&nal.payload)?;
            let (width, height) = get_cropped_dimensions(&sps);
            return Ok(ImageInfo { width, height });
        }
    }

    Err(HevcError::MissingParameterSet("SPS"))
}

/// Calculate cropped dimensions from SPS conformance window
fn get_cropped_dimensions(sps: &params::Sps) -> (u32, u32) {
    if sps.conformance_window_flag {
        let (sub_width_c, sub_height_c) = match sps.chroma_format_idc {
            0 => (1, 1), // Monochrome
            1 => (2, 2), // 4:2:0
            2 => (2, 1), // 4:2:2
            3 => (1, 1), // 4:4:4
            _ => (2, 2), // Default to 4:2:0
        };
        let crop_left = sps.conf_win_offset.0 * sub_width_c;
        let crop_right = sps.conf_win_offset.1 * sub_width_c;
        let crop_top = sps.conf_win_offset.2 * sub_height_c;
        let crop_bottom = sps.conf_win_offset.3 * sub_height_c;
        (
            sps.pic_width_in_luma_samples - crop_left - crop_right,
            sps.pic_height_in_luma_samples - crop_top - crop_bottom,
        )
    } else {
        (
            sps.pic_width_in_luma_samples,
            sps.pic_height_in_luma_samples,
        )
    }
}

/// Image info from SPS
#[derive(Debug, Clone, Copy)]
pub struct ImageInfo {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
}

fn decode_slice(
    nal: &bitstream::NalUnit<'_>,
    sps: &params::Sps,
    pps: &params::Pps,
    frame: &mut DecodedFrame,
) -> Result<()> {
    // 1. Parse slice header and get data offset
    let parse_result = slice::SliceHeader::parse(nal, sps, pps)?;
    let slice_header = parse_result.header;
    let data_offset = parse_result.data_offset;

    // Verify this is an I-slice (required for HEIC still images)
    if !slice_header.slice_type.is_intra() {
        return Err(HevcError::Unsupported(
            "only I-slices supported for still images",
        ));
    }

    // 2. Get slice data (after header)
    // Use the offset from slice header parsing to skip the header bytes
    let slice_data = &nal.payload[data_offset..];

    // 3. Create slice context and decode CTUs
    let mut ctx = ctu::SliceContext::new(sps, pps, &slice_header, slice_data)?;

    // 4. Decode all CTUs in the slice
    let (deblock_metadata, sao_params) = ctx.decode_slice(frame)?;

    // 5. Apply in-loop filters (H.265 8.7.1)
    // 5a. Deblocking filter
    if !slice_header.slice_deblocking_filter_disabled_flag && !std::env::var("HEVC_NO_FILTER").is_ok() {
        deblock::apply_deblocking_filter(frame, sps, pps, &slice_header, &deblock_metadata);
    }
    // 5b. SAO (Sample Adaptive Offset) - applied after deblocking
    if sps.sample_adaptive_offset_enabled_flag && !std::env::var("HEVC_NO_FILTER").is_ok() {
        apply_sao(frame, sps, &slice_header, &sao_params);
    }

    Ok(())
}

/// Apply SAO (Sample Adaptive Offset) filtering to the entire frame
/// Per H.265 section 8.7.3, SAO is applied after deblocking using pre-SAO samples.
fn apply_sao(
    frame: &mut DecodedFrame,
    sps: &params::Sps,
    header: &slice::SliceHeader,
    sao_params: &[SaoParams],
) {
    if !header.slice_sao_luma_flag && !header.slice_sao_chroma_flag {
        return;
    }

    let ctb_size = sps.ctb_size();
    let pic_width = sps.pic_width_in_luma_samples;
    let pic_height = sps.pic_height_in_luma_samples;
    let ctbs_per_row = pic_width.div_ceil(ctb_size);
    let ctbs_per_col = pic_height.div_ceil(ctb_size);

    // Snapshot pre-SAO planes for edge offset neighbor lookups
    let y_snapshot = frame.y_plane.clone();
    let cb_snapshot = frame.cb_plane.clone();
    let cr_snapshot = frame.cr_plane.clone();

    for ctb_y in 0..ctbs_per_col {
        for ctb_x in 0..ctbs_per_row {
            let addr = (ctb_y * ctbs_per_row + ctb_x) as usize;
            if addr >= sao_params.len() {
                continue;
            }
            let params = &sao_params[addr];
            let x0 = ctb_x * ctb_size;
            let y0 = ctb_y * ctb_size;
            let x_end = (x0 + ctb_size).min(pic_width);
            let y_end = (y0 + ctb_size).min(pic_height);

            // Luma
            if header.slice_sao_luma_flag && params.luma.sao_type != SaoType::None {
                apply_sao_ctb(
                    &params.luma,
                    &y_snapshot,
                    &mut frame.y_plane,
                    pic_width as usize,
                    pic_height as usize,
                    x0 as usize, y0 as usize,
                    x_end as usize, y_end as usize,
                    sps.bit_depth_y(),
                );
            }

            // Chroma (4:2:0)
            if header.slice_sao_chroma_flag {
                let cx0 = (x0 / 2) as usize;
                let cy0 = (y0 / 2) as usize;
                let cx_end = (x_end / 2) as usize;
                let cy_end = (y_end / 2) as usize;
                let cpw = (pic_width / 2) as usize;
                let cph = (pic_height / 2) as usize;

                if params.cb.sao_type != SaoType::None {
                    apply_sao_ctb(
                        &params.cb,
                        &cb_snapshot,
                        &mut frame.cb_plane,
                        cpw, cph,
                        cx0, cy0, cx_end, cy_end,
                        sps.bit_depth_c(),
                    );
                }
                if params.cr.sao_type != SaoType::None {
                    apply_sao_ctb(
                        &params.cr,
                        &cr_snapshot,
                        &mut frame.cr_plane,
                        cpw, cph,
                        cx0, cy0, cx_end, cy_end,
                        sps.bit_depth_c(),
                    );
                }
            }
        }
    }
}

/// Apply SAO to a single CTB for a single component
/// Uses `src` (pre-SAO snapshot) for neighbor reads, writes to `dst`.
#[allow(clippy::too_many_arguments)]
fn apply_sao_ctb(
    params: &SaoComponentParams,
    src: &[u16],
    dst: &mut [u16],
    pic_w: usize,
    pic_h: usize,
    x_start: usize, y_start: usize,
    x_end: usize, y_end: usize,
    bit_depth: u8,
) {
    let max_val = (1i32 << bit_depth) - 1;

    match params.sao_type {
        SaoType::None => {}
        SaoType::Band => {
            let band_shift = bit_depth - 5;
            let band_pos = params.band_position as i32;

            for y in y_start..y_end {
                for x in x_start..x_end {
                    let idx = y * pic_w + x;
                    let sample = src[idx] as i32;
                    let band = sample >> band_shift;
                    let relative = band - band_pos;
                    if relative >= 0 && relative < 4 {
                        let offset = params.offsets[relative as usize];
                        dst[idx] = (sample + offset).clamp(0, max_val) as u16;
                    }
                }
            }
        }
        SaoType::Edge => {
            // Edge offset direction patterns
            let (dx1, dy1, dx2, dy2): (i32, i32, i32, i32) = match params.eo_class {
                SaoEoClass::Horizontal  => (-1,  0,  1,  0),
                SaoEoClass::Vertical    => ( 0, -1,  0,  1),
                SaoEoClass::Diagonal135 => (-1, -1,  1,  1),
                SaoEoClass::Diagonal45  => ( 1, -1, -1,  1),
            };

            for y in y_start..y_end {
                for x in x_start..x_end {
                    let nx1 = x as i32 + dx1;
                    let ny1 = y as i32 + dy1;
                    let nx2 = x as i32 + dx2;
                    let ny2 = y as i32 + dy2;

                    // Skip if neighbors outside picture
                    if nx1 < 0 || nx1 >= pic_w as i32 || ny1 < 0 || ny1 >= pic_h as i32
                        || nx2 < 0 || nx2 >= pic_w as i32 || ny2 < 0 || ny2 >= pic_h as i32
                    {
                        continue;
                    }

                    let idx = y * pic_w + x;
                    let sample = src[idx] as i32;
                    let n1 = src[ny1 as usize * pic_w + nx1 as usize] as i32;
                    let n2 = src[ny2 as usize * pic_w + nx2 as usize] as i32;

                    let c1 = (sample > n1) as i32 - (sample < n1) as i32;
                    let c2 = (sample > n2) as i32 - (sample < n2) as i32;
                    let edge_idx = 2 + c1 + c2; // 0..4

                    let offset = match edge_idx {
                        0 => params.offsets[0],      // Valley
                        1 => params.offsets[1],      // Half valley
                        2 => 0,                       // Flat
                        3 => -params.offsets[2],     // Half peak
                        _ => -params.offsets[3],     // Peak
                    };

                    if offset != 0 {
                        dst[idx] = (sample + offset).clamp(0, max_val) as u16;
                    }
                }
            }
        }
    }
}
