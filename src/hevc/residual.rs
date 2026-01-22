//! Residual (transform coefficient) decoding
//!
//! This module handles parsing of transform coefficients via CABAC
//! and applying inverse transforms to residuals.

use super::cabac::{context, CabacDecoder, ContextModel};
use super::transform::MAX_COEFF;
use crate::error::HevcError;

type Result<T> = core::result::Result<T, HevcError>;

/// Scan order types for coefficient scanning
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanOrder {
    /// Diagonal scan (default)
    Diagonal = 0,
    /// Horizontal scan (for horizontal intra modes)
    Horizontal = 1,
    /// Vertical scan (for vertical intra modes)
    Vertical = 2,
}

/// Get scan order based on intra prediction mode
pub fn get_scan_order(log2_size: u8, intra_mode: u8) -> ScanOrder {
    // For 4x4 and 8x8 TUs, use scan order based on intra mode
    if log2_size == 2 || log2_size == 3 {
        if (6..=14).contains(&intra_mode) {
            ScanOrder::Vertical
        } else if (22..=30).contains(&intra_mode) {
            ScanOrder::Horizontal
        } else {
            ScanOrder::Diagonal
        }
    } else {
        ScanOrder::Diagonal
    }
}

/// 4x4 diagonal scan order (indexed by position 0-15)
pub static SCAN_ORDER_4X4_DIAG: [(u8, u8); 16] = [
    (0, 0),
    (1, 0),
    (0, 1),
    (2, 0),
    (1, 1),
    (0, 2),
    (3, 0),
    (2, 1),
    (1, 2),
    (0, 3),
    (3, 1),
    (2, 2),
    (1, 3),
    (3, 2),
    (2, 3),
    (3, 3),
];

/// 4x4 horizontal scan order
pub static SCAN_ORDER_4X4_HORIZ: [(u8, u8); 16] = [
    (0, 0),
    (1, 0),
    (2, 0),
    (3, 0),
    (0, 1),
    (1, 1),
    (2, 1),
    (3, 1),
    (0, 2),
    (1, 2),
    (2, 2),
    (3, 2),
    (0, 3),
    (1, 3),
    (2, 3),
    (3, 3),
];

/// 4x4 vertical scan order
pub static SCAN_ORDER_4X4_VERT: [(u8, u8); 16] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (0, 3),
    (1, 0),
    (1, 1),
    (1, 2),
    (1, 3),
    (2, 0),
    (2, 1),
    (2, 2),
    (2, 3),
    (3, 0),
    (3, 1),
    (3, 2),
    (3, 3),
];

/// Get scan order table for 4x4 sub-blocks
pub fn get_scan_4x4(order: ScanOrder) -> &'static [(u8, u8); 16] {
    match order {
        ScanOrder::Diagonal => &SCAN_ORDER_4X4_DIAG,
        ScanOrder::Horizontal => &SCAN_ORDER_4X4_HORIZ,
        ScanOrder::Vertical => &SCAN_ORDER_4X4_VERT,
    }
}

/// Coefficient buffer for a transform unit
#[derive(Clone)]
pub struct CoeffBuffer {
    /// Coefficients for this TU
    pub coeffs: [i16; MAX_COEFF],
    /// Transform size (log2)
    pub log2_size: u8,
    /// Number of non-zero coefficients
    pub num_nonzero: u16,
}

impl Default for CoeffBuffer {
    fn default() -> Self {
        Self {
            coeffs: [0; MAX_COEFF],
            log2_size: 2,
            num_nonzero: 0,
        }
    }
}

impl CoeffBuffer {
    /// Create a new coefficient buffer
    pub fn new(log2_size: u8) -> Self {
        Self {
            coeffs: [0; MAX_COEFF],
            log2_size,
            num_nonzero: 0,
        }
    }

    /// Get the transform size
    pub fn size(&self) -> usize {
        1 << self.log2_size
    }

    /// Get coefficient at position
    pub fn get(&self, x: usize, y: usize) -> i16 {
        let stride = self.size();
        self.coeffs[y * stride + x]
    }

    /// Set coefficient at position
    pub fn set(&mut self, x: usize, y: usize, value: i16) {
        let stride = self.size();
        self.coeffs[y * stride + x] = value;
        if value != 0 {
            self.num_nonzero = self.num_nonzero.saturating_add(1);
        }
    }

    /// Check if all coefficients are zero
    pub fn is_zero(&self) -> bool {
        self.num_nonzero == 0
    }
}

/// Decode residual coefficients for a transform unit
pub fn decode_residual(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    log2_size: u8,
    c_idx: u8, // 0=Y, 1=Cb, 2=Cr
    scan_order: ScanOrder,
    sign_data_hiding_enabled: bool,
    cu_transquant_bypass: bool,
) -> Result<CoeffBuffer> {
    let mut buffer = CoeffBuffer::new(log2_size);
    let size = 1u32 << log2_size;

    // Decode last significant coefficient position
    let (last_x, last_y) = decode_last_sig_coeff_pos(cabac, ctx, log2_size, c_idx)?;

    // DEBUG: Print first few last_sig positions
    static DEBUG_COUNT: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);
    let count = DEBUG_COUNT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    if count < 5 && c_idx == 0 {
        eprintln!("DEBUG: last_sig_coeff_pos: ({},{}) scan_order={:?} size={}", last_x, last_y, scan_order, size);
    }

    // Swap coordinates for vertical scan
    let (last_x, last_y) = if scan_order == ScanOrder::Vertical {
        (last_y, last_x)
    } else {
        (last_x, last_y)
    };

    if last_x >= size || last_y >= size {
        return Err(HevcError::InvalidBitstream("invalid last coeff position"));
    }

    // Get scan tables
    let scan_sub = get_scan_sub_block(log2_size, scan_order);
    let scan_pos = get_scan_4x4(scan_order);

    // Find last sub-block
    let sb_width = size / 4;
    let last_sb_x = last_x / 4;
    let last_sb_y = last_y / 4;
    let last_sb_idx = find_scan_pos(scan_sub, last_sb_x, last_sb_y, sb_width);

    // Find last position within sub-block
    let local_x = (last_x % 4) as u8;
    let local_y = (last_y % 4) as u8;
    let last_pos_in_sb = find_scan_pos_4x4(scan_pos, local_x, local_y);

    // Decode coefficients
    for sb_idx in (0..=last_sb_idx).rev() {
        let (sb_x, sb_y) = scan_sub[sb_idx as usize];

        // Check if sub-block is coded
        let sb_coded = if sb_idx > 0 && sb_idx < last_sb_idx {
            decode_coded_sub_block_flag(cabac, ctx, c_idx)?
        } else {
            true // First and last sub-blocks are always coded
        };

        if !sb_coded {
            continue;
        }

        // Decode coefficients in this sub-block
        let start_pos = if sb_idx == last_sb_idx {
            last_pos_in_sb
        } else {
            15
        };

        let mut coeff_values = [0i16; 16];
        let mut coeff_flags = [false; 16];
        let mut num_coeffs = 0u8;

        // Last coefficient in last sub-block
        if sb_idx == last_sb_idx {
            coeff_flags[start_pos as usize] = true;
            coeff_values[start_pos as usize] = 1;
            num_coeffs = 1;
        }

        // Decode significant_coeff_flags
        for n in (0..start_pos).rev() {
            let sig = decode_sig_coeff_flag(cabac, ctx, c_idx, n)?;
            if sig {
                coeff_flags[n as usize] = true;
                coeff_values[n as usize] = 1;
                num_coeffs += 1;
            }
        }

        if num_coeffs == 0 {
            continue;
        }

        // Reset c1 context for each sub-block
        let mut c1 = 1u8;

        // Decode greater-1 flags (up to 8)
        let mut first_g1_idx: Option<u8> = None;
        let mut g1_positions = [false; 16]; // Track which positions have g1=1
        let max_g1 = (num_coeffs as usize).min(8);
        let mut g1_count = 0;

        // Track which coefficients need remaining level decoding
        let mut needs_remaining = [false; 16];

        for n in (0..=start_pos).rev() {
            if !coeff_flags[n as usize] {
                continue;
            }

            if g1_count >= max_g1 {
                // Beyond first 8: base=1, always needs remaining for values > 1
                needs_remaining[n as usize] = true;
                continue;
            }

            let g1 = decode_coeff_greater1_flag(cabac, ctx, c_idx, c1)?;
            if g1 {
                coeff_values[n as usize] = 2;
                g1_positions[n as usize] = true;
                c1 = 0;
                if first_g1_idx.is_none() {
                    first_g1_idx = Some(n);
                } else {
                    // Non-first g1=1: base=2, needs remaining for values > 2
                    needs_remaining[n as usize] = true;
                }
            } else if c1 < 3 {
                c1 = c1.saturating_add(1);
            }
            g1_count += 1;
        }

        // Decode greater-2 flag (only for first coefficient with greater-1)
        if let Some(g1_idx) = first_g1_idx {
            let g2 = decode_coeff_greater2_flag(cabac, ctx, c_idx)?;
            if g2 {
                coeff_values[g1_idx as usize] = 3;
                needs_remaining[g1_idx as usize] = true;
            }
        }

        // Find first and last significant positions in this sub-block
        let mut first_sig_pos = None;
        let mut last_sig_pos = None;
        for n in 0..=start_pos {
            if coeff_flags[n as usize] {
                if first_sig_pos.is_none() {
                    first_sig_pos = Some(n);
                }
                last_sig_pos = Some(n);
            }
        }
        let first_sig_pos = first_sig_pos.unwrap_or(0);
        let last_sig_pos = last_sig_pos.unwrap_or(start_pos);

        // Determine if sign is hidden for this sub-block
        // Per H.265 9.3.4.3: sign is hidden if:
        // - sign_data_hiding_enabled_flag is true
        // - cu_transquant_bypass_flag is false
        // - lastScanPos - firstScanPos > 3
        // Note: also disabled for RDPCM modes (not implemented here)
        let sign_hidden = sign_data_hiding_enabled
            && !cu_transquant_bypass
            && (last_sig_pos - first_sig_pos) > 3;

        // Decode signs (bypass mode)
        // Following libde265's approach: decode signs in coefficient order (high scan pos to low)
        // The LAST coefficient (at first_sig_pos) has its sign hidden when sign_hidden=true
        //
        // Build list of significant positions in reverse scan order (high to low)
        let mut sig_positions = [0u8; 16];
        let mut n_sig = 0usize;
        for n in (0..=start_pos).rev() {
            if coeff_flags[n as usize] {
                sig_positions[n_sig] = n;
                n_sig += 1;
            }
        }

        // Decode signs for ALL coefficients (ignoring sign_hidden for now)
        // The sign_hidden feature causes CABAC desync - needs more investigation
        // TODO: Investigate why sign hiding causes early end_of_slice
        let _ = sign_hidden; // silence unused warning
        for i in 0..n_sig {
            let n = sig_positions[i] as usize;
            let sign = cabac.decode_bypass()?;
            if sign != 0 {
                coeff_values[n] = -coeff_values[n];
            }
        }

        // Decode remaining levels for all coefficients that need it
        // Rice parameter starts at 0 and is updated adaptively
        let mut rice_param = 0u8;
        for n in (0..=start_pos).rev() {
            if coeff_flags[n as usize] && needs_remaining[n as usize] {
                let base = coeff_values[n as usize];
                let (remaining, new_rice) =
                    decode_coeff_abs_level_remaining(cabac, rice_param, base)?;
                rice_param = new_rice;
                if base > 0 {
                    coeff_values[n as usize] = base + remaining;
                } else {
                    coeff_values[n as usize] = base - remaining;
                }
            }
        }

        // Infer hidden sign from parity
        // Per H.265: if sum of |levels| is odd, the hidden sign coeff is negative
        // NOTE: Sign hiding is disabled due to CABAC desync issues
        // This code would apply if sign_hidden were used
        let _ = first_sig_pos; // silence unused warning
        let _ = last_sig_pos;

        // Store coefficients in buffer
        for (n, &(px, py)) in scan_pos.iter().enumerate() {
            if coeff_flags[n] {
                let x = sb_x as usize * 4 + px as usize;
                let y = sb_y as usize * 4 + py as usize;
                buffer.set(x, y, coeff_values[n]);
            }
        }
    }

    Ok(buffer)
}

/// Get sub-block scan order
fn get_scan_sub_block(log2_size: u8, order: ScanOrder) -> &'static [(u8, u8)] {
    // For simplicity, always use diagonal scan for sub-blocks
    // A full implementation would have different tables for each size
    static SCAN_2X2_DIAG: [(u8, u8); 4] = [(0, 0), (1, 0), (0, 1), (1, 1)];
    static SCAN_1X1: [(u8, u8); 1] = [(0, 0)];

    let _ = order;
    match log2_size {
        2 => &SCAN_1X1,
        3 => &SCAN_2X2_DIAG,
        _ => &SCAN_2X2_DIAG, // Simplified
    }
}

/// Find position in scan order
fn find_scan_pos(scan: &[(u8, u8)], x: u32, y: u32, _width: u32) -> u32 {
    for (i, &(sx, sy)) in scan.iter().enumerate() {
        if sx as u32 == x && sy as u32 == y {
            return i as u32;
        }
    }
    0
}

/// Find position in 4x4 scan order
fn find_scan_pos_4x4(scan: &[(u8, u8); 16], x: u8, y: u8) -> u8 {
    for (i, &(sx, sy)) in scan.iter().enumerate() {
        if sx == x && sy == y {
            return i as u8;
        }
    }
    0
}

/// Decode last significant coefficient position
fn decode_last_sig_coeff_pos(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    log2_size: u8,
    c_idx: u8,
) -> Result<(u32, u32)> {
    // Decode prefix
    let x_prefix = decode_last_sig_coeff_prefix(cabac, ctx, log2_size, c_idx, true)?;
    let y_prefix = decode_last_sig_coeff_prefix(cabac, ctx, log2_size, c_idx, false)?;

    // Decode suffix if needed
    let x = if x_prefix > 3 {
        let n_bits = (x_prefix >> 1) - 1;
        let suffix = cabac.decode_bypass_bits(n_bits as u8)?;
        ((2 + (x_prefix & 1)) << n_bits) + suffix
    } else {
        x_prefix
    };

    let y = if y_prefix > 3 {
        let n_bits = (y_prefix >> 1) - 1;
        let suffix = cabac.decode_bypass_bits(n_bits as u8)?;
        ((2 + (y_prefix & 1)) << n_bits) + suffix
    } else {
        y_prefix
    };

    Ok((x, y))
}

/// Decode last_significant_coeff prefix
fn decode_last_sig_coeff_prefix(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    log2_size: u8,
    c_idx: u8,
    is_x: bool,
) -> Result<u32> {
    let ctx_offset = if is_x {
        context::LAST_SIG_COEFF_X_PREFIX
    } else {
        context::LAST_SIG_COEFF_Y_PREFIX
    };

    // Context offset based on component and size
    let ctx_shift = if c_idx == 0 {
        (log2_size + 1) >> 2
    } else {
        log2_size - 2
    };

    let ctx_base = ctx_offset + if c_idx > 0 { 15 } else { 0 };
    let max_prefix = (log2_size << 1) - 1;

    let mut prefix = 0u32;
    while prefix < max_prefix as u32 {
        let ctx_idx = ctx_base + ((prefix as usize) >> ctx_shift as usize).min(3);
        let bin = cabac.decode_bin(&mut ctx[ctx_idx])?;
        if bin == 0 {
            break;
        }
        prefix += 1;
    }

    Ok(prefix)
}

/// Decode coded_sub_block_flag
/// Note: Full HEVC uses neighbor-dependent context (csbfCtx = csbf_right + csbf_below)
/// This simplified version uses c_idx-based context only
fn decode_coded_sub_block_flag(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    c_idx: u8,
) -> Result<bool> {
    // Context offset: 0-1 for luma, 2-3 for chroma
    // Full spec uses: ctx_idx = base + min(csbf_right + csbf_below, 1) + c_idx_offset
    // Simplified: always use ctx 0 or 2
    let ctx_idx = context::CODED_SUB_BLOCK_FLAG + if c_idx > 0 { 2 } else { 0 };
    Ok(cabac.decode_bin(&mut ctx[ctx_idx])? != 0)
}

/// Decode significant_coeff_flag
fn decode_sig_coeff_flag(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    c_idx: u8,
    _pos: u8,
) -> Result<bool> {
    // Simplified: use single context
    let ctx_idx = context::SIG_COEFF_FLAG + if c_idx > 0 { 27 } else { 0 };
    Ok(cabac.decode_bin(&mut ctx[ctx_idx])? != 0)
}

/// Decode coeff_abs_level_greater1_flag
fn decode_coeff_greater1_flag(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    c_idx: u8,
    c1: u8,
) -> Result<bool> {
    let ctx_idx = context::COEFF_ABS_LEVEL_GREATER1_FLAG
        + if c_idx > 0 { 16 } else { 0 }
        + (c1 as usize).min(3);
    Ok(cabac.decode_bin(&mut ctx[ctx_idx])? != 0)
}

/// Decode coeff_abs_level_greater2_flag
fn decode_coeff_greater2_flag(
    cabac: &mut CabacDecoder<'_>,
    ctx: &mut [ContextModel],
    c_idx: u8,
) -> Result<bool> {
    let ctx_idx = context::COEFF_ABS_LEVEL_GREATER2_FLAG + if c_idx > 0 { 4 } else { 0 };
    Ok(cabac.decode_bin(&mut ctx[ctx_idx])? != 0)
}

/// Decode coeff_abs_level_remaining (Golomb-Rice with adaptive rice parameter)
/// Returns (value, updated_rice_param)
fn decode_coeff_abs_level_remaining(
    cabac: &mut CabacDecoder<'_>,
    rice_param: u8,
    base_level: i16,
) -> Result<(i16, u8)> {
    // Decode prefix (unary part)
    let mut prefix = 0u32;
    while cabac.decode_bypass()? != 0 && prefix < 32 {
        prefix += 1;
    }

    let value = if prefix <= 3 {
        // TR part only: value = (prefix << rice_param) + suffix
        let suffix = if rice_param > 0 {
            cabac.decode_bypass_bits(rice_param)?
        } else {
            0
        };
        ((prefix << rice_param) + suffix) as i16
    } else {
        // EGk part: suffix bits = prefix - 3 + rice_param
        let suffix_bits = (prefix - 3 + rice_param as u32) as u8;
        let suffix = cabac.decode_bypass_bits(suffix_bits)?;
        // value = (((1 << (prefix-3)) + 3 - 1) << rice_param) + suffix
        let base = ((1u32 << (prefix - 3)) + 2) << rice_param;
        (base + suffix) as i16
    };

    // Update rice parameter: if baseLevel + value > 3 * (1 << rice_param), increase
    let threshold = 3 * (1 << rice_param);
    let new_rice_param = if (base_level.unsigned_abs() as u32 + value as u32) > threshold {
        (rice_param + 1).min(4)
    } else {
        rice_param
    };

    Ok((value, new_rice_param))
}
