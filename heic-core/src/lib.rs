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
//! conversion lives in [`color`]).

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

pub mod color;
pub mod frame;
pub mod nal;

pub use frame::DecodedFrame;

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

/// Subset of the HEIF `hvcC` HEVC Decoder Configuration Record that backends
/// need to set up decoding.
///
/// The parent `heic` crate owns the full
/// [`HevcDecoderConfig`](../heic/heif/struct.HevcDecoderConfig.html) parsed
/// from the container and constructs an `HvccParams` view of it before
/// dispatching to a backend.
pub struct HvccParams<'a> {
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
}

/// HEVC backend implementation.
///
/// Implementors decode one HEIF tile (one HEVC access unit) from hvcC config
/// + length-prefixed slice data into a planar YCbCr [`DecodedFrame`]. The
/// parent `heic` crate constructs backends, holds the allowlist, and routes
/// container-level concerns (grid assembly, alpha plane, gain map) to
/// individual backend calls.
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

/// Helper: allocate a `Vec<T>` of `len` copies of `val`.
///
/// Mirrors the parent crate's `try_vec!` macro so backend crates that don't
/// pull in the parent can use the same allocation discipline. With the
/// `fallible-alloc` feature, uses `try_reserve` + `resize`; otherwise uses
/// `vec![val; len]` (fast memset, panics on OOM).
#[doc(hidden)]
#[inline]
pub fn alloc_vec<T: Clone>(len: usize, val: T) -> Result<Vec<T>, BackendError> {
    #[cfg(feature = "fallible-alloc")]
    {
        let mut v = Vec::new();
        v.try_reserve(len)
            .map_err(|_| BackendError::LimitsExceeded("allocation failed"))?;
        v.resize(len, val);
        Ok(v)
    }
    #[cfg(not(feature = "fallible-alloc"))]
    {
        Ok(alloc::vec![val; len])
    }
}
