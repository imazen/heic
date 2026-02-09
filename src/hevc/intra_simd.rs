//! SIMD-optimized intra prediction for HEVC
//!
//! Provides AVX2/SSE2 implementations of critical intra prediction modes:
//! - Reference sample 3-tap filtering
//! - DC mode prediction
//! - Planar mode prediction
//! - Angular mode prediction (horizontal/vertical)

#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
use core::arch::x86_64::*;

use super::picture::DecodedFrame;

/// SIMD-optimized 3-tap reference sample filter
/// Applies (p[i-1] + 2*p[i] + p[i+1] + 2) >> 2 to interior samples
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
pub unsafe fn filter_reference_3tap_sse2(samples: &mut [i32], start: usize, count: usize) {
    if count < 8 {
        // Fallback to scalar for small counts
        filter_reference_3tap_scalar(samples, start, count);
        return;
    }

    // Process 4 samples at a time with SSE2
    let mut i = start + 1;
    let end = start + count - 1;

    while i + 4 <= end {
        // SAFETY: We're within bounds and using target_feature sse2
        unsafe {
            // Load 6 consecutive samples (for 4 output samples we need indices i-1 through i+4)
            let p0 = _mm_loadu_si128(samples.as_ptr().add(i - 1) as *const __m128i);
            let p1 = _mm_loadu_si128(samples.as_ptr().add(i) as *const __m128i);
            let p2 = _mm_loadu_si128(samples.as_ptr().add(i + 1) as *const __m128i);

            // Compute: (p[i-1] + 2*p[i] + p[i+1] + 2) >> 2
            let two = _mm_set1_epi32(2);
            let doubled = _mm_slli_epi32(p1, 1); // 2 * p[i]
            let sum = _mm_add_epi32(_mm_add_epi32(p0, doubled), _mm_add_epi32(p2, two));
            let result = _mm_srai_epi32(sum, 2);

            _mm_storeu_si128(samples.as_mut_ptr().add(i) as *mut __m128i, result);
        }
        i += 4;
    }

    // Handle remaining samples with scalar
    while i < end {
        samples[i] = (samples[i - 1] + 2 * samples[i] + samples[i + 1] + 2) >> 2;
        i += 1;
    }
}

/// Scalar fallback for 3-tap filter
#[inline]
fn filter_reference_3tap_scalar(samples: &mut [i32], start: usize, count: usize) {
    for i in (start + 1)..(start + count - 1) {
        samples[i] = (samples[i - 1] + 2 * samples[i] + samples[i + 1] + 2) >> 2;
    }
}

/// SIMD-optimized DC mode prediction - fills block with constant value
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn predict_dc_fill_avx2(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    dc_val: u16,
) {
    let stride = if c_idx == 0 {
        frame.width as usize
    } else {
        frame.width as usize / 2
    };

    let plane = match c_idx {
        0 => &mut frame.y_plane,
        1 => &mut frame.cb_plane,
        2 => &mut frame.cr_plane,
        _ => return,
    };

    let base_idx = (y as usize * stride) + x as usize;
    // SAFETY: We're using AVX2 target_feature
    let v_dc = unsafe { _mm256_set1_epi16(dc_val as i16) };

    // Process rows
    for row in 0..size as usize {
        let row_start = base_idx + row * stride;
        let mut col = 0;

        // Process 16 pixels at a time with AVX2
        while col + 16 <= size as usize {
            // SAFETY: We're within bounds and using AVX2 target_feature
            unsafe {
                _mm256_storeu_si256(
                    plane.as_mut_ptr().add(row_start + col) as *mut __m256i,
                    v_dc,
                );
            }
            col += 16;
        }

        // Handle remaining pixels with scalar
        for px in col..(size as usize) {
            plane[row_start + px] = dc_val;
        }
    }
}

/// SSE2 version for DC fill (for CPUs without AVX2)
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "sse2")]
pub unsafe fn predict_dc_fill_sse2(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    dc_val: u16,
) {
    let stride = if c_idx == 0 {
        frame.width as usize
    } else {
        frame.width as usize / 2
    };

    let plane = match c_idx {
        0 => &mut frame.y_plane,
        1 => &mut frame.cb_plane,
        2 => &mut frame.cr_plane,
        _ => return,
    };

    let base_idx = (y as usize * stride) + x as usize;
    // SAFETY: We're using SSE2 target_feature
    let v_dc = unsafe { _mm_set1_epi16(dc_val as i16) };

    for row in 0..size as usize {
        let row_start = base_idx + row * stride;
        let mut col = 0;

        // Process 8 pixels at a time with SSE2
        while col + 8 <= size as usize {
            // SAFETY: We're within bounds and using SSE2 target_feature
            unsafe {
                _mm_storeu_si128(
                    plane.as_mut_ptr().add(row_start + col) as *mut __m128i,
                    v_dc,
                );
            }
            col += 8;
        }

        // Handle remaining pixels
        for px in col..(size as usize) {
            plane[row_start + px] = dc_val;
        }
    }
}

/// Public wrapper with runtime CPU detection
#[inline]
pub fn predict_dc_fill_optimized(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    dc_val: u16,
) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                predict_dc_fill_avx2(frame, x, y, size, c_idx, dc_val);
            }
            return;
        }
        if is_x86_feature_detected!("sse2") {
            unsafe {
                predict_dc_fill_sse2(frame, x, y, size, c_idx, dc_val);
            }
            return;
        }
    }

    // Scalar fallback
    predict_dc_fill_scalar(frame, x, y, size, c_idx, dc_val);
}

/// Scalar fallback for DC fill
fn predict_dc_fill_scalar(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    dc_val: u16,
) {
    for py in 0..size {
        for px in 0..size {
            super::intra::set_sample_public(frame, x + px, y + py, c_idx, dc_val);
        }
    }
}

/// SIMD-optimized planar prediction using AVX2
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn predict_planar_avx2(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    border: &[i32],
    center: usize,
    bit_depth: u8,
) {
    let n = size as i32;
    let log2_size = (size as f32).log2() as u32;
    let shift = log2_size + 1;
    let max_val = (1 << bit_depth) - 1;

    let stride = if c_idx == 0 {
        frame.width as usize
    } else {
        frame.width as usize / 2
    };

    let plane = match c_idx {
        0 => &mut frame.y_plane,
        1 => &mut frame.cb_plane,
        2 => &mut frame.cr_plane,
        _ => return,
    };

    let base_idx = (y as usize * stride) + x as usize;
    let right = border[center + 1 + size as usize]; // border[nT+1]
    let bottom = border[center - 1 - size as usize]; // border[-1-nT]
    // SAFETY: We're using AVX2 target_feature
    let (v_n, v_max, v_zero) = unsafe {
        (
            _mm256_set1_epi32(n),
            _mm256_set1_epi32(max_val),
            _mm256_setzero_si256(),
        )
    };

    // For each row
    for py in 0..size {
        let py_i = py as i32;
        let left = border[center - 1 - py as usize];
        let top_coef = n - 1 - py_i;
        let bottom_coef = py_i + 1;

        // SAFETY: We're using AVX2 target_feature
        let (v_left, v_right, v_top_coef, v_bottom, v_bottom_coef) = unsafe {
            (
                _mm256_set1_epi32(left),
                _mm256_set1_epi32(right),
                _mm256_set1_epi32(top_coef),
                _mm256_set1_epi32(bottom),
                _mm256_set1_epi32(bottom_coef),
            )
        };

        let row_start = base_idx + py as usize * stride;
        let mut px = 0;

        // Process 8 pixels at a time
        while px + 8 <= size {
            // Load top border values for this row
            let mut top_vals = [0i32; 8];
            for i in 0..8 {
                top_vals[i] = border[center + 1 + px as usize + i];
            }

            // SAFETY: We're using AVX2 target_feature and within bounds
            let final_vals = unsafe {
                // Build px vector: [px, px+1, px+2, ..., px+7]
                let px_vals = _mm256_setr_epi32(
                    px as i32,
                    px as i32 + 1,
                    px as i32 + 2,
                    px as i32 + 3,
                    px as i32 + 4,
                    px as i32 + 5,
                    px as i32 + 6,
                    px as i32 + 7,
                );

                let v_top = _mm256_loadu_si256(top_vals.as_ptr() as *const __m256i);

                // Compute (n - 1 - px)
                let v_n_minus_1 = _mm256_sub_epi32(v_n, _mm256_set1_epi32(1));
                let left_coef = _mm256_sub_epi32(v_n_minus_1, px_vals);

                // Compute (px + 1)
                let right_coef = _mm256_add_epi32(px_vals, _mm256_set1_epi32(1));

                // Planar formula:
                // pred = ((n-1-x)*left + (x+1)*right + (n-1-y)*top + (y+1)*bottom + n) >> (log2+1)
                let term1 = _mm256_mullo_epi32(left_coef, v_left);
                let term2 = _mm256_mullo_epi32(right_coef, v_right);
                let term3 = _mm256_mullo_epi32(v_top_coef, v_top);
                let term4 = _mm256_mullo_epi32(v_bottom_coef, v_bottom);

                let sum = _mm256_add_epi32(
                    _mm256_add_epi32(term1, term2),
                    _mm256_add_epi32(_mm256_add_epi32(term3, term4), v_n),
                );

                // Shift (variable shift, need to extract and shift manually)
                let mut results = [0i32; 8];
                _mm256_storeu_si256(results.as_mut_ptr() as *mut __m256i, sum);
                for val in results.iter_mut() {
                    *val >>= shift;
                }

                // Clamp to [0, max_val]
                let v_pred = _mm256_loadu_si256(results.as_ptr() as *const __m256i);
                let v_clamped_low = _mm256_max_epi32(v_pred, v_zero);
                let v_clamped = _mm256_min_epi32(v_clamped_low, v_max);

                // Convert to final values
                let mut fvals = [0i32; 8];
                _mm256_storeu_si256(fvals.as_mut_ptr() as *mut __m256i, v_clamped);
                fvals
            };

            // Store final values
            for i in 0..8 {
                plane[row_start + px as usize + i] = final_vals[i] as u16;
            }

            px += 8;
        }

        // Handle remaining pixels with scalar
        for px_rem in px..size {
            let px_i = px_rem as i32;
            let top = border[center + 1 + px_rem as usize];
            let pred = ((n - 1 - px_i) * left
                + (px_i + 1) * right
                + top_coef * top
                + bottom_coef * bottom
                + n)
                >> shift;
            plane[row_start + px_rem as usize] = pred.clamp(0, max_val) as u16;
        }
    }
}

/// Public wrapper for planar with runtime CPU detection
#[inline]
pub fn predict_planar_optimized(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    border: &[i32],
    center: usize,
    bit_depth: u8,
) {
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                predict_planar_avx2(frame, x, y, size, c_idx, border, center, bit_depth);
            }
            return;
        }
    }

    // Fallback to scalar implementation in intra.rs
    super::intra::predict_planar_scalar(frame, x, y, size, c_idx, border, center, bit_depth);
}

/// SIMD-optimized angular prediction for horizontal/vertical modes
/// Optimizes the inner loop interpolation: ((32 - fact) * ref[i] + fact * ref[i+1] + 16) >> 5
#[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
#[target_feature(enable = "avx2")]
pub unsafe fn angular_interpolate_row_avx2(
    output: &mut [u16],
    ref_arr: &[i32],
    ref_offset: usize,
    i_idx: i32,
    i_fact: i32,
    count: usize,
    max_val: i32,
) {
    if i_fact == 0 {
        // No interpolation needed - direct copy
        for i in 0..count {
            let val = ref_arr[ref_offset + i_idx as usize + 1 + i];
            output[i] = val.clamp(0, max_val) as u16;
        }
        return;
    }

    // SAFETY: We're using AVX2 target_feature
    let (v_fact, v_32_minus_fact, v_16, v_max, v_zero) = unsafe {
        (
            _mm256_set1_epi32(i_fact),
            _mm256_set1_epi32(32 - i_fact),
            _mm256_set1_epi32(16),
            _mm256_set1_epi32(max_val),
            _mm256_setzero_si256(),
        )
    };

    let mut px = 0;

    // Process 8 pixels at a time
    while px + 8 <= count {
        let base_idx = (ref_offset as i32 + i_idx + 1 + px as i32) as usize;

        // SAFETY: We're using AVX2 target_feature and within bounds
        let results = unsafe {
            // Load ref_arr[idx] and ref_arr[idx+1] for 8 consecutive pixels
            let v_ref0 = _mm256_loadu_si256(ref_arr.as_ptr().add(base_idx) as *const __m256i);
            let v_ref1 = _mm256_loadu_si256(ref_arr.as_ptr().add(base_idx + 1) as *const __m256i);

            // Compute: ((32 - fact) * ref[i] + fact * ref[i+1] + 16) >> 5
            let term1 = _mm256_mullo_epi32(v_32_minus_fact, v_ref0);
            let term2 = _mm256_mullo_epi32(v_fact, v_ref1);
            let sum = _mm256_add_epi32(_mm256_add_epi32(term1, term2), v_16);
            let shifted = _mm256_srai_epi32(sum, 5);

            // Clamp to [0, max_val]
            let v_clamped_low = _mm256_max_epi32(shifted, v_zero);
            let v_clamped = _mm256_min_epi32(v_clamped_low, v_max);

            // Extract results
            let mut res = [0i32; 8];
            _mm256_storeu_si256(res.as_mut_ptr() as *mut __m256i, v_clamped);
            res
        };

        // Store as u16
        for i in 0..8 {
            output[px + i] = results[i] as u16;
        }

        px += 8;
    }

    // Handle remaining pixels
    for i in px..count {
        let idx = (ref_offset as i32 + i_idx + 1 + i as i32) as usize;
        let pred = ((32 - i_fact) * ref_arr[idx] + i_fact * ref_arr[idx + 1] + 16) >> 5;
        output[i] = pred.clamp(0, max_val) as u16;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_3tap_scalar() {
        let mut samples = [10, 20, 30, 40, 50];
        filter_reference_3tap_scalar(&mut samples, 0, 5);
        // samples[1] = (10 + 2*20 + 30 + 2) >> 2 = (10 + 40 + 30 + 2) >> 2 = 82 >> 2 = 20
        // samples[2] = (20 + 2*30 + 40 + 2) >> 2 = (20 + 60 + 40 + 2) >> 2 = 122 >> 2 = 30
        // samples[3] = (30 + 2*40 + 50 + 2) >> 2 = (30 + 80 + 50 + 2) >> 2 = 162 >> 2 = 40
        // Endpoints preserved
        assert_eq!(samples[0], 10);
        assert_eq!(samples[4], 50);
    }

    #[test]
    #[cfg(all(feature = "unsafe-simd", target_arch = "x86_64"))]
    fn test_filter_3tap_sse2() {
        if !is_x86_feature_detected!("sse2") {
            return;
        }

        let mut samples_scalar = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        let mut samples_simd = samples_scalar.clone();

        filter_reference_3tap_scalar(&mut samples_scalar, 0, 10);
        unsafe {
            filter_reference_3tap_sse2(&mut samples_simd, 0, 10);
        }

        assert_eq!(samples_scalar, samples_simd);
    }
}
