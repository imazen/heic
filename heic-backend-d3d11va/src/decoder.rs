//! D3D11 HEVC `ID3D11VideoDecoder` session — creates and caches the
//! `ID3D11VideoDecoder` + `ID3D11VideoContext` + output texture + view
//! for one stream's dimensions and bit-depth.
//!
//! This module is the *plumbing* between the probe (which proves the
//! GPU supports HEVC) and the actual per-frame decode (which lands in
//! a follow-up commit). It owns the lifetime of the GPU-side decoder
//! resources so subsequent decodes of identical-dimension tiles can
//! reuse them — building a new decoder per tile is expensive on
//! every driver tested.
//!
//! # Pipeline outline (per
//! `media/gpu/windows/d3d11_h265_accelerator.cc::CreateAcceleratedVideoDecoder`)
//!
//! 1. Reuse the `ID3D11Device` from the probe (or create fresh).
//! 2. `ID3D11VideoDevice::CheckVideoDecoderFormat(profile, NV12)`.
//! 3. Build `D3D11_VIDEO_DECODER_DESC` { SampleWidth, SampleHeight,
//!    OutputFormat=NV12 or P010, Guid=HEVC_VLD_MAIN/Main10 }.
//! 4. Walk `GetVideoDecoderConfigCount` / `GetVideoDecoderConfig` to
//!    pick a config with `ConfigBitstreamRaw == 1` (short-format
//!    slice control — what we populate via `DxvaPicParamsHevc`).
//! 5. `CreateVideoDecoder(desc, config)`.
//! 6. `ID3D11Texture2D` with `Format=NV12`, `BindFlags=DECODER`,
//!    `ArraySize=1` (HEIC tiles are single-picture).
//! 7. `CreateVideoDecoderOutputView(texture, view_desc)`.
//! 8. Cast the `ID3D11DeviceContext` to `ID3D11VideoContext`.
//!
//! With those handles cached, per-tile decode is:
//!
//!   DecoderBeginFrame(decoder, view)
//!   GetDecoderBuffer(PICTURE_PARAMETERS) → memcpy DxvaPicParamsHevc → ReleaseDecoderBuffer
//!   GetDecoderBuffer(BITSTREAM) → memcpy Annex-B slice → ReleaseDecoderBuffer
//!   GetDecoderBuffer(SLICE_CONTROL) → memcpy DxvaSliceHevcShort → ReleaseDecoderBuffer
//!   SubmitDecoderBuffers([3 desc structs])
//!   DecoderEndFrame(decoder)
//!   // staging texture readback in a follow-up commit
//!
//! All `unsafe` blocks carry `SAFETY:` comments.

#![cfg(target_os = "windows")]
#![allow(missing_docs)] // documented inline + via the module header

extern crate alloc;

use heic_core::BackendError;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_CPU_ACCESS_FLAG, D3D11_CREATE_DEVICE_FLAG, D3D11_RESOURCE_MISC_FLAG,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VDOV_DIMENSION_TEXTURE2D,
    D3D11_VIDEO_DECODER_BUFFER_BITSTREAM, D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
    D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS, D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
    D3D11_VIDEO_DECODER_BUFFER_TYPE, D3D11_VIDEO_DECODER_CONFIG, D3D11_VIDEO_DECODER_DESC,
    D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC, D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0,
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, ID3D11VideoContext,
    ID3D11VideoDecoder, ID3D11VideoDecoderOutputView, ID3D11VideoDevice,
};
use windows::Win32::Graphics::Dxgi::Common::{
    DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC,
};
use windows::core::{GUID, Interface};

use crate::probe::{HEVC_VLD_MAIN, HEVC_VLD_MAIN10};

// Buffer-type constants come from the windows crate
// (windows::Win32::Graphics::Direct3D11). DXVA layout:
// 0=PICTURE_PARAMETERS, 4=INVERSE_QUANTIZATION_MATRIX,
// 5=SLICE_CONTROL, 6=BITSTREAM. My initial hand-coded constants had
// SLICE_CONTROL and BITSTREAM swapped + numbered wrong; the windows
// crate is authoritative.

/// Cached per-stream GPU decoder resources.
///
/// One `DecoderSession` covers a stream of constant (width, height,
/// bit_depth). For HEIC grid decoding where every tile shares those,
/// the session is built once and reused; for varying-dimension input
/// the session is rebuilt.
pub struct DecoderSession {
    /// The hardware D3D11 device — owns the lifetime of everything
    /// downstream.
    pub device: ID3D11Device,
    /// Immediate context cast to `ID3D11VideoContext` — drives
    /// per-frame buffer submission.
    pub video_context: ID3D11VideoContext,
    /// HEVC video decoder for this stream's dimensions.
    pub decoder: ID3D11VideoDecoder,
    /// NV12 / P010 output texture (`ArraySize=1` because HEIC tiles
    /// don't share reference frames).
    pub output_texture: ID3D11Texture2D,
    /// View on `output_texture` that the decoder writes through.
    pub output_view: ID3D11VideoDecoderOutputView,
    /// Picked HEVC profile GUID (Main vs Main10).
    pub profile_guid: GUID,
    /// Output surface format (NV12 for 8-bit, P010 for 10-bit).
    pub output_format: DXGI_FORMAT,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
}

impl DecoderSession {
    /// Build a fresh session for the given stream dimensions + bit
    /// depth. Creates a new D3D11 device on every call — callers that
    /// want device reuse across sessions should hold the device
    /// externally and pass it in (future extension).
    pub fn new(width: u32, height: u32, bit_depth: u8) -> Result<Self, BackendError> {
        let (profile_guid, output_format) = if bit_depth >= 10 {
            (HEVC_VLD_MAIN10, DXGI_FORMAT_P010)
        } else {
            (HEVC_VLD_MAIN, DXGI_FORMAT_NV12)
        };

        // 1. D3D11 hardware device.
        let device = create_d3d11_device()?;
        // 2. Get the video device + check format support.
        // SAFETY: device is alive; cast queries IUnknown::QueryInterface.
        let video_device: ID3D11VideoDevice = device
            .cast()
            .map_err(|e| BackendError::Decode(format!("ID3D11Device cast to VideoDevice: {e}")))?;
        // SAFETY: standard format-support check; both pointers are valid.
        let supports =
            unsafe { video_device.CheckVideoDecoderFormat(&profile_guid, output_format) }
                .map_err(|e| BackendError::Decode(format!("CheckVideoDecoderFormat: {e}")))?;
        if !supports.as_bool() {
            return Err(BackendError::Unavailable(
                "HEVC profile/format combination not supported by this GPU driver",
            ));
        }

        // 3. Decoder descriptor — bound to the stream's coded
        // dimensions (callers should pass coded_w/coded_h, not visible).
        let decoder_desc = D3D11_VIDEO_DECODER_DESC {
            Guid: profile_guid,
            SampleWidth: width,
            SampleHeight: height,
            OutputFormat: output_format,
        };

        // 4. Walk configurations and pick one with `ConfigBitstreamRaw
        // == 1`. Per Microsoft's DXVA HEVC spec ("shall be 1") and
        // chromium media/gpu/windows/d3d11_video_decoder_wrapper.cc:358,
        // HEVC, VP9, and AV1 all require raw-Annex-B bitstream input.
        // Picking index 0 blind sometimes gave us a different config
        // and the driver silently produced zero pixels.
        // SAFETY: video_device is alive, desc valid for the call.
        let cfg_count = unsafe { video_device.GetVideoDecoderConfigCount(&decoder_desc) }
            .map_err(|e| BackendError::Decode(format!("GetVideoDecoderConfigCount: {e}")))?;
        if cfg_count == 0 {
            return Err(BackendError::Unavailable(
                "No D3D11 video decoder config exposed for HEVC profile",
            ));
        }
        let mut config: D3D11_VIDEO_DECODER_CONFIG =
            // SAFETY: zero-initialized POD; filled by the API in the
            // loop below.
            unsafe { core::mem::zeroed() };
        let mut picked = false;
        for i in 0..cfg_count {
            let mut candidate: D3D11_VIDEO_DECODER_CONFIG =
                // SAFETY: zero-init POD; filled by GetVideoDecoderConfig.
                unsafe { core::mem::zeroed() };
            // SAFETY: i < cfg_count per the loop bound; out pointer is valid.
            unsafe { video_device.GetVideoDecoderConfig(&decoder_desc, i, &mut candidate) }
                .map_err(|e| BackendError::Decode(format!("GetVideoDecoderConfig[{i}]: {e}")))?;
            if candidate.ConfigBitstreamRaw == 1 {
                config = candidate;
                picked = true;
                break;
            }
        }
        if !picked {
            return Err(BackendError::Unavailable(
                "No D3D11 HEVC decoder config with ConfigBitstreamRaw=1 \
                 (driver doesn't expose the spec-required short-format)",
            ));
        }

        // 5. Create the decoder itself.
        // SAFETY: desc + config are valid; both pointers live through the call.
        let decoder = unsafe { video_device.CreateVideoDecoder(&decoder_desc, &config) }
            .map_err(|e| BackendError::Decode(format!("CreateVideoDecoder: {e}")))?;

        // 6. Output texture (NV12 / P010, decoder-bind, ArraySize=1).
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: output_format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_DECODER.0 as u32,
            CPUAccessFlags: D3D11_CPU_ACCESS_FLAG(0).0 as u32,
            MiscFlags: D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
        };
        let mut output_texture: Option<ID3D11Texture2D> = None;
        // SAFETY: tex_desc lives through the call; out pointer is non-null.
        unsafe { device.CreateTexture2D(&tex_desc, None, Some(&mut output_texture)) }
            .map_err(|e| BackendError::Decode(format!("CreateTexture2D(output): {e}")))?;
        let output_texture =
            output_texture.ok_or(BackendError::Decode("CreateTexture2D returned None".into()))?;

        // 7. Decoder output view on slice 0 of the texture.
        let view_desc = D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC {
            DecodeProfile: profile_guid,
            ViewDimension: D3D11_VDOV_DIMENSION_TEXTURE2D,
            Anonymous: D3D11_VIDEO_DECODER_OUTPUT_VIEW_DESC_0 {
                Texture2D: windows::Win32::Graphics::Direct3D11::D3D11_TEX2D_VDOV { ArraySlice: 0 },
            },
        };
        let mut output_view: Option<ID3D11VideoDecoderOutputView> = None;
        // SAFETY: texture is alive; view_desc lives through the call.
        unsafe {
            video_device.CreateVideoDecoderOutputView(
                &output_texture,
                &view_desc,
                Some(&mut output_view),
            )
        }
        .map_err(|e| BackendError::Decode(format!("CreateVideoDecoderOutputView: {e}")))?;
        let output_view = output_view.ok_or(BackendError::Decode(
            "CreateVideoDecoderOutputView returned None".into(),
        ))?;

        // 8. Immediate device context cast to ID3D11VideoContext.
        // SAFETY: GetImmediateContext is a free getter on a live device.
        let ctx: ID3D11DeviceContext = unsafe { device.GetImmediateContext() }
            .map_err(|e| BackendError::Decode(format!("GetImmediateContext: {e}")))?;
        let video_context: ID3D11VideoContext = ctx.cast().map_err(|e| {
            BackendError::Decode(format!("ID3D11DeviceContext cast to VideoContext: {e}"))
        })?;

        Ok(Self {
            device,
            video_context,
            decoder,
            output_texture,
            output_view,
            profile_guid,
            output_format,
            width,
            height,
            bit_depth,
        })
    }

    /// Returns true if the cached session matches the caller's stream
    /// dimensions + bit depth. Used by `Inner::decode` to decide
    /// whether to rebuild.
    #[must_use]
    pub fn matches(&self, width: u32, height: u32, bit_depth: u8) -> bool {
        self.width == width && self.height == height && self.bit_depth == bit_depth
    }

    /// Read back the decoded NV12 / P010 frame as planar `u16` Y + Cb + Cr.
    ///
    /// Allocates a fresh staging texture, copies the GPU output into
    /// it, maps it for CPU read, and unpacks NV12 / P010 → three
    /// `Vec<u16>` planes with the crop offsets applied. This mirrors
    /// the pattern in
    /// `heic-backend-mediafoundation/src/pixels.rs::unpack_nv12_or_p010`
    /// — both backends end up writing into the same DecodedFrame
    /// layout the parent crate expects.
    ///
    /// `visible_w` × `visible_h` are the ispe-visible dimensions;
    /// `crop_x` / `crop_y` are the SPS conformance-window offsets in
    /// luma samples (chroma offsets are derived as crop / 2 for
    /// 4:2:0). This matches `HvccParams.{width,height,crop_left,crop_top}`.
    pub fn read_decoded_planes(
        &self,
        visible_w: u32,
        visible_h: u32,
        crop_x: u32,
        crop_y: u32,
    ) -> Result<crate::dxva_read::OutputPlanes, BackendError> {
        crate::dxva_read::read_decoded_planes(
            &self.device,
            // Reuse the immediate context cast back to ID3D11DeviceContext —
            // ID3D11VideoContext shares the same vtable upcast.
            &self.video_context,
            &self.output_texture,
            self.output_format,
            self.width,
            self.height,
            visible_w,
            visible_h,
            crop_x,
            crop_y,
            self.bit_depth,
        )
    }

    /// Submit one HEVC access unit to the driver.
    ///
    /// `pic_params` is the SPS + PPS-populated picture-parameter
    /// buffer. `slice_data_annexb` is the slice NAL bytes in Annex-B
    /// form (start-code-prefixed; we use Annex-B because that matches
    /// what `DxvaSliceHevcShort` references via `SliceBytesIndex = 0`).
    ///
    /// On success the decoded frame lands in [`Self::output_texture`];
    /// callers can `CopySubresourceRegion` it into a CPU-readable
    /// staging texture and `Map` to read NV12 / P010 bytes.
    ///
    /// This is the "first attempt" decode entry point — it submits one
    /// slice in DXVA short format and calls `DecoderEndFrame` to flush.
    /// Multi-slice / multi-tile pictures need richer slice control
    /// buffer construction (one `DxvaSliceHevcShort` per slice + the
    /// concatenated bitstream); that lands in a follow-up.
    pub fn submit_one_frame(
        &self,
        pic_params: &crate::dxva::DxvaPicParamsHevc,
        slice_data_annexb: &[u8],
        iq_matrix: Option<&crate::dxva::DxvaQmatrixHevc>,
    ) -> Result<(), BackendError> {
        // 1. DecoderBeginFrame — locks the output view for write.
        // SAFETY: decoder + output_view are live for the session.
        unsafe {
            self.video_context
                .DecoderBeginFrame(&self.decoder, &self.output_view, 0, None)
        }
        .map_err(|e| BackendError::Decode(format!("DecoderBeginFrame: {e}")))?;

        // 2. PICTURE_PARAMETERS buffer.
        copy_into_buffer(
            &self.video_context,
            &self.decoder,
            D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
            // SAFETY: DxvaPicParamsHevc is #[repr(C)] + Copy; safe to
            // view its bytes for a memcpy into the GPU-side buffer.
            unsafe {
                core::slice::from_raw_parts(
                    (pic_params as *const _) as *const u8,
                    core::mem::size_of::<crate::dxva::DxvaPicParamsHevc>(),
                )
            },
        )?;

        // 2b. INVERSE_QUANTIZATION_MATRIX buffer (only when SPS enables
        // scaling lists, per DXVA HEVC spec section 4.2 / chromium
        // d3d11_h265_accelerator.cc::SubmitSlice). For HEIC fixtures we
        // pass the HEVC default lists since the bitstream parser doesn't
        // yet propagate custom scaling-list data.
        if let Some(q) = iq_matrix {
            copy_into_buffer(
                &self.video_context,
                &self.decoder,
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                // SAFETY: DxvaQmatrixHevc is #[repr(C)] + Copy.
                unsafe {
                    core::slice::from_raw_parts(
                        (q as *const _) as *const u8,
                        core::mem::size_of::<crate::dxva::DxvaQmatrixHevc>(),
                    )
                },
            )?;
        }

        // 3. BITSTREAM buffer — raw Annex-B bytes.
        copy_into_buffer(
            &self.video_context,
            &self.decoder,
            D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
            slice_data_annexb,
        )?;

        // 4. SLICE_CONTROL buffer — one DXVA_Slice_HEVC_Short
        // pointing at the slice NAL inside the bitstream buffer.
        // struct layout from dxva.h:
        //   BSNALunitDataLocation (UINT)  — byte offset of the first
        //                                   NAL data byte AFTER the
        //                                   start code in the
        //                                   BITSTREAM buffer.
        //   SliceBytesInBuffer    (UINT)  — total slice bytes from
        //                                   the location above.
        //   wBadSliceChopping     (USHORT) — 0 = full slice, no chopping.
        //
        // For HEIC single-slice IDR tiles the slice_annexb buffer is
        // [0x00 0x00 0x00 0x01][NAL header + RBSP]. Point at offset 0
        // because the driver expects to see the start code itself
        // (NB: chromium uses 0 for the full Annex-B blob).
        #[repr(C)]
        struct DxvaSliceHevcShort {
            bs_nalu_data_location: u32,
            slice_bytes_in_buffer: u32,
            w_bad_slice_chopping: u16,
        }
        let slice = DxvaSliceHevcShort {
            bs_nalu_data_location: 0,
            slice_bytes_in_buffer: u32::try_from(slice_data_annexb.len()).unwrap_or(u32::MAX),
            w_bad_slice_chopping: 0,
        };
        copy_into_buffer(
            &self.video_context,
            &self.decoder,
            D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
            // SAFETY: DxvaSliceHevcShort is #[repr(C)] + lives until
            // ReleaseDecoderBuffer copies its bytes.
            unsafe {
                core::slice::from_raw_parts(
                    (&slice as *const _) as *const u8,
                    core::mem::size_of::<DxvaSliceHevcShort>(),
                )
            },
        )?;

        // 5. SubmitDecoderBuffers — include the iq_matrix descriptor
        // when present, matching chromium's pattern of "send all
        // committed buffers" (the order doesn't strictly matter, but
        // PICTURE_PARAMETERS first is the spec recommendation).
        let mut buffer_descs: alloc::vec::Vec<_> = alloc::vec::Vec::with_capacity(4);
        buffer_descs.push(buffer_desc(
            D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS,
            core::mem::size_of::<crate::dxva::DxvaPicParamsHevc>() as u32,
        ));
        if iq_matrix.is_some() {
            buffer_descs.push(buffer_desc(
                D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX,
                core::mem::size_of::<crate::dxva::DxvaQmatrixHevc>() as u32,
            ));
        }
        buffer_descs.push(buffer_desc(
            D3D11_VIDEO_DECODER_BUFFER_BITSTREAM,
            u32::try_from(slice_data_annexb.len()).unwrap_or(u32::MAX),
        ));
        buffer_descs.push(buffer_desc(
            D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL,
            core::mem::size_of::<DxvaSliceHevcShort>() as u32,
        ));
        // SAFETY: buffer_descs is alive through the call.
        unsafe {
            self.video_context
                .SubmitDecoderBuffers(&self.decoder, &buffer_descs)
        }
        .map_err(|e| BackendError::Decode(format!("SubmitDecoderBuffers: {e}")))?;

        // 6. DecoderEndFrame flushes the decoder for this AU.
        // SAFETY: decoder + context still alive.
        unsafe { self.video_context.DecoderEndFrame(&self.decoder) }
            .map_err(|e| BackendError::Decode(format!("DecoderEndFrame: {e}")))?;
        Ok(())
    }
}

/// Helper: GetDecoderBuffer → memcpy `data` → ReleaseDecoderBuffer.
fn copy_into_buffer(
    ctx: &ID3D11VideoContext,
    decoder: &ID3D11VideoDecoder,
    buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE,
    data: &[u8],
) -> Result<(), BackendError> {
    let mut buf_size: u32 = 0;
    let mut buf_ptr: *mut core::ffi::c_void = core::ptr::null_mut();
    // SAFETY: GetDecoderBuffer returns a writable pointer + size for
    // the requested buffer slot.
    unsafe { ctx.GetDecoderBuffer(decoder, buffer_type, &mut buf_size, &mut buf_ptr) }
        .map_err(|e| BackendError::Decode(format!("GetDecoderBuffer({buffer_type:?}): {e}")))?;
    if buf_ptr.is_null() || (data.len() as u32) > buf_size {
        // SAFETY: pairs with GetDecoderBuffer; harmless on null.
        unsafe { ctx.ReleaseDecoderBuffer(decoder, buffer_type) }
            .map_err(|e| BackendError::Decode(format!("ReleaseDecoderBuffer: {e}")))?;
        return Err(BackendError::Decode(format!(
            "decoder buffer too small for {buffer_type:?}: have {buf_size}, need {}",
            data.len()
        )));
    }
    // SAFETY: buf_ptr valid for buf_size bytes; we write data.len() ≤ buf_size.
    unsafe {
        core::ptr::copy_nonoverlapping(data.as_ptr(), buf_ptr as *mut u8, data.len());
    }
    // SAFETY: pairs with the Get above.
    unsafe { ctx.ReleaseDecoderBuffer(decoder, buffer_type) }
        .map_err(|e| BackendError::Decode(format!("ReleaseDecoderBuffer: {e}")))
}

fn buffer_desc(
    buffer_type: D3D11_VIDEO_DECODER_BUFFER_TYPE,
    data_size: u32,
) -> windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_DECODER_BUFFER_DESC {
    use windows::Win32::Graphics::Direct3D11::D3D11_VIDEO_DECODER_BUFFER_DESC;
    // SAFETY: D3D11_VIDEO_DECODER_BUFFER_DESC is plain POD; zeroed is
    // a valid initial state (all reserved fields must be 0).
    let mut desc: D3D11_VIDEO_DECODER_BUFFER_DESC = unsafe { core::mem::zeroed() };
    desc.BufferType = buffer_type;
    desc.DataSize = data_size;
    desc
}

/// Create a hardware D3D11 device (no software fallback — HEVC decode
/// requires GPU acceleration).
fn create_d3d11_device() -> Result<ID3D11Device, BackendError> {
    let feature_levels = [D3D_FEATURE_LEVEL_11_0];
    let mut device: Option<ID3D11Device> = None;
    // SAFETY: standard D3D11 device creation against the default
    // hardware adapter. None adapter, None software-rasterizer, no
    // out-context (we use GetImmediateContext later).
    let hr = unsafe {
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_FLAG(0),
            Some(&feature_levels),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )
    };
    if hr.is_err() {
        return Err(BackendError::Unavailable(
            "D3D11CreateDevice(HARDWARE) failed — no GPU adapter or HEVC-unsupported driver",
        ));
    }
    device.ok_or(BackendError::Unavailable(
        "D3D11CreateDevice returned no device",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a host with an HEVC-decode-capable GPU (RTX 30+, Intel iGPU
    /// from Skylake+, AMD VCN), `DecoderSession::new(1280, 854, 8)`
    /// should succeed and report Main profile + NV12 output.
    ///
    /// Gated on `HEIC_D3D11VA_HW=1` so headless CI runners don't
    /// fail. Set the env var on hosts with a real GPU.
    #[test]
    fn decoder_session_new_succeeds_on_hardware() {
        if std::env::var_os("HEIC_D3D11VA_HW").is_none() {
            eprintln!("HEIC_D3D11VA_HW not set; skipping (requires GPU)");
            return;
        }
        let session = DecoderSession::new(1280, 854, 8).expect("Main session should construct");
        assert_eq!(session.profile_guid, HEVC_VLD_MAIN);
        assert_eq!(session.output_format, DXGI_FORMAT_NV12);
        assert!(session.matches(1280, 854, 8));
        assert!(!session.matches(1280, 854, 10));
    }

    /// Smoke test the readback path against an uninitialized output
    /// texture — proves `CreateTexture2D(STAGING)` + `CopyResource` +
    ///   `Map` + `Unmap` all succeed on real hardware. The unpacked
    ///   values are undefined (garbage from the uninitialized GPU
    ///   texture) but the plane lengths must match the visible region.
    #[test]
    fn read_decoded_planes_smoke_test_on_hardware() {
        if std::env::var_os("HEIC_D3D11VA_HW").is_none() {
            eprintln!("HEIC_D3D11VA_HW not set; skipping (requires GPU)");
            return;
        }
        let session = DecoderSession::new(1280, 858, 8).expect("Main session");
        // Visible 1280x854 = ispe; coded 1280x858 = sps height; crop top 4.
        let planes = session
            .read_decoded_planes(1280, 854, 0, 4)
            .expect("staging texture readback should succeed even for an uninit texture");
        assert_eq!(planes.y.len(), 1280 * 854);
        assert_eq!(planes.cb.len(), 640 * 427);
        assert_eq!(planes.cr.len(), 640 * 427);
    }
}
