//! HEVC/H.265 decoder
//!
//! This module implements the HEVC (High Efficiency Video Coding) decoder
//! for decoding HEIC still images.

pub(crate) mod bitstream;
mod cabac;
pub(crate) mod color_convert;
mod ctu;
mod deblock;
pub(crate) mod debug;
pub(crate) mod dpb;
pub(crate) mod inter;
mod intra;
pub(crate) mod mc;
pub(crate) mod params;
mod picture;
pub(crate) mod refpic;
mod residual;
mod sao;
mod slice;
mod transform;
mod transform_simd;
mod transforms;

pub use picture::DecodedFrame;

use crate::error::HevcError;
use crate::heif::HevcDecoderConfig;
use alloc::vec::Vec;

use dpb::{Dpb, DpbEntry};
use inter::RefPicLists;

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

    // Sanity-check dimensions before allocating (prevent OOM from malicious SPS)
    let w = sps.pic_width_in_luma_samples;
    let h = sps.pic_height_in_luma_samples;
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(HevcError::InvalidParameterSet {
            kind: "SPS",
            msg: alloc::format!("invalid dimensions {}x{}", w, h),
        });
    }
    if w.checked_mul(h).is_none() {
        return Err(HevcError::InvalidParameterSet {
            kind: "SPS",
            msg: alloc::format!("dimensions {}x{} overflow u32", w, h),
        });
    }

    // Create frame buffer with actual bit depth and chroma format from SPS
    let mut frame = DecodedFrame::with_params(
        sps.pic_width_in_luma_samples,
        sps.pic_height_in_luma_samples,
        sps.bit_depth_y(),
        sps.chroma_format_idc,
    );
    frame.full_range = sps.video_full_range_flag;
    frame.matrix_coeffs = sps.matrix_coeffs;
    frame.color_primaries = sps.color_primaries;
    frame.transfer_characteristics = sps.transfer_characteristics;

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

    // Decode slice data (base layer only — skip enhancement layer NALs in L-HEVC streams)
    for nal in nal_units {
        if nal.nal_type.is_slice() && nal.nuh_layer_id == 0 {
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
        let crop_left = sps.conf_win_offset.0.saturating_mul(sub_width_c);
        let crop_right = sps.conf_win_offset.1.saturating_mul(sub_width_c);
        let crop_top = sps.conf_win_offset.2.saturating_mul(sub_height_c);
        let crop_bottom = sps.conf_win_offset.3.saturating_mul(sub_height_c);
        let w = sps
            .pic_width_in_luma_samples
            .saturating_sub(crop_left)
            .saturating_sub(crop_right)
            .max(1);
        let h = sps
            .pic_height_in_luma_samples
            .saturating_sub(crop_top)
            .saturating_sub(crop_bottom)
            .max(1);
        (w, h)
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

/// Stateful HEVC video decoder with decoded picture buffer (DPB).
///
/// For multi-frame decoding (P/B slices), create a `VideoDecoder` and
/// feed NAL units sequentially. For single I-frame HEIC still images,
/// use the simpler `decode()` or `decode_with_config()` functions.
pub struct VideoDecoder {
    /// Current SPS
    sps: Option<params::Sps>,
    /// Current PPS
    pps: Option<params::Pps>,
    /// Decoded picture buffer
    dpb: Dpb,
    /// Previous POC LSB (for POC derivation)
    prev_poc_lsb: u32,
    /// Previous POC MSB (for POC derivation)
    prev_poc_msb: i32,
    /// POC of the last decoded frame (for display order sorting)
    last_decoded_poc: i32,
}

impl VideoDecoder {
    /// Create a new video decoder
    pub fn new(max_dpb_size: usize) -> Self {
        Self {
            sps: None,
            pps: None,
            dpb: Dpb::new(max_dpb_size),
            prev_poc_lsb: 0,
            prev_poc_msb: 0,
            last_decoded_poc: 0,
        }
    }

    /// Decode a single NAL unit. Returns a decoded frame if one is produced.
    ///
    /// Call this repeatedly with each NAL unit in decode order.
    pub fn decode_nal(&mut self, nal: &bitstream::NalUnit<'_>) -> Result<Option<DecodedFrame>> {
        match nal.nal_type {
            bitstream::NalType::VpsNut => {
                let _vps = params::parse_vps(&nal.payload)?;
                Ok(None)
            }
            bitstream::NalType::SpsNut => {
                self.sps = Some(params::parse_sps(&nal.payload)?);
                Ok(None)
            }
            bitstream::NalType::PpsNut => {
                self.pps = Some(params::parse_pps(&nal.payload)?);
                Ok(None)
            }
            nt if nt.is_slice() && nal.nuh_layer_id == 0 => self.decode_slice_nal(nal),
            _ => Ok(None),
        }
    }

    /// Decode a slice NAL unit
    fn decode_slice_nal(
        &mut self,
        nal: &bitstream::NalUnit<'_>,
    ) -> Result<Option<DecodedFrame>> {
        let sps = self
            .sps
            .as_ref()
            .ok_or(HevcError::MissingParameterSet("SPS"))?;
        let pps = self
            .pps
            .as_ref()
            .ok_or(HevcError::MissingParameterSet("PPS"))?;

        let parse_result = slice::SliceHeader::parse(nal, sps, pps)?;
        let slice_header = parse_result.header;
        let data_offset = parse_result.data_offset;

        // Derive POC
        let is_irap = nal.nal_type.is_irap();
        let (curr_poc, poc_lsb, poc_msb) = refpic::derive_poc(
            slice_header.slice_pic_order_cnt_lsb,
            sps.log2_max_pic_order_cnt_lsb_minus4 + 4,
            self.prev_poc_lsb,
            self.prev_poc_msb,
            is_irap,
            false,
        );

        // IRAP: flush DPB
        if is_irap {
            self.dpb.flush();
        }

        // Build reference picture lists
        let ref_pic_lists = if !slice_header.slice_type.is_intra() {
            let active_rps = if let Some(ref inline) = slice_header.inline_short_term_rps {
                inline
            } else {
                let idx = slice_header.short_term_ref_pic_set_idx as usize;
                if idx < sps.short_term_rps.len() {
                    &sps.short_term_rps[idx]
                } else {
                    return Err(HevcError::InvalidBitstream("RPS index out of range"));
                }
            };

            let dpb_slots = self.dpb.active_slots_and_pocs();
            refpic::build_ref_pic_lists(
                curr_poc,
                active_rps,
                &dpb_slots,
                [
                    slice_header.num_ref_idx_l0_active,
                    slice_header.num_ref_idx_l1_active,
                ],
                slice_header.ref_pic_list_modification.as_ref(),
                slice_header.ref_pic_list_modification_flag,
                slice_header.slice_type == slice::SliceType::B,
            )
        } else {
            RefPicLists::default()
        };

        // Collect reference frames from DPB for the slice context
        let ref_frames: Vec<Option<DecodedFrame>> = if !slice_header.slice_type.is_intra() {
            // We need to clone frames from the DPB for the duration of decoding.
            // The current frame lives outside the DPB, so no aliasing.
            let max_dpb_slot = self.dpb.capacity();
            let mut frames = Vec::with_capacity(max_dpb_slot);
            for slot in 0..max_dpb_slot {
                if let Some(entry) = self.dpb.get(slot) {
                    // Clone the frame planes for MC access during decode
                    frames.push(Some(clone_frame_for_ref(&entry.frame)));
                } else {
                    frames.push(None);
                }
            }
            frames
        } else {
            Vec::new()
        };

        // Create frame
        let mut frame = create_frame(sps);

        // Build collocated frame for temporal MVP
        let collocated_data = if slice_header.slice_temporal_mvp_enabled_flag
            && !slice_header.slice_type.is_intra()
        {
            // Determine collocated reference
            let col_list = if slice_header.slice_type == slice::SliceType::B
                && !slice_header.collocated_from_l0_flag
            {
                1usize
            } else {
                0
            };
            let col_ref_idx = slice_header.collocated_ref_idx as usize;
            let col_dpb_idx = ref_pic_lists
                .dpb_index
                .get(col_list)
                .and_then(|l| l.get(col_ref_idx))
                .copied()
                .unwrap_or(-1);

            if col_dpb_idx >= 0 {
                self.dpb.get(col_dpb_idx as usize).map(|entry| {
                    let min_pu = ((1u32 << sps.log2_min_cb_size()) / 2).max(1);
                    ctu::OwnedCollocatedFrame {
                        mv_info: entry.mv_info.clone(),
                        pred_mode: entry.pred_mode_map.clone(),
                        pu_stride: entry.mv_stride,
                        min_pu_size: min_pu,
                        poc: entry.poc,
                        ref_poc: ref_pic_lists.poc, // approximate: use current slice's ref POCs
                    }
                })
            } else {
                None
            }
        } else {
            None
        };

        // Decode slice
        let slice_data = &nal.payload[data_offset..];
        let mut ctx = ctu::SliceContext::new(sps, pps, &slice_header, slice_data)?;
        ctx.curr_poc = curr_poc;
        ctx.ref_pic_lists = ref_pic_lists;
        ctx.ref_frames = ref_frames;
        ctx.collocated_data = collocated_data;

        ctx.decode_slice(&mut frame)?;

        // Apply deblocking
        apply_loop_filters(&slice_header, sps, pps, &ctx, &mut frame);

        // Update POC state
        self.prev_poc_lsb = poc_lsb;
        self.prev_poc_msb = poc_msb;
        self.last_decoded_poc = curr_poc;

        // Insert into DPB for future reference
        let min_pu = ((1u32 << sps.log2_min_cb_size()) / 2).max(1);
        let mut entry = DpbEntry::new(clone_frame_for_ref(&frame), curr_poc, min_pu);
        entry.mv_info = ctx.mv_info;
        entry.pred_mode_map = ctx.pred_mode_map;
        entry.is_output = true;
        self.dpb.insert(entry);

        Ok(Some(frame))
    }

    /// Decode an entire Annex B bitstream, returning all decoded frames in **display order** (by POC).
    ///
    /// This is the simplest way to decode a raw H.265 bitstream with P/B slices.
    pub fn decode_annex_b(&mut self, data: &[u8]) -> Result<Vec<DecodedFrame>> {
        let nal_units = bitstream::parse_nal_units(data)?;
        let mut frames: Vec<(i32, DecodedFrame)> = Vec::new();
        for nal in &nal_units {
            if let Some(frame) = self.decode_nal(nal)? {
                frames.push((self.last_decoded_poc, frame));
            }
        }
        // Sort by POC to produce display order
        frames.sort_by_key(|(poc, _)| *poc);
        Ok(frames.into_iter().map(|(_, f)| f).collect())
    }

    /// Flush: return any remaining pictures and clear the DPB
    pub fn flush(&mut self) {
        self.dpb.clear();
        self.prev_poc_lsb = 0;
        self.prev_poc_msb = 0;
    }
}

/// Clone a DecodedFrame's planes for use as a reference (no deblock/qp metadata needed)
fn clone_frame_for_ref(f: &DecodedFrame) -> DecodedFrame {
    DecodedFrame {
        width: f.width,
        height: f.height,
        y_plane: f.y_plane.clone(),
        cb_plane: f.cb_plane.clone(),
        cr_plane: f.cr_plane.clone(),
        bit_depth: f.bit_depth,
        chroma_format: f.chroma_format,
        crop_left: f.crop_left,
        crop_right: f.crop_right,
        crop_top: f.crop_top,
        crop_bottom: f.crop_bottom,
        deblock_flags: Vec::new(),
        deblock_stride: 0,
        qp_map: Vec::new(),
        alpha_plane: None,
        full_range: f.full_range,
        matrix_coeffs: f.matrix_coeffs,
        color_primaries: f.color_primaries,
        transfer_characteristics: f.transfer_characteristics,
    }
}

/// Create a frame from SPS parameters
fn create_frame(sps: &params::Sps) -> DecodedFrame {
    let mut frame = DecodedFrame::with_params(
        sps.pic_width_in_luma_samples,
        sps.pic_height_in_luma_samples,
        sps.bit_depth_y(),
        sps.chroma_format_idc,
    );
    frame.full_range = sps.video_full_range_flag;
    frame.matrix_coeffs = sps.matrix_coeffs;
    frame.color_primaries = sps.color_primaries;
    frame.transfer_characteristics = sps.transfer_characteristics;

    if sps.conformance_window_flag {
        let (sub_width_c, sub_height_c) = match sps.chroma_format_idc {
            0 => (1, 1),
            1 => (2, 2),
            2 => (2, 1),
            3 => (1, 1),
            _ => (2, 2),
        };
        frame.set_crop(
            sps.conf_win_offset.0 * sub_width_c,
            sps.conf_win_offset.1 * sub_width_c,
            sps.conf_win_offset.2 * sub_height_c,
            sps.conf_win_offset.3 * sub_height_c,
        );
    }
    frame
}

/// Apply deblocking + SAO loop filters
fn apply_loop_filters(
    slice_header: &slice::SliceHeader,
    sps: &params::Sps,
    pps: &params::Pps,
    ctx: &ctu::SliceContext<'_>,
    frame: &mut DecodedFrame,
) {
    if !slice_header.slice_deblocking_filter_disabled_flag {
        let beta_offset = slice_header.slice_beta_offset_div2 as i32 * 2;
        let tc_offset = slice_header.slice_tc_offset_div2 as i32 * 2;
        let cb_qp_offset = pps.pps_cb_qp_offset as i32;
        let cr_qp_offset = pps.pps_cr_qp_offset as i32;
        let inter_ctx = if !slice_header.slice_type.is_intra() {
            Some(deblock::InterDeblockCtx {
                pred_mode: &ctx.pred_mode_map,
                mv_info: &ctx.mv_info,
                pu_stride: ctx.intra_mode_map_stride,
                min_pu_size: ctx.min_pu_size(),
                cbf_map: &ctx.cbf_map,
                cbf_map_stride: ctx.cbf_map_stride,
            })
        } else {
            None
        };
        deblock::apply_deblocking_filter(
            frame,
            beta_offset,
            tc_offset,
            cb_qp_offset,
            cr_qp_offset,
            inter_ctx.as_ref(),
        );
    }
    if slice_header.slice_sao_luma_flag || slice_header.slice_sao_chroma_flag {
        sao::apply_sao(frame, &ctx.sao_map, sps.ctb_size());
    }
}

/// Stateless decode of a single slice (for HEIC I-frame images).
/// Rejects P/B slices — use `VideoDecoder` for inter prediction.
fn decode_slice(
    nal: &bitstream::NalUnit<'_>,
    sps: &params::Sps,
    pps: &params::Pps,
    frame: &mut DecodedFrame,
) -> Result<()> {
    let parse_result = slice::SliceHeader::parse(nal, sps, pps)?;
    let slice_header = parse_result.header;
    let data_offset = parse_result.data_offset;

    if !slice_header.slice_type.is_intra() {
        return Err(HevcError::Unsupported(
            "P/B slices require VideoDecoder (use decode_nal())",
        ));
    }

    let slice_data = &nal.payload[data_offset..];
    let mut ctx = ctu::SliceContext::new(sps, pps, &slice_header, slice_data)?;
    ctx.decode_slice(frame)?;

    apply_loop_filters(&slice_header, sps, pps, &ctx, frame);

    Ok(())
}
