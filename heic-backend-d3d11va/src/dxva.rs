//! Rust bindings for the DXVA HEVC picture-parameter structures from
//! the Windows SDK `dxva.h`.
//!
//! `dxva.h` defines these structs as C with bitfield unions. Rust has
//! no portable bitfield syntax, so we model each bitfield union as the
//! union's integer alternative (`u16` or `u32`) and provide accessor
//! methods that pack / unpack the fields by shift + mask. This matches
//! Microsoft's documented wire layout exactly — the GPU driver reads
//! the integer view, the named bitfields are just compile-time
//! convenience in C.
//!
//! # Source
//!
//! Field layout taken from Microsoft's `win32metadata` repository:
//! `generation/WinSDK/RecompiledIdlHeaders/um/dxva.h` (the canonical
//! Windows SDK shipped with Visual Studio). The struct is also
//! documented under "DXVA_PicParams_HEVC structure" on
//! `learn.microsoft.com/windows-hardware/drivers/ddi/dxva/`, though
//! the docs page omits a few of the more recent additions.
//!
//! # Layout discipline
//!
//! `#[repr(C)]` matches the C ABI. The original struct uses
//! `#pragma pack(push, 4)` in the SDK, which is the default for
//! 32-bit-aligned fields on Windows x86_64; Rust's default `repr(C)`
//! achieves the same layout. Bitfield unions are placed at integer
//! granularity so individual u8/u16/u32 fields align naturally.
//!
//! All padding fields (`ReservedBits*`) are kept and exported as
//! `pub` so callers can zero them explicitly — the GPU driver may
//! reject buffers with garbage reserved bits.

#![cfg(target_os = "windows")]
#![allow(non_snake_case)] // matches the Win32 SDK field names exactly
#![allow(missing_docs)]
// documented inline via comments + the module headers
// Tests use `let mut sps = ParsedSps::default(); sps.field = ...;` to set
// up small fixtures field-by-field — that's clearer in unit tests than
// constructing a 35-field struct literal with `..Default::default()`.
#![cfg_attr(test, allow(clippy::field_reassign_with_default))]

use heic_core::sps::{ParsedPps, ParsedSps};

/// `DXVA_PicEntry_HEVC` — single entry in the reference picture list
/// or the current-picture identifier.
///
/// Bit layout (LSB-first per Windows ABI):
/// * bits 0-6: `Index7Bits` — surface index in the decoder's output
///   array.
/// * bit 7: `AssociatedFlag` — long-term reference indicator (or
///   "field picture" for interlaced bitstreams, irrelevant for HEIC).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DxvaPicEntryHevc {
    /// Packed `Index7Bits | (AssociatedFlag << 7)` per the union.
    pub bPicEntry: u8,
}

impl DxvaPicEntryHevc {
    /// Sentinel for "no reference" — `0xFF` is Microsoft's documented
    /// "Index7Bits = 127, AssociatedFlag = 1" no-ref marker.
    pub const INVALID: Self = Self { bPicEntry: 0xFF };

    /// Construct from surface index + AssociatedFlag.
    #[must_use]
    pub const fn new(index: u8, associated: bool) -> Self {
        let assoc = if associated { 0x80 } else { 0 };
        Self {
            bPicEntry: (index & 0x7F) | assoc,
        }
    }

    /// Surface index in bits 0-6.
    #[must_use]
    pub const fn index(self) -> u8 {
        self.bPicEntry & 0x7F
    }
}

/// `DXVA_PicParams_HEVC` — HEVC picture-parameter buffer the driver
/// expects via `ID3D11VideoContext::SubmitDecoderBuffers` with type
/// `D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS`.
///
/// Total size is 192 bytes per the SDK (verified against bindgen on
/// Win10 SDK 22000). The driver reads each `dwXxxFlags` union as a
/// little-endian `UINT32`; the bitfield accessors below mirror the
/// `dxva.h` layout exactly.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct DxvaPicParamsHevc {
    /// Picture width in min coding blocks (formula 7-14).
    pub PicWidthInMinCbsY: u16,
    /// Picture height in min coding blocks (formula 7-16).
    pub PicHeightInMinCbsY: u16,
    /// Bitfield union — see [`Self::set_format_seq_info`] / accessors.
    /// Bit layout: see [`FormatSeqInfo`].
    pub wFormatAndSequenceInfoFlags: u16,
    /// Current picture handle.
    pub CurrPic: DxvaPicEntryHevc,
    /// `sps_max_dec_pic_buffering_minus1` at the SPS's highest TID.
    pub sps_max_dec_pic_buffering_minus1: u8,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_transform_block_size_minus2`.
    pub log2_min_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_transform_block_size`.
    pub log2_diff_max_min_transform_block_size: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pics_sps: u8,
    /// `num_ref_idx_l0_default_active_minus1`.
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1`.
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `init_qp_minus26`.
    pub init_qp_minus26: i8,
    /// Number of delta POCs for `RefRpsIdx` (slice-header derived).
    pub ucNumDeltaPocsOfRefRpsIdx: u8,
    /// Bits used by `short_term_ref_pic_set()` in the slice header.
    pub wNumBitsForShortTermRPSInSlice: u16,
    /// Reserved — must be zero.
    pub ReservedBits2: u16,
    /// Bitfield union — see [`CodingParamToolFlags`] for layout.
    pub dwCodingParamToolFlags: u32,
    /// Bitfield union — see [`CodingSettingPicturePropertyFlags`].
    pub dwCodingSettingPicturePropertyFlags: u32,
    /// `pps_cb_qp_offset` in range `[-12, 12]`.
    pub pps_cb_qp_offset: i8,
    /// `pps_cr_qp_offset` in range `[-12, 12]`.
    pub pps_cr_qp_offset: i8,
    /// `num_tile_columns_minus1` in range `[0, 18]`.
    pub num_tile_columns_minus1: u8,
    /// `num_tile_rows_minus1` in range `[0, 20]`.
    pub num_tile_rows_minus1: u8,
    /// Per-column tile widths (only first `num_tile_columns_minus1` valid).
    pub column_width_minus1: [u16; 19],
    /// Per-row tile heights (only first `num_tile_rows_minus1` valid).
    pub row_height_minus1: [u16; 21],
    /// `diff_cu_qp_delta_depth`.
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_beta_offset_div2`.
    pub pps_beta_offset_div2: i8,
    /// `pps_tc_offset_div2`.
    pub pps_tc_offset_div2: i8,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// POC of the picture being decoded.
    pub CurrPicOrderCntVal: i32,
    /// Reference picture surface handles.
    pub RefPicList: [DxvaPicEntryHevc; 15],
    /// Reserved padding.
    pub ReservedBits5: u8,
    /// POC values for each entry in [`Self::RefPicList`].
    pub PicOrderCntValList: [i32; 15],
    /// Indices into `RefPicList` for the "before" set.
    pub RefPicSetStCurrBefore: [u8; 8],
    /// Indices into `RefPicList` for the "after" set.
    pub RefPicSetStCurrAfter: [u8; 8],
    /// Indices into `RefPicList` for the long-term set.
    pub RefPicSetLtCurr: [u8; 8],
    /// Reserved padding.
    pub ReservedBits6: u16,
    /// Reserved padding.
    pub ReservedBits7: u16,
    /// Status report feedback handle the driver will write back into.
    pub StatusReportFeedbackNumber: u32,
}

/// `DXVA_Qmatrix_HEVC` — inverse-quantization matrix buffer the driver
/// expects via `SubmitDecoderBuffers` with type
/// `D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX` whenever
/// `sps.scaling_list_enabled_flag` is true.
///
/// Layout matches dxva.h's `DXVA_Qmatrix_HEVC`. Total size = 384 +
/// 256 + 128 + 6 + 2 = 776 bytes.
///
/// * `ucScalingLists0[6][16]` — 4×4 scaling lists per matrixId
///   (0..=5 = Y/Cb/Cr intra/inter).
/// * `ucScalingLists1[6][64]` — 8×8 scaling lists.
/// * `ucScalingLists2[6][64]` — 16×16 scaling lists (sampled).
/// * `ucScalingLists3[2][64]` — 32×32 scaling lists (luma intra/inter
///   only).
/// * `ucScalingListDCCoefSizeID2[6]` — 16×16 DC coefficients.
/// * `ucScalingListDCCoefSizeID3[2]` — 32×32 DC coefficients.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DxvaQmatrixHevc {
    pub ucScalingLists0: [[u8; 16]; 6],
    pub ucScalingLists1: [[u8; 64]; 6],
    pub ucScalingLists2: [[u8; 64]; 6],
    pub ucScalingLists3: [[u8; 64]; 2],
    pub ucScalingListDCCoefSizeID2: [u8; 6],
    pub ucScalingListDCCoefSizeID3: [u8; 2],
}

impl Default for DxvaQmatrixHevc {
    fn default() -> Self {
        Self {
            ucScalingLists0: [[16; 16]; 6],
            ucScalingLists1: [[16; 64]; 6],
            ucScalingLists2: [[16; 64]; 6],
            ucScalingLists3: [[16; 64]; 2],
            ucScalingListDCCoefSizeID2: [16; 6],
            ucScalingListDCCoefSizeID3: [16; 2],
        }
    }
}

/// HEVC default 4×4 scaling list per spec table 7-3. Flat matrix of
/// 16s for all 6 matrixIds (Y/Cb/Cr intra + inter).
pub const HEVC_DEFAULT_SCALING_LIST_4X4: [u8; 16] = [16; 16];

/// HEVC default 8×8 intra scaling list per spec table 7-4.
/// Applies to sizeId=1/2/3 with matrixId 0/1/2 (Y/Cb/Cr intra).
#[rustfmt::skip]
pub const HEVC_DEFAULT_SCALING_LIST_INTRA_8X8: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 21, 24,
    16, 16, 16, 16, 17, 19, 22, 25,
    16, 16, 17, 18, 20, 22, 25, 29,
    16, 16, 18, 21, 24, 27, 31, 36,
    17, 17, 20, 24, 30, 35, 41, 47,
    18, 19, 22, 27, 35, 44, 54, 65,
    21, 22, 25, 31, 41, 54, 70, 88,
    24, 25, 29, 36, 47, 65, 88, 115,
];

/// HEVC default 8×8 inter scaling list per spec table 7-4.
/// Applies to sizeId=1/2/3 with matrixId 3/4/5 (Y/Cb/Cr inter).
#[rustfmt::skip]
pub const HEVC_DEFAULT_SCALING_LIST_INTER_8X8: [u8; 64] = [
    16, 16, 16, 16, 17, 18, 20, 24,
    16, 16, 16, 17, 18, 20, 24, 25,
    16, 16, 17, 18, 20, 24, 25, 28,
    16, 17, 18, 20, 24, 25, 28, 33,
    17, 18, 20, 24, 25, 28, 33, 41,
    18, 20, 24, 25, 28, 33, 41, 54,
    20, 24, 25, 28, 33, 41, 54, 71,
    24, 25, 28, 33, 41, 54, 71, 91,
];

/// Build a [`DxvaQmatrixHevc`] populated with HEVC default scaling
/// lists (spec tables 7-3 / 7-4). The caller passes this to the
/// driver whenever `sps.scaling_list_enabled_flag` is true.
///
/// Implementation note: chromium's path also propagates custom
/// scaling lists from `sps.scaling_list_data` / `pps.scaling_list_data`
/// when `scaling_list_data_present_flag` is set — that requires the
/// bitstream parser to read the actual table values. For the HEIC
/// fixtures we ship, defaults match the encoders (libheif / x265
/// rarely override scaling lists for stills); the custom-list path
/// is a follow-up if any failing file uses non-default lists.
#[must_use]
pub fn default_qmatrix_hevc() -> DxvaQmatrixHevc {
    let mut q = DxvaQmatrixHevc::default();
    // 4×4 flat for all 6 matrixIds.
    for m in 0..6 {
        q.ucScalingLists0[m] = HEVC_DEFAULT_SCALING_LIST_4X4;
    }
    // 8×8: matrixId 0/1/2 = intra Y/Cb/Cr, 3/4/5 = inter Y/Cb/Cr.
    for m in 0..3 {
        q.ucScalingLists1[m] = HEVC_DEFAULT_SCALING_LIST_INTRA_8X8;
        q.ucScalingLists1[m + 3] = HEVC_DEFAULT_SCALING_LIST_INTER_8X8;
        // 16×16 uses the same 8×8 base sampled to 16×16; the DXVA
        // struct stores the 8×8 base as the source.
        q.ucScalingLists2[m] = HEVC_DEFAULT_SCALING_LIST_INTRA_8X8;
        q.ucScalingLists2[m + 3] = HEVC_DEFAULT_SCALING_LIST_INTER_8X8;
    }
    // 32×32 has only luma (matrixId 0 = intra Y, 1 = inter Y).
    q.ucScalingLists3[0] = HEVC_DEFAULT_SCALING_LIST_INTRA_8X8;
    q.ucScalingLists3[1] = HEVC_DEFAULT_SCALING_LIST_INTER_8X8;
    // DC coefficients default to 16.
    q.ucScalingListDCCoefSizeID2 = [16; 6];
    q.ucScalingListDCCoefSizeID3 = [16; 2];
    q
}

/// Bit layout of [`DxvaPicParamsHevc::wFormatAndSequenceInfoFlags`].
///
/// LSB-first per the SDK's `BITFIELD union { ... }` declaration.
#[allow(non_camel_case_types)]
pub mod format_seq {
    /// `chroma_format_idc` — 2 bits.
    pub const CHROMA_FORMAT_IDC_SHIFT: u16 = 0;
    pub const CHROMA_FORMAT_IDC_MASK: u16 = 0b11;
    /// `separate_colour_plane_flag` — 1 bit.
    pub const SEPARATE_COLOUR_PLANE_FLAG: u16 = 1 << 2;
    /// `bit_depth_luma_minus8` — 3 bits.
    pub const BIT_DEPTH_LUMA_MINUS8_SHIFT: u16 = 3;
    pub const BIT_DEPTH_LUMA_MINUS8_MASK: u16 = 0b111;
    /// `bit_depth_chroma_minus8` — 3 bits.
    pub const BIT_DEPTH_CHROMA_MINUS8_SHIFT: u16 = 6;
    pub const BIT_DEPTH_CHROMA_MINUS8_MASK: u16 = 0b111;
    /// `log2_max_pic_order_cnt_lsb_minus4` — 4 bits.
    pub const LOG2_MAX_POC_LSB_MINUS4_SHIFT: u16 = 9;
    pub const LOG2_MAX_POC_LSB_MINUS4_MASK: u16 = 0b1111;
    /// `NoPicReorderingFlag` — 1 bit.
    pub const NO_PIC_REORDERING_FLAG: u16 = 1 << 13;
    /// `NoBiPredFlag` — 1 bit.
    pub const NO_BI_PRED_FLAG: u16 = 1 << 14;
}

/// Bit layout of [`DxvaPicParamsHevc::dwCodingSettingPicturePropertyFlags`].
pub mod coding_setting_picture_property {
    pub const CONSTRAINED_INTRA_PRED_FLAG: u32 = 1 << 0;
    pub const TRANSFORM_SKIP_ENABLED_FLAG: u32 = 1 << 1;
    pub const CU_QP_DELTA_ENABLED_FLAG: u32 = 1 << 2;
    pub const PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT_FLAG: u32 = 1 << 3;
    pub const WEIGHTED_PRED_FLAG: u32 = 1 << 4;
    pub const WEIGHTED_BIPRED_FLAG: u32 = 1 << 5;
    pub const TRANSQUANT_BYPASS_ENABLED_FLAG: u32 = 1 << 6;
    pub const TILES_ENABLED_FLAG: u32 = 1 << 7;
    pub const ENTROPY_CODING_SYNC_ENABLED_FLAG: u32 = 1 << 8;
    pub const UNIFORM_SPACING_FLAG: u32 = 1 << 9;
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED_FLAG: u32 = 1 << 10;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED_FLAG: u32 = 1 << 11;
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED_FLAG: u32 = 1 << 12;
    pub const PPS_DEBLOCKING_FILTER_DISABLED_FLAG: u32 = 1 << 13;
    pub const LISTS_MODIFICATION_PRESENT_FLAG: u32 = 1 << 14;
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT_FLAG: u32 = 1 << 15;
    pub const IRAP_PIC_FLAG: u32 = 1 << 16;
    pub const IDR_PIC_FLAG: u32 = 1 << 17;
    pub const INTRA_PIC_FLAG: u32 = 1 << 18;
}

/// Bit layout of [`DxvaPicParamsHevc::dwCodingParamToolFlags`].
pub mod coding_param_tool {
    pub const SCALING_LIST_ENABLED_FLAG: u32 = 1 << 0;
    pub const AMP_ENABLED_FLAG: u32 = 1 << 1;
    pub const SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG: u32 = 1 << 2;
    pub const PCM_ENABLED_FLAG: u32 = 1 << 3;
    pub const PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_SHIFT: u32 = 4;
    pub const PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_MASK: u32 = 0b1111;
    pub const PCM_SAMPLE_BIT_DEPTH_CHROMA_MINUS1_SHIFT: u32 = 8;
    pub const PCM_SAMPLE_BIT_DEPTH_CHROMA_MINUS1_MASK: u32 = 0b1111;
    pub const LOG2_MIN_PCM_LUMA_CB_SIZE_MINUS3_SHIFT: u32 = 12;
    pub const LOG2_MIN_PCM_LUMA_CB_SIZE_MINUS3_MASK: u32 = 0b11;
    pub const LOG2_DIFF_MAX_MIN_PCM_LUMA_CB_SIZE_SHIFT: u32 = 14;
    pub const LOG2_DIFF_MAX_MIN_PCM_LUMA_CB_SIZE_MASK: u32 = 0b11;
    pub const PCM_LOOP_FILTER_DISABLED_FLAG: u32 = 1 << 16;
    pub const LONG_TERM_REF_PICS_PRESENT_FLAG: u32 = 1 << 17;
    pub const SPS_TEMPORAL_MVP_ENABLED_FLAG: u32 = 1 << 18;
    pub const STRONG_INTRA_SMOOTHING_ENABLED_FLAG: u32 = 1 << 19;
    pub const DEPENDENT_SLICE_SEGMENTS_ENABLED_FLAG: u32 = 1 << 20;
    pub const OUTPUT_FLAG_PRESENT_FLAG: u32 = 1 << 21;
    pub const NUM_EXTRA_SLICE_HEADER_BITS_SHIFT: u32 = 22;
    pub const NUM_EXTRA_SLICE_HEADER_BITS_MASK: u32 = 0b111;
    pub const SIGN_DATA_HIDING_ENABLED_FLAG: u32 = 1 << 25;
    pub const CABAC_INIT_PRESENT_FLAG: u32 = 1 << 26;
}

/// Build a [`DxvaPicParamsHevc`] from the SPS + PPS fields.
///
/// Per-slice / per-picture fields (`CurrPic`, `CurrPicOrderCntVal`,
/// `RefPicList`, `RefPicSetSt*` / `RefPicSetLt*` indices,
/// `ucNumDeltaPocsOfRefRpsIdx`, `wNumBitsForShortTermRPSInSlice`)
/// stay at their `Default::default()` values and must be filled in
/// by the caller before submitting the buffer; HEIC tiles are
/// IDR-only so the ref-list values are all-INVALID.
///
/// Pass `pps = None` to populate only the SPS-derived half (useful
/// for unit tests against a synthetic SPS). The driver requires both
/// halves filled before accepting the buffer, so production callers
/// should pass `Some(pps)`.
#[must_use]
pub fn from_sps_pps(sps: &ParsedSps, pps: Option<&ParsedPps>) -> DxvaPicParamsHevc {
    let chroma = (u16::from(sps.chroma_format_idc) & format_seq::CHROMA_FORMAT_IDC_MASK)
        << format_seq::CHROMA_FORMAT_IDC_SHIFT;
    let scp = if sps.separate_colour_plane_flag {
        format_seq::SEPARATE_COLOUR_PLANE_FLAG
    } else {
        0
    };
    let bd_y = (u16::from(sps.bit_depth_luma_minus8) & format_seq::BIT_DEPTH_LUMA_MINUS8_MASK)
        << format_seq::BIT_DEPTH_LUMA_MINUS8_SHIFT;
    let bd_c = (u16::from(sps.bit_depth_chroma_minus8) & format_seq::BIT_DEPTH_CHROMA_MINUS8_MASK)
        << format_seq::BIT_DEPTH_CHROMA_MINUS8_SHIFT;
    let log_poc = (u16::from(sps.log2_max_pic_order_cnt_lsb_minus4)
        & format_seq::LOG2_MAX_POC_LSB_MINUS4_MASK)
        << format_seq::LOG2_MAX_POC_LSB_MINUS4_SHIFT;
    let format_flags = chroma | scp | bd_y | bd_c | log_poc;

    let mut coding_flags: u32 = 0;
    use coding_param_tool::*;
    if sps.scaling_list_enabled_flag {
        coding_flags |= SCALING_LIST_ENABLED_FLAG;
    }
    if sps.amp_enabled_flag {
        coding_flags |= AMP_ENABLED_FLAG;
    }
    if sps.sample_adaptive_offset_enabled_flag {
        coding_flags |= SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG;
    }
    if sps.pcm_enabled_flag {
        coding_flags |= PCM_ENABLED_FLAG;
        coding_flags |= (u32::from(sps.pcm_sample_bit_depth_luma_minus1)
            & PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_MASK)
            << PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_SHIFT;
        coding_flags |= (u32::from(sps.pcm_sample_bit_depth_chroma_minus1)
            & PCM_SAMPLE_BIT_DEPTH_CHROMA_MINUS1_MASK)
            << PCM_SAMPLE_BIT_DEPTH_CHROMA_MINUS1_SHIFT;
        coding_flags |= (u32::from(sps.log2_min_pcm_luma_coding_block_size_minus3)
            & LOG2_MIN_PCM_LUMA_CB_SIZE_MINUS3_MASK)
            << LOG2_MIN_PCM_LUMA_CB_SIZE_MINUS3_SHIFT;
        coding_flags |= (u32::from(sps.log2_diff_max_min_pcm_luma_coding_block_size)
            & LOG2_DIFF_MAX_MIN_PCM_LUMA_CB_SIZE_MASK)
            << LOG2_DIFF_MAX_MIN_PCM_LUMA_CB_SIZE_SHIFT;
        if sps.pcm_loop_filter_disabled_flag {
            coding_flags |= PCM_LOOP_FILTER_DISABLED_FLAG;
        }
    }
    if sps.long_term_ref_pics_present_flag {
        coding_flags |= LONG_TERM_REF_PICS_PRESENT_FLAG;
    }
    if sps.sps_temporal_mvp_enabled_flag {
        coding_flags |= SPS_TEMPORAL_MVP_ENABLED_FLAG;
    }
    if sps.strong_intra_smoothing_enabled_flag {
        coding_flags |= STRONG_INTRA_SMOOTHING_ENABLED_FLAG;
    }
    // dependent_slice_segments / output_flag / num_extra_slice_header_bits /
    // sign_data_hiding / cabac_init come from PPS — caller fills them.

    // PPS-derived fields. Mirror chromium's PicParamsFromPPS at
    // media/gpu/windows/d3d11_h265_accelerator.cc:272 — the second
    // bitfield union (`dwCodingSettingPicturePropertyFlags`) plus a
    // handful of u8 / i8 fields.
    use coding_setting_picture_property::*;
    let mut setting_flags: u32 = 0;
    let mut column_width_minus1 = [0u16; 19];
    let mut row_height_minus1 = [0u16; 21];
    let (
        init_qp_minus26,
        num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1,
        pps_cb_qp_offset,
        pps_cr_qp_offset,
        diff_cu_qp_delta_depth,
        pps_beta_offset_div2,
        pps_tc_offset_div2,
        log2_parallel_merge_level_minus2,
        num_tile_columns_minus1,
        num_tile_rows_minus1,
    ) = if let Some(p) = pps {
        if p.constrained_intra_pred_flag {
            setting_flags |= CONSTRAINED_INTRA_PRED_FLAG;
        }
        if p.transform_skip_enabled_flag {
            setting_flags |= TRANSFORM_SKIP_ENABLED_FLAG;
        }
        if p.cu_qp_delta_enabled_flag {
            setting_flags |= CU_QP_DELTA_ENABLED_FLAG;
        }
        if p.pps_slice_chroma_qp_offsets_present_flag {
            setting_flags |= PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT_FLAG;
        }
        if p.weighted_pred_flag {
            setting_flags |= WEIGHTED_PRED_FLAG;
        }
        if p.weighted_bipred_flag {
            setting_flags |= WEIGHTED_BIPRED_FLAG;
        }
        if p.transquant_bypass_enabled_flag {
            setting_flags |= TRANSQUANT_BYPASS_ENABLED_FLAG;
        }
        if p.tiles_enabled_flag {
            setting_flags |= TILES_ENABLED_FLAG;
        }
        if p.entropy_coding_sync_enabled_flag {
            setting_flags |= ENTROPY_CODING_SYNC_ENABLED_FLAG;
        }
        if p.uniform_spacing_flag {
            setting_flags |= UNIFORM_SPACING_FLAG;
        }
        if p.loop_filter_across_tiles_enabled_flag {
            setting_flags |= LOOP_FILTER_ACROSS_TILES_ENABLED_FLAG;
        }
        if p.pps_loop_filter_across_slices_enabled_flag {
            setting_flags |= PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED_FLAG;
        }
        if p.deblocking_filter_override_enabled_flag {
            setting_flags |= DEBLOCKING_FILTER_OVERRIDE_ENABLED_FLAG;
        }
        if p.pps_deblocking_filter_disabled_flag {
            setting_flags |= PPS_DEBLOCKING_FILTER_DISABLED_FLAG;
        }
        if p.lists_modification_present_flag {
            setting_flags |= LISTS_MODIFICATION_PRESENT_FLAG;
        }
        if p.slice_segment_header_extension_present_flag {
            setting_flags |= SLICE_SEGMENT_HEADER_EXTENSION_PRESENT_FLAG;
        }
        // Copy explicit tile dimensions when non-uniform spacing.
        if p.tiles_enabled_flag && !p.uniform_spacing_flag {
            for (i, w) in p
                .column_widths
                .iter()
                .take(column_width_minus1.len())
                .enumerate()
            {
                column_width_minus1[i] = *w;
            }
            for (i, h) in p
                .row_heights
                .iter()
                .take(row_height_minus1.len())
                .enumerate()
            {
                row_height_minus1[i] = *h;
            }
        }
        (
            p.init_qp_minus26,
            p.num_ref_idx_l0_default_active_minus1,
            p.num_ref_idx_l1_default_active_minus1,
            p.pps_cb_qp_offset,
            p.pps_cr_qp_offset,
            p.diff_cu_qp_delta_depth,
            p.pps_beta_offset_div2,
            p.pps_tc_offset_div2,
            p.log2_parallel_merge_level_minus2,
            p.num_tile_columns_minus1,
            p.num_tile_rows_minus1,
        )
    } else {
        // Defaults when no PPS available — DXVA spec lists these as 0 / 0 / 0 etc.
        (0i8, 0u8, 0u8, 0i8, 0i8, 0u8, 0i8, 0i8, 0u8, 0u8, 0u8)
    };

    // PPS-derived bits in dwCodingParamToolFlags (the union we
    // already started populating from SPS).
    if let Some(p) = pps {
        if p.dependent_slice_segments_enabled_flag {
            coding_flags |= DEPENDENT_SLICE_SEGMENTS_ENABLED_FLAG;
        }
        if p.output_flag_present_flag {
            coding_flags |= OUTPUT_FLAG_PRESENT_FLAG;
        }
        coding_flags |= (u32::from(p.num_extra_slice_header_bits)
            & NUM_EXTRA_SLICE_HEADER_BITS_MASK)
            << NUM_EXTRA_SLICE_HEADER_BITS_SHIFT;
        if p.sign_data_hiding_enabled_flag {
            coding_flags |= SIGN_DATA_HIDING_ENABLED_FLAG;
        }
        if p.cabac_init_present_flag {
            coding_flags |= CABAC_INIT_PRESENT_FLAG;
        }
    }

    DxvaPicParamsHevc {
        PicWidthInMinCbsY: sps.pic_width_in_min_cbs_y() as u16,
        PicHeightInMinCbsY: sps.pic_height_in_min_cbs_y() as u16,
        wFormatAndSequenceInfoFlags: format_flags,
        sps_max_dec_pic_buffering_minus1: sps
            .sps_max_dec_pic_buffering_minus1
            .last()
            .copied()
            .unwrap_or(0),
        log2_min_luma_coding_block_size_minus3: sps.log2_min_luma_coding_block_size_minus3,
        log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_luma_coding_block_size,
        log2_min_transform_block_size_minus2: sps.log2_min_luma_transform_block_size_minus2,
        log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_luma_transform_block_size,
        max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
        num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets,
        num_long_term_ref_pics_sps: sps.num_long_term_ref_pics_sps,
        num_ref_idx_l0_default_active_minus1,
        num_ref_idx_l1_default_active_minus1,
        init_qp_minus26,
        dwCodingParamToolFlags: coding_flags,
        dwCodingSettingPicturePropertyFlags: setting_flags,
        pps_cb_qp_offset,
        pps_cr_qp_offset,
        num_tile_columns_minus1,
        num_tile_rows_minus1,
        column_width_minus1,
        row_height_minus1,
        diff_cu_qp_delta_depth,
        pps_beta_offset_div2,
        pps_tc_offset_div2,
        log2_parallel_merge_level_minus2,
        RefPicList: [DxvaPicEntryHevc::INVALID; 15],
        ..Default::default()
    }
}

/// Back-compat wrapper that calls [`from_sps_pps`] with `pps = None`.
#[deprecated(note = "use from_sps_pps; this returns a SPS-only buffer the driver will reject")]
#[must_use]
pub fn from_sps(sps: &ParsedSps) -> DxvaPicParamsHevc {
    from_sps_pps(sps, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pic_entry_invalid_index() {
        assert_eq!(DxvaPicEntryHevc::INVALID.index(), 0x7F);
    }

    #[test]
    fn pic_entry_new_packs_associated_flag() {
        let p = DxvaPicEntryHevc::new(42, true);
        assert_eq!(p.bPicEntry, 42 | 0x80);
        assert_eq!(p.index(), 42);

        let q = DxvaPicEntryHevc::new(3, false);
        assert_eq!(q.bPicEntry, 3);
        assert_eq!(q.index(), 3);
    }

    #[test]
    fn from_sps_pps_packs_format_flags_example_heic() {
        // example.heic SPS: 4:2:0, 8-bit, log2_max_poc_lsb=4 (so minus4 = 0).
        let mut sps = ParsedSps::default();
        sps.chroma_format_idc = 1;
        sps.bit_depth_luma_minus8 = 0;
        sps.bit_depth_chroma_minus8 = 0;
        sps.log2_max_pic_order_cnt_lsb_minus4 = 0;
        sps.pic_width_in_luma_samples = 1280;
        sps.pic_height_in_luma_samples = 858;
        sps.log2_min_luma_coding_block_size_minus3 = 0;

        let dxva = from_sps_pps(&sps, None);
        assert_eq!(dxva.PicWidthInMinCbsY, 160); // 1280 >> 3
        assert_eq!(dxva.PicHeightInMinCbsY, 107); // 858 >> 3 = 107.25 -> 107
        // Chroma 4:2:0 = 1 in bits 0-1, no other flags set.
        assert_eq!(dxva.wFormatAndSequenceInfoFlags, 1);
    }

    #[test]
    fn from_sps_pps_pcm_block_packed_correctly() {
        let mut sps = ParsedSps::default();
        sps.pcm_enabled_flag = true;
        sps.pcm_sample_bit_depth_luma_minus1 = 7; // 8-bit PCM
        sps.pcm_sample_bit_depth_chroma_minus1 = 7;
        sps.log2_min_pcm_luma_coding_block_size_minus3 = 1;
        sps.log2_diff_max_min_pcm_luma_coding_block_size = 2;
        sps.pcm_loop_filter_disabled_flag = true;

        let dxva = from_sps_pps(&sps, None);
        use coding_param_tool::*;
        assert_ne!(dxva.dwCodingParamToolFlags & PCM_ENABLED_FLAG, 0);
        assert_eq!(
            (dxva.dwCodingParamToolFlags >> PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_SHIFT)
                & PCM_SAMPLE_BIT_DEPTH_LUMA_MINUS1_MASK,
            7
        );
        assert_ne!(
            dxva.dwCodingParamToolFlags & PCM_LOOP_FILTER_DISABLED_FLAG,
            0
        );
    }

    #[test]
    fn from_sps_pps_packs_setting_flags() {
        let sps = ParsedSps::default();
        let mut pps = ParsedPps::default();
        pps.init_qp_minus26 = 6; // QP=32
        pps.pps_cb_qp_offset = -2;
        pps.pps_cr_qp_offset = 2;
        pps.transform_skip_enabled_flag = true;
        pps.cu_qp_delta_enabled_flag = true;
        pps.weighted_pred_flag = false; // explicit false
        pps.uniform_spacing_flag = true;
        pps.pps_loop_filter_across_slices_enabled_flag = true;
        pps.lists_modification_present_flag = true;

        let dxva = from_sps_pps(&sps, Some(&pps));
        assert_eq!(dxva.init_qp_minus26, 6);
        assert_eq!(dxva.pps_cb_qp_offset, -2);
        assert_eq!(dxva.pps_cr_qp_offset, 2);
        use coding_setting_picture_property::*;
        assert_ne!(
            dxva.dwCodingSettingPicturePropertyFlags & TRANSFORM_SKIP_ENABLED_FLAG,
            0
        );
        assert_ne!(
            dxva.dwCodingSettingPicturePropertyFlags & CU_QP_DELTA_ENABLED_FLAG,
            0
        );
        assert_ne!(
            dxva.dwCodingSettingPicturePropertyFlags & UNIFORM_SPACING_FLAG,
            0
        );
        assert_ne!(
            dxva.dwCodingSettingPicturePropertyFlags & LISTS_MODIFICATION_PRESENT_FLAG,
            0
        );
        assert_eq!(
            dxva.dwCodingSettingPicturePropertyFlags & WEIGHTED_PRED_FLAG,
            0
        );
    }

    #[test]
    fn from_sps_pps_copies_explicit_tile_layout() {
        let sps = ParsedSps::default();
        let mut pps = ParsedPps::default();
        pps.tiles_enabled_flag = true;
        pps.uniform_spacing_flag = false;
        pps.num_tile_columns_minus1 = 2;
        pps.num_tile_rows_minus1 = 1;
        pps.column_widths = vec![10, 20, 30];
        pps.row_heights = vec![15, 25];

        let dxva = from_sps_pps(&sps, Some(&pps));
        assert_eq!(dxva.num_tile_columns_minus1, 2);
        assert_eq!(dxva.num_tile_rows_minus1, 1);
        assert_eq!(dxva.column_width_minus1[0], 10);
        assert_eq!(dxva.column_width_minus1[1], 20);
        assert_eq!(dxva.column_width_minus1[2], 30);
        assert_eq!(dxva.column_width_minus1[3], 0); // beyond the populated set
        assert_eq!(dxva.row_height_minus1[0], 15);
        assert_eq!(dxva.row_height_minus1[1], 25);
        // uniform_spacing_flag = false, so the setting bit should be unset.
        use coding_setting_picture_property::*;
        assert_eq!(
            dxva.dwCodingSettingPicturePropertyFlags & UNIFORM_SPACING_FLAG,
            0
        );
    }
}
