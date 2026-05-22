//! Windows Direct3D 11 Video Acceleration (D3D11VA) HEVC decoder backend.
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
//! # Availability — what [`Self::is_available`] checks
//!
//! 1. `D3D11CreateDevice` succeeds against the default hardware adapter.
//! 2. The resulting `ID3D11Device` can be queried for `ID3D11VideoDevice`.
//! 3. At least one decoder profile in `GetVideoDecoderProfile` matches
//!    the HEVC Main / Main10 GUIDs.
//! 4. `CheckVideoDecoderFormat` reports support for the selected
//!    profile + `DXGI_FORMAT_NV12` (8-bit) or `DXGI_FORMAT_P010` (10-bit).
//!
//! If any step fails, the parent's allowlist dispatcher falls through
//! to the next backend.
//!
//! # When to choose D3D11VA vs. Media Foundation
//!
//! * **Server SKUs (Windows Server 2025)**: D3D11VA still works if the
//!   server has a GPU; MF is structurally unavailable (Microsoft's docs
//!   say "Minimum supported server: None supported" for the HEVC MFT).
//! * **Headless / Hyper-V VMs without a GPU**: neither works — D3D11VA
//!   needs hardware acceleration (the WARP software D3D11 device does
//!   *not* support video decode; `CreateVideoDecoder` returns
//!   `E_NOTIMPL` for HEVC).
//! * **Per-vendor control**: D3D11VA exposes the underlying GPU adapter
//!   directly, so callers can pin decode to a specific GPU on a
//!   multi-GPU laptop via DXGI adapter enumeration.
//!
//! # Decode pipeline
//!
//! [`Self::decode_hevc`] runs the full DXVA HEVC decode path:
//! `CreateVideoDecoder` → `DecoderBeginFrame` →
//! `GetDecoderBuffer` / `ReleaseDecoderBuffer` /
//! `SubmitDecoderBuffers` for the picture-parameter / bitstream /
//! slice-control buffers → `DecoderEndFrame` → staging texture
//! `CopyResource` + `Map` for readback. The `DxvaPicParamsHevc`
//! populator follows Chromium's `PicParamsFromSPS` and
//! `PicParamsFromPPS` in
//! `media/gpu/windows/d3d11_h265_accelerator.cc`. See the `imp`
//! module for the per-frame flow.

#![cfg_attr(not(target_os = "windows"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Windows D3D11VA HEVC decoder backend.
#[derive(Default)]
pub struct D3d11VaBackend {
    #[cfg(target_os = "windows")]
    inner: imp::Inner,
}

// SAFETY: D3D11 device and video decoder objects are documented thread-safe
// when D3D11_CREATE_DEVICE_SINGLETHREADED is NOT set; call sites still need
// to serialize submissions. The wrapper itself is trivially Send.
#[cfg(target_os = "windows")]
unsafe impl Send for D3d11VaBackend {}

impl D3d11VaBackend {
    /// Create a new D3D11VA backend. The device + video decoder are
    /// constructed lazily.
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
        #[cfg(target_os = "windows")]
        {
            probe::probe()
        }
        #[cfg(not(target_os = "windows"))]
        {
            false
        }
    }

    fn decode_hevc(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        #[cfg(target_os = "windows")]
        {
            self.inner.decode(config, image_data, stop)
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = (config, image_data, stop);
            Err(BackendError::Unavailable(
                "heic-backend-d3d11va: not compiled for this target",
            ))
        }
    }
}

#[cfg(target_os = "windows")]
pub mod decoder;
#[cfg(target_os = "windows")]
pub mod dxva;
#[cfg(target_os = "windows")]
pub mod dxva_read;
#[cfg(target_os = "windows")]
mod imp;
#[cfg(target_os = "windows")]
mod probe;
