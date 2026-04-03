use crate::{At, DecodeOutput, DecoderConfig, HeicError, ImageInfo, PixelLayout, ProbeError};
use ::image::error::{DecodingError, ImageFormatHint, ParameterError, ParameterErrorKind};
use ::image::{ColorType, ImageDecoder, ImageError, ImageResult};
use image::hooks::GenericReader;
use std::io::Read;

/// Register HEIC/HEIF decoding hooks for the `image` crate.
pub fn register_decoding_hook() -> bool {
    let dec_heic = image::hooks::register_decoding_hook(
        "heic".into(),
        Box::new(|r| Ok(Box::new(HeicImageDecoder::new(r)?))),
    );

    let dec_heif = image::hooks::register_decoding_hook(
        "heif".into(),
        Box::new(|r| Ok(Box::new(HeicImageDecoder::new(r)?))),
    );

    let magic = b"\0\0\0\0ftyp";
    let mask = Some(&b"\0\0\0\0\xff\xff\xff\xff"[..]);

    image::hooks::register_format_detection_hook("heic".into(), magic, mask);
    image::hooks::register_format_detection_hook("heif".into(), magic, mask);

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
