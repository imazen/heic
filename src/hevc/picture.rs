//! Decoded frame representation — moved to the `heic-core` crate.
//!
//! The [`DecodedFrame`] struct, its construction primitives, internal
//! deblocking-metadata helpers, and the YCbCr→RGB output methods all live in
//! [`heic_core::frame`]. This module is a thin re-export so the pure-Rust
//! decoder modules under `src/hevc/` continue to use the familiar
//! `super::picture::DecodedFrame` import path while the move is in progress.
//!
//! The eventual `heic-backend-rust` extraction (planned PR 2) replaces this
//! shim with a direct import from `heic_core`.

pub use heic_core::frame::{
    DEBLOCK_FLAG_HORIZ, DEBLOCK_FLAG_VERT, DEBLOCK_PB_EDGE_HORIZ, DEBLOCK_PB_EDGE_VERT,
    DecodedFrame, UNINIT_SAMPLE,
};
