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
//! (Apple Silicon, every Intel Mac with Kaby Lake or newer iGPU). On older
//! Intel Macs lacking HW support, VT silently falls back to software
//! decode — `is_available()` still returns true and decode succeeds.
//!
//! # NAL format
//!
//! VideoToolbox accepts hvcC-style **length-prefixed** slice NAL units
//! directly via `CMBlockBuffer`, with the parameter sets pre-registered
//! via `CMVideoFormatDescriptionCreateFromHEVCParameterSets`. No Annex-B
//! conversion needed (unlike the Windows MF backend).
//!
//! # Status — skeleton
//!
//! This commit lands the crate structure and the `HevcBackend` trait
//! implementation as a stub that returns
//! [`BackendError::Unavailable`](heic_core::BackendError::Unavailable).
//! The real FFI (`CMVideoFormatDescriptionCreateFromHEVCParameterSets`,
//! `VTDecompressionSessionCreate`, `VTDecompressionSessionDecodeFrame`,
//! `CVPixelBufferLockBaseAddress`, NV12/P010 → planar u16 unpack) lands
//! in a follow-up PR with Apple-runner CI verification.

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
/// and the parameter-set `CMVideoFormatDescription` across decodes.
#[derive(Default)]
pub struct VideoToolboxBackend {
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "tvos",
        target_os = "visionos"
    ))]
    _placeholder: (),
}

// SAFETY: VTDecompressionSession is documented thread-safe in Apple's docs;
// instances can be used concurrently from multiple threads. For the skeleton
// state the wrapper is trivially Send (empty struct).
unsafe impl Send for VideoToolboxBackend {}

impl VideoToolboxBackend {
    /// Create a new VideoToolbox backend instance. Cheap; the actual session
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
        // Real implementation will probe via
        // `VTIsHardwareDecodeSupported(kCMVideoCodecType_HEVC)` or attempt a
        // throwaway `VTDecompressionSessionCreate` with a minimal format
        // description. For the skeleton, advertise unavailable so the
        // parent's allowlist falls through to backend-rust.
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
            "heic-backend-videotoolbox: FFI implementation pending (skeleton crate)",
        ))
    }
}
