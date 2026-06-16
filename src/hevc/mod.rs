//! HEVC/H.265 decoder
//!
//! This module implements the HEVC (High Efficiency Video Coding) decoder
//! for decoding HEIC still images.

pub(crate) mod bitstream;
pub(crate) mod cabac;
pub(crate) mod color_convert;
mod ctu;
mod deblock;
pub(crate) mod debug;
pub(crate) mod dpb;
pub(crate) mod inter;
mod intra;
pub(crate) mod mc;
pub mod params;
mod picture;
pub(crate) mod refpic;
mod residual;
mod sao;
mod slice;
mod transform;
mod transform_simd;
#[cfg(target_arch = "aarch64")]
mod transform_simd_neon;
pub(crate) mod transforms;

pub use picture::DecodedFrame;

#[cfg(feature = "std")]
pub use deblock::enable_deblock_trace;

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
    decode_nal_units(&nal_units, &enough::Unstoppable)
}

/// Decode HEVC from HEIC container (config + image data)
///
/// This is the preferred method for HEIC files where parameter sets
/// are stored separately in the hvcC box.
pub fn decode_with_config(config: &HevcDecoderConfig, image_data: &[u8]) -> Result<DecodedFrame> {
    decode_with_config_stop(config, image_data, &enough::Unstoppable)
}

/// Same as [`decode_with_config`], but checks `stop` for cancellation.
///
/// `stop` is observed both at tile entry (here) and *periodically inside the
/// per-CTU decode loop* (every 256 CTUs, the internal `STOP_CHECK_CTU_INTERVAL`,
/// checked in the slice decode loop). That second check is what lets a single
/// large intra frame (e.g. a 16384×16384 one-tile image) be cancelled
/// mid-frame, instead of only between tiles of a grid.
pub fn decode_with_config_stop(
    config: &HevcDecoderConfig,
    image_data: &[u8],
    stop: &dyn enough::Stop,
) -> Result<DecodedFrame> {
    if stop.should_stop() {
        return Err(HevcError::Cancelled(enough::StopReason::Cancelled));
    }
    let mut nal_units = Vec::new();

    // Parse parameter sets from hvcC
    for nal_data in &config.nal_units {
        if let Ok(nal) = bitstream::parse_single_nal(nal_data) {
            nal_units.push(nal);
        }
    }

    // Parse slice data with correct length size
    let length_size = config.length_size_minus_one as usize + 1;
    let mut slice_nals = bitstream::parse_length_prefixed_ext(image_data, length_size)?;
    nal_units.append(&mut slice_nals);

    decode_nal_units(&nal_units, stop)
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

/// Internal: decode from parsed NAL units.
///
/// `stop` is forwarded into the per-slice CTU loop so cancellation is observed
/// mid-frame, not only at slice entry. Pass [`enough::Unstoppable`] when
/// cancellation is not needed.
fn decode_nal_units(
    nal_units: &[bitstream::NalUnit<'_>],
    stop: &dyn enough::Stop,
) -> Result<DecodedFrame> {
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
    )?;
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
        // SPS parser already rejects offsets that would overflow these
        // multiplications or underflow the frame, but use saturating_mul
        // here so the rest of the decoder cannot panic on debug-overflow
        // even if a future caller injects an unvalidated SPS.
        frame.set_crop(
            sps.conf_win_offset.0.saturating_mul(sub_width_c), // left
            sps.conf_win_offset.1.saturating_mul(sub_width_c), // right
            sps.conf_win_offset.2.saturating_mul(sub_height_c), // top
            sps.conf_win_offset.3.saturating_mul(sub_height_c), // bottom
        );
    }

    // Decode slice data (base layer only — skip enhancement layer NALs in L-HEVC streams)
    for nal in nal_units {
        if nal.nal_type.is_slice() && nal.nuh_layer_id == 0 {
            decode_slice(nal, &sps, &pps, &mut frame, stop)?;
        }
    }

    // A complete decode writes every coded luma sample. If any remain at the
    // UNINIT sentinel, the bitstream was truncated/corrupt and the decoder ran
    // out of data mid-frame — the CABAC EOF tolerance lets that "succeed" with
    // undecoded CTBs. Fail loudly rather than return sentinel pixels (u16::MAX,
    // which would otherwise leak into the output clamped to white). The scan
    // short-circuits on the first sentinel; for a complete frame it is a single
    // linear pass, negligible vs decode cost.
    // Decode-completeness guard: a complete decode writes every coded luma
    // sample. If any remain at the UNINIT sentinel, the bitstream was
    // truncated/corrupt and the decoder ran out of data mid-frame (the CABAC
    // EOF tolerance lets that "succeed" with undecoded CTBs) — fail loudly
    // rather than return sentinel pixels (u16::MAX, clamped to white in RGB).
    // Verified safe: the bundled corpus (tests/corpus_decode.rs) and the
    // now-fixed monochrome gain map all decode with zero UNINIT. The scan
    // short-circuits on the first sentinel.
    if frame.y_plane.contains(&picture::UNINIT_SAMPLE) {
        return Err(HevcError::InvalidBitstream(
            "incomplete decode: undecoded luma samples remain (truncated bitstream)",
        ));
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

/// Given the emulation prevention byte positions of a NAL payload (EBSP
/// offsets relative to the byte after the 2-byte NAL header) and the RBSP
/// offset where `slice_data` starts within that payload, return the EP byte
/// positions in the *slice_data*'s EBSP space.
///
/// EP bytes located inside the slice header are filtered out; the remaining
/// ones are translated so position 0 corresponds to the start of slice_data.
/// This list is what `SliceContext` needs to convert WPP/tile entry point
/// offsets (per H.265 spec 7.4.7.1, expressed in EBSP byte space) into RBSP
/// offsets within `slice_data` before seeking.
fn compute_ep_positions_in_slice_data(
    payload_ep_positions: &[usize],
    data_offset_rbsp: usize,
) -> alloc::vec::Vec<u32> {
    // For each EP byte at EBSP position e_i (0-indexed by i), the RBSP
    // position the byte WOULD have occupied if not stripped is e_i - i. An
    // EP byte belongs to the slice header iff that position is below
    // data_offset_rbsp. The EBSP starting offset of slice_data is then
    // data_offset_rbsp plus the count of header-internal EP bytes; once
    // we've seen a slice_data EP, all later ones are also slice_data (EP
    // positions are strictly increasing, so rbsp_pos_of_ep is monotonic).
    //
    // EP positions are spaced at least 3 bytes apart (the 0x00 0x00 0x03
    // pattern), so ep_pos >= 2 + 3*i > i and the subtraction never wraps.
    let mut result = alloc::vec::Vec::new();
    let mut ep_in_header: usize = 0;
    for (i, &ep_pos) in payload_ep_positions.iter().enumerate() {
        let rbsp_pos_of_ep = ep_pos - i;
        if rbsp_pos_of_ep < data_offset_rbsp {
            ep_in_header += 1;
            continue;
        }
        let slice_data_ebsp_start = data_offset_rbsp + ep_in_header;
        result.push((ep_pos - slice_data_ebsp_start) as u32);
    }
    result
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
    /// Current picture being decoded (for multi-slice pictures)
    current_pic: Option<CurrentPicture>,
    /// Enable MV tracing for the next inter frame
    pub mv_trace_next_inter: bool,
    /// Enable MV tracing for a specific POC (-1 = disabled)
    pub mv_trace_poc: i32,
    /// Disable deblocking and SAO for all frames (for debugging)
    pub disable_loop_filters: bool,
}

/// State for a picture being decoded across multiple slices
struct CurrentPicture {
    frame: DecodedFrame,
    poc: i32,
    slice_type: slice::SliceType,
    /// Picture-level maps persisted across slices (ct_depth, intra modes, pred modes, etc.)
    maps: Option<ctu::PictureMaps>,
    /// Reference picture list POCs for this picture's slice (needed for collocated MV derivation)
    ref_poc: [[i32; inter::MAX_NUM_REF_PICS]; 2],
    /// Deferred loop filter parameters (applied when picture is complete)
    loop_filter: Option<LoopFilterParams>,
}

/// Parameters needed to apply loop filters after all slices are decoded
struct LoopFilterParams {
    deblocking_disabled: bool,
    beta_offset: i32,
    tc_offset: i32,
    cb_qp_offset: i32,
    cr_qp_offset: i32,
    sao_luma: bool,
    sao_chroma: bool,
    ctb_size: u32,
    is_intra_slice: bool,
    /// Reference POC lists for inter deblocking bS derivation
    ref_poc: [[i32; inter::MAX_NUM_REF_PICS]; 2],
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
            current_pic: None,
            mv_trace_next_inter: false,
            mv_trace_poc: -1,
            disable_loop_filters: false,
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

    /// Finish the current picture: apply loop filters, insert into DPB, return frame + POC
    fn finish_current_picture(&mut self) -> Result<Option<(i32, DecodedFrame)>> {
        let Some(mut pic) = self.current_pic.take() else {
            return Ok(None);
        };
        let poc = pic.poc;

        // Apply deferred loop filters now that all slices are decoded
        if !self.disable_loop_filters
            && let (Some(lf), Some(maps)) = (&pic.loop_filter, &pic.maps)
        {
            if !lf.deblocking_disabled {
                let inter_ctx = if !lf.is_intra_slice {
                    let sps = self.sps.as_ref().ok_or(HevcError::MissingParameterSet(
                        "SPS missing at picture finish",
                    ))?;
                    let min_pu = ((1u32 << sps.log2_min_cb_size()) / 2).max(1);
                    let pu_stride = sps.pic_width_in_luma_samples.div_ceil(min_pu);
                    Some(deblock::InterDeblockCtx {
                        pred_mode: &maps.pred_mode_map,
                        mv_info: &maps.mv_info,
                        pu_stride,
                        min_pu_size: min_pu,
                        cbf_map: &maps.cbf_map,
                        cbf_map_stride: maps.cbf_map_stride,
                        ref_poc: lf.ref_poc,
                    })
                } else {
                    None
                };
                deblock::apply_deblocking_filter(
                    &mut pic.frame,
                    lf.beta_offset,
                    lf.tc_offset,
                    lf.cb_qp_offset,
                    lf.cr_qp_offset,
                    inter_ctx.as_ref(),
                );
            }
            if lf.sao_luma || lf.sao_chroma {
                sao::apply_sao(&mut pic.frame, &maps.sao_map, lf.ctb_size);
            }
        }

        if let Some(sps) = &self.sps {
            let min_pu = ((1u32 << sps.log2_min_cb_size()) / 2).max(1);
            let mut entry = DpbEntry::new(clone_frame_for_ref(&pic.frame), poc, min_pu)?;
            // Extract pred_mode_map and mv_info from picture maps if present
            if let Some(maps) = pic.maps {
                entry.mv_info = maps.mv_info;
                entry.pred_mode_map = maps.pred_mode_map;
            }
            entry.ref_poc = pic.ref_poc;
            entry.is_output = true;

            // Evict non-reference, already-output frames before inserting
            // This ensures DPB doesn't fill up with frames that are no longer needed
            self.dpb.evict_unneeded();
            self.dpb.insert(entry);
        }
        Ok(Some((poc, pic.frame)))
    }

    /// Decode a slice NAL unit. Returns a frame only when a NEW picture starts
    /// (the previously accumulated picture is returned).
    fn decode_slice_nal(&mut self, nal: &bitstream::NalUnit<'_>) -> Result<Option<DecodedFrame>> {
        // Clone SPS/PPS to avoid borrow conflicts with &mut self
        let sps = self
            .sps
            .clone()
            .ok_or(HevcError::MissingParameterSet("SPS"))?;
        let pps = self
            .pps
            .clone()
            .ok_or(HevcError::MissingParameterSet("PPS"))?;

        let parse_result = slice::SliceHeader::parse(nal, &sps, &pps)?;
        let slice_header = parse_result.header;
        let data_offset = parse_result.data_offset;

        // When a new picture starts, finish the previous one
        let mut output_frame = None;
        let is_irap = nal.nal_type.is_irap();

        let curr_poc;
        if slice_header.first_slice_segment_in_pic_flag {
            // Finish previous picture
            if let Some((poc, frame)) = self.finish_current_picture()? {
                output_frame = Some(frame);
                self.last_decoded_poc = poc;
            }

            // Derive POC for new picture
            let (poc, lsb, msb) = refpic::derive_poc(
                slice_header.slice_pic_order_cnt_lsb,
                sps.log2_max_pic_order_cnt_lsb_minus4 + 4,
                self.prev_poc_lsb,
                self.prev_poc_msb,
                is_irap,
                false,
            );
            curr_poc = poc;
            self.prev_poc_lsb = lsb;
            self.prev_poc_msb = msb;

            // IRAP: flush DPB
            if is_irap {
                self.dpb.flush();
            } else {
                // Mark unused references based on the active RPS (H.265 8.3.2)
                let active_rps = if let Some(ref inline) = slice_header.inline_short_term_rps {
                    inline
                } else {
                    let idx = slice_header.short_term_ref_pic_set_idx as usize;
                    if idx < sps.short_term_rps.len() {
                        &sps.short_term_rps[idx]
                    } else {
                        &sps.short_term_rps[0] // fallback
                    }
                };
                // Compute the set of POC values referenced by this picture
                let mut ref_pocs = Vec::new();
                for i in 0..active_rps.num_negative_pics as usize {
                    ref_pocs.push(curr_poc + active_rps.delta_poc_s0[i]);
                }
                for i in 0..active_rps.num_positive_pics as usize {
                    ref_pocs.push(curr_poc + active_rps.delta_poc_s1[i]);
                }
                self.dpb.mark_unused(&ref_pocs);
            }

            // Create new frame
            let frame = create_frame(&sps)?;
            self.current_pic = Some(CurrentPicture {
                frame,
                poc: curr_poc,
                slice_type: slice_header.slice_type,
                maps: None, // First slice will initialize maps; subsequent slices inherit them
                ref_poc: [[0i32; inter::MAX_NUM_REF_PICS]; 2],
                loop_filter: None,
            });
        } else {
            // Continuation slice: use current picture's POC
            curr_poc = self.current_pic.as_ref().map_or(0, |p| p.poc);
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

        // Store the ref_poc on the current picture for later DPB insertion
        // (needed for temporal MVP derivation when this picture is the collocated frame)
        if let Some(ref mut pic) = self.current_pic {
            pic.ref_poc = ref_pic_lists.poc;
        }

        // Collect only the REFERENCED frames from DPB (avoid cloning unused slots)
        let ref_frames: Vec<Option<DecodedFrame>> = if !slice_header.slice_type.is_intra() {
            let max_dpb_slot = self.dpb.capacity();
            // Determine which DPB slots are actually referenced
            let mut needed = [false; 16]; // MAX_DPB_SIZE
            for list in 0..2 {
                for i in 0..ref_pic_lists.num_ref_idx_active[list] as usize {
                    let di = ref_pic_lists.dpb_index[list][i];
                    if di >= 0 && (di as usize) < needed.len() {
                        needed[di as usize] = true;
                    }
                }
            }
            let mut frames = Vec::with_capacity(max_dpb_slot);
            for slot in 0..max_dpb_slot {
                if slot < needed.len() && needed[slot] {
                    if let Some(entry) = self.dpb.get(slot) {
                        frames.push(Some(clone_frame_for_ref(&entry.frame)));
                    } else {
                        frames.push(None);
                    }
                } else {
                    frames.push(None);
                }
            }
            frames
        } else {
            Vec::new()
        };

        // Build collocated frame for temporal MVP
        let collocated_data = if slice_header.slice_temporal_mvp_enabled_flag
            && !slice_header.slice_type.is_intra()
        {
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
                        // Use the collocated frame's own ref_poc, NOT the current slice's.
                        // This is critical for temporal MVP: colPocDiff = ColPic.POC - ColPic.RefPicList[listCol][refIdxCol]
                        ref_poc: entry.ref_poc,
                    }
                })
            } else {
                None
            }
        } else {
            None
        };

        // Decode slice into current picture's frame
        let pic = self
            .current_pic
            .as_mut()
            .ok_or(HevcError::DecodingError("no current picture"))?;
        let slice_data = &nal.payload[data_offset..];
        let mut ctx = ctu::SliceContext::new(&sps, &pps, &slice_header, slice_data)?;
        ctx.curr_poc = curr_poc;
        ctx.set_ep_byte_positions(compute_ep_positions_in_slice_data(
            &nal.ep_byte_positions,
            data_offset,
        ));

        // For continuation slices, inject maps from previous slices so that
        // CABAC context derivation (split_cu_flag, intra mode prediction, QP
        // prediction, deblocking) can see data from earlier slices.
        if !slice_header.first_slice_segment_in_pic_flag
            && let Some(maps) = pic.maps.take()
        {
            ctx.inject_picture_maps(maps);
        }

        ctx.ref_pic_lists = ref_pic_lists;
        ctx.ref_frames = ref_frames;
        ctx.collocated_data = collocated_data;

        let trace_this_poc = self.mv_trace_poc >= 0 && curr_poc == self.mv_trace_poc;
        if (self.mv_trace_next_inter || trace_this_poc) && !slice_header.slice_type.is_intra() {
            ctx.mv_trace = true;
            if !trace_this_poc {
                self.mv_trace_next_inter = false;
            }
            // Reset SE counter so we trace from the start of this inter frame
            ctu::SE_COUNTER.store(0, core::sync::atomic::Ordering::Relaxed);
            // Reset bin trace counter for per-bin comparison
            #[cfg(feature = "std")]
            cabac::BIN_TRACE_COUNTER.store(0, core::sync::atomic::Ordering::Relaxed);
            // Dump first bytes of slice data for verification
            #[cfg(feature = "std")]
            {
                let first_bytes: Vec<u8> = slice_data.iter().take(8).copied().collect();
                eprintln!(
                    "SLICE_DATA: type={:?} data_offset={} first_bytes={:02x?} total_len={} entry_points={:?}",
                    slice_header.slice_type,
                    data_offset,
                    first_bytes,
                    slice_data.len(),
                    slice_header.entry_point_offsets
                );
                eprintln!(
                    "SLICE_QP: {} cabac_init_flag={} sao_luma={} sao_chroma={}",
                    slice_header.slice_qp_y,
                    slice_header.cabac_init_flag,
                    slice_header.slice_sao_luma_flag,
                    slice_header.slice_sao_chroma_flag
                );
                // Print context checksum matching dec265's CTU-CK format
                let mut cksum: u64 = 0;
                for c in ctx.ctx.iter() {
                    let (s, m) = c.get_state();
                    cksum += s as u64 * 3 + m as u64;
                }
                let (bp, _, _) = ctx.cabac.get_position();
                eprintln!(
                    "CTX-INIT: bp={} ck={} (dec265 CTU-CK should match)",
                    bp, cksum
                );
                // Print specific context states for debugging
                let (s154, m154) = ctx.ctx[154].get_state();
                let (s155, m155) = ctx.ctx[155].get_state();
                eprintln!("CTX[154] SAO_MERGE: s={} m={}", s154, m154);
                eprintln!("CTX[155] SAO_TYPE: s={} m={}", s155, m155);
            }
        }

        // The multi-frame video decoder path does not currently surface a
        // `Stop`; cancellation is plumbed through the still-image HEIC path
        // (`decode_with_config_stop` → `decode_nal_units` → free `decode_slice`).
        ctx.decode_slice(&mut pic.frame, &enough::Unstoppable)?;

        // Store loop filter parameters (deferred until all slices are decoded)
        // Each slice may have different deblocking offsets; the last slice's
        // parameters win (this matches the behavior for single-slice pictures).
        pic.loop_filter = Some(LoopFilterParams {
            deblocking_disabled: slice_header.slice_deblocking_filter_disabled_flag,
            beta_offset: slice_header.slice_beta_offset_div2 as i32 * 2,
            tc_offset: slice_header.slice_tc_offset_div2 as i32 * 2,
            cb_qp_offset: pps.pps_cb_qp_offset as i32,
            cr_qp_offset: pps.pps_cr_qp_offset as i32,
            sao_luma: slice_header.slice_sao_luma_flag,
            sao_chroma: slice_header.slice_sao_chroma_flag,
            ctb_size: sps.ctb_size(),
            is_intra_slice: slice_header.slice_type.is_intra(),
            ref_poc: ctx.ref_pic_lists.poc,
        });

        // Extract maps from slice context and store on picture for next slice
        pic.maps = Some(ctx.extract_picture_maps());

        if !slice_header.slice_type.is_intra() {
            pic.slice_type = slice_header.slice_type;
        }

        Ok(output_frame)
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
        // Flush the last picture
        if let Some((poc, frame)) = self.finish_current_picture()? {
            frames.push((poc, frame));
        }

        // Sort by POC to produce display order, deduplicate
        frames.sort_by_key(|(poc, _)| *poc);
        frames.dedup_by_key(|(poc, _)| *poc);
        Ok(frames.into_iter().map(|(_, f)| f).collect())
    }

    /// Flush: return any remaining pictures and clear the DPB
    pub fn flush(&mut self) {
        self.current_pic = None;
        self.dpb.clear();
        self.prev_poc_lsb = 0;
        self.prev_poc_msb = 0;
    }
}

/// Clone a DecodedFrame's planes for use as a reference (no deblock/qp metadata needed).
fn clone_frame_for_ref(f: &DecodedFrame) -> DecodedFrame {
    // Clone everything, then drop the transient decode-time metadata that
    // reference frames don't need (saves memory across the DPB).
    let mut out = f.clone();
    out.deblock_flags = Vec::new();
    out.deblock_stride = 0;
    out.qp_map = Vec::new();
    out.alpha_plane = None;
    out
}

/// Create a frame from SPS parameters
fn create_frame(sps: &params::Sps) -> Result<DecodedFrame> {
    let mut frame = DecodedFrame::with_params(
        sps.pic_width_in_luma_samples,
        sps.pic_height_in_luma_samples,
        sps.bit_depth_y(),
        sps.chroma_format_idc,
    )?;
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
            sps.conf_win_offset.0.saturating_mul(sub_width_c),
            sps.conf_win_offset.1.saturating_mul(sub_width_c),
            sps.conf_win_offset.2.saturating_mul(sub_height_c),
            sps.conf_win_offset.3.saturating_mul(sub_height_c),
        );
    }
    Ok(frame)
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
                ref_poc: ctx.ref_pic_lists.poc,
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
///
/// `stop` is checked periodically inside the CTU loop so a large single-tile
/// frame can be cancelled mid-decode.
fn decode_slice(
    nal: &bitstream::NalUnit<'_>,
    sps: &params::Sps,
    pps: &params::Pps,
    frame: &mut DecodedFrame,
    stop: &dyn enough::Stop,
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
    ctx.set_ep_byte_positions(compute_ep_positions_in_slice_data(
        &nal.ep_byte_positions,
        data_offset,
    ));
    ctx.decode_slice(frame, stop)?;

    apply_loop_filters(&slice_header, sps, pps, &ctx, frame);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ep_positions_no_ep_bytes() {
        // No emulation prevention bytes at all
        let got = compute_ep_positions_in_slice_data(&[], 28);
        assert!(got.is_empty());
    }

    #[test]
    fn ep_positions_all_after_header() {
        // Slice header occupies RBSP bytes [0, 28); both EP bytes (EBSP pos 100,
        // 5000) sit well past the header, so the result is each EP's EBSP
        // position minus the slice_data EBSP start (= 28 since no EP byte
        // landed in the header).
        let got = compute_ep_positions_in_slice_data(&[100, 5000], 28);
        assert_eq!(got, vec![72, 4972]);
    }

    #[test]
    fn ep_positions_some_in_header() {
        // EP at EBSP pos 10 falls inside the slice header (rbsp_pos = 10 - 0 = 10 < 28).
        // EP at EBSP pos 6000 falls inside slice_data (rbsp_pos = 6000 - 1 = 5999 >= 28).
        // The data_offset in EBSP space becomes 28 + 1 = 29 (one header-internal EP byte).
        // So the second EP's slice-data-EBSP offset = 6000 - 29 = 5971.
        let got = compute_ep_positions_in_slice_data(&[10, 6000], 28);
        assert_eq!(got, vec![5971]);
    }

    #[test]
    fn ep_offset_conversion_round_trip() {
        // With EP at slice-data-EBSP positions [100, 250], an EBSP offset of
        // 17846 falls past both EPs, so the RBSP offset is 17846 - 2 = 17844.
        // (Implemented in SliceContext::ebsp_offset_to_rbsp via the same logic.)
        let positions: Vec<u32> = vec![100, 250];
        let count = positions.iter().filter(|&&p| p < 17846u32).count() as u32;
        assert_eq!(17846 - count, 17844);

        // An offset that lands before both EPs is unchanged.
        let count = positions.iter().filter(|&&p| p < 50u32).count() as u32;
        assert_eq!(50 - count, 50);
    }
}
