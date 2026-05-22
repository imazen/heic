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
        let coded_w = config.coded_width.max(config.width);
        let coded_h = config.coded_height.max(config.height);
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
        let mut bitstream: Vec<u8> = Vec::with_capacity(image_data.len() + 64);
        let ls = config.length_size as usize;
        let mut i = 0;
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
            bitstream.extend_from_slice(&[0, 0, 1]); // 3-byte start code per chromium
            bitstream.extend_from_slice(&image_data[i..i + nal_len]);
            i += nal_len;
        }

        // Submit + read back. The DecoderSession's per-frame flow
        // documented in `decoder::DecoderSession::submit_one_frame`
        // handles BeginFrame → buffer Get/Release/Submit → EndFrame.
        session.submit_one_frame(&pic_params, &bitstream)?;

        let planes = session.read_decoded_planes(
            config.width,
            config.height,
            config.crop_left,
            config.crop_top,
        )?;

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
