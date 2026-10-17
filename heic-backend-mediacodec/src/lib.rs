//! Android MediaCodec HEVC decoder backend for `heic`.
//!
//! Wraps the NDK `AMediaCodec` C API and exposes it through the
//! [`heic_core::HevcBackend`] trait, so the parent `heic` crate can route
//! HEIC tile decoding through Android's built-in HEVC decoder. Every
//! shipping Android (API 21+) carries `c2.android.hevc.decoder` as a
//! software fallback at minimum; modern devices surface a hardware
//! decoder via the same API.
//!
//! # NAL format
//!
//! MediaCodec expects **Annex B** start-code-prefixed NAL units, both for
//! `csd-0` (VPS+SPS+PPS concatenated) and for each input access unit. The
//! parent crate's hvcC length-prefixed slices are converted on the fly
//! via [`heic_core::nal::hvcc_to_annexb`].
//!
//! # Threading
//!
//! `AMediaCodec` is documented thread-safe across instances but not
//! within one — single-threaded-per-instance, like the Windows MF
//! transform. Callers that decode tile-grids in parallel construct one
//! [`MediaCodecBackend`] per worker thread.

#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Android MediaCodec HEVC decoder backend.
///
/// Constructed via [`Self::new`]. Caches the `AMediaCodec` instance + the
/// configured `AMediaFormat` across decode calls and reuses them when
/// subsequent decodes have matching dimensions / bit-depth.
#[derive(Default)]
pub struct MediaCodecBackend {
    #[cfg(target_os = "android")]
    inner: imp::Inner,
}

// SAFETY: `AMediaCodec` is single-instance-single-thread per the NDK docs.
// The wrapper owns the codec exclusively, so it's safe to send the wrapper
// across threads as long as each thread serializes its own calls.
#[cfg(target_os = "android")]
unsafe impl Send for MediaCodecBackend {}

impl MediaCodecBackend {
    /// Create a new MediaCodec backend. The `AMediaCodec` is allocated
    /// lazily on the first decode.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl HevcBackend for MediaCodecBackend {
    fn name(&self) -> &'static str {
        "mediacodec"
    }

    fn is_available(&self) -> bool {
        #[cfg(target_os = "android")]
        {
            // Lazy probe: the actual `AMediaCodec_createDecoderByType` call
            // happens on decode. Returning true here is fine — the
            // dispatcher catches a real-world unavailable case via the
            // fallthrough path.
            true
        }
        #[cfg(not(target_os = "android"))]
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
        #[cfg(target_os = "android")]
        {
            self.inner.decode(config, image_data, stop)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = (config, image_data, stop);
            Err(BackendError::Unavailable(
                "heic-backend-mediacodec: not compiled for this target",
            ))
        }
    }
}

#[cfg(target_os = "android")]
mod imp;
