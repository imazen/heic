//! SIMD-accelerated YCbCr→RGB color conversion — moved to the `heic-core` crate.
//!
//! See [`heic_core::color_convert`]. This shim preserves the
//! `crate::hevc::color_convert::*` import path used internally and by the
//! existing public surface that exposes `convert_420_to_rgb` and friends.

pub use heic_core::color_convert::*;
