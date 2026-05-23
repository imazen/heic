//! Error types for HEIC decoding

use alloc::collections::TryReserveError;
use alloc::string::String;
use core::fmt;
use enough::StopReason;
use whereat::{At, at};

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
    /// Memory allocation failed
    OutOfMemory,
    /// Operation was cancelled via cooperative cancellation
    Cancelled(StopReason),
    /// A decode sink reported an error
    Sink(alloc::boxed::Box<dyn core::error::Error + Send + Sync>),
    /// Codec not supported (e.g., AV1 without the `av1` feature, or JPEG, or H.264)
    UnsupportedCodec(&'static str),
    /// `DecoderConfig` had an empty backend allowlist when decode was called.
    ///
    /// Pass an ordered list to
    /// [`DecoderConfig::with_backends`](crate::DecoderConfig::with_backends),
    /// or use [`DecoderConfig::new()`](crate::DecoderConfig::new) which
    /// installs [`recommended_backends`](crate::recommended_backends) by
    /// default.
    NoBackendSelected,
    /// Every backend in the allowlist either reported unavailable or failed
    /// on this bitstream.
    ///
    /// The string captures each backend's reason in order so users can tell
    /// whether the failure was "no decoder installed" vs "decoder rejected
    /// the bitstream".
    AllBackendsFailed(alloc::string::String),
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
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::Cancelled(reason) => write!(f, "{reason}"),
            Self::Sink(e) => write!(f, "decode sink error: {e}"),
            Self::UnsupportedCodec(msg) => write!(f, "unsupported codec: {msg}"),
            Self::NoBackendSelected => write!(
                f,
                "no HEVC backend selected in DecoderConfig (use \
                 DecoderConfig::with_backends or rely on \
                 DecoderConfig::new which installs the recommended allowlist)"
            ),
            Self::AllBackendsFailed(detail) => {
                write!(f, "every HEVC backend in the allowlist failed: {detail}")
            }
        }
    }
}

impl core::error::Error for HeicError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::HevcDecode(e) => Some(e),
            Self::Sink(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<HevcError> for HeicError {
    fn from(e: HevcError) -> Self {
        match e {
            // Cancellation propagates as a first-class HeicError so callers
            // can distinguish it from a real decode failure without
            // unwrapping the nested HevcError.
            HevcError::Cancelled(reason) => Self::Cancelled(reason),
            other => Self::HevcDecode(other),
        }
    }
}

impl From<StopReason> for HeicError {
    fn from(r: StopReason) -> Self {
        Self::Cancelled(r)
    }
}

impl From<TryReserveError> for HeicError {
    fn from(_: TryReserveError) -> Self {
        Self::OutOfMemory
    }
}

// Two-hop conversion for ? operator: HevcError → At<HeicError>
impl From<HevcError> for At<HeicError> {
    #[track_caller]
    fn from(e: HevcError) -> Self {
        at!(HeicError::from(e))
    }
}

// Promote heic-core's small DecodedFrame-construction error to the parent
// HevcError so the rust decoder's `try_vec![...]?` and DecodedFrame methods
// (now living in heic-core) compose naturally with the parent's error type.
impl From<heic_core::error::HevcError> for HevcError {
    fn from(e: heic_core::error::HevcError) -> Self {
        match e {
            heic_core::error::HevcError::AllocationFailed => Self::AllocationFailed,
            heic_core::error::HevcError::DimensionOverflow => Self::DimensionOverflow,
            // `heic_core::error::HevcError` is `#[non_exhaustive]`; future
            // variants get bucketed into the closest existing parent variant
            // until a more specific mapping is added.
            _ => Self::AllocationFailed,
        }
    }
}

impl From<heic_core::error::HevcError> for HeicError {
    fn from(e: heic_core::error::HevcError) -> Self {
        Self::HevcDecode(HevcError::from(e))
    }
}

// `impl From<heic_core::error::HevcError> for At<HeicError>` would be the
// natural `?`-friendly conversion but the orphan rule forbids it: both
// `From` and `At` are foreign, and the inner `HeicError` parameter of `At`
// doesn't count for orphan purposes. Use [`at_core`] at call sites instead.

/// Helper to convert a `heic_core::error::HevcError` into an `At<HeicError>`
/// at the current call site, capturing the source location like `?` would.
///
/// Use at the boundary where the parent crate calls heic-core methods that
/// return `heic_core::error::HevcError`:
///
/// ```ignore
/// frame.to_rgb().map_err(at_core)?;
/// ```
#[track_caller]
pub(crate) fn at_core(e: heic_core::error::HevcError) -> At<HeicError> {
    at!(HeicError::from(e))
}

/// Check a `Stop` token and convert to `At<HeicError>` on cancellation.
#[track_caller]
pub(crate) fn check_stop(stop: &dyn enough::Stop) -> Result<()> {
    stop.check().map_err(|r| at!(HeicError::Cancelled(r)))
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
    /// Memory allocation failed
    AllocationFailed,
    /// Dimension overflow (width * height exceeds limits)
    DimensionOverflow,
    /// Decode was cancelled by an [`enough::Stop`] token.
    Cancelled(enough::StopReason),
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
            Self::AllocationFailed => write!(f, "memory allocation failed"),
            Self::DimensionOverflow => write!(f, "frame dimensions overflow"),
            Self::Cancelled(reason) => write!(f, "HEVC decode cancelled: {reason:?}"),
        }
    }
}

impl core::error::Error for HevcError {}

impl From<TryReserveError> for HevcError {
    fn from(_: TryReserveError) -> Self {
        Self::AllocationFailed
    }
}

/// Errors from probing image headers
#[derive(Debug)]
#[non_exhaustive]
pub enum ProbeError {
    /// Not enough bytes to parse the header
    NeedMoreData,
    /// Data is not a recognized HEIC/HEIF format
    InvalidFormat,
    /// Header is present but malformed
    Corrupt(At<HeicError>),
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
