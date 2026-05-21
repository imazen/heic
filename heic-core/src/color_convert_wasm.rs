//! WebAssembly SIMD128 YCbCr→RGB color conversion.
//!
//! Processes 8 pixels per outer iteration using 128-bit `v128` lanes,
//! mirroring the AArch64 NEON layout: two `i32x4` halves per 8-pixel
//! tile. The math matches `convert_420_to_rgb_scalar` bit-for-bit.

#![allow(clippy::too_many_arguments)]

#[cfg(target_arch = "wasm32")]
use archmage::prelude::*;

#[cfg(target_arch = "wasm32")]
use safe_unaligned_simd::wasm32::{u32x4_load_extend_u16x4, v128_load, v128_store};

/// WASM128 YCbCr→RGB conversion — 8 pixels per iteration.
#[cfg(target_arch = "wasm32")]
#[arcane]
pub(crate) fn convert_420_to_rgb_wasm128(
    _token: Wasm128Token,
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

    let cr_r_v = i32x4_splat(cr_r);
    let cb_g_v = i32x4_splat(cb_g);
    let cr_g_v = i32x4_splat(cr_g);
    let cb_b_v = i32x4_splat(cb_b);
    let y_bias_v = i32x4_splat(y_bias);
    let y_scale_v = i32x4_splat(y_scale);
    let rnd_v = i32x4_splat(rnd);
    let bias128_v = i32x4_splat(128);
    let zero_v = i32x4_splat(0);
    let max255_v = i32x4_splat(255);
    let shr_u = shr as u32;
    let shift_u = shift;

    let x_simd_start = x_start.next_multiple_of(2);
    let row_pixels = x_end.saturating_sub(x_simd_start) as usize;
    let simd_count = (row_pixels / 8) * 8;
    let x_simd_end = x_simd_start + simd_count as u32;

    let mut out_idx = 0;

    for y in y_start..y_end {
        let y_row = y as usize * y_stride;
        let c_row = (y as usize / 2) * c_stride;

        // Scalar prefix
        for x in x_start..x_simd_start.min(x_end) {
            super::color_convert::scalar_pixel(
                y_plane,
                cb_plane,
                cr_plane,
                y_row,
                c_row,
                x as usize,
                shift,
                y_bias,
                y_scale,
                cr_r,
                cb_g,
                cr_g,
                cb_b,
                rnd,
                shr,
                rgb,
                &mut out_idx,
            );
        }

        let mut x = x_simd_start as usize;
        let x_end_simd = x_simd_end as usize;
        while x < x_end_simd {
            let cx = x / 2;

            // Load 8 Y values (u16) — widen to two i32x4
            let y_raw: v128 =
                v128_load::<[u16; 8]>(y_plane[y_row + x..y_row + x + 8].try_into().unwrap());
            let mut y_lo = u32x4_extend_low_u16x8(y_raw);
            let mut y_hi = u32x4_extend_high_u16x8(y_raw);

            // Load 4 Cb/Cr u16s, zero-extend to i32x4. Duplicate each lane
            // for 4:2:0 by selecting low/high halves via shuffle on i32x4.
            let cb_v: v128 = u32x4_load_extend_u16x4::<[u16; 4]>(
                cb_plane[c_row + cx..c_row + cx + 4].try_into().unwrap(),
            );
            let cr_v: v128 = u32x4_load_extend_u16x4::<[u16; 4]>(
                cr_plane[c_row + cx..c_row + cx + 4].try_into().unwrap(),
            );
            // cb_v = [a, b, c, d]; produce [a,a,b,b] and [c,c,d,d] as i32x4
            let mut cb_lo = i32x4_shuffle::<0, 0, 1, 1>(cb_v, cb_v);
            let mut cb_hi = i32x4_shuffle::<2, 2, 3, 3>(cb_v, cb_v);
            let mut cr_lo = i32x4_shuffle::<0, 0, 1, 1>(cr_v, cr_v);
            let mut cr_hi = i32x4_shuffle::<2, 2, 3, 3>(cr_v, cr_v);

            // 10-bit -> 8-bit shift (logical right shift on positive values)
            if shift_u > 0 {
                y_lo = u32x4_shr(y_lo, shift_u);
                y_hi = u32x4_shr(y_hi, shift_u);
                cb_lo = u32x4_shr(cb_lo, shift_u);
                cb_hi = u32x4_shr(cb_hi, shift_u);
                cr_lo = u32x4_shr(cr_lo, shift_u);
                cr_hi = u32x4_shr(cr_hi, shift_u);
            }

            // YCbCr → RGB (fixed-point, signed i32 from here on)
            let yv_lo = i32x4_mul(i32x4_sub(y_lo, y_bias_v), y_scale_v);
            let yv_hi = i32x4_mul(i32x4_sub(y_hi, y_bias_v), y_scale_v);
            let cb_adj_lo = i32x4_sub(cb_lo, bias128_v);
            let cb_adj_hi = i32x4_sub(cb_hi, bias128_v);
            let cr_adj_lo = i32x4_sub(cr_lo, bias128_v);
            let cr_adj_hi = i32x4_sub(cr_hi, bias128_v);

            // R = (yv + cr_r * cr + rnd) >> shr
            let r_lo = i32x4_shr(
                i32x4_add(i32x4_add(yv_lo, i32x4_mul(cr_r_v, cr_adj_lo)), rnd_v),
                shr_u,
            );
            let r_hi = i32x4_shr(
                i32x4_add(i32x4_add(yv_hi, i32x4_mul(cr_r_v, cr_adj_hi)), rnd_v),
                shr_u,
            );

            // G = (yv + cb_g * cb + cr_g * cr + rnd) >> shr
            let g_lo = i32x4_shr(
                i32x4_add(
                    i32x4_add(
                        i32x4_add(yv_lo, i32x4_mul(cb_g_v, cb_adj_lo)),
                        i32x4_mul(cr_g_v, cr_adj_lo),
                    ),
                    rnd_v,
                ),
                shr_u,
            );
            let g_hi = i32x4_shr(
                i32x4_add(
                    i32x4_add(
                        i32x4_add(yv_hi, i32x4_mul(cb_g_v, cb_adj_hi)),
                        i32x4_mul(cr_g_v, cr_adj_hi),
                    ),
                    rnd_v,
                ),
                shr_u,
            );

            // B = (yv + cb_b * cb + rnd) >> shr
            let b_lo = i32x4_shr(
                i32x4_add(i32x4_add(yv_lo, i32x4_mul(cb_b_v, cb_adj_lo)), rnd_v),
                shr_u,
            );
            let b_hi = i32x4_shr(
                i32x4_add(i32x4_add(yv_hi, i32x4_mul(cb_b_v, cb_adj_hi)), rnd_v),
                shr_u,
            );

            // Clamp [0, 255]
            let r_lo = i32x4_min(i32x4_max(r_lo, zero_v), max255_v);
            let r_hi = i32x4_min(i32x4_max(r_hi, zero_v), max255_v);
            let g_lo = i32x4_min(i32x4_max(g_lo, zero_v), max255_v);
            let g_hi = i32x4_min(i32x4_max(g_hi, zero_v), max255_v);
            let b_lo = i32x4_min(i32x4_max(b_lo, zero_v), max255_v);
            let b_hi = i32x4_min(i32x4_max(b_hi, zero_v), max255_v);

            // Extract and interleave to RGB bytes. Wasm128 has no AVX-style
            // shuffle that lays out R G B for free, so spill and write.
            let mut r_arr = [0i32; 8];
            let mut g_arr = [0i32; 8];
            let mut b_arr = [0i32; 8];
            v128_store::<[i32; 4]>((&mut r_arr[0..4]).try_into().unwrap(), r_lo);
            v128_store::<[i32; 4]>((&mut r_arr[4..8]).try_into().unwrap(), r_hi);
            v128_store::<[i32; 4]>((&mut g_arr[0..4]).try_into().unwrap(), g_lo);
            v128_store::<[i32; 4]>((&mut g_arr[4..8]).try_into().unwrap(), g_hi);
            v128_store::<[i32; 4]>((&mut b_arr[0..4]).try_into().unwrap(), b_lo);
            v128_store::<[i32; 4]>((&mut b_arr[4..8]).try_into().unwrap(), b_hi);

            for i in 0..8 {
                rgb[out_idx] = r_arr[i] as u8;
                rgb[out_idx + 1] = g_arr[i] as u8;
                rgb[out_idx + 2] = b_arr[i] as u8;
                out_idx += 3;
            }

            x += 8;
        }

        // Scalar tail
        for x in x_simd_end..x_end {
            super::color_convert::scalar_pixel(
                y_plane,
                cb_plane,
                cr_plane,
                y_row,
                c_row,
                x as usize,
                shift,
                y_bias,
                y_scale,
                cr_r,
                cb_g,
                cr_g,
                cb_b,
                rnd,
                shr,
                rgb,
                &mut out_idx,
            );
        }
    }
}
