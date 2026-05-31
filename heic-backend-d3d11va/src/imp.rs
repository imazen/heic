//! D3D11VA backend's `HevcBackend::decode_hevc` glue.
//!
//! Ties the per-stream [`DecoderSession`] (created lazily per
//! dimensions/bit-depth) to the parent crate's [`HvccParams`] +
//! Annex-B slice data + the heic-core [`DecodedFrame`] output the
//! parent's dispatcher consumes.
//!
//! Per-frame flow:
//!
//! 1. If no cached session matches the input's coded width/height +
//!    bit depth, build a fresh one.
//! 2. Convert the hvcC length-prefixed slice payload to Annex-B —
//!    the SPS/PPS NAL payloads are concatenated in front so the
//!    driver sees them as inline parameter sets, matching what
//!    Microsoft's MFT path does. The DXVA short-format slice
//!    control buffer references the whole concatenated blob.
//! 3. Build the picture-parameter buffer from the parsed SPS+PPS
//!    via `crate::dxva::from_sps_pps`.
//! 4. Submit (`DecoderBeginFrame` → buffers → `SubmitDecoderBuffers`
//!    → `DecoderEndFrame`).
//! 5. Read back NV12 / P010 via the staging texture into planar
//!    `u16` planes.
//! 6. Wrap into `DecodedFrame` with the same VUI / coded-dims /
//!    crop fields the MF backend writes.

#![cfg(target_os = "windows")]

use std::vec::Vec;

use heic_core::{BackendError, DecodedFrame, HvccParams};

use crate::decoder::DecoderSession;

#[derive(Default)]
pub(super) struct Inner {
    /// Cached decoder session — rebuilt when dimensions or bit depth
    /// change between calls.
    cached: Option<DecoderSession>,
}

impl Inner {
    pub(super) fn decode(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        _stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        // ParsedSps is mandatory — without it we can't fill the
        // picture-parameter buffer correctly. Fail loud rather than
        // submitting garbage.
        let sps = config.sps.ok_or_else(|| {
            BackendError::Decode(
                "heic-backend-d3d11va: HvccParams.sps is None — parent \
                 crate didn't parse SPS; need it to populate \
                 DxvaPicParamsHevc"
                    .to_string(),
            )
        })?;
        // Use the SPS-declared coded dimensions directly. Picking
        // anything else (e.g. the ispe-derived visible size) makes
        // the texture and PicWidthInMinCbsY disagree, and the driver
        // silently produces wrong content when the SPS coded size
        // exceeds the texture (it crops to the texture instead of
        // padding outward as the spec requires).
        let coded_w = sps.pic_width_in_luma_samples;
        let coded_h = sps.pic_height_in_luma_samples;
        let bit_depth = config.bit_depth_luma;

        // Rebuild on dimension / bit-depth mismatch (or first call).
        if self
            .cached
            .as_ref()
            .is_none_or(|s| !s.matches(coded_w, coded_h, bit_depth))
        {
            self.cached = Some(DecoderSession::new(coded_w, coded_h, bit_depth)?);
        }
        let session = self.cached.as_ref().expect("cached set above");
        let mut pic_params = crate::dxva::from_sps_pps(sps, config.pps);
        // Per-picture fields the SPS/PPS populator can't fill in:
        //
        // * `CurrPic` references our single output texture slice 0;
        //   leaving it INVALID confuses the driver about where to
        //   write decoded samples.
        // * For HEIC tiles every access unit is an IDR, so the
        //   IDR/IRAP/INTRA flags in
        //   `dwCodingSettingPicturePropertyFlags` are all true.
        //   Without them the driver thinks it needs reference frames
        //   we never provide and the output stays zero.
        // * `CurrPicOrderCntVal` is 0 for the first (only) tile in a
        //   single-IDR HEIC bitstream — no inter-frame POC scaling.
        pic_params.CurrPic = crate::dxva::DxvaPicEntryHevc::new(0, false);
        pic_params.CurrPicOrderCntVal = 0;
        // StatusReportFeedbackNumber must be non-zero per the DXVA
        // spec — drivers that don't use it tolerate 0, but Intel iHD
        // and recent NVIDIA drivers reject zero with a silent
        // no-op decode. Use a per-instance counter (just hash the
        // input length for now — each tile gets a different number).
        pic_params.StatusReportFeedbackNumber = (image_data.len() as u32).max(1);
        {
            use crate::dxva::coding_setting_picture_property::{
                IDR_PIC_FLAG, INTRA_PIC_FLAG, IRAP_PIC_FLAG,
            };
            pic_params.dwCodingSettingPicturePropertyFlags |=
                IDR_PIC_FLAG | IRAP_PIC_FLAG | INTRA_PIC_FLAG;
        }

        // Bitstream buffer for DXVA short-format slice control:
        // chromium uses a **3-byte** start code `{0, 0, 1}` before the
        // raw slice NAL bytes (not the 4-byte `{0, 0, 0, 1}` of standard
        // Annex-B). DXVA_Slice_HEVC_Short.BSNALunitDataLocation points
        // at offset 0 (the start code itself); the driver locates the
        // NAL header by scanning past the start code internally.
        //
        // The parameter sets (VPS/SPS/PPS) are NOT included in the
        // bitstream — they're already in the picture-parameter buffer.
        // VCL slice NAL types per HEVC spec Table 7-1: 0..=9 (trailing,
        // temporal sub-layer, RADL, RASL) and 16..=21 (BLA/IDR/CRA).
        // Everything else (VPS=32 / SPS=33 / PPS=34 / AUD=35 /
        // SEI prefix=39 / SEI suffix=40 / etc.) is non-VCL and must NOT
        // appear in the BITSTREAM buffer per the DXVA spec — submitting
        // them would put the driver's slice parser at the wrong offset
        // (the SEI NAL's payload, not the slice header), producing
        // midgray output. example.heic's full-image bitstream has a
        // SEI prefix in front of the IDR slice; filtering at this layer
        // is what makes that case work.
        fn is_vcl_nal_type(t: u8) -> bool {
            matches!(t, 0..=9 | 16..=21)
        }

        // Each VCL slice NAL becomes its own DXVA short-format slice
        // control entry: chromium's H265Decoder loop calls SubmitSlice
        // once per slice header, and each call appends one
        // DXVA_Slice_HEVC_Short via AppendBitstreamAndSliceDataWithStartCode
        // (media/gpu/windows/d3d_video_decoder_wrapper.cc). A tile with
        // multiple slice NALs therefore needs N entries, each pointing at
        // its own start-code-prefixed slice within the bitstream buffer.
        // Emitting one entry for the whole buffer (the previous behaviour)
        // made the driver decode only the first slice and ignore the
        // rest, corrupting multi-slice tiles. `slices` collects
        // (byte offset of the [0,0,1] start code, total bytes incl. start
        // code) per slice, in bitstream order.
        let mut bitstream: Vec<u8> = Vec::with_capacity(image_data.len() + 64);
        let mut slices: Vec<(u32, u32)> = Vec::new();
        let ls = config.length_size as usize;
        let mut i = 0;
        let mut nal_count = 0u32;
        let mut nal_type_first = 0u8;
        while i + ls <= image_data.len() {
            let mut nal_len: usize = 0;
            for &b in &image_data[i..i + ls] {
                nal_len = (nal_len << 8) | (b as usize);
            }
            i += ls;
            if i + nal_len > image_data.len() {
                return Err(BackendError::Decode(
                    "malformed hvcC length-prefixed slice data".into(),
                ));
            }
            let nal_type = if nal_len > 0 {
                (image_data[i] >> 1) & 0x3F
            } else {
                0
            };
            if nal_count == 0 {
                nal_type_first = nal_type;
            }
            if is_vcl_nal_type(nal_type) {
                let offset = u32::try_from(bitstream.len()).map_err(|_| {
                    BackendError::Decode("d3d11va: bitstream buffer exceeds u32 offset".into())
                })?;
                bitstream.extend_from_slice(&[0, 0, 1]); // 3-byte start code per chromium
                bitstream.extend_from_slice(&image_data[i..i + nal_len]);
                let slice_len = u32::try_from(nal_len + 3).map_err(|_| {
                    BackendError::Decode("d3d11va: slice NAL exceeds u32 length".into())
                })?;
                slices.push((offset, slice_len));
            }
            i += nal_len;
            nal_count += 1;
        }
        if slices.is_empty() {
            return Err(BackendError::Decode(
                "hvcC slice data contained no VCL NAL units".into(),
            ));
        }

        // When the SPS enables custom scaling lists, the DXVA spec
        // requires the host to submit an INVERSE_QUANTIZATION_MATRIX
        // buffer — without it, drivers vary between "use defaults
        // internally" (NVIDIA on simple synth streams) and "produce
        // mid-gray garbage" (NVIDIA on encoders that override defaults).
        //
        // Preference order per HEVC spec 7.4.3.3.1:
        //   1. PPS scaling lists when pps_scaling_list_data_present_flag = 1
        //   2. SPS scaling lists when sps_scaling_list_data_present_flag = 1
        //   3. HEVC default scaling lists otherwise
        let iq_matrix = if sps.scaling_list_enabled_flag {
            let custom = config
                .pps
                .and_then(|p| p.pps_scaling_list.as_ref())
                .or(sps.scaling_list.as_ref());
            Some(custom.map_or_else(
                crate::dxva::default_qmatrix_hevc,
                crate::dxva::qmatrix_from_parsed,
            ))
        } else {
            None
        };

        // Submit + read back. The DecoderSession's per-frame flow
        // documented in `decoder::DecoderSession::submit_one_frame`
        // handles BeginFrame → buffer Get/Release/Submit → EndFrame.
        session.submit_one_frame(&pic_params, &bitstream, &slices, iq_matrix.as_ref())?;

        let planes = session.read_decoded_planes(
            config.width,
            config.height,
            config.crop_left,
            config.crop_top,
        )?;

        if std::env::var_os("HEIC_D3D11VA_DEBUG").is_some() {
            let y0: Vec<_> = planes.y.iter().take(16).collect();
            let cb0: Vec<_> = planes.cb.iter().take(8).collect();
            let cr0: Vec<_> = planes.cr.iter().take(8).collect();
            eprintln!(
                "D3D11VA decode: {}x{} bd={} chroma_fmt={} \
                 spsW={} spsH={} crop_lrtb=({},{},{},{}) \
                 PicWHinMinCbs=({},{}) MinCbLog2={} \
                 amp={} sao={} strong_smooth={} \
                 pcm={} scaling={} \
                 nals={} first_nal_type={} input_bytes={} \
                 num_tile_cols={} num_tile_rows={} \
                 y0={:?} cb0={:?} cr0={:?}",
                config.width,
                config.height,
                bit_depth,
                config.chroma_format_idc,
                sps.pic_width_in_luma_samples,
                sps.pic_height_in_luma_samples,
                config.crop_left,
                config.crop_right,
                config.crop_top,
                config.crop_bottom,
                sps.pic_width_in_min_cbs_y(),
                sps.pic_height_in_min_cbs_y(),
                sps.min_cb_log2_size_y(),
                sps.amp_enabled_flag,
                sps.sample_adaptive_offset_enabled_flag,
                sps.strong_intra_smoothing_enabled_flag,
                sps.pcm_enabled_flag,
                sps.scaling_list_enabled_flag,
                nal_count,
                nal_type_first,
                image_data.len(),
                config
                    .pps
                    .map_or(0, |p| u32::from(p.num_tile_columns_minus1) + 1),
                config
                    .pps
                    .map_or(0, |p| u32::from(p.num_tile_rows_minus1) + 1),
                y0,
                cb0,
                cr0,
            );
            eprintln!(
                "  pps: wpp={} tiles_en={} tq_bypass={} sdh={} cabac_init={} \
                 cip={} ts={} lf_across_slices={} deblock_override={} \
                 init_qp_minus26={} cb_qp_offset={} cr_qp_offset={}",
                config
                    .pps
                    .is_some_and(|p| p.entropy_coding_sync_enabled_flag),
                config.pps.is_some_and(|p| p.tiles_enabled_flag),
                config.pps.is_some_and(|p| p.transquant_bypass_enabled_flag),
                config.pps.is_some_and(|p| p.sign_data_hiding_enabled_flag),
                config.pps.is_some_and(|p| p.cabac_init_present_flag),
                config.pps.is_some_and(|p| p.constrained_intra_pred_flag),
                config.pps.is_some_and(|p| p.transform_skip_enabled_flag),
                config
                    .pps
                    .is_some_and(|p| p.pps_loop_filter_across_slices_enabled_flag),
                config
                    .pps
                    .is_some_and(|p| p.deblocking_filter_override_enabled_flag),
                config.pps.map_or(0i32, |p| i32::from(p.init_qp_minus26)),
                config.pps.map_or(0i32, |p| i32::from(p.pps_cb_qp_offset)),
                config.pps.map_or(0i32, |p| i32::from(p.pps_cr_qp_offset)),
            );
        }

        Ok(DecodedFrame {
            width: config.width,
            height: config.height,
            y_plane: planes.y,
            cb_plane: planes.cb,
            cr_plane: planes.cr,
            bit_depth,
            chroma_format: config.chroma_format_idc,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            alpha_plane: None,
            full_range: config.full_range,
            matrix_coeffs: config.matrix_coeffs,
            color_primaries: config.color_primaries,
            transfer_characteristics: config.transfer_characteristics,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
        })
    }
}
