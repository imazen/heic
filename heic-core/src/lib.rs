//! Shared types and traits for the `heic` HEVC decoder backends.
//!
//! The parent `heic` crate parses the HEIF container, manages grid/alpha/gain-map
//! orchestration, and dispatches actual HEVC bitstream decoding to a pluggable
//! backend. Every backend — pure-Rust, Media Foundation, VideoToolbox,
//! MediaCodec, VA-API, D3D11VA — implements the [`HevcBackend`] trait declared
//! in this crate and produces a [`DecodedFrame`] in a uniform layout.
//!
//! This crate is `no_std + alloc`, `#![forbid(unsafe_code)]`, and has the
//! minimal possible dependency surface so it can be shared across the
//! native-FFI backend crates without forcing them to pull in everything the
//! parent depends on.
//!
//! ## Design contract
//!
//! Every backend:
//!
//! 1. Accepts an [`HvccParams`] (subset of the HEIF `hvcC` decoder
//!    configuration record) plus a length-prefixed slice-data buffer.
//! 2. Returns a [`DecodedFrame`] with planar YCbCr at the source bit depth.
//! 3. Surfaces unavailability via [`BackendError::Unavailable`] so the parent
//!    crate's allowlist dispatcher can fall through to the next backend.
//!
//! Backends do NOT handle HEIF container parsing, grid assembly, alpha
//! compositing, gain maps, EXIF/XMP/ICC extraction, transforms, or
//! YCbCr→RGB conversion. Those concerns live in the parent crate (and color
//! conversion lives in [`color_convert`]).

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::string::String;

// ── Public modules ────────────────────────────────────────────────────────

pub mod color_convert;
pub mod error;
pub mod frame;
pub mod nal;
pub mod sps;

#[cfg(target_arch = "aarch64")]
mod color_convert_neon;
#[cfg(target_arch = "wasm32")]
mod color_convert_wasm;

pub use frame::DecodedFrame;

// ── Internal allocation primitive (used by `try_vec!`) ────────────────────

/// Fallible vec allocation — called by [`try_vec!`] when `fallible-alloc` is
/// enabled.
#[cfg(feature = "fallible-alloc")]
#[doc(hidden)]
#[inline]
pub fn alloc_vec_fallible<T: Clone>(
    len: usize,
    val: T,
) -> Result<alloc::vec::Vec<T>, error::HevcError> {
    let mut v = alloc::vec::Vec::new();
    v.try_reserve(len)
        .map_err(|_| error::HevcError::AllocationFailed)?;
    v.resize(len, val);
    Ok(v)
}

/// Allocate a `Vec<T>` filled with `len` copies of `val`, returning
/// `Result<Vec<T>, error::HevcError>`.
///
/// With `fallible-alloc` feature: uses `try_reserve` + `resize` (never panics
/// on OOM). Without `fallible-alloc` (default): uses `vec![val; len]` (fast
/// memset path, panics on OOM, but wraps result in `Ok` so callers always
/// write `try_vec![...]?`).
#[doc(hidden)]
#[macro_export]
macro_rules! try_vec {
    ($val:expr; $len:expr) => {{
        #[cfg(feature = "fallible-alloc")]
        {
            $crate::alloc_vec_fallible($len, $val)
        }
        #[cfg(not(feature = "fallible-alloc"))]
        {
            Ok::<::alloc::vec::Vec<_>, $crate::error::HevcError>(::alloc::vec![$val; $len])
        }
    }};
}

// ── Backend contract ──────────────────────────────────────────────────────

/// Errors a backend can return from [`HevcBackend::decode_hevc`].
///
/// The parent crate's allowlist dispatcher treats [`BackendError::Unavailable`]
/// and [`BackendError::Decode`] as recoverable — it falls through to the next
/// backend in the allowlist. [`BackendError::LimitsExceeded`] and
/// [`BackendError::Cancelled`] are terminal and propagate to the caller.
#[derive(Debug)]
#[non_exhaustive]
pub enum BackendError {
    /// Backend is not available on this machine right now.
    ///
    /// Examples: Windows Media Foundation when the HEVC Video Extensions
    /// package isn't installed; VA-API when no driver supports
    /// `VAProfileHEVCMain`; AMF when the runtime DLL is missing.
    ///
    /// The dispatcher will try the next backend in the allowlist.
    Unavailable(&'static str),

    /// Backend was reached but decoding failed.
    ///
    /// The dispatcher MAY try the next backend in the allowlist as a recovery
    /// attempt — bitstreams that one backend rejects may be acceptable to
    /// another.
    Decode(String),

    /// A configured resource limit was exceeded (image dimensions, pixel
    /// count, memory). Terminal — do NOT fall through.
    LimitsExceeded(&'static str),

    /// Operation was cancelled via the [`Stop`](enough::Stop) token. Terminal.
    Cancelled,
}

impl core::fmt::Display for BackendError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable(m) => write!(f, "backend unavailable: {m}"),
            Self::Decode(m) => write!(f, "decode failed: {m}"),
            Self::LimitsExceeded(m) => write!(f, "limit exceeded: {m}"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for BackendError {}

/// Subset of the HEIF `hvcC` HEVC Decoder Configuration Record (plus the
/// `ispe`-derived frame dimensions) that backends need to set up decoding.
///
/// The parent `heic` crate owns the full
/// [`HevcDecoderConfig`](../heic/heif/struct.HevcDecoderConfig.html) parsed
/// from the container and constructs an `HvccParams` view of it before
/// dispatching to a backend. Width and height come from the HEIF `ispe`
/// (image spatial extent) box, which sits alongside `hvcC` in the item
/// properties; backends use them up-front (e.g. to size Media Foundation's
/// `MF_MT_FRAME_SIZE` or VideoToolbox's destination pixel buffer attributes)
/// without parsing the SPS themselves.
pub struct HvccParams<'a> {
    /// Visible width in pixels (from HEIF `ispe` — already cropped per
    /// the SPS conformance window).
    pub width: u32,

    /// Visible height in pixels (from HEIF `ispe`).
    pub height: u32,

    /// Bitstream-coded width before SPS conformance cropping
    /// (`pic_width_in_luma_samples`). Equal to `width` when the SPS
    /// doesn't specify a conformance window or the bitstream was coded
    /// at the visible size. Backends that decode at the coded size and
    /// then have to copy out the visible region use this together with
    /// the `crop_*` offsets.
    pub coded_width: u32,

    /// Bitstream-coded height before SPS conformance cropping
    /// (`pic_height_in_luma_samples`).
    pub coded_height: u32,

    /// SPS conformance window crop offsets (luma samples). Visible
    /// region is `[crop_left, coded_width - crop_right)` × `[crop_top,
    /// coded_height - crop_bottom)`. Backends that get raw decoder
    /// output at `coded_width × coded_height` should skip these rows /
    /// columns when populating the planes.
    pub crop_left: u32,
    /// See [`Self::crop_left`].
    pub crop_right: u32,
    /// See [`Self::crop_left`].
    pub crop_top: u32,
    /// See [`Self::crop_left`].
    pub crop_bottom: u32,

    /// VPS, SPS, PPS, and any prefix-SEI NAL payloads (RBSP, no start codes,
    /// no length prefix).
    pub nal_units: &'a [&'a [u8]],

    /// Length prefix size in bytes for the slice NAL units that follow in
    /// `image_data` — 1, 2, or 4. Standard hvcC uses 4.
    pub length_size: u8,

    /// Bit depth for the luma plane (8 or 10 for HEVC Main / Main10).
    pub bit_depth_luma: u8,

    /// Bit depth for the chroma plane.
    pub bit_depth_chroma: u8,

    /// Chroma format idc: 0=monochrome, 1=4:2:0, 2=4:2:2, 3=4:4:4.
    pub chroma_format_idc: u8,

    /// Color metadata sourced from the SPS VUI (or sensible defaults when
    /// `vui_parameters_present_flag` is 0). Backends populate the
    /// returned [`DecodedFrame`]'s color fields from this so the parent
    /// crate's YCbCr→RGB conversion applies the correct matrix /
    /// primaries / transfer / range without re-parsing the bitstream.
    ///
    /// Values follow ITU-T H.273 / CICP:
    /// - `full_range`: VUI `video_full_range_flag` (false = limited
    ///   [16,235], true = full [0,255]).
    /// - `matrix_coeffs`: 1=BT.709, 5/6=BT.601, 9=BT.2020, 2=unspecified.
    /// - `color_primaries`: 1=BT.709, 9=BT.2020, 12=Display P3, 2=unspecified.
    /// - `transfer_characteristics`: 1=BT.709, 13=sRGB, 16=PQ, 18=HLG.
    pub full_range: bool,
    /// Matrix coefficients (CICP). See [`HvccParams::full_range`] for
    /// the meaning of the field.
    pub matrix_coeffs: u8,
    /// Color primaries (CICP).
    pub color_primaries: u8,
    /// Transfer characteristics (CICP).
    pub transfer_characteristics: u8,
}

/// HEVC backend implementation.
///
/// Implementors decode one HEIF tile (one HEVC access unit) from an hvcC
/// config plus length-prefixed slice data into a planar YCbCr
/// [`DecodedFrame`]. The parent `heic` crate constructs backends, holds the
/// allowlist, and routes container-level concerns (grid assembly, alpha
/// plane, gain map) to individual backend calls.
pub trait HevcBackend: Send {
    /// Stable identifier for the backend, used in logs and error messages.
    /// Examples: `"rust"`, `"mediafoundation"`, `"videotoolbox"`,
    /// `"mediacodec"`, `"vaapi"`, `"d3d11va"`.
    fn name(&self) -> &'static str;

    /// Returns true if the backend can actually run on this machine right now
    /// — runtime DLL present, GPU driver loaded, OS supports the call, etc.
    ///
    /// Allowed to be a coarse heuristic; the dispatcher will catch false
    /// positives via the [`BackendError::Unavailable`] / fallthrough path.
    fn is_available(&self) -> bool;

    /// Decode one HEVC access unit.
    ///
    /// `image_data` is the length-prefixed slice-data payload for a single
    /// HEIF tile (as stored in the `idat` / `mdat`-referenced extent).
    ///
    /// The implementation may cache expensive state (decoder sessions, COM
    /// objects, GPU contexts) on `&mut self` across calls.
    fn decode_hevc(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError>;
}
