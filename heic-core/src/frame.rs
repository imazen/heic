//! Decoded YCbCr frame produced by every backend.
//!
//! Skeleton — the full `DecodedFrame` definition migrates from
//! `heic::hevc::picture` in a follow-up commit. This module currently re-exports
//! a placeholder so dependent backends can name the type while the move is
//! sequenced.

// Migration in progress: the canonical `DecodedFrame` still lives in the
// parent crate at `heic::hevc::picture::DecodedFrame`. A subsequent commit
// moves the struct definition here and turns the parent's path into a
// re-export. Backends that depend on `heic-core` should already import via
// `heic_core::DecodedFrame` so the eventual move is invisible to them.

use alloc::vec::Vec;

/// Decoded HEVC frame with planar YCbCr samples.
///
/// **Placeholder during migration** — see module-level note. Will become the
/// canonical type after the move from `heic::hevc::picture`.
#[derive(Debug)]
#[non_exhaustive]
pub struct DecodedFrame {
    /// Width in pixels (full frame, before cropping).
    pub width: u32,
    /// Height in pixels (full frame, before cropping).
    pub height: u32,
    /// Luma (Y) plane — `u16` samples, `bit_depth` bits significant.
    pub y_plane: Vec<u16>,
    /// Cb chroma plane (subsampled per `chroma_format`).
    pub cb_plane: Vec<u16>,
    /// Cr chroma plane (subsampled per `chroma_format`).
    pub cr_plane: Vec<u16>,
    /// Bit depth (8 or 10).
    pub bit_depth: u8,
    /// Chroma format (1=4:2:0, 2=4:2:2, 3=4:4:4).
    pub chroma_format: u8,
    /// Conformance window left offset (in luma samples).
    pub crop_left: u32,
    /// Conformance window right offset (in luma samples).
    pub crop_right: u32,
    /// Conformance window top offset (in luma samples).
    pub crop_top: u32,
    /// Conformance window bottom offset (in luma samples).
    pub crop_bottom: u32,
    /// Alpha plane (optional, from auxiliary alpha image).
    pub alpha_plane: Option<Vec<u16>>,
    /// Video full range flag (from SPS VUI). true = full \[0,255\], false = limited \[16,235\].
    pub full_range: bool,
    /// Matrix coefficients (CICP). 1=BT.709, 5/6=BT.601, 9=BT.2020, 2=unspecified.
    pub matrix_coeffs: u8,
    /// Color primaries (CICP). 1=BT.709, 9=BT.2020, 12=Display P3, 2=unspecified.
    pub color_primaries: u8,
    /// Transfer characteristics (CICP). 1=BT.709, 13=sRGB, 16=PQ, 18=HLG, 2=unspecified.
    pub transfer_characteristics: u8,
}
