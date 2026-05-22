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

use heic_core::{BackendError, DecodedFrame, HvccParams, nal};

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
        let pic_params = crate::dxva::from_sps_pps(sps, config.pps);

        // Annex-B: VPS+SPS+PPS prefix followed by the slice payload
        // converted from hvcC length-prefixed.
        let mut annexb = nal::annexb_parameter_sets(config.nal_units);
        let slice_annexb = nal::hvcc_to_annexb(image_data, config.length_size).ok_or(
            BackendError::Decode("malformed hvcC length-prefixed slice data".into()),
        )?;
        annexb.extend_from_slice(&slice_annexb);

        // Submit + read back. The DecoderSession's per-frame flow
        // documented in `decoder::DecoderSession::submit_one_frame`
        // handles BeginFrame → buffer Get/Release/Submit → EndFrame.
        session.submit_one_frame(&pic_params, &annexb)?;

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
