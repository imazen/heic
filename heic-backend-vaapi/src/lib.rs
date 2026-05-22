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
//! # Decode status
//!
//! [`Self::decode_hevc`] is a stub. The full HEVC decode path (SPS/PPS
//! → `VAPictureParameterBufferHEVC`, slice control buffer, IQ matrix,
//! `vaBeginPicture`/`vaRenderPicture`/`vaEndPicture`/`vaSyncSurface`,
//! `vaDeriveImage` → planar `u16`) follows the Chromium media/gpu
//! reference but is heavy enough (~1.5k LOC of bit-by-bit field
//! mapping) that it lives in a follow-up PR. The probe is the
//! first chunk that ships now so `recommended_backends()` can report
//! VA-API accurately to callers.

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
