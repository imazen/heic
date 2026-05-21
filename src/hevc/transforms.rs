//! Spatial transforms: rotation and mirror operations on decoded frames.
//!
//! Implemented as an extension trait on [`DecodedFrame`] because the type now
//! lives in the `heic-core` crate and Rust's coherence rules forbid inherent
//! impls on foreign types. Callers that previously wrote `frame.rotate_90_cw()`
//! continue to work — `DecodedFrameTransformExt` is imported alongside
//! `DecodedFrame` in `crate::hevc::mod` so method-syntax resolution picks it
//! up automatically.

use alloc::vec::Vec;

use super::DecodedFrame;
use crate::error::HevcError;

type Result<T> = core::result::Result<T, HevcError>;

/// Spatial-transform methods (`mirror_*`, `rotate_*`) on a [`DecodedFrame`].
///
/// Implemented for `DecodedFrame` directly; consumers reach the methods via
/// the usual `frame.rotate_180()?` syntax once the trait is in scope.
pub trait DecodedFrameTransformExt: Sized {
    /// Rotate the frame 90° clockwise, returning a new frame.
    fn rotate_90_cw(&self) -> Result<Self>;
    /// Rotate the frame 180°.
    fn rotate_180(&self) -> Result<Self>;
    /// Rotate the frame 270° clockwise (= 90° counter-clockwise).
    fn rotate_270_cw(&self) -> Result<Self>;
    /// Mirror the frame horizontally (left ↔ right).
    fn mirror_horizontal(&self) -> Result<Self>;
    /// Mirror the frame vertically (top ↔ bottom).
    fn mirror_vertical(&self) -> Result<Self>;
}

impl DecodedFrameTransformExt for DecodedFrame {
    /// Rotate the frame 90° clockwise, returning a new frame.
    ///
    /// Output dimensions are swapped: `(width, height)` becomes `(height, width)`.
    /// Crop offsets are transformed accordingly.
    fn rotate_90_cw(&self) -> Result<Self> {
        let ow = self.width;
        let oh = self.height;
        let nw = oh;
        let nh = ow;

        // Rotate luma: dst(dx, dy) = src(dy, oh-1-dx)
        let mut y_plane = try_vec![0u16; (nw * nh) as usize]?;
        for dy in 0..nh {
            for dx in 0..nw {
                y_plane[(dy * nw + dx) as usize] = self.y_plane[((oh - 1 - dx) * ow + dy) as usize];
            }
        }

        // Rotate alpha plane (same transform as luma)
        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut rotated = try_vec![0u16; (nw * nh) as usize]?;
                for dy in 0..nh {
                    for dx in 0..nw {
                        rotated[(dy * nw + dx) as usize] =
                            alpha[((oh - 1 - dx) * ow + dy) as usize];
                    }
                }
                Ok(rotated)
            })
            .transpose()?;

        // Rotate chroma planes
        let (ocw, och) = self.chroma_dims();
        let (cb_plane, cr_plane) = if ocw > 0 && och > 0 {
            let ncw = och;
            let nch = ocw;
            let csz = (ncw * nch) as usize;
            let mut cb = try_vec![0u16; csz]?;
            let mut cr = try_vec![0u16; csz]?;
            for dy in 0..nch {
                for dx in 0..ncw {
                    let si = (och - 1 - dx) as usize * ocw as usize + dy as usize;
                    let di = dy as usize * ncw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb[di] = self.cb_plane[si];
                        cr[di] = self.cr_plane[si];
                    }
                }
            }
            (cb, cr)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            width: nw,
            height: nh,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth: self.bit_depth,
            chroma_format: self.chroma_format,
            crop_left: self.crop_bottom,
            crop_right: self.crop_top,
            crop_top: self.crop_left,
            crop_bottom: self.crop_right,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane,
            full_range: self.full_range,
            matrix_coeffs: self.matrix_coeffs,
            color_primaries: self.color_primaries,
            transfer_characteristics: self.transfer_characteristics,
        })
    }

    /// Rotate the frame 180°, returning a new frame.
    ///
    /// Dimensions remain the same. Crop offsets are swapped (left↔right, top↔bottom).
    fn rotate_180(&self) -> Result<Self> {
        let w = self.width;
        let h = self.height;

        // Rotate luma: dst(dx, dy) = src(w-1-dx, h-1-dy)
        let mut y_plane = try_vec![0u16; (w * h) as usize]?;
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] =
                    self.y_plane[((h - 1 - dy) * w + (w - 1 - dx)) as usize];
            }
        }

        // Rotate alpha plane
        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut rotated = try_vec![0u16; (w * h) as usize]?;
                for dy in 0..h {
                    for dx in 0..w {
                        rotated[(dy * w + dx) as usize] =
                            alpha[((h - 1 - dy) * w + (w - 1 - dx)) as usize];
                    }
                }
                Ok(rotated)
            })
            .transpose()?;

        // Rotate chroma planes
        let (cw, ch) = self.chroma_dims();
        let (cb_plane, cr_plane) = if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb = try_vec![0u16; csz]?;
            let mut cr = try_vec![0u16; csz]?;
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = (ch - 1 - dy) as usize * cw as usize + (cw - 1 - dx) as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb[di] = self.cb_plane[si];
                        cr[di] = self.cr_plane[si];
                    }
                }
            }
            (cb, cr)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            width: w,
            height: h,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth: self.bit_depth,
            chroma_format: self.chroma_format,
            crop_left: self.crop_right,
            crop_right: self.crop_left,
            crop_top: self.crop_bottom,
            crop_bottom: self.crop_top,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane,
            full_range: self.full_range,
            matrix_coeffs: self.matrix_coeffs,
            color_primaries: self.color_primaries,
            transfer_characteristics: self.transfer_characteristics,
        })
    }

    /// Rotate the frame 270° clockwise (= 90° counter-clockwise), returning a new frame.
    ///
    /// Output dimensions are swapped: `(width, height)` becomes `(height, width)`.
    /// Crop offsets are transformed accordingly.
    fn rotate_270_cw(&self) -> Result<Self> {
        let ow = self.width;
        let oh = self.height;
        let nw = oh;
        let nh = ow;

        // Rotate luma: dst(dx, dy) = src(ow-1-dy, dx)
        let mut y_plane = try_vec![0u16; (nw * nh) as usize]?;
        for dy in 0..nh {
            for dx in 0..nw {
                y_plane[(dy * nw + dx) as usize] = self.y_plane[(dx * ow + (ow - 1 - dy)) as usize];
            }
        }

        // Rotate alpha plane
        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut rotated = try_vec![0u16; (nw * nh) as usize]?;
                for dy in 0..nh {
                    for dx in 0..nw {
                        rotated[(dy * nw + dx) as usize] =
                            alpha[(dx * ow + (ow - 1 - dy)) as usize];
                    }
                }
                Ok(rotated)
            })
            .transpose()?;

        // Rotate chroma planes
        let (ocw, och) = self.chroma_dims();
        let (cb_plane, cr_plane) = if ocw > 0 && och > 0 {
            let ncw = och;
            let nch = ocw;
            let csz = (ncw * nch) as usize;
            let mut cb = try_vec![0u16; csz]?;
            let mut cr = try_vec![0u16; csz]?;
            for dy in 0..nch {
                for dx in 0..ncw {
                    let si = dx as usize * ocw as usize + (ocw - 1 - dy) as usize;
                    let di = dy as usize * ncw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb[di] = self.cb_plane[si];
                        cr[di] = self.cr_plane[si];
                    }
                }
            }
            (cb, cr)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            width: nw,
            height: nh,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth: self.bit_depth,
            chroma_format: self.chroma_format,
            crop_left: self.crop_top,
            crop_right: self.crop_bottom,
            crop_top: self.crop_right,
            crop_bottom: self.crop_left,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane,
            full_range: self.full_range,
            matrix_coeffs: self.matrix_coeffs,
            color_primaries: self.color_primaries,
            transfer_characteristics: self.transfer_characteristics,
        })
    }

    /// Mirror the frame about the vertical axis (left-right flip), returning a new frame.
    ///
    /// Dimensions remain the same. Left and right crop offsets are swapped.
    fn mirror_horizontal(&self) -> Result<Self> {
        let w = self.width;
        let h = self.height;

        let mut y_plane = try_vec![0u16; (w * h) as usize]?;
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] = self.y_plane[(dy * w + (w - 1 - dx)) as usize];
            }
        }

        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut mirrored = try_vec![0u16; (w * h) as usize]?;
                for dy in 0..h {
                    for dx in 0..w {
                        mirrored[(dy * w + dx) as usize] = alpha[(dy * w + (w - 1 - dx)) as usize];
                    }
                }
                Ok(mirrored)
            })
            .transpose()?;

        let (cw, ch) = self.chroma_dims();
        let (cb_plane, cr_plane) = if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb = try_vec![0u16; csz]?;
            let mut cr = try_vec![0u16; csz]?;
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = dy as usize * cw as usize + (cw - 1 - dx) as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb[di] = self.cb_plane[si];
                        cr[di] = self.cr_plane[si];
                    }
                }
            }
            (cb, cr)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            width: w,
            height: h,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth: self.bit_depth,
            chroma_format: self.chroma_format,
            crop_left: self.crop_right,
            crop_right: self.crop_left,
            crop_top: self.crop_top,
            crop_bottom: self.crop_bottom,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane,
            full_range: self.full_range,
            matrix_coeffs: self.matrix_coeffs,
            color_primaries: self.color_primaries,
            transfer_characteristics: self.transfer_characteristics,
        })
    }

    /// Mirror the frame about the horizontal axis (top-bottom flip), returning a new frame.
    ///
    /// Dimensions remain the same. Top and bottom crop offsets are swapped.
    fn mirror_vertical(&self) -> Result<Self> {
        let w = self.width;
        let h = self.height;

        let mut y_plane = try_vec![0u16; (w * h) as usize]?;
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] = self.y_plane[((h - 1 - dy) * w + dx) as usize];
            }
        }

        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut mirrored = try_vec![0u16; (w * h) as usize]?;
                for dy in 0..h {
                    for dx in 0..w {
                        mirrored[(dy * w + dx) as usize] = alpha[((h - 1 - dy) * w + dx) as usize];
                    }
                }
                Ok(mirrored)
            })
            .transpose()?;

        let (cw, ch) = self.chroma_dims();
        let (cb_plane, cr_plane) = if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb = try_vec![0u16; csz]?;
            let mut cr = try_vec![0u16; csz]?;
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = (ch - 1 - dy) as usize * cw as usize + dx as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb[di] = self.cb_plane[si];
                        cr[di] = self.cr_plane[si];
                    }
                }
            }
            (cb, cr)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            width: w,
            height: h,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth: self.bit_depth,
            chroma_format: self.chroma_format,
            crop_left: self.crop_left,
            crop_right: self.crop_right,
            crop_top: self.crop_bottom,
            crop_bottom: self.crop_top,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane,
            full_range: self.full_range,
            matrix_coeffs: self.matrix_coeffs,
            color_primaries: self.color_primaries,
            transfer_characteristics: self.transfer_characteristics,
        })
    }
}
