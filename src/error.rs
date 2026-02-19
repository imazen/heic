//! Error types for HEIC decoding

use alloc::string::String;
use core::fmt;
use enough::StopReason;
use whereat::At;

/// Result type for HEIC operations, with error location tracking.
///
/// Errors carry a trace of where they were created and propagated,
/// accessible via [`At::full_trace()`] or [`At::last_error_trace()`].
pub type Result<T> = core::result::Result<T, At<HeicError>>;

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
    /// Buffer too small for decode_into
    BufferTooSmall {
        /// Required buffer size in bytes
        required: usize,
        /// Actual buffer size provided
        actual: usize,
    },
    /// A resource limit was exceeded (dimensions, pixel count, or memory)
    LimitExceeded(&'static str),
    /// Operation was cancelled via cooperative cancellation
    Cancelled(StopReason),
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
            Self::LimitExceeded(msg) => write!(f, "limit exceeded: {msg}"),
            Self::Cancelled(reason) => write!(f, "{reason}"),
        }
    }
}

impl core::error::Error for HeicError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
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

impl From<StopReason> for HeicError {
    fn from(r: StopReason) -> Self {
        Self::Cancelled(r)
    }
}

// Two-hop conversion for ? operator: HevcError → At<HeicError>
impl From<HevcError> for At<HeicError> {
    #[track_caller]
    fn from(e: HevcError) -> Self {
        At::from(HeicError::from(e))
    }
}

/// Check a `Stop` token and convert to `At<HeicError>` on cancellation.
#[track_caller]
pub(crate) fn check_stop(stop: &dyn enough::Stop) -> Result<()> {
    stop.check().map_err(|r| At::from(HeicError::Cancelled(r)))
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
    InvalidParameterSet {
        /// Parameter set type (e.g. "SPS", "PPS")
        kind: &'static str,
        /// Description of the issue
        msg: String,
    },
    /// CABAC decoding error
    CabacError(&'static str),
    /// Unsupported profile/level
    UnsupportedProfile {
        /// HEVC profile IDC
        profile: u8,
        /// HEVC level IDC
        level: u8,
    },
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

impl core::error::Error for HevcError {}

/// Errors from probing image headers
#[derive(Debug)]
#[non_exhaustive]
pub enum ProbeError {
    /// Not enough bytes to parse the header
    NeedMoreData,
    /// Data is not a recognized HEIC/HEIF format
    InvalidFormat,
    /// Header is present but malformed
    Corrupt(HeicError),
}

impl fmt::Display for ProbeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NeedMoreData => write!(f, "not enough data to parse header"),
            Self::InvalidFormat => write!(f, "not a valid HEIC/HEIF file"),
            Self::Corrupt(e) => write!(f, "corrupt header: {e}"),
        }
    }
}

impl core::error::Error for ProbeError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Corrupt(e) => Some(e),
            _ => None,
        }
    }
}
