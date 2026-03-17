//! Motion compensation (H.265 8.5.3.3)
//!
//! Quarter-pel luma interpolation (8-tap Wiener) and eighth-pel chroma
//! interpolation (4-tap). Supports uni-prediction and bi-prediction blending.

#![allow(dead_code)] // Phase 5: used when inter decode pipeline is wired up

use alloc::vec;

use super::inter::MotionVector;
use super::picture::DecodedFrame;

/// HEVC luma interpolation filter coefficients (Table 8-5)
/// 4 fractional positions (0=integer, 1=quarter, 2=half, 3=three-quarter) with 8 taps
const LUMA_FILTER: [[i16; 8]; 4] = [
    [0, 0, 0, 64, 0, 0, 0, 0],        // integer
    [-1, 4, -10, 58, 17, -5, 1, 0],   // quarter-pel
    [-1, 4, -11, 40, 40, -11, 4, -1], // half-pel
    [0, 1, -5, 17, 58, -10, 4, -1],   // three-quarter-pel
];

/// HEVC chroma interpolation filter coefficients (Table 8-6)
/// 8 fractional positions with 4 taps
const CHROMA_FILTER: [[i16; 4]; 8] = [
    [0, 64, 0, 0],    // integer
    [-2, 58, 10, -2], // 1/8
    [-4, 54, 16, -2], // 2/8
    [-6, 46, 28, -4], // 3/8
    [-4, 36, 36, -4], // 4/8
    [-4, 28, 46, -6], // 5/8
    [-2, 16, 54, -4], // 6/8
    [-2, 10, 58, -2], // 7/8
];

/// Parameters for a motion compensation block
pub struct McBlock {
    /// PU position x in luma samples
    pub xp: u32,
    /// PU position y in luma samples
    pub yp: u32,
    /// PU width
    pub w: u32,
    /// PU height
    pub h: u32,
    /// Bit depth
    pub bit_depth: u8,
}

/// Perform luma motion compensation for one PU
///
/// Writes prediction samples into `pred` buffer (w*h i16 values).
/// The MV is in quarter-pel units.
/// If `bi_pred` is true, outputs at intermediate precision (shifted left by 6)
/// for subsequent bi-prediction blending. Otherwise outputs final pixel values.
pub fn mc_luma(
    ref_frame: &DecodedFrame,
    mv: MotionVector,
    blk: &McBlock,
    pred: &mut [i16],
    bi_pred: bool,
) {
    let ref_plane = &ref_frame.y_plane;
    let stride = ref_frame.width as i32;
    let pic_w = ref_frame.width as i32;
    let pic_h = ref_frame.height as i32;
    let (w, h) = (blk.w, blk.h);

    let int_x = (blk.xp as i32) + (mv.x as i32 >> 2);
    let int_y = (blk.yp as i32) + (mv.y as i32 >> 2);
    let frac_x = (mv.x as i32 & 3) as usize;
    let frac_y = (mv.y as i32 & 3) as usize;

    // For uni-prediction: shift to final pixel value
    // For bi-prediction: output at intermediate precision (pixel << 6) per H.265 8.5.3.3.3
    let shift1 = blk.bit_depth as i32 - 8 + 6;
    let offset1 = 1i32 << (shift1 - 1);
    let max_val = (1i32 << blk.bit_depth) - 1;

    // For bi-pred: shift3 = 14 - bit_depth (left-shift to intermediate)
    let internal_shift = 14 - blk.bit_depth as i32; // = 6 for 8-bit

    if frac_x == 0 && frac_y == 0 {
        // Integer position: direct copy (or shift up for bi-pred)
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let sx = (int_x + i).clamp(0, pic_w - 1);
                let sy = (int_y + j).clamp(0, pic_h - 1);
                let val = ref_plane[(sy * stride + sx) as usize] as i32;
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    (val << internal_shift) as i16
                } else {
                    val as i16
                };
            }
        }
    } else if frac_y == 0 {
        let coeff = &LUMA_FILTER[frac_x];
        for j in 0..h as i32 {
            let sy = (int_y + j).clamp(0, pic_h - 1);
            for i in 0..w as i32 {
                let mut sum = 0i32;
                for k in 0..8i32 {
                    let sx = (int_x + i + k - 3).clamp(0, pic_w - 1);
                    sum += ref_plane[(sy * stride + sx) as usize] as i32 * coeff[k as usize] as i32;
                }
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    sum as i16 // Keep at filter precision (value << shift1)
                } else {
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16
                };
            }
        }
    } else if frac_x == 0 {
        let coeff = &LUMA_FILTER[frac_y];
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let sx = (int_x + i).clamp(0, pic_w - 1);
                let mut sum = 0i32;
                for k in 0..8i32 {
                    let sy = (int_y + j + k - 3).clamp(0, pic_h - 1);
                    sum += ref_plane[(sy * stride + sx) as usize] as i32 * coeff[k as usize] as i32;
                }
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    sum as i16
                } else {
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16
                };
            }
        }
    } else {
        // Both H and V: two-pass
        let tmp_w = w as i32;
        let tmp_h = h as i32 + 7;
        let mut tmp = vec![0i32; (tmp_w * tmp_h) as usize];

        let coeff_h = &LUMA_FILTER[frac_x];
        for j in 0..tmp_h {
            let sy = (int_y + j - 3).clamp(0, pic_h - 1);
            for i in 0..tmp_w {
                let mut sum = 0i32;
                for k in 0..8i32 {
                    let sx = (int_x + i + k - 3).clamp(0, pic_w - 1);
                    sum +=
                        ref_plane[(sy * stride + sx) as usize] as i32 * coeff_h[k as usize] as i32;
                }
                tmp[(j * tmp_w + i) as usize] = sum;
            }
        }

        let coeff_v = &LUMA_FILTER[frac_y];
        let shift2 = 6i32;
        if bi_pred {
            // For bi-pred: output at intermediate precision (shift by shift2 only)
            // Per H.265 8.5.3.3.3.2: NO rounding offset at this stage.
            // Rounding is applied later in the weighted prediction / averaging step.
            for j in 0..h as i32 {
                for i in 0..w as i32 {
                    let mut sum = 0i64;
                    for k in 0..8i32 {
                        sum +=
                            tmp[((j + k) * tmp_w + i) as usize] as i64 * coeff_v[k as usize] as i64;
                    }
                    pred[(j as u32 * w + i as u32) as usize] = (sum >> shift2) as i16;
                }
            }
        } else {
            // For uni-pred: full shift to pixel values
            let total_shift = shift1 + shift2;
            let total_offset = 1i64 << (total_shift - 1);
            for j in 0..h as i32 {
                for i in 0..w as i32 {
                    let mut sum = 0i64;
                    for k in 0..8i32 {
                        sum +=
                            tmp[((j + k) * tmp_w + i) as usize] as i64 * coeff_v[k as usize] as i64;
                    }
                    pred[(j as u32 * w + i as u32) as usize] =
                        (((sum + total_offset) >> total_shift) as i32).clamp(0, max_val) as i16;
                }
            }
        }
    }
}

/// Chroma reference plane parameters
pub struct ChromaRef<'a> {
    /// Chroma plane samples
    pub plane: &'a [u16],
    /// Chroma plane stride (pixels per row)
    pub stride: usize,
    /// Chroma plane height
    pub height: u32,
    /// Chroma subsampling factor X (2 for 4:2:0)
    pub sub_x: u32,
    /// Chroma subsampling factor Y (2 for 4:2:0)
    pub sub_y: u32,
}

/// Perform chroma motion compensation for one PU
///
/// `mv` is the *luma* MV in quarter-pel units. Chroma MV is derived internally.
/// If `bi_pred` is true, outputs at intermediate precision for bi-prediction blending.
pub fn mc_chroma(
    cref: &ChromaRef<'_>,
    mv: MotionVector,
    blk: &McBlock,
    pred: &mut [i16],
    bi_pred: bool,
) {
    let cmv_x = if cref.sub_x > 1 {
        mv.x as i32
    } else {
        mv.x as i32 * 2
    };
    let cmv_y = if cref.sub_y > 1 {
        mv.y as i32
    } else {
        mv.y as i32 * 2
    };

    let c_stride = cref.stride as i32;
    let c_w = cref.stride as i32;
    let c_h = cref.height as i32;
    let (w, h) = (blk.w, blk.h);

    let int_x = (blk.xp as i32) + (cmv_x >> 3);
    let int_y = (blk.yp as i32) + (cmv_y >> 3);
    let frac_x = (cmv_x & 7) as usize;
    let frac_y = (cmv_y & 7) as usize;

    // Combined shift: spec shift1 (Min(4, BitDepthC-8)) + shift3 (14-BitDepthC)
    // For 8-bit: 0 + 6 = 6. Same normalization as luma (coefficients sum to 64).
    let shift1 = blk.bit_depth as i32 - 8 + 6;
    let offset1 = 1i32 << (shift1 - 1);
    let max_val = (1i32 << blk.bit_depth) - 1;
    let internal_shift = 14 - blk.bit_depth as i32;

    let fetch = |sx: i32, sy: i32| -> i32 {
        let sx = sx.clamp(0, c_w - 1);
        let sy = sy.clamp(0, c_h - 1);
        let idx = (sy * c_stride + sx) as usize;
        if idx < cref.plane.len() {
            cref.plane[idx] as i32
        } else {
            0
        }
    };

    if frac_x == 0 && frac_y == 0 {
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let val = fetch(int_x + i, int_y + j);
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    (val << internal_shift) as i16
                } else {
                    val as i16
                };
            }
        }
    } else if frac_y == 0 {
        let coeff = &CHROMA_FILTER[frac_x];
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let mut sum = 0i32;
                for k in 0..4i32 {
                    sum += fetch(int_x + i + k - 1, int_y + j) * coeff[k as usize] as i32;
                }
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    sum as i16
                } else {
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16
                };
            }
        }
    } else if frac_x == 0 {
        let coeff = &CHROMA_FILTER[frac_y];
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let mut sum = 0i32;
                for k in 0..4i32 {
                    sum += fetch(int_x + i, int_y + j + k - 1) * coeff[k as usize] as i32;
                }
                pred[(j as u32 * w + i as u32) as usize] = if bi_pred {
                    sum as i16
                } else {
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16
                };
            }
        }
    } else {
        let tmp_w = w as i32;
        let tmp_h = h as i32 + 3;
        let mut tmp = vec![0i32; (tmp_w * tmp_h) as usize];

        let coeff_h = &CHROMA_FILTER[frac_x];
        for j in 0..tmp_h {
            for i in 0..tmp_w {
                let mut sum = 0i32;
                for k in 0..4i32 {
                    sum += fetch(int_x + i + k - 1, int_y + j - 1) * coeff_h[k as usize] as i32;
                }
                tmp[(j * tmp_w + i) as usize] = sum;
            }
        }

        let coeff_v = &CHROMA_FILTER[frac_y];
        let shift2 = 6i32; // H.265 spec shift2 = 6 (normalize 2nd filter pass)
        if bi_pred {
            // Per H.265 8.5.3.3.3.2: NO rounding offset at this intermediate stage.
            for j in 0..h as i32 {
                for i in 0..w as i32 {
                    let mut sum = 0i64;
                    for k in 0..4i32 {
                        sum +=
                            tmp[((j + k) * tmp_w + i) as usize] as i64 * coeff_v[k as usize] as i64;
                    }
                    pred[(j as u32 * w + i as u32) as usize] = (sum >> shift2) as i16;
                }
            }
        } else {
            let total_shift = shift1 + shift2;
            let total_offset = 1i64 << (total_shift - 1);
            for j in 0..h as i32 {
                for i in 0..w as i32 {
                    let mut sum = 0i64;
                    for k in 0..4i32 {
                        sum +=
                            tmp[((j + k) * tmp_w + i) as usize] as i64 * coeff_v[k as usize] as i64;
                    }
                    pred[(j as u32 * w + i as u32) as usize] =
                        (((sum + total_offset) >> total_shift) as i32).clamp(0, max_val) as i16;
                }
            }
        }
    }
}

/// Blend uni-prediction samples into a frame plane
pub fn blend_uni(pred: &[i16], plane: &mut [u16], plane_stride: usize, blk: &McBlock) {
    for j in 0..blk.h {
        for i in 0..blk.w {
            let src_idx = (j * blk.w + i) as usize;
            let dst_idx = (blk.yp + j) as usize * plane_stride + (blk.xp + i) as usize;
            if src_idx < pred.len() && dst_idx < plane.len() {
                plane[dst_idx] = pred[src_idx] as u16;
            }
        }
    }
}

/// Blend bi-prediction samples from intermediate-precision inputs
///
/// Inputs are at intermediate precision (pixel << (14 - bit_depth)).
/// Per H.265 8.5.3.3.4: output = Clip((pred0 + pred1 + offset) >> shift, bit_depth)
/// where shift = 15 - bit_depth, offset = 1 << (shift - 1).
pub fn blend_bi(
    pred_l0: &[i16],
    pred_l1: &[i16],
    plane: &mut [u16],
    plane_stride: usize,
    blk: &McBlock,
) {
    let max_val = (1i32 << blk.bit_depth) - 1;
    let shift = 15 - blk.bit_depth as i32; // 7 for 8-bit
    let offset = 1i32 << (shift - 1); // 64 for 8-bit
    for j in 0..blk.h {
        for i in 0..blk.w {
            let src_idx = (j * blk.w + i) as usize;
            let dst_idx = (blk.yp + j) as usize * plane_stride + (blk.xp + i) as usize;
            if src_idx < pred_l0.len() && src_idx < pred_l1.len() && dst_idx < plane.len() {
                let val = ((pred_l0[src_idx] as i32 + pred_l1[src_idx] as i32 + offset) >> shift)
                    .clamp(0, max_val);
                plane[dst_idx] = val as u16;
            }
        }
    }
}

/// Add residual to prediction samples in-place
pub fn add_residual_inter(plane: &mut [u16], plane_stride: usize, residual: &[i16], blk: &McBlock) {
    let max_val = (1i32 << blk.bit_depth) - 1;
    for j in 0..blk.h {
        for i in 0..blk.w {
            let res_idx = (j * blk.w + i) as usize;
            let dst_idx = (blk.yp + j) as usize * plane_stride + (blk.xp + i) as usize;
            if res_idx < residual.len() && dst_idx < plane.len() {
                let val = plane[dst_idx] as i32 + residual[res_idx] as i32;
                plane[dst_idx] = val.clamp(0, max_val) as u16;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_luma_filter_sum() {
        for f in &LUMA_FILTER {
            assert_eq!(f.iter().sum::<i16>(), 64);
        }
    }

    #[test]
    fn test_chroma_filter_sum() {
        for f in &CHROMA_FILTER {
            assert_eq!(f.iter().sum::<i16>(), 64);
        }
    }

    #[test]
    fn test_mc_luma_integer_pos() {
        let mut frame = DecodedFrame::with_params(8, 8, 8, 1);
        for y in 0..8u32 {
            for x in 0..8u32 {
                frame.y_plane[(y * 8 + x) as usize] = (y * 8 + x) as u16;
            }
        }
        let mut pred = vec![0i16; 4 * 4];
        let blk = McBlock {
            xp: 2,
            yp: 2,
            w: 4,
            h: 4,
            bit_depth: 8,
        };
        mc_luma(&frame, MotionVector::ZERO, &blk, &mut pred, false);
        assert_eq!(pred[0], 18); // (2,2) = 2*8+2
        assert_eq!(pred[5], 27); // (3,3) = 3*8+3
    }

    /// Test quarter-pel vertical filtering against hand-calculated value.
    /// Uses a constant reference (all pixels = 100) where output should be exactly 100.
    #[test]
    fn test_mc_luma_vpel_constant() {
        let mut frame = DecodedFrame::with_params(16, 16, 8, 1);
        for p in &mut frame.y_plane {
            *p = 100;
        }
        let mut pred = vec![0i16; 4 * 4];
        let blk = McBlock {
            xp: 4,
            yp: 4,
            w: 4,
            h: 4,
            bit_depth: 8,
        };
        // MV = (0, 1) quarter-pel → frac_y=1, int_y=4
        mc_luma(&frame, MotionVector { x: 0, y: 1 }, &blk, &mut pred, false);
        // All pixels should be 100 (constant input)
        for &v in &pred {
            assert_eq!(v, 100, "constant ref should give exact value");
        }
    }

    /// Test quarter-pel H filter with a known gradient.
    #[test]
    fn test_mc_luma_hpel_gradient() {
        let mut frame = DecodedFrame::with_params(16, 16, 8, 1);
        // Fill with column gradient: pixel = x * 16
        for y in 0..16u32 {
            for x in 0..16u32 {
                frame.y_plane[(y * 16 + x) as usize] = (x * 16) as u16;
            }
        }
        let mut pred = vec![0i16; 1];
        let blk = McBlock {
            xp: 4,
            yp: 4,
            w: 1,
            h: 1,
            bit_depth: 8,
        };
        // MV = (0, 0) → should give pixel at (4,4) = 64
        mc_luma(&frame, MotionVector::ZERO, &blk, &mut pred, false);
        assert_eq!(pred[0], 64);
        // MV = (2, 0) → half-pel horizontal at (4.5, 4)
        mc_luma(&frame, MotionVector { x: 2, y: 0 }, &blk, &mut pred, false);
        // Half-pel of gradient: should be close to average of 64 and 80 = 72
        // Exact: filter[-1,4,-11,40,40,-11,4,-1] applied to [16,32,48,64,80,96,112,128]
        #[allow(clippy::identity_op, clippy::neg_multiply)]
        let expected =
            (-1 * 16 + 4 * 32 - 11 * 48 + 40 * 64 + 40 * 80 - 11 * 96 + 4 * 112 - 1 * 128 + 32)
                >> 6;
        assert_eq!(pred[0], expected as i16, "half-pel horizontal");
    }

    /// Test bi-pred blending with known intermediate values
    #[test]
    fn test_mc_luma_bipred_blend() {
        let mut frame = DecodedFrame::with_params(16, 16, 8, 1);
        for p in &mut frame.y_plane {
            *p = 100;
        }
        let mut pred0 = vec![0i16; 1];
        let mut pred1 = vec![0i16; 1];
        let blk = McBlock {
            xp: 4,
            yp: 4,
            w: 1,
            h: 1,
            bit_depth: 8,
        };
        // Integer position bi-pred: both predict same pixel
        mc_luma(&frame, MotionVector::ZERO, &blk, &mut pred0, true);
        mc_luma(&frame, MotionVector::ZERO, &blk, &mut pred1, true);
        // Intermediate: 100 << 6 = 6400
        assert_eq!(pred0[0], 6400);
        assert_eq!(pred1[0], 6400);
        // Blend: (6400 + 6400 + 64) >> 7 = 12864 >> 7 = 100
        let mut plane = vec![0u16; 256];
        blend_bi(&pred0, &pred1, &mut plane, 16, &blk);
        assert_eq!(plane[4 * 16 + 4], 100);
    }

    #[test]
    fn test_blend_uni() {
        let pred = [100i16, 200, 50, 150];
        let mut plane = vec![0u16; 16];
        let blk = McBlock {
            xp: 1,
            yp: 1,
            w: 2,
            h: 2,
            bit_depth: 8,
        };
        blend_uni(&pred, &mut plane, 4, &blk);
        assert_eq!(plane[5], 100); // (1,1)
        assert_eq!(plane[6], 200); // (2,1)
        assert_eq!(plane[9], 50); // (1,2)
        assert_eq!(plane[10], 150); // (2,2)
    }
}
