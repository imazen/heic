//! YCbCr → RGB conversion shared by every backend.
//!
//! **Migration placeholder.** The full color-conversion implementation
//! (`to_rgb`, `to_rgba`, `to_rgb16`, `to_rgba16`, SIMD `convert_420_to_rgb`)
//! currently lives in the parent crate at `heic::hevc::picture` and
//! `heic::hevc::color_convert`. A subsequent commit moves it here so that
//! native-FFI backend crates can produce the same RGB output without
//! depending on the parent.
