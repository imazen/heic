//! Integration with the [`image`](https://crates.io/crates/image) crate's
//! plugin/hook system.
//!
//! Call [`register_decoding_hook`] once at startup to enable HEIC/HEIF decoding
//! via `image::open`, `image::load_from_memory`, and `ImageReader`.
//!
//! ```no_run
//! heic::register_decoding_hook();
//!
//! // Decode by path (extension-based, works for all HEIC/HEIF files)
//! let img = image::open("photo.heic").unwrap();
//!
//! // Decode from memory (magic-byte detection, see limitations below)
//! let data = std::fs::read("photo.heic").unwrap();
//! let img = image::load_from_memory(&data).unwrap();
//! ```
//!
//! # Format detection
//!
//! HEIC/HEIF files use the ISOBMFF container, which starts with:
//!
//! | Offset | Length | Field | Example |
//! |--------|--------|-------|---------|
//! | 0 | 4 | box size | `00 00 00 28` |
//! | 4 | 4 | box type | `ftyp` |
//! | 8 | 4 | major brand | `heic` |
//! | 12 | 4 | minor version | `00 00 00 00` |
//! | 16.. | 4 each | compatible brands | `mif1`, `hevc`, ... |
//!
//! The `image` crate's hook system does static byte-pattern matching against
//! the first 16 bytes of the file, with an optional mask. We register one
//! detection pattern per unambiguous HEVC brand at bytes 8-11:
//!
//! - `heic` -- HEVC image, the most common (iPhones, Samsung, etc.)
//! - `heix` -- HEVC image with extensions
//! - `hevc` -- HEVC image (uncommon variant)
//! - `hevx` -- HEVC image with extensions (uncommon variant)
//! - `heim` -- HEVC image sequence main
//! - `heis` -- HEVC image sequence subset
//! - `hevm` -- HEVC video sequence main
//! - `hevs` -- HEVC video sequence subset
//!
//! Bytes 0-3 (box size) are masked out since they vary per file.
//!
//! # The `mif1` gap
//!
//! Some tools (notably libheif) write `mif1` ("Multi-Image Application Format")
//! as the major brand, with codec-specific brands like `heic` only in the
//! *compatible brands* list starting at byte 16. AVIF files can also use `mif1`.
//!
//! The `image` crate reads only 16 bytes for detection, so compatible brands
//! are invisible. We cannot distinguish `mif1`-branded HEIC from `mif1`-branded
//! AVIF (or any other HEIF-based format) using static patterns alone.
//!
//! Our zencodec detection solves this by reading up to 512 bytes and scanning
//! the compatible brands list, but the `image` crate hook API doesn't support
//! callback-based detection.
//!
//! **What this means in practice:**
//!
//! | Scenario | Works? |
//! |----------|--------|
//! | `image::open("photo.heic")` | Yes -- extension triggers decoding hook |
//! | `load_from_memory` with `heic` major brand | Yes -- magic detection matches |
//! | `load_from_memory` with `mif1` major brand | No -- use `ImageReader::with_format` |
//!
//! The `image` crate's own built-in AVIF detection has the same limitation: it
//! only matches `avif` as major brand, not `mif1` with AVIF in compatible
//! brands. This is a fundamental constraint of the 16-byte detection window.
//!
//! # Coexistence with AVIF
//!
//! The `image` crate's format detection pipeline runs hook detection first,
//! then falls through to built-in detection. Our hooks only match HEVC-specific
//! brands, so AVIF files (`avif`/`avis` major brand) fall through to the
//! built-in AVIF detector. There is no conflict.

// TODO: the current implementation decodes eagerly in the constructor.
// The image crate expects:  new() → set_limits() → read_image()
// We should defer decoding to read_image() and implement set_limits()
// to forward limits to heic::Limits. This would also let read_image()
// use decode_into() for zero-copy output.

use crate::{At, DecodeOutput, DecoderConfig, HeicError, ImageInfo, PixelLayout, ProbeError};
use ::image::error::{DecodingError, ImageFormatHint, ParameterError, ParameterErrorKind};
use ::image::{ColorType, ImageDecoder, ImageError, ImageResult};
use image::hooks::GenericReader;
use std::io::Read;

/// Register HEIC/HEIF decoding hooks for the `image` crate.
///
/// Registers:
/// - **Decoding hooks** for the `heic` and `heif` file extensions, so
///   `image::open("photo.heic")` and `image::open("photo.heif")` work.
/// - **Format detection hooks** for each HEVC brand, so `load_from_memory`
///   works for files with an unambiguous major brand. See [module docs](self)
///   for the `mif1` limitation.
///
/// Returns `true` if both decoding hooks were registered (they may fail
/// if another crate already registered for the same extension).
pub fn register_decoding_hook() -> bool {
    // Decoding hooks: one per file extension.
    let dec_heic = image::hooks::register_decoding_hook(
        "heic".into(),
        Box::new(|r| Ok(Box::new(HeicImageDecoder::new(r)?))),
    );

    let dec_heif = image::hooks::register_decoding_hook(
        "heif".into(),
        Box::new(|r| Ok(Box::new(HeicImageDecoder::new(r)?))),
    );

    // Format detection hooks: one per HEVC brand, all mapping to "heic".
    //
    // Each signature is 12 bytes: [0x00 × 4][ftyp][brand]
    // The mask ignores bytes 0-3 (variable box size) and requires exact
    // match on bytes 4-11 ("ftyp" + brand).
    const MASK: &[u8] = b"\x00\x00\x00\x00\xFF\xFF\xFF\xFF\xFF\xFF\xFF\xFF";

    for sig in [
        b"\x00\x00\x00\x00ftypheic" as &[u8],
        b"\x00\x00\x00\x00ftypheix",
        b"\x00\x00\x00\x00ftyphevc",
        b"\x00\x00\x00\x00ftyphevx",
        b"\x00\x00\x00\x00ftypheim",
        b"\x00\x00\x00\x00ftypheis",
        b"\x00\x00\x00\x00ftyphevm",
        b"\x00\x00\x00\x00ftyphevs",
    ] {
        image::hooks::register_format_detection_hook("heic".into(), sig, Some(MASK));
        image::hooks::register_format_detection_hook("heif".into(), sig, Some(MASK));
    }

    dec_heic && dec_heif
}

fn map_heic_error(err: At<HeicError>) -> ImageError {
    if matches!(err.error(), HeicError::BufferTooSmall { .. }) {
        return ImageError::Parameter(ParameterError::from_kind(
            ParameterErrorKind::DimensionMismatch,
        ));
    }

    if matches!(err.error(), HeicError::Cancelled(_)) {
        return ImageError::IoError(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            err.to_string(),
        ));
    }

    if matches!(err.error(), HeicError::LimitExceeded(_)) {
        return ImageError::IoError(std::io::Error::new(
            std::io::ErrorKind::OutOfMemory,
            err.to_string(),
        ));
    }

    ImageError::Decoding(DecodingError::new(
        ImageFormatHint::Name("HEIC".into()),
        err,
    ))
}

fn map_probe_error(err: ProbeError) -> ImageError {
    match err {
        ProbeError::NeedMoreData => ImageError::IoError(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "not enough data to parse HEIC header",
        )),
        ProbeError::InvalidFormat => ImageError::Decoding(DecodingError::new(
            ImageFormatHint::Name("HEIC".into()),
            "not a valid HEIC/HEIF file",
        )),
        ProbeError::Corrupt(inner) => map_heic_error(inner),
    }
}

struct HeicImageDecoder {
    info: ImageInfo,
    color_type: ColorType,
    out: DecodeOutput,
}

impl HeicImageDecoder {
    /// Create a HEIC decoder from full file bytes.
    pub fn new(mut data: GenericReader) -> ImageResult<Self> {
        let mut bytes = Vec::new();
        data.read_to_end(&mut bytes).map_err(ImageError::IoError)?;

        let info = ImageInfo::from_bytes(&bytes).map_err(map_probe_error)?;

        let (layout, color_type) = if info.has_alpha {
            (PixelLayout::Rgba8, ColorType::Rgba8)
        } else {
            (PixelLayout::Rgb8, ColorType::Rgb8)
        };

        let out = DecoderConfig::new()
            .decode(&bytes, layout)
            .map_err(map_heic_error)?;

        Ok(Self {
            info,
            color_type,
            out,
        })
    }
}

impl ImageDecoder for HeicImageDecoder {
    fn dimensions(&self) -> (u32, u32) {
        (self.info.width, self.info.height)
    }

    fn color_type(&self) -> ColorType {
        self.color_type
    }

    fn icc_profile(&mut self) -> ImageResult<Option<Vec<u8>>> {
        Ok(self.info.icc_profile.clone())
    }

    fn exif_metadata(&mut self) -> ImageResult<Option<Vec<u8>>> {
        Ok(self.info.exif.clone())
    }

    fn xmp_metadata(&mut self) -> ImageResult<Option<Vec<u8>>> {
        Ok(self.info.xmp.clone())
    }

    fn total_bytes(&self) -> u64 {
        self.out.data.len() as u64
    }

    fn read_image(self, buf: &mut [u8]) -> ImageResult<()> {
        if buf.len() != self.out.data.len() {
            return Err(ImageError::Parameter(ParameterError::from_kind(
                ParameterErrorKind::DimensionMismatch,
            )));
        }
        buf.copy_from_slice(&self.out.data);
        Ok(())
    }

    fn read_image_boxed(self: Box<Self>, buf: &mut [u8]) -> ImageResult<()> {
        (*self).read_image(buf)
    }
}
