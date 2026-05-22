//! Rust mirror of libva's `VAPictureParameterBufferHEVC` (from
//! `va/va_dec_hevc.h`) plus an SPS-fields populator that builds it
//! from a [`heic_core::sps::ParsedSps`].
//!
//! libva's struct has ~50 fields including 15-entry reference picture
//! array, two bitfield unions (`pic_fields` for VUI/PPS bits and
//! `slice_parsing_fields` for slice-header bits), tile column/row
//! arrays, and the current picture identifier. The Rust mirror here
//! uses `#[repr(C)]` so it can be `transmute`'d into the actual libva
//! struct when the future `cros-libva`-feature-gated adapter lands —
//! field order matches libva's C declaration exactly.
//!
//! For now the populator works on the Rust mirror; the libva
//! `vaRenderPicture` submission path will copy or transmute when the
//! feature lands. Keeping the struct + populator independent of
//! `cros-libva` means systems without `libva-dev` still build cleanly
//! and the tests run on any target.
//!
//! Source: chromium `media/gpu/vaapi/h265_vaapi_video_decoder_delegate.cc::FillPicParams`
//! (sparse-checked-out at `~/work/chromium/media/gpu/vaapi/`), plus
//! libva's `va/va_dec_hevc.h`.

#![allow(non_snake_case)] // matches the libva C field names exactly
#![allow(missing_docs)] // documented inline via module headers

use heic_core::sps::ParsedSps;

/// libva `VAPictureHEVC` — one entry in the reference-frame array or
/// the current picture identifier.
///
/// Field layout matches `va/va_dec_hevc.h`. The `flags` bitfield uses
/// the documented constants from libva:
///
/// * `VA_PICTURE_HEVC_INVALID` = 0x01 — slot is empty.
/// * `VA_PICTURE_HEVC_LONG_TERM_REFERENCE` = 0x02.
/// * `VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE` = 0x10.
/// * `VA_PICTURE_HEVC_RPS_ST_CURR_AFTER` = 0x20.
/// * `VA_PICTURE_HEVC_RPS_LT_CURR` = 0x40.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VaPictureHevc {
    /// VASurfaceID handle for this picture's decoded surface.
    pub picture_id: u32,
    /// Picture-order-count value.
    pub pic_order_cnt: i32,
    /// Flags per the `VA_PICTURE_HEVC_*` constants above.
    pub flags: u32,
    /// Reserved — must be zero.
    pub reserved: [u32; 4],
}

impl VaPictureHevc {
    /// libva sentinel: the slot is empty / unused.
    pub const INVALID: Self = Self {
        picture_id: 0xFFFF_FFFF,
        pic_order_cnt: 0,
        flags: 0x01, // VA_PICTURE_HEVC_INVALID
        reserved: [0; 4],
    };
}

/// libva `VAPictureParameterBufferHEVC` Rust mirror. Field layout
/// must match the libva C declaration; see
/// `~/.cache/cargo-read/cros-libva-0.0.13/src/buffer/hevc.rs`'s
/// `PictureParameterBufferHEVC::new` signature for the canonical
/// 38-arg ctor that wraps this struct.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VaPictureParameterBufferHevc {
    /// Current picture identifier (VASurfaceID + POC + flags).
    pub CurrPic: VaPictureHevc,
    /// Reference picture array — 15 slots, all but the active set
    /// filled with `VaPictureHevc::INVALID`.
    pub ReferenceFrames: [VaPictureHevc; 15],
    /// Picture width in luma samples (SPS).
    pub pic_width_in_luma_samples: u16,
    /// Picture height in luma samples (SPS).
    pub pic_height_in_luma_samples: u16,
    /// Bitfield union — see [`pic_fields`] for layout.
    pub pic_fields: u32,
    /// Bitfield union — see [`slice_parsing_fields`] for layout.
    pub slice_parsing_fields: u32,
    /// `sps_max_dec_pic_buffering_minus1` at the SPS's highest TID.
    pub sps_max_dec_pic_buffering_minus1: u8,
    /// `bit_depth_luma_minus8`.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_chroma_minus8`.
    pub bit_depth_chroma_minus8: u8,
    /// `pcm_sample_bit_depth_luma_minus1` (only meaningful when
    /// `pcm_enabled_flag = 1`).
    pub pcm_sample_bit_depth_luma_minus1: u8,
    /// `pcm_sample_bit_depth_chroma_minus1`.
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_transform_block_size_minus2` (renamed from
    /// `log2_min_luma_transform_block_size_minus2` per libva).
    pub log2_min_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_transform_block_size`.
    pub log2_diff_max_min_transform_block_size: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3`.
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size`.
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `init_qp_minus26` (PPS-derived).
    pub init_qp_minus26: i8,
    /// `diff_cu_qp_delta_depth` (PPS-derived).
    pub diff_cu_qp_delta_depth: u8,
    /// `pps_cb_qp_offset`.
    pub pps_cb_qp_offset: i8,
    /// `pps_cr_qp_offset`.
    pub pps_cr_qp_offset: i8,
    /// `log2_parallel_merge_level_minus2`.
    pub log2_parallel_merge_level_minus2: u8,
    /// `num_tile_columns_minus1`.
    pub num_tile_columns_minus1: u8,
    /// `num_tile_rows_minus1`.
    pub num_tile_rows_minus1: u8,
    /// Per-column tile widths (only first `num_tile_columns_minus1 + 1` valid).
    pub column_width_minus1: [u16; 19],
    /// Per-row tile heights (only first `num_tile_rows_minus1 + 1` valid).
    pub row_height_minus1: [u16; 21],
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pic_sps`.
    pub num_long_term_ref_pic_sps: u8,
    /// `num_ref_idx_l0_default_active_minus1` (PPS).
    pub num_ref_idx_l0_default_active_minus1: u8,
    /// `num_ref_idx_l1_default_active_minus1` (PPS).
    pub num_ref_idx_l1_default_active_minus1: u8,
    /// `pps_beta_offset_div2`.
    pub pps_beta_offset_div2: i8,
    /// `pps_tc_offset_div2`.
    pub pps_tc_offset_div2: i8,
    /// `num_extra_slice_header_bits` (PPS).
    pub num_extra_slice_header_bits: u8,
    /// Number of bits used for the inline `short_term_ref_pic_set`
    /// (from slice header).
    pub st_rps_bits: u32,
    /// Reserved — must be zero.
    pub va_reserved: [u32; 8],
}

/// Bit layout of [`VaPictureParameterBufferHevc::pic_fields`].
///
/// Order taken from libva's `HevcPicFields` union in
/// `va/va_dec_hevc.h`.
pub mod pic_fields {
    pub const CHROMA_FORMAT_IDC_SHIFT: u32 = 0;
    pub const CHROMA_FORMAT_IDC_MASK: u32 = 0b11;
    pub const SEPARATE_COLOUR_PLANE_FLAG: u32 = 1 << 2;
    pub const PCM_ENABLED_FLAG: u32 = 1 << 3;
    pub const SCALING_LIST_ENABLED_FLAG: u32 = 1 << 4;
    pub const TRANSFORM_SKIP_ENABLED_FLAG: u32 = 1 << 5;
    pub const AMP_ENABLED_FLAG: u32 = 1 << 6;
    pub const STRONG_INTRA_SMOOTHING_ENABLED_FLAG: u32 = 1 << 7;
    pub const SIGN_DATA_HIDING_ENABLED_FLAG: u32 = 1 << 8;
    pub const CONSTRAINED_INTRA_PRED_FLAG: u32 = 1 << 9;
    pub const CU_QP_DELTA_ENABLED_FLAG: u32 = 1 << 10;
    pub const WEIGHTED_PRED_FLAG: u32 = 1 << 11;
    pub const WEIGHTED_BIPRED_FLAG: u32 = 1 << 12;
    pub const TRANSQUANT_BYPASS_ENABLED_FLAG: u32 = 1 << 13;
    pub const TILES_ENABLED_FLAG: u32 = 1 << 14;
    pub const ENTROPY_CODING_SYNC_ENABLED_FLAG: u32 = 1 << 15;
    pub const PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED_FLAG: u32 = 1 << 16;
    pub const LOOP_FILTER_ACROSS_TILES_ENABLED_FLAG: u32 = 1 << 17;
    pub const PCM_LOOP_FILTER_DISABLED_FLAG: u32 = 1 << 18;
    /// 2-bit field at bits 19-20. `0` for a single sub-layer (HEIC).
    pub const NO_PIC_REORDERING_FLAG: u32 = 1 << 19;
    pub const NO_BIPRED_FLAG: u32 = 1 << 20;
}

/// Bit layout of [`VaPictureParameterBufferHevc::slice_parsing_fields`].
pub mod slice_parsing_fields {
    pub const LISTS_MODIFICATION_PRESENT_FLAG: u32 = 1 << 0;
    pub const LONG_TERM_REF_PICS_PRESENT_FLAG: u32 = 1 << 1;
    pub const SPS_TEMPORAL_MVP_ENABLED_FLAG: u32 = 1 << 2;
    pub const CABAC_INIT_PRESENT_FLAG: u32 = 1 << 3;
    pub const OUTPUT_FLAG_PRESENT_FLAG: u32 = 1 << 4;
    pub const DEPENDENT_SLICE_SEGMENTS_ENABLED_FLAG: u32 = 1 << 5;
    pub const PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT_FLAG: u32 = 1 << 6;
    pub const SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG: u32 = 1 << 7;
    pub const DEBLOCKING_FILTER_OVERRIDE_ENABLED_FLAG: u32 = 1 << 8;
    pub const PPS_DISABLE_DEBLOCKING_FILTER_FLAG: u32 = 1 << 9;
    pub const SLICE_SEGMENT_HEADER_EXTENSION_PRESENT_FLAG: u32 = 1 << 10;
    pub const RAP_PIC_FLAG: u32 = 1 << 11;
    pub const IDR_PIC_FLAG: u32 = 1 << 12;
    pub const INTRA_PIC_FLAG: u32 = 1 << 13;
}

/// Build a partially-populated [`VaPictureParameterBufferHevc`] from
/// the SPS fields.
///
/// Only the ~22 SPS-derived fields are populated; PPS-derived fields
/// (tile layout, deblocking offsets, weighted prediction, init_qp) and
/// per-slice / per-picture fields (`CurrPic`, `ReferenceFrames`,
/// `st_rps_bits`, `IDR_PIC_FLAG` etc.) stay at `Default::default()`
/// and must be filled in by the caller before submitting the buffer.
///
/// HEIC-specific simplifications baked in:
///
/// * `ReferenceFrames` is initialized to `[VaPictureHevc::INVALID; 15]`
///   — HEIC tiles are IDR-only, no inter-frame references.
/// * The slice-parsing-fields `RAP_PIC_FLAG`, `IDR_PIC_FLAG`,
///   `INTRA_PIC_FLAG` should be set by the caller to 1 for HEIC tiles
///   (they're set per-picture, not from the SPS).
#[must_use]
pub fn from_sps(sps: &ParsedSps) -> VaPictureParameterBufferHevc {
    let mut pic_flags: u32 = 0;
    use pic_fields::*;
    pic_flags |=
        (u32::from(sps.chroma_format_idc) & CHROMA_FORMAT_IDC_MASK) << CHROMA_FORMAT_IDC_SHIFT;
    if sps.separate_colour_plane_flag {
        pic_flags |= SEPARATE_COLOUR_PLANE_FLAG;
    }
    if sps.pcm_enabled_flag {
        pic_flags |= PCM_ENABLED_FLAG;
    }
    if sps.scaling_list_enabled_flag {
        pic_flags |= SCALING_LIST_ENABLED_FLAG;
    }
    if sps.amp_enabled_flag {
        pic_flags |= AMP_ENABLED_FLAG;
    }
    if sps.strong_intra_smoothing_enabled_flag {
        pic_flags |= STRONG_INTRA_SMOOTHING_ENABLED_FLAG;
    }
    if sps.pcm_loop_filter_disabled_flag {
        pic_flags |= PCM_LOOP_FILTER_DISABLED_FLAG;
    }

    let mut slice_flags: u32 = 0;
    {
        use slice_parsing_fields::*;
        if sps.long_term_ref_pics_present_flag {
            slice_flags |= LONG_TERM_REF_PICS_PRESENT_FLAG;
        }
        if sps.sps_temporal_mvp_enabled_flag {
            slice_flags |= SPS_TEMPORAL_MVP_ENABLED_FLAG;
        }
        if sps.sample_adaptive_offset_enabled_flag {
            slice_flags |= SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG;
        }
        // RAP/IDR/INTRA_PIC_FLAG set by caller per-picture.
    }

    VaPictureParameterBufferHevc {
        ReferenceFrames: [VaPictureHevc::INVALID; 15],
        pic_width_in_luma_samples: sps.pic_width_in_luma_samples as u16,
        pic_height_in_luma_samples: sps.pic_height_in_luma_samples as u16,
        pic_fields: pic_flags,
        slice_parsing_fields: slice_flags,
        sps_max_dec_pic_buffering_minus1: sps
            .sps_max_dec_pic_buffering_minus1
            .last()
            .copied()
            .unwrap_or(0),
        bit_depth_luma_minus8: sps.bit_depth_luma_minus8,
        bit_depth_chroma_minus8: sps.bit_depth_chroma_minus8,
        pcm_sample_bit_depth_luma_minus1: sps.pcm_sample_bit_depth_luma_minus1,
        pcm_sample_bit_depth_chroma_minus1: sps.pcm_sample_bit_depth_chroma_minus1,
        log2_min_luma_coding_block_size_minus3: sps.log2_min_luma_coding_block_size_minus3,
        log2_diff_max_min_luma_coding_block_size: sps.log2_diff_max_min_luma_coding_block_size,
        log2_min_transform_block_size_minus2: sps.log2_min_luma_transform_block_size_minus2,
        log2_diff_max_min_transform_block_size: sps.log2_diff_max_min_luma_transform_block_size,
        log2_min_pcm_luma_coding_block_size_minus3: sps.log2_min_pcm_luma_coding_block_size_minus3,
        log2_diff_max_min_pcm_luma_coding_block_size: sps
            .log2_diff_max_min_pcm_luma_coding_block_size,
        max_transform_hierarchy_depth_intra: sps.max_transform_hierarchy_depth_intra,
        max_transform_hierarchy_depth_inter: sps.max_transform_hierarchy_depth_inter,
        log2_max_pic_order_cnt_lsb_minus4: sps.log2_max_pic_order_cnt_lsb_minus4,
        num_short_term_ref_pic_sets: sps.num_short_term_ref_pic_sets,
        num_long_term_ref_pic_sps: sps.num_long_term_ref_pics_sps,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_invalid_uses_libva_sentinel() {
        assert_eq!(VaPictureHevc::INVALID.flags & 0x01, 0x01);
        assert_eq!(VaPictureHevc::INVALID.picture_id, 0xFFFF_FFFF);
    }

    #[test]
    fn from_sps_packs_pic_fields_example_heic() {
        // example.heic: 4:2:0 (chroma_format_idc=1), 8-bit, AMP enabled.
        let mut sps = ParsedSps::default();
        sps.chroma_format_idc = 1;
        sps.bit_depth_luma_minus8 = 0;
        sps.bit_depth_chroma_minus8 = 0;
        sps.amp_enabled_flag = true;
        sps.scaling_list_enabled_flag = true;
        sps.pic_width_in_luma_samples = 1280;
        sps.pic_height_in_luma_samples = 858;

        let va = from_sps(&sps);
        assert_eq!(va.pic_width_in_luma_samples, 1280);
        assert_eq!(va.pic_height_in_luma_samples, 858);
        // chroma_format_idc=1 in bits 0-1, AMP at bit 6, scaling_list at bit 4.
        let expected = 1 | pic_fields::AMP_ENABLED_FLAG | pic_fields::SCALING_LIST_ENABLED_FLAG;
        assert_eq!(va.pic_fields, expected);
    }

    #[test]
    fn from_sps_initializes_reference_frames_to_invalid() {
        let sps = ParsedSps::default();
        let va = from_sps(&sps);
        // Every reference slot must be the libva-INVALID sentinel.
        for r in &va.ReferenceFrames {
            assert_eq!(r.flags & 0x01, 0x01);
        }
    }

    #[test]
    fn from_sps_packs_slice_parsing_sao_flag() {
        let mut sps = ParsedSps::default();
        sps.sample_adaptive_offset_enabled_flag = true;
        let va = from_sps(&sps);
        assert_ne!(
            va.slice_parsing_fields & slice_parsing_fields::SAMPLE_ADAPTIVE_OFFSET_ENABLED_FLAG,
            0
        );
    }
}
