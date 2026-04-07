//! HEVC slice header and slice segment decoding
//!
//! This module handles parsing of slice segment headers (H.265 spec 7.3.6)
//! and orchestrates CTU decoding for each slice.

use super::bitstream::{BitstreamReader, NalUnit};
use super::params::{Pps, Sps};
use super::refpic;
use crate::error::HevcError;
use alloc::vec::Vec;

type Result<T> = core::result::Result<T, HevcError>;

/// Slice type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SliceType {
    /// B slice (bidirectional prediction)
    B = 0,
    /// P slice (unidirectional prediction)
    P = 1,
    /// I slice (intra prediction only)
    I = 2,
}

impl SliceType {
    /// Create from raw value
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::B),
            1 => Some(Self::P),
            2 => Some(Self::I),
            _ => None,
        }
    }

    /// Check if this is an intra-only slice
    pub fn is_intra(self) -> bool {
        self == Self::I
    }
}

/// Partition mode for coding units
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PartMode {
    /// 2Nx2N - single partition
    Part2Nx2N = 0,
    /// 2NxN - horizontal split
    Part2NxN = 1,
    /// Nx2N - vertical split
    PartNx2N = 2,
    /// NxN - quad split (only for smallest CU with intra)
    PartNxN = 3,
    /// 2NxnU - asymmetric horizontal (small top)
    Part2NxnU = 4,
    /// 2NxnD - asymmetric horizontal (small bottom)
    Part2NxnD = 5,
    /// nLx2N - asymmetric vertical (small left)
    PartnLx2N = 6,
    /// nRx2N - asymmetric vertical (small right)
    PartnRx2N = 7,
}

/// Prediction mode
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredMode {
    /// Intra prediction
    Intra,
    /// Inter prediction
    Inter,
    /// Skip mode (inter)
    Skip,
}

/// Intra prediction mode (35 modes total)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum IntraPredMode {
    /// Planar mode (smooth gradient)
    Planar = 0,
    /// DC mode (average value)
    Dc = 1,
    /// Angular modes 2-34 (directional)
    Angular2 = 2,
    Angular3 = 3,
    Angular4 = 4,
    Angular5 = 5,
    Angular6 = 6,
    Angular7 = 7,
    Angular8 = 8,
    Angular9 = 9,
    Angular10 = 10,
    Angular11 = 11,
    Angular12 = 12,
    Angular13 = 13,
    Angular14 = 14,
    Angular15 = 15,
    Angular16 = 16,
    Angular17 = 17,
    Angular18 = 18,
    Angular19 = 19,
    Angular20 = 20,
    Angular21 = 21,
    Angular22 = 22,
    Angular23 = 23,
    Angular24 = 24,
    Angular25 = 25,
    Angular26 = 26,
    Angular27 = 27,
    Angular28 = 28,
    Angular29 = 29,
    Angular30 = 30,
    Angular31 = 31,
    Angular32 = 32,
    Angular33 = 33,
    Angular34 = 34,
}

impl IntraPredMode {
    /// Create from raw value
    #[inline]
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            0 => Some(Self::Planar),
            1 => Some(Self::Dc),
            2 => Some(Self::Angular2),
            3 => Some(Self::Angular3),
            4 => Some(Self::Angular4),
            5 => Some(Self::Angular5),
            6 => Some(Self::Angular6),
            7 => Some(Self::Angular7),
            8 => Some(Self::Angular8),
            9 => Some(Self::Angular9),
            10 => Some(Self::Angular10),
            11 => Some(Self::Angular11),
            12 => Some(Self::Angular12),
            13 => Some(Self::Angular13),
            14 => Some(Self::Angular14),
            15 => Some(Self::Angular15),
            16 => Some(Self::Angular16),
            17 => Some(Self::Angular17),
            18 => Some(Self::Angular18),
            19 => Some(Self::Angular19),
            20 => Some(Self::Angular20),
            21 => Some(Self::Angular21),
            22 => Some(Self::Angular22),
            23 => Some(Self::Angular23),
            24 => Some(Self::Angular24),
            25 => Some(Self::Angular25),
            26 => Some(Self::Angular26),
            27 => Some(Self::Angular27),
            28 => Some(Self::Angular28),
            29 => Some(Self::Angular29),
            30 => Some(Self::Angular30),
            31 => Some(Self::Angular31),
            32 => Some(Self::Angular32),
            33 => Some(Self::Angular33),
            34 => Some(Self::Angular34),
            _ => None,
        }
    }

    /// Get the raw mode value
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Slice segment header
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SliceHeader {
    /// First slice segment in picture flag
    pub first_slice_segment_in_pic_flag: bool,
    /// No output of prior pics flag (for RAP pictures)
    pub no_output_of_prior_pics_flag: bool,
    /// PPS ID
    pub pps_id: u8,
    /// Dependent slice segment flag
    pub dependent_slice_segment_flag: bool,
    /// Slice segment address (CTB index)
    pub slice_segment_address: u32,

    /// Slice type (I, P, B)
    pub slice_type: SliceType,
    /// Picture output flag
    pub pic_output_flag: bool,
    /// Colour plane ID (for separate colour plane)
    pub colour_plane_id: u8,
    /// Picture order count LSB
    pub slice_pic_order_cnt_lsb: u32,

    /// SAO luma flag
    pub slice_sao_luma_flag: bool,
    /// SAO chroma flag
    pub slice_sao_chroma_flag: bool,

    /// Slice QP delta
    pub slice_qp_delta: i8,
    /// Slice Cb QP offset
    pub slice_cb_qp_offset: i8,
    /// Slice Cr QP offset
    pub slice_cr_qp_offset: i8,

    /// CU chroma QP offset enabled flag
    pub cu_chroma_qp_offset_enabled_flag: bool,

    /// Deblocking filter override flag
    pub deblocking_filter_override_flag: bool,
    /// Slice deblocking filter disabled flag
    pub slice_deblocking_filter_disabled_flag: bool,
    /// Beta offset div2
    pub slice_beta_offset_div2: i8,
    /// Tc offset div2
    pub slice_tc_offset_div2: i8,

    /// Loop filter across slices enabled flag
    pub slice_loop_filter_across_slices_enabled_flag: bool,

    /// Number of entry point offsets (for tiles/WPP)
    pub num_entry_point_offsets: u32,
    /// Entry point byte offsets for substream boundaries
    pub entry_point_offsets: Vec<u32>,

    /// Active short-term RPS index (into SPS short_term_rps, or inline)
    pub short_term_ref_pic_set_idx: u8,
    /// Inline short-term RPS parsed from slice header (if not from SPS)
    pub inline_short_term_rps: Option<refpic::ShortTermRefPicSet>,
    /// Temporal MVP enabled flag (per-slice)
    pub slice_temporal_mvp_enabled_flag: bool,

    // -- Inter prediction fields (P/B slices) --
    /// Number of active L0 references
    pub num_ref_idx_l0_active: u8,
    /// Number of active L1 references
    pub num_ref_idx_l1_active: u8,
    /// MVD L1 zero flag (B-slices)
    pub mvd_l1_zero_flag: bool,
    /// CABAC init flag
    pub cabac_init_flag: bool,
    /// Collocated from L0 flag (temporal MVP)
    pub collocated_from_l0_flag: bool,
    /// Collocated reference index
    pub collocated_ref_idx: u8,
    /// Maximum number of merge candidates
    pub max_num_merge_cand: u8,
    /// Reference picture list modification tables \[L0\]\[L1\]
    pub ref_pic_list_modification: Option<[[u8; super::inter::MAX_NUM_REF_PICS]; 2]>,
    /// Reference picture list modification flags \[L0, L1\]
    pub ref_pic_list_modification_flag: [bool; 2],
    /// Prediction weight table
    pub pred_weight_table: Option<super::inter::PredWeightTable>,

    /// Derived: SliceQPY = 26 + pps.init_qp_minus26 + slice_qp_delta
    pub slice_qp_y: i32,
}

/// Parse result containing header and data offset
pub struct SliceParseResult {
    /// Parsed slice header
    pub header: SliceHeader,
    /// Byte offset where slice data begins (after header)
    pub data_offset: usize,
}

impl SliceHeader {
    /// Parse slice segment header from NAL unit
    /// Returns both the header and the byte offset where slice data begins
    pub fn parse(nal: &NalUnit<'_>, sps: &Sps, pps: &Pps) -> Result<SliceParseResult> {
        let mut reader = BitstreamReader::new(&nal.payload);

        let first_slice_segment_in_pic_flag = reader.read_bit()? != 0;

        // no_output_of_prior_pics_flag only present for IRAP pictures
        let no_output_of_prior_pics_flag = if nal.nal_type.is_irap() {
            reader.read_bit()? != 0
        } else {
            false
        };

        let pps_id = reader.read_ue()? as u8;
        if pps_id != pps.pps_id {
            return Err(HevcError::InvalidBitstream("PPS ID mismatch"));
        }

        let dependent_slice_segment_flag;
        let slice_segment_address;

        if !first_slice_segment_in_pic_flag {
            dependent_slice_segment_flag = if pps.dependent_slice_segments_enabled_flag {
                reader.read_bit()? != 0
            } else {
                false
            };

            // Calculate bits needed for slice_segment_address
            let pic_size_in_ctbs = sps.pic_width_in_ctbs() * sps.pic_height_in_ctbs();
            let address_bits = ceil_log2(pic_size_in_ctbs);
            slice_segment_address = reader.read_bits(address_bits)?;
        } else {
            dependent_slice_segment_flag = false;
            slice_segment_address = 0;
        }

        // If dependent slice, we'd inherit from previous slice header
        // For simplicity in still images, we don't support dependent slices
        if dependent_slice_segment_flag {
            return Err(HevcError::Unsupported("dependent slice segments"));
        }

        // Skip reserved bits
        for _ in 0..pps.num_extra_slice_header_bits {
            reader.read_bit()?;
        }

        let slice_type_val = reader.read_ue()? as u8;
        let slice_type = SliceType::from_u8(slice_type_val)
            .ok_or(HevcError::InvalidBitstream("invalid slice type"))?;

        let pic_output_flag = if pps.output_flag_present_flag {
            reader.read_bit()? != 0
        } else {
            true
        };

        let colour_plane_id = if sps.separate_colour_plane_flag {
            reader.read_bits(2)? as u8
        } else {
            0
        };

        // For IDR pictures, POC LSB and ref pic set are not present
        let slice_pic_order_cnt_lsb = if !nal.nal_type.is_idr() {
            let poc_bits = sps.log2_max_pic_order_cnt_lsb_minus4 + 4;
            reader.read_bits(poc_bits)?
        } else {
            0
        };

        // Parse short-term and long-term reference picture sets
        let mut short_term_ref_pic_set_idx = 0u8;
        let mut inline_short_term_rps = None;
        let mut slice_temporal_mvp_enabled_flag = false;

        if !nal.nal_type.is_idr() {
            let short_term_ref_pic_set_sps_flag = reader.read_bit()? != 0;

            if !short_term_ref_pic_set_sps_flag {
                // Parse inline short-term ref pic set (index = num_short_term_ref_pic_sets)
                let rps = refpic::parse_short_term_rps(
                    &mut reader,
                    sps.num_short_term_ref_pic_sets,
                    sps.num_short_term_ref_pic_sets,
                    &sps.short_term_rps,
                )?;
                inline_short_term_rps = Some(rps);
                short_term_ref_pic_set_idx = sps.num_short_term_ref_pic_sets;
            } else if sps.num_short_term_ref_pic_sets > 1 {
                let bits = ceil_log2(sps.num_short_term_ref_pic_sets as u32);
                short_term_ref_pic_set_idx = reader.read_bits(bits)? as u8;
            }

            // Long-term ref pics
            if sps.long_term_ref_pics_present_flag {
                let num_lt_sps = sps.long_term_ref_pics_sps.lt_ref_pic_poc_lsb.len();
                let num_long_term_sps = if num_lt_sps > 0 { reader.read_ue()? } else { 0 };
                let num_long_term_pics = reader.read_ue()?;

                let poc_bits = sps.log2_max_pic_order_cnt_lsb_minus4 + 4;

                for i in 0..(num_long_term_sps + num_long_term_pics) {
                    if i < num_long_term_sps {
                        // lt_idx_sps
                        if num_lt_sps > 1 {
                            let bits = ceil_log2(num_lt_sps as u32);
                            reader.read_bits(bits)?;
                        }
                    } else {
                        reader.read_bits(poc_bits)?; // poc_lsb_lt
                        reader.read_bit()?; // used_by_curr_pic_lt_flag
                    }
                    let delta_poc_msb_present = reader.read_bit()? != 0;
                    if delta_poc_msb_present {
                        reader.read_ue()?; // delta_poc_msb_cycle_lt
                    }
                }
            }

            // Temporal MVP
            if sps.sps_temporal_mvp_enabled_flag {
                slice_temporal_mvp_enabled_flag = reader.read_bit()? != 0;
            }
        }

        // SAO flags
        let (slice_sao_luma_flag, slice_sao_chroma_flag) =
            if sps.sample_adaptive_offset_enabled_flag {
                let luma = reader.read_bit()? != 0;
                let chroma = if sps.chroma_array_type() != 0 {
                    reader.read_bit()? != 0
                } else {
                    false
                };
                (luma, chroma)
            } else {
                (false, false)
            };

        // Parse P/B slice inter prediction fields
        let mut num_ref_idx_l0_active =
            pps.num_ref_idx_l0_default_active_minus1.min(14).saturating_add(1);
        let mut num_ref_idx_l1_active = if slice_type == SliceType::B {
            pps.num_ref_idx_l1_default_active_minus1.min(14).saturating_add(1)
        } else {
            0
        };
        let mut mvd_l1_zero_flag = false;
        let mut cabac_init_flag = false;
        let mut collocated_from_l0_flag = true;
        let mut collocated_ref_idx = 0u8;
        let mut max_num_merge_cand = 5u8;
        let mut ref_pic_list_modification = None;
        let mut ref_pic_list_modification_flag = [false; 2];
        let mut pred_weight_table = None;

        if slice_type != SliceType::I {
            // num_ref_idx_active_override_flag
            let override_flag = reader.read_bit()? != 0;
            if override_flag {
                let v = reader.read_ue()?;
                if v > 14 {
                    return Err(HevcError::InvalidBitstream(
                        "num_ref_idx_l0_active_minus1 exceeds 14",
                    ));
                }
                num_ref_idx_l0_active = v as u8 + 1;
                if slice_type == SliceType::B {
                    let v = reader.read_ue()?;
                    if v > 14 {
                        return Err(HevcError::InvalidBitstream(
                            "num_ref_idx_l1_active_minus1 exceeds 14",
                        ));
                    }
                    num_ref_idx_l1_active = v as u8 + 1;
                }
            }

            // ref_pic_list_modification (H.265 7.3.6.2)
            if pps.lists_modification_present_flag {
                let total_curr_pics =
                    count_curr_pics(sps, short_term_ref_pic_set_idx, &inline_short_term_rps);
                if total_curr_pics > 1 {
                    let mut mod_table = [[0u8; super::inter::MAX_NUM_REF_PICS]; 2];
                    let bits = ceil_log2(total_curr_pics).max(1);

                    // L0 modification
                    let l0_flag = reader.read_bit()? != 0;
                    ref_pic_list_modification_flag[0] = l0_flag;
                    if l0_flag {
                        for entry in mod_table[0].iter_mut().take(num_ref_idx_l0_active as usize) {
                            *entry = reader.read_bits(bits)? as u8;
                        }
                    }

                    // L1 modification (B-slices only)
                    if slice_type == SliceType::B {
                        let l1_flag = reader.read_bit()? != 0;
                        ref_pic_list_modification_flag[1] = l1_flag;
                        if l1_flag {
                            for entry in
                                mod_table[1].iter_mut().take(num_ref_idx_l1_active as usize)
                            {
                                *entry = reader.read_bits(bits)? as u8;
                            }
                        }
                    }

                    ref_pic_list_modification = Some(mod_table);
                }
            }

            // mvd_l1_zero_flag (B-slices only)
            if slice_type == SliceType::B {
                mvd_l1_zero_flag = reader.read_bit()? != 0;
            }

            // cabac_init_flag
            if pps.cabac_init_present_flag {
                cabac_init_flag = reader.read_bit()? != 0;
            }

            // Collocated info (for temporal MVP)
            if slice_temporal_mvp_enabled_flag {
                if slice_type == SliceType::B {
                    collocated_from_l0_flag = reader.read_bit()? != 0;
                }
                let max_ref = if collocated_from_l0_flag {
                    num_ref_idx_l0_active
                } else {
                    num_ref_idx_l1_active
                };
                if max_ref > 1 {
                    collocated_ref_idx = reader.read_ue()? as u8;
                }
            }

            // pred_weight_table
            if (pps.weighted_pred_flag && slice_type == SliceType::P)
                || (pps.weighted_bipred_flag && slice_type == SliceType::B)
            {
                pred_weight_table = Some(parse_pred_weight_table(
                    &mut reader,
                    sps,
                    slice_type,
                    num_ref_idx_l0_active,
                    num_ref_idx_l1_active,
                )?);
            }

            // five_minus_max_num_merge_cand
            let five_minus = reader.read_ue()? as u8;
            max_num_merge_cand = 5u8.saturating_sub(five_minus);
        }

        // slice_qp_delta
        let slice_qp_delta = reader.read_se()? as i8;

        // Chroma QP offsets
        let (slice_cb_qp_offset, slice_cr_qp_offset) =
            if pps.pps_slice_chroma_qp_offsets_present_flag {
                let cb = reader.read_se()? as i8;
                let cr = reader.read_se()? as i8;
                (cb, cr)
            } else {
                (0, 0)
            };

        // CU chroma QP offset
        let cu_chroma_qp_offset_enabled_flag = false; // Skip range extension for now

        // Deblocking filter
        let deblocking_filter_override_flag = if pps.deblocking_filter_override_enabled_flag {
            reader.read_bit()? != 0
        } else {
            false
        };

        let (slice_deblocking_filter_disabled_flag, slice_beta_offset_div2, slice_tc_offset_div2) =
            if deblocking_filter_override_flag {
                let disabled = reader.read_bit()? != 0;
                if !disabled {
                    let beta = reader.read_se()? as i8;
                    let tc = reader.read_se()? as i8;
                    (disabled, beta, tc)
                } else {
                    (disabled, 0, 0)
                }
            } else {
                (
                    pps.pps_deblocking_filter_disabled_flag,
                    pps.pps_beta_offset_div2,
                    pps.pps_tc_offset_div2,
                )
            };

        // Loop filter across slices
        let slice_loop_filter_across_slices_enabled_flag = if pps
            .pps_loop_filter_across_slices_enabled_flag
            && (slice_sao_luma_flag
                || slice_sao_chroma_flag
                || !slice_deblocking_filter_disabled_flag)
        {
            reader.read_bit()? != 0
        } else {
            pps.pps_loop_filter_across_slices_enabled_flag
        };

        // Entry point offsets (tiles/WPP)
        let mut entry_point_offsets = Vec::new();
        let num_entry_point_offsets =
            if pps.tiles_enabled_flag || pps.entropy_coding_sync_enabled_flag {
                let n = reader.read_ue()?;
                if n > 0 {
                    let offset_len = reader.read_ue()? as u8 + 1;
                    for _ in 0..n {
                        let offset = reader.read_bits(offset_len)? + 1; // offset_minus1 + 1
                        entry_point_offsets.push(offset);
                    }
                }
                n
            } else {
                0
            };

        // Skip slice segment header extension
        if pps.slice_segment_header_extension_present_flag {
            let ext_len = reader.read_ue()?;
            for _ in 0..ext_len {
                reader.read_bits(8)?;
            }
        }

        // Byte alignment
        let _alignment_bit = reader.read_bit()?; // alignment_bit_equal_to_one (should be 1)
        reader.byte_align();

        // Get the byte offset where slice data begins
        let data_offset = reader.byte_position();

        // Calculate derived values
        let slice_qp_y = 26 + pps.init_qp_minus26 as i32 + slice_qp_delta as i32;

        Ok(SliceParseResult {
            header: SliceHeader {
                first_slice_segment_in_pic_flag,
                no_output_of_prior_pics_flag,
                pps_id,
                dependent_slice_segment_flag,
                slice_segment_address,
                slice_type,
                pic_output_flag,
                colour_plane_id,
                slice_pic_order_cnt_lsb,
                slice_sao_luma_flag,
                slice_sao_chroma_flag,
                slice_qp_delta,
                slice_cb_qp_offset,
                slice_cr_qp_offset,
                cu_chroma_qp_offset_enabled_flag,
                deblocking_filter_override_flag,
                slice_deblocking_filter_disabled_flag,
                slice_beta_offset_div2,
                slice_tc_offset_div2,
                slice_loop_filter_across_slices_enabled_flag,
                num_entry_point_offsets,
                entry_point_offsets,
                short_term_ref_pic_set_idx,
                inline_short_term_rps,
                slice_temporal_mvp_enabled_flag,
                num_ref_idx_l0_active,
                num_ref_idx_l1_active,
                mvd_l1_zero_flag,
                cabac_init_flag,
                collocated_from_l0_flag,
                collocated_ref_idx,
                max_num_merge_cand,
                ref_pic_list_modification,
                ref_pic_list_modification_flag,
                pred_weight_table,
                slice_qp_y,
            },
            data_offset,
        })
    }
}

/// Count the total number of reference pictures used by the current picture (NumPocTotalCurr).
/// Used to determine ref_pic_list_modification entry bit width.
fn count_curr_pics(sps: &Sps, rps_idx: u8, inline_rps: &Option<refpic::ShortTermRefPicSet>) -> u32 {
    let rps = if let Some(rps) = inline_rps {
        rps
    } else if (rps_idx as usize) < sps.short_term_rps.len() {
        &sps.short_term_rps[rps_idx as usize]
    } else {
        return 0;
    };

    let mut count = 0u32;
    for i in 0..rps.num_negative_pics as usize {
        if rps.used_by_curr_pic_s0[i] {
            count += 1;
        }
    }
    for i in 0..rps.num_positive_pics as usize {
        if rps.used_by_curr_pic_s1[i] {
            count += 1;
        }
    }
    count
}

/// Parse prediction weight table (H.265 7.3.6.3)
fn parse_pred_weight_table(
    reader: &mut BitstreamReader<'_>,
    sps: &Sps,
    slice_type: SliceType,
    num_ref_l0: u8,
    num_ref_l1: u8,
) -> Result<super::inter::PredWeightTable> {
    // Defense-in-depth: ensure indices fit in weight table arrays
    if num_ref_l0 as usize > super::inter::MAX_NUM_REF_PICS
        || num_ref_l1 as usize > super::inter::MAX_NUM_REF_PICS
    {
        return Err(HevcError::InvalidBitstream(
            "num_ref_idx exceeds MAX_NUM_REF_PICS",
        ));
    }
    let luma_log2_weight_denom = reader.read_ue()? as u8;
    let chroma_log2_weight_denom = if sps.chroma_array_type() != 0 {
        let delta = reader.read_se()?;
        (luma_log2_weight_denom as i32 + delta).clamp(0, 7) as u8
    } else {
        0
    };
    let mut wt = super::inter::PredWeightTable {
        luma_log2_weight_denom,
        chroma_log2_weight_denom,
        ..super::inter::PredWeightTable::default()
    };

    // L0 flags
    for i in 0..num_ref_l0 as usize {
        wt.luma_weight_flag[0][i] = reader.read_bit()? != 0;
    }
    if sps.chroma_array_type() != 0 {
        for i in 0..num_ref_l0 as usize {
            wt.chroma_weight_flag[0][i] = reader.read_bit()? != 0;
        }
    }

    // L0 weights/offsets
    let luma_denom = 1i16 << wt.luma_log2_weight_denom;
    let chroma_denom = 1i16 << wt.chroma_log2_weight_denom;
    for i in 0..num_ref_l0 as usize {
        if wt.luma_weight_flag[0][i] {
            let delta = reader.read_se()? as i16;
            wt.luma_weight[0][i] = luma_denom + delta;
            wt.luma_offset[0][i] = reader.read_se()? as i16;
        } else {
            wt.luma_weight[0][i] = luma_denom;
            wt.luma_offset[0][i] = 0;
        }
        if wt.chroma_weight_flag[0][i] {
            for j in 0..2 {
                let delta = reader.read_se()? as i16;
                wt.chroma_weight[0][i][j] = chroma_denom + delta;
                let offset = reader.read_se()? as i16;
                // H.265 eq: ChromaOffset = Clip3(-128, 127, offset - ((128*w + 2^(wd-1)) >> wd) + 128)
                let wp_offset = offset
                    - ((128 * wt.chroma_weight[0][i][j] as i32
                        + (1 << (wt.chroma_log2_weight_denom - 1))) as i16
                        >> wt.chroma_log2_weight_denom)
                    + 128;
                wt.chroma_offset[0][i][j] = wp_offset.clamp(-128, 127);
            }
        } else {
            for j in 0..2 {
                wt.chroma_weight[0][i][j] = chroma_denom;
                wt.chroma_offset[0][i][j] = 0;
            }
        }
    }

    // L1 weights/offsets (B-slices only)
    if slice_type == SliceType::B {
        // L1 flags
        for i in 0..num_ref_l1 as usize {
            wt.luma_weight_flag[1][i] = reader.read_bit()? != 0;
        }
        if sps.chroma_array_type() != 0 {
            for i in 0..num_ref_l1 as usize {
                wt.chroma_weight_flag[1][i] = reader.read_bit()? != 0;
            }
        }

        for i in 0..num_ref_l1 as usize {
            if wt.luma_weight_flag[1][i] {
                let delta = reader.read_se()? as i16;
                wt.luma_weight[1][i] = luma_denom + delta;
                wt.luma_offset[1][i] = reader.read_se()? as i16;
            } else {
                wt.luma_weight[1][i] = luma_denom;
                wt.luma_offset[1][i] = 0;
            }
            if wt.chroma_weight_flag[1][i] {
                for j in 0..2 {
                    let delta = reader.read_se()? as i16;
                    wt.chroma_weight[1][i][j] = chroma_denom + delta;
                    let offset = reader.read_se()? as i16;
                    let wp_offset = offset
                        - ((128 * wt.chroma_weight[1][i][j] as i32
                            + (1 << (wt.chroma_log2_weight_denom - 1)))
                            as i16
                            >> wt.chroma_log2_weight_denom)
                        + 128;
                    wt.chroma_offset[1][i][j] = wp_offset.clamp(-128, 127);
                }
            } else {
                for j in 0..2 {
                    wt.chroma_weight[1][i][j] = chroma_denom;
                    wt.chroma_offset[1][i][j] = 0;
                }
            }
        }
    }

    Ok(wt)
}

/// Calculate ceil(log2(x))
fn ceil_log2(x: u32) -> u8 {
    if x <= 1 {
        0
    } else {
        32 - (x - 1).leading_zeros() as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ceil_log2() {
        assert_eq!(ceil_log2(1), 0);
        assert_eq!(ceil_log2(2), 1);
        assert_eq!(ceil_log2(3), 2);
        assert_eq!(ceil_log2(4), 2);
        assert_eq!(ceil_log2(5), 3);
        assert_eq!(ceil_log2(8), 3);
        assert_eq!(ceil_log2(9), 4);
    }

    #[test]
    fn test_intra_pred_mode() {
        assert_eq!(IntraPredMode::from_u8(0), Some(IntraPredMode::Planar));
        assert_eq!(IntraPredMode::from_u8(1), Some(IntraPredMode::Dc));
        assert_eq!(IntraPredMode::from_u8(26), Some(IntraPredMode::Angular26));
        assert_eq!(IntraPredMode::from_u8(34), Some(IntraPredMode::Angular34));
        assert_eq!(IntraPredMode::from_u8(35), None);
    }
}
