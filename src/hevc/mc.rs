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
    [0, 0, 0, 64, 0, 0, 0, 0],       // integer
    [-1, 4, -10, 58, 17, -5, 1, 0],   // quarter-pel
    [-1, 4, -11, 40, 40, -11, 4, -1], // half-pel
    [0, 1, -5, 17, 58, -10, 4, -1],   // three-quarter-pel
];

/// HEVC chroma interpolation filter coefficients (Table 8-6)
/// 8 fractional positions with 4 taps
const CHROMA_FILTER: [[i16; 4]; 8] = [
    [0, 64, 0, 0],   // integer
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
pub fn mc_luma(ref_frame: &DecodedFrame, mv: MotionVector, blk: &McBlock, pred: &mut [i16]) {
    let ref_plane = &ref_frame.y_plane;
    let stride = ref_frame.width as i32;
    let pic_w = ref_frame.width as i32;
    let pic_h = ref_frame.height as i32;
    let (w, h) = (blk.w, blk.h);

    let int_x = (blk.xp as i32) + (mv.x as i32 >> 2);
    let int_y = (blk.yp as i32) + (mv.y as i32 >> 2);
    let frac_x = (mv.x as i32 & 3) as usize;
    let frac_y = (mv.y as i32 & 3) as usize;

    let shift1 = blk.bit_depth as i32 - 8 + 6;
    let offset1 = 1i32 << (shift1 - 1);
    let max_val = (1i32 << blk.bit_depth) - 1;

    if frac_x == 0 && frac_y == 0 {
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let sx = (int_x + i).clamp(0, pic_w - 1);
                let sy = (int_y + j).clamp(0, pic_h - 1);
                pred[(j as u32 * w + i as u32) as usize] =
                    ref_plane[(sy * stride + sx) as usize] as i16;
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
                pred[(j as u32 * w + i as u32) as usize] =
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16;
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
                pred[(j as u32 * w + i as u32) as usize] =
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16;
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
                    sum += ref_plane[(sy * stride + sx) as usize] as i32 * coeff_h[k as usize] as i32;
                }
                tmp[(j * tmp_w + i) as usize] = sum;
            }
        }

        let coeff_v = &LUMA_FILTER[frac_y];
        let shift2 = 6i32;
        let total_shift = shift1 + shift2;
        let total_offset = 1i64 << (total_shift - 1);
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let mut sum = 0i64;
                for k in 0..8i32 {
                    sum += tmp[((j + k) * tmp_w + i) as usize] as i64
                        * coeff_v[k as usize] as i64;
                }
                pred[(j as u32 * w + i as u32) as usize] =
                    (((sum + total_offset) >> total_shift) as i32).clamp(0, max_val) as i16;
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
pub fn mc_chroma(cref: &ChromaRef<'_>, mv: MotionVector, blk: &McBlock, pred: &mut [i16]) {
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
    let max_val = (1i32 << blk.bit_depth) - 1;

    let int_x = (blk.xp as i32) + (cmv_x >> 3);
    let int_y = (blk.yp as i32) + (cmv_y >> 3);
    let frac_x = (cmv_x & 7) as usize;
    let frac_y = (cmv_y & 7) as usize;

    let shift1 = blk.bit_depth as i32 - 8 + 4;
    let offset1 = 1i32 << (shift1 - 1);

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
                pred[(j as u32 * w + i as u32) as usize] = fetch(int_x + i, int_y + j) as i16;
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
                pred[(j as u32 * w + i as u32) as usize] =
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16;
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
                pred[(j as u32 * w + i as u32) as usize] =
                    ((sum + offset1) >> shift1).clamp(0, max_val) as i16;
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
        let shift2 = 4i32;
        let total_shift = shift1 + shift2;
        let total_offset = 1i64 << (total_shift - 1);
        for j in 0..h as i32 {
            for i in 0..w as i32 {
                let mut sum = 0i64;
                for k in 0..4i32 {
                    sum += tmp[((j + k) * tmp_w + i) as usize] as i64
                        * coeff_v[k as usize] as i64;
                }
                pred[(j as u32 * w + i as u32) as usize] =
                    (((sum + total_offset) >> total_shift) as i32).clamp(0, max_val) as i16;
            }
        }
    }
}

/// Blend uni-prediction samples into a frame plane
pub fn blend_uni(
    pred: &[i16],
    plane: &mut [u16],
    plane_stride: usize,
    blk: &McBlock,
) {
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

/// Blend bi-prediction samples: average of L0 and L1 prediction
pub fn blend_bi(
    pred_l0: &[i16],
    pred_l1: &[i16],
    plane: &mut [u16],
    plane_stride: usize,
    blk: &McBlock,
) {
    let max_val = (1i32 << blk.bit_depth) - 1;
    for j in 0..blk.h {
        for i in 0..blk.w {
            let src_idx = (j * blk.w + i) as usize;
            let dst_idx = (blk.yp + j) as usize * plane_stride + (blk.xp + i) as usize;
            if src_idx < pred_l0.len() && src_idx < pred_l1.len() && dst_idx < plane.len() {
                let val = ((pred_l0[src_idx] as i32 + pred_l1[src_idx] as i32 + 1) >> 1)
                    .clamp(0, max_val);
                plane[dst_idx] = val as u16;
            }
        }
    }
}

/// Add residual to prediction samples in-place
pub fn add_residual_inter(
    plane: &mut [u16],
    plane_stride: usize,
    residual: &[i16],
    blk: &McBlock,
) {
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
        mc_luma(&frame, MotionVector::ZERO, &blk, &mut pred);
        assert_eq!(pred[0], 18); // (2,2) = 2*8+2
        assert_eq!(pred[5], 27); // (3,3) = 3*8+3
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
