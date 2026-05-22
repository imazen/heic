//! Parsed-SPS field set that native backends need to populate their
//! picture-parameter buffers (DXVA_PicParams_HEVC for D3D11VA,
//! VAPictureParameterBufferHEVC for VA-API).
//!
//! `heic-core` defines the data type; the parent `heic` crate populates
//! it by running its in-tree SPS parser. Native backends consume the
//! populated [`ParsedSps`] via [`crate::HvccParams::sps`] without
//! re-parsing the bitstream.
//!
//! # Field selection
//!
//! The fields here are the union of what
//! `media/gpu/windows/d3d11_h265_accelerator.cc::PicParamsFromSPS` (chromium)
//! and `media/gpu/vaapi/h265_vaapi_video_decoder_delegate.cc::FillPicParams`
//! pull off the SPS — about 35 fields for HEVC Main / Main10. The
//! Range-Extension fields (cross-component prediction, transform skip
//! rotation) are documented but optional; both probes in
//! `heic-backend-{vaapi,d3d11va}` are Main/Main10 only.
//!
//! # CICP color-info reminder
//!
//! Color metadata (`full_range`, `matrix_coeffs`, `color_primaries`,
//! `transfer_characteristics`) lives on [`crate::HvccParams`] directly,
//! not here — backends populate those into [`crate::DecodedFrame`]
//! after decode, not into the picture parameter buffer.

use alloc::vec::Vec;

/// Parsed HEVC SPS fields required by native backends to populate
/// picture-parameter buffers (DXVA_PicParams_HEVC, VAPictureParameter
/// BufferHEVC).
///
/// All fields are documented in ITU-T H.265 §7.3.2.2 (`seq_parameter_set_rbsp`).
/// The parent crate's `crate::hevc::params::parse_sps` populates this
/// struct after stripping emulation-prevention bytes; native backends
/// consume it through [`crate::HvccParams::sps`] without re-parsing.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ParsedSps {
    /// `chroma_format_idc` — 0 (monochrome), 1 (4:2:0), 2 (4:2:2), 3 (4:4:4).
    pub chroma_format_idc: u8,
    /// `separate_colour_plane_flag` — only when chroma_format_idc=3.
    pub separate_colour_plane_flag: bool,
    /// `pic_width_in_luma_samples` (SPS coded width).
    pub pic_width_in_luma_samples: u32,
    /// `pic_height_in_luma_samples` (SPS coded height).
    pub pic_height_in_luma_samples: u32,
    /// `bit_depth_y - 8`. So a Main bitstream has 0; Main10 has 2.
    pub bit_depth_luma_minus8: u8,
    /// `bit_depth_c - 8`. Equal to `bit_depth_luma_minus8` for HEIC.
    pub bit_depth_chroma_minus8: u8,
    /// `log2_max_pic_order_cnt_lsb_minus4`.
    pub log2_max_pic_order_cnt_lsb_minus4: u8,
    /// `sps_max_sub_layers_minus1` (highest temporal id).
    pub sps_max_sub_layers_minus1: u8,
    /// Per-sub-layer DPB sizes from `sps_max_dec_pic_buffering_minus1[i]`.
    /// HEIC tiles always use index 0 (single-layer); the array is sized
    /// for the highest sub-layer when populated.
    pub sps_max_dec_pic_buffering_minus1: Vec<u8>,
    /// `log2_min_luma_coding_block_size_minus3`.
    pub log2_min_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_luma_coding_block_size`.
    pub log2_diff_max_min_luma_coding_block_size: u8,
    /// `log2_min_luma_transform_block_size_minus2`.
    pub log2_min_luma_transform_block_size_minus2: u8,
    /// `log2_diff_max_min_luma_transform_block_size`.
    pub log2_diff_max_min_luma_transform_block_size: u8,
    /// `max_transform_hierarchy_depth_inter`.
    pub max_transform_hierarchy_depth_inter: u8,
    /// `max_transform_hierarchy_depth_intra`.
    pub max_transform_hierarchy_depth_intra: u8,
    /// `scaling_list_enabled_flag`.
    pub scaling_list_enabled_flag: bool,
    /// `amp_enabled_flag` (asymmetric motion partitions).
    pub amp_enabled_flag: bool,
    /// `sample_adaptive_offset_enabled_flag`.
    pub sample_adaptive_offset_enabled_flag: bool,
    /// `pcm_enabled_flag`.
    pub pcm_enabled_flag: bool,
    /// `pcm_sample_bit_depth_luma_minus1` (only when pcm_enabled).
    pub pcm_sample_bit_depth_luma_minus1: u8,
    /// `pcm_sample_bit_depth_chroma_minus1` (only when pcm_enabled).
    pub pcm_sample_bit_depth_chroma_minus1: u8,
    /// `log2_min_pcm_luma_coding_block_size_minus3` (only when pcm_enabled).
    pub log2_min_pcm_luma_coding_block_size_minus3: u8,
    /// `log2_diff_max_min_pcm_luma_coding_block_size` (only when pcm_enabled).
    pub log2_diff_max_min_pcm_luma_coding_block_size: u8,
    /// `pcm_loop_filter_disabled_flag` (only when pcm_enabled).
    pub pcm_loop_filter_disabled_flag: bool,
    /// `num_short_term_ref_pic_sets`.
    pub num_short_term_ref_pic_sets: u8,
    /// `num_long_term_ref_pics_sps`.
    pub num_long_term_ref_pics_sps: u8,
    /// `long_term_ref_pics_present_flag`.
    pub long_term_ref_pics_present_flag: bool,
    /// `sps_temporal_mvp_enabled_flag`.
    pub sps_temporal_mvp_enabled_flag: bool,
    /// `strong_intra_smoothing_enabled_flag`.
    pub strong_intra_smoothing_enabled_flag: bool,
    /// `conformance_window_flag`.
    pub conformance_window_flag: bool,
    /// Conformance window offsets in chroma-subsampling units (NOT luma
    /// samples). Order: (left, right, top, bottom). To get luma-sample
    /// crop multiply by SubWidthC / SubHeightC per chroma_format_idc.
    /// The pre-multiplied luma-sample values are already exposed on
    /// [`crate::HvccParams`] as `crop_*` for convenience; this raw form
    /// is preserved here for the native picture-parameter buffer fields
    /// that DXVA / libva consume directly.
    pub conf_win_offset: (u32, u32, u32, u32),
    /// SPS range-extension flag. When true, `range_extension` carries
    /// the additional RExt fields.
    pub sps_range_extension_flag: bool,
    /// Range-extension fields (only meaningful when
    /// `sps_range_extension_flag = true`). HEIC main-profile streams
    /// leave this `Default::default()`.
    pub range_extension: SpsRangeExtension,
}

/// SPS Range Extension fields — populated only when
/// [`ParsedSps::sps_range_extension_flag`] is true. HEIC main-profile
/// bitstreams set the flag to false, in which case every field here is
/// at its default (0 / false).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SpsRangeExtension {
    /// `transform_skip_rotation_enabled_flag`.
    pub transform_skip_rotation_enabled_flag: bool,
    /// `transform_skip_context_enabled_flag`.
    pub transform_skip_context_enabled_flag: bool,
    /// `implicit_rdpcm_enabled_flag`.
    pub implicit_rdpcm_enabled_flag: bool,
    /// `explicit_rdpcm_enabled_flag`.
    pub explicit_rdpcm_enabled_flag: bool,
    /// `extended_precision_processing_flag`.
    pub extended_precision_processing_flag: bool,
    /// `intra_smoothing_disabled_flag`.
    pub intra_smoothing_disabled_flag: bool,
    /// `high_precision_offsets_enabled_flag`.
    pub high_precision_offsets_enabled_flag: bool,
    /// `persistent_rice_adaptation_enabled_flag`.
    pub persistent_rice_adaptation_enabled_flag: bool,
    /// `cabac_bypass_alignment_enabled_flag`.
    pub cabac_bypass_alignment_enabled_flag: bool,
}

impl ParsedSps {
    /// Derive `min_cb_log2_size_y` per HEVC formula 7-14:
    /// `min_cb_log2_size_y = log2_min_luma_coding_block_size_minus3 + 3`.
    /// Used to compute `PicWidthInMinCbsY` for DXVA_PicParams_HEVC.
    #[must_use]
    pub fn min_cb_log2_size_y(&self) -> u8 {
        self.log2_min_luma_coding_block_size_minus3 + 3
    }

    /// `PicWidthInMinCbsY` per HEVC formula 7-14.
    #[must_use]
    pub fn pic_width_in_min_cbs_y(&self) -> u32 {
        self.pic_width_in_luma_samples >> self.min_cb_log2_size_y()
    }

    /// `PicHeightInMinCbsY` per HEVC formula 7-16.
    #[must_use]
    pub fn pic_height_in_min_cbs_y(&self) -> u32 {
        self.pic_height_in_luma_samples >> self.min_cb_log2_size_y()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_cb_log2_size_y_formula() {
        let mut sps = ParsedSps::default();
        sps.log2_min_luma_coding_block_size_minus3 = 0; // smallest legal value
        assert_eq!(sps.min_cb_log2_size_y(), 3); // 8x8 minimum CB

        sps.log2_min_luma_coding_block_size_minus3 = 3; // larger
        assert_eq!(sps.min_cb_log2_size_y(), 6); // 64x64
    }

    #[test]
    fn pic_width_in_min_cbs_y_example_heic() {
        // example.heic: pic_width=1280, min_cb=8 → 160 min-CBs across.
        let mut sps = ParsedSps::default();
        sps.pic_width_in_luma_samples = 1280;
        sps.log2_min_luma_coding_block_size_minus3 = 0;
        assert_eq!(sps.pic_width_in_min_cbs_y(), 160);
    }

    #[test]
    fn default_main_profile_has_no_rext() {
        let sps = ParsedSps::default();
        assert!(!sps.sps_range_extension_flag);
        assert!(!sps.range_extension.transform_skip_rotation_enabled_flag);
    }
}
