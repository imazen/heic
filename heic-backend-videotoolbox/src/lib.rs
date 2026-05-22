//! Apple VideoToolbox HEVC decoder backend for `heic`.
//!
//! Wraps `VTDecompressionSession` on macOS, iOS (device + simulator), tvOS,
//! and visionOS, exposing it through the [`heic_core::HevcBackend`] trait
//! so the parent `heic` crate can route HEIC tile decoding through Apple's
//! built-in HEVC decoder.
//!
//! # Availability
//!
//! VideoToolbox HEVC decode is shipped on every macOS 10.13+, iOS 11+,
//! tvOS 11+, and visionOS 1+ release; no extra install or entitlement is
//! needed. Hardware-accelerated paths require an HEVC-capable GPU
//! (Apple Silicon, every Intel Mac with Kaby Lake or newer iGPU). On
//! older Intel Macs lacking HW support, VT silently falls back to
//! software decode — [`Self::is_available`] still returns true and decode
//! succeeds.
//!
//! # NAL format
//!
//! VideoToolbox accepts hvcC-style **length-prefixed** slice NAL units
//! directly via `CMBlockBuffer`, with the parameter sets pre-registered
//! via `CMVideoFormatDescriptionCreateFromHEVCParameterSets`. No Annex-B
//! conversion needed (unlike the Windows MF backend).
//!
//! # Threading
//!
//! `VTDecompressionSession` is documented thread-safe — concurrent
//! decodes from multiple threads on one session are allowed. Each
//! [`VideoToolboxBackend`] caches one session on `&mut self`; callers
//! doing parallel grid decode can construct one backend per worker or
//! share a single backend across worker threads.

#![cfg_attr(
    not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    )),
    allow(dead_code, unused_imports)
)]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Apple VideoToolbox HEVC decoder backend.
///
/// Constructed via [`Self::new`]. Holds the cached `VTDecompressionSession`
/// and the parameter-set `CMVideoFormatDescription` across decodes — both
/// are reused when subsequent decodes have matching SPS/PPS, rebuilt
/// otherwise.
#[derive(Default)]
pub struct VideoToolboxBackend {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    ))]
    inner: imp::Inner,
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos"
))]
// SAFETY: VTDecompressionSession is documented thread-safe in Apple's docs.
// Our wrapper holds CFRetained CoreMedia/VideoToolbox handles which are
// reference-counted and safe to send across threads. Concurrent calls from
// multiple threads are explicitly supported by the underlying API.
unsafe impl Send for VideoToolboxBackend {}

impl VideoToolboxBackend {
    /// Create a new VideoToolbox backend instance. The underlying session
    /// is created lazily on the first [`HevcBackend::decode_hevc`] call.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HevcBackend for VideoToolboxBackend {
    fn name(&self) -> &'static str {
        "videotoolbox"
    }

    fn is_available(&self) -> bool {
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        ))]
        {
            imp::is_available()
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )))]
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
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        ))]
        {
            self.inner.decode(config, image_data, stop)
        }
        #[cfg(not(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )))]
        {
            let _ = (config, image_data, stop);
            Err(BackendError::Unavailable(
                "heic-backend-videotoolbox: not compiled for this target",
            ))
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos"
))]
mod imp;
