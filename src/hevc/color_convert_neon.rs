//! NEON (AArch64) SIMD-accelerated YCbCr->RGB color conversion.
//!
//! Processes 8 pixels per iteration using 128-bit NEON intrinsics.
//! The x86 AVX2 version processes 8 pixels with 256-bit registers;
//! this NEON version achieves the same throughput per iteration since
//! i32 operations on 8 values require 2x int32x4_t per channel.

#![allow(clippy::too_many_arguments)]

use archmage::prelude::*;

#[cfg(target_arch = "aarch64")]
use safe_unaligned_simd::aarch64::{vld1q_u16, vld1_u16, vst1q_s32};

/// NEON YCbCr->RGB conversion -- processes 8 pixels per iteration
#[cfg(target_arch = "aarch64")]
#[arcane]
pub(crate) fn convert_420_to_rgb_neon(
    _token: NeonToken,
    y_plane: &[u16],
    cb_plane: &[u16],
    cr_plane: &[u16],
    y_stride: usize,
    c_stride: usize,
    y_start: u32,
    y_end: u32,
    x_start: u32,
    x_end: u32,
    shift: u32,
    full_range: bool,
    matrix_coeffs: u8,
    rgb: &mut [u8],
) {
    let (cr_r, cb_g, cr_g, cb_b, y_bias, y_scale, rnd, shr) =
        super::color_convert::get_coefficients(full_range, matrix_coeffs);

    let cr_r_v = vdupq_n_s32(cr_r);
    let cb_g_v = vdupq_n_s32(cb_g);
    let cr_g_v = vdupq_n_s32(cr_g);
    let cb_b_v = vdupq_n_s32(cb_b);
    let y_bias_v = vdupq_n_s32(y_bias);
    let y_scale_v = vdupq_n_s32(y_scale);
    let rnd_v = vdupq_n_s32(rnd);
    let bias128_v = vdupq_n_s32(128);
    let zero = vdupq_n_s32(0);
    let max255 = vdupq_n_s32(255);
    let neg_shr = vdupq_n_s32(-shr);
    let neg_shift = if shift > 0 {
        Some(vdupq_n_s32(-(shift as i32)))
    } else {
        None
    };

    // Align SIMD start to even x for 4:2:0 chroma alignment
    let x_simd_start = x_start.next_multiple_of(2);
    let row_pixels = x_end.saturating_sub(x_simd_start) as usize;
    let simd_count = (row_pixels / 8) * 8;
    let x_simd_end = x_simd_start + simd_count as u32;

    let mut out_idx = 0;

    for y in y_start..y_end {
        let y_row = y as usize * y_stride;
        let c_row = (y as usize / 2) * c_stride;

        // Scalar prefix: handle odd x_start
        for x in x_start..x_simd_start.min(x_end) {
            super::color_convert::scalar_pixel(
                y_plane, cb_plane, cr_plane, y_row, c_row, x as usize, shift, y_bias,
                y_scale, cr_r, cb_g, cr_g, cb_b, rnd, shr, rgb, &mut out_idx,
            );
        }

        // SIMD: 8 pixels per iteration
        let mut x = x_simd_start as usize;
        let x_end_simd = x_simd_end as usize;
        while x < x_end_simd {
            let cx = x / 2;

            // Load 8 Y values (u16) -> zero-extend to 8xi32 (2x int32x4_t)
            let y_raw = vld1q_u16(
                y_plane[y_row + x..y_row + x + 8].try_into().unwrap(),
            );
            let mut y_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(y_raw)));
            let mut y_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(y_raw)));

            // Load 4 Cb/Cr values, duplicate each for 4:2:0 -> 8xi32
            let cb_raw = vld1_u16(
                cb_plane[c_row + cx..c_row + cx + 4].try_into().unwrap(),
            );
            let cr_raw = vld1_u16(
                cr_plane[c_row + cx..c_row + cx + 4].try_into().unwrap(),
            );
            // Duplicate each chroma sample: [a,b,c,d] -> [a,a,b,b,c,c,d,d]
            // vzip1/vzip2 on 64-bit registers gives [a,a,b,b] and [c,c,d,d]
            let cb_dup_full = vcombine_u16(
                vzip1_u16(cb_raw, cb_raw),
                vzip2_u16(cb_raw, cb_raw),
            );
            let cr_dup_full = vcombine_u16(
                vzip1_u16(cr_raw, cr_raw),
                vzip2_u16(cr_raw, cr_raw),
            );

            let mut cb_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(cb_dup_full)));
            let mut cb_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(cb_dup_full)));
            let mut cr_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(cr_dup_full)));
            let mut cr_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(cr_dup_full)));

            // 10-bit -> 8-bit shift
            if let Some(ns) = neg_shift {
                y_lo = vshlq_s32(y_lo, ns);
                y_hi = vshlq_s32(y_hi, ns);
                cb_lo = vshlq_s32(cb_lo, ns);
                cb_hi = vshlq_s32(cb_hi, ns);
                cr_lo = vshlq_s32(cr_lo, ns);
                cr_hi = vshlq_s32(cr_hi, ns);
            }

            // Fixed-point YCbCr -> RGB
            let yv_lo = vmulq_s32(vsubq_s32(y_lo, y_bias_v), y_scale_v);
            let yv_hi = vmulq_s32(vsubq_s32(y_hi, y_bias_v), y_scale_v);
            let cb_adj_lo = vsubq_s32(cb_lo, bias128_v);
            let cb_adj_hi = vsubq_s32(cb_hi, bias128_v);
            let cr_adj_lo = vsubq_s32(cr_lo, bias128_v);
            let cr_adj_hi = vsubq_s32(cr_hi, bias128_v);

            // R = (yv + cr_r * cr + rnd) >> shr
            let r_lo = vshlq_s32(
                vaddq_s32(vaddq_s32(yv_lo, vmulq_s32(cr_r_v, cr_adj_lo)), rnd_v),
                neg_shr,
            );
            let r_hi = vshlq_s32(
                vaddq_s32(vaddq_s32(yv_hi, vmulq_s32(cr_r_v, cr_adj_hi)), rnd_v),
                neg_shr,
            );

            // G = (yv + cb_g * cb + cr_g * cr + rnd) >> shr
            let g_lo = vshlq_s32(
                vaddq_s32(
                    vaddq_s32(
                        vaddq_s32(yv_lo, vmulq_s32(cb_g_v, cb_adj_lo)),
                        vmulq_s32(cr_g_v, cr_adj_lo),
                    ),
                    rnd_v,
                ),
                neg_shr,
            );
            let g_hi = vshlq_s32(
                vaddq_s32(
                    vaddq_s32(
                        vaddq_s32(yv_hi, vmulq_s32(cb_g_v, cb_adj_hi)),
                        vmulq_s32(cr_g_v, cr_adj_hi),
                    ),
                    rnd_v,
                ),
                neg_shr,
            );

            // B = (yv + cb_b * cb + rnd) >> shr
            let b_lo = vshlq_s32(
                vaddq_s32(vaddq_s32(yv_lo, vmulq_s32(cb_b_v, cb_adj_lo)), rnd_v),
                neg_shr,
            );
            let b_hi = vshlq_s32(
                vaddq_s32(vaddq_s32(yv_hi, vmulq_s32(cb_b_v, cb_adj_hi)), rnd_v),
                neg_shr,
            );

            // Clamp [0, 255]
            let r_lo = vminq_s32(vmaxq_s32(r_lo, zero), max255);
            let r_hi = vminq_s32(vmaxq_s32(r_hi, zero), max255);
            let g_lo = vminq_s32(vmaxq_s32(g_lo, zero), max255);
            let g_hi = vminq_s32(vmaxq_s32(g_hi, zero), max255);
            let b_lo = vminq_s32(vmaxq_s32(b_lo, zero), max255);
            let b_hi = vminq_s32(vmaxq_s32(b_hi, zero), max255);

            // Extract to scalar and write RGB bytes
            // (NEON doesn't have a convenient i32->interleaved-RGB-u8 path like AVX2 shuffle,
            //  so we extract and write pixel by pixel. This is still faster than full scalar
            //  because the computation above is vectorized.)
            let mut r_arr = [0i32; 8];
            let mut g_arr = [0i32; 8];
            let mut b_arr = [0i32; 8];
            vst1q_s32((&mut r_arr[0..4]).try_into().unwrap(), r_lo);
            vst1q_s32((&mut r_arr[4..8]).try_into().unwrap(), r_hi);
            vst1q_s32((&mut g_arr[0..4]).try_into().unwrap(), g_lo);
            vst1q_s32((&mut g_arr[4..8]).try_into().unwrap(), g_hi);
            vst1q_s32((&mut b_arr[0..4]).try_into().unwrap(), b_lo);
            vst1q_s32((&mut b_arr[4..8]).try_into().unwrap(), b_hi);

            for i in 0..8 {
                rgb[out_idx] = r_arr[i] as u8;
                rgb[out_idx + 1] = g_arr[i] as u8;
                rgb[out_idx + 2] = b_arr[i] as u8;
                out_idx += 3;
            }

            x += 8;
        }

        // Scalar tail: remaining 0-7 pixels
        for x in x_simd_end..x_end {
            super::color_convert::scalar_pixel(
                y_plane, cb_plane, cr_plane, y_row, c_row, x as usize, shift, y_bias,
                y_scale, cr_r, cb_g, cr_g, cb_b, rnd, shr, rgb, &mut out_idx,
            );
        }
    }
}
