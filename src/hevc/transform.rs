//! HEVC transform and inverse quantization
//!
//! This module implements the inverse transforms used in HEVC:
//! - 4x4 Inverse DST (for intra 4x4 luma)
//! - 4x4, 8x8, 16x16, 32x32 Inverse DCT

// Transform and inverse quantization for HEVC

/// Maximum number of coefficients (32x32 transform)
pub const MAX_COEFF: usize = 32 * 32;

/// DST-VII basis functions for 4x4 (scaled by 64)
pub(crate) static DST4_MATRIX: [[i16; 4]; 4] = [
    [29, 55, 74, 84],
    [74, 74, 0, -74],
    [84, -29, -74, 55],
    [55, -84, 74, -29],
];

/// DCT-II basis functions for 4x4 (scaled by 64)
pub(crate) static DCT4_MATRIX: [[i16; 4]; 4] = [
    [64, 64, 64, 64],
    [83, 36, -36, -83],
    [64, -64, -64, 64],
    [36, -83, 83, -36],
];

/// DCT-II basis functions for 8x8 (scaled by 64)
pub(crate) static DCT8_MATRIX: [[i16; 8]; 8] = [
    [64, 64, 64, 64, 64, 64, 64, 64],
    [89, 75, 50, 18, -18, -50, -75, -89],
    [83, 36, -36, -83, -83, -36, 36, 83],
    [75, -18, -89, -50, 50, 89, 18, -75],
    [64, -64, -64, 64, 64, -64, -64, 64],
    [50, -89, 18, 75, -75, -18, 89, -50],
    [36, -83, 83, -36, -36, 83, -83, 36],
    [18, -50, 75, -89, 89, -75, 50, -18],
];

/// DCT-II basis functions for 16x16 (scaled by 64)
pub(crate) static DCT16_MATRIX: [[i16; 16]; 16] = [
    [
        64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
    ],
    [
        90, 87, 80, 70, 57, 43, 25, 9, -9, -25, -43, -57, -70, -80, -87, -90,
    ],
    [
        89, 75, 50, 18, -18, -50, -75, -89, -89, -75, -50, -18, 18, 50, 75, 89,
    ],
    [
        87, 57, 9, -43, -80, -90, -70, -25, 25, 70, 90, 80, 43, -9, -57, -87,
    ],
    [
        83, 36, -36, -83, -83, -36, 36, 83, 83, 36, -36, -83, -83, -36, 36, 83,
    ],
    [
        80, 9, -70, -87, -25, 57, 90, 43, -43, -90, -57, 25, 87, 70, -9, -80,
    ],
    [
        75, -18, -89, -50, 50, 89, 18, -75, -75, 18, 89, 50, -50, -89, -18, 75,
    ],
    [
        70, -43, -87, 9, 90, 25, -80, -57, 57, 80, -25, -90, -9, 87, 43, -70,
    ],
    [
        64, -64, -64, 64, 64, -64, -64, 64, 64, -64, -64, 64, 64, -64, -64, 64,
    ],
    [
        57, -80, -25, 90, -9, -87, 43, 70, -70, -43, 87, 9, -90, 25, 80, -57,
    ],
    [
        50, -89, 18, 75, -75, -18, 89, -50, -50, 89, -18, -75, 75, 18, -89, 50,
    ],
    [
        43, -90, 57, 25, -87, 70, 9, -80, 80, -9, -70, 87, -25, -57, 90, -43,
    ],
    [
        36, -83, 83, -36, -36, 83, -83, 36, 36, -83, 83, -36, -36, 83, -83, 36,
    ],
    [
        25, -70, 90, -80, 43, 9, -57, 87, -87, 57, -9, -43, 80, -90, 70, -25,
    ],
    [
        18, -50, 75, -89, 89, -75, 50, -18, -18, 50, -75, 89, -89, 75, -50, 18,
    ],
    [
        9, -25, 43, -57, 70, -80, 87, -90, 90, -87, 80, -70, 57, -43, 25, -9,
    ],
];

/// DCT-II basis functions for 32x32 (first 16 rows shown, rest follow pattern)
pub(crate) static DCT32_EVEN: [i16; 16] = [
    64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64, 64,
];
pub(crate) static DCT32_ODD: [[i16; 16]; 16] = [
    [
        90, 90, 88, 85, 82, 78, 73, 67, 61, 54, 46, 38, 31, 22, 13, 4,
    ],
    [
        90, 82, 67, 46, 22, -4, -31, -54, -73, -85, -90, -88, -78, -61, -38, -13,
    ],
    [
        88, 67, 31, -13, -54, -82, -90, -78, -46, -4, 38, 73, 90, 85, 61, 22,
    ],
    [
        85, 46, -13, -67, -90, -73, -22, 38, 82, 88, 54, -4, -61, -90, -78, -31,
    ],
    [
        82, 22, -54, -90, -61, 13, 78, 85, 31, -46, -90, -67, 4, 73, 88, 38,
    ],
    [
        78, -4, -82, -73, 13, 85, 67, -22, -88, -61, 31, 90, 54, -38, -90, -46,
    ],
    [
        73, -31, -90, -22, 78, 67, -38, -90, -13, 82, 61, -46, -88, -4, 85, 54,
    ],
    [
        67, -54, -78, 38, 85, -22, -90, 4, 90, 13, -88, -31, 82, 46, -73, -61,
    ],
    [
        61, -73, -46, 82, 31, -88, -13, 90, -4, -90, 22, 85, -38, -78, 54, 67,
    ],
    [
        54, -85, -4, 88, -46, -61, 82, 13, -90, 38, 67, -78, -22, 90, -31, -73,
    ],
    [
        46, -90, 38, 54, -90, 31, 61, -88, 22, 67, -85, 13, 73, -82, 4, 78,
    ],
    [
        38, -88, 73, -4, -67, 90, -46, -31, 85, -78, 13, 61, -90, 54, 22, -82,
    ],
    [
        31, -78, 90, -61, 4, 54, -88, 82, -38, -22, 73, -90, 67, -13, -46, 85,
    ],
    [
        22, -61, 85, -90, 73, -38, -4, 46, -78, 90, -82, 54, -13, -31, 67, -88,
    ],
    [
        13, -38, 61, -78, 88, -90, 85, -73, 54, -31, 4, 22, -46, 67, -82, 90,
    ],
    [
        4, -13, 22, -31, 38, -46, 54, -61, 67, -73, 78, -82, 85, -88, 90, -90,
    ],
];

/// Inverse 4x4 DST (for intra 4x4 luma blocks)
pub fn idst4(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1 << (shift1 - 1);
    let add2 = 1 << (shift2 - 1);

    let mut tmp = [0i32; 16];

    // First pass (vertical)
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = 0i32;
            for k in 0..4 {
                sum += DST4_MATRIX[k][j] as i32 * coeffs[k * 4 + i] as i32;
            }
            tmp[j * 4 + i] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal)
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = 0i32;
            for k in 0..4 {
                sum += DST4_MATRIX[k][j] as i32 * tmp[i * 4 + k];
            }
            output[i * 4 + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Inverse 4x4 DCT
pub fn idct4(coeffs: &[i16; 16], output: &mut [i16; 16], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1 << (shift1 - 1);
    let add2 = 1 << (shift2 - 1);

    let mut tmp = [0i32; 16];

    // First pass (vertical)
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = 0i32;
            for k in 0..4 {
                sum += DCT4_MATRIX[k][j] as i32 * coeffs[k * 4 + i] as i32;
            }
            tmp[j * 4 + i] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal)
    for i in 0..4 {
        for j in 0..4 {
            let mut sum = 0i32;
            for k in 0..4 {
                sum += DCT4_MATRIX[k][j] as i32 * tmp[i * 4 + k];
            }
            output[i * 4 + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Inverse 8x8 DCT
pub fn idct8(coeffs: &[i16; 64], output: &mut [i16; 64], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1 << (shift1 - 1);
    let add2 = 1 << (shift2 - 1);

    let mut tmp = [0i32; 64];

    // First pass (vertical)
    for i in 0..8 {
        for j in 0..8 {
            let mut sum = 0i32;
            for k in 0..8 {
                sum += DCT8_MATRIX[k][j] as i32 * coeffs[k * 8 + i] as i32;
            }
            tmp[j * 8 + i] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal)
    for i in 0..8 {
        for j in 0..8 {
            let mut sum = 0i32;
            for k in 0..8 {
                sum += DCT8_MATRIX[k][j] as i32 * tmp[i * 8 + k];
            }
            output[i * 8 + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Inverse 16x16 DCT
pub fn idct16(coeffs: &[i16; 256], output: &mut [i16; 256], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1 << (shift1 - 1);
    let add2 = 1 << (shift2 - 1);

    let mut tmp = [0i32; 256];

    // First pass (vertical)
    for i in 0..16 {
        for j in 0..16 {
            let mut sum = 0i32;
            for k in 0..16 {
                sum += DCT16_MATRIX[k][j] as i32 * coeffs[k * 16 + i] as i32;
            }
            tmp[j * 16 + i] = (sum + add1) >> shift1;
        }
    }

    // Second pass (horizontal)
    for i in 0..16 {
        for j in 0..16 {
            let mut sum = 0i32;
            for k in 0..16 {
                sum += DCT16_MATRIX[k][j] as i32 * tmp[i * 16 + k];
            }
            output[i * 16 + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Inverse 32x32 DCT (direct matrix multiply)
pub fn idct32(coeffs: &[i16; 1024], output: &mut [i16; 1024], bit_depth: u8) {
    let shift1 = 7;
    let shift2 = 20 - bit_depth;
    let add1 = 1 << (shift1 - 1);
    let add2 = 1i64 << (shift2 - 1);

    let mut tmp = [0i32; 1024];

    // First pass (vertical)
    for i in 0..32 {
        partial_butterfly_inverse_32(&coeffs[i..], 32, &mut tmp[i..], 32, shift1, add1);
    }

    // Second pass (horizontal) - use i32 intermediates directly (NOT i16!)
    // First pass output can exceed i16 range (up to ~737k), so truncating to i16
    // would corrupt the transform result.
    for i in 0..32 {
        let row_start = i * 32;
        for j in 0..32 {
            let mut sum = 0i64;
            for k in 0..32 {
                let coef = get_dct32_coef(k, j);
                sum += coef as i64 * tmp[row_start + k] as i64;
            }
            output[row_start + j] = ((sum + add2) >> shift2) as i16;
        }
    }
}

/// Inverse 32-point transform (column)
fn partial_butterfly_inverse_32(
    src: &[i16],
    src_stride: usize,
    dst: &mut [i32],
    dst_stride: usize,
    shift: i32,
    add: i32,
) {
    for j in 0..32 {
        let mut sum = 0i32;
        for k in 0..32 {
            // Use precomputed DCT32 coefficients
            let coef = get_dct32_coef(k, j);
            sum += coef as i32 * src[k * src_stride] as i32;
        }
        dst[j * dst_stride] = (sum + add) >> shift;
    }
}

/// Inverse 32-point transform (row)
fn partial_butterfly_inverse_32_row(src: &[i16; 32], dst: &mut [i32; 32], shift: i32, add: i32) {
    for (j, dst_val) in dst.iter_mut().enumerate() {
        let mut sum = 0i32;
        for (k, &src_val) in src.iter().enumerate() {
            let coef = get_dct32_coef(k, j);
            sum += coef as i32 * src_val as i32;
        }
        *dst_val = (sum + add) >> shift;
    }
}

/// Get DCT32 coefficient at position (row, col)
pub(crate) fn get_dct32_coef(row: usize, col: usize) -> i16 {
    if row.is_multiple_of(2) {
        // Even rows: symmetric
        if row == 0 {
            // Row 0 is all 64s
            DCT32_EVEN[col % 16]
        } else if col < 16 {
            DCT16_MATRIX[row / 2][col]
        } else {
            // Symmetry: T[2k][col] = T[2k][31-col] for col >= 16
            DCT16_MATRIX[row / 2][31 - col]
        }
    } else {
        // Odd rows: anti-symmetric
        let odd_row = row / 2;
        if col < 16 {
            DCT32_ODD[odd_row][col]
        } else {
            // Anti-symmetry: T[2k+1][col] = -T[2k+1][31-col] for col >= 16
            -DCT32_ODD[odd_row][31 - col]
        }
    }
}

/// Dequantization parameters
#[derive(Debug, Clone, Copy)]
pub struct DequantParams {
    /// QP value
    pub qp: i32,
    /// Bit depth
    pub bit_depth: u8,
    /// Transform size log2
    pub log2_tr_size: u8,
}

pub fn dequantize(coeffs: &mut [i16], params: DequantParams) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") && coeffs.len() >= 16 {
            unsafe {
                return dequantize_avx2(coeffs, params);
            }
        }
    }
    dequantize_scalar(coeffs, params);
}

fn dequantize_scalar(coeffs: &mut [i16], params: DequantParams) {
    static LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

    let qp_per = params.qp / 6;
    let qp_rem = params.qp % 6;
    let scale = LEVEL_SCALE[qp_rem as usize];

    let shift = params.bit_depth as i32 - 9 + params.log2_tr_size as i32;
    let add = if shift > 0 { 1 << (shift - 1) } else { 0 };

    if shift >= 0 {
        for coef in coeffs.iter_mut() {
            let value = (*coef as i32 * scale * (1 << qp_per) + add) >> shift;
            *coef = value.clamp(-32768, 32767) as i16;
        }
    } else {
        let neg_shift = -shift;
        for coef in coeffs.iter_mut() {
            let value = (*coef as i32 * scale * (1 << qp_per)) << neg_shift;
            *coef = value.clamp(-32768, 32767) as i16;
        }
    }
}

#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
unsafe fn dequantize_avx2(coeffs: &mut [i16], params: DequantParams) {
    use std::arch::x86_64::*;

    static LEVEL_SCALE: [i32; 6] = [40, 45, 51, 57, 64, 72];

    let qp_per = params.qp / 6;
    let qp_rem = params.qp % 6;
    let scale = LEVEL_SCALE[qp_rem as usize];
    let multiplier = scale * (1 << qp_per);

    let shift = params.bit_depth as i32 - 9 + params.log2_tr_size as i32;

    if shift >= 0 {
        let add = 1 << (shift - 1);
        let v_mult = _mm256_set1_epi32(multiplier);
        let v_add = _mm256_set1_epi32(add);
        let v_min = _mm256_set1_epi32(-32768);
        let v_max = _mm256_set1_epi32(32767);

        let chunks = coeffs.len() / 8;
        for i in 0..chunks {
            let offset = i * 8;

            let v_coef_i16 = _mm_loadu_si128(coeffs.as_ptr().add(offset) as *const __m128i);
            let v_coef = _mm256_cvtepi16_epi32(v_coef_i16);

            let v_scaled = _mm256_mullo_epi32(v_coef, v_mult);
            let v_added = _mm256_add_epi32(v_scaled, v_add);

            let mut results = [0i32; 8];
            _mm256_storeu_si256(results.as_mut_ptr() as *mut __m256i, v_added);

            for val in results.iter_mut() {
                *val >>= shift;
            }

            let v_shifted = _mm256_loadu_si256(results.as_ptr() as *const __m256i);
            let v_clamped_lo = _mm256_max_epi32(v_shifted, v_min);
            let v_clamped = _mm256_min_epi32(v_clamped_lo, v_max);

            // Pack 8x i32 to 8x i16
            // _mm256_packs_epi32 operates on 128-bit lanes independently, creating [0,1,2,3,0,1,2,3|4,5,6,7,4,5,6,7]
            // We need to permute to get [0,1,2,3,4,5,6,7|...]
            let v_packed = _mm256_packs_epi32(v_clamped, v_clamped);
            let v_permuted = _mm256_permute4x64_epi64(v_packed, 0b11_01_10_00); // Reorder lanes: [0,2,1,3]
            let v_result_128 = _mm256_castsi256_si128(v_permuted);
            _mm_storeu_si128(coeffs.as_mut_ptr().add(offset) as *mut __m128i, v_result_128);
        }

        for coef in &mut coeffs[chunks * 8..] {
            let value = (*coef as i32 * multiplier + add) >> shift;
            *coef = value.clamp(-32768, 32767) as i16;
        }
    } else {
        let neg_shift = -shift;
        for coef in coeffs.iter_mut() {
            let value = (*coef as i32 * multiplier) << neg_shift;
            *coef = value.clamp(-32768, 32767) as i16;
        }
    }
}

/// Generic inverse transform dispatch
pub fn inverse_transform(
    coeffs: &[i16],
    output: &mut [i16],
    size: usize,
    bit_depth: u8,
    is_intra_4x4_luma: bool,
) {
    match size {
        4 => {
            let mut in_arr = [0i16; 16];
            let mut out_arr = [0i16; 16];
            in_arr[..coeffs.len().min(16)].copy_from_slice(&coeffs[..coeffs.len().min(16)]);

            if is_intra_4x4_luma {
                super::transform_simd::idst4_optimized(&in_arr, &mut out_arr, bit_depth);
            } else {
                super::transform_simd::idct4_optimized(&in_arr, &mut out_arr, bit_depth);
            }

            output[..16].copy_from_slice(&out_arr);
        }
        8 => {
            let mut in_arr = [0i16; 64];
            let mut out_arr = [0i16; 64];
            in_arr[..coeffs.len().min(64)].copy_from_slice(&coeffs[..coeffs.len().min(64)]);
            super::transform_simd::idct8_optimized(&in_arr, &mut out_arr, bit_depth);
            output[..64].copy_from_slice(&out_arr);
        }
        16 => {
            let mut in_arr = [0i16; 256];
            let mut out_arr = [0i16; 256];
            in_arr[..coeffs.len().min(256)].copy_from_slice(&coeffs[..coeffs.len().min(256)]);
            super::transform_simd::idct16_optimized(&in_arr, &mut out_arr, bit_depth);
            output[..256].copy_from_slice(&out_arr);
        }
        32 => {
            let mut in_arr = [0i16; 1024];
            let mut out_arr = [0i16; 1024];
            in_arr[..coeffs.len().min(1024)].copy_from_slice(&coeffs[..coeffs.len().min(1024)]);
            // Use SIMD-optimized version for 32x32
            super::transform_simd::idct32_optimized(&in_arr, &mut out_arr, bit_depth);
            output[..1024].copy_from_slice(&out_arr);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_idct4_dc_only() {
        // With DC coefficient = 64 (after dequant), all output samples should be equal
        let mut coeffs = [0i16; 16];
        coeffs[0] = 64; // DC coefficient at (0,0)

        let mut output = [0i16; 16];
        idct4(&coeffs, &mut output, 8);

        println!("IDCT4 output with DC=64:");
        for y in 0..4 {
            println!("  {:?}", &output[y * 4..y * 4 + 4]);
        }

        // For DC-only input, DCT should produce uniform output
        // DC value propagates as: DC * 64 * 64 >> 7 >> 12 = DC >> 7 = 0 for DC=64
        // Actually: 64 * 64 >> 7 = 32 for first pass per sample
        // Then 32 * 64 * 4 >> 12 = 32 for each output
        // Let me just verify all outputs are equal
        let first = output[0];
        for &v in &output {
            assert_eq!(v, first, "DC-only should produce uniform output");
        }
    }

    #[test]
    fn test_idst4_dc_only() {
        let mut coeffs = [0i16; 16];
        coeffs[0] = 64; // DC coefficient

        let mut output = [0i16; 16];
        idst4(&coeffs, &mut output, 8);

        println!("IDST4 output with DC=64:");
        for y in 0..4 {
            println!("  {:?}", &output[y * 4..y * 4 + 4]);
        }

        // DST doesn't produce uniform output for DC input (unlike DCT)
        // Just verify it produces non-zero values
        let non_zero = output.iter().any(|&v| v != 0);
        assert!(
            non_zero,
            "IDST4 should produce non-zero output for DC input"
        );
    }

    #[test]
    fn test_idst4_with_real_coeffs() {
        // Use actual coefficients from our first decoded TU
        let mut coeffs = [0i16; 16];
        // Dequantized coeffs: [144, -3024, -288, 0, -144, -432, -288, 0, 144, -576, 432, 0, -144, 288, 288, 0]
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

        let mut output = [0i16; 16];
        idst4(&coeffs, &mut output, 8);

        println!("IDST4 output with real coefficients:");
        for y in 0..4 {
            println!("  {:?}", &output[y * 4..y * 4 + 4]);
        }
        println!(
            "Expected residuals: [-18, -23, -4, 23, -41, -24, 11, 22, -28, -22, 3, 18, -33, -34, 3, 44]"
        );
    }
}
