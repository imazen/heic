//! Errors produced by the shared [`DecodedFrame`](crate::DecodedFrame)
//! construction path.
//!
//! These are the smallest subset of error variants that
//! `DecodedFrame::with_params` and friends can produce. The parent `heic`
//! crate's `HevcError` is a superset and implements `From<HevcError>` to
//! propagate them.

/// Errors produced during `DecodedFrame` construction (dimension overflow,
/// allocation failure).
///
/// Named `HevcError` for source compatibility with the original
/// `heic::hevc::picture` module while DecodedFrame lived inside it. The
/// parent crate has its own `HevcError` (with many more variants); they are
/// related by an `impl From<heic_core::error::HevcError> for heic::HevcError`
/// shim in the parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum HevcError {
    /// Frame dimensions overflow when multiplied (width × height ≥ u32::MAX).
    DimensionOverflow,
    /// Memory allocation failed.
    AllocationFailed,
}

impl core::fmt::Display for HevcError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::DimensionOverflow => f.write_str("frame dimensions overflow"),
            Self::AllocationFailed => f.write_str("allocation failed"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for HevcError {}
