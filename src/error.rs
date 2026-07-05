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
    /// Input ended before enough bytes were available to probe/parse the
    /// container header — distinct from [`InvalidData`](Self::InvalidData):
    /// the bytes seen so far are consistent with a HEIC/HEIF file, there just
    /// aren't enough of them yet (truncated download, streamed input cut
    /// short, …).
    Truncated(&'static str),
    /// The input's magic bytes / container brand don't match any recognized
    /// HEIF/HEIC brand — this isn't a HEIC/HEIF file at all, as opposed to
    /// [`InvalidData`](Self::InvalidData) (a HEIF file whose content is
    /// corrupt) or [`Unsupported`](Self::Unsupported) (a recognized feature
    /// this decoder doesn't implement).
    NotHeif(&'static str),
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
            Self::Truncated(msg) => write!(f, "truncated input: {msg}"),
            Self::NotHeif(msg) => write!(f, "not a HEIC/HEIF file: {msg}"),
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

/// Pattern B envelope bridge: a **bare** [`HeicError`] converts into the shared
/// [`zencodec::CodecError`] envelope as `At<CodecError>`, reading its
/// [`category`](zencodec::CategorizedError::category) and codec name (`"heic"`)
/// from the value. This is what lets the `zencodec` *trait* impls (on
/// [`HeicDecoderConfig`](crate::HeicDecoderConfig) and its decode job / decoder
/// types) declare `type Error = At<CodecError>`, so a generic
/// consumer recovers the category and codec name *through `Dyn*` dispatch* —
/// after the error is erased to a `Box<dyn Error>` — by downcasting to the
/// concrete `At<CodecError>`. (The native rich-error API keeps returning
/// `At<HeicError>` directly; only the trait boundary uses this envelope.)
///
/// `.start_at()` begins the location trace; [`CodecError::of`](zencodec::CodecError::of)
/// keeps it on the outside (`At<CodecError>`). An
/// `impl From<At<HeicError>> for At<CodecError>` is impossible (orphan rule —
/// `At` is foreign and non-fundamental), so an *already-located* `At<HeicError>`
/// converts at the boundary with `.map_err(zencodec::CodecError::of)` instead;
/// this bridge handles bare `HeicError` constructions (via `?` / `.into()`).
impl From<HeicError> for At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: HeicError) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
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

// ── Codec-agnostic error taxonomy (zencodec PR #103) ───────────────────────
//
// Maps every native error variant to exactly one coarse
// [`zencodec::ErrorCategory`] so a consumer can route on the category (HTTP
// status, retry policy, logging) without naming these enums. `zencodec` is an
// optional dependency, so these impls are feature-gated; the native enums are
// unchanged and remain the public error surface. The blanket
// `impl CategorizedError for At<E>` in zencodec forwards through the `At<…>`
// location wrapper these errors are returned in, so `At<HeicError>::category()`
// works automatically.

/// Coarse [`zencodec::ErrorCategory`] for a [`HeicError`].
impl zencodec::CategorizedError for HeicError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::ErrorCategory as C;
        match self {
            // Corrupt / invalid container or bitstream content.
            Self::InvalidContainer(_) | Self::InvalidData(_) => C::MalformedImage,
            // A parseable container missing its required primary item — bad
            // input (it is surfaced under `ProbeError::Corrupt`), not a bug.
            Self::NoPrimaryImage => C::MalformedImage,
            // Not enough bytes yet to probe/parse the header — the caller
            // should retry once more data is available, not treat this as a
            // malformed image.
            Self::Truncated(_) => C::UnexpectedEof,
            // The input isn't a HEIC/HEIF file at all (magic bytes / brand
            // don't match), as opposed to a recognized-but-corrupt one.
            Self::NotHeif(_) => C::UnsupportedImageType,
            // A valid HEIC feature this decoder does not implement. The message
            // bundles several flavours (overlay version, unci component layout,
            // construction_method, GainMapRender mode, …); the codec *type* not
            // being handled has its own `UnsupportedCodec` arm, so the best
            // single fit here is "unsupported feature".
            Self::Unsupported(_) => C::UnsupportedImageFeature,
            // The image's codec/profile itself is not handled (AV1 without the
            // `av1` feature, JPEG, H.264, …).
            Self::UnsupportedCodec(_) => C::UnsupportedImageType,
            // Delegate to the nested HEVC error's own classification.
            Self::HevcDecode(e) => e.category(),
            // A caller-supplied output buffer is too small for `decode_into`.
            Self::BufferTooSmall { .. } => C::InvalidBuffer,
            // A `ResourceLimits` cap was exceeded. The stringly variant cannot
            // preserve the specific `LimitKind` — its sites span width / height /
            // pixel-count / memory / input-size / element-count overflows — so it
            // is reported under the representative `Pixels` kind; the message
            // still names the specific cap. (Codecs that need the exact kind can
            // recover the cause from the trace.)
            Self::LimitExceeded(_) => C::LimitsExceeded(zencodec::LimitKind::Pixels),
            // A real allocation failure, distinct from a configured cap.
            Self::OutOfMemory => C::OutOfMemory,
            // Cooperative cancellation — delegate (Cancelled vs TimedOut).
            Self::Cancelled(r) => r.category(),
            // A decode sink (the caller's output) reported an I/O failure.
            Self::Sink(_) => C::Io(zencodec::CodecIoKind::opaque()),
            // An empty backend allowlist in the caller's `DecoderConfig`: bad
            // caller configuration, not bad image data.
            Self::NoBackendSelected => C::InvalidParameters,
            // No backend produced an image (none installed, or every backend
            // rejected the bitstream) — a mixed sub-component failure.
            Self::AllBackendsFailed(_) => C::Internal,
        }
    }
}

/// Coarse [`zencodec::ErrorCategory`] for a [`HevcError`].
///
/// Delegated to by [`HeicError::HevcDecode`] so the parent error inherits the
/// per-variant HEVC classification instead of a blanket `Internal`.
impl zencodec::CategorizedError for HevcError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::ErrorCategory as C;
        match self {
            // Corrupt / invalid HEVC bitstream content.
            Self::InvalidNalUnit(_)
            | Self::InvalidBitstream(_)
            | Self::MissingParameterSet(_)
            | Self::InvalidParameterSet { .. }
            | Self::CabacError(_) => C::MalformedImage,
            // A profile/level this decoder does not handle.
            Self::UnsupportedProfile { .. } => C::UnsupportedImageType,
            // A valid HEVC feature we don't implement (e.g. dependent slices).
            Self::Unsupported(_) => C::UnsupportedImageFeature,
            // Internal decode-invariant failures (missing current picture,
            // internal map size overflows) — not attributable to the input.
            Self::DecodingError(_) => C::Internal,
            // Allocation failure.
            Self::AllocationFailed => C::OutOfMemory,
            // width × height overflow — a (computed) pixel-count limit.
            Self::DimensionOverflow => C::LimitsExceeded(zencodec::LimitKind::Pixels),
            // Cooperative cancellation — delegate.
            Self::Cancelled(r) => r.category(),
        }
    }
}

/// Coarse [`zencodec::ErrorCategory`] for a [`ProbeError`] (caller-facing, from
/// header probing).
impl zencodec::CategorizedError for ProbeError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::ErrorCategory as C;
        match self {
            // Truncated header / insufficient input.
            Self::NeedMoreData => C::UnexpectedEof,
            // Magic bytes don't match — this isn't a HEIC/HEIF file at all.
            Self::InvalidFormat => C::UnsupportedImageType,
            // A present-but-malformed header: delegate to the wrapped located
            // `HeicError` (the `At<E>` blanket impl forwards its category).
            Self::Corrupt(e) => e.category(),
        }
    }
}

#[cfg(test)]
mod error_tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::string::ToString;

    #[test]
    fn heic_error_display_and_debug_all_variants() {
        let sink: Box<dyn core::error::Error + Send + Sync> = "sink boom".into();
        let variants: alloc::vec::Vec<HeicError> = alloc::vec![
            HeicError::InvalidContainer("ic"),
            HeicError::InvalidData("id"),
            HeicError::Truncated("t"),
            HeicError::NotHeif("nh"),
            HeicError::Unsupported("u"),
            HeicError::NoPrimaryImage,
            HeicError::HevcDecode(HevcError::InvalidBitstream("b")),
            HeicError::BufferTooSmall {
                required: 100,
                actual: 50
            },
            HeicError::LimitExceeded("le"),
            HeicError::OutOfMemory,
            HeicError::Cancelled(StopReason::Cancelled),
            HeicError::Sink(sink),
            HeicError::UnsupportedCodec("uc"),
            HeicError::NoBackendSelected,
            HeicError::AllBackendsFailed("abf".to_string()),
        ];
        for v in &variants {
            assert!(!alloc::format!("{v}").is_empty(), "empty Display for {v:?}");
            assert!(!alloc::format!("{v:?}").is_empty());
        }
    }

    #[test]
    fn hevc_error_display_and_debug_all_variants() {
        let variants: alloc::vec::Vec<HevcError> = alloc::vec![
            HevcError::InvalidNalUnit("n"),
            HevcError::InvalidBitstream("b"),
            HevcError::MissingParameterSet("SPS"),
            HevcError::InvalidParameterSet {
                kind: "SPS",
                msg: "m".into()
            },
            HevcError::CabacError("c"),
            HevcError::UnsupportedProfile {
                profile: 4,
                level: 120
            },
            HevcError::Unsupported("u"),
            HevcError::DecodingError("d"),
            HevcError::AllocationFailed,
            HevcError::DimensionOverflow,
            HevcError::Cancelled(enough::StopReason::Cancelled),
        ];
        for v in &variants {
            assert!(!alloc::format!("{v}").is_empty(), "empty Display for {v:?}");
            assert!(!alloc::format!("{v:?}").is_empty());
        }
    }

    #[test]
    fn probe_error_display() {
        for v in [ProbeError::NeedMoreData, ProbeError::InvalidFormat] {
            assert!(!alloc::format!("{v}").is_empty());
            assert!(!alloc::format!("{v:?}").is_empty());
        }
    }

    #[test]
    fn from_conversions() {
        let h: HeicError = HevcError::InvalidBitstream("x").into();
        assert!(matches!(h, HeicError::HevcDecode(_)));
        let h: HeicError = StopReason::Cancelled.into();
        assert!(matches!(h, HeicError::Cancelled(_)));
        // core::error::Error::source / std error trait object usability
        let e: HeicError = HeicError::OutOfMemory;
        let _src = core::error::Error::source(&e);
    }
}

#[cfg(test)]
mod category_tests {
    use super::*;
    use alloc::boxed::Box;
    use zencodec::{CategorizedError, ErrorCategory as C, LimitKind as L};

    #[test]
    fn heic_error_category_maps_every_variant() {
        assert_eq!(HeicError::NoPrimaryImage.codec_name(), Some("heic"));
        let sink: Box<dyn core::error::Error + Send + Sync> = "boom".into();
        let cases: alloc::vec::Vec<(HeicError, C)> = alloc::vec![
            (HeicError::InvalidContainer("c"), C::MalformedImage),
            (HeicError::InvalidData("d"), C::MalformedImage),
            (HeicError::NoPrimaryImage, C::MalformedImage),
            (HeicError::Truncated("t"), C::UnexpectedEof),
            (HeicError::NotHeif("nh"), C::UnsupportedImageType),
            (HeicError::Unsupported("u"), C::UnsupportedImageFeature),
            (HeicError::UnsupportedCodec("av1"), C::UnsupportedImageType),
            (
                HeicError::HevcDecode(HevcError::InvalidBitstream("b")),
                C::MalformedImage,
            ),
            (
                HeicError::BufferTooSmall {
                    required: 10,
                    actual: 4,
                },
                C::InvalidBuffer,
            ),
            (
                HeicError::LimitExceeded("pixel count exceeds limit"),
                C::LimitsExceeded(L::Pixels),
            ),
            (HeicError::OutOfMemory, C::OutOfMemory),
            (HeicError::Cancelled(StopReason::Cancelled), C::Cancelled),
            (HeicError::Cancelled(StopReason::TimedOut), C::TimedOut),
            (
                HeicError::Sink(sink),
                C::Io(zencodec::CodecIoKind::opaque())
            ),
            (HeicError::NoBackendSelected, C::InvalidParameters),
            (HeicError::AllBackendsFailed("abf".into()), C::Internal),
        ];
        for (err, want) in &cases {
            assert_eq!(err.category(), *want, "wrong category for {err:?}");
        }
    }

    #[test]
    fn hevc_decode_delegates_to_inner_category() {
        // The parent inherits the nested HEVC classification rather than a
        // blanket `Internal`.
        assert_eq!(
            HeicError::HevcDecode(HevcError::UnsupportedProfile {
                profile: 4,
                level: 120
            })
            .category(),
            C::UnsupportedImageType,
        );
        assert_eq!(
            HeicError::HevcDecode(HevcError::DimensionOverflow).category(),
            C::LimitsExceeded(L::Pixels),
        );
        assert_eq!(
            HeicError::HevcDecode(HevcError::Cancelled(StopReason::TimedOut)).category(),
            C::TimedOut,
        );
    }

    #[test]
    fn hevc_error_category_maps_every_variant() {
        assert_eq!(HevcError::DimensionOverflow.codec_name(), Some("heic"));
        let cases: alloc::vec::Vec<(HevcError, C)> = alloc::vec![
            (HevcError::InvalidNalUnit("n"), C::MalformedImage),
            (HevcError::InvalidBitstream("b"), C::MalformedImage),
            (HevcError::MissingParameterSet("SPS"), C::MalformedImage),
            (
                HevcError::InvalidParameterSet {
                    kind: "SPS",
                    msg: "m".into()
                },
                C::MalformedImage,
            ),
            (HevcError::CabacError("c"), C::MalformedImage),
            (
                HevcError::UnsupportedProfile {
                    profile: 4,
                    level: 120
                },
                C::UnsupportedImageType,
            ),
            (HevcError::Unsupported("u"), C::UnsupportedImageFeature),
            (HevcError::DecodingError("d"), C::Internal),
            (HevcError::AllocationFailed, C::OutOfMemory),
            (HevcError::DimensionOverflow, C::LimitsExceeded(L::Pixels)),
            (HevcError::Cancelled(StopReason::Cancelled), C::Cancelled),
        ];
        for (err, want) in &cases {
            assert_eq!(err.category(), *want, "wrong category for {err:?}");
        }
    }

    #[test]
    fn probe_error_category_maps_every_variant() {
        assert_eq!(ProbeError::NeedMoreData.codec_name(), Some("heic"));
        assert_eq!(ProbeError::NeedMoreData.category(), C::UnexpectedEof);
        assert_eq!(
            ProbeError::InvalidFormat.category(),
            C::UnsupportedImageType
        );
        // Corrupt delegates to the wrapped located HeicError.
        assert_eq!(
            ProbeError::Corrupt(at!(HeicError::InvalidData("x"))).category(),
            C::MalformedImage,
        );
        assert_eq!(
            ProbeError::Corrupt(at!(HeicError::NoPrimaryImage)).category(),
            C::MalformedImage,
        );
    }

    #[test]
    fn category_and_codec_name_forward_through_at() {
        // The form codecs actually return: At<HeicError>. The blanket
        // `impl CategorizedError for At<E>` forwards both axes.
        let located: At<HeicError> = at!(HeicError::Cancelled(StopReason::TimedOut));
        assert_eq!(located.category(), C::TimedOut);
        assert_eq!(located.codec_name(), Some("heic"));

        let located2: At<HevcError> = at!(HevcError::CabacError("x"));
        assert_eq!(located2.category(), C::MalformedImage);
        assert_eq!(located2.codec_name(), Some("heic"));
    }
}
