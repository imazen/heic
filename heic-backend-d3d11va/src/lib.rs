//! Windows Direct3D 11 Video Acceleration (D3D11VA) HEVC decoder backend for `heic`.
//!
//! D3D11VA is the modern replacement for DXVA2 and gives a single decoder
//! API that works across **all** Windows GPU vendors (Intel iGPU, NVIDIA,
//! AMD) without requiring the Microsoft Store HEVC Video Extensions
//! package — D3D11 ships in every Windows install since Windows 8.1, and
//! the GPU driver (not the OS) provides the HEVC decode profile. This is
//! distinct from [`heic-backend-mediafoundation`], which goes through
//! Microsoft's Media Foundation Transform layer and requires the Store
//! extension package.
//!
//! # When to choose D3D11VA vs. Media Foundation
//!
//! * **Server SKUs (Windows Server 2025)**: D3D11VA still works if the
//!   server has a GPU; MF is structurally unavailable (Microsoft's docs
//!   say "Minimum supported server: None supported" for the HEVC MFT).
//! * **Headless / hyper-V VMs without a GPU**: neither works — D3D11VA
//!   needs hardware acceleration (the WARP software D3D11 device does
//!   **not** support video decode; `CreateVideoDecoder` returns
//!   `E_NOTIMPL` for HEVC).
//! * **Per-vendor control**: D3D11VA exposes the underlying GPU adapter
//!   directly, so callers who want to pin decode to a specific GPU on a
//!   multi-GPU laptop can do so via DXGI adapter enumeration.
//!
//! # Replacing AMF
//!
//! This backend replaces the originally-planned `heic-backend-amf`. AMF
//! requires the proprietary `amdgpu-pro` Linux drivers and has
//! known-broken Main10 decode on NAVI1x/NAVI2x/VCN2.x (AMF issue #348).
//! D3D11VA covers AMD on Windows alongside Intel and NVIDIA, with a
//! cleaner availability story and no vendor SDK dependency. AMD-specific
//! AMF support could land as a separate `heic-backend-amf` crate in the
//! future for users with specialized needs (encoder reuse, hardware tone
//! mapping); it is not on the critical path.
//!
//! # Status — skeleton
//!
//! This commit lands the crate structure and the `HevcBackend` trait
//! implementation as a stub that returns
//! [`BackendError::Unavailable`](heic_core::BackendError::Unavailable).
//! The real FFI (`D3D11CreateDevice`, `ID3D11VideoDevice::CreateVideoDecoder`
//! with `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN`, video decoder input/output
//! views, NV12/P010 staging texture readback → planar u16) lands in a
//! follow-up PR with Windows + GPU CI hardware (compile-only CI in this
//! commit).

#![cfg_attr(not(target_os = "windows"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Windows D3D11VA HEVC decoder backend.
#[derive(Default)]
pub struct D3d11VaBackend {
    #[cfg(target_os = "windows")]
    _placeholder: (),
}

// SAFETY: D3D11 device and video decoder objects are documented thread-safe
// when D3D11_CREATE_DEVICE_SINGLETHREADED is *not* set, but call sites
// still need to serialize submissions. Skeleton wrapper is trivially Send.
#[cfg(target_os = "windows")]
unsafe impl Send for D3d11VaBackend {}

impl D3d11VaBackend {
    /// Create a new D3D11VA backend instance. The device + video decoder
    /// are created lazily on the first decode call.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HevcBackend for D3d11VaBackend {
    fn name(&self) -> &'static str {
        "d3d11va"
    }

    fn is_available(&self) -> bool {
        // Real implementation: try `D3D11CreateDevice` with a hardware
        // driver type, then `ID3D11VideoDevice::CheckVideoDecoderFormat`
        // for `DXGI_FORMAT_NV12` against
        // `D3D11_DECODER_PROFILE_HEVC_VLD_MAIN`. Skeleton: false.
        false
    }

    fn decode_hevc(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        let _ = (config, image_data, stop);
        Err(BackendError::Unavailable(
            "heic-backend-d3d11va: FFI implementation pending (skeleton crate)",
        ))
    }
}
