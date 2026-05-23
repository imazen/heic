//! Linux VA-API (libva) HEVC decoder backend for `heic`.
//!
//! Exposes the libva `VAEntrypointVLD` HEVC decoder via the
//! [`heic_core::HevcBackend`] trait. Runtime-loads `libva.so.2` +
//! `libva-drm.so.2` through `libloading`, so systems without
//! `libva-dev` build cleanly and the backend gracefully reports
//! [`heic_core::BackendError::Unavailable`].
//!
//! # Availability — what [`Self::is_available`] checks
//!
//! 1. `libva.so.2` and `libva-drm.so.2` are present on the dynamic
//!    linker's path.
//! 2. `/dev/dri/renderD128..D191` opens (the calling user is in the
//!    `render` group).
//! 3. `vaGetDisplayDRM` + `vaInitialize` succeed against that node.
//! 4. `vaQueryConfigProfiles` returns at least one of `VAProfileHEVCMain`
//!    or `VAProfileHEVCMain10`.
//!
//! If any step fails, the parent's allowlist dispatcher falls through
//! to the next backend.
//!
//! # WSL2 setup (no `/dev/dri`)
//!
//! WSL2 doesn't expose `/dev/dri` because the host GPU is reached
//! through Microsoft's `/dev/dxg` paravirtual device, not standard
//! DRM. The probe transparently falls back to an X11-backed
//! `VADisplay` via `XOpenDisplay($DISPLAY)` (works under WSLg).
//!
//! End-to-end setup on Ubuntu 22.04 + WSL2 with an NVIDIA host:
//!
//! ```bash
//! sudo apt-get install -y libdrm-dev libegl1-mesa-dev \
//!     libgstreamer-plugins-bad1.0-dev pkg-config vainfo
//! git clone https://github.com/FFmpeg/nv-codec-headers.git
//! sudo make -C nv-codec-headers install
//! git clone https://github.com/elFarto/nvidia-vaapi-driver.git
//! # apply the heic-crate ~30-LOC patch to src/export-buf.c so the
//! # findGPUIndexFromFd fallback picks the first EGL device when
//! # there's no DRM render-node file (WSL is the canonical case)
//! cd nvidia-vaapi-driver && meson setup build && ninja -C build
//! sudo cp build/nvidia_drv_video.so /usr/lib/x86_64-linux-gnu/dri/
//! export LIBVA_DRIVER_NAME=nvidia NVD_BACKEND=egl
//! ```
//!
//! After that, `cargo run -p heic-backend-vaapi --example
//! vaapi_probe` should print `is_available() = true` and exit 0.
//!
//! # Decode status
//!
//! [`Self::decode_hevc`] is a stub. The full HEVC decode path
//! (SPS/PPS → `VAPictureParameterBufferHEVC` (already in tree as
//! `va_hevc::from_sps_pps`), slice control buffer, IQ matrix,
//! `vaBeginPicture` / `vaRenderPicture` / `vaEndPicture` /
//! `vaSyncSurface`, `vaDeriveImage` → planar `u16`) follows the
//! Chromium `media/gpu/vaapi` reference. It needs a
//! `VASliceParameterBufferHEVC` populator (~30 fields beyond what
//! `va_hevc.rs` already has — `slice_data_byte_offset`, RefPicList
//! indices, num_ref_idx_l0_active, weighted-pred tables, etc.)
//! plus ~600 LOC of libloading-wrapped FFI for the actual
//! `vaCreateBuffer` / `vaRenderPicture` / `vaSyncSurface` chain.
//! That lives in a follow-up PR; the probe ships now so
//! `recommended_backends()` can route to VA-API on Linux without
//! waiting for the runtime.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Linux VA-API HEVC decoder backend.
#[derive(Default)]
pub struct VaApiBackend {
    #[cfg(target_os = "linux")]
    _placeholder: (),
}

// SAFETY: VADisplay handles are documented thread-safe under per-display
// serialization; the wrapper enforces single-instance ownership.
#[cfg(target_os = "linux")]
unsafe impl Send for VaApiBackend {}

impl VaApiBackend {
    /// Create a new VA-API backend. Probes and the eventual decoder
    /// session are constructed lazily.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HevcBackend for VaApiBackend {
    fn name(&self) -> &'static str {
        "vaapi"
    }

    fn is_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            probe::probe().unwrap_or(false)
        }
        #[cfg(not(target_os = "linux"))]
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
        let _ = (config, image_data, stop);
        Err(BackendError::Unavailable(
            "heic-backend-vaapi: HEVC decode FFI pending — probe succeeded \
             but the full SPS/PPS → VAPictureParameterBufferHEVC mapping \
             ships in a follow-up PR",
        ))
    }
}

#[cfg(target_os = "linux")]
mod probe;

pub mod va_hevc;
