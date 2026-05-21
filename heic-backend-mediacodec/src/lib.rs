//! Android MediaCodec HEVC decoder backend for `heic`.
//!
//! Wraps the NDK `AMediaCodec` C API (`createDecoderByType("video/hevc")`,
//! `configure` with null surface, byte-buffer or `AImage` output) and
//! exposes it through the [`heic_core::HevcBackend`] trait, so the parent
//! `heic` crate can route HEIC tile decoding through Android's built-in
//! HEVC decoder.
//!
//! # Availability
//!
//! Every Android device since API 21 (Lollipop, 2014) ships
//! `c2.android.hevc.decoder` as a software HEVC fallback at minimum;
//! modern devices have hardware decoders surfaced via the same API.
//! 10-bit Main10 / `COLOR_FormatYUVP010` is reliable from API 33 onward.
//!
//! # NAL format
//!
//! MediaCodec expects **Annex B** start-code-prefixed NAL units, both for
//! `csd-0` (VPS+SPS+PPS concatenated) and for each input access unit. The
//! parent crate's hvcC length-prefixed slices are converted at the call
//! site via [`heic_core::nal::hvcc_to_annexb`].
//!
//! # Status — skeleton
//!
//! This commit lands the crate structure and the `HevcBackend` trait
//! implementation as a stub that returns
//! [`BackendError::Unavailable`](heic_core::BackendError::Unavailable).
//! The real FFI (`AMediaCodec_createDecoderByType`, `AMediaFormat`
//! configuration, the queue/dequeue loop, `AMediaCodec_getOutputImage` for
//! API 33+ with byte-buffer fallback for older API levels, NV12/NV21/I420/
//! YV12 / P010 → planar u16 unpack) lands in a follow-up PR with Android
//! emulator CI verification (per the spec's `reactivecircus/android-emulator-runner@v2`
//! setup).

#![cfg_attr(not(target_os = "android"), allow(dead_code, unused_imports))]

use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};

/// Android MediaCodec HEVC decoder backend.
#[derive(Default)]
pub struct MediaCodecBackend {
    #[cfg(target_os = "android")]
    _placeholder: (),
}

// SAFETY: `AMediaCodec` instances are documented to be safe to use from any
// thread as long as a single instance is not called concurrently from
// multiple threads — same single-threaded-per-instance contract as the
// Windows MF backend. For the skeleton, the wrapper is trivially Send.
unsafe impl Send for MediaCodecBackend {}

impl MediaCodecBackend {
    /// Create a new MediaCodec backend instance. The underlying `AMediaCodec`
    /// is created lazily on the first [`HevcBackend::decode_hevc`] call.
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
        // Real implementation will use `AMediaCodecList` (API 28+) to look
        // up `video/hevc` decoder capabilities; falling back to the
        // "construct + start, then teardown" probe on older API levels.
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
            "heic-backend-mediacodec: FFI implementation pending (skeleton crate)",
        ))
    }
}
