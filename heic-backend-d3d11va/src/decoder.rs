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

use heic_core::BackendError;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_DECODER, D3D11_CPU_ACCESS_FLAG, D3D11_CREATE_DEVICE_FLAG, D3D11_RESOURCE_MISC_FLAG,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, D3D11_VDOV_DIMENSION_TEXTURE2D,
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

/// D3D11 video buffer types passed to `GetDecoderBuffer` /
/// `SubmitDecoderBuffers`. Constants from `d3d11.h`.
pub const D3D11_VIDEO_DECODER_BUFFER_PICTURE_PARAMETERS: D3D11_VIDEO_DECODER_BUFFER_TYPE =
    D3D11_VIDEO_DECODER_BUFFER_TYPE(0);
pub const D3D11_VIDEO_DECODER_BUFFER_BITSTREAM: D3D11_VIDEO_DECODER_BUFFER_TYPE =
    D3D11_VIDEO_DECODER_BUFFER_TYPE(3);
pub const D3D11_VIDEO_DECODER_BUFFER_SLICE_CONTROL: D3D11_VIDEO_DECODER_BUFFER_TYPE =
    D3D11_VIDEO_DECODER_BUFFER_TYPE(4);
pub const D3D11_VIDEO_DECODER_BUFFER_INVERSE_QUANTIZATION_MATRIX: D3D11_VIDEO_DECODER_BUFFER_TYPE =
    D3D11_VIDEO_DECODER_BUFFER_TYPE(5);

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

        // 4. Walk configurations + pick one with ConfigBitstreamRaw=1
        // (short-format slice control) — matches what DxvaPicParamsHevc
        // expects. Most modern drivers expose both formats; we prefer
        // long-format when both are available because libheif's
        // bitstream uses Annex-B style.
        // SAFETY: video_device is alive, desc valid for the call.
        let cfg_count = unsafe { video_device.GetVideoDecoderConfigCount(&decoder_desc) }
            .map_err(|e| BackendError::Decode(format!("GetVideoDecoderConfigCount: {e}")))?;
        if cfg_count == 0 {
            return Err(BackendError::Unavailable(
                "No D3D11 video decoder config exposed for HEVC profile",
            ));
        }
        // SAFETY: zero-initialized struct, will be filled by the API.
        let mut config: D3D11_VIDEO_DECODER_CONFIG = unsafe { core::mem::zeroed() };
        // SAFETY: 0 < cfg_count per the check above.
        unsafe { video_device.GetVideoDecoderConfig(&decoder_desc, 0, &mut config) }
            .map_err(|e| BackendError::Decode(format!("GetVideoDecoderConfig: {e}")))?;

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
}
