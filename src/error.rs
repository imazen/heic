//! Error types for HEIC decoding

use alloc::string::String;
use core::fmt;

/// Result type for HEIC operations
pub type Result<T> = core::result::Result<T, HeicError>;

/// Errors that can occur during HEIC decoding
#[derive(Debug)]
#[non_exhaustive]
pub enum HeicError {
    /// Invalid HEIF container structure
    InvalidContainer(&'static str),
    /// Invalid or corrupt data
    InvalidData(&'static str),
    /// Unsupported feature
    Unsupported(&'static str),
    /// No primary image found in container
    NoPrimaryImage,
    /// HEVC decoding error
    HevcDecode(HevcError),
    /// Buffer too small
    BufferTooSmall {
        /// Required size
        required: usize,
        /// Actual size
        actual: usize,
    },
}

impl fmt::Display for HeicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidContainer(msg) => write!(f, "invalid HEIF container: {msg}"),
            Self::InvalidData(msg) => write!(f, "invalid data: {msg}"),
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Self::NoPrimaryImage => write!(f, "no primary image in container"),
            Self::HevcDecode(e) => write!(f, "HEVC decode error: {e}"),
            Self::BufferTooSmall { required, actual } => {
                write!(f, "buffer too small: need {required}, got {actual}")
            }
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for HeicError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::HevcDecode(e) => Some(e),
            _ => None,
        }
    }
}

impl From<HevcError> for HeicError {
    fn from(e: HevcError) -> Self {
        Self::HevcDecode(e)
    }
}

/// Errors specific to HEVC decoding
#[derive(Debug)]
#[non_exhaustive]
pub enum HevcError {
    /// Invalid NAL unit
    InvalidNalUnit(&'static str),
    /// Invalid bitstream
    InvalidBitstream(&'static str),
    /// Missing required parameter set
    MissingParameterSet(&'static str),
    /// Invalid parameter set
    InvalidParameterSet { kind: &'static str, msg: String },
    /// CABAC decoding error
    CabacError(&'static str),
    /// Unsupported profile/level
    UnsupportedProfile { profile: u8, level: u8 },
    /// Unsupported feature
    Unsupported(&'static str),
    /// Decoding error
    DecodingError(&'static str),
}

impl fmt::Display for HevcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidNalUnit(msg) => write!(f, "invalid NAL unit: {msg}"),
            Self::InvalidBitstream(msg) => write!(f, "invalid bitstream: {msg}"),
            Self::MissingParameterSet(kind) => write!(f, "missing {kind}"),
            Self::InvalidParameterSet { kind, msg } => {
                write!(f, "invalid {kind}: {msg}")
            }
            Self::CabacError(msg) => write!(f, "CABAC error: {msg}"),
            Self::UnsupportedProfile { profile, level } => {
                write!(f, "unsupported profile {profile} level {level}")
            }
            Self::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            Self::DecodingError(msg) => write!(f, "decoding error: {msg}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for HevcError {}
