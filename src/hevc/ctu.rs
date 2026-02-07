//! CTU (Coding Tree Unit) and CU (Coding Unit) decoding
//!
//! This module handles the hierarchical quad-tree structure of HEVC:
//! - CTU: Coding Tree Unit (largest block, typically 64x64)
//! - CU: Coding Unit (result of quad-tree split, 8x8 to 64x64)
//! - PU: Prediction Unit (for motion/intra prediction)
//! - TU: Transform Unit (for residual coding)

use alloc::vec::Vec;

use super::availability::ReconstructionMap;
use super::cabac::{CabacDecoder, ContextModel, INIT_VALUES, context};
use super::debug;
use super::intra;
use super::params::{Pps, Sps};
use super::picture::DecodedFrame;
use super::residual::{self, ScanOrder};
use super::slice::{IntraPredMode, PartMode, PredMode, SliceHeader};
use super::transform;
use crate::error::HevcError;

type Result<T> = core::result::Result<T, HevcError>;

/// Chroma QP mapping table (H.265 Table 8-10)
/// Maps qPi (0-57) to QpC for 8-bit video
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
    /// Debug flag for current CTU
    debug_ctu: bool,
    /// Debug: track chroma prediction calls
    chroma_pred_count: u32,
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
}

impl<'a> SliceContext<'a> {
    /// Create a new slice context
    pub fn new(
        sps: &'a Sps,
        pps: &'a Pps,
        header: &'a SliceHeader,
        slice_data: &'a [u8],
    ) -> Result<Self> {
        // DEBUG: Print first few bytes of slice data
        eprintln!(
            "DEBUG: Slice data first 16 bytes: {:02x?}",
            &slice_data[..16.min(slice_data.len())]
        );
        eprintln!(
            "DEBUG: SPS: {}x{}, ctb_size={}, min_cb_size={}, scaling_list={}, sao={}",
            sps.pic_width_in_luma_samples,
            sps.pic_height_in_luma_samples,
            sps.ctb_size(),
            1 << sps.log2_min_cb_size(),
            sps.scaling_list_enabled_flag,
            sps.sample_adaptive_offset_enabled_flag
        );
        eprintln!(
            "DEBUG: SPS: max_transform_hierarchy_depth_intra={}",
            sps.max_transform_hierarchy_depth_intra
        );
        eprintln!(
            "DEBUG: SPS: log2_min_tb={}, log2_max_tb={}",
            sps.log2_min_tb_size(),
            sps.log2_max_tb_size()
        );

        let cabac = CabacDecoder::new(slice_data)?;
        let (range, offset) = cabac.get_state();
        eprintln!(
            "DEBUG: CABAC init state: range={}, offset={}",
            range, offset
        );

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

        eprintln!(
            "DEBUG: Chroma QP: qp_y={}, qp_cb={}, qp_cr={}",
            slice_qp, qp_cb, qp_cr
        );
        eprintln!(
            "DEBUG: sign_data_hiding_enabled_flag={}",
            pps.sign_data_hiding_enabled_flag
        );

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
            debug_ctu: false,
            chroma_pred_count: 0,
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

    /// Decode all CTUs in the slice
    pub fn decode_slice(&mut self, frame: &mut DecodedFrame) -> Result<()> {
        // Initialize CABAC tracker for debugging
        debug::init_tracker();

        eprintln!("DEBUG PPS: cu_qp_delta_enabled={} diff_cu_qp_delta_depth={} init_qp_minus26={}",
            self.pps.cu_qp_delta_enabled_flag, self.pps.diff_cu_qp_delta_depth,
            self.pps.init_qp_minus26);
        eprintln!("DEBUG SPS: log2_ctb_size={} log2_min_tb={} log2_max_tb={} max_trafo_depth_intra={}",
            self.sps.log2_ctb_size(), self.sps.log2_min_tb_size(),
            self.sps.log2_max_tb_size(), self.sps.max_transform_hierarchy_depth_intra);

        let ctb_size = self.sps.ctb_size();
        let pic_width_in_ctbs = self.sps.pic_width_in_ctbs();
        let pic_height_in_ctbs = self.sps.pic_height_in_ctbs();
        let wpp_enabled = self.pps.entropy_coding_sync_enabled_flag;

        // Start from slice segment address
        let start_addr = self.header.slice_segment_address;
        self.ctb_y = start_addr / pic_width_in_ctbs;
        self.ctb_x = start_addr % pic_width_in_ctbs;

        let mut ctu_count = 0u32;
        let total_ctus = pic_width_in_ctbs * pic_height_in_ctbs;

        // For WPP, track which entry point offset to use next (0-indexed into entry_point_offsets)
        let mut wpp_entry_idx = 0usize;

        if wpp_enabled {
            eprintln!(
                "DEBUG: WPP enabled, {} entry point offsets, {} CTB rows",
                self.header.num_entry_point_offsets, pic_height_in_ctbs
            );
        }

        loop {
            // Decode one CTU
            let x_ctb = self.ctb_x * ctb_size;
            let y_ctb = self.ctb_y * ctb_size;

            // Track CTU position for debugging
            let (byte_pos, _, _) = self.cabac.get_position();
            debug::track_ctu_start(ctu_count, byte_pos);

            // DEBUG: Print CTU state periodically
            if ctu_count.is_multiple_of(50) || ctu_count <= 3 {
                let (range, offset) = self.cabac.get_state();
                eprintln!(
                    "DEBUG: CTU {} byte={} cabac=({},{}) x={} y={}",
                    ctu_count, byte_pos, range, offset, self.ctb_x, self.ctb_y
                );
            }
            // Enable verbose debug for first CTU to trace cbf_luma etc
            self.debug_ctu = ctu_count == 0;

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

                        eprintln!(
                            "DEBUG: WPP row transition: row {} -> {}, entry_offset={}, slice_data[{}..] len={}",
                            self.ctb_y, self.ctb_y + 1, offset, offset,
                            self.slice_data.len() - offset
                        );
                    } else {
                        eprintln!(
                            "DEBUG: WPP entry point offset {} beyond slice data len {}",
                            offset, self.slice_data.len()
                        );
                        break;
                    }
                } else if end_of_slice != 0 {
                    eprintln!(
                        "DEBUG: end_of_slice after CTU {}, decoded {}/{} CTUs",
                        ctu_count, ctu_count, total_ctus
                    );
                    break;
                }

                self.ctb_x = 0;
                self.ctb_y += 1;
            } else if end_of_slice != 0 {
                // Mid-row termination (shouldn't happen normally, but handle gracefully)
                eprintln!(
                    "DEBUG: unexpected end_of_slice mid-row after CTU {}, decoded {}/{} CTUs",
                    ctu_count, ctu_count, total_ctus
                );
                break;
            }

            // Check for end of picture
            if self.ctb_y >= pic_height_in_ctbs {
                break;
            }
        }

        // Print CABAC tracker summary
        debug::print_tracker_summary();
        Ok(())
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
            {
                let (r, v, bn) = self.cabac.get_state_extended();
                eprintln!("DEBUG: before SAO: range={} value={} bits_needed={}", r, v, bn);
            }
            self.decode_sao(x_ctb, y_ctb)?;
        }
        {
            let (r, v, bn) = self.cabac.get_state_extended();
            let (byte, _, _) = self.cabac.get_position();
            eprintln!("DEBUG: after SAO, before split: byte={} range={} value={} bits_needed={}", byte, r, v, bn);
        }

        // Decode the coding quadtree
        self.decode_coding_quadtree(x_ctb, y_ctb, log2_ctb_size, 0, frame)
    }

    /// Decode SAO (Sample Adaptive Offset) parameters for a CTU
    /// Per H.265 7.3.8.3: sao_merge_left_flag/sao_merge_up_flag only present when neighbor exists
    fn decode_sao(&mut self, x_ctb: u32, y_ctb: u32) -> Result<()> {
        #[cfg(feature = "trace-coefficients")]
        {
            let (r, o) = self.cabac.get_state();
            let (byte, _, _) = self.cabac.get_position();
            eprintln!("SAO: start at ({},{}) byte={} cabac=({},{})", x_ctb, y_ctb, byte, r, o);
        }

        // sao_merge_left_flag only present if leftCtbInSliceSeg is available (x > 0)
        let sao_merge_left_flag = if x_ctb > 0 {
            #[cfg(feature = "trace-coefficients")]
            { self.cabac.trace_ctx_idx = context::SAO_MERGE_FLAG as i32; }
            self.cabac.decode_bin(&mut self.ctx[context::SAO_MERGE_FLAG])? != 0
        } else {
            false
        };

        if !sao_merge_left_flag {
            // sao_merge_up_flag only present if upCtbInSliceSeg is available (y > 0)
            let sao_merge_up_flag = if y_ctb > 0 {
                #[cfg(feature = "trace-coefficients")]
                { self.cabac.trace_ctx_idx = context::SAO_MERGE_FLAG as i32; }
                self.cabac.decode_bin(&mut self.ctx[context::SAO_MERGE_FLAG])? != 0
            } else {
                false
            };

            if !sao_merge_up_flag {
                // Per H.265 Section 7.3.8.3:
                // - sao_type_idx_luma decoded for c_idx=0
                // - sao_type_idx_chroma decoded for c_idx=1
                // - c_idx=2 (Cr) inherits type from c_idx=1 (Cb) — NOT decoded
                // - sao_eo_class decoded only for c_idx < 2; c_idx=2 inherits from c_idx=1
                // - sao_offset_abs decoded for all 3 components if type != 0
                // - sao_offset_sign and sao_band_position only for band offset (type=1)
                let mut sao_type = [0u8; 3]; // 0=none, 1=band, 2=edge

                for c_idx in 0..3u8 {
                    // Type index: only decoded for c_idx < 2
                    if c_idx < 2 {
                        #[cfg(feature = "trace-coefficients")]
                        { self.cabac.trace_ctx_idx = context::SAO_TYPE_IDX as i32; }
                        let first_bin = self.cabac.decode_bin(&mut self.ctx[context::SAO_TYPE_IDX])? != 0;

                        #[cfg(feature = "trace-coefficients")]
                        {
                            let (r, o) = self.cabac.get_state();
                            let (byte, _, _) = self.cabac.get_position();
                            eprintln!("SAO: c_idx={} first_bin={} byte={} cabac=({},{})", c_idx, first_bin, byte, r, o);
                        }

                        if first_bin {
                            let second_bin = self.cabac.decode_bypass()? != 0;
                            // TR binarization cMax=2: value=1 → bins "10", value=2 → bins "11"
                            // second_bin=0 → value=1 (band), second_bin=1 → value=2 (edge)
                            sao_type[c_idx as usize] = if second_bin { 2 } else { 1 };

                            #[cfg(feature = "trace-coefficients")]
                            eprintln!("SAO: c_idx={} type={} (1=band, 2=edge)", c_idx, sao_type[c_idx as usize]);
                        }
                    } else {
                        // c_idx=2: inherit type from c_idx=1
                        sao_type[2] = sao_type[1];

                        #[cfg(feature = "trace-coefficients")]
                        eprintln!("SAO: c_idx=2 inherited type={} from c_idx=1", sao_type[2]);
                    }

                    // Decode offsets if SAO is enabled for this component
                    if sao_type[c_idx as usize] != 0 {
                        // Decode 4 offset absolute values (unary bypass coding)
                        let mut offset_abs = [0u32; 4];
                        for i in 0..4 {
                            let mut abs_val = 0u32;
                            loop {
                                let bin = self.cabac.decode_bypass()?;
                                if bin == 0 {
                                    break;
                                }
                                abs_val += 1;
                                if abs_val >= 31 {
                                    break;
                                }
                            }
                            offset_abs[i] = abs_val;
                        }

                        if sao_type[c_idx as usize] == 1 {
                            // Band offset: decode signs for non-zero offsets, then band_position
                            for i in 0..4 {
                                if offset_abs[i] > 0 {
                                    let _sign = self.cabac.decode_bypass()?;
                                }
                            }
                            let _band_position = self.cabac.decode_bypass_bits(5)?;
                        } else {
                            // Edge offset: decode eo_class only for c_idx < 2
                            // (c_idx=2 inherits eo_class from c_idx=1)
                            // No sign decoding for edge offset (signs derived from edge class)
                            if c_idx < 2 {
                                let _eo_class = self.cabac.decode_bypass_bits(2)?;
                                #[cfg(feature = "trace-coefficients")]
                                eprintln!("SAO: c_idx={} eo_class={}", c_idx, _eo_class);
                            }
                        }
                    }
                }
            }
        }

        #[cfg(feature = "trace-coefficients")]
        {
            let (r, o) = self.cabac.get_state();
            let (byte, _, _) = self.cabac.get_position();
            eprintln!("SAO: end byte={} cabac=({},{})", byte, r, o);
        }

        Ok(())
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
            if self.debug_ctu {
                let (r, o) = self.cabac.get_state();
                eprintln!(
                    "  CTU37: split_cu_flag at ({},{}) depth={} log2={} → {} (r={},o={})",
                    x0, y0, ct_depth, log2_cb_size, flag, r, o
                );
            }
            flag
        } else if log2_cb_size > log2_min_cb_size {
            // Must split if partially outside picture
            if self.debug_ctu {
                eprintln!(
                    "  CTU37: forced split at ({},{}) depth={} - outside picture",
                    x0, y0, ct_depth
                );
            }
            true
        } else {
            // At minimum size, don't split
            if self.debug_ctu {
                eprintln!(
                    "  CTU37: no split at ({},{}) depth={} - min size",
                    x0, y0, ct_depth
                );
            }
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

        // DEBUG: Print first few split decisions
        if x0 < 64 && y0 < 64 {
            eprintln!(
                "DEBUG: split_cu_flag at ({},{}) depth={} ctx={} (condL={} condA={}) = {}",
                x0, y0, ct_depth, ctx_idx, cond_l, cond_a, bin
            );
        }

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
            if self.debug_ctu {
                let (r, o) = self.cabac.get_state();
                eprintln!(
                    "  CTU37: CU at ({},{}) log2={} part_mode={:?} (r={},o={})",
                    x0, y0, log2_cb_size, pm, r, o
                );
            }
            pm
        } else {
            // Larger sizes are always 2Nx2N for intra
            if self.debug_ctu {
                eprintln!(
                    "  CTU37: CU at ({},{}) log2={} part_mode=2Nx2N (implicit)",
                    x0, y0, log2_cb_size
                );
            }
            PartMode::Part2Nx2N
        };

        // Decode prediction info and get intra mode for scan order
        let intra_mode = match part_mode {
            PartMode::Part2Nx2N => {
                // Single PU covering entire CU
                let mode = self.decode_intra_prediction(x0, y0, log2_cb_size, true, frame)?;
                // Store intra mode for all 4x4 blocks in this CU
                self.set_intra_pred_mode(x0, y0, cb_size, mode);
                if self.debug_ctu {
                    let (r, o) = self.cabac.get_state();
                    eprintln!(
                        "  CTU37: After intra_prediction at (1144,120): mode={:?} (r={},o={}) bits={}",
                        mode,
                        r,
                        o,
                        self.cabac.get_position().2
                    );
                }
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

            if self.debug_ctu {
                let (r, o) = self.cabac.get_state();
                eprintln!(
                    "  CTU37: After transform_tree at ({},{}) log2={} (r={},o={})",
                    x0, y0, log2_cb_size, r, o
                );
            }
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
        let debug_tt = self.debug_ctu;

        // Step 1: Determine if we should split
        // H.265 spec 7.3.8.7: split_transform_flag is decoded when:
        //   log2TrafoSize <= MaxTbLog2SizeY && log2TrafoSize > MinTbLog2SizeY &&
        //   trafoDepth < MaxTrafoDepth && !(IntraSplitFlag && trafoDepth == 0)
        // When IntraSplitFlag==1 && trafoDepth==0, split is forced to true (implicit)
        let split_transform = if intra_split_flag && trafo_depth == 0 {
            // H.265 spec: IntraNxN at root level forces split (not coded)
            if debug_tt {
                eprintln!(
                    "    TT(1144,120): split forced (IntraSplitFlag=1, depth=0)"
                );
            }
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
            if debug_tt {
                let (r, o) = self.cabac.get_state();
                let bits = self.cabac.get_position().2;
                eprintln!(
                    "    TT(1144,120): split_transform_flag={} depth={} (r={},o={}) bits={}",
                    flag, trafo_depth, r, o, bits
                );
            }
            flag
        } else if log2_size > log2_max_trafo_size {
            true // Must split if larger than max
        } else {
            if debug_tt {
                eprintln!(
                    "    TT(1144,120): no split (log2={} min={} max={} depth={} maxdepth={})",
                    log2_size,
                    log2_min_trafo_size,
                    log2_max_trafo_size,
                    trafo_depth,
                    max_trafo_depth
                );
            }
            false
        };

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
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!(
                        "    TT(1144,120): cbf_cb={} depth={} (r={},o={})",
                        val, trafo_depth, r, o
                    );
                }
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
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!(
                        "    TT(1144,120): cbf_cr={} depth={} (r={},o={})",
                        val, trafo_depth, r, o
                    );
                }
                val
            } else {
                false
            };
            // DEBUG: Print cbf decoding
            if x0 < 16 && y0 < 16 {
                eprintln!(
                    "DEBUG: cbf at ({},{}) log2={} depth={}: decoded cb={} cr={}",
                    x0, y0, log2_size, trafo_depth, cb, cr
                );
            }
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
                eprintln!("DEBUG: 8x8→4x4 split complete, decoding chroma at luma({},{}) → chroma({},{})",
                    x0, y0, x0/2, y0/2);

                // Apply chroma prediction for the deferred chroma TU before adding residuals
                let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
                intra::predict_intra(frame, x0 / 2, y0 / 2, 2, chroma_mode, 1, &self.reco_map);
                intra::predict_intra(frame, x0 / 2, y0 / 2, 2, chroma_mode, 2, &self.reco_map);

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
        let debug_tt = self.debug_ctu;

        // Apply intra prediction for this TU's luma block BEFORE decoding residuals
        // This ensures prediction uses the latest reconstructed frame state
        let actual_luma_mode = self.get_intra_pred_mode(x0, y0);
        intra::predict_intra(frame, x0, y0, log2_size, actual_luma_mode, 0, &self.reco_map);

        // Apply chroma prediction if this TU handles chroma (log2_size >= 3)
        if log2_size >= 3 {
            let chroma_mode = self.get_intra_pred_mode_c(x0, y0);
            let chroma_x = x0 / 2;
            let chroma_y = y0 / 2;
            let chroma_log2_size = log2_size - 1;
            intra::predict_intra(frame, chroma_x, chroma_y, chroma_log2_size, chroma_mode, 1, &self.reco_map);
            intra::predict_intra(frame, chroma_x, chroma_y, chroma_log2_size, chroma_mode, 2, &self.reco_map);
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
            if debug_tt {
                let (r, o) = self.cabac.get_state();
                eprintln!(
                    "    TT: cbf_luma={} depth={} log2={} ctx_offset={} (r={},o={})",
                    val, trafo_depth, log2_size, ctx_offset, r, o
                );
            }
            val
        } else {
            if debug_tt {
                eprintln!(
                    "    TT: cbf_luma=1 (implicit) log2={} depth={}",
                    log2_size, trafo_depth
                );
            }
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

                // Apply QP delta per H.265 section 8.6.1
                // QP_Y = ((qP_Y_PRED + CuQpDeltaVal + 52 + 2*QpBdOffsetY) % (52 + QpBdOffsetY)) - QpBdOffsetY
                let qp_bd_offset_y = 6 * (self.sps.bit_depth_y() as i32 - 8);
                self.qp_y = ((self.qp_y + self.cu_qp_delta + 52 + 2 * qp_bd_offset_y)
                    % (52 + qp_bd_offset_y))
                    - qp_bd_offset_y;

                // Update chroma QP values
                let qp_i_cb = self.qp_y + self.pps.pps_cb_qp_offset as i32
                    + self.header.slice_cb_qp_offset as i32;
                let qp_i_cr = self.qp_y + self.pps.pps_cr_qp_offset as i32
                    + self.header.slice_cr_qp_offset as i32;
                self.qp_cb = chroma_qp_mapping(qp_i_cb.clamp(0, 57));
                self.qp_cr = chroma_qp_mapping(qp_i_cr.clamp(0, 57));

                if debug_tt {
                    eprintln!("    TT: cu_qp_delta_abs={} delta={} -> qp_y={} qp_cb={} qp_cr={}",
                        cu_qp_delta_abs, self.cu_qp_delta, self.qp_y, self.qp_cb, self.qp_cr);
                }
            }
        }

        // Decode and apply luma residuals
        if cbf_luma {
            if debug_tt {
                let (r, o) = self.cabac.get_state();
                eprintln!("    TT(1144,120): decoding luma residual (r={},o={})", r, o);
            }
            // Use per-position intra mode for scan order (critical for NxN partitions
            // where each sub-TU has a different intra prediction mode)
            let actual_mode = self.get_intra_pred_mode(x0, y0);
            let scan_order = residual::get_scan_order(log2_size, actual_mode.as_u8(), 0);
            self.decode_and_apply_residual(x0, y0, log2_size, 0, scan_order, frame)?;
            if debug_tt {
                let (r, o) = self.cabac.get_state();
                eprintln!("    TT(1144,120): after luma residual (r={},o={})", r, o);
            }
        }

        // Decode chroma residuals if not handled by parent (log2_size >= 3)
        if log2_size >= 3 {
            let chroma_log2_size = log2_size - 1;
            if cbf_cb {
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!("    TT(1144,120): decoding Cb residual (r={},o={})", r, o);
                }
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
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!("    TT(1144,120): after Cb residual (r={},o={})", r, o);
                }
            }
            if cbf_cr {
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!("    TT(1144,120): decoding Cr residual (r={},o={})", r, o);
                }
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
                if debug_tt {
                    let (r, o) = self.cabac.get_state();
                    eprintln!("    TT(1144,120): after Cr residual (r={},o={})", r, o);
                }
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

        // RECONSTRUCTION TRACE: Print reconstructed pixels for differential debugging
        {
            use core::sync::atomic::{AtomicU32, Ordering};
            static TU_RECO_COUNT: AtomicU32 = AtomicU32::new(0);
            let count = TU_RECO_COUNT.load(Ordering::Relaxed);
            let size = 1u32 << log2_size;

            // Trace luma
            if count < 200 {
                let row0: Vec<u16> = (0..size.min(8)).map(|x| frame.get_y(x0 + x, y0)).collect();
                let row1: Vec<u16> = if size > 1 { (0..size.min(8)).map(|x| frame.get_y(x0 + x, y0 + 1)).collect() } else { vec![] };
                eprint!("TU_RECO {}: pos=({},{}) nT={} cIdx=0 qP={} cbf={}",
                    count, x0, y0, size, self.qp_y, if cbf_luma { 1 } else { 0 });
                eprint!(" row0=[{}]", row0.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                if !row1.is_empty() {
                    eprint!(" row1=[{}]", row1.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                }
                eprintln!();
                TU_RECO_COUNT.store(count + 1, Ordering::Relaxed);
            }

            // Trace chroma if this TU handles chroma
            if log2_size >= 3 && count + 1 < 200 {
                let cx = x0 / 2;
                let cy = y0 / 2;
                let cs = size / 2;
                let count2 = TU_RECO_COUNT.load(Ordering::Relaxed);

                let cb_row0: Vec<u16> = (0..cs.min(8)).map(|x| frame.get_cb(cx + x, cy)).collect();
                let cb_row1: Vec<u16> = if cs > 1 { (0..cs.min(8)).map(|x| frame.get_cb(cx + x, cy + 1)).collect() } else { vec![] };
                eprint!("TU_RECO {}: pos=({},{}) nT={} cIdx=1 qP={} cbf={}",
                    count2, cx, cy, cs, self.qp_cb, if cbf_cb { 1 } else { 0 });
                eprint!(" row0=[{}]", cb_row0.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                if !cb_row1.is_empty() {
                    eprint!(" row1=[{}]", cb_row1.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                }
                eprintln!();
                TU_RECO_COUNT.store(count2 + 1, Ordering::Relaxed);

                let count3 = TU_RECO_COUNT.load(Ordering::Relaxed);
                let cr_row0: Vec<u16> = (0..cs.min(8)).map(|x| frame.get_cr(cx + x, cy)).collect();
                let cr_row1: Vec<u16> = if cs > 1 { (0..cs.min(8)).map(|x| frame.get_cr(cx + x, cy + 1)).collect() } else { vec![] };
                eprint!("TU_RECO {}: pos=({},{}) nT={} cIdx=2 qP={} cbf={}",
                    count3, cx, cy, cs, self.qp_cr, if cbf_cr { 1 } else { 0 });
                eprint!(" row0=[{}]", cr_row0.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                if !cr_row1.is_empty() {
                    eprint!(" row1=[{}]", cr_row1.iter().map(|v| v.to_string()).collect::<Vec<_>>().join(","));
                }
                eprintln!();
                TU_RECO_COUNT.store(count3 + 1, Ordering::Relaxed);
            }
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
        // Track chroma decode statistics
        static CB_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        static CR_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
        static CB_RESIDUAL_SUM: core::sync::atomic::AtomicI64 =
            core::sync::atomic::AtomicI64::new(0);
        static CR_RESIDUAL_SUM: core::sync::atomic::AtomicI64 =
            core::sync::atomic::AtomicI64::new(0);

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

        if coeff_buf.is_zero() {
            return Ok(());
        }

        // DEBUG: Print first few coefficient blocks
        if x0 < 8 && y0 < 8 && c_idx == 0 {
            let size = 1usize << log2_size;
            eprintln!(
                "DEBUG: coeffs at ({},{}) c_idx={} size={}:",
                x0, y0, c_idx, size
            );
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|x| coeff_buf.get(x, y)).collect();
                eprintln!("  {:?}", row);
            }
        }

        // DEBUG: Print Cr coefficients at CTU 1 boundary
        if x0 == 32 && y0 == 0 && c_idx == 2 {
            let size = 1usize << log2_size;
            eprintln!("DEBUG: Cr coeffs at (32,0) raw:");
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|x| coeff_buf.get(x, y)).collect();
                eprintln!("  {:?}", row);
            }
        }
        // DEBUG: Print Cb coefficients at CTU 1 boundary for comparison
        if x0 == 32 && y0 == 0 && c_idx == 1 {
            let size = 1usize << log2_size;
            eprintln!("DEBUG: Cb coeffs at (32,0) raw:");
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|x| coeff_buf.get(x, y)).collect();
                eprintln!("  {:?}", row);
            }
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

        // DEBUG: Print dequantized coefficients for first block
        if x0 == 0 && y0 == 0 && c_idx == 0 {
            eprintln!("DEBUG: QP={}, bit_depth={}", self.qp_y, bit_depth);
            eprintln!("DEBUG: dequantized coeffs:");
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|px| coeffs[y * size + px]).collect();
                eprintln!("  {:?}", row);
            }
        }

        // DEBUG: Print dequantized Cr coefficients at x=104
        if x0 == 104 && y0 == 0 && c_idx == 2 {
            let call_num =
                residual::DEBUG_RESIDUAL_COUNTER.load(core::sync::atomic::Ordering::Relaxed);
            eprintln!(
                "DEBUG: Cr at (104,0) residual_call #{} BEFORE dequant: {:?}",
                call_num - 1,
                &coeff_buf.coeffs[..num_coeffs]
            );
            eprintln!(
                "DEBUG: Cr at (104,0) AFTER dequant QP={}, bit_depth={}:",
                qp, bit_depth
            );
            eprintln!("  {:?}", &coeffs[..num_coeffs]);
        }

        // Apply inverse transform
        let mut residual = [0i16; 1024];
        let is_intra_4x4_luma = log2_size == 2 && c_idx == 0;
        transform::inverse_transform(&coeffs, &mut residual, size, bit_depth, is_intra_4x4_luma);

        // DEBUG: Print residuals for first blocks (all components)
        if x0 < 4 && y0 < 4 {
            eprintln!("DEBUG: residuals at ({},{}) c_idx={}:", x0, y0, c_idx);
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|x| residual[y * size + x]).collect();
                eprintln!("  {:?}", row);
            }
        }

        // DEBUG: Print residuals for Cb/Cr at CTU 1 boundary
        if x0 == 32 && y0 == 0 && (c_idx == 1 || c_idx == 2) {
            eprintln!(
                "DEBUG: {} residuals at (32,0):",
                if c_idx == 1 { "Cb" } else { "Cr" }
            );
            for y in 0..size.min(4) {
                let row: Vec<i16> = (0..size.min(4)).map(|px| residual[y * size + px]).collect();
                eprintln!("  {:?}", row);
            }
        }

        // Add residual to prediction
        let max_val = (1i32 << bit_depth) - 1;

        // Track chroma residual sums and prediction sums
        static CB_PRED_SUM: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
        static CR_PRED_SUM: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);

        if c_idx == 1 {
            let res_sum: i64 = residual[..num_coeffs].iter().map(|&r| r as i64).sum();
            let mut pred_sum: i64 = 0;
            for py in 0..size {
                for px in 0..size {
                    pred_sum += frame.get_cb(x0 + px as u32, y0 + py as u32) as i64;
                }
            }
            CB_RESIDUAL_SUM.fetch_add(res_sum, core::sync::atomic::Ordering::Relaxed);
            CB_PRED_SUM.fetch_add(pred_sum, core::sync::atomic::Ordering::Relaxed);
            let count = CB_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let pred_avg = pred_sum as f64 / num_coeffs as f64;
            if count < 5 || (50..55).contains(&count) || (pred_avg > 240.0 && count < 30) {
                eprintln!(
                    "DEBUG: Cb TU at ({},{}) size={} residual_sum={} pred_avg={:.1}",
                    x0, y0, size, res_sum, pred_avg
                );
            }
        } else if c_idx == 2 {
            let res_sum: i64 = residual[..num_coeffs].iter().map(|&r| r as i64).sum();
            let mut pred_sum: i64 = 0;
            for py in 0..size {
                for px in 0..size {
                    pred_sum += frame.get_cr(x0 + px as u32, y0 + py as u32) as i64;
                }
            }
            CR_RESIDUAL_SUM.fetch_add(res_sum, core::sync::atomic::Ordering::Relaxed);
            CR_PRED_SUM.fetch_add(pred_sum, core::sync::atomic::Ordering::Relaxed);
            let _count = CR_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
            let pred_avg = pred_sum as f64 / num_coeffs as f64;
            // Debug for all Cr TUs at y=0 to trace corruption
            if y0 == 0 {
                eprintln!(
                    "DEBUG: Cr TU at ({},{}) size={} residual_sum={} pred_avg={:.1}",
                    x0, y0, size, res_sum, pred_avg
                );
                // Extra debug: print residual array for corrupted TU
                if (104..=108).contains(&x0) {
                    let res_vals: Vec<i16> = residual[..num_coeffs].to_vec();
                    eprintln!("  residual array: {:?}", res_vals);
                }
            }
        }

        // DEBUG: Print first sample reconstruction for chroma
        if x0 == 0 && y0 == 0 && c_idx > 0 {
            let pred = match c_idx {
                1 => frame.get_cb(0, 0) as i32,
                2 => frame.get_cr(0, 0) as i32,
                _ => 0,
            };
            let r = residual[0] as i32;
            eprintln!(
                "DEBUG: c_idx={} at (0,0): pred={}, residual={}, recon={}",
                c_idx,
                pred,
                r,
                (pred + r).clamp(0, max_val)
            );
        }

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

                // DEBUG: Track Cr writes from residual at x=104-111, y=0
                if c_idx == 2 && y == 0 && (104..=111).contains(&x) {
                    eprintln!(
                        "DEBUG: residual set_cr({},{}) = {} (pred={} r={})",
                        x, y, recon, pred, r
                    );
                }

                match c_idx {
                    0 => frame.set_y(x, y, recon),
                    1 => frame.set_cb(x, y, recon),
                    2 => frame.set_cr(x, y, recon),
                    _ => {}
                }
            }
        }

        // Print final chroma statistics
        if c_idx == 2 {
            let cr_count = CR_COUNT.load(core::sync::atomic::Ordering::Relaxed);
            if cr_count == 100 || cr_count == 500 || cr_count == 1000 {
                let cb_count = CB_COUNT.load(core::sync::atomic::Ordering::Relaxed);
                let cb_res_sum = CB_RESIDUAL_SUM.load(core::sync::atomic::Ordering::Relaxed);
                let cr_res_sum = CR_RESIDUAL_SUM.load(core::sync::atomic::Ordering::Relaxed);
                let cb_pred_sum = CB_PRED_SUM.load(core::sync::atomic::Ordering::Relaxed);
                let cr_pred_sum = CR_PRED_SUM.load(core::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "DEBUG CHROMA STATS: Cb count={} res_sum={} pred_sum={} pred_avg={:.1}",
                    cb_count,
                    cb_res_sum,
                    cb_pred_sum,
                    cb_pred_sum as f64 / (cb_count * 16) as f64
                );
                eprintln!(
                    "DEBUG CHROMA STATS: Cr count={} res_sum={} pred_sum={} pred_avg={:.1}",
                    cr_count,
                    cr_res_sum,
                    cr_pred_sum,
                    cr_pred_sum as f64 / (cr_count * 16) as f64
                );
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

        // DEBUG: Print first few intra modes
        if x0 < 16 && y0 < 16 {
            eprintln!(
                "DEBUG: intra_mode at ({},{}) size={}: mode={:?}",
                x0,
                y0,
                1u32 << log2_size,
                intra_luma_mode
            );
        }

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
        // CTB row boundary check: if y-1 is above the current CTB's top edge
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
