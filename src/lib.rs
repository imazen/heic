//! Pure Rust HEIC/HEIF image decoder
//!
//! This crate decodes HEIC/HEIF images (as used by iPhones and modern cameras)
//! without any C/C++ dependencies. `#![forbid(unsafe_code)]`, `no_std + alloc`
//! compatible, and SIMD-accelerated on x86-64 (AVX2).
//!
//! # Quick Start
//!
//! ```no_run
//! use heic::{DecoderConfig, PixelLayout};
//!
//! let data = std::fs::read("image.heic").unwrap();
//! let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8).unwrap();
//! println!("Decoded {}x{} image", output.width, output.height);
//! ```
//!
//! # Decode with Limits and Cancellation
//!
//! ```no_run
//! use heic::{DecoderConfig, PixelLayout, Limits};
//!
//! let data = std::fs::read("image.heic").unwrap();
//! let mut limits = Limits::default();
//! limits.max_width = Some(8192);
//! limits.max_height = Some(8192);
//! limits.max_pixels = Some(64_000_000);
//!
//! let output = DecoderConfig::new()
//!     .decode_request(&data)
//!     .with_output_layout(PixelLayout::Rgba8)
//!     .with_limits(&limits)
//!     .decode()
//!     .unwrap();
//! ```
//!
//! # Zero-Copy into Pre-Allocated Buffer
//!
//! ```no_run
//! use heic::{DecoderConfig, ImageInfo, PixelLayout};
//!
//! let data = std::fs::read("image.heic").unwrap();
//! let info = ImageInfo::from_bytes(&data).unwrap();
//! let mut buf = vec![0u8; info.output_buffer_size(PixelLayout::Rgb8).unwrap()];
//! let (w, h) = DecoderConfig::new()
//!     .decode_request(&data)
//!     .with_output_layout(PixelLayout::Rgb8)
//!     .decode_into(&mut buf)
//!     .unwrap();
//! ```
//!
//! For grid-based images (most iPhone photos), [`DecodeRequest::decode_into`]
//! uses a streaming path that color-converts tiles directly into the output
//! buffer, avoiding the intermediate full-frame YCbCr allocation.
//!
//! # Features
//!
//! | Feature | Default | Description |
//! |---------|---------|-------------|
//! | `std` | yes | Standard library support. Disable for `no_std + alloc`. |
//! | `parallel` | no | Parallel tile decoding via rayon. Implies `std`. |
//!
//! # Error Handling
//!
//! Decode methods return [`Result<T>`], which is `core::result::Result<T, At<HeicError>>`.
//! The [`At`] wrapper from the `whereat` crate attaches source location to errors
//! for easier debugging. Use `.error()` to inspect the inner [`HeicError`], or
//! `.decompose()` to separate the error from its trace.
//!
//! For probing, [`ImageInfo::from_bytes`] returns a separate [`ProbeError`] enum
//! that distinguishes "not enough data" from "not a HEIC file" from "corrupt header".
//!
//! # Advanced: Raw YCbCr Access
//!
//! ```no_run
//! use heic::DecoderConfig;
//!
//! let data = std::fs::read("image.heic").unwrap();
//! let frame = DecoderConfig::new().decode_to_frame(&data).unwrap();
//! println!("{}x{}, bit_depth={}, chroma={}",
//!     frame.cropped_width(), frame.cropped_height(),
//!     frame.bit_depth, frame.chroma_format);
//!
//! // Access raw YCbCr planes
//! let (y_plane, y_stride) = frame.plane(0);
//! let (cb_plane, c_stride) = frame.plane(1);
//! let (cr_plane, _) = frame.plane(2);
//! ```
//!
//! # Memory
//!
//! Use [`DecoderConfig::estimate_memory`] to check memory requirements before
//! decoding. For grid-based images, [`DecodeRequest::decode_into`] reduces peak
//! memory by ~60% compared to [`DecodeRequest::decode`] by streaming tiles
//! directly to the output buffer.
//!
//! # Patents
//!
//! HEVC (H.265) and its use in HEIF containers may be covered by patents
//! held by third parties, including members of the Access Advance HEVC
//! patent pool. This decoder may or may not be subject to those patents
//! depending on jurisdiction and use.
//!
//! **Imazen holds no patents** and grants only **copyright** permissions
//! for the implementation in this crate (AGPL-3.0 or commercial). No
//! patent rights are granted. This codec is **decode-only**. We are not
//! lawyers; consult a patent attorney to determine whether a patent
//! license is required for your use.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
extern crate alloc;

// At least one backend feature must be active for the current target, or the
// crate is useless — a `heic` build with no HEVC decoder cannot decode HEIC
// images. Fail loudly with a list of available backends; user picks at least
// one in their `Cargo.toml`.
//
// As new native backends land (mediafoundation, videotoolbox, mediacodec,
// vaapi, d3d11va), they get added to the cfg-any list below paired with the
// target_os they support.
#[cfg(not(any(
    feature = "backend-rust",
    all(feature = "backend-mediafoundation", target_os = "windows"),
    all(
        feature = "backend-videotoolbox",
        any(
            target_os = "macos",
            target_os = "ios",
            target_os = "tvos",
            target_os = "visionos"
        )
    ),
    all(feature = "backend-mediacodec", target_os = "android"),
    all(feature = "backend-vaapi", target_os = "linux"),
    all(feature = "backend-d3d11va", target_os = "windows"),
)))]
compile_error!(
    "heic: no HEVC backend is enabled for this target. Enable at least one \
     of: `backend-rust` (any target), `backend-mediafoundation` (windows), \
     `backend-videotoolbox` (apple), `backend-mediacodec` (android), \
     `backend-vaapi` (linux), `backend-d3d11va` (windows). \
     Native backends are wired but not yet plumbed through the decode \
     dispatcher — they ship as opt-in subcrates and fall through to \
     `backend-rust` at decode time until the next PR. \
     Example: `cargo add heic --features backend-rust`."
);

whereat::define_at_crate_info!();

/// Allocate a `Vec<T>` filled with `len` copies of `val`, returning `Result`.
///
/// With `fallible-alloc` feature: uses `try_reserve` + `resize` (never panics on OOM).
/// Without `fallible-alloc` (default): uses `vec![val; len]` (fast memset path,
/// panics on OOM like standard Rust, but wraps result in `Ok`).
///
/// Always returns `Result<Vec<T>, HevcError>` so callers use `try_vec![...; ...]?`
/// — the `?` is visible at the call site, not hidden in the macro.
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

/// Fallible vec allocation — called by `try_vec!` when `fallible-alloc` is enabled.
#[cfg(feature = "fallible-alloc")]
#[inline]
pub(crate) fn alloc_vec_fallible<T: Clone>(
    len: usize,
    val: T,
) -> core::result::Result<alloc::vec::Vec<T>, error::HevcError> {
    let mut v = alloc::vec::Vec::new();
    v.try_reserve(len)
        .map_err(|_| error::HevcError::AllocationFailed)?;
    v.resize(len, val);
    Ok(v)
}

mod alloc_util;
mod auxiliary;
mod backend;
mod decode;
mod error;
#[doc(hidden)]
pub mod heif;
#[doc(hidden)]
pub mod hevc;

pub use backend::{Backend, recommended_backends};

#[cfg(feature = "zencodec")]
mod codec;

#[cfg(feature = "zencodec")]
pub use codec::{
    HeicAuxiliaryInfo, HeicDecodeJob, HeicDecoder as HeicZenDecoder, HeicDecoderConfig,
    HeicStreamDecoder,
};

// #[cfg(feature = "zennode")]
// pub mod zennode_defs;

pub use auxiliary::{
    AuxiliaryImageDescriptor, AuxiliaryImageType, DepthMap, DepthRepresentationInfo,
    DepthRepresentationType, SegmentationMatte,
};
pub use error::{HeicError, HevcError, ProbeError, Result};
pub use hevc::{DecodedFrame, VideoDecoder};

/// Decode a HEIC/HEIF image to tightly-packed 8-bit RGBA in one call.
///
/// Returns `(rgba, width, height)`, where `rgba` is `width * height * 4` bytes.
/// Decoding applies the decoder's safe default resource limits (16384×16384,
/// 256 MP, 1 GiB), so untrusted input can't exhaust memory; HDR / 10-bit content
/// is downconverted to 8-bit. For 16-bit/HDR output, tighter [`Limits`],
/// cancellation, zero-copy [`DecodeRequest::decode_into`], probing, or metadata,
/// use [`DecoderConfig`].
///
/// ```no_run
/// let data = std::fs::read("photo.heic").unwrap();
/// let (rgba, width, height) = heic::decode_rgba8(&data).unwrap();
/// assert_eq!(rgba.len(), width as usize * height as usize * 4);
/// ```
pub fn decode_rgba8(heic: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
    let out = DecoderConfig::new().decode(heic, PixelLayout::Rgba8)?;
    Ok((out.data, out.width, out.height))
}

/// Enable deblocking filter trace output (writes to /tmp/our_deblock_trace.txt)
#[cfg(feature = "std")]
pub fn enable_deblock_trace() {
    hevc::enable_deblock_trace();
}

/// Enable per-bin CABAC trace for the next `limit` bins (0 = disable)
#[cfg(feature = "std")]
pub fn cabac_bin_trace(limit: u32) {
    hevc::cabac::BIN_TRACE_LIMIT.store(limit, core::sync::atomic::Ordering::Relaxed);
    hevc::cabac::BIN_TRACE_COUNTER.store(0, core::sync::atomic::Ordering::Relaxed);
}

// Re-export Stop and Unstoppable for ergonomics
pub use enough::{Stop, StopReason, Unstoppable};

// Re-export At for error location tracking
pub use whereat::{At, at};

use alloc::borrow::Cow;
use alloc::vec::Vec;
use heif::{FourCC, ItemType};

/// Pixel layout for decoded output.
///
/// Determines the byte order and channel count of the decoded pixels.
/// All codecs must support at minimum `Rgba8` and `Bgra8`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PixelLayout {
    /// 3 bytes per pixel: red, green, blue
    Rgb8,
    /// 4 bytes per pixel: red, green, blue, alpha
    Rgba8,
    /// 3 bytes per pixel: blue, green, red
    Bgr8,
    /// 4 bytes per pixel: blue, green, red, alpha
    Bgra8,
}

impl PixelLayout {
    /// Bytes per pixel for this layout
    #[must_use]
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb8 | Self::Bgr8 => 3,
            Self::Rgba8 | Self::Bgra8 => 4,
        }
    }

    /// Whether this layout includes an alpha channel
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba8 | Self::Bgra8)
    }
}

/// Resource limits for decoding.
///
/// [`Limits::default()`] carries the same safe fallback the decoder applies
/// when no `Limits` are supplied at all (`16_384 × 16_384`, 256 megapixels,
/// 1 GiB) — see [`Limits::server_defaults`] for the rationale of those
/// numbers. This is deliberate: a caller that passes `Limits::default()`
/// gets a bounded decode, never a *weaker* one than passing nothing. Set a
/// field to `None` to lift that particular cap, or set tighter values to
/// further constrain adversarial or oversized input.
///
/// # Server safety
///
/// `Limits::default()` is **all-`None` (uncapped)** — passing
/// `Some(Limits::default())` to a decode therefore *removes* protection rather
/// than adding it. (Passing `None`/omitting limits is different: the decoder
/// then applies generous built-in default caps.) For untrusted input, start
/// from [`Limits::server_defaults`] (16 384² / 256 MP / 1 GiB) and tighten from
/// there, rather than `default()`:
///
/// ```
/// use heic::Limits;
///
/// let limits = Limits::server_defaults(); // capped; safe for untrusted input
/// ```
///
/// # Example
///
/// ```
/// use heic::Limits;
///
/// // Start from the safe defaults and tighten.
/// let mut limits = Limits::default();
/// limits.max_width = Some(8192);
/// limits.max_height = Some(8192);
/// limits.max_pixels = Some(64_000_000);
/// limits.max_memory_bytes = Some(512 * 1024 * 1024);
///
/// // Or explicitly lift a cap (back to unlimited for that field).
/// let mut unbounded_pixels = Limits::default();
/// unbounded_pixels.max_pixels = None;
/// ```
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum image width in pixels
    pub max_width: Option<u64>,
    /// Maximum image height in pixels
    pub max_height: Option<u64>,
    /// Maximum total pixel count (width * height)
    pub max_pixels: Option<u64>,
    /// Maximum memory usage in bytes
    pub max_memory_bytes: Option<u64>,
    /// Per-site allocation-fallibility preference, applied at the untrusted
    /// decode allocation sites (full-image planes, concatenated payloads).
    ///
    /// Internal carrier (`pub(crate)`): the `zencodec` decode adapter
    /// ([`crate::codec`]) sets it from
    /// `ResourceLimits::prefer_fallible_allocations`; the direct decode API
    /// leaves it `CodecDefault`, so each allocation site keeps its own default
    /// (big untrusted buffers fallible, small bounded scratch infallible). See
    /// [`crate::alloc_util`].
    pub(crate) alloc_pref: crate::alloc_util::AllocPreference,
}

impl Default for Limits {
    /// Returns the **safe fallback** caps (`16_384 × 16_384`, 256 megapixels,
    /// 1 GiB), identical to [`Limits::server_defaults`].
    ///
    /// This is intentionally *not* an all-`None` (unlimited) value. The
    /// decoder applies the same fallback internally when a caller supplies no
    /// `Limits` at all, so `Limits::default()` must match it — otherwise
    /// `Limits::default()` would be *weaker* than passing nothing, which is a
    /// footgun. A caller who genuinely wants no cap on a field sets it to
    /// `None` explicitly.
    fn default() -> Self {
        Self::server_defaults()
    }
}

impl Limits {
    /// Conservative defaults for **server-side** deployments where
    /// the input is untrusted (a public image-upload endpoint, a
    /// CDN transformation worker, etc.).
    ///
    /// * `max_width = max_height = 16_384` — covers 16K imagery; the
    ///   single-frame HEVC level-6.2 ceiling is 32_768 but real-
    ///   world HEIC photos top out at ~12K (full-frame DSLR
    ///   panoramas).
    /// * `max_pixels = 268_435_456` (256 megapixels) — high enough
    ///   for high-end stills, low enough to block 65k×65k probe
    ///   bombs.
    /// * `max_memory_bytes = 1_073_741_824` (1 GiB) — matches the
    ///   default upstream memory budget but documented explicitly.
    ///
    /// Tune per-deployment based on the hosting tier's actual
    /// memory budget. The defaults are intentionally generous
    /// enough to decode the median real photo without
    /// intervention, while bounding hostile-input blast radius.
    #[must_use]
    pub fn server_defaults() -> Self {
        Self {
            max_width: Some(16_384),
            max_height: Some(16_384),
            max_pixels: Some(268_435_456),
            max_memory_bytes: Some(1_073_741_824),
            alloc_pref: crate::alloc_util::AllocPreference::CodecDefault,
        }
    }

    /// Check that dimensions are within limits.
    pub(crate) fn check_dimensions(&self, width: u32, height: u32) -> Result<()> {
        if let Some(max_w) = self.max_width
            && u64::from(width) > max_w
        {
            return Err(at!(HeicError::LimitExceeded("image width exceeds limit")));
        }
        if let Some(max_h) = self.max_height
            && u64::from(height) > max_h
        {
            return Err(at!(HeicError::LimitExceeded("image height exceeds limit")));
        }
        if let Some(max_px) = self.max_pixels
            && u64::from(width) * u64::from(height) > max_px
        {
            return Err(at!(HeicError::LimitExceeded("pixel count exceeds limit")));
        }
        Ok(())
    }

    /// Check that estimated memory usage is within limits.
    pub(crate) fn check_memory(&self, estimated_bytes: u64) -> Result<()> {
        if let Some(max_mem) = self.max_memory_bytes
            && estimated_bytes > max_mem
        {
            return Err(at!(HeicError::LimitExceeded(
                "estimated memory exceeds limit"
            )));
        }
        Ok(())
    }
}

/// Sink for receiving decoded pixel rows during streaming decode.
///
/// The decoder calls [`demand()`](RowSink::demand) for each strip of rows,
/// writes decoded pixels into the returned buffer, then calls `demand()`
/// again for the next strip.
///
/// This enables streaming decode: the caller owns the output memory and
/// the decoder writes directly into it, one tile-row at a time for grid
/// images.
///
/// # Contract
///
/// - The codec calls `demand()` once per strip, in top-to-bottom order
///   (`y` increases monotonically).
/// - The returned buffer must be at least `min_bytes` bytes.
/// - Pixels are written tightly packed: `width × bpp` bytes per row,
///   `height` rows, no padding between rows.
/// - When `demand()` is called again, the previous buffer has been fully
///   written. When `decode_rows()` returns, the last buffer has been written.
pub trait RowSink {
    /// Provide a mutable buffer for decoded rows `y .. y + height`.
    ///
    /// `min_bytes` is the minimum buffer size needed. The returned slice
    /// must be at least `min_bytes` bytes long.
    fn demand(&mut self, y: u32, height: u32, min_bytes: usize) -> &mut [u8];
}

/// Decoded image output
#[derive(Debug, Clone)]
#[must_use]
pub struct DecodeOutput {
    /// Raw pixel data in the requested layout
    pub data: Vec<u8>,
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Pixel layout of the output data
    pub layout: PixelLayout,
}

/// Image metadata without full decode
#[derive(Debug, Clone)]
#[must_use]
pub struct ImageInfo {
    /// Image width in pixels
    pub width: u32,
    /// Image height in pixels
    pub height: u32,
    /// Whether the image has an alpha channel
    pub has_alpha: bool,
    /// Bit depth of the luma channel
    pub bit_depth: u8,
    /// Chroma format (0=mono, 1=4:2:0, 2=4:2:2, 3=4:4:4)
    pub chroma_format: u8,
    /// Whether the file contains EXIF metadata
    pub has_exif: bool,
    /// Whether the file contains XMP metadata
    pub has_xmp: bool,
    /// Whether the file contains a thumbnail image
    pub has_thumbnail: bool,
    /// Color primaries (CICP). 1=BT.709, 9=BT.2020, 12=Display P3, 2=unspecified
    pub color_primaries: u16,
    /// Transfer characteristics (CICP). 1=BT.709, 13=sRGB, 16=PQ, 18=HLG, 2=unspecified
    pub transfer_characteristics: u16,
    /// Matrix coefficients (CICP). 1=BT.709, 5/6=BT.601, 9=BT.2020, 2=unspecified
    pub matrix_coefficients: u16,
    /// Full range flag (CICP). true = full `[0,255]`, false = limited `[16,235]`
    pub video_full_range: bool,
    /// Whether the file contains an ICC profile (in colr box)
    pub has_icc_profile: bool,
    /// Whether the file contains a depth auxiliary image
    pub has_depth: bool,
    /// Whether the file contains an HDR gain map auxiliary image
    pub has_gain_map: bool,
    /// Raw EXIF bytes (TIFF format, starting with byte-order mark `II` or `MM`).
    ///
    /// The HEIF 4-byte offset prefix is stripped. `None` if no EXIF metadata is present.
    pub exif: Option<Vec<u8>>,
    /// Raw XMP XML bytes. `None` if no XMP metadata is present.
    pub xmp: Option<Vec<u8>>,
    /// Raw ICC profile bytes from the `colr` box. `None` if the file uses nclx
    /// color parameters or has no ICC profile.
    pub icc_profile: Option<Vec<u8>>,
}

/// Adjust dimensions for transforms that the decoder will apply.
///
/// Rotation by 90° or 270° swaps width and height. Mirror and 180° rotation
/// don't change dimensions. Clean aperture crops to rational dimensions
/// (approximated here via integer division).
fn apply_transform_dimensions(
    mut w: u32,
    mut h: u32,
    transforms: &[heif::Transform],
) -> (u32, u32) {
    for t in transforms {
        match t {
            heif::Transform::Rotation(rot) if rot.angle == 90 || rot.angle == 270 => {
                core::mem::swap(&mut w, &mut h);
            }
            heif::Transform::CleanAperture(clap) if clap.width_d > 0 && clap.height_d > 0 => {
                w = clap.width_n / clap.width_d;
                h = clap.height_n / clap.height_d;
            }
            _ => {} // mirror, 0°, 180° don't change dimensions
        }
    }
    (w, h)
}

impl ImageInfo {
    /// Minimum bytes needed to attempt header parsing.
    ///
    /// HEIF containers have variable-length headers, so this is a typical
    /// minimum. [`from_bytes`](Self::from_bytes) may return
    /// [`ProbeError::NeedMoreData`] if the header extends beyond this.
    pub const PROBE_BYTES: usize = 4096;

    /// Parse image metadata from a byte slice without full decoding.
    ///
    /// This only parses the HEIF container and HEVC parameter sets,
    /// without decoding any pixel data.
    ///
    /// # Errors
    ///
    /// Returns [`ProbeError::NeedMoreData`] if the buffer is too small,
    /// [`ProbeError::InvalidFormat`] if this is not a HEIC/HEIF file,
    /// or [`ProbeError::Corrupt`] if the header is malformed.
    pub fn from_bytes(data: &[u8]) -> core::result::Result<Self, ProbeError> {
        if data.len() < 12 {
            return Err(ProbeError::NeedMoreData);
        }

        // Quick format check: HEIF files start with ftyp box
        let box_type = &data[4..8];
        if box_type != b"ftyp" {
            return Err(ProbeError::InvalidFormat);
        }

        let container = heif::parse(data, &Unstoppable).map_err(ProbeError::Corrupt)?;

        let primary_item = container
            .primary_item()
            .ok_or_else(|| ProbeError::Corrupt(at!(HeicError::NoPrimaryImage)))?;

        // Check for alpha auxiliary image
        let has_alpha = !container
            .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
            .is_empty()
            || !container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
                )
                .is_empty();

        // Check for EXIF and XMP metadata
        let has_exif = container
            .item_infos
            .iter()
            .any(|i| i.item_type == FourCC(*b"Exif"));
        let has_xmp = container.item_infos.iter().any(|i| {
            i.item_type == FourCC(*b"mime")
                && (i.content_type.contains("xmp") || i.content_type.contains("rdf+xml"))
        });
        let has_thumbnail = !container.find_thumbnails(primary_item.id).is_empty();

        // Check for depth auxiliary image
        let has_depth = !container
            .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:2")
            .is_empty()
            || !container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:depth",
                )
                .is_empty();

        // Check for HDR gain map. Either the Apple aux-item URN, OR a
        // `tmap` derived item (HEIF Amendment 1 / ISO 23008-12:2025) with
        // at least two `dimg` references (base + gain map).
        let has_gain_map = !container
            .find_auxiliary_items(primary_item.id, "urn:com:apple:photo:2020:aux:hdrgainmap")
            .is_empty()
            || container.items().any(|i| {
                i.item_type == ItemType::Tmap
                    && container.get_item_references(i.id, FourCC::DIMG).len() >= 2
            });

        // Extract CICP from colr nclx box on the primary item
        let (color_primaries, transfer_characteristics, matrix_coefficients, video_full_range) =
            match &primary_item.color_info {
                Some(heif::ColorInfo::Nclx {
                    color_primaries,
                    transfer_characteristics,
                    matrix_coefficients,
                    full_range,
                }) => (
                    *color_primaries,
                    *transfer_characteristics,
                    *matrix_coefficients,
                    *full_range,
                ),
                _ => (2, 2, 2, false), // unspecified defaults
            };
        let has_icc_profile = matches!(
            &primary_item.color_info,
            Some(heif::ColorInfo::IccProfile(_))
        );

        // Extract raw metadata bytes
        let exif = Self::extract_exif_from_container(&container);
        let xmp = Self::extract_xmp_from_container(&container);
        let icc_profile = match &primary_item.color_info {
            Some(heif::ColorInfo::IccProfile(icc)) => Some(icc.clone()),
            _ => None,
        };

        // Try to get info from HEVC config (fast path for direct HEVC items)
        if let Some(ref config) = primary_item.hevc_config
            && let Ok(hevc_info) = hevc::get_info_from_config(config)
        {
            let bit_depth = config.bit_depth_luma_minus8 + 8;
            let chroma_format = config.chroma_format;
            let (width, height) = apply_transform_dimensions(
                hevc_info.width,
                hevc_info.height,
                &primary_item.transforms,
            );
            return Ok(ImageInfo {
                width,
                height,
                has_alpha,
                bit_depth,
                chroma_format,
                has_exif,
                has_xmp,
                has_thumbnail,
                color_primaries,
                transfer_characteristics,
                matrix_coefficients,
                video_full_range,
                has_icc_profile,
                has_depth,
                has_gain_map,
                exif: exif.clone(),
                xmp: xmp.clone(),
                icc_profile: icc_profile.clone(),
            });
        }

        // For grid/iden/iovl/av01/unci and other non-hvc1: get dimensions from ispe,
        // bit depth from first tile's codec config or from the item's own config.
        if let Some(ref config) = primary_item.av1_config
            && let Some((w, h)) = primary_item.dimensions
        {
            let bit_depth = config.bit_depth();
            let chroma_format = config.chroma_format();
            let (width, height) = apply_transform_dimensions(w, h, &primary_item.transforms);
            return Ok(ImageInfo {
                width,
                height,
                has_alpha,
                bit_depth,
                chroma_format,
                has_exif,
                has_xmp,
                has_thumbnail,
                color_primaries,
                transfer_characteristics,
                matrix_coefficients,
                video_full_range,
                has_icc_profile,
                has_depth,
                has_gain_map,
                exif: exif.clone(),
                xmp: xmp.clone(),
                icc_profile: icc_profile.clone(),
            });
        }

        if primary_item.uncompressed_config.is_some()
            && let Some((w, h)) = primary_item.dimensions
        {
            let bit_depth = primary_item
                .uncompressed_config
                .as_ref()
                .and_then(|c| c.components.first())
                .map(|c| c.component_bit_depth_minus_one + 1)
                .unwrap_or(8);
            let (width, height) = apply_transform_dimensions(w, h, &primary_item.transforms);
            return Ok(ImageInfo {
                width,
                height,
                has_alpha,
                bit_depth,
                chroma_format: 3, // unci is typically RGB = 4:4:4
                has_exif,
                has_xmp,
                has_thumbnail,
                color_primaries,
                transfer_characteristics,
                matrix_coefficients,
                video_full_range,
                has_icc_profile,
                has_depth,
                has_gain_map,
                exif: exif.clone(),
                xmp: xmp.clone(),
                icc_profile: icc_profile.clone(),
            });
        }

        if primary_item.item_type != ItemType::Hvc1
            && let Some((w, h)) = primary_item.dimensions
        {
            // Try to get bit depth from the first dimg tile reference
            let mut bit_depth = 8u8;
            let mut chroma_format = 1u8;
            for r in &container.item_references {
                if r.reference_type == FourCC::DIMG
                    && r.from_item_id == primary_item.id
                    && let Some(&tile_id) = r.to_item_ids.first()
                    && let Some(tile) = container.get_item(tile_id)
                {
                    if let Some(ref config) = tile.hevc_config {
                        bit_depth = config.bit_depth_luma_minus8 + 8;
                        chroma_format = config.chroma_format;
                        break;
                    } else if let Some(ref config) = tile.av1_config {
                        bit_depth = config.bit_depth();
                        chroma_format = config.chroma_format();
                        break;
                    }
                }
            }
            let (width, height) = apply_transform_dimensions(w, h, &primary_item.transforms);
            return Ok(ImageInfo {
                width,
                height,
                has_alpha,
                bit_depth,
                chroma_format,
                has_exif,
                has_xmp,
                has_thumbnail,
                color_primaries,
                transfer_characteristics,
                matrix_coefficients,
                video_full_range,
                has_icc_profile,
                has_depth,
                has_gain_map,
                exif: exif.clone(),
                xmp: xmp.clone(),
                icc_profile: icc_profile.clone(),
            });
        }

        // Fallback to reading image data
        let image_data = container
            .get_item_data(primary_item.id)
            .map_err(ProbeError::Corrupt)?;

        let hevc_info = hevc::get_info(&image_data)
            .map_err(|e| ProbeError::Corrupt(at!(HeicError::from(e))))?;

        let (width, height) =
            apply_transform_dimensions(hevc_info.width, hevc_info.height, &primary_item.transforms);
        Ok(ImageInfo {
            width,
            height,
            has_alpha,
            bit_depth: 8,
            chroma_format: 1,
            has_exif,
            has_xmp,
            has_thumbnail,
            color_primaries,
            transfer_characteristics,
            matrix_coefficients,
            video_full_range,
            has_icc_profile,
            has_depth,
            has_gain_map,
            exif,
            xmp,
            icc_profile,
        })
    }

    /// Extract EXIF TIFF bytes from a pre-parsed HEIF container.
    fn extract_exif_from_container(container: &heif::HeifContainer<'_>) -> Option<Vec<u8>> {
        for info in &container.item_infos {
            if info.item_type != FourCC(*b"Exif") {
                continue;
            }
            let Ok(exif_data) = container.get_item_data(info.item_id) else {
                continue;
            };
            if exif_data.len() < 4 {
                continue;
            }
            let tiff_offset =
                u32::from_be_bytes([exif_data[0], exif_data[1], exif_data[2], exif_data[3]])
                    as usize;
            let tiff_start = 4 + tiff_offset;
            if tiff_start < exif_data.len() {
                return Some(exif_data[tiff_start..].to_vec());
            }
        }
        None
    }

    /// Extract XMP XML bytes from a pre-parsed HEIF container.
    fn extract_xmp_from_container(container: &heif::HeifContainer<'_>) -> Option<Vec<u8>> {
        for info in &container.item_infos {
            if info.item_type == FourCC(*b"mime")
                && (info.content_type.contains("xmp")
                    || info.content_type.contains("rdf+xml")
                    || info.content_type == "application/rdf+xml")
                && let Ok(xmp_data) = container.get_item_data(info.item_id)
            {
                return Some(xmp_data.into_owned());
            }
        }
        None
    }

    /// Calculate the required output buffer size for a given pixel layout.
    ///
    /// Returns `None` if the dimensions would overflow `usize`.
    #[must_use]
    pub fn output_buffer_size(self, layout: PixelLayout) -> Option<usize> {
        (self.width as usize)
            .checked_mul(self.height as usize)?
            .checked_mul(layout.bytes_per_pixel())
    }
}

/// Which container mechanism produced the [`HdrGainMap`].
///
/// HEIC stores gain maps via either Apple's iOS 14+ auxiliary item path
/// (XMP metadata, `urn:com:apple:photo:2020:aux:hdrgainmap`) or the
/// HEIF Amendment 1 / ISO 23008-12:2025 `tmap` derived image item
/// (ISO 21496-1 binary metadata, the canonical AVIF tmap variant).
/// Consumers branch on this when interpreting the metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GainMapOrigin {
    /// Apple HEIC auxiliary item with the
    /// `urn:com:apple:photo:2020:aux:hdrgainmap` URN.
    /// Metadata lives in [`HdrGainMap::xmp`].
    AppleAuxItem,
    /// HEIF Amendment 1 `tmap` derived image item. Metadata lives in
    /// [`HdrGainMap::iso21496`] as the AVIF tmap byte payload (per
    /// ISO 21496-1, version byte first).
    HeifTmap,
}

/// HDR gain map data extracted from an auxiliary image or tone-map item.
///
/// The gain map is a grayscale image (typically lower resolution than the
/// primary) used with the Apple HDR or ISO 21496-1 formula to reconstruct HDR:
/// ```text
/// sdr_linear = sRGB_EOTF(sdr_pixel)
/// gainmap_linear = sRGB_EOTF(gainmap_pixel)
/// scale = 1.0 + (headroom - 1.0) * gainmap_linear
/// hdr_linear = sdr_linear * scale
/// ```
/// Where `headroom` and other parameters come from either [`xmp`](Self::xmp)
/// (Apple aux-item path) or [`iso21496`](Self::iso21496) (HEIF Amendment 1
/// tmap path). [`origin`](Self::origin) names which mechanism produced this
/// instance. Parse the metadata with `ultrahdr-core` (for XMP) or
/// `zencodec::gainmap::parse_iso21496_fmt(..., Iso21496Format::AvifTmap)`
/// (for the binary payload).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HdrGainMap {
    /// Grayscale gain map pixels (u8).
    ///
    /// For sources with `bit_depth > 8`, values are scaled to 8-bit.
    /// Length is `width * height`.
    pub data: Vec<u8>,
    /// Gain map width in pixels.
    pub width: u32,
    /// Gain map height in pixels.
    pub height: u32,
    /// Significant bits per sample in the source HEVC stream (typically 8).
    pub bit_depth: u8,
    /// Raw XMP bytes attached to the gain map item, when present. `None`
    /// outside the [`AppleAuxItem`] origin.
    ///
    /// Note: Apple's aux-item XMP is a small Apple-specific packet — it does
    /// NOT carry ISO 21496-1 / `hdrgm:` gain-map parameters. Parameter
    /// sources are [`iso21496`](Self::iso21496) (`tmap` files) or the EXIF
    /// MakerNote HDR headroom (legacy Apple files).
    ///
    /// [`AppleAuxItem`]: GainMapOrigin::AppleAuxItem
    pub xmp: Option<Vec<u8>>,
    /// Raw ISO 21496-1 binary metadata from a `tmap` derived item (AVIF
    /// tmap byte layout: version byte first, then the ISO payload). `None`
    /// outside the [`HeifTmap`] origin.
    ///
    /// [`HeifTmap`]: GainMapOrigin::HeifTmap
    pub iso21496: Option<Vec<u8>>,
    /// Which container mechanism this gain map was decoded from.
    pub origin: GainMapOrigin,
}

/// Decoder configuration. Reusable across multiple decode operations.
///
/// For HEIC, the decoder has no required configuration parameters.
/// Use [`new()`](Self::new) for sensible defaults.
///
/// # Example
///
/// ```ignore
/// use heic::{DecoderConfig, PixelLayout};
///
/// let config = DecoderConfig::new();
/// let output = config.decode(&data, PixelLayout::Rgba8)?;
/// ```
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    /// Ordered allowlist of HEVC backends to try for each decode.
    ///
    /// Empty by default — `DecoderConfig::new()` populates the list with
    /// [`recommended_backends`] so the most common case (`config.decode(...)`)
    /// just works. Override with [`Self::with_backends`] or
    /// [`Self::with_backend`].
    ///
    /// See the [`backend`](crate::backend) module for fallthrough semantics.
    /// As of PR2 the dispatcher is not yet plumbed through the internal
    /// decode pipeline (still always uses `backend-rust`); subsequent PRs
    /// land that wiring as the native backends arrive.
    backends: Vec<Backend>,
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoderConfig {
    /// Create a new decoder configuration with sensible defaults.
    ///
    /// The default backend allowlist is [`recommended_backends`]: native
    /// platform backends (when compiled in for the host target) followed by
    /// the pure-Rust decoder as a last-resort fallback.
    #[must_use]
    pub fn new() -> Self {
        Self {
            backends: recommended_backends(),
        }
    }

    /// Set the ordered allowlist of HEVC backends.
    ///
    /// The first entry whose `is_available()` returns true is tried first;
    /// recoverable failures (backend unavailable, bitstream rejection)
    /// fall through to subsequent entries. An empty list causes decode to
    /// return [`HeicError::NoBackendSelected`].
    ///
    /// See [`Backend`] for the available variants and the allowlist
    /// fallthrough semantics.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use heic::{Backend, DecoderConfig};
    ///
    /// // Prefer the platform decoder; fall back to pure-Rust if it can't run.
    /// let config = DecoderConfig::new()
    ///     .with_backends(&[Backend::VideoToolbox, Backend::Rust]);
    /// ```
    #[must_use]
    pub fn with_backends(mut self, backends: &[Backend]) -> Self {
        self.backends = backends.to_vec();
        self
    }

    /// Convenience shortcut for [`Self::with_backends`] with a single entry.
    #[must_use]
    pub fn with_backend(self, backend: Backend) -> Self {
        self.with_backends(&[backend])
    }

    /// Return the backend allowlist currently set on this config.
    ///
    /// The decode pipeline consults this in order on every HEVC tile.
    #[must_use]
    pub fn backends(&self) -> &[Backend] {
        &self.backends
    }

    /// One-shot decode: decode HEIC data to pixels in the requested layout.
    ///
    /// This is a convenience shortcut for:
    /// ```ignore
    /// config.decode_request(data)
    ///     .with_output_layout(layout)
    ///     .decode()
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format
    /// or if decoding fails.
    pub fn decode(&self, data: &[u8], layout: PixelLayout) -> Result<DecodeOutput> {
        self.decode_request(data)
            .with_output_layout(layout)
            .decode()
    }

    /// Create a decode request for full control over the decode operation.
    ///
    /// The request defaults to `PixelLayout::Rgba8`. Use builder methods
    /// to set output layout, limits, and cancellation.
    pub fn decode_request<'a>(&'a self, data: &'a [u8]) -> DecodeRequest<'a> {
        DecodeRequest {
            _config: self,
            data,
            layout: PixelLayout::Rgba8,
            limits: None,
            stop: None,
            max_threads: None,
            apply_orientation: true,
        }
    }

    /// Decode HEIC data to raw YCbCr frame.
    ///
    /// This returns the internal `DecodedFrame` representation before
    /// color conversion. Useful for debugging, testing, and advanced
    /// use cases that need direct YCbCr access.
    ///
    /// # Errors
    ///
    /// Returns an error if the data is not valid HEIC/HEIF format.
    pub fn decode_to_frame(&self, data: &[u8]) -> Result<hevc::DecodedFrame> {
        decode::decode_to_frame(data, None, &Unstoppable, None, self.backends(), true)
    }

    /// Estimate the peak memory usage for decoding an image of given dimensions.
    ///
    /// Returns the estimated byte count including:
    /// - YCbCr frame planes (Y + Cb + Cr at 4:2:0)
    /// - Output pixel buffer at the requested layout
    /// - Deblocking metadata
    ///
    /// This is a conservative upper bound. Actual usage may be lower if the
    /// image uses monochrome or if tiles are decoded sequentially.
    #[must_use]
    pub fn estimate_memory(width: u32, height: u32, layout: PixelLayout) -> u64 {
        // Saturating arithmetic so a hostile SPS (`pic_width_in_luma_
        // samples = u32::MAX`, etc.) can't wrap the byte estimate to a
        // small value and slip past `Limits::check_memory`. The
        // saturation point (u64::MAX bytes ≈ 16 exabytes) is far
        // above any sensible `max_memory_bytes` cap, so a wrapped
        // estimate would correctly fail the limit check.
        let w = u64::from(width);
        let h = u64::from(height);
        let pixels = w.saturating_mul(h);

        // YCbCr planes (u16 per sample)
        let luma_bytes = pixels.saturating_mul(2);
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);
        let chroma_bytes = chroma_w
            .saturating_mul(chroma_h)
            .saturating_mul(2)
            .saturating_mul(2); // Cb + Cr

        // Output pixel buffer
        let output_bytes = pixels.saturating_mul(layout.bytes_per_pixel() as u64);

        // Deblocking metadata (flags + QP map at 4x4 granularity)
        let blocks_w = w.div_ceil(4);
        let blocks_h = h.div_ceil(4);
        let deblock_bytes = blocks_w.saturating_mul(blocks_h).saturating_mul(2);

        luma_bytes
            .saturating_add(chroma_bytes)
            .saturating_add(output_bytes)
            .saturating_add(deblock_bytes)
    }

    /// Check if the primary image has an HDR gain map auxiliary image.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn has_gain_map(&self, data: &[u8]) -> Result<bool> {
        decode::has_gain_map(data)
    }

    /// Decode the HDR gain map from an Apple HDR or HEIF `tmap` HEIC file.
    ///
    /// Returns the grayscale gain map pixels (scaled to 8-bit), the source
    /// bit depth, and whichever reconstruction metadata the container carries.
    /// The gain map is typically lower resolution than the primary image, and
    /// [`HdrGainMap::origin`] names which mechanism produced it.
    ///
    /// The parameters needed to apply the gain map come from one of two paths,
    /// depending on the origin (see [`HdrGainMap`] for the full layout):
    /// - [`HdrGainMap::xmp`] — Apple aux-item XMP (Apple-specific packet; does
    ///   NOT carry ISO 21496-1 / `hdrgm:` parameters). Parse with
    ///   `ultrahdr-core`. `None` for `tmap` gain maps.
    /// - [`HdrGainMap::iso21496`] — raw ISO 21496-1 binary metadata from a
    ///   HEIF Amendment 1 `tmap` derived item. Parse with e.g.
    ///   `zencodec::gainmap::parse_iso21496_fmt(..., Iso21496Format::AvifTmap)`.
    ///   `None` for the Apple aux-item path.
    ///
    /// # Errors
    ///
    /// Returns an error if the file has no gain map or decoding fails.
    pub fn decode_gain_map(&self, data: &[u8]) -> Result<HdrGainMap> {
        decode::decode_gain_map(data, self.backends())
    }

    /// Extract raw EXIF (TIFF) data from a HEIC file.
    ///
    /// Returns the TIFF-header data (starting with byte-order mark `II` or `MM`)
    /// with the HEIF 4-byte offset prefix stripped. Returns `None` if the file
    /// contains no EXIF metadata.
    ///
    /// Returns `Cow::Borrowed` (zero-copy) for single-extent items,
    /// `Cow::Owned` for multi-extent items.
    ///
    /// The returned bytes can be passed to any EXIF parser (e.g., `exif` or `kamadak-exif` crate).
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn extract_exif<'a>(&self, data: &'a [u8]) -> Result<Option<Cow<'a, [u8]>>> {
        decode::extract_exif(data)
    }

    /// Extract raw XMP (XML) data from a HEIC file.
    ///
    /// Returns the raw XML bytes of the XMP metadata. Returns `None` if the
    /// file contains no XMP metadata.
    ///
    /// Returns `Cow::Borrowed` (zero-copy) for single-extent items,
    /// `Cow::Owned` for multi-extent items.
    ///
    /// XMP items are stored as `mime` type items with content type
    /// `application/rdf+xml` in the HEIF container.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn extract_xmp<'a>(&self, data: &'a [u8]) -> Result<Option<Cow<'a, [u8]>>> {
        decode::extract_xmp(data)
    }

    /// Extract ICC profile data from a HEIC file.
    ///
    /// Returns the raw ICC profile bytes from the colr box, or `None` if the
    /// file uses nclx color parameters instead.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn extract_icc(&self, data: &[u8]) -> Result<Option<Vec<u8>>> {
        let container = heif::parse(data, &Unstoppable)?;
        let primary_item = container
            .primary_item()
            .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;
        match &primary_item.color_info {
            Some(heif::ColorInfo::IccProfile(icc)) => Ok(Some(icc.clone())),
            _ => Ok(None),
        }
    }

    /// Decode the thumbnail image from a HEIC file.
    ///
    /// Returns the decoded thumbnail as a `DecodeOutput` in the requested layout,
    /// or `None` if no thumbnail is present. Thumbnails are typically much smaller
    /// than the primary image (e.g. 320x212 for a 1280x854 primary).
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed or thumbnail decoding fails.
    pub fn decode_thumbnail(
        &self,
        data: &[u8],
        layout: PixelLayout,
    ) -> Result<Option<DecodeOutput>> {
        decode::decode_thumbnail(data, layout, self.backends())
    }

    /// List all auxiliary images linked to the primary image.
    ///
    /// Returns descriptors for each auxiliary image, including its type,
    /// item ID, and dimensions (if known).
    ///
    /// Common auxiliary types include alpha planes, depth maps, portrait
    /// effect mattes, and HDR gain maps.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn auxiliary_images(&self, data: &[u8]) -> Result<Vec<AuxiliaryImageDescriptor>> {
        decode::list_auxiliary_images(data)
    }

    /// List the types of all auxiliary images present.
    ///
    /// Convenience wrapper around [`auxiliary_images`](Self::auxiliary_images)
    /// that returns just the type enum values.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn auxiliary_types(&self, data: &[u8]) -> Result<Vec<AuxiliaryImageType>> {
        Ok(decode::list_auxiliary_images(data)?
            .into_iter()
            .map(|d| d.aux_type)
            .collect())
    }

    /// Check if the primary image has a depth auxiliary image.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn has_depth(&self, data: &[u8]) -> Result<bool> {
        decode::has_depth(data)
    }

    /// Decode the depth map auxiliary image.
    ///
    /// Returns the depth pixels as grayscale u16 samples (luma plane)
    /// along with width, height, bit depth, and depth representation info.
    ///
    /// iPhone depth maps are typically monochrome HEVC at lower resolution
    /// than the primary image. The depth representation info describes
    /// how to interpret the pixel values (inverse Z, disparity, etc.).
    ///
    /// # Errors
    ///
    /// Returns an error if the file has no depth map, the depth item
    /// cannot be located, or HEVC decoding fails.
    pub fn decode_depth(&self, data: &[u8]) -> Result<DepthMap> {
        decode::decode_depth(data, self.backends())
    }

    /// Decode a specific auxiliary image by item ID.
    ///
    /// Use [`auxiliary_images`](Self::auxiliary_images) to discover item IDs,
    /// then decode individual auxiliary images with this method.
    ///
    /// The output follows the same pixel layout as the primary image decoder.
    /// For monochrome auxiliary images (depth, mattes), the R/G/B channels
    /// will all contain the luma value.
    ///
    /// # Errors
    ///
    /// Returns an error if the item ID is invalid or decoding fails.
    pub fn decode_auxiliary(
        &self,
        data: &[u8],
        item_id: u32,
        layout: PixelLayout,
    ) -> Result<DecodeOutput> {
        decode::decode_auxiliary_item(data, item_id, layout, self.backends())
    }

    /// Decode all available segmentation mattes from a HEIC file.
    ///
    /// Looks for portrait, skin, hair, teeth, and glasses mattes stored
    /// as monochrome HEVC auxiliary images and decodes each to an 8-bit
    /// grayscale mask.
    ///
    /// Returns an empty `Vec` if no mattes are present.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed or matte
    /// decoding fails.
    pub fn decode_mattes(&self, data: &[u8]) -> Result<Vec<SegmentationMatte>> {
        decode::decode_mattes(data, self.backends())
    }

    /// Decode a specific segmentation matte type from a HEIC file.
    ///
    /// Returns `None` if the requested matte type is not present in
    /// the file.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed or matte
    /// decoding fails.
    pub fn decode_matte(
        &self,
        data: &[u8],
        matte_type: &AuxiliaryImageType,
    ) -> Result<Option<SegmentationMatte>> {
        decode::decode_matte(data, matte_type, self.backends())
    }
}

/// A decode request binding data, output format, limits, and cancellation.
///
/// Created by [`DecoderConfig::decode_request`]. Use builder methods to
/// configure, then call [`decode`](Self::decode) or
/// [`decode_into`](Self::decode_into).
#[must_use]
pub struct DecodeRequest<'a> {
    _config: &'a DecoderConfig,
    data: &'a [u8],
    layout: PixelLayout,
    limits: Option<&'a Limits>,
    stop: Option<&'a dyn Stop>,
    /// Maximum thread count for parallel tile decoding.
    ///
    /// `None` means use the default (global rayon pool when `parallel` feature
    /// is enabled, sequential otherwise). `Some(1)` forces single-threaded
    /// decode even when the `parallel` feature is on.
    max_threads: Option<usize>,
    /// Whether to bake the image's orientation (`irot`/`imir`) into the
    /// decoded pixels. Default `true`.
    apply_orientation: bool,
}

impl<'a> DecodeRequest<'a> {
    /// Set the desired output pixel layout.
    ///
    /// Default is `PixelLayout::Rgba8`.
    pub fn with_output_layout(mut self, layout: PixelLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Set whether the image's orientation transforms (`irot`/`imir`) are
    /// baked into the decoded pixels.
    ///
    /// Default `true`: the decoder applies rotation/mirror so the output is in
    /// display orientation (the historical, convenient behavior). Set `false`
    /// to keep the pixels in their stored orientation — the caller is then
    /// responsible for applying the orientation. The clean-aperture crop
    /// (`clap`) is always applied regardless of this flag.
    pub fn with_apply_orientation(mut self, apply_orientation: bool) -> Self {
        self.apply_orientation = apply_orientation;
        self
    }

    /// Set resource limits for this decode operation.
    ///
    /// Limits are checked before allocations. Exceeding any limit
    /// returns [`HeicError::LimitExceeded`].
    pub fn with_limits(mut self, limits: &'a Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Set a cooperative cancellation token.
    ///
    /// The decoder will periodically check this token and return
    /// [`HeicError::Cancelled`] if the operation should stop.
    pub fn with_stop(mut self, stop: &'a dyn Stop) -> Self {
        self.stop = Some(stop);
        self
    }

    /// Limit the number of threads used for parallel tile decoding.
    ///
    /// When the `parallel` feature is enabled, tile decoding uses rayon for
    /// parallelism. This method overrides the default thread count:
    ///
    /// - `Some(1)` forces single-threaded decode
    /// - `Some(n)` uses at most `n` threads
    /// - `None` (default) uses the global rayon pool
    ///
    /// Without the `parallel` feature, this has no effect.
    pub fn with_max_threads(mut self, max_threads: usize) -> Self {
        self.max_threads = Some(max_threads);
        self
    }

    /// Execute the decode and return pixel data.
    ///
    /// # Errors
    ///
    /// Returns an error if the data is invalid, a limit is exceeded,
    /// or the operation is cancelled.
    pub fn decode(self) -> Result<DecodeOutput> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);
        let frame = decode::decode_to_frame(
            self.data,
            self.limits,
            stop,
            self.max_threads,
            self._config.backends(),
            self.apply_orientation,
        )?;

        let width = frame.cropped_width();
        let height = frame.cropped_height();

        // Check limits on final output dimensions
        if let Some(limits) = self.limits {
            limits.check_dimensions(width, height)?;
            let output_bytes =
                u64::from(width) * u64::from(height) * self.layout.bytes_per_pixel() as u64;
            limits.check_memory(output_bytes)?;
        }

        let data = match self.layout {
            PixelLayout::Rgb8 => frame.to_rgb().map_err(crate::error::at_core)?,
            PixelLayout::Rgba8 => frame.to_rgba().map_err(crate::error::at_core)?,
            PixelLayout::Bgr8 => frame.to_bgr().map_err(crate::error::at_core)?,
            PixelLayout::Bgra8 => frame.to_bgra().map_err(crate::error::at_core)?,
        };

        Ok(DecodeOutput {
            data,
            width,
            height,
            layout: self.layout,
        })
    }

    /// Decode directly into a pre-allocated buffer.
    ///
    /// The buffer must be at least `width * height * layout.bytes_per_pixel()` bytes.
    /// Use [`ImageInfo::from_bytes`] to determine the required size beforehand.
    ///
    /// Returns `(width, height)` on success.
    ///
    /// For grid-based images (most iPhone photos) without transforms or alpha,
    /// this uses a streaming path that color-converts each tile directly into
    /// the output buffer, avoiding the intermediate full-frame YCbCr allocation.
    ///
    /// # Errors
    ///
    /// Returns [`HeicError::BufferTooSmall`] if the output buffer is too small,
    /// or other errors if decoding fails.
    pub fn decode_into(self, output: &mut [u8]) -> Result<(u32, u32)> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);

        // Try streaming path for eligible grid images (no full-frame YCbCr allocation)
        if let Some(result) = decode::try_decode_grid_streaming(
            self.data,
            self.limits,
            stop,
            self.layout,
            output,
            self.max_threads,
            self._config.backends(),
        )? {
            return Ok(result);
        }

        // Fallback: full-frame decode then color convert
        let frame = decode::decode_to_frame(
            self.data,
            self.limits,
            stop,
            self.max_threads,
            self._config.backends(),
            self.apply_orientation,
        )?;

        let width = frame.cropped_width();
        let height = frame.cropped_height();
        let required = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(self.layout.bytes_per_pixel()))
            .ok_or_else(|| {
                at!(HeicError::LimitExceeded(
                    "output buffer size overflows usize",
                ))
            })?;

        if output.len() < required {
            return Err(at!(HeicError::BufferTooSmall {
                required,
                actual: output.len(),
            }));
        }

        match self.layout {
            PixelLayout::Rgb8 => {
                frame.write_rgb_into(output);
            }
            PixelLayout::Rgba8 => {
                frame.write_rgba_into(output);
            }
            PixelLayout::Bgr8 => {
                frame.write_bgr_into(output);
            }
            PixelLayout::Bgra8 => {
                frame.write_bgra_into(output);
            }
        }

        Ok((width, height))
    }

    /// Decode with row-level streaming into a caller-managed sink.
    ///
    /// The decoder calls [`RowSink::demand()`] for each strip of rows and
    /// writes decoded pixels directly into the returned buffer.
    ///
    /// For grid-based images (most iPhone photos) without transforms or alpha,
    /// this streams one tile-row at a time — peak memory is proportional to
    /// one row of tiles instead of the full image.
    ///
    /// For other images, falls back to full-frame decode then writes strips
    /// to the sink.
    ///
    /// Returns `(width, height)` on success.
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails, limits are exceeded,
    /// or the operation is cancelled.
    pub fn decode_rows(self, sink: &mut dyn RowSink) -> Result<(u32, u32)> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);

        // Try streaming path for eligible grid images
        if let Some(result) = decode::try_decode_grid_to_sink(
            self.data,
            self.limits,
            stop,
            self.layout,
            sink,
            self.max_threads,
            self._config.backends(),
        )? {
            return Ok(result);
        }

        // Fallback: full-frame decode then write to sink as one strip
        let frame = decode::decode_to_frame(
            self.data,
            self.limits,
            stop,
            self.max_threads,
            self._config.backends(),
            self.apply_orientation,
        )?;

        let width = frame.cropped_width();
        let height = frame.cropped_height();

        // Check limits on final output dimensions
        if let Some(limits) = self.limits {
            limits.check_dimensions(width, height)?;
            let output_bytes =
                u64::from(width) * u64::from(height) * self.layout.bytes_per_pixel() as u64;
            limits.check_memory(output_bytes)?;
        }

        let data = match self.layout {
            PixelLayout::Rgb8 => frame.to_rgb().map_err(crate::error::at_core)?,
            PixelLayout::Rgba8 => frame.to_rgba().map_err(crate::error::at_core)?,
            PixelLayout::Bgr8 => frame.to_bgr().map_err(crate::error::at_core)?,
            PixelLayout::Bgra8 => frame.to_bgra().map_err(crate::error::at_core)?,
        };

        let bpp = self.layout.bytes_per_pixel();
        let row_bytes = width as usize * bpp;
        let min_bytes = row_bytes * height as usize;
        let buf = sink.demand(0, height, min_bytes);
        buf[..data.len()].copy_from_slice(&data);

        Ok((width, height))
    }

    /// Decode to raw YCbCr frame (advanced use).
    ///
    /// Returns the internal `DecodedFrame` before color conversion.
    /// Respects limits and cancellation.
    ///
    /// # Errors
    ///
    /// Returns an error if decoding fails, limits are exceeded,
    /// or the operation is cancelled.
    pub fn decode_yuv(self) -> Result<hevc::DecodedFrame> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);
        decode::decode_to_frame(
            self.data,
            self.limits,
            stop,
            self.max_threads,
            self._config.backends(),
            self.apply_orientation,
        )
    }
}

// ---------------------------------------------------------------------------
// no_std float helpers (f64::floor/round require std)
// ---------------------------------------------------------------------------

/// Floor for f64 (truncate toward negative infinity)
#[inline]
fn floor_f64(x: f64) -> f64 {
    let i = x as i64;
    let f = i as f64;
    if f > x { f - 1.0 } else { f }
}

/// Round-half-away-from-zero for f64
#[inline]
fn round_f64(x: f64) -> f64 {
    if x >= 0.0 {
        floor_f64(x + 0.5)
    } else {
        -floor_f64(-x + 0.5)
    }
}

#[cfg(test)]
mod limits_default_tests {
    use super::*;

    /// `Limits::default()` carries the safe fallback (16_384² / 256 MP / 1 GiB),
    /// identical to `server_defaults()` — NOT the old all-`None` (unlimited)
    /// value. This is the footgun fix: a default `Limits` is never weaker than
    /// passing no limits at all.
    #[test]
    fn default_is_safe_fallback_not_none() {
        let d = Limits::default();
        assert_eq!(d.max_width, Some(16_384));
        assert_eq!(d.max_height, Some(16_384));
        assert_eq!(d.max_pixels, Some(268_435_456)); // 256 MP
        assert_eq!(d.max_memory_bytes, Some(1_073_741_824)); // 1 GiB

        let s = Limits::server_defaults();
        assert_eq!(d.max_width, s.max_width);
        assert_eq!(d.max_height, s.max_height);
        assert_eq!(d.max_pixels, s.max_pixels);
        assert_eq!(d.max_memory_bytes, s.max_memory_bytes);
    }

    /// A dimension exceeding the 256-megapixel cap is rejected with
    /// `Limits::default()` (it would have been silently accepted by the old
    /// all-`None` default).
    #[test]
    fn default_rejects_over_256mp_dimension() {
        let d = Limits::default();
        // 20_000 × 20_000 = 400 MP — over both the 16_384 side cap and the
        // 256 MP pixel cap. Either way, default() must reject it.
        let r = d.check_dimensions(20_000, 20_000);
        assert!(
            matches!(r, Err(ref e) if matches!(e.error(), HeicError::LimitExceeded(_))),
            "expected LimitExceeded for a >256MP dimension under default limits, got {r:?}"
        );

        // And a memory estimate above the 1 GiB cap is rejected too.
        let mem = d.check_memory(2 * 1024 * 1024 * 1024); // 2 GiB
        assert!(
            matches!(mem, Err(ref e) if matches!(e.error(), HeicError::LimitExceeded(_))),
            "expected LimitExceeded for a >1GiB estimate under default limits, got {mem:?}"
        );

        // A dimension within the caps is accepted.
        assert!(d.check_dimensions(1280, 854).is_ok());
    }

    /// Contrast: an explicitly all-`None` `Limits` imposes no cap, so the same
    /// oversized inputs pass. This documents *why* the default was changed —
    /// the unbounded behavior is still reachable, just no longer the default.
    #[test]
    fn explicit_all_none_is_unbounded() {
        // All four caps explicitly cleared — the unbounded value the docs say a
        // caller sets to opt out of every limit. (Built as a struct literal
        // rather than mutating `Limits::default()`, which carries the safe
        // fallback, not all-`None`.)
        let none = Limits {
            max_width: None,
            max_height: None,
            max_pixels: None,
            max_memory_bytes: None,
            alloc_pref: crate::alloc_util::AllocPreference::CodecDefault,
        };
        assert!(none.check_dimensions(20_000, 20_000).is_ok());
        assert!(none.check_memory(8 * 1024 * 1024 * 1024).is_ok());
    }
}
