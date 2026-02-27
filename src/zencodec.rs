//! zencodec-types trait implementations for heic-decoder.
//!
//! Provides [`HeicDecoderConfig`] that implements the 4-layer decode trait
//! hierarchy from zencodec-types, wrapping the native heic-decoder API.
//!
//! # Trait mapping
//!
//! | zencodec-types | heic-decoder adapter |
//! |----------------|----------------------|
//! | `DecoderConfig` | [`HeicDecoderConfig`] |
//! | `DecodeJob<'a>` | [`HeicDecodeJob`] |
//! | `Decode` | [`HeicDecoder`] |
//! | `FrameDecode` | [`HeicFrameDecoder`] (stub, HEIC has no animation) |
//!
//! # Examples
//!
//! ```rust,ignore
//! use zencodec_types::{Decode, DecodeJob, DecoderConfig};
//! use heic_decoder::HeicDecoderConfig;
//!
//! let config = HeicDecoderConfig::new();
//! let output = config.decode(&heic_bytes)?;
//! println!("{}x{}", output.width(), output.height());
//! ```

use rgb::{Rgb, Rgba};
use zencodec_types::{
    ChannelType, DecodeFrame, DecodeOutput, ImageFormat, ImageInfo, PixelData, PixelDescriptor,
    ResourceLimits, Stop,
};

use crate::error::HeicError;

// ── Supported descriptors ──────────────────────────────────────────────────

static DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::BGRA8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
];

// ── Decoder Config ─────────────────────────────────────────────────────────

/// HEIC decoder configuration implementing [`zencodec_types::DecoderConfig`].
///
/// Wraps [`crate::DecoderConfig`] for use with the zencodec-types trait system.
/// HEIC decoding has no tunable parameters, so this is a thin wrapper.
#[derive(Clone, Debug)]
pub struct HeicDecoderConfig {
    inner: crate::DecoderConfig,
}

impl HeicDecoderConfig {
    /// Create a default HEIC decoder config.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: crate::DecoderConfig::new(),
        }
    }

    /// Access the underlying [`crate::DecoderConfig`].
    #[must_use]
    pub fn inner(&self) -> &crate::DecoderConfig {
        &self.inner
    }

    /// Convenience: decode image with this config.
    pub fn decode(&self, data: &[u8]) -> Result<DecodeOutput, HeicError> {
        use zencodec_types::{Decode as _, DecodeJob as _, DecoderConfig as _};
        self.job().decoder()?.decode(data, &[])
    }

    /// Convenience: probe image header with this config.
    pub fn probe_header(&self, data: &[u8]) -> Result<ImageInfo, HeicError> {
        use zencodec_types::{DecodeJob as _, DecoderConfig as _};
        self.job().probe(data)
    }
}

impl Default for HeicDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl zencodec_types::DecoderConfig for HeicDecoderConfig {
    type Error = HeicError;
    type Job<'a> = HeicDecodeJob<'a>;

    fn format() -> ImageFormat {
        ImageFormat::Heic
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn job(&self) -> HeicDecodeJob<'_> {
        HeicDecodeJob {
            config: self,
            stop: None,
            limits: ResourceLimits::none(),
        }
    }
}

// ── Decode Job ─────────────────────────────────────────────────────────────

/// Per-operation HEIC decode job.
pub struct HeicDecodeJob<'a> {
    config: &'a HeicDecoderConfig,
    stop: Option<&'a dyn Stop>,
    limits: ResourceLimits,
}

impl<'a> HeicDecodeJob<'a> {
    /// Build native limits from zencodec-types ResourceLimits.
    fn native_limits(&self) -> Option<crate::Limits> {
        if !self.limits.has_any() {
            return None;
        }
        let mut limits = crate::Limits::default();
        limits.max_width = self.limits.max_width.map(u64::from);
        limits.max_height = self.limits.max_height.map(u64::from);
        limits.max_pixels = self.limits.max_pixels;
        limits.max_memory_bytes = self.limits.max_memory_bytes;
        Some(limits)
    }
}

impl<'a> zencodec_types::DecodeJob<'a> for HeicDecodeJob<'a> {
    type Error = HeicError;
    type Dec = HeicDecoder<'a>;
    type FrameDec = HeicFrameDecoder;

    fn with_stop(mut self, stop: &'a dyn Stop) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, HeicError> {
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        Ok(convert_info(&native))
    }

    fn output_info(&self, data: &[u8]) -> Result<zencodec_types::OutputInfo, HeicError> {
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        let desc = if native.bit_depth > 8 {
            if native.has_alpha {
                PixelDescriptor::RGBA16_SRGB
            } else {
                PixelDescriptor::RGB16_SRGB
            }
        } else if native.has_alpha {
            PixelDescriptor::RGBA8_SRGB
        } else {
            PixelDescriptor::RGB8_SRGB
        };
        Ok(zencodec_types::OutputInfo::full_decode(
            native.width,
            native.height,
            desc,
        ))
    }

    fn decoder(self) -> Result<HeicDecoder<'a>, HeicError> {
        Ok(HeicDecoder {
            config: self.config,
            stop: self.stop,
            limits: self.native_limits(),
        })
    }

    fn frame_decoder(self, _data: &'a [u8]) -> Result<HeicFrameDecoder, HeicError> {
        Err(HeicError::Unsupported(
            "HEIC does not support animation decoding",
        ))
    }
}

// ── Decoder ────────────────────────────────────────────────────────────────

/// Single-image HEIC decoder.
pub struct HeicDecoder<'a> {
    config: &'a HeicDecoderConfig,
    stop: Option<&'a dyn Stop>,
    limits: Option<crate::Limits>,
}

impl zencodec_types::Decode for HeicDecoder<'_> {
    type Error = HeicError;

    fn decode(self, data: &[u8], preferred: &[PixelDescriptor]) -> Result<DecodeOutput, HeicError> {
        // Probe for image info (bit depth, alpha) — best-effort.
        let probe_info = crate::ImageInfo::from_bytes(data).ok();
        let bit_depth = probe_info.as_ref().map_or(8, |pi| pi.bit_depth);

        // Choose output format: 16-bit if source is >8-bit and caller wants it
        // (or caller has no preference and source is >8-bit).
        let use_16bit = should_use_16bit(preferred, bit_depth);

        let pixels = if use_16bit {
            // 16-bit path: decode to YCbCr frame, then convert at full precision.
            let mut req = self.config.inner.decode_request(data);
            if let Some(ref limits) = self.limits {
                req = req.with_limits(limits);
            }
            if let Some(stop) = self.stop {
                req = req.with_stop(stop);
            }
            let frame = req.decode_yuv().map_err(|e| e.into_inner())?;

            let has_alpha = frame.alpha_plane.is_some();
            let w = frame.cropped_width() as usize;
            let h = frame.cropped_height() as usize;

            if has_alpha || wants_alpha_16(preferred) {
                let rgba_data = frame.to_rgba16();
                let pixels: alloc::vec::Vec<Rgba<u16>> = rgba_data
                    .chunks_exact(4)
                    .map(|c| Rgba {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    })
                    .collect();
                (PixelData::Rgba16(imgref::ImgVec::new(pixels, w, h)), w as u32, h as u32, true)
            } else {
                let rgb_data = frame.to_rgb16();
                let pixels: alloc::vec::Vec<Rgb<u16>> = rgb_data
                    .chunks_exact(3)
                    .map(|c| Rgb {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                    })
                    .collect();
                (PixelData::Rgb16(imgref::ImgVec::new(pixels, w, h)), w as u32, h as u32, false)
            }
        } else {
            // 8-bit path: use the existing layout-based decode.
            let layout = choose_layout(preferred, data);
            let mut req = self
                .config
                .inner
                .decode_request(data)
                .with_output_layout(layout);
            if let Some(ref limits) = self.limits {
                req = req.with_limits(limits);
            }
            if let Some(stop) = self.stop {
                req = req.with_stop(stop);
            }
            let native_output = req.decode().map_err(|e| e.into_inner())?;
            let has_alpha = layout == crate::PixelLayout::Rgba8
                || layout == crate::PixelLayout::Bgra8;
            let w = native_output.width;
            let h = native_output.height;
            let pd = raw_to_pixel_data(
                native_output.data,
                w as usize,
                h as usize,
                native_output.layout,
            );
            (pd, w, h, has_alpha)
        };

        let (pixel_data, width, height, has_alpha) = pixels;

        // Probe for metadata (EXIF/XMP) — best-effort, don't fail the decode.
        let exif = self
            .config
            .inner
            .extract_exif(data)
            .ok()
            .flatten()
            .map(|c| c.into_owned());
        let xmp = self
            .config
            .inner
            .extract_xmp(data)
            .ok()
            .flatten()
            .map(|c| c.into_owned());

        let mut info = ImageInfo::new(width, height, ImageFormat::Heic);

        if let Some(pi) = &probe_info {
            info = info.with_alpha(pi.has_alpha).with_bit_depth(pi.bit_depth);
        } else {
            info = info.with_alpha(has_alpha);
        }

        if let Some(exif_data) = exif {
            info = info.with_exif(exif_data);
        }
        if let Some(xmp_data) = xmp {
            info = info.with_xmp(xmp_data);
        }

        Ok(DecodeOutput::new(pixel_data, info))
    }
}

// ── Frame Decoder (stub) ───────────────────────────────────────────────────

/// Stub frame decoder for HEIC (animation not supported).
pub struct HeicFrameDecoder;

impl zencodec_types::FrameDecode for HeicFrameDecoder {
    type Error = HeicError;

    fn next_frame(
        &mut self,
        _preferred: &[PixelDescriptor],
    ) -> Result<Option<DecodeFrame>, HeicError> {
        Err(HeicError::Unsupported(
            "HEIC does not support animation decoding",
        ))
    }
}

// ── Pixel conversion helpers ───────────────────────────────────────────────

/// Convert raw `Vec<u8>` pixel data from the native decoder into `PixelData`.
fn raw_to_pixel_data(
    raw: alloc::vec::Vec<u8>,
    w: usize,
    h: usize,
    layout: crate::PixelLayout,
) -> PixelData {
    match layout {
        crate::PixelLayout::Rgb8 => {
            let pixels: alloc::vec::Vec<Rgb<u8>> = raw
                .chunks_exact(3)
                .map(|c| Rgb {
                    r: c[0],
                    g: c[1],
                    b: c[2],
                })
                .collect();
            PixelData::Rgb8(imgref::ImgVec::new(pixels, w, h))
        }
        crate::PixelLayout::Rgba8 => {
            let pixels: alloc::vec::Vec<Rgba<u8>> = raw
                .chunks_exact(4)
                .map(|c| Rgba {
                    r: c[0],
                    g: c[1],
                    b: c[2],
                    a: c[3],
                })
                .collect();
            PixelData::Rgba8(imgref::ImgVec::new(pixels, w, h))
        }
        crate::PixelLayout::Bgr8 => {
            // Convert BGR to RGB.
            let pixels: alloc::vec::Vec<Rgb<u8>> = raw
                .chunks_exact(3)
                .map(|c| Rgb {
                    r: c[2],
                    g: c[1],
                    b: c[0],
                })
                .collect();
            PixelData::Rgb8(imgref::ImgVec::new(pixels, w, h))
        }
        crate::PixelLayout::Bgra8 => {
            // Keep as BGRA — PixelData has a Bgra8 variant.
            let pixels: alloc::vec::Vec<rgb::alt::BGRA<u8>> = raw
                .chunks_exact(4)
                .map(|c| rgb::alt::BGRA {
                    b: c[0],
                    g: c[1],
                    r: c[2],
                    a: c[3],
                })
                .collect();
            PixelData::Bgra8(imgref::ImgVec::new(pixels, w, h))
        }
    }
}

/// Decide whether to use the 16-bit output path.
///
/// Returns true if:
/// - `preferred` is empty and `bit_depth > 8` (native format is lossless)
/// - `preferred` contains a 16-bit descriptor before any 8-bit descriptor
fn should_use_16bit(preferred: &[PixelDescriptor], bit_depth: u8) -> bool {
    if preferred.is_empty() {
        return bit_depth > 8;
    }
    // Find the first descriptor we can produce
    for desc in preferred {
        match desc.channel_type {
            ChannelType::U16 => {
                // Caller's top preference is 16-bit — honor it
                if matches!(
                    *desc,
                    d if d == PixelDescriptor::RGB16_SRGB || d == PixelDescriptor::RGBA16_SRGB
                ) {
                    return true;
                }
            }
            ChannelType::U8 => {
                // Caller prefers 8-bit — use 8-bit path
                if matches!(
                    *desc,
                    d if d == PixelDescriptor::RGB8_SRGB
                        || d == PixelDescriptor::RGBA8_SRGB
                        || d == PixelDescriptor::BGRA8_SRGB
                ) {
                    return false;
                }
            }
            _ => continue,
        }
    }
    // No matching preference found — fall back to native
    bit_depth > 8
}

/// Check if the preferred list contains a 16-bit RGBA descriptor.
fn wants_alpha_16(preferred: &[PixelDescriptor]) -> bool {
    preferred.contains(&PixelDescriptor::RGBA16_SRGB)
}

/// Choose the best native pixel layout based on caller's preferred descriptors.
///
/// If the caller has no preference (empty slice), we auto-detect: use RGBA8 if
/// the image has alpha, RGB8 otherwise. When preferences are given, we pick the
/// first one we can produce without lossy conversion.
fn choose_layout(preferred: &[PixelDescriptor], data: &[u8]) -> crate::PixelLayout {
    for desc in preferred {
        if *desc == PixelDescriptor::RGBA8_SRGB {
            return crate::PixelLayout::Rgba8;
        }
        if *desc == PixelDescriptor::RGB8_SRGB {
            return crate::PixelLayout::Rgb8;
        }
        if *desc == PixelDescriptor::BGRA8_SRGB {
            return crate::PixelLayout::Bgra8;
        }
    }

    // No matching preference — auto-detect based on alpha.
    let has_alpha = crate::ImageInfo::from_bytes(data)
        .map(|info| info.has_alpha)
        .unwrap_or(false);

    if has_alpha {
        crate::PixelLayout::Rgba8
    } else {
        crate::PixelLayout::Rgb8
    }
}

// ── Native → trait metadata conversion ─────────────────────────────────────

/// Convert `crate::ImageInfo` to `zencodec_types::ImageInfo`.
fn convert_info(native: &crate::ImageInfo) -> ImageInfo {
    let channels: u8 = if native.has_alpha { 4 } else { 3 };

    ImageInfo::new(native.width, native.height, ImageFormat::Heic)
        .with_alpha(native.has_alpha)
        .with_bit_depth(native.bit_depth)
        .with_channel_count(channels)
}

/// Convert `ProbeError` to `HeicError` for trait compatibility.
fn probe_error_to_heic(e: crate::ProbeError) -> HeicError {
    match e {
        crate::ProbeError::NeedMoreData => HeicError::InvalidData("not enough data to probe"),
        crate::ProbeError::InvalidFormat => HeicError::InvalidData("not a valid HEIC/HEIF file"),
        crate::ProbeError::Corrupt(inner) => inner,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_creation() {
        let config = HeicDecoderConfig::new();
        assert_eq!(
            <HeicDecoderConfig as zencodec_types::DecoderConfig>::format(),
            ImageFormat::Heic
        );
        let descriptors =
            <HeicDecoderConfig as zencodec_types::DecoderConfig>::supported_descriptors();
        assert!(!descriptors.is_empty());
        assert!(descriptors.contains(&PixelDescriptor::RGB8_SRGB));
        assert!(descriptors.contains(&PixelDescriptor::RGBA8_SRGB));
        assert!(descriptors.contains(&PixelDescriptor::BGRA8_SRGB));
        let _ = config; // use it
    }

    #[test]
    fn default_config() {
        let config = HeicDecoderConfig::default();
        assert_eq!(
            <HeicDecoderConfig as zencodec_types::DecoderConfig>::format(),
            ImageFormat::Heic
        );
        let _ = config;
    }

    #[test]
    fn job_creation() {
        use zencodec_types::DecoderConfig as _;
        let config = HeicDecoderConfig::new();
        let _job = config.job();
    }

    #[test]
    fn frame_decoder_returns_unsupported() {
        use zencodec_types::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config.job().frame_decoder(&[]);
        assert!(result.is_err());
    }

    #[test]
    fn probe_invalid_data() {
        use zencodec_types::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config.job().probe(b"not a heic file");
        assert!(result.is_err());
    }

    #[test]
    fn choose_layout_empty_preference_no_alpha() {
        // With no preference and no parseable alpha, default to RGB8.
        let layout = choose_layout(&[], b"garbage data");
        assert_eq!(layout, crate::PixelLayout::Rgb8);
    }

    #[test]
    fn choose_layout_rgba_preference() {
        let layout = choose_layout(&[PixelDescriptor::RGBA8_SRGB], b"");
        assert_eq!(layout, crate::PixelLayout::Rgba8);
    }

    #[test]
    fn choose_layout_bgra_preference() {
        let layout = choose_layout(&[PixelDescriptor::BGRA8_SRGB], b"");
        assert_eq!(layout, crate::PixelLayout::Bgra8);
    }

    #[test]
    fn choose_layout_rgb_preference() {
        let layout = choose_layout(&[PixelDescriptor::RGB8_SRGB], b"");
        assert_eq!(layout, crate::PixelLayout::Rgb8);
    }

    #[test]
    fn raw_to_pixel_data_rgb8() {
        let raw = alloc::vec![10, 20, 30, 40, 50, 60];
        let pixels = raw_to_pixel_data(raw, 2, 1, crate::PixelLayout::Rgb8);
        match pixels {
            PixelData::Rgb8(img) => {
                assert_eq!(img.width(), 2);
                assert_eq!(img.height(), 1);
                assert_eq!(
                    img.buf()[0],
                    Rgb {
                        r: 10,
                        g: 20,
                        b: 30
                    }
                );
                assert_eq!(
                    img.buf()[1],
                    Rgb {
                        r: 40,
                        g: 50,
                        b: 60
                    }
                );
            }
            _ => panic!("expected Rgb8"),
        }
    }

    #[test]
    fn raw_to_pixel_data_rgba8() {
        let raw = alloc::vec![10, 20, 30, 255, 40, 50, 60, 128];
        let pixels = raw_to_pixel_data(raw, 2, 1, crate::PixelLayout::Rgba8);
        match pixels {
            PixelData::Rgba8(img) => {
                assert_eq!(img.width(), 2);
                assert_eq!(img.height(), 1);
                assert_eq!(
                    img.buf()[0],
                    Rgba {
                        r: 10,
                        g: 20,
                        b: 30,
                        a: 255
                    }
                );
            }
            _ => panic!("expected Rgba8"),
        }
    }

    #[test]
    fn raw_to_pixel_data_bgr8() {
        // BGR input should be converted to RGB.
        let raw = alloc::vec![30, 20, 10];
        let pixels = raw_to_pixel_data(raw, 1, 1, crate::PixelLayout::Bgr8);
        match pixels {
            PixelData::Rgb8(img) => {
                assert_eq!(
                    img.buf()[0],
                    Rgb {
                        r: 10,
                        g: 20,
                        b: 30
                    }
                );
            }
            _ => panic!("expected Rgb8 from BGR conversion"),
        }
    }

    #[test]
    fn raw_to_pixel_data_bgra8() {
        let raw = alloc::vec![30, 20, 10, 255];
        let pixels = raw_to_pixel_data(raw, 1, 1, crate::PixelLayout::Bgra8);
        match pixels {
            PixelData::Bgra8(img) => {
                let px = &img.buf()[0];
                assert_eq!(px.b, 30);
                assert_eq!(px.g, 20);
                assert_eq!(px.r, 10);
                assert_eq!(px.a, 255);
            }
            _ => panic!("expected Bgra8"),
        }
    }

    #[test]
    fn probe_error_conversion() {
        let e = probe_error_to_heic(crate::ProbeError::NeedMoreData);
        assert!(matches!(e, HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::InvalidFormat);
        assert!(matches!(e, HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::Corrupt(HeicError::NoPrimaryImage));
        assert!(matches!(e, HeicError::NoPrimaryImage));
    }
}
