//! Intra prediction for HEVC

use super::picture::DecodedFrame;
use super::slice::IntraPredMode;

/// Tracks which samples in the frame have been reconstructed.
/// Used for intra prediction reference sample availability (H.265 8.4.4.2.1).
pub(super) struct ReconstructionMap {
    luma: Vec<u8>,
    cb: Vec<u8>,
    cr: Vec<u8>,
    width: u32,
    height: u32,
    chroma_width: u32,
    chroma_height: u32,
}

impl ReconstructionMap {
    pub(super) fn new(width: u32, height: u32) -> Self {
        let luma_bits = (width * height) as usize;
        let luma_bytes = luma_bits.div_ceil(8);
        let cw = width.div_ceil(2);
        let ch = height.div_ceil(2);
        let chroma_bits = (cw * ch) as usize;
        let chroma_bytes = chroma_bits.div_ceil(8);

        Self {
            luma: vec![0; luma_bytes],
            cb: vec![0; chroma_bytes],
            cr: vec![0; chroma_bytes],
            width,
            height,
            chroma_width: cw,
            chroma_height: ch,
        }
    }

    pub(super) fn mark_reconstructed(&mut self, x: u32, y: u32, size: u32, c_idx: u8) {
        let (map, w, h) = match c_idx {
            0 => (&mut self.luma, self.width, self.height),
            1 => (&mut self.cb, self.chroma_width, self.chroma_height),
            2 => (&mut self.cr, self.chroma_width, self.chroma_height),
            _ => return,
        };

        for dy in 0..size {
            let py = y + dy;
            if py >= h {
                break;
            }
            for dx in 0..size {
                let px = x + dx;
                if px >= w {
                    break;
                }
                let idx = (py * w + px) as usize;
                map[idx / 8] |= 1 << (idx % 8);
            }
        }
    }

    fn is_reconstructed(&self, x: u32, y: u32, c_idx: u8) -> bool {
        let (map, w, h) = match c_idx {
            0 => (&self.luma, self.width, self.height),
            1 => (&self.cb, self.chroma_width, self.chroma_height),
            2 => (&self.cr, self.chroma_width, self.chroma_height),
            _ => return false,
        };

        if x >= w || y >= h {
            return false;
        }

        let idx = (y * w + x) as usize;
        (map[idx / 8] >> (idx % 8)) & 1 != 0
    }
}

pub const MAX_INTRA_PRED_BLOCK_SIZE: usize = 64;

/// Intra prediction angle table (H.265 Table 8-4)
/// Index 0-1 are placeholders, modes 2-34 have actual angles
pub static INTRA_PRED_ANGLE: [i16; 35] = [
    0, 0, // modes 0, 1 (planar, DC)
    32, 26, 21, 17, 13, 9, 5, 2, // modes 2-9
    0, // mode 10 (horizontal)
    -2, -5, -9, -13, -17, -21, -26, // modes 11-17
    -32, // mode 18 (diagonal down-left)
    -26, -21, -17, -13, -9, -5, -2, // modes 19-25
    0,  // mode 26 (vertical)
    2, 5, 9, 13, 17, 21, 26, // modes 27-33
    32, // mode 34 (diagonal down-right)
];

/// Inverse angle table for negative angles (modes 11-17 and 19-25)
/// Used to extend reference samples for negative angle prediction
pub static INV_ANGLE: [i32; 15] = [
    -4096, -1638, -910, -630, -482, -390, -315, // modes 11-17
    -256, // mode 18
    -315, -390, -482, -630, -910, -1638, -4096, // modes 19-25
];

/// Get inverse angle for a mode (for negative angle modes only)
#[inline]
fn get_inv_angle(mode: u8) -> i32 {
    if (11..=25).contains(&mode) {
        INV_ANGLE[(mode - 11) as usize]
    } else {
        0
    }
}

/// Reference sample filtering (H.265 8.4.4.2.3)
/// Applies 3-tap smoothing filter or strong intra smoothing to border samples
/// BEFORE prediction. This is critical for correct intra prediction.
///
/// Per libde265: only applied for luma in 4:2:0 mode (cIdx==0 || ChromaArrayType==444)
fn filter_reference_samples(
    border: &mut [i32],
    center: usize,
    size: u32,
    mode: IntraPredMode,
    c_idx: u8,
    strong_intra_smoothing_enabled: bool,
    bit_depth: u8,
) {
    let n = size as i32;
    let mode_val = mode.as_u8() as i32;

    // No filtering for 4:2:0 chroma (only luma)
    if c_idx != 0 {
        return;
    }

    // No filtering for DC mode or 4x4 blocks
    if mode == IntraPredMode::Dc || size == 4 {
        return;
    }

    // Compute minimum distance to horizontal (mode 10) and vertical (mode 26)
    let min_dist_ver_hor = (mode_val - 26).abs().min((mode_val - 10).abs());

    // Determine filterFlag based on block size (H.265 Table 8-3 intraHorVerDistThres)
    let filter_flag = match size {
        8 => min_dist_ver_hor > 7,
        16 => min_dist_ver_hor > 1,
        32 => min_dist_ver_hor > 0,
        _ => false, // 64 and larger: no filtering
    };

    if !filter_flag {
        return;
    }

    // Check for strong intra smoothing (bilinear interpolation)
    let bi_int_flag = strong_intra_smoothing_enabled
        && c_idx == 0
        && size == 32
        && {
            // Smoothness check: boundary samples should be approximately linear
            let threshold = 1i32 << (bit_depth as i32 - 5);
            let p0 = border[center]; // top-left corner
            let p_top_end = border[center + 2 * n as usize]; // top-right corner (p[+64])
            let p_top_mid = border[center + n as usize]; // top midpoint (p[+32])
            let p_left_end = border[center - 2 * n as usize]; // bottom-left corner (p[-64])
            let p_left_mid = border[center - n as usize]; // left midpoint (p[-32])

            (p0 + p_top_end - 2 * p_top_mid).abs() < threshold
                && (p0 + p_left_end - 2 * p_left_mid).abs() < threshold
        };

    // Allocate temporary filtered array
    let total = 4 * size as usize + 1;
    let f_center = 2 * size as usize;
    let mut filtered = vec![0i32; total];

    if bi_int_flag {
        // Strong intra smoothing: bilinear interpolation from corners
        // Only for size==32, so 2*nT = 64, shift = 6, rounding = 32
        let p0 = border[center];
        let p_top_end = border[center + 2 * n as usize];
        let p_left_end = border[center - 2 * n as usize];
        let two_n = 2 * n; // = 64 for size=32

        filtered[f_center] = p0; // top-left corner preserved
        filtered[0] = p_left_end; // bottom-left preserved
        filtered[total - 1] = p_top_end; // top-right preserved

        // libde265: pF[-i] = p[0] + ((i*(p[-64]-p[0])+32)>>6)
        //           pF[+i] = p[0] + ((i*(p[+64]-p[0])+32)>>6)
        for i in 1..(two_n as usize) {
            // Left side: interpolate from p0 to p_left_end
            filtered[f_center - i] = p0 + ((i as i32 * (p_left_end - p0) + (two_n / 2)) >> 6);
            // Top side: interpolate from p0 to p_top_end
            filtered[f_center + i] = p0 + ((i as i32 * (p_top_end - p0) + (two_n / 2)) >> 6);
        }
    } else {
        // Regular 3-tap filter: f[i] = (p[i-1] + 2*p[i] + p[i+1] + 2) >> 2
        // Copy border to filtered first
        for i in 0..total {
            filtered[i] = border[center - 2 * n as usize + i];
        }

        // Note: SIMD 3-tap filter disabled - overhead too high for small filter sizes

        // Scalar fallback - filter all interior samples
        for i in 1..=(total - 2) {
            filtered[i] = (filtered[i - 1] + 2 * filtered[i] + filtered[i + 1] + 2) >> 2;
        }
    }

    // Copy filtered samples back to border
    for i in 0..total {
        let border_idx = center - 2 * n as usize + i;
        border[border_idx] = filtered[i];
    }
}

/// Perform intra prediction for a block
pub fn predict_intra(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    log2_size: u8,
    mode: IntraPredMode,
    c_idx: u8, // 0=Y, 1=Cb, 2=Cr
    reco_map: &ReconstructionMap,
    strong_intra_smoothing_enabled: bool,
) {
    let size = 1u32 << log2_size;

    // Get reference samples (border pixels)
    let mut border = [0i32; 4 * MAX_INTRA_PRED_BLOCK_SIZE + 1];
    let border_center = 2 * MAX_INTRA_PRED_BLOCK_SIZE;

    fill_border_samples(frame, x, y, size, c_idx, &mut border, border_center, reco_map);

    // Apply reference sample filtering (H.265 8.4.4.2.3) BEFORE prediction
    if !std::env::var("HEVC_NO_REF_FILTER").is_ok() {
        filter_reference_samples(
            &mut border,
            border_center,
            size,
            mode,
            c_idx,
            strong_intra_smoothing_enabled,
            frame.bit_depth as u8,
        );
    }

    // Apply prediction based on mode
    match mode {
        IntraPredMode::Planar => {
            predict_planar(frame, x, y, size, c_idx, &border, border_center);
        }
        IntraPredMode::Dc => {
            predict_dc(frame, x, y, size, c_idx, &border, border_center);
        }
        _ => {
            let mode_val = mode.as_u8();
            predict_angular(frame, x, y, size, c_idx, mode_val, &border, border_center);
        }
    }

}

/// Fill border samples from neighboring pixels using z-scan availability (H.265 8.4.4.2.1)
///
/// Uses the ReconstructionMap to determine which reference samples have actually
/// been decoded, rather than relying on pixel values as sentinels.
fn fill_border_samples(
    frame: &DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    border: &mut [i32],
    center: usize,
    reco_map: &ReconstructionMap,
) {
    let (frame_w, frame_h) = if c_idx == 0 {
        (frame.width, frame.height)
    } else {
        (frame.width / 2, frame.height / 2)
    };

    let default_val = 1i32 << (frame.bit_depth - 1);

    // Helper: check if a sample at (sx, sy) is available for reference
    let is_avail = |sx: u32, sy: u32| -> bool {
        sx < frame_w && sy < frame_h && reco_map.is_reconstructed(sx, sy, c_idx)
    };

    // Build availability + sample arrays per H.265 8.4.4.2.1
    // Total 4*size + 1 samples: 2*size left (bottom-left to top), corner, 2*size top (left to top-right)
    let total = 4 * size as usize + 1;
    let mut avail = vec![false; total];
    let mut samples = vec![0i32; total];

    // Index mapping: [0..2*size-1] = bottom-left to left, [2*size] = corner, [2*size+1..4*size] = top to top-right
    let corner_idx = 2 * size as usize;

    // Bottom-left samples (index 0 = bottom-most, going up)
    for i in 0..(2 * size) {
        let sy = y + 2 * size - 1 - i;
        if x > 0 && is_avail(x - 1, sy) {
            avail[i as usize] = true;
            samples[i as usize] = get_sample(frame, x - 1, sy, c_idx) as i32;
        }
    }

    // Top-left corner
    if x > 0 && y > 0 && is_avail(x - 1, y - 1) {
        avail[corner_idx] = true;
        samples[corner_idx] = get_sample(frame, x - 1, y - 1, c_idx) as i32;
    }

    // Top and top-right samples
    for i in 0..(2 * size) {
        let sx = x + i;
        if y > 0 && is_avail(sx, y - 1) {
            avail[corner_idx + 1 + i as usize] = true;
            samples[corner_idx + 1 + i as usize] = get_sample(frame, sx, y - 1, c_idx) as i32;
        }
    }

    // Reference sample substitution (H.265 8.4.4.2.2)
    // Find first available sample scanning from bottom-left to top-right
    let mut first_avail_val = None;
    for i in 0..total {
        if avail[i] {
            first_avail_val = Some(samples[i]);
            break;
        }
    }
    let subst_val = first_avail_val.unwrap_or(default_val);

    // Substitute unavailable samples: scan from bottom-left to top-right,
    // replacing unavailable with the nearest available to their left (in scan order)
    let mut last_val = subst_val;
    for i in 0..total {
        if avail[i] {
            last_val = samples[i];
        } else {
            samples[i] = last_val;
        }
    }

    // Copy into border array format:
    // border[center - 1 - i] = left[i] (i=0 is top-most left, i=2*size-1 is bottom-most)
    // border[center] = corner
    // border[center + 1 + i] = top[i]
    for i in 0..(2 * size as usize) {
        // samples index for left: bottom-most is index 0, top-most is index 2*size-1
        // border index: border[center-1] = top-most left (y), border[center-1-i] goes down
        // So border[center-1-i] = samples[2*size-1-i]
        border[center - 1 - i] = samples[2 * size as usize - 1 - i];
    }
    border[center] = samples[corner_idx];
    for i in 0..(2 * size as usize) {
        border[center + 1 + i] = samples[corner_idx + 1 + i];
    }
}

/// Substitute unavailable reference samples (H.265 8.4.4.2.2)
fn reference_sample_substitution(border: &mut [i32], center: usize, size: usize) {
    // Find first available sample
    let mut first_avail = None;

    // Search from bottom-left to top-right
    for i in (0..(2 * size)).rev() {
        if border[center - 1 - i] != 0 {
            first_avail = Some(border[center - 1 - i]);
            break;
        }
    }

    if first_avail.is_none() && border[center] != 0 {
        first_avail = Some(border[center]);
    }

    if first_avail.is_none() {
        for i in 0..(2 * size) {
            if border[center + 1 + i] != 0 {
                first_avail = Some(border[center + 1 + i]);
                break;
            }
        }
    }

    // Substitute unavailable samples with first available
    let val = first_avail.unwrap_or(1 << 7); // Default to mid-gray if all unavailable

    for i in 0..(2 * size) {
        if border[center - 1 - i] == 0 {
            border[center - 1 - i] = val;
        }
    }
    if border[center] == 0 {
        border[center] = val;
    }
    for i in 0..(2 * size) {
        if border[center + 1 + i] == 0 {
            border[center + 1 + i] = val;
        }
    }
}

/// Get a sample from the frame
#[inline]
fn get_sample(frame: &DecodedFrame, x: u32, y: u32, c_idx: u8) -> u16 {
    match c_idx {
        0 => frame.get_y(x, y),
        1 => frame.get_cb(x, y),
        2 => frame.get_cr(x, y),
        _ => 0,
    }
}

/// Set a sample in the frame
#[inline]
fn set_sample(frame: &mut DecodedFrame, x: u32, y: u32, c_idx: u8, value: u16) {
    match c_idx {
        0 => frame.set_y(x, y, value),
        1 => frame.set_cb(x, y, value),
        2 => frame.set_cr(x, y, value),
        _ => {}
    }
}

/// Public version of set_sample for SIMD code
#[inline]
pub(super) fn set_sample_public(frame: &mut DecodedFrame, x: u32, y: u32, c_idx: u8, value: u16) {
    set_sample(frame, x, y, c_idx, value);
}

/// Planar prediction (mode 0) - H.265 8.4.4.2.4
fn predict_planar(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    border: &[i32],
    center: usize,
) {
    // Use scalar version (SIMD version had too much overhead for typical HEVC block sizes)
    predict_planar_scalar(
        frame,
        x,
        y,
        size,
        c_idx,
        border,
        center,
        frame.bit_depth as u8,
    );
}

/// Scalar fallback for planar prediction (called from SIMD code)
pub(super) fn predict_planar_scalar(
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

    for py in 0..size {
        for px in 0..size {
            let px_i = px as i32;
            let py_i = py as i32;

            // Planar formula:
            // pred = ((nT-1-x)*border[-1-y] + (x+1)*border[nT+1] +
            //         (nT-1-y)*border[1+x] + (y+1)*border[-1-nT] + nT) >> (log2(nT)+1)
            let left = border[center - 1 - py as usize];
            let right = border[center + 1 + size as usize]; // border[nT+1]
            let top = border[center + 1 + px as usize];
            let bottom = border[center - 1 - size as usize]; // border[-1-nT]

            let pred = ((n - 1 - px_i) * left
                + (px_i + 1) * right
                + (n - 1 - py_i) * top
                + (py_i + 1) * bottom
                + n)
                >> (log2_size + 1);

            let value = pred.clamp(0, (1 << bit_depth) - 1) as u16;
            set_sample(frame, x + px, y + py, c_idx, value);
        }
    }
}

/// DC prediction (mode 1) - H.265 8.4.4.2.5
fn predict_dc(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    border: &[i32],
    center: usize,
) {
    let n = size as i32;
    let log2_size = (size as f32).log2() as u32;

    // Calculate DC value as average of top and left samples
    let mut dc_val = 0i32;
    for i in 0..size {
        dc_val += border[center + 1 + i as usize]; // top
        dc_val += border[center - 1 - i as usize]; // left
    }
    dc_val = (dc_val + n) >> (log2_size + 1);

    let max_val = (1 << frame.bit_depth) - 1;

    // Apply DC filtering for luma and small blocks
    if c_idx == 0 && size < 32 {
        // Corner pixel: average of corner neighbors and 2*DC
        let corner = (border[center - 1] + 2 * dc_val + border[center + 1] + 2) >> 2;
        set_sample(frame, x, y, c_idx, corner.clamp(0, max_val) as u16);

        // Top edge: blend top border with DC
        for px in 1..size {
            let pred = (border[center + 1 + px as usize] + 3 * dc_val + 2) >> 2;
            set_sample(frame, x + px, y, c_idx, pred.clamp(0, max_val) as u16);
        }

        // Left edge: blend left border with DC
        for py in 1..size {
            let pred = (border[center - 1 - py as usize] + 3 * dc_val + 2) >> 2;
            set_sample(frame, x, y + py, c_idx, pred.clamp(0, max_val) as u16);
        }

        // Interior: pure DC
        let dc_u16 = dc_val.clamp(0, max_val) as u16;
        for py in 1..size {
            for px in 1..size {
                set_sample(frame, x + px, y + py, c_idx, dc_u16);
            }
        }
    } else {
        // No filtering: fill entire block with DC value
        let dc_u16 = dc_val.clamp(0, max_val) as u16;
        for py in 0..size {
            for px in 0..size {
                set_sample(frame, x + px, y + py, c_idx, dc_u16);
            }
        }
    }
}

/// Angular prediction (modes 2-34) - H.265 8.4.4.2.6
#[allow(clippy::too_many_arguments)]
fn predict_angular(
    frame: &mut DecodedFrame,
    x: u32,
    y: u32,
    size: u32,
    c_idx: u8,
    mode: u8,
    border: &[i32],
    center: usize,
) {
    let n = size as i32;
    let intra_pred_angle = INTRA_PRED_ANGLE[mode as usize] as i32;

    // Build reference array
    let mut ref_arr = [0i32; 4 * MAX_INTRA_PRED_BLOCK_SIZE + 1];
    let ref_center = 2 * MAX_INTRA_PRED_BLOCK_SIZE;

    let max_val = (1 << frame.bit_depth) - 1;

    if mode >= 18 {
        // Horizontal-ish modes (18-34)
        // Reference is top samples

        // Copy top samples to ref[0..nT]
        for i in 0..=n {
            ref_arr[ref_center + i as usize] = border[center + i as usize];
        }

        if intra_pred_angle < 0 {
            // Negative angle: need to extend reference to the left
            let inv_angle = get_inv_angle(mode);
            let ext = (n * intra_pred_angle) >> 5;

            if ext < -1 {
                for xx in ext..=-1 {
                    // Note: xx is negative, inv_angle is negative for modes 19-25
                    // So xx * inv_angle is positive, giving a positive idx
                    let idx = (xx * inv_angle + 128) >> 8;
                    if idx >= 0 && idx <= (2 * n) {
                        ref_arr[(ref_center as i32 + xx) as usize] =
                            border[(center as i32 - idx) as usize];
                    }
                }
            }
        } else {
            // Positive angle: extend reference to the right
            for xx in (n + 1)..=(2 * n) {
                ref_arr[ref_center + xx as usize] = border[center + xx as usize];
            }
        }

        // Generate prediction
        for py in 0..n {
            for px in 0..n {
                let i_idx = ((py + 1) * intra_pred_angle) >> 5;
                let i_fact = ((py + 1) * intra_pred_angle) & 31;

                let pred = if i_fact != 0 {
                    let idx = (ref_center as i32 + px + i_idx + 1) as usize;
                    ((32 - i_fact) * ref_arr[idx] + i_fact * ref_arr[idx + 1] + 16) >> 5
                } else {
                    let idx = (ref_center as i32 + px + i_idx + 1) as usize;
                    ref_arr[idx]
                };

                let clamped = pred.clamp(0, max_val) as u16;
                set_sample(
                    frame,
                    x + px as u32,
                    y + py as u32,
                    c_idx,
                    clamped,
                );
            }
        }

        // Boundary filter for mode 26 (vertical)
        if mode == 26 && c_idx == 0 && size < 32 {
            for py in 0..n {
                let pred =
                    border[center + 1] + ((border[center - 1 - py as usize] - border[center]) >> 1);
                set_sample(
                    frame,
                    x,
                    y + py as u32,
                    c_idx,
                    pred.clamp(0, max_val) as u16,
                );
            }
        }
    } else {
        // Vertical-ish modes (2-17)
        // Reference is left samples (mirrored)

        // Copy left samples (negated indices) to ref[0..nT]
        for i in 0..=n {
            ref_arr[ref_center + i as usize] = border[center - i as usize];
        }

        if intra_pred_angle < 0 {
            // Negative angle: extend reference
            let inv_angle = get_inv_angle(mode);
            let ext = (n * intra_pred_angle) >> 5;

            if ext < -1 {
                for xx in ext..=-1 {
                    let idx = (xx * inv_angle + 128) >> 8;
                    if idx >= 0 && idx <= (2 * n) {
                        ref_arr[(ref_center as i32 + xx) as usize] =
                            border[(center as i32 + idx) as usize];
                    }
                }
            }
        } else {
            // Positive angle: extend reference
            for xx in (n + 1)..=(2 * n) {
                ref_arr[ref_center + xx as usize] = border[center - xx as usize];
            }
        }

        // Generate prediction (transposed compared to mode >= 18)
        for py in 0..n {
            for px in 0..n {
                let i_idx = ((px + 1) * intra_pred_angle) >> 5;
                let i_fact = ((px + 1) * intra_pred_angle) & 31;

                let pred = if i_fact != 0 {
                    let idx = (ref_center as i32 + py + i_idx + 1) as usize;
                    ((32 - i_fact) * ref_arr[idx] + i_fact * ref_arr[idx + 1] + 16) >> 5
                } else {
                    let idx = (ref_center as i32 + py + i_idx + 1) as usize;
                    ref_arr[idx]
                };

                set_sample(
                    frame,
                    x + px as u32,
                    y + py as u32,
                    c_idx,
                    pred.clamp(0, max_val) as u16,
                );
            }
        }

        // Boundary filter for mode 10 (horizontal)
        if mode == 10 && c_idx == 0 && size < 32 {
            for px in 0..n {
                let pred =
                    border[center - 1] + ((border[center + 1 + px as usize] - border[center]) >> 1);
                set_sample(
                    frame,
                    x + px as u32,
                    y,
                    c_idx,
                    pred.clamp(0, max_val) as u16,
                );
            }
        }
    }
}

/// Fill MPM (Most Probable Mode) candidate list
pub fn fill_mpm_candidates(
    cand_a: IntraPredMode, // left neighbor mode
    cand_b: IntraPredMode, // above neighbor mode
) -> [IntraPredMode; 3] {
    if cand_a == cand_b {
        if cand_a.as_u8() < 2 {
            // DC or Planar
            [
                IntraPredMode::Planar,
                IntraPredMode::Dc,
                IntraPredMode::Angular26, // Vertical
            ]
        } else {
            // Angular mode
            let mode = cand_a.as_u8();
            let left = 2 + ((mode - 2).wrapping_sub(1) % 32);
            let right = 2 + ((mode - 2) + 1) % 32;
            [
                cand_a,
                IntraPredMode::from_u8(left).unwrap_or(IntraPredMode::Dc),
                IntraPredMode::from_u8(right).unwrap_or(IntraPredMode::Dc),
            ]
        }
    } else {
        // Different modes
        let third = if cand_a != IntraPredMode::Planar && cand_b != IntraPredMode::Planar {
            IntraPredMode::Planar
        } else if cand_a != IntraPredMode::Dc && cand_b != IntraPredMode::Dc {
            IntraPredMode::Dc
        } else {
            IntraPredMode::Angular26
        };
        [cand_a, cand_b, third]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mpm_candidates_same_dc() {
        let mpm = fill_mpm_candidates(IntraPredMode::Dc, IntraPredMode::Dc);
        assert_eq!(mpm[0], IntraPredMode::Planar);
        assert_eq!(mpm[1], IntraPredMode::Dc);
        assert_eq!(mpm[2], IntraPredMode::Angular26);
    }

    #[test]
    fn test_mpm_candidates_different() {
        let mpm = fill_mpm_candidates(IntraPredMode::Dc, IntraPredMode::Planar);
        assert_eq!(mpm[0], IntraPredMode::Dc);
        assert_eq!(mpm[1], IntraPredMode::Planar);
        assert_eq!(mpm[2], IntraPredMode::Angular26);
    }

    #[test]
    fn test_intra_angles() {
        // Mode 10 should be horizontal (angle 0)
        assert_eq!(INTRA_PRED_ANGLE[10], 0);
        // Mode 26 should be vertical (angle 0)
        assert_eq!(INTRA_PRED_ANGLE[26], 0);
        // Mode 2 should have positive angle
        assert_eq!(INTRA_PRED_ANGLE[2], 32);
        // Mode 34 should have positive angle
        assert_eq!(INTRA_PRED_ANGLE[34], 32);
    }
}
