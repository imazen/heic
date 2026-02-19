//! Pure Rust HEIC/HEIF image decoder
//!
//! This crate provides a safe, sandboxed HEIC image decoder without
//! any C/C++ dependencies.
//!
//! # Quick Start
//!
//! ```ignore
//! use heic_decoder::{DecoderConfig, PixelLayout};
//!
//! let data = std::fs::read("image.heic")?;
//! let output = DecoderConfig::new().decode(&data, PixelLayout::Rgba8)?;
//! println!("Decoded {}x{} image", output.width, output.height);
//! ```
//!
//! # Full Control
//!
//! ```ignore
//! use heic_decoder::{DecoderConfig, DecodeRequest, PixelLayout, Limits};
//! use enough::Unstoppable;
//!
//! let limits = Limits {
//!     max_width: Some(8192),
//!     max_height: Some(8192),
//!     max_pixels: Some(64_000_000),
//!     ..Limits::default()
//! };
//!
//! let output = DecoderConfig::new()
//!     .decode_request(&data)
//!     .with_output_layout(PixelLayout::Rgba8)
//!     .with_limits(&limits)
//!     .decode()?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
// Allow dead code during development - will be removed when decoder is complete
#![allow(dead_code)]

extern crate alloc;

mod error;
pub mod heif;
pub mod hevc;

pub use error::{HeicError, HevcError, ProbeError, Result};

// Re-export Stop and Unstoppable for ergonomics
pub use enough::{Stop, StopReason, Unstoppable};

// Re-export At for error location tracking
pub use whereat::At;

use alloc::vec::Vec;
use error::check_stop;
use heif::{ColorInfo, FourCC, ItemType, Transform};

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
/// let limits = Limits {
///     max_width: Some(8192),
///     max_height: Some(8192),
///     max_pixels: Some(64_000_000),
///     max_memory_bytes: Some(512 * 1024 * 1024),
/// };
/// ```
#[derive(Clone, Debug, Default)]
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
    fn check_dimensions(&self, width: u32, height: u32) -> Result<()> {
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
    fn check_memory(&self, estimated_bytes: u64) -> Result<()> {
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

        let container = heif::parse(data).map_err(|e| ProbeError::Corrupt(e.into_inner()))?;

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
            .ok_or(ProbeError::NeedMoreData)?;

        let hevc_info =
            hevc::get_info(image_data).map_err(|e| ProbeError::Corrupt(HeicError::from(e)))?;

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
    #[must_use]
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
        decode_to_frame_inner(data, None, &Unstoppable)
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
        decode_gain_map_inner(data)
    }

    /// Extract raw EXIF (TIFF) data from a HEIC file.
    ///
    /// Returns the TIFF-header data (starting with byte-order mark `II` or `MM`)
    /// with the HEIF 4-byte offset prefix stripped. Returns `None` if the file
    /// contains no EXIF metadata.
    ///
    /// The returned bytes can be passed to any EXIF parser (e.g., `exif` or `kamadak-exif` crate).
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn extract_exif<'a>(&self, data: &'a [u8]) -> Result<Option<&'a [u8]>> {
        extract_exif_inner(data)
    }

    /// Extract raw XMP (XML) data from a HEIC file.
    ///
    /// Returns the raw XML bytes of the XMP metadata. Returns `None` if the
    /// file contains no XMP metadata.
    ///
    /// XMP items are stored as `mime` type items with content type
    /// `application/rdf+xml` in the HEIF container.
    ///
    /// # Errors
    ///
    /// Returns an error if the HEIF container is malformed.
    pub fn extract_xmp<'a>(&self, data: &'a [u8]) -> Result<Option<&'a [u8]>> {
        extract_xmp_inner(data)
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
    pub fn decode_thumbnail(&self, data: &[u8], layout: PixelLayout) -> Result<Option<DecodeOutput>> {
        decode_thumbnail_inner(data, layout)
    }
}

/// A decode request binding data, output format, limits, and cancellation.
///
/// Created by [`DecoderConfig::decode_request`]. Use builder methods to
/// configure, then call [`decode`](Self::decode) or
/// [`decode_into`](Self::decode_into).
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
    #[must_use]
    pub fn with_output_layout(mut self, layout: PixelLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Set resource limits for this decode operation.
    ///
    /// Limits are checked before allocations. Exceeding any limit
    /// returns [`HeicError::LimitExceeded`].
    #[must_use]
    pub fn with_limits(mut self, limits: &'a Limits) -> Self {
        self.limits = Some(limits);
        self
    }

    /// Set a cooperative cancellation token.
    ///
    /// The decoder will periodically check this token and return
    /// [`HeicError::Cancelled`] if the operation should stop.
    #[must_use]
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
        let frame = decode_to_frame_inner(self.data, self.limits, stop)?;

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
    /// Returns the image info (width, height, etc.) on success.
    ///
    /// # Errors
    ///
    /// Returns [`HeicError::BufferTooSmall`] if the output buffer is too small,
    /// or other errors if decoding fails.
    pub fn decode_into(self, output: &mut [u8]) -> Result<ImageInfo> {
        let stop: &dyn Stop = self.stop.unwrap_or(&Unstoppable);
        let frame = decode_to_frame_inner(self.data, self.limits, stop)?;

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

        Ok(ImageInfo {
            width,
            height,
            has_alpha: frame.alpha_plane.is_some(),
            bit_depth: frame.bit_depth,
            chroma_format: frame.chroma_format,
            has_exif: false, // Use ImageInfo::from_bytes() for metadata probing
            has_xmp: false,
            has_thumbnail: false,
        })
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
        decode_to_frame_inner(self.data, self.limits, stop)
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

// ---------------------------------------------------------------------------
// Internal decode pipeline
// ---------------------------------------------------------------------------

/// Sentinel for no limits
static NO_LIMITS: Limits = Limits {
    max_width: None,
    max_height: None,
    max_pixels: None,
    max_memory_bytes: None,
};

/// Core decode-to-frame implementation shared by all entry points.
fn decode_to_frame_inner(
    data: &[u8],
    limits: Option<&Limits>,
    stop: &dyn Stop,
) -> Result<hevc::DecodedFrame> {
    let limits = limits.unwrap_or(&NO_LIMITS);

    check_stop(stop)?;

    let container = heif::parse(data)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    // Check limits on primary item dimensions if available from ispe
    if let Some((w, h)) = primary_item.dimensions {
        limits.check_dimensions(w, h)?;
        // Estimate memory before allocating frames
        let estimated = DecoderConfig::estimate_memory(w, h, PixelLayout::Rgba8);
        limits.check_memory(estimated)?;
    }

    check_stop(stop)?;

    let mut frame = decode_item(&container, &primary_item, 0, limits, stop)?;

    check_stop(stop)?;

    // Try to decode alpha plane from auxiliary image.
    let alpha_id = container
        .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
        .first()
        .copied()
        .or_else(|| {
            container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
                )
                .first()
                .copied()
        });
    if let Some(alpha_id) = alpha_id
        && let Some(alpha_plane) = decode_alpha_plane(&container, alpha_id, &frame)
    {
        frame.alpha_plane = Some(alpha_plane);
    }

    Ok(frame)
}

/// Decode an item, handling derived image types (iden, grid, iovl).
/// Applies the item's own transforms (clap, irot, imir) after decoding.
fn decode_item(
    container: &heif::HeifContainer<'_>,
    item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<hevc::DecodedFrame> {
    if depth > 8 {
        return Err(HeicError::InvalidData("Derived image reference chain too deep").into());
    }

    check_stop(stop)?;

    let mut frame = match item.item_type {
        ItemType::Grid => decode_grid(container, item, limits, stop)?,
        ItemType::Iden => decode_iden(container, item, depth, limits, stop)?,
        ItemType::Iovl => decode_iovl(container, item, depth, limits, stop)?,
        _ => {
            let image_data = container
                .get_item_data(item.id)
                .ok_or(HeicError::InvalidData("Missing image data"))?;

            if let Some(ref config) = item.hevc_config {
                hevc::decode_with_config(config, image_data)?
            } else {
                hevc::decode(image_data)?
            }
        }
    };

    // Set color conversion parameters from colr nclx box if present.
    if let Some(ColorInfo::Nclx {
        full_range,
        matrix_coefficients,
        ..
    }) = &item.color_info
    {
        frame.full_range = *full_range;
        frame.matrix_coeffs = *matrix_coefficients as u8;
    }

    // Apply transformative properties in ipma listing order (HEIF spec requirement)
    for transform in &item.transforms {
        match transform {
            Transform::CleanAperture(clap) => {
                apply_clean_aperture(&mut frame, clap);
            }
            Transform::Mirror(mirror) => {
                frame = match mirror.axis {
                    0 => frame.mirror_vertical(),
                    1 => frame.mirror_horizontal(),
                    _ => frame,
                };
            }
            Transform::Rotation(rotation) => {
                frame = match rotation.angle {
                    90 => frame.rotate_90_cw(),
                    180 => frame.rotate_180(),
                    270 => frame.rotate_270_cw(),
                    _ => frame,
                };
            }
        }
    }

    Ok(frame)
}

/// Decode an identity-derived image by following dimg references.
fn decode_iden(
    container: &heif::HeifContainer<'_>,
    iden_item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<hevc::DecodedFrame> {
    let source_ids = container.get_item_references(iden_item.id, FourCC::DIMG);
    let source_id = source_ids
        .first()
        .ok_or(HeicError::InvalidData("iden item has no dimg reference"))?;

    let source_item = container
        .get_item(*source_id)
        .ok_or(HeicError::InvalidData("iden dimg target item not found"))?;

    decode_item(container, &source_item, depth + 1, limits, stop)
}

/// Decode an image overlay (iovl) by compositing referenced tiles onto a canvas.
fn decode_iovl(
    container: &heif::HeifContainer<'_>,
    iovl_item: &heif::Item,
    depth: u32,
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<hevc::DecodedFrame> {
    let iovl_data = container
        .get_item_data(iovl_item.id)
        .ok_or(HeicError::InvalidData("Missing overlay descriptor"))?;

    // Parse iovl descriptor:
    // - version (1 byte) + flags (3 bytes)
    // - canvas_fill_value: 2 bytes * num_channels (flags & 0x01 determines 32-bit offsets)
    if iovl_data.len() < 6 {
        return Err(HeicError::InvalidData("Overlay descriptor too short").into());
    }

    let flags = iovl_data[1];
    let large = (flags & 1) != 0;

    let tile_ids = container.get_item_references(iovl_item.id, FourCC::DIMG);
    if tile_ids.is_empty() {
        return Err(HeicError::InvalidData("Overlay has no tile references").into());
    }

    // Calculate expected layout
    let off_size = if large { 4usize } else { 2 };
    let per_tile = 2 * off_size;
    let fixed_end = 4 + 2 * off_size; // version/flags + width/height
    let tile_data_size = tile_ids.len() * per_tile;
    let fill_bytes = iovl_data
        .len()
        .checked_sub(fixed_end + tile_data_size)
        .ok_or(HeicError::InvalidData(
            "Overlay descriptor too short for tiles",
        ))?;

    // Parse canvas fill values (16-bit per channel)
    let num_fill_channels = fill_bytes / 2;
    let mut fill_values = [0u16; 4];
    for i in 0..num_fill_channels.min(4) {
        fill_values[i] = u16::from_be_bytes([iovl_data[4 + i * 2], iovl_data[4 + i * 2 + 1]]);
    }

    let mut pos = 4 + fill_bytes;

    // Read canvas dimensions
    let (canvas_width, canvas_height) = if large {
        if pos + 8 > iovl_data.len() {
            return Err(HeicError::InvalidData("Overlay descriptor truncated").into());
        }
        let w = u32::from_be_bytes([
            iovl_data[pos],
            iovl_data[pos + 1],
            iovl_data[pos + 2],
            iovl_data[pos + 3],
        ]);
        let h = u32::from_be_bytes([
            iovl_data[pos + 4],
            iovl_data[pos + 5],
            iovl_data[pos + 6],
            iovl_data[pos + 7],
        ]);
        pos += 8;
        (w, h)
    } else {
        if pos + 4 > iovl_data.len() {
            return Err(HeicError::InvalidData("Overlay descriptor truncated").into());
        }
        let w = u16::from_be_bytes([iovl_data[pos], iovl_data[pos + 1]]) as u32;
        let h = u16::from_be_bytes([iovl_data[pos + 2], iovl_data[pos + 3]]) as u32;
        pos += 4;
        (w, h)
    };

    // Check canvas dimensions against limits
    limits.check_dimensions(canvas_width, canvas_height)?;

    // Read per-tile offsets
    let mut offsets = Vec::with_capacity(tile_ids.len());
    for _ in 0..tile_ids.len() {
        let (x, y) = if large {
            if pos + 8 > iovl_data.len() {
                return Err(HeicError::InvalidData("Overlay offset data truncated").into());
            }
            let x = i32::from_be_bytes([
                iovl_data[pos],
                iovl_data[pos + 1],
                iovl_data[pos + 2],
                iovl_data[pos + 3],
            ]);
            let y = i32::from_be_bytes([
                iovl_data[pos + 4],
                iovl_data[pos + 5],
                iovl_data[pos + 6],
                iovl_data[pos + 7],
            ]);
            pos += 8;
            (x, y)
        } else {
            if pos + 4 > iovl_data.len() {
                return Err(HeicError::InvalidData("Overlay offset data truncated").into());
            }
            let x = i16::from_be_bytes([iovl_data[pos], iovl_data[pos + 1]]) as i32;
            let y = i16::from_be_bytes([iovl_data[pos + 2], iovl_data[pos + 3]]) as i32;
            pos += 4;
            (x, y)
        };
        offsets.push((x, y));
    }

    // Decode first tile to get format info
    let first_tile_item = container
        .get_item(tile_ids[0])
        .ok_or(HeicError::InvalidData("Missing overlay tile item"))?;
    let first_tile_config = first_tile_item
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing overlay tile hvcC"))?;

    let bit_depth = first_tile_config.bit_depth_luma_minus8 + 8;
    let chroma_format = first_tile_config.chroma_format;

    let mut output =
        hevc::DecodedFrame::with_params(canvas_width, canvas_height, bit_depth, chroma_format);

    // Apply canvas fill values (16-bit values scaled to bit depth)
    let fill_shift = 16u32.saturating_sub(bit_depth as u32);
    let y_fill = fill_values[0] >> fill_shift;
    let cb_fill = if num_fill_channels > 1 {
        fill_values[1] >> fill_shift
    } else {
        1u16 << (bit_depth - 1) // neutral chroma
    };
    let cr_fill = if num_fill_channels > 2 {
        fill_values[2] >> fill_shift
    } else {
        1u16 << (bit_depth - 1) // neutral chroma
    };
    output.y_plane.fill(y_fill);
    output.cb_plane.fill(cb_fill);
    output.cr_plane.fill(cr_fill);

    // Decode each tile and composite onto the canvas
    for (idx, &tile_id) in tile_ids.iter().enumerate() {
        check_stop(stop)?;

        let tile_item = container
            .get_item(tile_id)
            .ok_or(HeicError::InvalidData("Missing overlay tile"))?;

        let tile_frame = decode_item(container, &tile_item, depth + 1, limits, stop)?;

        // Propagate color conversion settings from first tile
        if idx == 0 {
            output.full_range = tile_frame.full_range;
            output.matrix_coeffs = tile_frame.matrix_coeffs;
        }

        let (off_x, off_y) = offsets[idx];
        let dst_x = off_x.max(0) as u32;
        let dst_y = off_y.max(0) as u32;
        let tile_w = tile_frame.cropped_width();
        let tile_h = tile_frame.cropped_height();

        // Copy luma
        let copy_w = tile_w.min(canvas_width.saturating_sub(dst_x));
        let copy_h = tile_h.min(canvas_height.saturating_sub(dst_y));

        for row in 0..copy_h {
            let src_row = (tile_frame.crop_top + row) as usize;
            let dst_row = (dst_y + row) as usize;
            for col in 0..copy_w {
                let src_col = (tile_frame.crop_left + col) as usize;
                let dst_col = (dst_x + col) as usize;
                let src_idx = src_row * tile_frame.y_stride() + src_col;
                let dst_idx = dst_row * output.y_stride() + dst_col;
                if src_idx < tile_frame.y_plane.len() && dst_idx < output.y_plane.len() {
                    output.y_plane[dst_idx] = tile_frame.y_plane[src_idx];
                }
            }
        }

        // Copy chroma
        if chroma_format > 0 {
            let (sub_x, sub_y) = match chroma_format {
                1 => (2u32, 2u32),
                2 => (2, 1),
                3 => (1, 1),
                _ => (2, 2),
            };
            let c_copy_w = copy_w.div_ceil(sub_x);
            let c_copy_h = copy_h.div_ceil(sub_y);
            let c_dst_x = dst_x / sub_x;
            let c_dst_y = dst_y / sub_y;
            let c_src_x = tile_frame.crop_left / sub_x;
            let c_src_y = tile_frame.crop_top / sub_y;

            let src_c_stride = tile_frame.c_stride();
            let dst_c_stride = output.c_stride();

            for row in 0..c_copy_h {
                let src_row = (c_src_y + row) as usize;
                let dst_row = (c_dst_y + row) as usize;
                for col in 0..c_copy_w {
                    let src_col = (c_src_x + col) as usize;
                    let dst_col = (c_dst_x + col) as usize;
                    let src_idx = src_row * src_c_stride + src_col;
                    let dst_idx = dst_row * dst_c_stride + dst_col;
                    if src_idx < tile_frame.cb_plane.len() && dst_idx < output.cb_plane.len() {
                        output.cb_plane[dst_idx] = tile_frame.cb_plane[src_idx];
                        output.cr_plane[dst_idx] = tile_frame.cr_plane[src_idx];
                    }
                }
            }
        }
    }

    Ok(output)
}

/// Decode a grid-based HEIC image
fn decode_grid(
    container: &heif::HeifContainer<'_>,
    grid_item: &heif::Item,
    limits: &Limits,
    stop: &dyn Stop,
) -> Result<hevc::DecodedFrame> {
    // Parse grid descriptor
    let grid_data = container
        .get_item_data(grid_item.id)
        .ok_or(HeicError::InvalidData("Missing grid descriptor"))?;

    if grid_data.len() < 8 {
        return Err(HeicError::InvalidData("Grid descriptor too short").into());
    }

    let flags = grid_data[1];
    let rows = grid_data[2] as u32 + 1;
    let cols = grid_data[3] as u32 + 1;
    let (output_width, output_height) = if (flags & 1) != 0 {
        if grid_data.len() < 12 {
            return Err(HeicError::InvalidData("Grid descriptor too short for 32-bit dims").into());
        }
        (
            u32::from_be_bytes([grid_data[4], grid_data[5], grid_data[6], grid_data[7]]),
            u32::from_be_bytes([grid_data[8], grid_data[9], grid_data[10], grid_data[11]]),
        )
    } else {
        (
            u16::from_be_bytes([grid_data[4], grid_data[5]]) as u32,
            u16::from_be_bytes([grid_data[6], grid_data[7]]) as u32,
        )
    };

    // Check grid output dimensions against limits
    limits.check_dimensions(output_width, output_height)?;

    // Get tile item IDs from iref
    let tile_ids = container.get_item_references(grid_item.id, FourCC::DIMG);
    let expected_tiles = (rows * cols) as usize;
    if tile_ids.len() != expected_tiles {
        return Err(HeicError::InvalidData("Grid tile count mismatch").into());
    }

    // Get hvcC config from the first tile item
    let first_tile = container
        .get_item(tile_ids[0])
        .ok_or(HeicError::InvalidData("Missing tile item"))?;
    let tile_config = first_tile
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing tile hvcC config"))?;

    // Get tile dimensions from ispe
    let (tile_width, tile_height) = first_tile
        .dimensions
        .ok_or(HeicError::InvalidData("Missing tile dimensions"))?;

    // Create output frame at the grid's output dimensions
    let bit_depth = tile_config.bit_depth_luma_minus8 + 8;
    let chroma_format = tile_config.chroma_format;
    let mut output =
        hevc::DecodedFrame::with_params(output_width, output_height, bit_depth, chroma_format);

    // Decode each tile and copy into the output frame
    for (tile_idx, &tile_id) in tile_ids.iter().enumerate() {
        check_stop(stop)?;

        let tile_row = tile_idx as u32 / cols;
        let tile_col = tile_idx as u32 % cols;

        let tile_data = container
            .get_item_data(tile_id)
            .ok_or(HeicError::InvalidData("Missing tile data"))?;

        // Decode tile
        let tile_frame = hevc::decode_with_config(tile_config, tile_data)?;

        // Propagate color conversion settings from first tile
        if tile_idx == 0 {
            output.full_range = tile_frame.full_range;
            output.matrix_coeffs = tile_frame.matrix_coeffs;
        }

        // Copy tile into output frame
        let dst_x = tile_col * tile_width;
        let dst_y = tile_row * tile_height;

        // Luma: copy visible portion (clamp to output dimensions)
        let copy_w = tile_frame
            .cropped_width()
            .min(output_width.saturating_sub(dst_x));
        let copy_h = tile_frame
            .cropped_height()
            .min(output_height.saturating_sub(dst_y));

        let src_y_start = tile_frame.crop_top;
        let src_x_start = tile_frame.crop_left;

        for row in 0..copy_h {
            let src_row = (src_y_start + row) as usize;
            let dst_row = (dst_y + row) as usize;
            for col in 0..copy_w {
                let src_col = (src_x_start + col) as usize;
                let dst_col = (dst_x + col) as usize;

                let src_idx = src_row * tile_frame.y_stride() + src_col;
                let dst_idx = dst_row * output.y_stride() + dst_col;
                output.y_plane[dst_idx] = tile_frame.y_plane[src_idx];
            }
        }

        // Chroma: copy with subsampling
        if chroma_format > 0 {
            let (sub_x, sub_y) = match chroma_format {
                1 => (2u32, 2u32), // 4:2:0
                2 => (2, 1),       // 4:2:2
                3 => (1, 1),       // 4:4:4
                _ => (2, 2),
            };
            let c_copy_w = copy_w.div_ceil(sub_x);
            let c_copy_h = copy_h.div_ceil(sub_y);
            let c_dst_x = dst_x / sub_x;
            let c_dst_y = dst_y / sub_y;
            let c_src_x = src_x_start / sub_x;
            let c_src_y = src_y_start / sub_y;

            let src_c_stride = tile_frame.c_stride();
            let dst_c_stride = output.c_stride();

            for row in 0..c_copy_h {
                let src_row = (c_src_y + row) as usize;
                let dst_row = (c_dst_y + row) as usize;
                for col in 0..c_copy_w {
                    let src_col = (c_src_x + col) as usize;
                    let dst_col = (c_dst_x + col) as usize;

                    let src_idx = src_row * src_c_stride + src_col;
                    let dst_idx = dst_row * dst_c_stride + dst_col;
                    if src_idx < tile_frame.cb_plane.len() && dst_idx < output.cb_plane.len() {
                        output.cb_plane[dst_idx] = tile_frame.cb_plane[src_idx];
                        output.cr_plane[dst_idx] = tile_frame.cr_plane[src_idx];
                    }
                }
            }
        }
    }

    Ok(output)
}

/// Decode an auxiliary alpha plane and return it sized to match the primary frame.
///
/// Returns the alpha plane as a Vec<u16> with one value per cropped pixel,
/// or None if decoding fails.
fn decode_alpha_plane(
    container: &heif::HeifContainer<'_>,
    alpha_id: u32,
    primary_frame: &hevc::DecodedFrame,
) -> Option<Vec<u16>> {
    let alpha_item = container.get_item(alpha_id)?;
    let alpha_data = container.get_item_data(alpha_id)?;
    let alpha_config = alpha_item.hevc_config.as_ref()?;

    let alpha_frame = hevc::decode_with_config(alpha_config, alpha_data).ok()?;

    let primary_w = primary_frame.cropped_width();
    let primary_h = primary_frame.cropped_height();
    let alpha_w = alpha_frame.cropped_width();
    let alpha_h = alpha_frame.cropped_height();

    let total_pixels = (primary_w * primary_h) as usize;
    let mut alpha_plane = Vec::with_capacity(total_pixels);

    if alpha_w == primary_w && alpha_h == primary_h {
        // Same dimensions — direct copy of Y plane from cropped region
        let y_start = alpha_frame.crop_top;
        let x_start = alpha_frame.crop_left;
        for y in 0..primary_h {
            for x in 0..primary_w {
                let src_idx = ((y_start + y) * alpha_frame.width + (x_start + x)) as usize;
                alpha_plane.push(alpha_frame.y_plane[src_idx]);
            }
        }
    } else {
        // Different dimensions — bilinear resize
        for dy in 0..primary_h {
            for dx in 0..primary_w {
                let sx = (dx as f64) * (alpha_w as f64 - 1.0) / (primary_w as f64 - 1.0).max(1.0);
                let sy = (dy as f64) * (alpha_h as f64 - 1.0) / (primary_h as f64 - 1.0).max(1.0);

                let x0 = floor_f64(sx) as u32;
                let y0 = floor_f64(sy) as u32;
                let x1 = (x0 + 1).min(alpha_w - 1);
                let y1 = (y0 + 1).min(alpha_h - 1);
                let fx = sx - x0 as f64;
                let fy = sy - y0 as f64;

                let stride = alpha_frame.width;
                let off_y = alpha_frame.crop_top;
                let off_x = alpha_frame.crop_left;

                let get = |px: u32, py: u32| -> f64 {
                    let idx = ((off_y + py) * stride + (off_x + px)) as usize;
                    alpha_frame.y_plane.get(idx).copied().unwrap_or(0) as f64
                };

                let v00 = get(x0, y0);
                let v10 = get(x1, y0);
                let v01 = get(x0, y1);
                let v11 = get(x1, y1);

                let val = v00 * (1.0 - fx) * (1.0 - fy)
                    + v10 * fx * (1.0 - fy)
                    + v01 * (1.0 - fx) * fy
                    + v11 * fx * fy;

                alpha_plane.push(round_f64(val) as u16);
            }
        }
    }

    Some(alpha_plane)
}

/// Internal: decode gain map
fn decode_gain_map_inner(data: &[u8]) -> Result<HdrGainMap> {
    let container = heif::parse(data)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    let gainmap_ids =
        container.find_auxiliary_items(primary_item.id, "urn:com:apple:photo:2020:aux:hdrgainmap");

    let &gainmap_id = gainmap_ids
        .first()
        .ok_or(HeicError::InvalidData("No HDR gain map found"))?;

    let gainmap_item = container
        .get_item(gainmap_id)
        .ok_or(HeicError::InvalidData("Missing gain map item"))?;
    let gainmap_data = container
        .get_item_data(gainmap_id)
        .ok_or(HeicError::InvalidData("Missing gain map data"))?;
    let gainmap_config = gainmap_item
        .hevc_config
        .as_ref()
        .ok_or(HeicError::InvalidData("Missing gain map hvcC config"))?;

    let frame = hevc::decode_with_config(gainmap_config, gainmap_data)?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();
    let max_val = ((1u32 << frame.bit_depth) - 1) as f32;

    let mut float_data = Vec::with_capacity((width * height) as usize);
    let y_start = frame.crop_top;
    let x_start = frame.crop_left;

    for y in 0..height {
        for x in 0..width {
            let src_idx = ((y_start + y) * frame.width + (x_start + x)) as usize;
            let raw = frame.y_plane[src_idx] as f32;
            float_data.push(raw / max_val);
        }
    }

    Ok(HdrGainMap {
        data: float_data,
        width,
        height,
    })
}

/// Apply clean aperture (clap box) crop to a decoded frame
fn apply_clean_aperture(frame: &mut hevc::DecodedFrame, clap: &heif::CleanAperture) {
    let conf_width = frame.cropped_width();
    let conf_height = frame.cropped_height();

    let clean_width = if clap.width_d > 0 {
        clap.width_n / clap.width_d
    } else {
        conf_width
    };
    let clean_height = if clap.height_d > 0 {
        clap.height_n / clap.height_d
    } else {
        conf_height
    };

    if clean_width >= conf_width && clean_height >= conf_height {
        return;
    }

    let horiz_off_pixels = if clap.horiz_off_d > 0 {
        (clap.horiz_off_n as f64) / (clap.horiz_off_d as f64)
    } else {
        0.0
    };
    let vert_off_pixels = if clap.vert_off_d > 0 {
        (clap.vert_off_n as f64) / (clap.vert_off_d as f64)
    } else {
        0.0
    };

    let extra_left =
        round_f64((conf_width as f64 - clean_width as f64) / 2.0 + horiz_off_pixels) as u32;
    let extra_top =
        round_f64((conf_height as f64 - clean_height as f64) / 2.0 + vert_off_pixels) as u32;
    let extra_right = conf_width
        .saturating_sub(clean_width)
        .saturating_sub(extra_left);
    let extra_bottom = conf_height
        .saturating_sub(clean_height)
        .saturating_sub(extra_top);

    frame.crop_left += extra_left;
    frame.crop_right += extra_right;
    frame.crop_top += extra_top;
    frame.crop_bottom += extra_bottom;
}

/// Internal: extract EXIF TIFF data from HEIC container
fn extract_exif_inner(data: &[u8]) -> Result<Option<&[u8]>> {
    let container = heif::parse(data)?;

    // Find Exif item(s)
    for info in &container.item_infos {
        if info.item_type == FourCC(*b"Exif")
            && let Some(exif_data) = container.get_item_data(info.item_id)
        {
            // HEIF EXIF format: 4 bytes big-endian offset to TIFF header, then data.
            // The offset is from byte 4 (after the 4-byte offset field itself).
            // Typically 0, meaning TIFF data starts at byte 4.
            if exif_data.len() < 4 {
                continue;
            }
            let tiff_offset =
                u32::from_be_bytes([exif_data[0], exif_data[1], exif_data[2], exif_data[3]])
                    as usize;
            let tiff_start = 4 + tiff_offset;
            if tiff_start < exif_data.len() {
                return Ok(Some(&exif_data[tiff_start..]));
            }
        }
    }

    Ok(None)
}

/// Internal: decode thumbnail image from HEIC container
fn decode_thumbnail_inner(data: &[u8], layout: PixelLayout) -> Result<Option<DecodeOutput>> {
    let container = heif::parse(data)?;
    let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

    let thumb_ids = container.find_thumbnails(primary_item.id);
    let Some(&thumb_id) = thumb_ids.first() else {
        return Ok(None);
    };

    let thumb_item = container
        .get_item(thumb_id)
        .ok_or(HeicError::InvalidData("Thumbnail item not found"))?;

    let stop: &dyn Stop = &Unstoppable;
    let frame = decode_item(&container, &thumb_item, 0, &NO_LIMITS, stop)?;

    let width = frame.cropped_width();
    let height = frame.cropped_height();

    let pixels = match layout {
        PixelLayout::Rgb8 => frame.to_rgb(),
        PixelLayout::Rgba8 => frame.to_rgba(),
        PixelLayout::Bgr8 => frame.to_bgr(),
        PixelLayout::Bgra8 => frame.to_bgra(),
    };

    Ok(Some(DecodeOutput {
        data: pixels,
        width,
        height,
        layout,
    }))
}

/// Internal: extract XMP XML data from HEIC container
fn extract_xmp_inner(data: &[u8]) -> Result<Option<&[u8]>> {
    let container = heif::parse(data)?;

    // Find mime items with XMP content type
    for info in &container.item_infos {
        if info.item_type == FourCC(*b"mime")
            && (info.content_type.contains("xmp")
                || info.content_type.contains("rdf+xml")
                || info.content_type == "application/rdf+xml")
            && let Some(xmp_data) = container.get_item_data(info.item_id)
        {
            return Ok(Some(xmp_data));
        }
    }

    Ok(None)
}
