//! NEON (AArch64) SIMD implementations of HEVC inverse transforms and utilities.
//!
//! Provides NEON paths for:
//! - IDST 4x4 (native NEON intrinsics)
//! - IDCT 8x8 (native NEON intrinsics)
//! - IDCT 16x16 (scalar delegation — 256-bit butterfly too complex for initial port)
//! - IDCT 32x32 (scalar delegation — same reason)
//! - add_residual_block (native NEON)
//! - dequantize (native NEON)

// The `#[arcane]` macro generates multiple function variants for SIMD dispatch;
// the allow attribute on individual functions does not propagate to generated code.
#![allow(clippy::too_many_arguments)]

use archmage::prelude::*;

#[cfg(target_arch = "aarch64")]
use safe_unaligned_simd::aarch64::{vld1q_s16, vld1q_u16, vst1q_s16, vst1q_u16};

// =============================================================================
// 4x4 IDST — native NEON (128-bit, maps perfectly)
// =============================================================================

/// NEON inverse DST 4x4: processes all 4 columns in parallel using 128-bit SIMD.
///
/// Two-pass approach identical to the SSE4.1 version:
/// - Pass 1 (vertical): load 4 rows as i32, multiply by DST4 coefficients, shift/clamp
/// - Transpose 4x4 i16 matrix
/// - Pass 2 (horizontal): same multiply pattern, pack and store
#[cfg(target_arch = "aarch64")]
#[arcane]
pub(crate) fn idst4_neon(
    _token: NeonToken,
    coeffs: &[i16; 16],
    output: &mut [i16; 16],
    bit_depth: u8,
) {
    // Load all 16 i16 coefficients as 4 rows of 4 i32 values
    let load01 = vld1q_s16(coeffs[0..8].try_into().unwrap());
    let load23 = vld1q_s16(coeffs[8..16].try_into().unwrap());
    // Sign-extend low/high halves to i32
    let row0 = vmovl_s16(vget_low_s16(load01));
    let row1 = vmovl_s16(vget_high_s16(load01));
    let row2 = vmovl_s16(vget_low_s16(load23));
    let row3 = vmovl_s16(vget_high_s16(load23));

    // === Pass 1 (vertical): DST4^T x COEFFS, 4 columns in parallel ===
    let add1 = vdupq_n_s32(64); // 1 << (7 - 1)

    // j=0: 29*r0 + 74*r1 + 84*r2 + 55*r3
    let t0 = vshrq_n_s32::<7>(vaddq_s32(
        vaddq_s32(
            vaddq_s32(vmulq_n_s32(row0, 29), vmulq_n_s32(row1, 74)),
            vaddq_s32(vmulq_n_s32(row2, 84), vmulq_n_s32(row3, 55)),
        ),
        add1,
    ));

    // j=1: 55*r0 + 74*r1 - 29*r2 - 84*r3
    let t1 = vshrq_n_s32::<7>(vaddq_s32(
        vaddq_s32(
            vaddq_s32(vmulq_n_s32(row0, 55), vmulq_n_s32(row1, 74)),
            vaddq_s32(vmulq_n_s32(row2, -29), vmulq_n_s32(row3, -84)),
        ),
        add1,
    ));

    // j=2: 74*(r0 - r2 + r3)
    let t2 = vshrq_n_s32::<7>(vaddq_s32(
        vmulq_n_s32(vaddq_s32(vsubq_s32(row0, row2), row3), 74),
        add1,
    ));

    // j=3: 84*r0 - 74*r1 + 55*r2 - 29*r3
    let t3 = vshrq_n_s32::<7>(vaddq_s32(
        vaddq_s32(
            vaddq_s32(vmulq_n_s32(row0, 84), vmulq_n_s32(row1, -74)),
            vaddq_s32(vmulq_n_s32(row2, 55), vmulq_n_s32(row3, -29)),
        ),
        add1,
    ));

    // Clamp to i16 range: vqmovn_s32 saturates i32 -> i16
    let packed01 = vcombine_s16(vqmovn_s32(t0), vqmovn_s32(t1));
    let packed23 = vcombine_s16(vqmovn_s32(t2), vqmovn_s32(t3));

    // Transpose 4x4 i16 matrix for pass 2
    let a = vzip1q_s16(packed01, packed23);
    let b = vzip2q_s16(packed01, packed23);
    let tp_lo = vzip1q_s16(a, b);
    let tp_hi = vzip2q_s16(a, b);

    // === Pass 2 (horizontal): DST4^T x TMP^T ===
    let r0 = vmovl_s16(vget_low_s16(tp_lo));
    let r1 = vmovl_s16(vget_high_s16(tp_lo));
    let r2 = vmovl_s16(vget_low_s16(tp_hi));
    let r3 = vmovl_s16(vget_high_s16(tp_hi));

    let shift2 = 20 - bit_depth as i32;
    let add2 = vdupq_n_s32(1i32 << (shift2 - 1));
    let neg_shift2 = vdupq_n_s32(-shift2);

    let o0 = vshlq_s32(
        vaddq_s32(
            vaddq_s32(
                vaddq_s32(vmulq_n_s32(r0, 29), vmulq_n_s32(r1, 74)),
                vaddq_s32(vmulq_n_s32(r2, 84), vmulq_n_s32(r3, 55)),
            ),
            add2,
        ),
        neg_shift2,
    );

    let o1 = vshlq_s32(
        vaddq_s32(
            vaddq_s32(
                vaddq_s32(vmulq_n_s32(r0, 55), vmulq_n_s32(r1, 74)),
                vaddq_s32(vmulq_n_s32(r2, -29), vmulq_n_s32(r3, -84)),
            ),
            add2,
        ),
        neg_shift2,
    );

    let o2 = vshlq_s32(
        vaddq_s32(vmulq_n_s32(vaddq_s32(vsubq_s32(r0, r2), r3), 74), add2),
        neg_shift2,
    );

    let o3 = vshlq_s32(
        vaddq_s32(
            vaddq_s32(
                vaddq_s32(vmulq_n_s32(r0, 84), vmulq_n_s32(r1, -74)),
                vaddq_s32(vmulq_n_s32(r2, 55), vmulq_n_s32(r3, -29)),
            ),
            add2,
        ),
        neg_shift2,
    );

    // Pack to i16 and transpose back to row-major
    let out01 = vcombine_s16(vqmovn_s32(o0), vqmovn_s32(o1));
    let out23 = vcombine_s16(vqmovn_s32(o2), vqmovn_s32(o3));

    let a = vzip1q_s16(out01, out23);
    let b = vzip2q_s16(out01, out23);
    let final_lo = vzip1q_s16(a, b);
    let final_hi = vzip2q_s16(a, b);

    vst1q_s16((&mut output[0..8]).try_into().unwrap(), final_lo);
    vst1q_s16((&mut output[8..16]).try_into().unwrap(), final_hi);
}

// =============================================================================
// 8x8 IDCT — native NEON (128-bit registers, same width as SSE)
// =============================================================================

/// Transpose an 8x8 matrix of i16 values stored in 8 int16x8_t registers.
#[cfg(target_arch = "aarch64")]
#[rite]
fn transpose_8x8_neon(
    _token: NeonToken,
    r0: int16x8_t,
    r1: int16x8_t,
    r2: int16x8_t,
    r3: int16x8_t,
    r4: int16x8_t,
    r5: int16x8_t,
    r6: int16x8_t,
    r7: int16x8_t,
) -> (
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
) {
    // Phase 1: interleave 16-bit pairs
    let t0 = vzip1q_s16(r0, r1);
    let t1 = vzip2q_s16(r0, r1);
    let t2 = vzip1q_s16(r2, r3);
    let t3 = vzip2q_s16(r2, r3);
    let t4 = vzip1q_s16(r4, r5);
    let t5 = vzip2q_s16(r4, r5);
    let t6 = vzip1q_s16(r6, r7);
    let t7 = vzip2q_s16(r6, r7);

    // Phase 2: interleave 32-bit pairs (reinterpret as s32)
    let t0_32 = vreinterpretq_s32_s16(t0);
    let t2_32 = vreinterpretq_s32_s16(t2);
    let t1_32 = vreinterpretq_s32_s16(t1);
    let t3_32 = vreinterpretq_s32_s16(t3);
    let t4_32 = vreinterpretq_s32_s16(t4);
    let t6_32 = vreinterpretq_s32_s16(t6);
    let t5_32 = vreinterpretq_s32_s16(t5);
    let t7_32 = vreinterpretq_s32_s16(t7);

    let u0 = vreinterpretq_s16_s32(vzip1q_s32(t0_32, t2_32));
    let u1 = vreinterpretq_s16_s32(vzip2q_s32(t0_32, t2_32));
    let u2 = vreinterpretq_s16_s32(vzip1q_s32(t1_32, t3_32));
    let u3 = vreinterpretq_s16_s32(vzip2q_s32(t1_32, t3_32));
    let u4 = vreinterpretq_s16_s32(vzip1q_s32(t4_32, t6_32));
    let u5 = vreinterpretq_s16_s32(vzip2q_s32(t4_32, t6_32));
    let u6 = vreinterpretq_s16_s32(vzip1q_s32(t5_32, t7_32));
    let u7 = vreinterpretq_s16_s32(vzip2q_s32(t5_32, t7_32));

    // Phase 3: interleave 64-bit pairs (reinterpret as s64)
    let u0_64 = vreinterpretq_s64_s16(u0);
    let u4_64 = vreinterpretq_s64_s16(u4);
    let u1_64 = vreinterpretq_s64_s16(u1);
    let u5_64 = vreinterpretq_s64_s16(u5);
    let u2_64 = vreinterpretq_s64_s16(u2);
    let u6_64 = vreinterpretq_s64_s16(u6);
    let u3_64 = vreinterpretq_s64_s16(u3);
    let u7_64 = vreinterpretq_s64_s16(u7);

    (
        vreinterpretq_s16_s64(vzip1q_s64(u0_64, u4_64)),
        vreinterpretq_s16_s64(vzip2q_s64(u0_64, u4_64)),
        vreinterpretq_s16_s64(vzip1q_s64(u1_64, u5_64)),
        vreinterpretq_s16_s64(vzip2q_s64(u1_64, u5_64)),
        vreinterpretq_s16_s64(vzip1q_s64(u2_64, u6_64)),
        vreinterpretq_s16_s64(vzip2q_s64(u2_64, u6_64)),
        vreinterpretq_s16_s64(vzip1q_s64(u3_64, u7_64)),
        vreinterpretq_s16_s64(vzip2q_s64(u3_64, u7_64)),
    )
}

/// NEON equivalent of the x86 IDCT8 interleave+madd pattern.
///
/// The x86 version interleaves two rows into a __m256i and uses madd_epi16.
/// For NEON we do: interleave the two rows (zip), then widening multiply + pairwise add.
///
/// Takes two i16x8 rows and two coefficients. Returns two int32x4_t (lo, hi halves).
#[cfg(target_arch = "aarch64")]
#[rite]
fn interleave_madd_neon(
    _token: NeonToken,
    ra: int16x8_t,
    rb: int16x8_t,
    ca: i16,
    cb: i16,
) -> (int32x4_t, int32x4_t) {
    // Interleave: zip1/zip2 produce [a0,b0,a1,b1,...] and [a4,b4,a5,b5,...]
    let interleaved_lo = vzip1q_s16(ra, rb);
    let interleaved_hi = vzip2q_s16(ra, rb);

    // Create coefficient vector: [ca, cb, ca, cb, ca, cb, ca, cb]
    let coeff_lo = vreinterpretq_s16_s32(vdupq_n_s32((ca as i32 & 0xFFFF) | ((cb as i32) << 16)));

    // Widening multiply + pairwise add for each half
    let prod_lo_lo = vmull_s16(vget_low_s16(interleaved_lo), vget_low_s16(coeff_lo));
    let prod_lo_hi = vmull_s16(vget_high_s16(interleaved_lo), vget_high_s16(coeff_lo));
    let prod_hi_lo = vmull_s16(vget_low_s16(interleaved_hi), vget_low_s16(coeff_lo));
    let prod_hi_hi = vmull_s16(vget_high_s16(interleaved_hi), vget_high_s16(coeff_lo));

    let lo = vpaddq_s32(prod_lo_lo, prod_lo_hi);
    let hi = vpaddq_s32(prod_hi_lo, prod_hi_hi);

    (lo, hi)
}

/// 8-point 1D IDCT on 8 columns simultaneously using NEON.
///
/// Input: 8 rows as int16x8_t (each row = 8 i16 values, one per column).
/// Output: 8 transformed rows as int16x8_t (i16, clamped via saturating narrowing).
#[cfg(target_arch = "aarch64")]
#[rite]
fn idct8_1d_columns_neon(
    _token: NeonToken,
    r0: int16x8_t,
    r1: int16x8_t,
    r2: int16x8_t,
    r3: int16x8_t,
    r4: int16x8_t,
    r5: int16x8_t,
    r6: int16x8_t,
    r7: int16x8_t,
    shift: i32,
    add: int32x4_t,
) -> (
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
    int16x8_t,
) {
    let neg_shift = vdupq_n_s32(-shift);

    // Odd part using interleave+madd pattern
    let (o0_13l, o0_13h) = interleave_madd_neon(_token, r1, r3, 89, 75);
    let (o0_57l, o0_57h) = interleave_madd_neon(_token, r5, r7, 50, 18);
    let o0l = vaddq_s32(o0_13l, o0_57l);
    let o0h = vaddq_s32(o0_13h, o0_57h);

    let (o1_13l, o1_13h) = interleave_madd_neon(_token, r1, r3, 75, -18);
    let (o1_57l, o1_57h) = interleave_madd_neon(_token, r5, r7, -89, -50);
    let o1l = vaddq_s32(o1_13l, o1_57l);
    let o1h = vaddq_s32(o1_13h, o1_57h);

    let (o2_13l, o2_13h) = interleave_madd_neon(_token, r1, r3, 50, -89);
    let (o2_57l, o2_57h) = interleave_madd_neon(_token, r5, r7, 18, 75);
    let o2l = vaddq_s32(o2_13l, o2_57l);
    let o2h = vaddq_s32(o2_13h, o2_57h);

    let (o3_13l, o3_13h) = interleave_madd_neon(_token, r1, r3, 18, -50);
    let (o3_57l, o3_57h) = interleave_madd_neon(_token, r5, r7, 75, -89);
    let o3l = vaddq_s32(o3_13l, o3_57l);
    let o3h = vaddq_s32(o3_13h, o3_57h);

    // Even part
    let (ee0l, ee0h) = interleave_madd_neon(_token, r0, r4, 64, 64);
    let (ee1l, ee1h) = interleave_madd_neon(_token, r0, r4, 64, -64);
    let (eo0l, eo0h) = interleave_madd_neon(_token, r2, r6, 83, 36);
    let (eo1l, eo1h) = interleave_madd_neon(_token, r2, r6, 36, -83);

    let e0l = vaddq_s32(ee0l, eo0l);
    let e0h = vaddq_s32(ee0h, eo0h);
    let e1l = vaddq_s32(ee1l, eo1l);
    let e1h = vaddq_s32(ee1h, eo1h);
    let e2l = vsubq_s32(ee1l, eo1l);
    let e2h = vsubq_s32(ee1h, eo1h);
    let e3l = vsubq_s32(ee0l, eo0l);
    let e3h = vsubq_s32(ee0h, eo0h);

    // Butterfly + round + shift + pack
    macro_rules! butterfly_pack {
        ($el:expr, $eh:expr, $ol:expr, $oh:expr, add) => {{
            let dl = vshlq_s32(vaddq_s32(vaddq_s32($el, $ol), add), neg_shift);
            let dh = vshlq_s32(vaddq_s32(vaddq_s32($eh, $oh), add), neg_shift);
            vcombine_s16(vqmovn_s32(dl), vqmovn_s32(dh))
        }};
        ($el:expr, $eh:expr, $ol:expr, $oh:expr, sub) => {{
            let dl = vshlq_s32(vaddq_s32(vsubq_s32($el, $ol), add), neg_shift);
            let dh = vshlq_s32(vaddq_s32(vsubq_s32($eh, $oh), add), neg_shift);
            vcombine_s16(vqmovn_s32(dl), vqmovn_s32(dh))
        }};
    }

    (
        butterfly_pack!(e0l, e0h, o0l, o0h, add),
        butterfly_pack!(e1l, e1h, o1l, o1h, add),
        butterfly_pack!(e2l, e2h, o2l, o2h, add),
        butterfly_pack!(e3l, e3h, o3l, o3h, add),
        butterfly_pack!(e3l, e3h, o3l, o3h, sub),
        butterfly_pack!(e2l, e2h, o2l, o2h, sub),
        butterfly_pack!(e1l, e1h, o1l, o1h, sub),
        butterfly_pack!(e0l, e0h, o0l, o0h, sub),
    )
}

/// NEON 8x8 IDCT entry point
#[arcane]
pub(crate) fn idct8_neon(
    _token: NeonToken,
    coeffs: &[i16; 64],
    output: &mut [i16; 64],
    bit_depth: u8,
) {
    // Load 8 input rows
    let r0 = vld1q_s16(coeffs[0..8].try_into().unwrap());
    let r1 = vld1q_s16(coeffs[8..16].try_into().unwrap());
    let r2 = vld1q_s16(coeffs[16..24].try_into().unwrap());
    let r3 = vld1q_s16(coeffs[24..32].try_into().unwrap());
    let r4 = vld1q_s16(coeffs[32..40].try_into().unwrap());
    let r5 = vld1q_s16(coeffs[40..48].try_into().unwrap());
    let r6 = vld1q_s16(coeffs[48..56].try_into().unwrap());
    let r7 = vld1q_s16(coeffs[56..64].try_into().unwrap());

    // Pass 1: vertical (column transform), shift = 7
    let add1 = vdupq_n_s32(1 << 6); // 1 << (7-1)
    let (d0, d1, d2, d3, d4, d5, d6, d7) =
        idct8_1d_columns_neon(_token, r0, r1, r2, r3, r4, r5, r6, r7, 7, add1);

    // Transpose for horizontal pass
    let (t0, t1, t2, t3, t4, t5, t6, t7) =
        transpose_8x8_neon(_token, d0, d1, d2, d3, d4, d5, d6, d7);

    // Pass 2: horizontal (row transform), shift = 20 - bit_depth
    let shift2 = 20 - bit_depth as i32;
    let add2 = vdupq_n_s32(1 << (shift2 - 1));
    let (e0, e1, e2, e3, e4, e5, e6, e7) =
        idct8_1d_columns_neon(_token, t0, t1, t2, t3, t4, t5, t6, t7, shift2, add2);

    // Transpose back for row-major storage
    let (f0, f1, f2, f3, f4, f5, f6, f7) =
        transpose_8x8_neon(_token, e0, e1, e2, e3, e4, e5, e6, e7);

    // Store 8 output rows
    vst1q_s16((&mut output[0..8]).try_into().unwrap(), f0);
    vst1q_s16((&mut output[8..16]).try_into().unwrap(), f1);
    vst1q_s16((&mut output[16..24]).try_into().unwrap(), f2);
    vst1q_s16((&mut output[24..32]).try_into().unwrap(), f3);
    vst1q_s16((&mut output[32..40]).try_into().unwrap(), f4);
    vst1q_s16((&mut output[40..48]).try_into().unwrap(), f5);
    vst1q_s16((&mut output[48..56]).try_into().unwrap(), f6);
    vst1q_s16((&mut output[56..64]).try_into().unwrap(), f7);
}

// =============================================================================
// 16x16 IDCT — delegates to scalar (256-bit butterfly too complex for initial port)
// =============================================================================

/// NEON 16x16 IDCT — delegates to scalar implementation.
///
/// A native NEON port would process 16-wide rows as 2x int16x8_t with separate
/// lo/hi butterfly chains. Deferred to a follow-up optimization pass.
pub(crate) fn idct16_neon(
    _token: NeonToken,
    coeffs: &[i16; 256],
    output: &mut [i16; 256],
    bit_depth: u8,
) {
    super::transform::idct16_inner(coeffs, output, bit_depth);
}

// =============================================================================
// 32x32 IDCT — delegates to scalar (same rationale)
// =============================================================================

/// NEON 32x32 IDCT — delegates to scalar implementation.
pub(crate) fn idct32_neon(
    _token: NeonToken,
    coeffs: &[i16; 1024],
    output: &mut [i16; 1024],
    bit_depth: u8,
) {
    super::transform::idct32_inner(coeffs, output, bit_depth);
}

// =============================================================================
// NEON residual add: u16 prediction + i16 residual -> clamped u16
// =============================================================================

/// Add i16 residual block to u16 prediction block with clamping to [0, max_val].
/// Processes full block (multiple rows) with NEON 128-bit operations (8 u16 per iteration).
#[cfg(target_arch = "aarch64")]
#[arcane]
pub(crate) fn add_residual_block_neon(
    _token: NeonToken,
    plane: &mut [u16],
    stride: usize,
    x0: usize,
    y0: usize,
    residual: &[i16],
    size: usize,
    max_val: i32,
) {
    // Use signed i16 operations: pred values are in [0, max_val] which fits in i16
    // for typical bit depths (8-bit: max_val=255, 10-bit: max_val=1023).
    let zero = vdupq_n_s16(0);
    let max_v = vdupq_n_s16(max_val as i16);

    for py in 0..size {
        let row_start = (y0 + py) * stride + x0;
        let row = &mut plane[row_start..row_start + size];
        let res_row = &residual[py * size..(py + 1) * size];

        let chunks = size / 8;
        for c in 0..chunks {
            let offset = c * 8;
            // Load prediction as u16, reinterpret as i16 (values fit in i16 for <=15-bit)
            let pred_u = vld1q_u16(row[offset..offset + 8].try_into().unwrap());
            let pred = vreinterpretq_s16_u16(pred_u);
            let res = vld1q_s16(res_row[offset..offset + 8].try_into().unwrap());
            // Add and clamp with signed operations (handles negative results correctly)
            let sum = vaddq_s16(pred, res);
            let clamped = vminq_s16(vmaxq_s16(sum, zero), max_v);
            // Store back as u16
            let clamped_u = vreinterpretq_u16_s16(clamped);
            vst1q_u16(
                (&mut row[offset..offset + 8]).try_into().unwrap(),
                clamped_u,
            );
        }
        // Scalar remainder
        for i in (chunks * 8)..size {
            let pred = row[i] as i32;
            let r = res_row[i] as i32;
            row[i] = (pred + r).clamp(0, max_val) as u16;
        }
    }
}

// =============================================================================
// NEON dequantize: i16 x const -> i16 with shift and clamp
// =============================================================================

/// Dequantize i16 coefficients using NEON.
/// Processes 8 coefficients per iteration (128-bit NEON).
#[cfg(target_arch = "aarch64")]
#[arcane]
pub(crate) fn dequantize_neon(
    _token: NeonToken,
    coeffs: &mut [i16],
    combined_scale: i32,
    shift: i32,
    add: i32,
) {
    let scale_v = vdupq_n_s32(combined_scale);
    let add_v = vdupq_n_s32(add);
    let neg_shift = vdupq_n_s32(-shift);

    let chunks = coeffs.len() / 8;
    for c in 0..chunks {
        let offset = c * 8;
        let src = vld1q_s16(coeffs[offset..offset + 8].try_into().unwrap());

        // Widen low/high 4 i16 to i32
        let lo_32 = vmovl_s16(vget_low_s16(src));
        let hi_32 = vmovl_s16(vget_high_s16(src));

        // Multiply by combined_scale
        let prod_lo = vmulq_s32(lo_32, scale_v);
        let prod_hi = vmulq_s32(hi_32, scale_v);

        // Add rounding and shift right (negative shift = right shift in vshlq)
        let shifted_lo = vshlq_s32(vaddq_s32(prod_lo, add_v), neg_shift);
        let shifted_hi = vshlq_s32(vaddq_s32(prod_hi, add_v), neg_shift);

        // Pack back to i16 with saturation
        let result = vcombine_s16(vqmovn_s32(shifted_lo), vqmovn_s32(shifted_hi));

        vst1q_s16(
            (&mut coeffs[offset..offset + 8]).try_into().unwrap(),
            result,
        );
    }

    // Scalar remainder
    for coef in coeffs.iter_mut().skip(chunks * 8) {
        let value = (*coef as i32 * combined_scale + add) >> shift;
        *coef = value.clamp(-32768, 32767) as i16;
    }
}
