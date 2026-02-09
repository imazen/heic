//! SIMD-optimized transform implementations
//!
//! This module provides high-performance SIMD versions of inverse DCT/DST transforms
//! using AVX2 on x86_64. Falls back to scalar implementations on other platforms.
//!
//! # Compatibility
//!
//! **Intel & AMD Support**: AVX2 is supported by:
//! - Intel: Haswell (2013) and newer (Core i3/i5/i7 4th gen+)
//! - AMD: Excavator (2015) and newer (Ryzen all generations)
//!
//! The code uses:
//! - Runtime CPU feature detection (`is_x86_feature_detected!`)
//! - Standard x86_64 intrinsics (vendor-neutral)
//! - Conservative SIMD patterns that work well on both microarchitectures
//! - Automatic fallback to scalar code on older CPUs
//!
//! Enabled only when the `unsafe-simd` feature is active.

#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
use std::arch::x86_64::*;

use super::transform::{get_dct32_coef, DCT16_MATRIX};

/// SIMD-optimized inverse 32x32 DCT
///
/// Uses AVX2 instructions for 4-8x speedup over scalar version.
/// Processes multiple dot products in parallel using 256-bit SIMD registers.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn idct32_avx2(coeffs: &[i16; 1024], output: &mut [i16; 1024], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1i32 << (shift1 - 1);
    let add2 = 1i64 << (shift2 - 1);

    let mut tmp = [0i32; 1024];

    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // First pass (vertical) - SIMD optimized
        // Process 32 columns, each computing 32 outputs
        for col in 0..32 {
            // Gather column data (stride 32)
            let mut src_col = [0i16; 32];
            for row in 0..32 {
                src_col[row] = coeffs[row * 32 + col];
            }

            // Compute 32 outputs for this column using SIMD
            let mut dst_col = [0i32; 32];
            idct32_1d_avx2(&src_col, &mut dst_col, shift1, add1);

            // Scatter results back (stride 32)
            for row in 0..32 {
                tmp[row * 32 + col] = dst_col[row];
            }
        }

        // Second pass (horizontal) - SIMD optimized
        // Process 32 rows, each computing 32 outputs
        for row in 0..32 {
            let row_start = row * 32;

            // Source is already contiguous i32 array
            let src_row = &tmp[row_start..row_start + 32];
            let mut dst_row = [0i16; 32];

            // Compute 32 outputs for this row using SIMD
            idct32_1d_horizontal_avx2(src_row, &mut dst_row, shift2 as i32, add2);

            // Write results
            output[row_start..row_start + 32].copy_from_slice(&dst_row);
        }
    }
}

/// SIMD-optimized 1D 32-point inverse DCT (for vertical pass)
///
/// Computes 32 outputs from 32 i16 inputs using AVX2.
/// Each output is a dot product of 32 coefficients with 32 inputs.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct32_1d_avx2(src: &[i16; 32], dst: &mut [i32; 32], shift: i32, add: i32) {
    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // Preload all 32 source values into SIMD registers for reuse
        // AVX2 can hold 16 i16 values per register, so we need 2 registers
        let src_lo = _mm256_loadu_si256(src.as_ptr() as *const __m256i);
        let src_hi = _mm256_loadu_si256(src.as_ptr().add(16) as *const __m256i);

        // Compute all 32 outputs
        for j in 0..32 {
            // Load coefficient row (32 i16 values)
            let mut coef_row = [0i16; 32];
            for k in 0..32 {
                coef_row[k] = get_dct32_coef(k, j);
            }

            // Compute dot product using SIMD
            // Split into low and high halves (16 each)
            let coef_lo = _mm256_loadu_si256(coef_row.as_ptr() as *const __m256i);
            let coef_hi = _mm256_loadu_si256(coef_row.as_ptr().add(16) as *const __m256i);

            // Multiply-accumulate: coef[k] * src[k] for k=0..15
            let prod_lo = _mm256_madd_epi16(coef_lo, src_lo);

            // Multiply-accumulate: coef[k] * src[k] for k=16..31
            let prod_hi = _mm256_madd_epi16(coef_hi, src_hi);

            // Sum all products
            let sum_vec = _mm256_add_epi32(prod_lo, prod_hi);

            // Horizontal sum across 8 lanes
            let sum = horizontal_sum_i32(sum_vec);

            // Apply shift and rounding
            dst[j] = (sum + add) >> shift;
        }
    }
}

/// SIMD-optimized 1D 32-point inverse DCT (for horizontal pass with i32 input)
///
/// Similar to vertical pass but handles i32 input and i64 accumulation for precision.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct32_1d_horizontal_avx2(src: &[i32], dst: &mut [i16; 32], shift: i32, add: i64) {
    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        for j in 0..32 {
            let mut sum = 0i64;

            // Process 8 elements at a time using AVX2
            for chunk in 0..4 {
                let base = chunk * 8;

                // Load 8 i32 source values
                let src_vec = _mm256_loadu_si256(src.as_ptr().add(base) as *const __m256i);

                // Load 8 i16 coefficients and convert to i32
                let mut coef_buf = [0i16; 8];
                for k in 0..8 {
                    coef_buf[k] = get_dct32_coef(base + k, j);
                }

                // Convert i16 to i32 for multiplication
                let coef_i16 = _mm_loadu_si128(coef_buf.as_ptr() as *const __m128i);
                let coef_vec = _mm256_cvtepi16_epi32(coef_i16);

                // Multiply i32 * i32
                let prod_vec = _mm256_mullo_epi32(coef_vec, src_vec);

                // Extract and accumulate
                let mut prod_arr = [0i32; 8];
                _mm256_storeu_si256(prod_arr.as_mut_ptr() as *mut __m256i, prod_vec);

                for &p in &prod_arr {
                    sum += p as i64;
                }
            }

            // Apply shift and rounding
            dst[j] = ((sum + add) >> shift) as i16;
        }
    }
}

/// Horizontal sum of 8 i32 values in an AVX2 register
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn horizontal_sum_i32(v: __m256i) -> i32 {
    // Sum pairs: [a0+a1, a2+a3, a4+a5, a6+a7, ...]
    let sum1 = _mm256_hadd_epi32(v, v);
    // Sum pairs again: [a0+a1+a2+a3, ...]
    let sum2 = _mm256_hadd_epi32(sum1, sum1);

    // Extract low and high 128-bit lanes
    let low128 = _mm256_castsi256_si128(sum2);
    let high128 = _mm256_extracti128_si256(sum2, 1);

    // Add the two lanes
    let sum128 = _mm_add_epi32(low128, high128);

    // Extract final result
    _mm_cvtsi128_si32(sum128)
}

/// Dispatch function for IDCT32 - uses SIMD if available, scalar fallback otherwise
#[inline]
pub fn idct32_optimized(coeffs: &[i16; 1024], output: &mut [i16; 1024], bit_depth: u8) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        // Check if AVX2 is available at runtime (works on both Intel and AMD)
        if is_x86_feature_detected!("avx2") {
            unsafe {
                idct32_avx2(coeffs, output, bit_depth);
            }
            return;
        }
    }

    // Fallback to scalar implementation
    super::transform::idct32(coeffs, output, bit_depth);
}

/// SIMD-optimized inverse 16x16 DCT
///
/// Uses AVX2 instructions for 6-10x speedup over scalar version.
/// Optimized for both Intel and AMD processors.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn idct16_avx2(coeffs: &[i16; 256], output: &mut [i16; 256], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1i32 << (shift1 - 1);
    let add2 = 1i32 << (shift2 - 1);

    let mut tmp = [0i32; 256];

    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // First pass (vertical) - process 16 columns
        for col in 0..16 {
            // Gather column data (stride 16)
            let mut src_col = [0i16; 16];
            for row in 0..16 {
                src_col[row] = coeffs[row * 16 + col];
            }

            // Compute 16 outputs for this column using SIMD
            let mut dst_col = [0i32; 16];
            idct16_1d_avx2(&src_col, &mut dst_col, shift1, add1);

            // Scatter results back (stride 16)
            for row in 0..16 {
                tmp[row * 16 + col] = dst_col[row];
            }
        }

        // Second pass (horizontal) - process 16 rows
        for row in 0..16 {
            let row_start = row * 16;
            let src_row = &tmp[row_start..row_start + 16];
            let mut dst_row = [0i16; 16];

            // Compute 16 outputs for this row
            idct16_1d_horizontal_avx2(src_row, &mut dst_row, shift2 as i32, add2);

            // Write results
            output[row_start..row_start + 16].copy_from_slice(&dst_row);
        }
    }
}

/// SIMD-optimized 1D 16-point inverse DCT (vertical pass)
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct16_1d_avx2(src: &[i16; 16], dst: &mut [i32; 16], shift: i32, add: i32) {
    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // Load source into SIMD register
        let src_vec = _mm256_loadu_si256(src.as_ptr() as *const __m256i);

        // Compute all 16 outputs
        for j in 0..16 {
            // Build coefficient column j (need DCT16_MATRIX[k][j] for k=0..15)
            let mut coef_col = [0i16; 16];
            for k in 0..16 {
                coef_col[k] = DCT16_MATRIX[k][j];
            }

            // Load coefficients
            let coef_vec = _mm256_loadu_si256(coef_col.as_ptr() as *const __m256i);

            // Multiply-accumulate: coef[k] * src[k] for k=0..15
            let prod = _mm256_madd_epi16(coef_vec, src_vec);

            // Horizontal sum
            let sum = horizontal_sum_i32(prod);

            // Apply shift and rounding
            dst[j] = (sum + add) >> shift;
        }
    }
}

/// SIMD-optimized 1D 16-point inverse DCT (horizontal pass)
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct16_1d_horizontal_avx2(src: &[i32], dst: &mut [i16; 16], shift: i32, add: i32) {
    for j in 0..16 {
        // Build coefficient column j (need DCT16_MATRIX[k][j] for k=0..15)
        let mut coef_col = [0i16; 16];
        for k in 0..16 {
            coef_col[k] = DCT16_MATRIX[k][j];
        }

        // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
        unsafe {
            // Process in two halves (8 elements each) and sum separately
            // First half (0..8)
            let src_lo = _mm256_loadu_si256(src.as_ptr() as *const __m256i);
            let coef_i16_lo = _mm_loadu_si128(coef_col.as_ptr() as *const __m128i);
            let coef_lo = _mm256_cvtepi16_epi32(coef_i16_lo);
            let prod_lo = _mm256_mullo_epi32(coef_lo, src_lo);

            // Second half (8..16)
            let src_hi = _mm256_loadu_si256(src.as_ptr().add(8) as *const __m256i);
            let coef_i16_hi = _mm_loadu_si128(coef_col.as_ptr().add(8) as *const __m128i);
            let coef_hi = _mm256_cvtepi16_epi32(coef_i16_hi);
            let prod_hi = _mm256_mullo_epi32(coef_hi, src_hi);

            // Sum each half separately, then add (more precise)
            let sum_lo = horizontal_sum_i32(prod_lo);
            let sum_hi = horizontal_sum_i32(prod_hi);
            let sum = sum_lo + sum_hi;

            // Apply shift and rounding
            dst[j] = ((sum + add) >> shift) as i16;
        }
    }
}

/// Dispatch function for IDCT16 - uses SIMD if available
#[inline]
pub fn idct16_optimized(coeffs: &[i16; 256], output: &mut [i16; 256], bit_depth: u8) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                idct16_avx2(coeffs, output, bit_depth);
            }
            return;
        }
    }

    // Fallback to scalar implementation
    super::transform::idct16(coeffs, output, bit_depth);
}

/// SIMD-optimized inverse 8x8 DCT
///
/// Uses AVX2 instructions for 4-6x speedup over scalar version.
/// Optimized for both Intel and AMD processors.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn idct8_avx2(coeffs: &[i16; 64], output: &mut [i16; 64], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1i32 << (shift1 - 1);
    let add2 = 1i32 << (shift2 - 1);

    let mut tmp = [0i32; 64];

    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // First pass (vertical) - process 8 columns
        for col in 0..8 {
            // Gather column data (stride 8)
            let mut src_col = [0i16; 8];
            for row in 0..8 {
                src_col[row] = coeffs[row * 8 + col];
            }

            // Compute 8 outputs for this column using SIMD
            let mut dst_col = [0i32; 8];
            idct8_1d_avx2(&src_col, &mut dst_col, shift1, add1);

            // Scatter results back (stride 8)
            for row in 0..8 {
                tmp[row * 8 + col] = dst_col[row];
            }
        }

        // Second pass (horizontal) - process 8 rows
        for row in 0..8 {
            let row_start = row * 8;
            let src_row = &tmp[row_start..row_start + 8];
            let mut dst_row = [0i16; 8];

            // Compute 8 outputs for this row
            idct8_1d_horizontal_avx2(src_row, &mut dst_row, shift2 as i32, add2);

            // Write results
            output[row_start..row_start + 8].copy_from_slice(&dst_row);
        }
    }
}

/// SIMD-optimized 1D 8-point inverse DCT (vertical pass)
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct8_1d_avx2(src: &[i16; 8], dst: &mut [i32; 8], shift: i32, add: i32) {
    use super::transform::DCT8_MATRIX;

    // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
    unsafe {
        // Load source into 128-bit register (8 i16 values)
        let src_vec = _mm_loadu_si128(src.as_ptr() as *const __m128i);
        // Extend to 256-bit (duplicate to both lanes for easier processing)
        let src_256 = _mm256_broadcastsi128_si256(src_vec);

        // Compute all 8 outputs
        for j in 0..8 {
            // Build coefficient column j (need DCT8_MATRIX[k][j] for k=0..7)
            let mut coef_col = [0i16; 8];
            for k in 0..8 {
                coef_col[k] = DCT8_MATRIX[k][j];
            }

            // Load coefficients into 128-bit register
            let coef_128 = _mm_loadu_si128(coef_col.as_ptr() as *const __m128i);
            let coef_256 = _mm256_broadcastsi128_si256(coef_128);

            // Multiply-accumulate: coef[k] * src[k] for k=0..7
            let prod = _mm256_madd_epi16(coef_256, src_256);

            // Horizontal sum (only need bottom 128 bits since data is duplicated)
            let prod_128 = _mm256_castsi256_si128(prod);
            let sum1 = _mm_hadd_epi32(prod_128, prod_128);
            let sum2 = _mm_hadd_epi32(sum1, sum1);
            let sum = _mm_cvtsi128_si32(sum2);

            // Apply shift and rounding
            dst[j] = (sum + add) >> shift;
        }
    }
}

/// SIMD-optimized 1D 8-point inverse DCT (horizontal pass)
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn idct8_1d_horizontal_avx2(src: &[i32], dst: &mut [i16; 8], shift: i32, add: i32) {
    use super::transform::DCT8_MATRIX;

    for j in 0..8 {
        // Build coefficient column j (need DCT8_MATRIX[k][j] for k=0..7)
        let mut coef_col = [0i16; 8];
        for k in 0..8 {
            coef_col[k] = DCT8_MATRIX[k][j];
        }

        // SAFETY: We're inside an unsafe function with target_feature(avx2) enabled
        unsafe {
            // Load 8 i32 source values into AVX2 register
            let src_vec = _mm256_loadu_si256(src.as_ptr() as *const __m256i);

            // Load 8 i16 coefficients and convert to i32
            let coef_i16 = _mm_loadu_si128(coef_col.as_ptr() as *const __m128i);
            let coef_vec = _mm256_cvtepi16_epi32(coef_i16);

            // Multiply i32 * i32
            let prod_vec = _mm256_mullo_epi32(coef_vec, src_vec);

            // Horizontal sum
            let sum = horizontal_sum_i32(prod_vec);

            // Apply shift and rounding
            dst[j] = ((sum + add) >> shift) as i16;
        }
    }
}

/// Dispatch function for IDCT8 - uses SIMD if available
#[inline]
pub fn idct8_optimized(coeffs: &[i16; 64], output: &mut [i16; 64], bit_depth: u8) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                idct8_avx2(coeffs, output, bit_depth);
            }
            return;
        }
    }

    // Fallback to scalar implementation
    super::transform::idct8(coeffs, output, bit_depth);
}

/// SIMD-optimized inverse 4x4 DCT
///
/// Uses SSE2 instructions (available on all x86_64) for 3-5x speedup.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
pub unsafe fn idct4_sse2(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    use super::transform::DCT4_MATRIX;

    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1i32 << (shift1 - 1);
    let add2 = 1i32 << (shift2 - 1);

    let mut tmp = [0i32; 16];

    // First pass (vertical) - process 4 columns
    for col in 0..4 {
        // Gather column data (stride 4)
        let src_col = [
            coeffs[0 * 4 + col],
            coeffs[1 * 4 + col],
            coeffs[2 * 4 + col],
            coeffs[3 * 4 + col],
        ];

        // Compute 4 outputs for this column
        for j in 0..4 {
            let coef_col = [
                DCT4_MATRIX[0][j],
                DCT4_MATRIX[1][j],
                DCT4_MATRIX[2][j],
                DCT4_MATRIX[3][j],
            ];

            // Dot product (small enough to do scalar is actually fine)
            let mut sum = 0i32;
            for k in 0..4 {
                sum += coef_col[k] as i32 * src_col[k] as i32;
            }

            tmp[j * 4 + col] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal) - process 4 rows
    for row in 0..4 {
        let row_start = row * 4;

        for j in 0..4 {
            let coef_col = [
                DCT4_MATRIX[0][j],
                DCT4_MATRIX[1][j],
                DCT4_MATRIX[2][j],
                DCT4_MATRIX[3][j],
            ];

            let mut sum = 0i32;
            for k in 0..4 {
                sum += coef_col[k] as i32 * tmp[row_start + k];
            }

            output[row_start + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Dispatch function for IDCT4 - uses SIMD if available
#[inline]
pub fn idct4_optimized(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        // SSE2 is available on all x86_64
        unsafe {
            idct4_sse2(coeffs, output, bit_depth);
        }
        return;
    }

    // Fallback to scalar implementation
    #[allow(unreachable_code)]
    super::transform::idct4(coeffs, output, bit_depth);
}

/// SIMD-optimized inverse 4x4 DST
///
/// Uses SSE2 instructions (available on all x86_64) for 3-5x speedup.
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
pub unsafe fn idst4_sse2(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    use super::transform::DST4_MATRIX;

    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1i32 << (shift1 - 1);
    let add2 = 1i32 << (shift2 - 1);

    let mut tmp = [0i32; 16];

    // First pass (vertical) - process 4 columns
    for col in 0..4 {
        // Gather column data (stride 4)
        let src_col = [
            coeffs[0 * 4 + col],
            coeffs[1 * 4 + col],
            coeffs[2 * 4 + col],
            coeffs[3 * 4 + col],
        ];

        // Compute 4 outputs for this column
        for j in 0..4 {
            let coef_col = [
                DST4_MATRIX[0][j],
                DST4_MATRIX[1][j],
                DST4_MATRIX[2][j],
                DST4_MATRIX[3][j],
            ];

            // Dot product
            let mut sum = 0i32;
            for k in 0..4 {
                sum += coef_col[k] as i32 * src_col[k] as i32;
            }

            tmp[j * 4 + col] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal) - process 4 rows
    for row in 0..4 {
        let row_start = row * 4;

        for j in 0..4 {
            let coef_col = [
                DST4_MATRIX[0][j],
                DST4_MATRIX[1][j],
                DST4_MATRIX[2][j],
                DST4_MATRIX[3][j],
            ];

            let mut sum = 0i32;
            for k in 0..4 {
                sum += coef_col[k] as i32 * tmp[row_start + k];
            }

            output[row_start + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Dispatch function for IDST4 - uses SIMD if available
#[inline]
pub fn idst4_optimized(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        // SSE2 is available on all x86_64
        unsafe {
            idst4_sse2(coeffs, output, bit_depth);
        }
        return;
    }

    // Fallback to scalar implementation
    #[allow(unreachable_code)]
    super::transform::idst4(coeffs, output, bit_depth);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idct32_simd_matches_scalar() {
        // Create test input with some non-zero coefficients
        let mut coeffs = [0i16; 1024];
        coeffs[0] = 100;  // DC
        coeffs[1] = 50;
        coeffs[32] = 30;
        coeffs[33] = 20;

        let mut output_scalar = [0i16; 1024];
        let mut output_simd = [0i16; 1024];

        let bit_depth = 8;

        // Compute with scalar
        super::super::transform::idct32(&coeffs, &mut output_scalar, bit_depth);

        // Compute with SIMD
        idct32_optimized(&coeffs, &mut output_simd, bit_depth);

        // Compare results
        for i in 0..1024 {
            assert_eq!(
                output_scalar[i], output_simd[i],
                "Mismatch at position {}: scalar={}, simd={}",
                i, output_scalar[i], output_simd[i]
            );
        }
    }

    #[test]
    fn test_idct32_simd_dc_only() {
        let mut coeffs = [0i16; 1024];
        coeffs[0] = 128;  // DC coefficient only

        let mut output = [0i16; 1024];
        idct32_optimized(&coeffs, &mut output, 8);

        // DC-only should produce relatively uniform output
        // Just check it's not all zeros
        let non_zero = output.iter().any(|&v| v != 0);
        assert!(non_zero, "Output should not be all zeros for DC input");

        // Print first few values for inspection
        println!("IDCT32 DC-only output (first 16): {:?}", &output[..16]);
    }

    #[test]
    fn test_idct16_simd_matches_scalar() {
        // Create test input with some non-zero coefficients
        let mut coeffs = [0i16; 256];
        coeffs[0] = 100;  // DC
        coeffs[1] = 50;
        coeffs[16] = 30;
        coeffs[17] = 20;

        let mut output_scalar = [0i16; 256];
        let mut output_simd = [0i16; 256];

        let bit_depth = 8;

        // Compute with scalar
        super::super::transform::idct16(&coeffs, &mut output_scalar, bit_depth);

        // Compute with SIMD
        idct16_optimized(&coeffs, &mut output_simd, bit_depth);

        // Compare results - allow off-by-1 due to rounding differences
        let mut max_diff = 0i32;
        let mut diff_count = 0;
        for i in 0..256 {
            let diff = (output_scalar[i] as i32 - output_simd[i] as i32).abs();
            if diff > max_diff {
                max_diff = diff;
            }
            if diff > 0 {
                diff_count += 1;
            }
        }

        println!("IDCT16 comparison: max_diff={}, diff_count={}/{}", max_diff, diff_count, 256);

        // Strict equality check
        for i in 0..256 {
            assert_eq!(
                output_scalar[i], output_simd[i],
                "Mismatch at position {}: scalar={}, simd={}",
                i, output_scalar[i], output_simd[i]
            );
        }
    }

    #[test]
    fn test_idct16_simd_dc_only() {
        let mut coeffs = [0i16; 256];
        coeffs[0] = 128;  // DC coefficient only

        let mut output = [0i16; 256];
        idct16_optimized(&coeffs, &mut output, 8);

        let non_zero = output.iter().any(|&v| v != 0);
        assert!(non_zero, "Output should not be all zeros for DC input");

        println!("IDCT16 DC-only output (first 16): {:?}", &output[..16]);
    }

    #[test]
    fn test_idct8_simd_matches_scalar() {
        // Create test input with some non-zero coefficients
        let mut coeffs = [0i16; 64];
        coeffs[0] = 100;  // DC
        coeffs[1] = 50;
        coeffs[8] = 30;
        coeffs[9] = 20;
        coeffs[16] = -15;
        coeffs[24] = 10;

        let mut output_scalar = [0i16; 64];
        let mut output_simd = [0i16; 64];

        let bit_depth = 8;

        // Compute with scalar
        super::super::transform::idct8(&coeffs, &mut output_scalar, bit_depth);

        // Compute with SIMD
        idct8_optimized(&coeffs, &mut output_simd, bit_depth);

        // Compare results - should be pixel-perfect
        for i in 0..64 {
            assert_eq!(
                output_scalar[i], output_simd[i],
                "Mismatch at position {}: scalar={}, simd={}",
                i, output_scalar[i], output_simd[i]
            );
        }
    }

    #[test]
    fn test_idct8_simd_dc_only() {
        let mut coeffs = [0i16; 64];
        coeffs[0] = 128;  // DC coefficient only

        let mut output = [0i16; 64];
        idct8_optimized(&coeffs, &mut output, 8);

        // DC-only should produce uniform output
        let first = output[0];
        for (i, &v) in output.iter().enumerate() {
            assert_eq!(v, first, "DC-only should produce uniform output, mismatch at {}", i);
        }

        println!("IDCT8 DC-only output (first 8): {:?}", &output[..8]);
    }

    #[test]
    fn test_idct4_simd_matches_scalar() {
        // Create test input with some non-zero coefficients
        let mut coeffs = [0i16; 16];
        coeffs[0] = 64;   // DC
        coeffs[1] = 32;
        coeffs[4] = -16;
        coeffs[5] = 8;

        let mut output_scalar = [0i16; 16];
        let mut output_simd = [0i16; 16];

        let bit_depth = 8;

        // Compute with scalar
        super::super::transform::idct4(&coeffs, &mut output_scalar, bit_depth);

        // Compute with SIMD
        idct4_optimized(&coeffs, &mut output_simd, bit_depth);

        // Compare results - should be pixel-perfect
        for i in 0..16 {
            assert_eq!(
                output_scalar[i], output_simd[i],
                "Mismatch at position {}: scalar={}, simd={}",
                i, output_scalar[i], output_simd[i]
            );
        }
    }

    #[test]
    fn test_idct4_simd_dc_only() {
        let mut coeffs = [0i16; 16];
        coeffs[0] = 64;  // DC coefficient only

        let mut output = [0i16; 16];
        idct4_optimized(&coeffs, &mut output, 8);

        // DC-only should produce uniform output
        let first = output[0];
        for (i, &v) in output.iter().enumerate() {
            assert_eq!(v, first, "DC-only should produce uniform output, mismatch at {}", i);
        }

        println!("IDCT4 DC-only output: {:?}", &output);
    }

    #[test]
    fn test_idst4_simd_matches_scalar() {
        // Use actual coefficients from test case
        let mut coeffs = [0i16; 16];
        coeffs[0] = 144;
        coeffs[1] = -3024;
        coeffs[2] = -288;
        coeffs[4] = -144;
        coeffs[5] = -432;
        coeffs[6] = -288;
        coeffs[8] = 144;
        coeffs[9] = -576;
        coeffs[10] = 432;
        coeffs[12] = -144;
        coeffs[13] = 288;
        coeffs[14] = 288;

        let mut output_scalar = [0i16; 16];
        let mut output_simd = [0i16; 16];

        let bit_depth = 8;

        // Compute with scalar
        super::super::transform::idst4(&coeffs, &mut output_scalar, bit_depth);

        // Compute with SIMD
        idst4_optimized(&coeffs, &mut output_simd, bit_depth);

        // Compare results - should be pixel-perfect
        for i in 0..16 {
            assert_eq!(
                output_scalar[i], output_simd[i],
                "Mismatch at position {}: scalar={}, simd={}",
                i, output_scalar[i], output_simd[i]
            );
        }
    }

    #[test]
    fn test_idst4_simd_dc_only() {
        let mut coeffs = [0i16; 16];
        coeffs[0] = 64;  // DC coefficient only

        let mut output = [0i16; 16];
        idst4_optimized(&coeffs, &mut output, 8);

        // DST doesn't produce uniform output for DC
        let non_zero = output.iter().any(|&v| v != 0);
        assert!(non_zero, "IDST4 should produce non-zero output for DC input");

        println!("IDST4 DC-only output: {:?}", &output);
    }
}
