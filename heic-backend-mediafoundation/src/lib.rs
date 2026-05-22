//! Windows Media Foundation HEVC decoder backend for `heic`.
//!
//! Wraps the Microsoft "H.265 / HEVC Video Decoder" Media Foundation
//! Transform (MFT) and exposes it through the
//! [`heic_core::HevcBackend`] trait, so the parent `heic` crate can route
//! HEIC tile decoding through Windows-native HEVC instead of (or as a
//! fallback to) the pure-Rust decoder.
//!
//! # Availability
//!
//! The HEVC MFT ships with Windows but the underlying decoder DLLs live in
//! the **HEVC Video Extensions** Microsoft Store package, which:
//!
//! * Is **not installed by default** on Windows 10/11 client editions; the
//!   "Device Manufacturer" variant (Store ID `9N4WGH0Z6VHQ`) is free,
//!   "Microsoft" variant (`9NMZLZ57R3T7`) is $0.99.
//! * Is **not supported at all** on Windows Server SKUs (Microsoft's own
//!   docs say "Minimum supported server: None supported"). On Server 2025
//!   `MFTEnumEx` returns zero HEVC decoders even after the AppX install
//!   succeeds.
//!
//! [`MediaFoundationBackend::is_available`] returns `false` (and decode
//! calls return [`heic_core::BackendError::Unavailable`]) when the MFT
//! isn't installed, so the parent crate's allowlist dispatcher cleanly
//! falls through to the next backend.
//!
//! # NAL format
//!
//! Microsoft's HEVC MFT expects **Annex B** input — start-code-prefixed
//! NAL units, both for the `MF_MT_MPEG_SEQUENCE_HEADER` blob (concatenated
//! VPS+SPS+PPS) and for the slice samples. The parent crate hands us
//! HEIF's `hvcC`-style length-prefixed slice payloads; we convert with the
//! helpers in [`heic_core::nal`].
//!
//! # Threading
//!
//! `IMFTransform` instances are **not** safe to call concurrently — even
//! though the runtime is MTA. Each [`MediaFoundationBackend`] caches one
//! transform on `&mut self`, so callers that need parallel tile decode
//! must construct one backend per thread.

#![cfg_attr(not(target_os = "windows"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Windows Media Foundation HEVC decoder backend.
///
/// Constructed via [`Self::new()`]. Holds the cached `IMFTransform` and
/// runtime-initialization state once decoding starts.
///
/// # Thread safety
///
/// `IMFTransform` instances hold raw COM `IUnknown` references which are
/// `!Send` by default in the `windows` crate. We assert `Send` manually
/// because each `MediaFoundationBackend` is used **serially** by one
/// thread at a time — moving the instance between threads is fine; calling
/// it concurrently from multiple threads is undefined behavior. Callers
/// that need parallel tile decode (e.g. rayon-based grid decode) must
/// construct one backend per worker thread.
#[derive(Default)]
pub struct MediaFoundationBackend {
    #[cfg(target_os = "windows")]
    inner: imp::Inner,
}

// SAFETY: see the type doc comment. The crate enforces single-threaded
// access at the call site; this is the same contract FFmpeg and other
// MFT-using libraries rely on.
#[cfg(target_os = "windows")]
unsafe impl Send for MediaFoundationBackend {}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    /// Smoke test: `MediaFoundationBackend::is_available()` should return
    /// `true` on a Windows host where the HEVC Video Extensions package is
    /// installed (true on every dev box for this crate; CI runners need
    /// `winget install --id 9N4WGH0Z6VHQ` first per spec.md).
    ///
    /// Skipped via env var on CI runners that don't have the extension
    /// installed — exits early with a printed message rather than failing,
    /// matching the existing "set the gate at the workflow YAML, not in
    /// the test body" pattern.
    #[test]
    fn mft_enumeration_finds_hevc_decoder_when_extension_installed() {
        if std::env::var_os("HEIC_SKIP_MF_HEVC").is_some() {
            eprintln!(
                "HEIC_SKIP_MF_HEVC set: skipping MF HEVC enumeration test \
                 (CI runner without the HEVC Video Extensions package)"
            );
            return;
        }
        let backend = MediaFoundationBackend::new();
        assert!(
            backend.is_available(),
            "MediaFoundationBackend::is_available() returned false — \
             the HEVC Video Extensions package is not installed (run \
             `winget install --id 9N4WGH0Z6VHQ`) or `MFTEnumEx` is \
             rejecting the synchronous-decoder filter for some other \
             reason. Set HEIC_SKIP_MF_HEVC=1 to bypass."
        );
    }

    #[test]
    fn name_is_mediafoundation() {
        let backend = MediaFoundationBackend::new();
        assert_eq!(backend.name(), "mediafoundation");
    }
}

impl MediaFoundationBackend {
    /// Create a new backend instance. Cheap on non-Windows targets (stub);
    /// on Windows, the actual COM/MFT setup is deferred to the first
    /// [`Self::decode_hevc`] call.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HevcBackend for MediaFoundationBackend {
    fn name(&self) -> &'static str {
        "mediafoundation"
    }

    fn is_available(&self) -> bool {
        #[cfg(target_os = "windows")]
        {
            imp::is_available()
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
                "heic-backend-mediafoundation: not compiled for this target",
            ))
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Real implementation lives in `imp` so the non-Windows build is a clean
// no-op without `#[cfg(target_os = "windows")]` cluttering every function.

#[cfg(target_os = "windows")]
mod imp;
