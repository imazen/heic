//! Pure Rust HEIC/HEIF image decoder
//!
//! This crate decodes HEIC/HEIF images (as used by iPhones and modern cameras)
//! without any C/C++ dependencies. `#![forbid(unsafe_code)]`, `no_std + alloc`
//! compatible, and SIMD-accelerated on x86-64 (AVX2).
//!
//! # Quick Start
//!
//! ```no_run
//! use heic_decoder::{DecoderConfig, PixelLayout};
//!
//! let data = std::fs::read("image.heic").unwrap();
//! let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8).unwrap();
//! println!("Decoded {}x{} image", output.width, output.height);
//! ```
//!
//! # Decode with Limits and Cancellation
//!
//! ```no_run
//! use heic_decoder::{DecoderConfig, PixelLayout, Limits};
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
//! use heic_decoder::{DecoderConfig, ImageInfo, PixelLayout};
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
//! for easier debugging. Use `.into_inner()` to unwrap the location and get the
//! underlying [`HeicError`].
//!
//! For probing, [`ImageInfo::from_bytes`] returns a separate [`ProbeError`] enum
//! that distinguishes "not enough data" from "not a HEIC file" from "corrupt header".
//!
//! # Advanced: Raw YCbCr Access
//!
//! ```no_run
//! use heic_decoder::DecoderConfig;
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

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
extern crate alloc;

mod decode;
mod error;
#[doc(hidden)]
pub mod heif;
#[doc(hidden)]
pub mod hevc;

pub use error::{HeicError, HevcError, ProbeError, Result};
pub use hevc::DecodedFrame;

// Re-export Stop and Unstoppable for ergonomics
pub use enough::{Stop, StopReason, Unstoppable};

// Re-export At for error location tracking
pub use whereat::At;

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
/// All fields default to `None` (no limit). Set limits to prevent
/// resource exhaustion from adversarial or oversized input.
///
/// # Example
///
/// ```
/// use heic_decoder::Limits;
///
/// let mut limits = Limits::default();
/// limits.max_width = Some(8192);
/// limits.max_height = Some(8192);
/// limits.max_pixels = Some(64_000_000);
/// limits.max_memory_bytes = Some(512 * 1024 * 1024);
/// ```
#[derive(Clone, Debug, Default)]
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
}

impl Limits {
    /// Check that dimensions are within limits.
    pub(crate) fn check_dimensions(&self, width: u32, height: u32) -> Result<()> {
        if let Some(max_w) = self.max_width
            && u64::from(width) > max_w
        {
            return Err(HeicError::LimitExceeded("image width exceeds limit").into());
        }
        if let Some(max_h) = self.max_height
            && u64::from(height) > max_h
        {
            return Err(HeicError::LimitExceeded("image height exceeds limit").into());
        }
        if let Some(max_px) = self.max_pixels
            && u64::from(width) * u64::from(height) > max_px
        {
            return Err(HeicError::LimitExceeded("pixel count exceeds limit").into());
        }
        Ok(())
    }

    /// Check that estimated memory usage is within limits.
    pub(crate) fn check_memory(&self, estimated_bytes: u64) -> Result<()> {
        if let Some(max_mem) = self.max_memory_bytes
            && estimated_bytes > max_mem
        {
            return Err(HeicError::LimitExceeded("estimated memory exceeds limit").into());
        }
        Ok(())
    }
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
#[derive(Debug, Clone, Copy)]
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

        let container =
            heif::parse(data, &Unstoppable).map_err(|e: At<HeicError>| ProbeError::Corrupt(e.into_inner()))?;

        let primary_item = container
            .primary_item()
            .ok_or(ProbeError::Corrupt(HeicError::NoPrimaryImage))?;

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

        // Try to get info from HEVC config (fast path for direct HEVC items)
        if let Some(ref config) = primary_item.hevc_config
            && let Ok(hevc_info) = hevc::get_info_from_config(config)
        {
            let bit_depth = config.bit_depth_luma_minus8 + 8;
            let chroma_format = config.chroma_format;
            return Ok(ImageInfo {
                width: hevc_info.width,
                height: hevc_info.height,
                has_alpha,
                bit_depth,
                chroma_format,
                has_exif,
                has_xmp,
                has_thumbnail,
            });
        }

        // For grid/iden/iovl: get dimensions from ispe, bit depth from first tile's hvcC
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
                    && let Some(ref config) = tile.hevc_config
                {
                    bit_depth = config.bit_depth_luma_minus8 + 8;
                    chroma_format = config.chroma_format;
                    break;
                }
            }
            return Ok(ImageInfo {
                width: w,
                height: h,
                has_alpha,
                bit_depth,
                chroma_format,
                has_exif,
                has_xmp,
                has_thumbnail,
            });
        }

        // Fallback to reading image data
        let image_data = container
            .get_item_data(primary_item.id)
            .map_err(|e: At<HeicError>| ProbeError::Corrupt(e.into_inner()))?;

        let hevc_info =
            hevc::get_info(&image_data).map_err(|e| ProbeError::Corrupt(HeicError::from(e)))?;

        Ok(ImageInfo {
            width: hevc_info.width,
            height: hevc_info.height,
            has_alpha,
            bit_depth: 8,
            chroma_format: 1,
            has_exif,
            has_xmp,
            has_thumbnail,
        })
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

/// HDR gain map data extracted from an auxiliary image.
///
/// The gain map can be used with the Apple HDR formula to reconstruct HDR:
/// ```text
/// sdr_linear = sRGB_EOTF(sdr_pixel)
/// gainmap_linear = sRGB_EOTF(gainmap_pixel)
/// scale = 1.0 + (headroom - 1.0) * gainmap_linear
/// hdr_linear = sdr_linear * scale
/// ```
/// Where `headroom` comes from EXIF maker notes (tags 0x0021 and 0x0030).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HdrGainMap {
    /// Gain map pixel data normalized to 0.0-1.0
    pub data: Vec<f32>,
    /// Gain map width in pixels
    pub width: u32,
    /// Gain map height in pixels
    pub height: u32,
}

/// Decoder configuration. Reusable across multiple decode operations.
///
/// For HEIC, the decoder has no required configuration parameters.
/// Use [`new()`](Self::new) for sensible defaults.
///
/// # Example
///
/// ```ignore
/// use heic_decoder::{DecoderConfig, PixelLayout};
///
/// let config = DecoderConfig::new();
/// let output = config.decode(&data, PixelLayout::Rgba8)?;
/// ```
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    _private: (),
}

impl Default for DecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl DecoderConfig {
    /// Create a new decoder configuration with sensible defaults.
    #[must_use]
    pub fn new() -> Self {
        Self { _private: () }
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
        decode::decode_to_frame(data, None, &Unstoppable)
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
        let w = u64::from(width);
        let h = u64::from(height);
        let pixels = w * h;

        // YCbCr planes (u16 per sample)
        let luma_bytes = pixels * 2;
        let chroma_w = w.div_ceil(2);
        let chroma_h = h.div_ceil(2);
        let chroma_bytes = chroma_w * chroma_h * 2 * 2; // Cb + Cr

        // Output pixel buffer
        let output_bytes = pixels * layout.bytes_per_pixel() as u64;

        // Deblocking metadata (flags + QP map at 4x4 granularity)
        let blocks_w = w.div_ceil(4);
        let blocks_h = h.div_ceil(4);
        let deblock_bytes = blocks_w * blocks_h * 2; // flags(u8) + qp(i8)

        luma_bytes + chroma_bytes + output_bytes + deblock_bytes
    }

    /// Decode the HDR gain map from an Apple HDR HEIC file.
    ///
    /// Returns the raw gain map pixel data normalized to 0.0-1.0.
    /// The gain map is typically lower resolution than the primary image.
    ///
    /// # Errors
    ///
    /// Returns an error if the file has no gain map or decoding fails.
    pub fn decode_gain_map(&self, data: &[u8]) -> Result<HdrGainMap> {
        decode::decode_gain_map(data)
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
        decode::decode_thumbnail(data, layout)
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
}

impl<'a> DecodeRequest<'a> {
    /// Set the desired output pixel layout.
    ///
    /// Default is `PixelLayout::Rgba8`.
    pub fn with_output_layout(mut self, layout: PixelLayout) -> Self {
        self.layout = layout;
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

    /// Execute the decode and return pixel data.
    ///
    /// # Errors
    ///
    /// Returns an error if the data is invalid, a limit is exceeded,
    /// or the operation is cancelled.
    pub fn decode(self) -> Result<DecodeOutput> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);
        let frame = decode::decode_to_frame(self.data, self.limits, stop)?;

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
            PixelLayout::Rgb8 => frame.to_rgb(),
            PixelLayout::Rgba8 => frame.to_rgba(),
            PixelLayout::Bgr8 => frame.to_bgr(),
            PixelLayout::Bgra8 => frame.to_bgra(),
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
        if let Some(result) =
            decode::try_decode_grid_streaming(self.data, self.limits, stop, self.layout, output)?
        {
            return Ok(result);
        }

        // Fallback: full-frame decode then color convert
        let frame = decode::decode_to_frame(self.data, self.limits, stop)?;

        let width = frame.cropped_width();
        let height = frame.cropped_height();
        let required = (width as usize)
            .checked_mul(height as usize)
            .and_then(|n| n.checked_mul(self.layout.bytes_per_pixel()))
            .ok_or(HeicError::LimitExceeded(
                "output buffer size overflows usize",
            ))?;

        if output.len() < required {
            return Err(HeicError::BufferTooSmall {
                required,
                actual: output.len(),
            }
            .into());
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
        decode::decode_to_frame(self.data, self.limits, stop)
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
