//! Linux VA-API (libva) HEVC decoder backend for `heic`.
//!
//! Wraps libva's `VAEntrypointVLD` HEVC decode path through Chromium-OS's
//! safe [`cros-libva`] bindings, and exposes it via the
//! [`heic_core::HevcBackend`] trait.
//!
//! # Availability
//!
//! VA-API HEVC decode is available on any Linux system where:
//!
//! * `libva.so.2` is installed (Ubuntu `libva2`, Fedora `libva`, etc.).
//! * A driver registers `VAProfileHEVCMain` (8-bit) and/or
//!   `VAProfileHEVCMain10` for `VAEntrypointVLD`. Common drivers:
//!   - `iHD` (Intel) — Tiger Lake+ for Main10, Skylake+ for Main.
//!   - `radeonsi` (AMD via Mesa) — VCN-class GPUs.
//!   - `nvidia-vaapi-driver` — wraps NVDEC; Main and Main10, no SCC.
//! * The user is in the `render` group so `/dev/dri/renderD128` is
//!   accessible without root.
//!
//! [`Self::is_available`] queries the driver at startup and returns false
//! if no HEVC profile is registered, so the parent's allowlist falls
//! through cleanly.
//!
//! # SPS/PPS → VAPictureParameterBufferHEVC
//!
//! VA-API doesn't decode the bitstream's parameter sets itself — callers
//! must pre-parse the SPS/PPS and fill the ~150 fields of
//! `VAPictureParameterBufferHEVC` (and `VAIQMatrixBufferHEVC` for scaling
//! lists). The reference Rust implementation is
//! `cros-codecs-0.0.6/src/decoder/stateless/h265/vaapi.rs` (1.4k LOC).
//!
//! # Status — skeleton
//!
//! This commit lands the crate structure and the `HevcBackend` trait
//! implementation as a stub that returns
//! [`BackendError::Unavailable`](heic_core::BackendError::Unavailable).
//! Functional CI requires a self-hosted Linux runner with a libva-capable
//! GPU (per the platform-validation findings the cheapest path is a $200
//! Intel N100 mini-PC with `intel-media-va-driver-non-free`); meanwhile
//! we'll ship compile-only CI on `ubuntu-latest` with `libva-dev`
//! installed.

#![cfg_attr(not(target_os = "linux"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Linux VA-API HEVC decoder backend.
#[derive(Default)]
pub struct VaApiBackend {
    #[cfg(target_os = "linux")]
    _placeholder: (),
}

// SAFETY: cros-libva's Display/Context wrappers internally Rc some state;
// individual VaApiBackend instances are intended to be used by one thread.
// For the skeleton, trivially Send.
unsafe impl Send for VaApiBackend {}

impl VaApiBackend {
    /// Create a new VA-API backend instance. Display open + driver probe
    /// happen lazily on the first decode call.
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
        // Real implementation: `Display::open()` walks
        // `/dev/dri/renderD128..D191`, then `query_config_profiles` filters
        // for `VAProfileHEVCMain` or `VAProfileHEVCMain10`. Skeleton: false.
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
            "heic-backend-vaapi: FFI implementation pending (skeleton crate)",
        ))
    }
}
