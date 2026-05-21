//! HEVC backend selection.
//!
//! The parent `heic` crate parses the HEIF container and dispatches actual
//! HEVC bitstream decoding to a pluggable backend. Users choose backends with
//! an **ordered allowlist** via
//! [`DecoderConfig::with_backends`](crate::DecoderConfig::with_backends);
//! decoding falls through to the next entry in the list when a backend reports
//! unavailable or fails on the bitstream.
//!
//! ## Current status
//!
//! The `backend-rust` pure-Rust decoder is the only backend that ships today;
//! native backends (Media Foundation on Windows, VideoToolbox on Apple,
//! MediaCodec on Android, VA-API on Linux, D3D11VA on Windows) land in
//! subsequent PRs. The [`Backend`] enum, the allowlist API, and the
//! [`recommended_backends`] helper exist now so that downstream code can be
//! written against the final shape; the dispatcher's per-tile fallthrough
//! loop will start being honored as soon as a second backend variant lands.
//!
//! ## Allowlist semantics
//!
//! ```ignore
//! use heic::{Backend, DecoderConfig};
//!
//! // Try VideoToolbox first; fall through to the pure-Rust decoder if
//! // the platform decoder reports unavailable or rejects the bitstream.
//! let config = DecoderConfig::new()
//!     .with_backends(&[Backend::VideoToolbox, Backend::Rust]);
//! ```
//!
//! - Empty allowlist → decode returns
//!   [`HeicError::NoBackendSelected`](crate::HeicError::NoBackendSelected).
//! - A backend variant that isn't compiled in (its feature is off or the
//!   target_os doesn't match) is silently skipped.
//! - Recoverable errors (`Unavailable`, `Decode`) fall through to the next
//!   entry; terminal errors (`LimitsExceeded`, `Cancelled`) propagate.

use alloc::vec::Vec;

/// A HEVC backend the parent `heic` crate can dispatch decode requests to.
///
/// The set of variants is conditioned on Cargo features — variants whose
/// feature isn't enabled (or whose `target_os` doesn't match) are not
/// constructible. Today only [`Backend::Rust`] exists; native variants
/// (`MediaFoundation`, `VideoToolbox`, `MediaCodec`, `Vaapi`, `D3d11va`) land
/// in subsequent PRs and will appear here, each gated on its feature +
/// target_os.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Backend {
    /// The pure-Rust HEVC decoder bundled with this crate.
    #[cfg(feature = "backend-rust")]
    Rust,
    /// Windows Media Foundation HEVC decoder MFT.
    ///
    /// Requires the Microsoft "HEVC Video Extensions" Store package on
    /// the host (free "Device Manufacturer" variant 9N4WGH0Z6VHQ). Not
    /// available on Windows Server SKUs.
    #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
    MediaFoundation,
    /// Apple VideoToolbox HEVC decoder.
    ///
    /// Built into every shipping macOS 10.13+, iOS 11+, tvOS 11+, and
    /// visionOS 1+ release; no extra install needed.
    #[cfg(all(
        feature = "backend-videotoolbox",
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )
    ))]
    VideoToolbox,
    /// Android MediaCodec HEVC decoder (NDK C API).
    ///
    /// Available since API 21; software fallback (`c2.android.hevc.decoder`)
    /// ships on every modern device.
    #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
    MediaCodec,
    /// Linux VA-API HEVC decoder (`libva`).
    ///
    /// Requires a libva-capable GPU driver (iHD / radeonsi /
    /// nvidia-vaapi-driver).
    #[cfg(all(feature = "backend-vaapi", target_os = "linux"))]
    Vaapi,
    /// Windows Direct3D 11 Video Acceleration HEVC decoder.
    ///
    /// Covers Intel + NVIDIA + AMD on Windows via a single API; ships in
    /// every Windows install since 8.1, no Store extension required.
    /// Requires hardware GPU (the WARP software D3D11 device does not
    /// support video decode).
    #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
    D3d11va,
}

impl Backend {
    /// Stable identifier for the backend, used in logs and error messages.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            #[cfg(feature = "backend-rust")]
            Self::Rust => "rust",
            #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
            Self::MediaFoundation => "mediafoundation",
            #[cfg(all(
                feature = "backend-videotoolbox",
                any(
                    target_os = "macos",
                    target_os = "ios",
                    target_os = "tvos",
                    target_os = "visionos"
                )
            ))]
            Self::VideoToolbox => "videotoolbox",
            #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
            Self::MediaCodec => "mediacodec",
            #[cfg(all(feature = "backend-vaapi", target_os = "linux"))]
            Self::Vaapi => "vaapi",
            #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
            Self::D3d11va => "d3d11va",
        }
    }
}

/// Build a sensible default allowlist for the current build & target.
///
/// Order: native backends matching the host `target_os` first (when their
/// feature is enabled), then [`Backend::Rust`] as a last-resort fallback.
/// Currently only `Backend::Rust` is included because no native backends
/// have been wired in yet.
///
/// Use this if you don't want to enumerate backends explicitly:
///
/// ```ignore
/// let config = DecoderConfig::new()
///     .with_backends(&heic::recommended_backends());
/// ```
#[must_use]
pub fn recommended_backends() -> Vec<Backend> {
    let mut out: Vec<Backend> = Vec::new();
    // Native backends first (when feature + target_os both match), then
    // backend-rust as a last-resort fallback.
    #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
    {
        out.push(Backend::MediaFoundation);
    }
    #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
    {
        out.push(Backend::D3d11va);
    }
    #[cfg(all(
        feature = "backend-videotoolbox",
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )
    ))]
    {
        out.push(Backend::VideoToolbox);
    }
    #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
    {
        out.push(Backend::MediaCodec);
    }
    #[cfg(all(feature = "backend-vaapi", target_os = "linux"))]
    {
        out.push(Backend::Vaapi);
    }
    #[cfg(feature = "backend-rust")]
    {
        out.push(Backend::Rust);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "backend-rust")]
    #[test]
    fn rust_backend_name() {
        assert_eq!(Backend::Rust.name(), "rust");
    }

    #[test]
    fn recommended_includes_rust_when_compiled() {
        let order = recommended_backends();
        #[cfg(feature = "backend-rust")]
        assert!(order.contains(&Backend::Rust));
        #[cfg(not(feature = "backend-rust"))]
        assert!(order.is_empty());
    }
}
