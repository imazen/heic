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

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use enough::Stop;
use heic_core::{BackendError, DecodedFrame, HevcBackend, HvccParams};
use whereat::at;

use crate::Result;
use crate::error::HeicError;
use crate::heif::HevcDecoderConfig;

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

impl Backend {
    /// Construct a boxed [`HevcBackend`] trait object for this variant.
    ///
    /// Each call allocates a fresh backend instance — backends with
    /// expensive per-instance state (cached MFTransform / VTSession /
    /// AMediaCodec) trade off setup cost across decode invocations on
    /// the same `&mut self`, so callers that decode many tiles should
    /// cache the result rather than re-instantiating per tile.
    pub(crate) fn instance(self) -> Box<dyn HevcBackend> {
        match self {
            #[cfg(feature = "backend-rust")]
            Self::Rust => Box::new(RustBackend),
            #[cfg(all(feature = "backend-mediafoundation", target_os = "windows"))]
            Self::MediaFoundation => {
                Box::new(heic_backend_mediafoundation::MediaFoundationBackend::new())
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
            Self::VideoToolbox => Box::new(heic_backend_videotoolbox::VideoToolboxBackend::new()),
            #[cfg(all(feature = "backend-mediacodec", target_os = "android"))]
            Self::MediaCodec => Box::new(heic_backend_mediacodec::MediaCodecBackend::new()),
            #[cfg(all(feature = "backend-vaapi", target_os = "linux"))]
            Self::Vaapi => Box::new(heic_backend_vaapi::VaApiBackend::new()),
            #[cfg(all(feature = "backend-d3d11va", target_os = "windows"))]
            Self::D3d11va => Box::new(heic_backend_d3d11va::D3d11VaBackend::new()),
        }
    }
}

/// Pure-Rust `HevcBackend` wrapper around the in-crate `crate::hevc::decode_with_config`.
#[cfg(feature = "backend-rust")]
struct RustBackend;

#[cfg(feature = "backend-rust")]
impl HevcBackend for RustBackend {
    fn name(&self) -> &'static str {
        "rust"
    }

    fn is_available(&self) -> bool {
        true
    }

    fn decode_hevc(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        _stop: &dyn Stop,
    ) -> core::result::Result<DecodedFrame, BackendError> {
        // Reconstruct the parent's HevcDecoderConfig view from HvccParams.
        // For PR2 we have only one consumer of decode_one_tile (the rust
        // path), so we can call directly without going through the
        // HvccParams round-trip.
        let _ = (config, image_data);
        unreachable!(
            "RustBackend::decode_hevc is only reachable when decode_one_tile \
             is called through the trait — the parent crate currently calls \
             crate::hevc::decode_with_config directly via the fast-path \
             shortcut in `decode_one_tile`."
        )
    }
}

/// Dispatch a single HEVC tile decode through the user-provided allowlist.
///
/// Iterates `backends` in order; for each entry, constructs the backend,
/// probes [`HevcBackend::is_available`], and on success calls
/// [`HevcBackend::decode_hevc`]. Recoverable errors
/// ([`BackendError::Unavailable`] / [`BackendError::Decode`]) fall through
/// to the next backend; terminal errors
/// ([`BackendError::LimitsExceeded`] / [`BackendError::Cancelled`])
/// short-circuit.
///
/// Fast path: when `backends == [Backend::Rust]` and `backend-rust` is
/// compiled in, calls `crate::hevc::decode_with_config` directly without
/// constructing a trait object — avoids the Box allocation on the hot
/// per-tile path in the common case.
pub(crate) fn decode_one_tile(
    backends: &[Backend],
    config: &HevcDecoderConfig,
    image_data: &[u8],
    width: u32,
    height: u32,
    stop: &dyn Stop,
) -> Result<DecodedFrame> {
    if backends.is_empty() {
        return Err(at!(HeicError::NoBackendSelected));
    }

    // Fast path: single-Rust-backend selection skips the trait dispatch +
    // HvccParams construction entirely.
    #[cfg(feature = "backend-rust")]
    if backends.len() == 1 && backends[0] == Backend::Rust {
        // `?` triggers the existing From<HevcError> for At<HeicError> in
        // error.rs — no further conversion needed.
        let _ = (stop, width, height);
        return Ok(crate::hevc::decode_with_config(config, image_data)?);
    }

    // Slow path: build the HvccParams view once and walk the allowlist.
    // Parse the first SPS NAL we find to extract VUI color metadata +
    // bitstream-coded dimensions + conformance-window crop offsets.
    // Without coded dims / crop, native backends produce visibly
    // wrong output for files whose SPS encodes a larger frame than the
    // HEIF `ispe` visible region (the `example.heic` case: ispe
    // 1280×854, SPS pic_height 858 with crop_top=4).
    let nal_refs: Vec<&[u8]> = config.nal_units.iter().map(|n| n.as_slice()).collect();
    let sps_meta = extract_sps_metadata(config);
    // Default `coded_*` to the visible size when there's no SPS info —
    // backends that don't know any better will produce output at the
    // visible size (which is correct for already-cropped bitstreams).
    let coded_width = sps_meta.coded_width.max(width);
    let coded_height = sps_meta.coded_height.max(height);
    let params = HvccParams {
        width,
        height,
        coded_width,
        coded_height,
        crop_left: sps_meta.crop_left,
        crop_right: sps_meta.crop_right,
        crop_top: sps_meta.crop_top,
        crop_bottom: sps_meta.crop_bottom,
        nal_units: &nal_refs,
        length_size: config.length_size_minus_one + 1,
        bit_depth_luma: config.bit_depth_luma_minus8 + 8,
        bit_depth_chroma: config.bit_depth_chroma_minus8 + 8,
        chroma_format_idc: config.chroma_format,
        full_range: sps_meta.full_range,
        matrix_coeffs: sps_meta.matrix_coeffs,
        color_primaries: sps_meta.color_primaries,
        transfer_characteristics: sps_meta.transfer_characteristics,
    };

    let mut last_err: Option<String> = None;
    for &b in backends {
        #[cfg(feature = "backend-rust")]
        if b == Backend::Rust {
            // Fast path even when Rust is mid-allowlist — bypass trait.
            let _ = stop;
            return Ok(crate::hevc::decode_with_config(config, image_data)?);
        }
        let mut inst = b.instance();
        if !inst.is_available() {
            last_err = Some(format!("{}: backend reported unavailable", b.name()));
            continue;
        }
        match inst.decode_hevc(&params, image_data, stop) {
            Ok(frame) => return Ok(frame),
            Err(BackendError::LimitsExceeded(m)) => {
                return Err(at!(HeicError::LimitExceeded(m)));
            }
            Err(BackendError::Cancelled) => {
                return Err(at!(HeicError::Cancelled(enough::StopReason::Cancelled)));
            }
            Err(BackendError::Unavailable(m)) => {
                last_err = Some(format!("{}: {m}", b.name()));
            }
            Err(BackendError::Decode(m)) => {
                last_err = Some(format!("{}: {m}", b.name()));
            }
            // BackendError is #[non_exhaustive]; new variants without a
            // specific mapping fall through to the "try next backend" path.
            Err(other) => {
                last_err = Some(format!("{}: {other}", b.name()));
            }
        }
    }
    Err(at!(HeicError::AllBackendsFailed(
        last_err.unwrap_or_else(|| "no backends were available".into())
    )))
}

/// SPS metadata extracted by [`extract_sps_metadata`].
///
/// All fields default to "unspecified / no crop" when no SPS is found
/// or it fails to parse; callers replace `coded_*` with the visible
/// dimensions when zero.
#[derive(Default)]
struct SpsMetadata {
    coded_width: u32,
    coded_height: u32,
    crop_left: u32,
    crop_right: u32,
    crop_top: u32,
    crop_bottom: u32,
    full_range: bool,
    matrix_coeffs: u8,
    color_primaries: u8,
    transfer_characteristics: u8,
}

/// Parse the first SPS NAL we find in `config.nal_units` and return its
/// coded dimensions, conformance-window crop offsets, and VUI color
/// metadata.
///
/// Falls back to `Default::default()` ("unspecified, no crop") when no
/// SPS is present or the SPS parser rejects the payload.
fn extract_sps_metadata(config: &HevcDecoderConfig) -> SpsMetadata {
    let mut out = SpsMetadata {
        matrix_coeffs: 2,
        color_primaries: 2,
        transfer_characteristics: 2,
        ..Default::default()
    };
    for nal_blob in &config.nal_units {
        if nal_blob.len() < 3 {
            continue;
        }
        let nal_type = (nal_blob[0] >> 1) & 0x3F;
        if nal_type != 33 {
            // not SPS_NUT
            continue;
        }
        // Parse through the NAL helper so emulation prevention bytes
        // (`00 00 03` → `00 00`) are stripped before the bitstream
        // reader runs. Calling parse_sps directly on `&nal[2..]` works
        // for some files but breaks on any payload that hits a
        // 0x000003 sequence — the example.heic SPS does.
        let Ok(nal) = crate::hevc::bitstream::parse_single_nal(nal_blob) else {
            continue;
        };
        if let Ok(sps) = crate::hevc::params::parse_sps(&nal.payload) {
            out.coded_width = sps.pic_width_in_luma_samples;
            out.coded_height = sps.pic_height_in_luma_samples;
            if sps.conformance_window_flag {
                // SPS conf_win_offset is in chroma-subsampling units
                // (SubWidthC / SubHeightC). Convert to luma samples
                // matching the rust decoder's set_crop semantics.
                let (sub_w, sub_h) = match sps.chroma_format_idc {
                    1 => (2u32, 2u32), // 4:2:0
                    2 => (2, 1),       // 4:2:2
                    3 => (1, 1),       // 4:4:4
                    _ => (2, 2),
                };
                out.crop_left = sps.conf_win_offset.0.saturating_mul(sub_w);
                out.crop_right = sps.conf_win_offset.1.saturating_mul(sub_w);
                out.crop_top = sps.conf_win_offset.2.saturating_mul(sub_h);
                out.crop_bottom = sps.conf_win_offset.3.saturating_mul(sub_h);
            }
            out.full_range = sps.video_full_range_flag;
            out.matrix_coeffs = sps.matrix_coeffs;
            out.color_primaries = sps.color_primaries;
            out.transfer_characteristics = sps.transfer_characteristics;
            return out;
        }
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
