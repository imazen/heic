//! CTU (Coding Tree Unit) and CU (Coding Unit) decoding
//!
//! This module handles the hierarchical quad-tree structure of HEVC:
//! - CTU: Coding Tree Unit (largest block, typically 64x64)
//! - CU: Coding Unit (result of quad-tree split, 8x8 to 64x64)
//! - PU: Prediction Unit (for motion/intra prediction)
//! - TU: Transform Unit (for residual coding)

use alloc::vec::Vec;

use super::cabac::{CabacDecoder, ContextModel, INIT_VALUES, context};
use super::debug;
use super::deblock::DeblockMetadata;
use super::intra::{self, ReconstructionMap};
use super::params::{Pps, Sps};
use super::picture::DecodedFrame;
use super::residual::{self, ScanOrder};
use super::slice::{IntraPredMode, PartMode, PredMode, SliceHeader};
use super::transform;
use crate::error::HevcError;

type Result<T> = core::result::Result<T, HevcError>;

/// SAO (Sample Adaptive Offset) type
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaoType {
    /// SAO disabled for this component
    None,
    /// Band offset: offsets applied based on sample value bands
    Band,
    /// Edge offset: offsets applied based on edge direction
    Edge,
}

/// Edge offset class (direction for edge comparison)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaoEoClass {
    Horizontal = 0,   // 0°
    Vertical = 1,     // 90°
    Diagonal135 = 2,  // 135°
    Diagonal45 = 3,   // 45°
}

impl SaoEoClass {
    fn from_bits(bits: u32) -> Self {
        match bits {
            0 => SaoEoClass::Horizontal,
            1 => SaoEoClass::Vertical,
            2 => SaoEoClass::Diagonal135,
            _ => SaoEoClass::Diagonal45,
        }
    }
}

/// SAO parameters for a single component
#[derive(Clone, Debug)]
pub struct SaoComponentParams {
    /// SAO type: None, Band, or Edge
    pub sao_type: SaoType,
    /// 4 offset values (can be positive or negative)
    pub offsets: [i32; 4],
    /// For band offset: starting band position (0-31)
    pub band_position: u8,
    /// For edge offset: edge class (direction)
    pub eo_class: SaoEoClass,
}

impl Default for SaoComponentParams {
    fn default() -> Self {
        Self {
            sao_type: SaoType::None,
            offsets: [0; 4],
            band_position: 0,
            eo_class: SaoEoClass::Horizontal,
        }
    }
}

/// SAO parameters for a CTU (all 3 components)
#[derive(Clone, Debug, Default)]
pub struct SaoParams {
    pub luma: SaoComponentParams,
    pub cb: SaoComponentParams,
    pub cr: SaoComponentParams,
}

/// Chroma QP mapping table (H.265 Table 8-10)
/// Maps qPi (0-57) to QpC for 8-bit video
#[inline]
fn chroma_qp_mapping(qp_i: i32) -> i32 {
    // Table 8-10: qPi to QpC mapping
    // For qPi 0-29, QpC = qPi
    // For qPi 30-57, QpC follows the table
    static CHROMA_QP_TABLE: [i32; 58] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
        25, 26, 27, 28, 29, 29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37, 38, 39, 40, 41,
        42, 43, 44, 45, 46, 47, 48, 49, 50, 51,
    ];
    CHROMA_QP_TABLE[qp_i.clamp(0, 57) as usize]
}

/// Decoding context for a slice
pub struct SliceContext<'a> {
    /// Sequence parameter set
    pub sps: &'a Sps,
    /// Picture parameter set
    pub pps: &'a Pps,
    /// Slice header
    pub header: &'a SliceHeader,
    /// CABAC decoder
    pub cabac: CabacDecoder<'a>,
    /// Context models
    pub ctx: [ContextModel; context::NUM_CONTEXTS],
    /// Current CTB X position (in CTB units)
    pub ctb_x: u32,
    /// Current CTB Y position (in CTB units)
    pub ctb_y: u32,
    /// Current luma QP value
    pub qp_y: i32,
    /// Current Cb QP value
    pub qp_cb: i32,
    /// Current Cr QP value
    pub qp_cr: i32,
    /// Is CU QP delta coded flag
    pub is_cu_qp_delta_coded: bool,
    /// CU QP delta value
    pub cu_qp_delta: i32,
    /// CU transquant bypass flag
    pub cu_transquant_bypass_flag: bool,
    /// CT depth map for split_cu_flag context derivation (indexed by min_cb_size grid)
    ct_depth_map: Vec<u8>,
    /// Width of ct_depth_map in min_cb_size units
    ct_depth_map_stride: u32,
    /// Per-4x4-block intra prediction mode for luma (for scan order determination)
    intra_pred_mode_y: Vec<u8>,
    /// Per-4x4-block intra prediction mode for chroma (for scan order determination)
    intra_pred_mode_c: Vec<u8>,
    /// Stride for intra_pred_mode arrays in 4x4 block units
    intra_pred_stride: u32,
    /// Full slice data (needed for WPP row transitions)
    slice_data: &'a [u8],
    /// Saved context models per CTB row for WPP (saved after 2nd CTU in each row)
    wpp_saved_ctx: Vec<[ContextModel; context::NUM_CONTEXTS]>,
    /// Reconstruction map tracking which samples have been decoded (for intra prediction availability)
    reco_map: ReconstructionMap,
    /// SAO parameters per CTU, indexed by (ctb_y * ctbs_per_row + ctb_x)
    sao_params: Vec<SaoParams>,
    /// Number of CTBs per row for SAO indexing
    ctbs_per_row: u32,
    /// Per-position QP_Y map for QP prediction (indexed by min_tb_size grid)
    qp_y_map: Vec<i32>,
    /// Stride for QP map (in min_tb_size units)
    qp_y_map_stride: u32,
    /// QP from the previous quantization group (for fallback in QP prediction)
    last_qpy_in_previous_qg: i32,
    /// Current quantization group X position (top-left pixel)
    current_qg_x: i32,
    /// Current quantization group Y position (top-left pixel)
    current_qg_y: i32,
    /// Current CTB address in tile scan (for same-CTB neighbor check)
    ctb_addr_in_ts: u32,
    /// Metadata for deblocking filter
    deblock_metadata: DeblockMetadata,
}

impl<'a> SliceContext<'a> {
    /// Create a new slice context
    pub fn new(
        sps: &'a Sps,
        pps: &'a Pps,
        header: &'a SliceHeader,
        slice_data: &'a [u8],
    ) -> Result<Self> {
        let cabac = CabacDecoder::new(slice_data)?;

        // Initialize context models
        let mut ctx = [ContextModel::new(154); context::NUM_CONTEXTS];
        let slice_qp = header.slice_qp_y;

        for (i, init_val) in INIT_VALUES.iter().enumerate() {
            ctx[i].init(*init_val, slice_qp);
        }

        // Calculate chroma QP values (H.265 Table 8-10 and section 8.6.1)
        // qPi_Cb = qP_Y + pps_cb_qp_offset + slice_cb_qp_offset
        // qPi_Cr = qP_Y + pps_cr_qp_offset + slice_cr_qp_offset
        let qp_i_cb = slice_qp + pps.pps_cb_qp_offset as i32 + header.slice_cb_qp_offset as i32;
        let qp_i_cr = slice_qp + pps.pps_cr_qp_offset as i32 + header.slice_cr_qp_offset as i32;

        // Apply chroma QP mapping table (H.265 Table 8-10)
        let qp_cb = chroma_qp_mapping(qp_i_cb.clamp(0, 57));
        let qp_cr = chroma_qp_mapping(qp_i_cr.clamp(0, 57));

        // Initialize ct_depth_map for split_cu_flag context derivation
        // Map is in units of min_cb_size (typically 8x8)
        let min_cb_size = 1u32 << sps.log2_min_cb_size();
        let ct_depth_map_stride = sps.pic_width_in_luma_samples.div_ceil(min_cb_size);
        let ct_depth_map_height = sps.pic_height_in_luma_samples.div_ceil(min_cb_size);
        let ct_depth_map = vec![0xFF; (ct_depth_map_stride * ct_depth_map_height) as usize];

        // Initialize per-4x4-block intra prediction mode map
        let intra_pred_stride = sps.pic_width_in_luma_samples.div_ceil(4);
        let intra_pred_height = sps.pic_height_in_luma_samples.div_ceil(4);
        let intra_pred_mode_y = vec![0u8; (intra_pred_stride * intra_pred_height) as usize];
        let intra_pred_mode_c = vec![0u8; (intra_pred_stride * intra_pred_height) as usize];

        // Initialize SAO parameters storage
        let ctb_size = sps.ctb_size();
        let ctbs_per_row = sps.pic_width_in_luma_samples.div_ceil(ctb_size);
        let ctbs_per_col = sps.pic_height_in_luma_samples.div_ceil(ctb_size);
        let sao_params = vec![SaoParams::default(); (ctbs_per_row * ctbs_per_col) as usize];

        // Initialize QP map (at min_tb_size granularity)
        let min_tb_size = 1u32 << sps.log2_min_tb_size();
        let qp_y_map_stride = sps.pic_width_in_luma_samples.div_ceil(min_tb_size);
        let qp_y_map_height = sps.pic_height_in_luma_samples.div_ceil(min_tb_size);
        let qp_y_map = vec![slice_qp; (qp_y_map_stride * qp_y_map_height) as usize];

        Ok(Self {
            sps,
            pps,
            header,
            cabac,
            ctx,
            ctb_x: 0,
            ctb_y: 0,
            qp_y: slice_qp,
            qp_cb,
            qp_cr,
            is_cu_qp_delta_coded: false,
            cu_qp_delta: 0,
            cu_transquant_bypass_flag: false,
            ct_depth_map,
            ct_depth_map_stride,
            intra_pred_mode_y,
            intra_pred_mode_c,
            intra_pred_stride,
            slice_data,
            wpp_saved_ctx: Vec::new(),
            reco_map: ReconstructionMap::new(
                sps.pic_width_in_luma_samples,
                sps.pic_height_in_luma_samples,
            ),
            sao_params,
            ctbs_per_row,
            qp_y_map,
            qp_y_map_stride,
            last_qpy_in_previous_qg: slice_qp,
            current_qg_x: -1,
            current_qg_y: -1,
            ctb_addr_in_ts: 0,
            deblock_metadata: DeblockMetadata::new(
                sps.pic_width_in_luma_samples,
                sps.pic_height_in_luma_samples,
            ),
        })
    }

    /// Store intra prediction mode for luma at a given position covering size×size pixels
    fn set_intra_pred_mode(&mut self, x: u32, y: u32, size: u32, mode: IntraPredMode) {
        let blocks = (size / 4).max(1);
        let bx = x / 4;
        let by = y / 4;
        for dy in 0..blocks {
            for dx in 0..blocks {
                let idx = ((by + dy) * self.intra_pred_stride + bx + dx) as usize;
                if idx < self.intra_pred_mode_y.len() {
                    self.intra_pred_mode_y[idx] = mode.as_u8();
                }
            }
        }
    }

    /// Get stored intra prediction mode for luma at a given position
    fn get_intra_pred_mode(&self, x: u32, y: u32) -> IntraPredMode {
        let idx = ((y / 4) * self.intra_pred_stride + x / 4) as usize;
        if idx < self.intra_pred_mode_y.len() {
            IntraPredMode::from_u8(self.intra_pred_mode_y[idx]).unwrap_or(IntraPredMode::Planar)
        } else {
            IntraPredMode::Planar
        }
    }

    /// Store intra prediction mode for chroma at a given position (luma coordinates)
    fn set_intra_pred_mode_c(&mut self, x: u32, y: u32, size: u32, mode: IntraPredMode) {
        let blocks = (size / 4).max(1);
        let bx = x / 4;
        let by = y / 4;
        for dy in 0..blocks {
            for dx in 0..blocks {
                let idx = ((by + dy) * self.intra_pred_stride + bx + dx) as usize;
                if idx < self.intra_pred_mode_c.len() {
                    self.intra_pred_mode_c[idx] = mode.as_u8();
                }
            }
        }
    }

    /// Get stored intra prediction mode for chroma at a given luma position
    fn get_intra_pred_mode_c(&self, x: u32, y: u32) -> IntraPredMode {
        let idx = ((y / 4) * self.intra_pred_stride + x / 4) as usize;
        if idx < self.intra_pred_mode_c.len() {
            IntraPredMode::from_u8(self.intra_pred_mode_c[idx]).unwrap_or(IntraPredMode::Planar)
        } else {
            IntraPredMode::Planar
        }
    }

    /// Store QP_Y for a CU covering size×size pixels (at min_tb_size granularity)
    fn set_qpy(&mut self, x: u32, y: u32, size: u32, qp_y: i32) {
        let min_tb_size = 1u32 << self.sps.log2_min_tb_size();
        let blocks = (size / min_tb_size).max(1);
        let bx = x / min_tb_size;
        let by = y / min_tb_size;
        for dy in 0..blocks {
            for dx in 0..blocks {
                let idx = ((by + dy) * self.qp_y_map_stride + bx + dx) as usize;
                if idx < self.qp_y_map.len() {
                    self.qp_y_map[idx] = qp_y;
                }
            }
        }
    }

    /// Get QP_Y at a specific pixel position (from the QP map)
    fn get_qpy(&self, x: u32, y: u32) -> i32 {
        let min_tb_size = 1u32 << self.sps.log2_min_tb_size();
        let idx = ((y / min_tb_size) * self.qp_y_map_stride + x / min_tb_size) as usize;
        if idx < self.qp_y_map.len() {
            self.qp_y_map[idx]
        } else {
            self.header.slice_qp_y
        }
    }

    /// Derive QP_Y prediction per H.265 section 8.6.1
    /// Uses left and above neighbor QPs, averaged, with fallback to previous QG's QP
    fn derive_qp_y_pred(&mut self, x_cu_base: u32, y_cu_base: u32) -> i32 {
        let log2_min_cu_qp_delta_size = self.sps.log2_ctb_size() - self.pps.diff_cu_qp_delta_depth;
        let qg_size_mask = (1u32 << log2_min_cu_qp_delta_size) - 1;

        // Top-left pixel position of current quantization group
        let x_qg = x_cu_base & !qg_size_mask;
        let y_qg = y_cu_base & !qg_size_mask;

        // Track QG transitions: save previous QP when entering a new QG
        if x_qg as i32 != self.current_qg_x || y_qg as i32 != self.current_qg_y {
            self.last_qpy_in_previous_qg = self.qp_y;
            self.current_qg_x = x_qg as i32;
            self.current_qg_y = y_qg as i32;
        }

        // Determine if this is the first QG in a CTB row / slice / tile
        let ctb_size = self.sps.ctb_size();
        let ctb_lsb_mask = ctb_size - 1;
        let first_in_ctb_row = x_qg == 0 && (y_qg & ctb_lsb_mask) == 0;

        // For simplicity, check first QG in slice
        let first_in_slice = x_qg == 0 && y_qg == 0; // Only correct for single-slice; TODO: handle multi-slice

        let qp_y_prev = if first_in_slice || (first_in_ctb_row && self.pps.entropy_coding_sync_enabled_flag) {
            self.header.slice_qp_y
        } else {
            self.last_qpy_in_previous_qg
        };

        // Derive qPY_A (left neighbor of current QG)
        // Per libde265: check available_zscan first, then verify same CTB via MinTbAddrZS
        // If neighbor is in a different CTB, fall back to qP_Y_PREV
        let qpy_a = if x_qg > 0 {
            let left_ctb_x = (x_qg - 1) / ctb_size;
            let cur_ctb_x = x_qg / ctb_size;
            let left_ctb_y = y_qg / ctb_size;
            let cur_ctb_y = y_qg / ctb_size;
            if left_ctb_x == cur_ctb_x && left_ctb_y == cur_ctb_y {
                self.get_qpy(x_qg - 1, y_qg)
            } else {
                qp_y_prev
            }
        } else {
            qp_y_prev
        };

        // Derive qPY_B (above neighbor of current QG)
        let qpy_b = if y_qg > 0 {
            let above_ctb_x = x_qg / ctb_size;
            let cur_ctb_x = x_qg / ctb_size;
            let above_ctb_y = (y_qg - 1) / ctb_size;
            let cur_ctb_y = y_qg / ctb_size;
            if above_ctb_x == cur_ctb_x && above_ctb_y == cur_ctb_y {
                self.get_qpy(x_qg, y_qg - 1)
            } else {
                qp_y_prev
            }
        } else {
            qp_y_prev
        };

        // Average left and above
        let qp_y_pred = (qpy_a + qpy_b + 1) >> 1;

        qp_y_pred
    }

    /// Decode all CTUs in the slice
    pub fn decode_slice(&mut self, frame: &mut DecodedFrame) -> Result<(DeblockMetadata, Vec<SaoParams>)> {
        // Initialize CABAC tracker for debugging
        debug::init_tracker();

        let ctb_size = self.sps.ctb_size();
        let pic_width_in_ctbs = self.sps.pic_width_in_ctbs();
        let pic_height_in_ctbs = self.sps.pic_height_in_ctbs();
        let wpp_enabled = self.pps.entropy_coding_sync_enabled_flag;

        // Start from slice segment address
        let start_addr = self.header.slice_segment_address;
        self.ctb_y = start_addr / pic_width_in_ctbs;
        self.ctb_x = start_addr % pic_width_in_ctbs;

        let mut ctu_count = 0u32;
        let _total_ctus = pic_width_in_ctbs * pic_height_in_ctbs;

        // For WPP, track which entry point offset to use next (0-indexed into entry_point_offsets)
        let mut wpp_entry_idx = 0usize;

        loop {
            // Decode one CTU
            let x_ctb = self.ctb_x * ctb_size;
            let y_ctb = self.ctb_y * ctb_size;

            // Set CTB address for same-CTB neighbor check in QP prediction
            self.ctb_addr_in_ts = ctu_count;

            self.decode_ctu(x_ctb, y_ctb, frame)?;
            ctu_count += 1;

            // WPP: save context models after the 2nd CTU in each row (ctb_x == 1)
            // These will be restored when starting the next row
            if wpp_enabled && self.ctb_x == 1 {
                let row = self.ctb_y as usize;
                if self.wpp_saved_ctx.len() <= row {
                    self.wpp_saved_ctx.resize(row + 1, [ContextModel::new(154); context::NUM_CONTEXTS]);
                }
                self.wpp_saved_ctx[row] = self.ctx;
            }

            // Check for end of slice segment (end_of_sub_stream_one_bit for WPP)
            let end_of_slice = self.cabac.decode_terminate()?;

            // Move to next CTB
            self.ctb_x += 1;
            let row_complete = self.ctb_x >= pic_width_in_ctbs;

            if row_complete {
                // Row complete
                if wpp_enabled && wpp_entry_idx < self.header.entry_point_offsets.len() {
                    // WPP row transition: reinitialize CABAC from the next substream
                    let offset = self.header.entry_point_offsets[wpp_entry_idx] as usize;
                    wpp_entry_idx += 1;

                    if offset < self.slice_data.len() {
                        self.cabac = CabacDecoder::new(&self.slice_data[offset..])?;

                        // Restore context models saved after 2nd CTU of previous row
                        let prev_row = self.ctb_y as usize;
                        if let Some(saved) = self.wpp_saved_ctx.get(prev_row) {
                            self.ctx = *saved;
                        }

                    } else {
                        break;
                    }
                } else if end_of_slice != 0 {
                    break;
                }

                self.ctb_x = 0;
                self.ctb_y += 1;
            } else if end_of_slice != 0 {
                break;
            }

            // Check for end of picture
            if self.ctb_y >= pic_height_in_ctbs {
                break;
            }
        }

        // Print CABAC tracker summary
        debug::print_tracker_summary();

        // Return metadata for deblocking filter and SAO params
        let deblock_meta = core::mem::replace(
            &mut self.deblock_metadata,
            DeblockMetadata::new(self.sps.pic_width_in_luma_samples, self.sps.pic_height_in_luma_samples),
        );
        let sao = core::mem::take(&mut self.sao_params);
        Ok((deblock_meta, sao))
    }

    /// Decode a single CTU (Coding Tree Unit)
    fn decode_ctu(&mut self, x_ctb: u32, y_ctb: u32, frame: &mut DecodedFrame) -> Result<()> {
        let log2_ctb_size = self.sps.log2_ctb_size();

        // Reset per-CTU state
        if self.pps.cu_qp_delta_enabled_flag {
            self.is_cu_qp_delta_coded = false;
            self.cu_qp_delta = 0;
        }

        // Decode SAO parameters before coding quadtree
        if self.sps.sample_adaptive_offset_enabled_flag {
            self.decode_sao(x_ctb, y_ctb)?;
        }

        // Decode the coding quadtree
        self.decode_coding_quadtree(x_ctb, y_ctb, log2_ctb_size, 0, frame)?;

        // NOTE: SAO filtering is applied as a post-processing step after all CTUs
        // are decoded and deblocking is applied (see mod.rs). SAO parameters are
        // parsed here but application is deferred per H.265 section 8.7.1.

        Ok(())
    }

    /// Decode SAO (Sample Adaptive Offset) parameters for a CTU
    /// Per H.265 7.3.8.3: sao_merge_left_flag/sao_merge_up_flag only present when neighbor exists
    /// Note: x_ctb and y_ctb are in PIXEL coordinates (CTB position * CTB size)
    fn decode_sao(&mut self, x_ctb: u32, y_ctb: u32) -> Result<()> {
        let ctb_size = self.sps.ctb_size();
        let ctb_x_idx = x_ctb / ctb_size;
        let ctb_y_idx = y_ctb / ctb_size;
        let ctb_addr = (ctb_y_idx * self.ctbs_per_row + ctb_x_idx) as usize;

        #[cfg(feature = "trace-coefficients")]
        {
            let (r, o) = self.cabac.get_state();
            let (byte, _, _) = self.cabac.get_position();
            eprintln!("SAO: start at ({},{}) byte={} cabac=({},{})", x_ctb, y_ctb, byte, r, o);
        }

        // sao_merge_left_flag only present if leftCtbInSliceSeg is available (x > 0)
        let sao_merge_left_flag = if ctb_x_idx > 0 {
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = context::SAO_MERGE_FLAG as i32; }
            self.cabac.decode_bin(&mut self.ctx[context::SAO_MERGE_FLAG])? != 0
        } else {
            false
        };

        if sao_merge_left_flag {
            // Copy SAO params from left CTU
            let left_addr = (ctb_y_idx * self.ctbs_per_row + ctb_x_idx - 1) as usize;
            self.sao_params[ctb_addr] = self.sao_params[left_addr].clone();
            #[cfg(feature = "trace-coefficients")]
            eprintln!("SAO: merge_left from CTU at ({},{})", ctb_x_idx - 1, ctb_y_idx);
            return Ok(());
        }

        // sao_merge_up_flag only present if upCtbInSliceSeg is available (y > 0)
        let sao_merge_up_flag = if ctb_y_idx > 0 {
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = context::SAO_MERGE_FLAG as i32; }
            self.cabac.decode_bin(&mut self.ctx[context::SAO_MERGE_FLAG])? != 0
        } else {
            false
        };

        if sao_merge_up_flag {
            // Copy SAO params from upper CTU
            let up_addr = ((ctb_y_idx - 1) * self.ctbs_per_row + ctb_x_idx) as usize;
            self.sao_params[ctb_addr] = self.sao_params[up_addr].clone();
            #[cfg(feature = "trace-coefficients")]
            eprintln!("SAO: merge_up from CTU at ({},{})", ctb_x_idx, ctb_y_idx - 1);
            return Ok(());
        }

        // Decode SAO parameters for this CTU
        // Per H.265 Section 7.3.8.3:
        // - sao_type_idx_luma decoded for c_idx=0
        // - sao_type_idx_chroma decoded for c_idx=1
        // - c_idx=2 (Cr) inherits type from c_idx=1 (Cb) — NOT decoded
        // - sao_eo_class decoded only for c_idx < 2; c_idx=2 inherits from c_idx=1
        // - sao_offset_abs decoded for all 3 components if type != 0
        // - sao_offset_sign and sao_band_position only for band offset (type=1)
        let mut sao_type = [SaoType::None; 3];
        let mut offsets = [[0i32; 4]; 3];
        let mut band_position = [0u8; 3];
        let mut eo_class = [SaoEoClass::Horizontal; 3];

        for c_idx in 0..3usize {
            // Type index: only decoded for c_idx < 2
            if c_idx < 2 {
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = context::SAO_TYPE_IDX as i32; }
                let first_bin = self.cabac.decode_bin(&mut self.ctx[context::SAO_TYPE_IDX])? != 0;

                if first_bin {
                    let second_bin = self.cabac.decode_bypass()? != 0;
                    // TR binarization cMax=2: value=1 → bins "10", value=2 → bins "11"
                    // second_bin=0 → value=1 (band), second_bin=1 → value=2 (edge)
                    sao_type[c_idx] = if second_bin { SaoType::Edge } else { SaoType::Band };

                    #[cfg(feature = "trace-coefficients")]
                    eprintln!("SAO: c_idx={} type={:?}", c_idx, sao_type[c_idx]);
                }
            } else {
                // c_idx=2: inherit type from c_idx=1
                sao_type[2] = sao_type[1];
                eo_class[2] = eo_class[1]; // Also inherit eo_class

                #[cfg(feature = "trace-coefficients")]
                eprintln!("SAO: c_idx=2 inherited type={:?} from c_idx=1", sao_type[2]);
            }

            // Decode offsets if SAO is enabled for this component
            if sao_type[c_idx] != SaoType::None {
                // Decode 4 offset absolute values (truncated unary bypass coding)
                // Per H.265 Table 9-32: cMax = (1 << (Min(bitDepth, 10) - 5)) - 1
                let bit_depth = if c_idx == 0 { self.sps.bit_depth_y() } else { self.sps.bit_depth_c() };
                let max_offset_abs = ((1u32 << (bit_depth.min(10) - 5)) - 1) as u32;
                let mut offset_abs = [0u32; 4];
                for i in 0..4 {
                    let mut abs_val = 0u32;
                    // Truncated unary: at most max_offset_abs bins.
                    // If value == max_offset_abs, no trailing 0 (truncated).
                    for _ in 0..max_offset_abs {
                        let bin = self.cabac.decode_bypass()?;
                        if bin == 0 {
                            break;
                        }
                        abs_val += 1;
                    }
                    offset_abs[i] = abs_val;
                }

                if sao_type[c_idx] == SaoType::Band {
                    // Band offset: decode signs for non-zero offsets, then band_position
                    for i in 0..4 {
                        if offset_abs[i] > 0 {
                            let sign = self.cabac.decode_bypass()?;
                            // sign=1 means negative
                            offsets[c_idx][i] = if sign != 0 {
                                -(offset_abs[i] as i32)
                            } else {
                                offset_abs[i] as i32
                            };
                        } else {
                            offsets[c_idx][i] = 0;
                        }
                    }
                    band_position[c_idx] = self.cabac.decode_bypass_bits(5)? as u8;
                } else {
                    // Edge offset: signs are derived from edge category (Table 8-3)
                    // Edge categories: 0=valley (+), 1=peak (+), 2=flat_below (-), 3=flat_above (-)
                    // category 0, 1 → positive offset; category 2, 3 → negative offset
                    // But we store as positive because sign is implicit based on category
                    // Per H.265 8.7.3.2: SaoOffsetVal[i] = offset_abs[i] for category 0,1
                    //                    SaoOffsetVal[i] = -offset_abs[i] for category 2,3
                    // Store absolute values and let apply_sao handle sign based on category
                    for i in 0..4 {
                        offsets[c_idx][i] = offset_abs[i] as i32;
                    }
                    
                    // decode eo_class only for c_idx < 2 (c_idx=2 inherits from c_idx=1)
                    if c_idx < 2 {
                        eo_class[c_idx] = SaoEoClass::from_bits(self.cabac.decode_bypass_bits(2)?);
                        #[cfg(feature = "trace-coefficients")]
                        eprintln!("SAO: c_idx={} eo_class={:?}", c_idx, eo_class[c_idx]);
                    }
                }
            }
        }

        // Store the decoded SAO parameters
        self.sao_params[ctb_addr] = SaoParams {
            luma: SaoComponentParams {
                sao_type: sao_type[0],
                offsets: offsets[0],
                band_position: band_position[0],
                eo_class: eo_class[0],
            },
            cb: SaoComponentParams {
                sao_type: sao_type[1],
                offsets: offsets[1],
                band_position: band_position[1],
                eo_class: eo_class[1],
            },
            cr: SaoComponentParams {
                sao_type: sao_type[2],
                offsets: offsets[2],
                band_position: band_position[2],
                eo_class: eo_class[2],
            },
        };

        #[cfg(feature = "trace-coefficients")]
        {
            let (r, o) = self.cabac.get_state();
            let (byte, _, _) = self.cabac.get_position();
            eprintln!("SAO: end byte={} cabac=({},{})", byte, r, o);
        }

        Ok(())
    }

    /// Apply SAO (Sample Adaptive Offset) filtering to a CTU
    /// Per H.265 Section 8.7.3
    /// Note: x_ctb and y_ctb are in PIXEL coordinates (CTB position * CTB size)
    fn apply_sao(&self, x_ctb: u32, y_ctb: u32, frame: &mut DecodedFrame) {
        let ctb_size = self.sps.ctb_size();
        let ctb_x_idx = x_ctb / ctb_size;
        let ctb_y_idx = y_ctb / ctb_size;
        let ctb_addr = (ctb_y_idx * self.ctbs_per_row + ctb_x_idx) as usize;
        let params = &self.sao_params[ctb_addr];
        let pic_width = self.sps.pic_width_in_luma_samples;
        let pic_height = self.sps.pic_height_in_luma_samples;
        
        // Calculate CTU bounds in pixels (x_ctb/y_ctb already in pixels)
        let x_start = x_ctb as i32;
        let y_start = y_ctb as i32;
        let x_end = (x_ctb + ctb_size).min(pic_width) as i32;
        let y_end = (y_ctb + ctb_size).min(pic_height) as i32;
        
        // Apply SAO to luma
        if params.luma.sao_type != SaoType::None {
            self.apply_sao_component(
                &params.luma,
                x_start, y_start, x_end, y_end,
                pic_width as i32, pic_height as i32,
                0, // c_idx = 0 for luma
                frame,
            );
        }
        
        // Apply SAO to chroma (subsampled coordinates for 4:2:0)
        let chroma_x_start = x_start / 2;
        let chroma_y_start = y_start / 2;
        let chroma_x_end = x_end / 2;
        let chroma_y_end = y_end / 2;
        let chroma_pic_w = (pic_width / 2) as i32;
        let chroma_pic_h = (pic_height / 2) as i32;
        
        if params.cb.sao_type != SaoType::None {
            self.apply_sao_component(
                &params.cb,
                chroma_x_start, chroma_y_start, chroma_x_end, chroma_y_end,
                chroma_pic_w, chroma_pic_h,
                1, // c_idx = 1 for Cb
                frame,
            );
        }
        
        if params.cr.sao_type != SaoType::None {
            self.apply_sao_component(
                &params.cr,
                chroma_x_start, chroma_y_start, chroma_x_end, chroma_y_end,
                chroma_pic_w, chroma_pic_h,
                2, // c_idx = 2 for Cr
                frame,
            );
        }
    }
    
    /// Apply SAO to a single component
    fn apply_sao_component(
        &self,
        params: &SaoComponentParams,
        x_start: i32, y_start: i32, x_end: i32, y_end: i32,
        pic_w: i32, pic_h: i32,
        c_idx: u8,
        frame: &mut DecodedFrame,
    ) {
        match params.sao_type {
            SaoType::None => {},
            SaoType::Band => {
                self.apply_sao_band(params, x_start, y_start, x_end, y_end, c_idx, frame);
            },
            SaoType::Edge => {
                self.apply_sao_edge(params, x_start, y_start, x_end, y_end, pic_w, pic_h, c_idx, frame);
            },
        }
    }
    
    /// Apply band offset SAO
    /// Per H.265 8.7.3.1: Band offset assigns samples to 32 bands based on value
    fn apply_sao_band(
        &self,
        params: &SaoComponentParams,
        x_start: i32, y_start: i32, x_end: i32, y_end: i32,
        c_idx: u8,
        frame: &mut DecodedFrame,
    ) {
        let band_shift = self.sps.bit_depth_y() - 5; // For 8-bit: shift by 3 to get band (0-31)
        let band_pos = params.band_position as i32;
        
        for y in y_start..y_end {
            for x in x_start..x_end {
                let sample = match c_idx {
                    0 => frame.get_y(x as u32, y as u32) as i32,
                    1 => frame.get_cb(x as u32, y as u32) as i32,
                    _ => frame.get_cr(x as u32, y as u32) as i32,
                };
                
                // Determine band index
                let band = sample >> band_shift;
                
                // Check if band is in the 4 activated bands starting at band_position
                // The 4 activated bands are: band_pos, band_pos+1, band_pos+2, band_pos+3
                let relative_band = band - band_pos;
                if relative_band >= 0 && relative_band < 4 {
                    let offset = params.offsets[relative_band as usize];
                    let new_sample = (sample + offset).clamp(0, 255) as u16;
                    match c_idx {
                        0 => frame.set_y(x as u32, y as u32, new_sample),
                        1 => frame.set_cb(x as u32, y as u32, new_sample),
                        _ => frame.set_cr(x as u32, y as u32, new_sample),
                    }
                }
            }
        }
    }
    
    /// Apply edge offset SAO
    /// Per H.265 8.7.3.2: Edge offset compares sample to neighbors
    fn apply_sao_edge(
        &self,
        params: &SaoComponentParams,
        x_start: i32, y_start: i32, x_end: i32, y_end: i32,
        pic_w: i32, pic_h: i32,
        c_idx: u8,
        frame: &mut DecodedFrame,
    ) {
        // Edge offset direction patterns (dx1, dy1, dx2, dy2)
        // eo_class 0: horizontal (90°)  - compare to left and right
        // eo_class 1: vertical (0°)     - compare to top and bottom
        // eo_class 2: 135° diagonal     - compare to top-left and bottom-right
        // eo_class 3: 45° diagonal      - compare to top-right and bottom-left
        let (dx1, dy1, dx2, dy2) = match params.eo_class {
            SaoEoClass::Horizontal => (-1, 0, 1, 0),
            SaoEoClass::Vertical   => (0, -1, 0, 1),
            SaoEoClass::Diagonal135 => (-1, -1, 1, 1),
            SaoEoClass::Diagonal45  => (1, -1, -1, 1),
        };
        
        // Edge category to offset index mapping per H.265 Table 8-3
        // Category 0: sample < both neighbors   → offset[0] (positive)
        // Category 1: sample < one neighbor, = other → offset[1] (positive)
        // Category 2: sample = both neighbors   → no offset
        // Category 3: sample > one neighbor, = other → offset[2] (negative)
        // Category 4: sample > both neighbors   → offset[3] (negative)
        
        for y in y_start..y_end {
            for x in x_start..x_end {
                // Get neighbor positions
                let x1 = x + dx1;
                let y1 = y + dy1;
                let x2 = x + dx2;
                let y2 = y + dy2;
                
                // Skip if neighbors are outside picture bounds
                if x1 < 0 || x1 >= pic_w || y1 < 0 || y1 >= pic_h ||
                   x2 < 0 || x2 >= pic_w || y2 < 0 || y2 >= pic_h {
                    continue;
                }
                
                let sample = match c_idx {
                    0 => frame.get_y(x as u32, y as u32) as i32,
                    1 => frame.get_cb(x as u32, y as u32) as i32,
                    _ => frame.get_cr(x as u32, y as u32) as i32,
                };
                
                let neighbor1 = match c_idx {
                    0 => frame.get_y(x1 as u32, y1 as u32) as i32,
                    1 => frame.get_cb(x1 as u32, y1 as u32) as i32,
                    _ => frame.get_cr(x1 as u32, y1 as u32) as i32,
                };
                
                let neighbor2 = match c_idx {
                    0 => frame.get_y(x2 as u32, y2 as u32) as i32,
                    1 => frame.get_cb(x2 as u32, y2 as u32) as i32,
                    _ => frame.get_cr(x2 as u32, y2 as u32) as i32,
                };
                
                // Compute comparison values
                let c1 = if sample < neighbor1 { -1 } else if sample > neighbor1 { 1 } else { 0 };
                let c2 = if sample < neighbor2 { -1 } else if sample > neighbor2 { 1 } else { 0 };
                let edge_idx = 2 + c1 + c2; // Results in 0-4
                
                // Apply offset based on edge category
                let offset = match edge_idx {
                    0 => params.offsets[0],  // Valley: both neighbors higher
                    1 => params.offsets[1],  // Half valley
                    2 => 0,                   // Flat
                    3 => -params.offsets[2], // Half peak (negative offset)
                    _ => -params.offsets[3], // Peak: both neighbors lower (negative)
                };
                
                if offset != 0 {
                    let new_sample = (sample + offset).clamp(0, 255) as u16;
                    match c_idx {
                        0 => frame.set_y(x as u32, y as u32, new_sample),
                        1 => frame.set_cb(x as u32, y as u32, new_sample),
                        _ => frame.set_cr(x as u32, y as u32, new_sample),
                    }
                }
            }
        }
    }

    /// Decode coding quadtree recursively
    fn decode_coding_quadtree(
        &mut self,
        x0: u32,
        y0: u32,
        log2_cb_size: u8,
        ct_depth: u8,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        let cb_size = 1u32 << log2_cb_size;
        let pic_width = self.sps.pic_width_in_luma_samples;
        let pic_height = self.sps.pic_height_in_luma_samples;
        let log2_min_cb_size = self.sps.log2_min_cb_size();

        // Determine if we need to split
        let split_flag = if x0 + cb_size <= pic_width
            && y0 + cb_size <= pic_height
            && log2_cb_size > log2_min_cb_size
        {
            // Decode split_cu_flag
            let flag = self.decode_split_cu_flag(x0, y0, ct_depth)?;
            flag
        } else if log2_cb_size > log2_min_cb_size {
            // Must split if partially outside picture
            true
        } else {
            // At minimum size, don't split
            false
        };

        // Handle QP delta depth
        // Per H.265 spec 7.4.9.10: IsCuQpDeltaCoded = 0 when log2CbSize >= Log2MinCuQpDeltaSize
        // where Log2MinCuQpDeltaSize = CtbLog2SizeY - diff_cu_qp_delta_depth
        if self.pps.cu_qp_delta_enabled_flag
            && log2_cb_size >= self.sps.log2_ctb_size() - self.pps.diff_cu_qp_delta_depth
        {
            self.is_cu_qp_delta_coded = false;
            self.cu_qp_delta = 0;
        }

        if split_flag {
            let half = cb_size / 2;
            let x1 = x0 + half;
            let y1 = y0 + half;

            // Decode four sub-CUs
            self.decode_coding_quadtree(x0, y0, log2_cb_size - 1, ct_depth + 1, frame)?;

            if x1 < pic_width {
                self.decode_coding_quadtree(x1, y0, log2_cb_size - 1, ct_depth + 1, frame)?;
            }

            if y1 < pic_height {
                self.decode_coding_quadtree(x0, y1, log2_cb_size - 1, ct_depth + 1, frame)?;
            }

            if x1 < pic_width && y1 < pic_height {
                self.decode_coding_quadtree(x1, y1, log2_cb_size - 1, ct_depth + 1, frame)?;
            }
        } else {
            // Decode the coding unit
            self.decode_coding_unit(x0, y0, log2_cb_size, ct_depth, frame)?;
        }

        Ok(())
    }

    /// Get ctDepth at a pixel position (returns 0xFF if not yet decoded)
    fn get_ct_depth(&self, x: u32, y: u32) -> u8 {
        let min_cb_size = 1u32 << self.sps.log2_min_cb_size();
        let map_x = x / min_cb_size;
        let map_y = y / min_cb_size;

        if map_x >= self.ct_depth_map_stride
            || map_y * self.ct_depth_map_stride + map_x >= self.ct_depth_map.len() as u32
        {
            return 0xFF; // Out of bounds
        }

        self.ct_depth_map[(map_y * self.ct_depth_map_stride + map_x) as usize]
    }

    /// Set ctDepth for a CU region
    fn set_ct_depth(&mut self, x0: u32, y0: u32, log2_cb_size: u8, ct_depth: u8) {
        let min_cb_size = 1u32 << self.sps.log2_min_cb_size();
        let cb_size = 1u32 << log2_cb_size;

        // Fill the ct_depth_map for this CU region
        let start_x = x0 / min_cb_size;
        let start_y = y0 / min_cb_size;
        let num_blocks = cb_size / min_cb_size;

        for dy in 0..num_blocks {
            for dx in 0..num_blocks {
                let map_x = start_x + dx;
                let map_y = start_y + dy;
                if map_x < self.ct_depth_map_stride {
                    let idx = (map_y * self.ct_depth_map_stride + map_x) as usize;
                    if idx < self.ct_depth_map.len() {
                        self.ct_depth_map[idx] = ct_depth;
                    }
                }
            }
        }
    }

    /// Check if a neighbor position is available (within picture bounds)
    fn is_neighbor_available(&self, x: i32, y: i32) -> bool {
        x >= 0
            && y >= 0
            && (x as u32) < self.sps.pic_width_in_luma_samples
            && (y as u32) < self.sps.pic_height_in_luma_samples
    }

    /// Decode split_cu_flag using CABAC
    fn decode_split_cu_flag(&mut self, x0: u32, y0: u32, ct_depth: u8) -> Result<bool> {
        // Context selection based on neighboring CU depths (H.265 9.3.4.2.2)
        // condTermL: 1 if left neighbor has larger depth (was split more)
        // condTermA: 1 if above neighbor has larger depth
        // ctxInc = condTermL + condTermA

        let available_l = self.is_neighbor_available(x0 as i32 - 1, y0 as i32);
        let available_a = self.is_neighbor_available(x0 as i32, y0 as i32 - 1);

        let mut cond_l = 0;
        let mut cond_a = 0;

        if available_l {
            let depth_l = self.get_ct_depth(x0 - 1, y0);
            if depth_l != 0xFF && depth_l > ct_depth {
                cond_l = 1;
            }
        }

        if available_a {
            let depth_a = self.get_ct_depth(x0, y0 - 1);
            if depth_a != 0xFF && depth_a > ct_depth {
                cond_a = 1;
            }
        }

        let ctx_idx = context::SPLIT_CU_FLAG + cond_l + cond_a;
        #[cfg(feature = "trace-coefficients")]
        { self.cabac.trace_ctx_idx = ctx_idx as i32; }
        let bin = self.cabac.decode_bin(&mut self.ctx[ctx_idx])?;

        Ok(bin != 0)
    }

    /// Decode a coding unit
    fn decode_coding_unit(
        &mut self,
        x0: u32,
        y0: u32,
        log2_cb_size: u8,
        ct_depth: u8,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        let cb_size = 1u32 << log2_cb_size;
        let _ = cb_size; // Used in PartNxN

        // Set ct_depth for this CU (used by split_cu_flag context derivation)
        self.set_ct_depth(x0, y0, log2_cb_size, ct_depth);

        // For I-slices, prediction mode is always INTRA
        let pred_mode = PredMode::Intra;

        // Track prediction mode for deblocking filter
        // Mark all 4x4 blocks in this CU as intra
        for y in (0..cb_size).step_by(4) {
            for x in (0..cb_size).step_by(4) {
                self.deblock_metadata.set_pred_mode(x0 + x, y0 + y, true);
            }
        }

        // Decode transquant_bypass_flag if enabled
        self.cu_transquant_bypass_flag = if self.pps.transquant_bypass_enabled_flag {
            let ctx_idx = context::CU_TRANSQUANT_BYPASS_FLAG;
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = ctx_idx as i32; }
            self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0
        } else {
            false
        };

        // Decode partition mode
        let part_mode = if log2_cb_size == self.sps.log2_min_cb_size() {
            // At minimum size, can be 2Nx2N or NxN
            let pm = self.decode_part_mode(pred_mode, log2_cb_size)?;
            pm
        } else {
            // Larger sizes are always 2Nx2N for intra
            PartMode::Part2Nx2N
        };

        // Decode prediction info and get intra mode for scan order
        let intra_mode = match part_mode {
            PartMode::Part2Nx2N => {
                // Single PU covering entire CU
                let mode = self.decode_intra_prediction(x0, y0, log2_cb_size, true, frame)?;
                // Store intra mode for all 4x4 blocks in this CU
                self.set_intra_pred_mode(x0, y0, cb_size, mode);
                mode
            }
            PartMode::PartNxN => {
                // Four PUs (only at minimum CU size for intra)
                // For 4:2:0, all four 4x4 luma PUs share one 4x4 chroma block
                let half = cb_size / 2;
                let log2_pu_size = log2_cb_size - 1;

                // CRITICAL for CABAC sync: Per HEVC spec 7.3.8.5:
                // Loop 1: Decode ALL 4 prev_intra_luma_pred_flags first
                let ctx_idx = context::PREV_INTRA_LUMA_PRED_FLAG;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let prev_flag_0 = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let prev_flag_1 = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let prev_flag_2 = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let prev_flag_3 = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;

                // Loop 2: Decode all 4 modes based on flags
                // CRITICAL: Store each mode IMMEDIATELY after decoding so that
                // subsequent sub-PUs can use it as a neighbor for MPM derivation.
                let luma_mode_0 = self.decode_intra_mode_from_flag(prev_flag_0, x0, y0)?;
                self.set_intra_pred_mode(x0, y0, half, luma_mode_0);

                let luma_mode_1 = self.decode_intra_mode_from_flag(prev_flag_1, x0 + half, y0)?;
                self.set_intra_pred_mode(x0 + half, y0, half, luma_mode_1);

                let luma_mode_2 = self.decode_intra_mode_from_flag(prev_flag_2, x0, y0 + half)?;
                self.set_intra_pred_mode(x0, y0 + half, half, luma_mode_2);

                let luma_mode_3 = self.decode_intra_mode_from_flag(prev_flag_3, x0 + half, y0 + half)?;
                self.set_intra_pred_mode(x0 + half, y0 + half, half, luma_mode_3);

                // Decode chroma mode once (using first luma mode for derivation if mode=4)
                let chroma_mode = self.decode_intra_chroma_mode(luma_mode_0)?;

                // Store chroma mode for all blocks in this CU
                self.set_intra_pred_mode_c(x0, y0, cb_size, chroma_mode);

                // NOTE: Prediction is now applied per-TU in decode_transform_unit_leaf,
                // not at the CU level, so that each TU can use reconstructed pixels
                // from earlier TUs as reference samples.

                luma_mode_0
            }
            _ => {
                // Other partition modes not used for intra
                return Err(HevcError::InvalidBitstream("invalid intra partition mode"));
            }
        };

        // Decode rqt_root_cbf (residual quad-tree coded block flag)
        // For intra, this is always coded (not signaled, assumed 1)
        // unless transquant_bypass is enabled
        if !self.cu_transquant_bypass_flag {
            // IntraSplitFlag = 1 when part_mode is NxN (H.265 spec 7.4.9.8)
            let intra_split_flag = part_mode == PartMode::PartNxN;
            // Decode transform tree
            self.decode_transform_tree(
                x0,
                y0,
                log2_cb_size,
                0, // trafo_depth
                intra_mode,
                intra_split_flag,
                frame,
            )?;
        }

        // Per H.265 8.6.1 / libde265 decode_quantization_parameters:
        // Always derive and store QPY for every CU, not just those with coded cu_qp_delta.
        // This ensures the QP map is correct for future neighbor lookups.
        // For CUs without coded delta, QPY = qPY_PRED + 0 = qPY_PRED.
        if self.pps.cu_qp_delta_enabled_flag {
            let qp_y_pred = self.derive_qp_y_pred(x0, y0);
            let qp_bd_offset_y = 6 * (self.sps.bit_depth_y() as i32 - 8);
            self.qp_y = ((qp_y_pred + self.cu_qp_delta + 52 + 2 * qp_bd_offset_y)
                % (52 + qp_bd_offset_y))
                - qp_bd_offset_y;

            // Update chroma QP
            let qp_i_cb = self.qp_y + self.pps.pps_cb_qp_offset as i32
                + self.header.slice_cb_qp_offset as i32;
            let qp_i_cr = self.qp_y + self.pps.pps_cr_qp_offset as i32
                + self.header.slice_cr_qp_offset as i32;
            self.qp_cb = chroma_qp_mapping(qp_i_cb.clamp(0, 57));
            self.qp_cr = chroma_qp_mapping(qp_i_cr.clamp(0, 57));

            // Store QPY in the map for the CU's area
            let cb_size_cu = 1u32 << log2_cb_size;
            self.set_qpy(x0, y0, cb_size_cu, self.qp_y);
        }

        Ok(())
    }

    /// Decode transform tree recursively
    fn decode_transform_tree(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        trafo_depth: u8,
        intra_mode: IntraPredMode,
        intra_split_flag: bool,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        // For 4:2:0, start with root having chroma responsibility
        self.decode_transform_tree_inner(
            x0,
            y0,
            log2_size,
            trafo_depth,
            intra_mode,
            intra_split_flag,
            true,
            true,
            frame,
        )
    }

    /// Inner transform tree decoding
    /// cbf_cb_parent/cbf_cr_parent: whether parent says chroma has residuals (or true at root)
    #[allow(clippy::too_many_arguments)]
    fn decode_transform_tree_inner(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        trafo_depth: u8,
        intra_mode: IntraPredMode,
        intra_split_flag: bool,
        cbf_cb_parent: bool,
        cbf_cr_parent: bool,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        // H.265 spec (7.4.9.8): MaxTrafoDepth = max_transform_hierarchy_depth_intra + IntraSplitFlag
        let max_trafo_depth = self.sps.max_transform_hierarchy_depth_intra
            + if intra_split_flag { 1 } else { 0 };
        let log2_min_trafo_size = self.sps.log2_min_tb_size();
        let log2_max_trafo_size = self.sps.log2_max_tb_size();

        // Per HEVC spec 7.3.8.7, the order is:
        // 1. split_transform_flag (if applicable)
        // 2. cbf_cb (if applicable)
        // 3. cbf_cr (if applicable)

        // Debug for specific position

        // Step 1: Determine if we should split
        // H.265 spec 7.3.8.7: split_transform_flag is decoded when:
        //   log2TrafoSize <= MaxTbLog2SizeY && log2TrafoSize > MinTbLog2SizeY &&
        //   trafoDepth < MaxTrafoDepth && !(IntraSplitFlag && trafoDepth == 0)
        // When IntraSplitFlag==1 && trafoDepth==0, split is forced to true (implicit)
        let split_transform = if intra_split_flag && trafo_depth == 0 {
            // H.265 spec: IntraNxN at root level forces split (not coded)
            true
        } else if log2_size <= log2_max_trafo_size
            && log2_size > log2_min_trafo_size
            && trafo_depth < max_trafo_depth
        {
            // Decode split_transform_flag
            let ctx_idx = context::SPLIT_TRANSFORM_FLAG + (5 - log2_size as usize).min(2);
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = ctx_idx as i32; }
            let flag = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
            flag
        } else if log2_size > log2_max_trafo_size {
            true // Must split if larger than max
        } else {
            false
        };

        // Track split transform flag for deblocking filter
        if split_transform {
            let tu_size = 1u32 << log2_size;
            for y in (0..tu_size).step_by(4) {
                for x in (0..tu_size).step_by(4) {
                    self.deblock_metadata.set_split_transform(x0 + x, y0 + y, true);
                }
            }
        }

        // Step 2: Decode cbf_cb and cbf_cr
        // For 4:2:0, decode chroma cbf at this level if log2_size > 2
        // cbf_cb/cbf_cr decoded if log2_size > 2 AND (trafoDepth == 0 OR parent cbf is set)
        let (cbf_cb, cbf_cr) = if log2_size > 2 {
            // Decode cbf_cb if trafo_depth == 0 (always) or parent had cbf_cb
            let cb = if trafo_depth == 0 || cbf_cb_parent {
                let ctx_idx = context::CBF_CBCR + trafo_depth as usize;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let val = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
                val
            } else {
                false
            };
            // Decode cbf_cr if trafo_depth == 0 (always) or parent had cbf_cr
            let cr = if trafo_depth == 0 || cbf_cr_parent {
                let ctx_idx = context::CBF_CBCR + trafo_depth as usize;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx as i32; }
                let val = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
                val
            } else {
                false
            };
            (cb, cr)
        } else {
            // log2_size == 2: inherit from parent (chroma decoded at parent level)
            (cbf_cb_parent, cbf_cr_parent)
        };

        if split_transform {
            let half = 1u32 << (log2_size - 1);
            let new_depth = trafo_depth + 1;
            let new_log2_size = log2_size - 1;

            self.decode_transform_tree_inner(
                x0,
                y0,
                new_log2_size,
                new_depth,
                intra_mode,
                intra_split_flag,
                cbf_cb,
                cbf_cr,
                frame,
            )?;
            self.decode_transform_tree_inner(
                x0 + half,
                y0,
                new_log2_size,
                new_depth,
                intra_mode,
                intra_split_flag,
                cbf_cb,
                cbf_cr,
                frame,
            )?;
            self.decode_transform_tree_inner(
                x0,
                y0 + half,
                new_log2_size,
                new_depth,
                intra_mode,
                intra_split_flag,
                cbf_cb,
                cbf_cr,
                frame,
            )?;
            self.decode_transform_tree_inner(
                x0 + half,
                y0 + half,
                new_log2_size,
                new_depth,
                intra_mode,
                intra_split_flag,
                cbf_cb,
                cbf_cr,
                frame,
            )?;

            // For 4:2:0, if we split from 8x8 to 4x4, decode chroma residuals now
            // (because 4x4 children can't have chroma TUs)
            if log2_size == 3 {
                // Apply chroma prediction for the deferred chroma TU before adding residuals
                let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
                intra::predict_intra(frame, x0 / 2, y0 / 2, 2, chroma_mode, 1, &self.reco_map, self.sps.strong_intra_smoothing_enabled_flag);
                intra::predict_intra(frame, x0 / 2, y0 / 2, 2, chroma_mode, 2, &self.reco_map, self.sps.strong_intra_smoothing_enabled_flag);

                // Use stored chroma intra mode for chroma scan order
                let scan_order_cb = residual::get_scan_order(2, chroma_mode.as_u8(), 1);

                if cbf_cb {
                    self.decode_and_apply_residual(x0 / 2, y0 / 2, 2, 1, scan_order_cb, frame)?;
                }
                if cbf_cr {
                    let scan_order_cr = residual::get_scan_order(2, chroma_mode.as_u8(), 2);
                    self.decode_and_apply_residual(x0 / 2, y0 / 2, 2, 2, scan_order_cr, frame)?;
                }
                // Mark deferred chroma blocks as reconstructed
                self.reco_map.mark_reconstructed(x0 / 2, y0 / 2, 4, 1);
                self.reco_map.mark_reconstructed(x0 / 2, y0 / 2, 4, 2);
            }
        } else {
            // Decode transform unit (leaf node)
            self.decode_transform_unit_leaf(
                x0,
                y0,
                log2_size,
                trafo_depth,
                intra_mode,
                cbf_cb,
                cbf_cr,
                frame,
            )?;
        }

        Ok(())
    }

    /// Decode transform unit at leaf node
    #[allow(clippy::too_many_arguments)]
    fn decode_transform_unit_leaf(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        trafo_depth: u8,
        intra_mode: IntraPredMode,
        cbf_cb: bool,
        cbf_cr: bool,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        let actual_luma_mode = self.get_intra_pred_mode(x0, y0);
        intra::predict_intra(frame, x0, y0, log2_size, actual_luma_mode, 0, &self.reco_map, self.sps.strong_intra_smoothing_enabled_flag);

        // Apply chroma prediction if this TU handles chroma (log2_size >= 3)
        if log2_size >= 3 {
            let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
            let chroma_x = x0 / 2;
            let chroma_y = y0 / 2;
            let chroma_log2_size = log2_size - 1;
            intra::predict_intra(frame, chroma_x, chroma_y, chroma_log2_size, chroma_mode, 1, &self.reco_map, self.sps.strong_intra_smoothing_enabled_flag);
            intra::predict_intra(frame, chroma_x, chroma_y, chroma_log2_size, chroma_mode, 2, &self.reco_map, self.sps.strong_intra_smoothing_enabled_flag);
        }

        // Decode cbf_luma - Per H.265 spec 7.3.8.8:
        // Condition: CuPredMode == MODE_INTRA || trafoDepth != 0 || cbf_cb || cbf_cr
        // For I-slice, CuPredMode is always INTRA, so cbf_luma is ALWAYS decoded
        let is_intra = true; // I-slice: always intra
        let cbf_luma = if is_intra || trafo_depth != 0 || cbf_cb || cbf_cr {
            // Context: offset 0 if trafo_depth > 0, offset 1 if trafo_depth == 0
            let ctx_offset = if trafo_depth == 0 { 1 } else { 0 };
            let ctx_idx = context::CBF_LUMA + ctx_offset;
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = ctx_idx as i32; }
            let val = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;
            val
        } else {
            true // Implicitly 1 when trafo_depth > 0 and no chroma cbf
        };

        // Decode and apply luma residuals
        if cbf_luma || cbf_cb || cbf_cr {
            // H.265 spec 7.3.8.11: Decode cu_qp_delta_abs BEFORE residual data
            // when cu_qp_delta_enabled_flag is set and not yet coded for this CU
            if self.pps.cu_qp_delta_enabled_flag && !self.is_cu_qp_delta_coded {
                // cu_qp_delta_abs: TU(5) binarization with context-coded prefix
                // First bin: CU_QP_DELTA_ABS + 0, subsequent bins: CU_QP_DELTA_ABS + 1
                let mut cu_qp_delta_abs: i32 = 0;
                let ctx_idx0 = context::CU_QP_DELTA_ABS;
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = ctx_idx0 as i32; }
                let first_bin = self.cabac.decode_bin(&mut self.ctx[ctx_idx0])?;
                if first_bin != 0 {
                    cu_qp_delta_abs = 1;
                    let ctx_idx1 = context::CU_QP_DELTA_ABS + 1;
                    // Decode up to 4 more unary bins (max prefix = 5)
                    for _ in 0..4 {
                        #[cfg(feature = "trace-coefficients")]
                        { self.cabac.trace_ctx_idx = ctx_idx1 as i32; }
                        let bin = self.cabac.decode_bin(&mut self.ctx[ctx_idx1])?;
                        if bin == 0 {
                            break;
                        }
                        cu_qp_delta_abs += 1;
                    }
                    // If prefix == 5, decode exp-Golomb suffix (bypass)
                    if cu_qp_delta_abs >= 5 {
                        let mut k = 0u32;
                        loop {
                            let bin = self.cabac.decode_bypass()?;
                            if bin == 0 {
                                break;
                            }
                            k += 1;
                        }
                        let mut val = 0i32;
                        for _ in 0..k {
                            val = (val << 1) | self.cabac.decode_bypass()? as i32;
                        }
                        cu_qp_delta_abs += val + (1 << k) - 1;
                    }
                }
                if cu_qp_delta_abs > 0 {
                    // Decode sign flag (bypass)
                    let sign = self.cabac.decode_bypass()?;
                    self.cu_qp_delta = if sign != 0 { -cu_qp_delta_abs } else { cu_qp_delta_abs };
                } else {
                    self.cu_qp_delta = 0;
                }
                self.is_cu_qp_delta_coded = true;

                // Derive QP predictor per H.265 section 8.6.1
                // This uses left/above neighbor averaging instead of simple carry-forward
                let qp_y_pred = self.derive_qp_y_pred(x0, y0);

                // Apply QP delta per H.265 section 8.6.1
                // QP_Y = ((qP_Y_PRED + CuQpDeltaVal + 52 + 2*QpBdOffsetY) % (52 + QpBdOffsetY)) - QpBdOffsetY
                let qp_bd_offset_y = 6 * (self.sps.bit_depth_y() as i32 - 8);
                self.qp_y = ((qp_y_pred + self.cu_qp_delta + 52 + 2 * qp_bd_offset_y)
                    % (52 + qp_bd_offset_y))
                    - qp_bd_offset_y;

                // NOTE: Do NOT store QP into the map here.
                // Site B (end of decode_coding_unit) stores QP per-CU with the correct
                // CU base and size. Storing over the full QG area here would overwrite
                // QP values of already-processed CUs within this QG, corrupting future
                // neighbor lookups (H.265 §8.6.1 qP_Y_A / qP_Y_B).

                // Update chroma QP values
                let qp_i_cb = self.qp_y + self.pps.pps_cb_qp_offset as i32
                    + self.header.slice_cb_qp_offset as i32;
                let qp_i_cr = self.qp_y + self.pps.pps_cr_qp_offset as i32
                    + self.header.slice_cr_qp_offset as i32;
                self.qp_cb = chroma_qp_mapping(qp_i_cb.clamp(0, 57));
                self.qp_cr = chroma_qp_mapping(qp_i_cr.clamp(0, 57));

            }
        }

        // Decode and apply luma residuals
        if cbf_luma {
            // Use per-position intra mode for scan order (critical for NxN partitions
            // where each sub-TU has a different intra prediction mode)
            let actual_mode = self.get_intra_pred_mode(x0, y0);
            let scan_order = residual::get_scan_order(log2_size, actual_mode.as_u8(), 0);
            self.decode_and_apply_residual(x0, y0, log2_size, 0, scan_order, frame)?;
        }

        // Decode chroma residuals if not handled by parent (log2_size >= 3)
        if log2_size >= 3 {
            let chroma_log2_size = log2_size - 1;
            if cbf_cb {
                let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
                let scan_order = residual::get_scan_order(chroma_log2_size, chroma_mode.as_u8(), 1);
                self.decode_and_apply_residual(
                    x0 / 2,
                    y0 / 2,
                    chroma_log2_size,
                    1,
                    scan_order,
                    frame,
                )?;
            }
            if cbf_cr {
                let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
                let scan_order = residual::get_scan_order(chroma_log2_size, chroma_mode.as_u8(), 2);
                self.decode_and_apply_residual(
                    x0 / 2,
                    y0 / 2,
                    chroma_log2_size,
                    2,
                    scan_order,
                    frame,
                )?;
            }
        }
        // Note: if log2_size < 3, chroma was decoded by parent when splitting from 8x8

        // Mark luma block as reconstructed
        let size = 1u32 << log2_size;
        self.reco_map.mark_reconstructed(x0, y0, size, 0);
        // Mark chroma blocks if this TU handles chroma
        if log2_size >= 3 {
            let chroma_size = size / 2;
            self.reco_map.mark_reconstructed(x0 / 2, y0 / 2, chroma_size, 1);
            self.reco_map.mark_reconstructed(x0 / 2, y0 / 2, chroma_size, 2);
        }

        Ok(())
    }

    /// Decode residual coefficients and apply to frame
    fn decode_and_apply_residual(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        c_idx: u8,
        scan_order: ScanOrder,
        frame: &mut DecodedFrame,
    ) -> Result<()> {
        // Decode coefficients via CABAC
        let coeff_buf = residual::decode_residual(
            &mut self.cabac,
            &mut self.ctx,
            log2_size,
            c_idx,
            scan_order,
            self.pps.sign_data_hiding_enabled_flag,
            self.cu_transquant_bypass_flag,
            x0,
            y0,
        )?;

        let has_nonzero = !coeff_buf.is_zero();

        // Track non-zero coefficients for deblocking (only track luma)
        if c_idx == 0 && has_nonzero {
            let tu_size = 1u32 << log2_size;
            for y in (0..tu_size).step_by(4) {
                for x in (0..tu_size).step_by(4) {
                    self.deblock_metadata.set_nonzero_coeff(x0 + x, y0 + y, true);
                }
            }
        }

        if !has_nonzero {
            return Ok(());
        }

        let size = 1usize << log2_size;
        let num_coeffs = size * size;

        // Dequantize coefficients
        let mut coeffs = [0i16; 1024];
        coeffs[..num_coeffs].copy_from_slice(&coeff_buf.coeffs[..num_coeffs]);

        // Use component-specific QP for dequantization
        let (qp, bit_depth) = match c_idx {
            0 => (self.qp_y, self.sps.bit_depth_y()),
            1 => (self.qp_cb, self.sps.bit_depth_c()),
            2 => (self.qp_cr, self.sps.bit_depth_c()),
            _ => (self.qp_y, self.sps.bit_depth_y()),
        };
        let dequant_params = transform::DequantParams {
            qp,
            bit_depth,
            log2_tr_size: log2_size,
        };
        transform::dequantize(&mut coeffs[..num_coeffs], dequant_params);

        // Apply inverse transform
        let mut residual = [0i16; 1024];
        let is_intra_4x4_luma = log2_size == 2 && c_idx == 0;
        transform::inverse_transform(&coeffs, &mut residual, size, bit_depth, is_intra_4x4_luma);

        // Add residual to prediction
        let max_val = (1i32 << bit_depth) - 1;

        for py in 0..size {
            for px in 0..size {
                let r = residual[py * size + px] as i32;
                let x = x0 + px as u32;
                let y = y0 + py as u32;

                let pred = match c_idx {
                    0 => frame.get_y(x, y) as i32,
                    1 => frame.get_cb(x, y) as i32,
                    2 => frame.get_cr(x, y) as i32,
                    _ => 0,
                };

                let recon = (pred + r).clamp(0, max_val) as u16;

                match c_idx {
                    0 => frame.set_y(x, y, recon),
                    1 => frame.set_cb(x, y, recon),
                    2 => frame.set_cr(x, y, recon),
                    _ => {}
                }
            }
        }



        Ok(())
    }

    /// Decode partition mode
    fn decode_part_mode(&mut self, pred_mode: PredMode, log2_cb_size: u8) -> Result<PartMode> {
        if pred_mode == PredMode::Intra {
            // For intra, first bin distinguishes 2Nx2N from NxN
            let ctx_idx = context::PART_MODE;
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = ctx_idx as i32; }
            let bin = self.cabac.decode_bin(&mut self.ctx[ctx_idx])?;

            if bin != 0 {
                Ok(PartMode::Part2Nx2N)
            } else {
                // NxN only allowed at minimum CU size
                if log2_cb_size == self.sps.log2_min_cb_size() {
                    Ok(PartMode::PartNxN)
                } else {
                    Err(HevcError::InvalidBitstream("NxN not allowed at this size"))
                }
            }
        } else {
            // Inter partition modes (not implemented)
            Err(HevcError::Unsupported("inter partition modes"))
        }
    }

    /// Decode intra prediction modes and apply prediction
    fn decode_intra_prediction(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        apply_chroma: bool,
        frame: &mut DecodedFrame,
    ) -> Result<IntraPredMode> {
        let (intra_luma_mode, intra_chroma_mode) =
            self.decode_intra_prediction_modes(x0, y0, log2_size, frame)?;

        // Store chroma mode for scan order lookup in transform tree
        let cb_size = 1u32 << log2_size;
        self.set_intra_pred_mode_c(x0, y0, cb_size, intra_chroma_mode);

        // NOTE: Prediction is now applied per-TU in decode_transform_unit_leaf,
        // not at the CU level, so that each TU can use reconstructed pixels
        // from earlier TUs as reference samples.

        Ok(intra_luma_mode)
    }

    /// Decode intra luma mode only (for NxN PUs after the first)
    fn decode_intra_luma_mode(&mut self, x0: u32, y0: u32) -> Result<IntraPredMode> {
        // Decode prev_intra_luma_pred_flag
        let ctx_idx = context::PREV_INTRA_LUMA_PRED_FLAG;
        #[cfg(feature = "trace-coefficients")]
        { self.cabac.trace_ctx_idx = ctx_idx as i32; }
        let prev_intra_luma_pred_flag = self.cabac.decode_bin(&mut self.ctx[ctx_idx])? != 0;

        self.decode_intra_mode_from_flag(prev_intra_luma_pred_flag, x0, y0)
    }

    /// Decode intra mode from a previously-decoded prev_intra_luma_pred_flag
    fn decode_intra_mode_from_flag(&mut self, prev_flag: bool, x0: u32, y0: u32) -> Result<IntraPredMode> {
        let cand_a = self.get_neighbor_intra_mode_left(x0, y0);
        let cand_b = self.get_neighbor_intra_mode_above(x0, y0);
        let mpm = intra::fill_mpm_candidates(cand_a, cand_b);

        let intra_luma_mode = if prev_flag {
            // Use one of the three most probable modes
            let mpm_idx = self.decode_mpm_idx()?;
            mpm[mpm_idx as usize]
        } else {
            // Decode rem_intra_luma_pred_mode (5 bits via bypass)
            let rem = self.decode_rem_intra_luma_pred_mode()?;
            let mode = self.map_rem_mode_to_intra(rem, &mpm);
            mode
        };

        Ok(intra_luma_mode)
    }

    /// Decode intra chroma mode
    /// Per HEVC spec and libde265 reference:
    /// - First bin (context-coded): if 0 → mode 4 (derived from luma)
    /// - If first bin is 1: read 2 fixed-length bypass bits → modes 0-3
    fn decode_intra_chroma_mode(&mut self, luma_mode: IntraPredMode) -> Result<IntraPredMode> {
        let ctx_idx = context::INTRA_CHROMA_PRED_MODE;
        #[cfg(feature = "trace-coefficients")]
        { self.cabac.trace_ctx_idx = ctx_idx as i32; }
        if self.cabac.decode_bin(&mut self.ctx[ctx_idx])? == 0 {
            // Mode 4: derived from luma
            return Ok(luma_mode);
        }

        // Read 2 fixed-length bypass bits for modes 0-3
        let mode_idx = self.cabac.decode_bypass_bits(2)? as u8;

        // Table 8-4: candidate list is [Planar, Angular26, Angular10, DC, DM(luma)]
        // The first 4 entries form the base list; if any equals luma_mode, replace with Angular34
        let candidates = [
            IntraPredMode::Planar,
            IntraPredMode::Angular26,
            IntraPredMode::Angular10,
            IntraPredMode::Dc,
        ];

        let mut chroma_candidates = candidates;
        for c in &mut chroma_candidates {
            if *c == luma_mode {
                *c = IntraPredMode::Angular34;
            }
        }

        let intra_chroma_mode = chroma_candidates[mode_idx as usize];

        Ok(intra_chroma_mode)
    }

    /// Decode intra prediction modes (luma + chroma) for Part2Nx2N
    fn decode_intra_prediction_modes(
        &mut self,
        x0: u32,
        y0: u32,
        log2_size: u8,
        _frame: &DecodedFrame,
    ) -> Result<(IntraPredMode, IntraPredMode)> {
        let intra_luma_mode = self.decode_intra_luma_mode(x0, y0)?;

        let intra_chroma_mode = self.decode_intra_chroma_mode(intra_luma_mode)?;

        Ok((intra_luma_mode, intra_chroma_mode))
    }

    /// Get intra prediction mode of a neighbor for MPM candidate derivation.
    /// Per H.265 spec 8.4.2: unavailable neighbors → INTRA_DC.
    /// Per H.265 spec 6.4.1 + libde265 intrapred.cc: above neighbor across CTB row → INTRA_DC.
    fn get_neighbor_intra_mode(&self, x: u32, y: u32) -> IntraPredMode {
        // Out-of-picture positions (wrapping_sub makes them huge)
        if x >= self.sps.pic_width_in_luma_samples || y >= self.sps.pic_height_in_luma_samples {
            return IntraPredMode::Dc;
        }
        self.get_intra_pred_mode(x, y)
    }

    /// Get intra prediction mode of the LEFT neighbor for position (x0, y0).
    /// Checks picture boundaries only (left in same CTB row is always OK).
    fn get_neighbor_intra_mode_left(&self, x0: u32, y0: u32) -> IntraPredMode {
        self.get_neighbor_intra_mode(x0.wrapping_sub(1), y0)
    }

    /// Get intra prediction mode of the ABOVE neighbor for position (x0, y0).
    /// Per H.265 spec: if the above position is in a different CTB row, return DC.
    fn get_neighbor_intra_mode_above(&self, x0: u32, y0: u32) -> IntraPredMode {
        let y_above = y0.wrapping_sub(1);
        // Out-of-picture check
        if y_above >= self.sps.pic_height_in_luma_samples {
            return IntraPredMode::Dc;
        }
        // Check CTB row boundary: if above is in a different CTB row, return DC
        let ctb_size = self.sps.ctb_size();
        let ctb_row_start = (y0 / ctb_size) * ctb_size;
        if y_above < ctb_row_start {
            return IntraPredMode::Dc;
        }
        self.get_intra_pred_mode(x0, y_above)
    }

    /// Map rem_intra_luma_pred_mode to actual mode (excluding MPM candidates)
    fn map_rem_mode_to_intra(&self, rem: u32, mpm: &[IntraPredMode; 3]) -> IntraPredMode {
        // Sort MPM candidates
        let mut mpm_vals = [mpm[0].as_u8(), mpm[1].as_u8(), mpm[2].as_u8()];
        mpm_vals.sort_unstable();

        // Map remaining mode
        let mut mode = rem as u8;
        for &mpm_val in &mpm_vals {
            if mode >= mpm_val {
                mode += 1;
            }
        }

        IntraPredMode::from_u8(mode).unwrap_or(IntraPredMode::Dc)
    }

    /// Decode mpm_idx (0, 1, or 2)
    fn decode_mpm_idx(&mut self) -> Result<u8> {
        // Truncated unary: 0, 10, 11
        if self.cabac.decode_bypass()? == 0 {
            Ok(0)
        } else if self.cabac.decode_bypass()? == 0 {
            Ok(1)
        } else {
            Ok(2)
        }
    }

    /// Decode rem_intra_luma_pred_mode (5 bits)
    fn decode_rem_intra_luma_pred_mode(&mut self) -> Result<u32> {
        let mut val = 0u32;
        for _ in 0..5 {
            val = (val << 1) | self.cabac.decode_bypass()? as u32;
        }
        Ok(val)
    }
}

/// Information about a coding block
#[derive(Debug, Clone, Copy, Default)]
pub struct CbInfo {
    /// Log2 of coding block size (only valid at top-left of CB)
    pub log2_cb_size: u8,
    /// Partition mode (only valid at top-left of CB)
    pub part_mode: u8,
    /// Prediction mode (INTRA/INTER/SKIP)
    pub pred_mode: u8,
    /// PCM flag
    pub pcm_flag: bool,
    /// Transquant bypass flag
    pub transquant_bypass: bool,
}

/// Information about a prediction unit
#[derive(Debug, Clone, Copy, Default)]
pub struct PuInfo {
    /// Intra prediction mode for luma
    pub intra_pred_mode_y: u8,
    /// Intra prediction mode for chroma
    pub intra_pred_mode_c: u8,
}

/// Metadata array for frame decoding
#[derive(Debug)]
pub struct MetaArray<T> {
    data: Vec<T>,
    width: u32,
    height: u32,
    log2_unit_size: u8,
}

impl<T: Default + Clone> MetaArray<T> {
    /// Create a new metadata array
    pub fn new(pic_width: u32, pic_height: u32, log2_unit_size: u8) -> Self {
        let unit_size = 1u32 << log2_unit_size;
        let width = pic_width.div_ceil(unit_size);
        let height = pic_height.div_ceil(unit_size);
        let size = (width * height) as usize;

        Self {
            data: vec![T::default(); size],
            width,
            height,
            log2_unit_size,
        }
    }

    /// Get value at pixel position
    pub fn get(&self, x: u32, y: u32) -> &T {
        let unit_x = x >> self.log2_unit_size;
        let unit_y = y >> self.log2_unit_size;
        let idx = (unit_y * self.width + unit_x) as usize;
        &self.data[idx]
    }

    /// Get mutable value at pixel position
    pub fn get_mut(&mut self, x: u32, y: u32) -> &mut T {
        let unit_x = x >> self.log2_unit_size;
        let unit_y = y >> self.log2_unit_size;
        let idx = (unit_y * self.width + unit_x) as usize;
        &mut self.data[idx]
    }

    /// Set value for a block at pixel position
    pub fn set_block(&mut self, x: u32, y: u32, log2_blk_size: u8, value: T) {
        let blk_size = 1u32 << log2_blk_size;
        let unit_size = 1u32 << self.log2_unit_size;

        let start_x = x >> self.log2_unit_size;
        let start_y = y >> self.log2_unit_size;
        let units = blk_size / unit_size;

        for dy in 0..units {
            for dx in 0..units {
                let idx = ((start_y + dy) * self.width + (start_x + dx)) as usize;
                if idx < self.data.len() {
                    self.data[idx] = value.clone();
                }
            }
        }
    }
}
