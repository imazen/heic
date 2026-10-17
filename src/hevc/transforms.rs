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

use super::Result;
use crate::error::HevcError;
use whereat::at;

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
        let mut y_plane =
            try_vec![0u16; (nw * nh) as usize].map_err(|_| at!(HevcError::AllocationFailed))?;
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
                let mut rotated = try_vec![0u16; (nw * nh) as usize]
                    .map_err(|_| at!(HevcError::AllocationFailed))?;
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
            let mut cb = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
            let mut cr = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
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
        let mut y_plane =
            try_vec![0u16; (w * h) as usize].map_err(|_| at!(HevcError::AllocationFailed))?;
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
                let mut rotated = try_vec![0u16; (w * h) as usize]
                    .map_err(|_| at!(HevcError::AllocationFailed))?;
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
            let mut cb = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
            let mut cr = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
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
        let mut y_plane =
            try_vec![0u16; (nw * nh) as usize].map_err(|_| at!(HevcError::AllocationFailed))?;
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
                let mut rotated = try_vec![0u16; (nw * nh) as usize]
                    .map_err(|_| at!(HevcError::AllocationFailed))?;
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
            let mut cb = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
            let mut cr = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
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

        let mut y_plane =
            try_vec![0u16; (w * h) as usize].map_err(|_| at!(HevcError::AllocationFailed))?;
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] = self.y_plane[(dy * w + (w - 1 - dx)) as usize];
            }
        }

        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut mirrored = try_vec![0u16; (w * h) as usize]
                    .map_err(|_| at!(HevcError::AllocationFailed))?;
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
            let mut cb = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
            let mut cr = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
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

        let mut y_plane =
            try_vec![0u16; (w * h) as usize].map_err(|_| at!(HevcError::AllocationFailed))?;
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] = self.y_plane[((h - 1 - dy) * w + dx) as usize];
            }
        }

        let alpha_plane = self
            .alpha_plane
            .as_ref()
            .map(|alpha| -> Result<Vec<u16>> {
                let mut mirrored = try_vec![0u16; (w * h) as usize]
                    .map_err(|_| at!(HevcError::AllocationFailed))?;
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
            let mut cb = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
            let mut cr = try_vec![0u16; csz].map_err(|_| at!(HevcError::AllocationFailed))?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hevc::DecodedFrame;

    /// Build a small frame with distinct, deterministic per-plane pixel values.
    fn frame(w: u32, h: u32, chroma: u8) -> DecodedFrame {
        let mut f = DecodedFrame::with_params(w, h, 8, chroma).unwrap();
        for (i, p) in f.y_plane.iter_mut().enumerate() {
            *p = (i as u16) & 0x3FF;
        }
        for (i, p) in f.cb_plane.iter_mut().enumerate() {
            *p = ((i as u16).wrapping_mul(3).wrapping_add(1)) & 0x3FF;
        }
        for (i, p) in f.cr_plane.iter_mut().enumerate() {
            *p = ((i as u16).wrapping_mul(5).wrapping_add(2)) & 0x3FF;
        }
        f
    }

    fn planes_eq(a: &DecodedFrame, b: &DecodedFrame) -> bool {
        a.width == b.width
            && a.height == b.height
            && a.y_plane == b.y_plane
            && a.cb_plane == b.cb_plane
            && a.cr_plane == b.cr_plane
    }

    #[test]
    fn rotate_90_swaps_dims_and_roundtrips() {
        let f = frame(4, 6, 1); // 4:2:0, even dims
        let r1 = f.rotate_90_cw().unwrap();
        assert_eq!((r1.width, r1.height), (6, 4), "90° swaps w/h");
        let back = f
            .rotate_90_cw()
            .unwrap()
            .rotate_90_cw()
            .unwrap()
            .rotate_90_cw()
            .unwrap()
            .rotate_90_cw()
            .unwrap();
        assert!(planes_eq(&f, &back), "rotate_90 ×4 must be identity");
    }

    #[test]
    fn rotate_180_is_double_90_and_involution() {
        let f = frame(4, 6, 1);
        let r180 = f.rotate_180().unwrap();
        assert_eq!((r180.width, r180.height), (4, 6));
        let via_90 = f.rotate_90_cw().unwrap().rotate_90_cw().unwrap();
        assert!(planes_eq(&r180, &via_90), "rotate_180 == two rotate_90");
        assert!(
            planes_eq(&f, &r180.rotate_180().unwrap()),
            "rotate_180 is an involution"
        );
    }

    #[test]
    fn rotate_270_inverts_90() {
        let f = frame(4, 6, 1);
        let r270 = f.rotate_270_cw().unwrap();
        assert_eq!((r270.width, r270.height), (6, 4));
        let back = f.rotate_90_cw().unwrap().rotate_270_cw().unwrap();
        assert!(planes_eq(&f, &back), "rotate_270 inverts rotate_90");
    }

    #[test]
    fn mirrors_are_involutions_and_compose_to_180() {
        let f = frame(4, 6, 1);
        let h = f.mirror_horizontal().unwrap();
        assert_eq!((h.width, h.height), (4, 6));
        assert!(
            planes_eq(&f, &h.mirror_horizontal().unwrap()),
            "mirror_h involution"
        );
        let v = f.mirror_vertical().unwrap();
        assert!(
            planes_eq(&f, &v.mirror_vertical().unwrap()),
            "mirror_v involution"
        );
        let hv = f.mirror_horizontal().unwrap().mirror_vertical().unwrap();
        assert!(
            planes_eq(&hv, &f.rotate_180().unwrap()),
            "mirror_h∘mirror_v == rotate_180"
        );
    }
}
