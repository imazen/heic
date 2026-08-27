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
    /// container header or read a required body — distinct from
    /// [`InvalidData`](Self::InvalidData): the bytes seen so far are
    /// consistent with a HEIC/HEIF file, there just aren't enough of them
    /// yet (truncated download, streamed input cut short, …).
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
    /// A resource limit was exceeded (dimensions, pixel count, or memory).
    ///
    /// Stringly by design: the call sites that construct this span
    /// width/height/pixel-count/memory/input-size/element-count overflows
    /// scattered across the container parser and don't each hold a typed
    /// [`zencodec::LimitKind`](https://docs.rs/zencodec) (that would make
    /// `zencodec` a required dependency of this always-compiled error type,
    /// not just the optional adapter). See
    /// [`ResourceLimit`](Self::ResourceLimit) for the (feature-gated) typed
    /// alternative used where a real `zencodec::LimitExceeded` is already in
    /// hand.
    LimitExceeded(&'static str),
    /// A configured [`zencodec::ResourceLimits`](https://docs.rs/zencodec)
    /// cap was exceeded, with the typed limit preserved (kind + actual/max) —
    /// used at the `zencodec` adapter boundary (`src/codec.rs`), where a real
    /// `zencodec::LimitExceeded` value is already in hand instead of being
    /// collapsed into the stringly [`LimitExceeded`](Self::LimitExceeded).
    /// Only constructible with the `zencodec` feature enabled.
    #[cfg(feature = "zencodec")]
    ResourceLimit(zencodec::LimitExceeded),
    /// Memory allocation failed
    OutOfMemory,
    /// Operation was cancelled via cooperative cancellation
    Cancelled(StopReason),
    /// A decode sink reported an error
    Sink(alloc::boxed::Box<dyn core::error::Error + Send + Sync>),
    /// Codec not supported (e.g., AV1 without the `av1` feature, or JPEG, or H.264)
    UnsupportedCodec(&'static str),
    /// The request was well-formed but asks for an operation or pixel format
    /// this decoder does not support — e.g. animation decoding (HEIC has no
    /// concept of an animation), or pixel-format negotiation finding no
    /// overlap between the caller's `preferred` list and what this image can
    /// produce. Distinct from [`Unsupported`](Self::Unsupported): the image
    /// bytes are not the problem, the *invocation* is — a different call
    /// with a different requested op/format could succeed on the same bytes.
    /// Only constructible with the `zencodec` feature enabled (the
    /// [`zencodec::UnsupportedOperation`](https://docs.rs/zencodec) axis is a
    /// zencodec type).
    #[cfg(feature = "zencodec")]
    UnsupportedOperation(zencodec::UnsupportedOperation),
    /// The caller's request was invalid for a reason that isn't a pixel
    /// buffer/state protocol violation — e.g. a
    /// [`zencodec::GainMapRender`](https://docs.rs/zencodec) variant this
    /// build of the decoder doesn't recognize (a future, `#[non_exhaustive]`
    /// mode from a newer `zencodec` than this crate was compiled against).
    /// General-purpose bucket for caller-request-origin failures that don't
    /// fit [`UnsupportedOperation`](Self::UnsupportedOperation) or
    /// [`BufferTooSmall`](Self::BufferTooSmall).
    InvalidRequest(&'static str),
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
    AllBackendsFailed {
        /// Each backend's reason, in allowlist order, joined into one
        /// message — so users can read exactly what happened.
        detail: alloc::string::String,
        /// `true` when at least one backend was available and its
        /// [`decode_hevc`](heic_core::HevcBackend::decode_hevc) call
        /// actually rejected the bitstream
        /// ([`heic_core::BackendError::Decode`]), rather than every backend
        /// reporting itself unavailable
        /// ([`heic_core::BackendError::Unavailable`], never reaching a real
        /// decode attempt). Distinguishes "no backend could even be tried"
        /// (an environment/deployment gap) from "every available backend
        /// rejected this specific input" (an image-bytes fault) — see the
        /// `category()` mapping below.
        rejected_bitstream: bool,
    },
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
            #[cfg(feature = "zencodec")]
            Self::ResourceLimit(limit) => write!(f, "resource limit exceeded: {limit}"),
            Self::OutOfMemory => write!(f, "out of memory"),
            Self::Cancelled(reason) => write!(f, "{reason}"),
            Self::Sink(e) => write!(f, "decode sink error: {e}"),
            Self::UnsupportedCodec(msg) => write!(f, "unsupported codec: {msg}"),
            #[cfg(feature = "zencodec")]
            Self::UnsupportedOperation(op) => write!(f, "{op}"),
            Self::InvalidRequest(msg) => write!(f, "invalid request: {msg}"),
            Self::NoBackendSelected => write!(
                f,
                "no HEVC backend selected in DecoderConfig (use \
                 DecoderConfig::with_backends or rely on \
                 DecoderConfig::new which installs the recommended allowlist)"
            ),
            Self::AllBackendsFailed { detail, .. } => {
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
            #[cfg(feature = "zencodec")]
            Self::ResourceLimit(e) => Some(e),
            #[cfg(feature = "zencodec")]
            Self::UnsupportedOperation(e) => Some(e),
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

/// Carry a located HEVC decoder error across the module boundary.
///
/// The decoder's entry points (`crate::hevc::decode`,
/// `decode_with_config_stop`, …) return `At<HevcError>` whose trace starts
/// at the line inside the CABAC / residual / slice / parameter-set code that
/// detected the problem (#25). `map_error` keeps that trace while converting
/// the payload (`HevcError::Cancelled` still becomes `HeicError::Cancelled`).
///
/// Call sites append the boundary frame themselves with
/// `.map_err(crate::error::hevc_at).at()` (`whereat::ResultAtExt::at` is
/// `#[track_caller]`). This function deliberately is NOT `#[track_caller]`:
/// passed by name to `map_err` it is invoked through `FnOnce::call_once`, and
/// a `#[track_caller]` location taken there points at
/// `core/src/ops/function.rs`, not at the caller.
pub(crate) fn hevc_at(e: At<HevcError>) -> At<HeicError> {
    e.map_error(HeicError::from)
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
            // `heic_core::error::HevcError` is `#[non_exhaustive]`; a variant
            // this crate doesn't recognize yet (added to heic-core after
            // this crate was last updated) is captured honestly via its
            // Display text rather than silently guessed as
            // `AllocationFailed` — which would mislabel any future
            // non-alloc heic-core error as OOM. Categorizes as
            // `Internal(InternalKind::Dependency)`: an honest
            // "unclassified", not a permanent home (see `category()` below).
            other => Self::CoreUnclassified(alloc::format!("{other}")),
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
    /// Unsupported profile/level.
    ///
    /// The base format is recognized (this crate decodes HEVC in general) —
    /// a specific *profile* is a declared HEVC coding-tool subset
    /// (`general_profile_idc` + compatibility/constraint flags in
    /// `profile_tier_level()`), analogous to "arithmetic-coded JPEG": the
    /// container is understood, this particular coding-tool combination
    /// isn't implemented. Categorizes as `Image(Unsupported(Feature))`, not
    /// `Type` (see `category()` below for the reasoning — this reads
    /// differently from zencodec's own `UnsupportedImageKind::Type` doc
    /// example, which cites "an unsupported HEVC profile"; flagged rather
    /// than silently picked either way).
    ///
    /// No current call site constructs this — `general_profile_idc` is
    /// parsed into `ProfileTierLevel` (`hevc/params.rs`) but never checked
    /// against a supported-profile allowlist. When real profile gating is
    /// added, attach `profile`/`level` to the located error's trace via
    /// `whereat`'s `at_data` (e.g. a small `HevcProfileLevel { profile,
    /// level }` context type) so a generic consumer can recover the pair
    /// after type erasure to `Box<dyn Error>`, without downcasting to this
    /// crate's own error types.
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
    /// An error surfaced from `heic_core::error::HevcError` that this
    /// crate's `HevcError` has no specific counterpart for (captured via its
    /// `Display` text). `heic_core::error::HevcError` is `#[non_exhaustive]`
    /// with exactly two variants today (`AllocationFailed`,
    /// `DimensionOverflow`), both matched explicitly in
    /// `From<heic_core::error::HevcError> for HevcError` — this only fires
    /// for a future variant added there before this crate is updated to
    /// recognize it. Categorizes as `Internal(InternalKind::Dependency)`: an
    /// honest "unclassified", not a permanent home.
    CoreUnclassified(String),
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
            Self::CoreUnclassified(msg) => write!(f, "unclassified heic-core error: {msg}"),
        }
    }
}

impl core::error::Error for HevcError {}

impl From<TryReserveError> for HevcError {
    fn from(_: TryReserveError) -> Self {
        Self::AllocationFailed
    }
}

/// Errors from probing image headers.
///
/// [`ImageInfo::from_bytes`](crate::ImageInfo::from_bytes) returns this
/// wrapped in [`At`], so the trace of a [`Corrupt`](Self::Corrupt) probe
/// starts at the container / parameter-set line that rejected the input
/// (the same origin a full decode would report), followed by the probe
/// boundary frame. The `Corrupt` payload is the bare [`HeicError`] — the
/// location lives on the enclosing `At<ProbeError>`, not nested inside.
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

// ── Codec-agnostic error taxonomy (zencodec PR #116 — two-level origin-first
// ErrorCategory) ─────────────────────────────────────────────────────────────
//
// Maps every native error variant to exactly one coarse
// `zencodec::ErrorCategory` so a consumer can route on the category (HTTP
// status, retry policy, logging) without naming these enums. `zencodec` is an
// optional dependency here (unlike zenpng/zenjpeg, where it is required), so
// these impls — and the handful of `HeicError` variants that carry a
// zencodec-typed payload (`ResourceLimit`, `UnsupportedOperation`) — are
// feature-gated; the native enums are otherwise unchanged and remain the
// public error surface regardless of the `zencodec` feature. The blanket
// `impl CategorizedError for At<E>` in zencodec forwards through the `At<…>`
// location wrapper these errors are returned in, so `At<HeicError>::category()`
// works automatically.

/// Coarse [`zencodec::ErrorCategory`] for a [`HeicError`].
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for HeicError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError as Img, InternalKind, InvalidKind, RequestError as Req,
            ResourceError as Res, UnsupportedImageKind as UIK,
        };
        match self {
            // Corrupt / invalid container or bitstream content.
            Self::InvalidContainer(_) | Self::InvalidData(_) => C::Image(Img::Malformed),
            // A parseable container missing its required primary item — bad
            // input (surfaced under `ProbeError::Corrupt`), not a bug.
            Self::NoPrimaryImage => C::Image(Img::Malformed),
            // Not enough bytes yet to probe/parse the header or a required
            // body — the caller should retry once more data is available,
            // not treat this as a malformed image.
            Self::Truncated(_) => C::Image(Img::UnexpectedEof),
            // The input isn't a HEIC/HEIF file at all (magic bytes / brand
            // don't match), as opposed to a recognized-but-corrupt one.
            Self::NotHeif(_) => C::Image(Img::Unsupported(UIK::Type)),
            // A valid HEIC feature this decoder does not implement (overlay
            // version, unci component layout, construction_method, …).
            Self::Unsupported(_) => C::Image(Img::Unsupported(UIK::Feature)),
            // The image's codec/profile itself is not handled (AV1 without
            // the `av1` feature, JPEG, H.264, …) — the codec/container type,
            // not a specific coding-tool feature within a handled codec.
            Self::UnsupportedCodec(_) => C::Image(Img::Unsupported(UIK::Type)),
            // Delegate to the nested HEVC error's own classification.
            Self::HevcDecode(e) => e.category(),
            // A caller-supplied output buffer is too small for `decode_into`
            // — a pixel-buffer geometry fault, not bad image data.
            Self::BufferTooSmall { .. } => C::Request(Req::Invalid(InvalidKind::Buffer)),
            // Stringly bucket spanning width/height/pixel-count/memory/
            // input-size overflows with no single common `LimitKind` and no
            // typed value in hand at the call site (see the variant doc).
            // `InputSize` is the least-wrong single default: most call sites
            // are "some substructure derived from the input is implausibly
            // large", which is closer to an input-size-shaped anti-DoS bound
            // than to a literal pixel/dimension cap (only a minority of
            // sites are literally width/height/pixel overflow). Known
            // imprecision — not a per-site kind.
            Self::LimitExceeded(_) => C::Resource(Res::Limits(zencodec::LimitKind::InputSize)),
            // A real `zencodec::LimitExceeded` was in hand at the call site —
            // delegate for the precise kind + actual/max.
            Self::ResourceLimit(limit) => limit.category(),
            // Memory acquisition failure (alloc failed or address-space
            // overflow via `TryReserveError`).
            Self::OutOfMemory => C::Resource(Res::OutOfMemory),
            // Cooperative cancellation — delegate (Cancelled vs TimedOut).
            Self::Cancelled(r) => r.category(),
            // A decode sink (the caller's output) reported a failure.
            Self::Sink(_) => C::Io(zencodec::CodecIoKind::opaque()),
            // The whole operation axis (including `PixelFormat`) is a
            // caller-request fault — delegate to carry *which* operation.
            Self::UnsupportedOperation(op) => op.category(),
            // A caller-request-origin failure with no more specific bucket
            // (e.g. an unrecognized future `GainMapRender` variant).
            Self::InvalidRequest(_) => C::Request(Req::Invalid(InvalidKind::Parameters)),
            // An empty backend allowlist in the caller's `DecoderConfig`: bad
            // caller configuration, not bad image data.
            Self::NoBackendSelected => C::Request(Req::Invalid(InvalidKind::Parameters)),
            // At least one backend actually attempted the bitstream and
            // rejected it (`BackendError::Decode`) — an image-bytes fault.
            // When every backend only ever reported itself unavailable, no
            // real decoder ever looked at these bytes — an
            // environment/deployment gap, not a fact about the image.
            Self::AllBackendsFailed {
                rejected_bitstream: true,
                ..
            } => C::Image(Img::Malformed),
            Self::AllBackendsFailed {
                rejected_bitstream: false,
                ..
            } => C::Internal(InternalKind::Dependency),
        }
    }
}

/// Coarse [`zencodec::ErrorCategory`] for a [`HevcError`].
///
/// Delegated to by [`HeicError::HevcDecode`] so the parent error inherits the
/// per-variant HEVC classification instead of a blanket `Internal`.
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for HevcError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{
            ErrorCategory as C, ImageError as Img, InternalKind, UnsupportedImageKind as UIK,
        };
        match self {
            // Corrupt / invalid HEVC bitstream content.
            Self::InvalidNalUnit(_)
            | Self::InvalidBitstream(_)
            | Self::MissingParameterSet(_)
            | Self::InvalidParameterSet { .. }
            | Self::CabacError(_) => C::Image(Img::Malformed),
            // A specific HEVC coding-tool combination (profile) this decoder
            // doesn't implement — the base format (HEVC) IS handled, so this
            // is a *feature* gap, not a different codec/container type. See
            // the variant doc for the reasoning (deliberately disagrees with
            // zencodec's own `UnsupportedImageKind::Type` doc example, which
            // names "an unsupported HEVC profile" — flagged, not silently
            // picked either way).
            Self::UnsupportedProfile { .. } => C::Image(Img::Unsupported(UIK::Feature)),
            // A valid HEVC feature we don't implement (e.g. dependent slices).
            Self::Unsupported(_) => C::Image(Img::Unsupported(UIK::Feature)),
            // A broken invariant in this decoder's own logic (missing
            // current picture, internal map size overflows) — matches this
            // variant's own doc ("not attributable to the input").
            Self::DecodingError(_) => C::Internal(InternalKind::Bug),
            // Allocation failure.
            Self::AllocationFailed => C::Resource(zencodec::ResourceError::OutOfMemory),
            // width × height overflow — a (computed) pixel-count limit.
            Self::DimensionOverflow => {
                C::Resource(zencodec::ResourceError::Limits(zencodec::LimitKind::Pixels))
            }
            // Cooperative cancellation — delegate.
            Self::Cancelled(r) => r.category(),
            // A heic-core error this crate doesn't recognize yet — honest
            // "unclassified" rather than a guessed home (see variant doc).
            Self::CoreUnclassified(_) => C::Internal(InternalKind::Dependency),
        }
    }
}

/// Coarse [`zencodec::ErrorCategory`] for a [`ProbeError`] (caller-facing,
/// from header probing).
#[cfg(feature = "zencodec")]
impl zencodec::CategorizedError for ProbeError {
    fn codec_name(&self) -> Option<&'static str> {
        Some("heic")
    }

    fn category(&self) -> zencodec::ErrorCategory {
        use zencodec::{ErrorCategory as C, ImageError as Img, UnsupportedImageKind as UIK};
        match self {
            // Truncated header / insufficient input.
            Self::NeedMoreData => C::Image(Img::UnexpectedEof),
            // Magic bytes don't match — this isn't a HEIC/HEIF file at all.
            Self::InvalidFormat => C::Image(Img::Unsupported(UIK::Type)),
            // A present-but-malformed header: delegate to the wrapped
            // `HeicError`.
            Self::Corrupt(e) => e.category(),
        }
    }
}

/// Bridge a bare [`HeicError`] into the shared
/// [`CodecError`](zencodec::CodecError) envelope (Pattern B) — what lets the
/// `zencodec` *trait* impls (`src/codec.rs`) declare `type Error =
/// At<zencodec::CodecError>`, so a generic consumer recovers the
/// [`category`](zencodec::CategorizedError::category) *and* the codec name
/// through `Dyn*` dispatch, after erasure to a boxed `dyn Error`.
///
/// `.start_at()` begins the location trace; [`CodecError::of`](zencodec::CodecError::of) keeps it on
/// the outside (`At<CodecError>`). An `impl From<At<HeicError>> for
/// At<CodecError>` is impossible (orphan rule — `At` is foreign and
/// non-fundamental), so an *already-located* `At<HeicError>` converts at the
/// boundary with `.map_err(zencodec::CodecError::of)` instead; this bridge
/// handles bare `HeicError` constructions (via `?` / `.into()`). The native
/// rich-error API (`DecoderConfig::decode`, `decode_rgba8`,
/// `ImageInfo::from_bytes`, …) is unaffected and keeps returning
/// `At<HeicError>` directly — only the `zencodec` trait boundary uses this
/// envelope.
#[cfg(feature = "zencodec")]
impl From<HeicError> for At<zencodec::CodecError> {
    #[track_caller]
    fn from(e: HeicError) -> Self {
        use whereat::ErrorAtExt;
        zencodec::CodecError::of(e.start_at())
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
            HeicError::InvalidRequest("ir"),
            HeicError::OutOfMemory,
            HeicError::Cancelled(StopReason::Cancelled),
            HeicError::Sink(sink),
            HeicError::UnsupportedCodec("uc"),
            HeicError::NoBackendSelected,
            HeicError::AllBackendsFailed {
                detail: "abf".to_string(),
                rejected_bitstream: false,
            },
        ];
        for v in &variants {
            assert!(!alloc::format!("{v}").is_empty(), "empty Display for {v:?}");
            assert!(!alloc::format!("{v:?}").is_empty());
        }
    }

    #[test]
    #[cfg(feature = "zencodec")]
    fn heic_error_zencodec_variants_display_and_debug() {
        let variants: alloc::vec::Vec<HeicError> = alloc::vec![
            HeicError::ResourceLimit(zencodec::LimitExceeded::Pixels { actual: 9, max: 4 }),
            HeicError::UnsupportedOperation(zencodec::UnsupportedOperation::AnimationDecode),
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
            HevcError::CoreUnclassified("future core error".to_string()),
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

    #[test]
    fn heic_core_hevc_error_maps_known_variants() {
        // The two heic_core::error::HevcError variants that exist today map
        // explicitly, never through the CoreUnclassified fallback.
        let h: HevcError = heic_core::error::HevcError::AllocationFailed.into();
        assert!(matches!(h, HevcError::AllocationFailed));
        let h: HevcError = heic_core::error::HevcError::DimensionOverflow.into();
        assert!(matches!(h, HevcError::DimensionOverflow));
    }
}

#[cfg(all(test, feature = "zencodec"))]
mod category_tests {
    use super::*;
    use alloc::boxed::Box;
    use alloc::string::ToString;
    use zencodec::{
        CategorizedError, ErrorCategory as C, ImageError as Img, InternalKind, InvalidKind,
        LimitKind as L, RequestError as Req, ResourceError as Res, UnsupportedImageKind as UIK,
        UnsupportedOperation as Op,
    };

    #[test]
    fn heic_error_category_maps_every_variant() {
        assert_eq!(HeicError::NoPrimaryImage.codec_name(), Some("heic"));
        let sink: Box<dyn core::error::Error + Send + Sync> = "boom".into();
        let cases: alloc::vec::Vec<(HeicError, C)> = alloc::vec![
            (HeicError::InvalidContainer("c"), C::Image(Img::Malformed)),
            (HeicError::InvalidData("d"), C::Image(Img::Malformed)),
            (HeicError::NoPrimaryImage, C::Image(Img::Malformed)),
            (HeicError::Truncated("t"), C::Image(Img::UnexpectedEof)),
            (
                HeicError::NotHeif("nh"),
                C::Image(Img::Unsupported(UIK::Type))
            ),
            (
                HeicError::Unsupported("u"),
                C::Image(Img::Unsupported(UIK::Feature)),
            ),
            (
                HeicError::UnsupportedCodec("av1"),
                C::Image(Img::Unsupported(UIK::Type)),
            ),
            (
                HeicError::HevcDecode(HevcError::InvalidBitstream("b")),
                C::Image(Img::Malformed),
            ),
            (
                HeicError::BufferTooSmall {
                    required: 10,
                    actual: 4,
                },
                C::Request(Req::Invalid(InvalidKind::Buffer)),
            ),
            (
                HeicError::LimitExceeded("pixel count exceeds limit"),
                C::Resource(Res::Limits(L::InputSize)),
            ),
            (
                HeicError::ResourceLimit(zencodec::LimitExceeded::Memory { actual: 9, max: 4 }),
                C::Resource(Res::Limits(L::Memory)),
            ),
            (
                HeicError::UnsupportedOperation(Op::PixelFormat),
                C::Request(Req::Unsupported(Op::PixelFormat)),
            ),
            (
                HeicError::UnsupportedOperation(Op::AnimationDecode),
                C::Request(Req::Unsupported(Op::AnimationDecode)),
            ),
            (
                HeicError::InvalidRequest("unrecognized GainMapRender mode"),
                C::Request(Req::Invalid(InvalidKind::Parameters)),
            ),
            (HeicError::OutOfMemory, C::Resource(Res::OutOfMemory)),
            (
                HeicError::Cancelled(StopReason::Cancelled),
                C::Stopped(StopReason::Cancelled),
            ),
            (
                HeicError::Cancelled(StopReason::TimedOut),
                C::Stopped(StopReason::TimedOut),
            ),
            (
                HeicError::Sink(sink),
                C::Io(zencodec::CodecIoKind::opaque()),
            ),
            (
                HeicError::NoBackendSelected,
                C::Request(Req::Invalid(InvalidKind::Parameters)),
            ),
            (
                HeicError::AllBackendsFailed {
                    detail: "abf".to_string(),
                    rejected_bitstream: true,
                },
                C::Image(Img::Malformed),
            ),
            (
                HeicError::AllBackendsFailed {
                    detail: "abf".to_string(),
                    rejected_bitstream: false,
                },
                C::Internal(InternalKind::Dependency),
            ),
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
            C::Image(Img::Unsupported(UIK::Feature)),
        );
        assert_eq!(
            HeicError::HevcDecode(HevcError::DimensionOverflow).category(),
            C::Resource(Res::Limits(L::Pixels)),
        );
        assert_eq!(
            HeicError::HevcDecode(HevcError::Cancelled(StopReason::TimedOut)).category(),
            C::Stopped(StopReason::TimedOut),
        );
    }

    #[test]
    fn hevc_error_category_maps_every_variant() {
        assert_eq!(HevcError::DimensionOverflow.codec_name(), Some("heic"));
        let cases: alloc::vec::Vec<(HevcError, C)> = alloc::vec![
            (HevcError::InvalidNalUnit("n"), C::Image(Img::Malformed)),
            (HevcError::InvalidBitstream("b"), C::Image(Img::Malformed)),
            (
                HevcError::MissingParameterSet("SPS"),
                C::Image(Img::Malformed),
            ),
            (
                HevcError::InvalidParameterSet {
                    kind: "SPS",
                    msg: "m".into()
                },
                C::Image(Img::Malformed),
            ),
            (HevcError::CabacError("c"), C::Image(Img::Malformed)),
            (
                HevcError::UnsupportedProfile {
                    profile: 4,
                    level: 120
                },
                C::Image(Img::Unsupported(UIK::Feature)),
            ),
            (
                HevcError::Unsupported("u"),
                C::Image(Img::Unsupported(UIK::Feature)),
            ),
            (
                HevcError::DecodingError("d"),
                C::Internal(InternalKind::Bug),
            ),
            (HevcError::AllocationFailed, C::Resource(Res::OutOfMemory)),
            (
                HevcError::DimensionOverflow,
                C::Resource(Res::Limits(L::Pixels)),
            ),
            (
                HevcError::Cancelled(StopReason::Cancelled),
                C::Stopped(StopReason::Cancelled),
            ),
            (
                HevcError::CoreUnclassified("x".to_string()),
                C::Internal(InternalKind::Dependency),
            ),
        ];
        for (err, want) in &cases {
            assert_eq!(err.category(), *want, "wrong category for {err:?}");
        }
    }

    #[test]
    fn probe_error_category_maps_every_variant() {
        assert_eq!(ProbeError::NeedMoreData.codec_name(), Some("heic"));
        assert_eq!(
            ProbeError::NeedMoreData.category(),
            C::Image(Img::UnexpectedEof)
        );
        assert_eq!(
            ProbeError::InvalidFormat.category(),
            C::Image(Img::Unsupported(UIK::Type))
        );
        // Corrupt delegates to the wrapped HeicError.
        assert_eq!(
            ProbeError::Corrupt(HeicError::InvalidData("x")).category(),
            C::Image(Img::Malformed),
        );
        assert_eq!(
            ProbeError::Corrupt(HeicError::NoPrimaryImage).category(),
            C::Image(Img::Malformed),
        );
    }

    #[test]
    fn category_and_codec_name_forward_through_at() {
        // The form codecs actually return: At<HeicError>. The blanket
        // `impl CategorizedError for At<E>` forwards both axes.
        let located: At<HeicError> = at!(HeicError::Cancelled(StopReason::TimedOut));
        assert_eq!(located.category(), C::Stopped(StopReason::TimedOut));
        assert_eq!(located.codec_name(), Some("heic"));

        let located2: At<HevcError> = at!(HevcError::CabacError("x"));
        assert_eq!(located2.category(), C::Image(Img::Malformed));
        assert_eq!(located2.codec_name(), Some("heic"));
    }

    #[test]
    fn codec_error_envelope_preserves_category_and_codec_name() {
        // The Pattern-B bridge: bare HeicError -> At<CodecError>, surviving
        // erasure to Box<dyn Error>.
        let e: At<zencodec::CodecError> = HeicError::Truncated("eof").into();
        assert_eq!(e.category(), C::Image(Img::UnexpectedEof));
        assert_eq!(e.error().codec(), Some("heic"));

        let boxed: Box<dyn core::error::Error + Send + Sync> = Box::new(e);
        use zencodec::CodecErrorExt;
        assert_eq!(boxed.error_category(), Some(C::Image(Img::UnexpectedEof)));
        assert_eq!(
            boxed.codec_error().and_then(zencodec::CodecError::codec),
            Some("heic")
        );
    }
}
