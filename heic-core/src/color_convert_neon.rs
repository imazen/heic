//! NEON (AArch64) SIMD-accelerated YCbCr->RGB color conversion.
//!
//! Processes 8 pixels per iteration using 128-bit NEON intrinsics.
//! The x86 AVX2 version processes 8 pixels with 256-bit registers;
//! this NEON version achieves the same throughput per iteration since
//! i32 operations on 8 values require 2x int32x4_t per channel.

#![allow(clippy::too_many_arguments)]

use archmage::prelude::*;

#[cfg(target_arch = "aarch64")]
use safe_unaligned_simd::aarch64::{vld1_u16, vld1q_u16, vst1q_s32, vst3q_u8};

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

        // SIMD: 8 pixels per iteration
        let mut x = x_simd_start as usize;
        let x_end_simd = x_simd_end as usize;
        while x < x_end_simd {
            let cx = x / 2;

            // Load 8 Y values (u16) -> zero-extend to 8xi32 (2x int32x4_t)
            let y_raw = vld1q_u16(y_plane[y_row + x..y_row + x + 8].try_into().unwrap());
            let mut y_lo = vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(y_raw)));
            let mut y_hi = vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(y_raw)));

            // Load 4 Cb/Cr values, duplicate each for 4:2:0 -> 8xi32
            let cb_raw = vld1_u16(cb_plane[c_row + cx..c_row + cx + 4].try_into().unwrap());
            let cr_raw = vld1_u16(cr_plane[c_row + cx..c_row + cx + 4].try_into().unwrap());
            // Duplicate each chroma sample: [a,b,c,d] -> [a,a,b,b,c,c,d,d]
            // vzip1/vzip2 on 64-bit registers gives [a,a,b,b] and [c,c,d,d]
            let cb_dup_full = vcombine_u16(vzip1_u16(cb_raw, cb_raw), vzip2_u16(cb_raw, cb_raw));
            let cr_dup_full = vcombine_u16(vzip1_u16(cr_raw, cr_raw), vzip2_u16(cr_raw, cr_raw));

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

/// NEON YCbCr->RGB for 4:4:4 -- 16 pixels per iteration.
///
/// Added 2026-08-01. `convert_444_to_rgb` dispatched `[v3, scalar]` — it had an
/// AVX2 arm but NO NEON arm, so every aarch64 4:4:4 HEIC decode ran the scalar
/// per-pixel loop while x86 got vectorized. The 4:2:0 sibling above always had
/// its NEON arm; 4:4:4 was simply missed.
///
/// Two things this does better than the 4:2:0 kernel above, both possible only
/// because 4:4:4 has no chroma upsampling (each pixel carries its own Cb/Cr, so
/// the loop is a flat 1:1 transform with no duplication step):
///
/// 1. **The store is `vst3q_u8`, not a scalar extract.** The 4:2:0 path writes
///    its result by spilling three `[i32; 8]` arrays and copying pixel by pixel
///    (see its comment: "NEON doesn't have a convenient i32->interleaved-RGB-u8
///    path like AVX2 shuffle"). It does — `vst3q_u8` interleaves three u8x16
///    registers straight to memory. Processing 16 pixels per iteration is what
///    makes that width available.
/// 2. **The clamp is free.** `vqmovun_s32` saturates i32→u16 at [0, 65535] and
///    `vqmovn_u16` saturates u16→u8 at [0, 255]; chained they are exactly
///    `clamp(0, 255)`, so the explicit `vminq_s32`/`vmaxq_s32` pair the 4:2:0
///    path needs disappears into the narrowing that had to happen anyway.
///
/// Bit-exact with `convert_444_to_rgb_scalar` — same i32 arithmetic in the same
/// order, and the saturating narrows are defined to give the same result as the
/// scalar `clamp(0, 255)`. Gated by `neon_444_matches_scalar_exhaustive`.
#[cfg(target_arch = "aarch64")]
#[arcane]
pub(crate) fn convert_444_to_rgb_neon(
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
    let neg_shr = vdupq_n_s32(-shr);
    let neg_shift = vdupq_n_s32(-(shift as i32));
    let needs_shift = shift > 0;

    // One quarter-vector (4 pixels) of the transform. Returns (r, g, b) as
    // unclamped i32 lanes; clamping happens in the narrowing store.
    let lane = |y_s: int32x4_t, cb_s: int32x4_t, cr_s: int32x4_t| {
        let (y_s, cb_s, cr_s) = if needs_shift {
            (
                vshlq_s32(y_s, neg_shift),
                vshlq_s32(cb_s, neg_shift),
                vshlq_s32(cr_s, neg_shift),
            )
        } else {
            (y_s, cb_s, cr_s)
        };
        let yv = vmulq_s32(vsubq_s32(y_s, y_bias_v), y_scale_v);
        let cb_adj = vsubq_s32(cb_s, bias128_v);
        let cr_adj = vsubq_s32(cr_s, bias128_v);
        let r = vshlq_s32(
            vaddq_s32(vaddq_s32(yv, vmulq_s32(cr_r_v, cr_adj)), rnd_v),
            neg_shr,
        );
        let g = vshlq_s32(
            vaddq_s32(
                vaddq_s32(
                    vaddq_s32(yv, vmulq_s32(cb_g_v, cb_adj)),
                    vmulq_s32(cr_g_v, cr_adj),
                ),
                rnd_v,
            ),
            neg_shr,
        );
        let b = vshlq_s32(
            vaddq_s32(vaddq_s32(yv, vmulq_s32(cb_b_v, cb_adj)), rnd_v),
            neg_shr,
        );
        (r, g, b)
    };

    // i32x4 x4 -> u8x16, saturating at each step. This IS the clamp.
    let pack = |a: int32x4_t, b: int32x4_t, c: int32x4_t, d: int32x4_t| {
        let lo = vcombine_u16(vqmovun_s32(a), vqmovun_s32(b));
        let hi = vcombine_u16(vqmovun_s32(c), vqmovun_s32(d));
        vcombine_u8(vqmovn_u16(lo), vqmovn_u16(hi))
    };

    let row_pixels = x_end.saturating_sub(x_start) as usize;
    let simd_count = (row_pixels / 16) * 16;

    let mut out_idx = 0usize;
    for y in y_start..y_end {
        let y_row = y as usize * y_stride;
        let c_row = y as usize * c_stride;

        let mut x = x_start as usize;
        let x_simd_end = x_start as usize + simd_count;
        while x < x_simd_end {
            let y0 = vld1q_u16(y_plane[y_row + x..y_row + x + 8].try_into().unwrap());
            let y1 = vld1q_u16(y_plane[y_row + x + 8..y_row + x + 16].try_into().unwrap());
            let cb0 = vld1q_u16(cb_plane[c_row + x..c_row + x + 8].try_into().unwrap());
            let cb1 = vld1q_u16(cb_plane[c_row + x + 8..c_row + x + 16].try_into().unwrap());
            let cr0 = vld1q_u16(cr_plane[c_row + x..c_row + x + 8].try_into().unwrap());
            let cr1 = vld1q_u16(cr_plane[c_row + x + 8..c_row + x + 16].try_into().unwrap());

            let w = |v: uint16x8_t| {
                (
                    vreinterpretq_s32_u32(vmovl_u16(vget_low_u16(v))),
                    vreinterpretq_s32_u32(vmovl_u16(vget_high_u16(v))),
                )
            };
            let (ya, yb) = w(y0);
            let (yc, yd) = w(y1);
            let (cba, cbb) = w(cb0);
            let (cbc, cbd) = w(cb1);
            let (cra, crb) = w(cr0);
            let (crc, crd) = w(cr1);

            let (r0, g0, b0) = lane(ya, cba, cra);
            let (r1, g1, b1) = lane(yb, cbb, crb);
            let (r2, g2, b2) = lane(yc, cbc, crc);
            let (r3, g3, b3) = lane(yd, cbd, crd);

            let rv = pack(r0, r1, r2, r3);
            let gv = pack(g0, g1, g2, g3);
            let bv = pack(b0, b1, b2, b3);

            vst3q_u8(
                (&mut rgb[out_idx..out_idx + 48]).try_into().unwrap(),
                uint8x16x3_t(rv, gv, bv),
            );
            out_idx += 48;
            x += 16;
        }

        // Scalar tail: the row's remaining `row_pixels % 16` pixels, through
        // the exact same per-pixel reference the scalar tier uses.
        while x < x_end as usize {
            super::color_convert::scalar_pixel_444(
                y_plane,
                cb_plane,
                cr_plane,
                y_row,
                c_row,
                x,
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
            x += 1;
        }
    }
}

#[cfg(all(test, target_arch = "aarch64"))]
mod tests_444_neon {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The NEON 4:4:4 kernel must agree with the scalar tier BIT-FOR-BIT.
    ///
    /// This is a colour path: a one-level disagreement is a wrong pixel, and
    /// per the workspace rule ("if two code paths for the same operation
    /// produce different output, that is a bug in one of them") the tolerance
    /// is zero, not "close enough".
    ///
    /// Widths deliberately straddle the 16-pixel SIMD stride so the scalar
    /// tail is exercised at every remainder 0..=15 — a kernel that is correct
    /// on exact multiples and wrong in the tail is the classic way this breaks.
    #[test]
    fn neon_444_matches_scalar_exhaustive() {
        use archmage::SimdToken;
        let Some(token) = NeonToken::summon() else {
            panic!("aarch64 must have NEON: this test cannot be skipped silently");
        };

        let mut checked = 0usize;
        for width in 1usize..=40 {
            for height in [1usize, 3] {
                for &(shift, full_range, mc) in &[
                    (0u32, false, 1u8),
                    (0, true, 1),
                    (2, false, 1),
                    (2, true, 5),
                    (0, false, 9),
                ] {
                    // Deterministic planes spanning the full input range,
                    // including the extremes that drive the clamp.
                    let n = width * height;
                    let mut s = 0x1234_5678u32;
                    let mut mkplane = |lo: u16, hi: u16| -> Vec<u16> {
                        (0..n)
                            .map(|i| {
                                s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                                match i % 7 {
                                    0 => lo,
                                    1 => hi,
                                    _ => lo + ((s >> 16) as u16 % (hi - lo + 1)),
                                }
                            })
                            .collect()
                    };
                    let top: u16 = if shift > 0 { 1023 } else { 255 };
                    let yp = mkplane(0, top);
                    let cbp = mkplane(0, top);
                    let crp = mkplane(0, top);

                    let mut got = vec![0u8; n * 3];
                    let mut want = vec![0u8; n * 3];

                    convert_444_to_rgb_neon(
                        token,
                        &yp,
                        &cbp,
                        &crp,
                        width,
                        width,
                        0,
                        height as u32,
                        0,
                        width as u32,
                        shift,
                        full_range,
                        mc,
                        &mut got,
                    );
                    // The SCALAR tier explicitly — NOT `convert_444_to_rgb`,
                    // which now dispatches to NEON on aarch64 and would make
                    // this compare the new kernel against itself. That version
                    // of this test passed before the reference was corrected,
                    // which is exactly what a vacuous gate looks like.
                    super::super::color_convert::convert_444_to_rgb_scalar(
                        ScalarToken,
                        &yp,
                        &cbp,
                        &crp,
                        width,
                        width,
                        0,
                        height as u32,
                        0,
                        width as u32,
                        shift,
                        full_range,
                        mc,
                        &mut want,
                    );

                    assert_eq!(
                        got, want,
                        "NEON 4:4:4 diverges from scalar at width={width} height={height} \
                         shift={shift} full_range={full_range} matrix_coeffs={mc}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(
            checked >= 400,
            "expected a wide sweep, only ran {checked} cases"
        );
    }
}
