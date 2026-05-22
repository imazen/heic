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
#![allow(missing_docs)] // documented inline via comments + the module headers

use heic_core::sps::ParsedSps;

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

/// Build a partially-populated [`DxvaPicParamsHevc`] from the SPS fields.
///
/// Only the SPS-derived fields are populated; PPS-derived fields
/// (tile layout, deblocking offsets, weighted prediction) plus
/// per-slice / per-picture fields (`CurrPic`, `CurrPicOrderCntVal`,
/// `RefPicList`) stay at their `Default::default()` values and must
/// be filled in by the caller before submitting the buffer.
#[must_use]
pub fn from_sps(sps: &ParsedSps) -> DxvaPicParamsHevc {
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

    DxvaPicParamsHevc {
        PicWidthInMinCbsY: sps.pic_width_in_min_cbs_y() as u16,
        PicHeightInMinCbsY: sps.pic_height_in_min_cbs_y() as u16,
        wFormatAndSequenceInfoFlags: format_flags,
        // sps_max_dec_pic_buffering_minus1 is per-TID; HEIC tiles always
        // use TID 0 (single-layer). If the ParsedSps vec is empty, use 0.
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
        dwCodingParamToolFlags: coding_flags,
        // RefPicList is sized for inter-prediction; HEIC tiles are
        // IDR-only so every entry is INVALID. Set explicitly so the
        // driver doesn't read garbage references.
        RefPicList: [DxvaPicEntryHevc::INVALID; 15],
        ..Default::default()
    }
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
    fn from_sps_packs_format_flags_example_heic() {
        // example.heic SPS: 4:2:0, 8-bit, log2_max_poc_lsb=4 (so minus4 = 0).
        let mut sps = ParsedSps::default();
        sps.chroma_format_idc = 1;
        sps.bit_depth_luma_minus8 = 0;
        sps.bit_depth_chroma_minus8 = 0;
        sps.log2_max_pic_order_cnt_lsb_minus4 = 0;
        sps.pic_width_in_luma_samples = 1280;
        sps.pic_height_in_luma_samples = 858;
        sps.log2_min_luma_coding_block_size_minus3 = 0;

        let dxva = from_sps(&sps);
        assert_eq!(dxva.PicWidthInMinCbsY, 160); // 1280 >> 3
        assert_eq!(dxva.PicHeightInMinCbsY, 107); // 858 >> 3 = 107.25 -> 107
        // Chroma 4:2:0 = 1 in bits 0-1, no other flags set.
        assert_eq!(dxva.wFormatAndSequenceInfoFlags, 1);
    }

    #[test]
    fn from_sps_pcm_block_packed_correctly() {
        let mut sps = ParsedSps::default();
        sps.pcm_enabled_flag = true;
        sps.pcm_sample_bit_depth_luma_minus1 = 7; // 8-bit PCM
        sps.pcm_sample_bit_depth_chroma_minus1 = 7;
        sps.log2_min_pcm_luma_coding_block_size_minus3 = 1;
        sps.log2_diff_max_min_pcm_luma_coding_block_size = 2;
        sps.pcm_loop_filter_disabled_flag = true;

        let dxva = from_sps(&sps);
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
}
