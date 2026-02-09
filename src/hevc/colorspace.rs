//! Color space handling and conversion
//!
//! Implements proper color space conversions including:
//! - YCbCr to RGB conversion with various matrix coefficients
//! - HDR transfer functions (PQ/HDR10, HLG)
//! - Tone mapping from HDR to SDR
//! - Color primaries and gamut conversion
//!
//! References:
//! - ITU-T H.265 Annex E (VUI parameters)
//! - ITU-R BT.709 (HDTV standard)
//! - ITU-R BT.2020 (UHDTV standard)
//! - SMPTE ST 2084 (PQ transfer function for HDR10)
//! - ITU-R BT.2100 (HLG transfer function)

use alloc::vec::Vec;

/// Color primaries (ITU-T H.265 Table E.3)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ColorPrimaries {
    /// Reserved
    Reserved = 0,
    /// BT.709 / sRGB / BT.601 (HDTV standard)
    Bt709 = 1,
    /// Unspecified
    Unspecified = 2,
    /// Reserved
    Reserved3 = 3,
    /// BT.470 System M (FCC)
    Bt470M = 4,
    /// BT.470 System B, G (PAL)
    Bt470Bg = 5,
    /// BT.601 / SMPTE 170M (NTSC)
    Bt601 = 6,
    /// SMPTE 240M
    Smpte240M = 7,
    /// Generic film
    Film = 8,
    /// BT.2020 / BT.2100 (UHDTV/HDR standard)
    Bt2020 = 9,
    /// SMPTE ST 428 (CIE 1931 XYZ)
    Smpte428 = 10,
    /// DCI-P3 (Digital Cinema)
    DciP3 = 11,
    /// Display P3 (Apple)
    DisplayP3 = 12,
}

impl ColorPrimaries {
    /// Create from raw u8 value
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Bt709,
            2 => Self::Unspecified,
            4 => Self::Bt470M,
            5 => Self::Bt470Bg,
            6 => Self::Bt601,
            7 => Self::Smpte240M,
            8 => Self::Film,
            9 => Self::Bt2020,
            10 => Self::Smpte428,
            11 => Self::DciP3,
            12 => Self::DisplayP3,
            _ => Self::Reserved,
        }
    }
}

impl Default for ColorPrimaries {
    fn default() -> Self {
        Self::Bt709
    }
}

/// Transfer characteristics (ITU-T H.265 Table E.4)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransferCharacteristics {
    /// Reserved
    Reserved = 0,
    /// BT.709 (gamma 2.4 with linear segment)
    Bt709 = 1,
    /// Unspecified
    Unspecified = 2,
    /// Reserved
    Reserved3 = 3,
    /// Gamma 2.2 (BT.470 System M)
    Gamma22 = 4,
    /// Gamma 2.8 (BT.470 System B, G)
    Gamma28 = 5,
    /// BT.601 / SMPTE 170M
    Bt601 = 6,
    /// SMPTE 240M
    Smpte240M = 7,
    /// Linear
    Linear = 8,
    /// Logarithmic (100:1 range)
    Log100 = 9,
    /// Logarithmic (100*Sqrt(10):1 range)
    Log100Sqrt10 = 10,
    /// IEC 61966-2-4
    Iec61966_2_4 = 11,
    /// BT.1361 extended color gamut
    Bt1361 = 12,
    /// sRGB / IEC 61966-2-1 (standard sRGB gamma)
    Srgb = 13,
    /// BT.2020 10-bit
    Bt2020_10bit = 14,
    /// BT.2020 12-bit
    Bt2020_12bit = 15,
    /// SMPTE ST 2084 (PQ) - HDR10
    Pq = 16,
    /// SMPTE ST 428
    Smpte428 = 17,
    /// ARIB STD-B67 (HLG) - Hybrid Log Gamma
    Hlg = 18,
}

impl TransferCharacteristics {
    /// Create from raw u8 value
    pub fn from_u8(val: u8) -> Self {
        match val {
            1 => Self::Bt709,
            2 => Self::Unspecified,
            4 => Self::Gamma22,
            5 => Self::Gamma28,
            6 => Self::Bt601,
            7 => Self::Smpte240M,
            8 => Self::Linear,
            9 => Self::Log100,
            10 => Self::Log100Sqrt10,
            11 => Self::Iec61966_2_4,
            12 => Self::Bt1361,
            13 => Self::Srgb,
            14 => Self::Bt2020_10bit,
            15 => Self::Bt2020_12bit,
            16 => Self::Pq,
            17 => Self::Smpte428,
            18 => Self::Hlg,
            _ => Self::Reserved,
        }
    }

    /// Check if this is an HDR transfer function
    pub fn is_hdr(&self) -> bool {
        matches!(self, Self::Pq | Self::Hlg)
    }
}

impl Default for TransferCharacteristics {
    fn default() -> Self {
        Self::Bt709
    }
}

/// Matrix coefficients for YCbCr to RGB conversion (ITU-T H.265 Table E.5)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MatrixCoefficients {
    /// RGB (no matrix, identity)
    Rgb = 0,
    /// BT.709 / BT.1361
    Bt709 = 1,
    /// Unspecified
    Unspecified = 2,
    /// Reserved
    Reserved = 3,
    /// FCC
    Fcc = 4,
    /// BT.470 System B, G
    Bt470Bg = 5,
    /// BT.601 / SMPTE 170M
    Bt601 = 6,
    /// SMPTE 240M
    Smpte240M = 7,
    /// YCgCo
    YCgCo = 8,
    /// BT.2020 non-constant luminance
    Bt2020Ncl = 9,
    /// BT.2020 constant luminance
    Bt2020Cl = 10,
    /// SMPTE ST 2085
    Smpte2085 = 11,
    /// Chromaticity-derived non-constant luminance
    ChromaDerivedNcl = 12,
    /// Chromaticity-derived constant luminance
    ChromaDerivedCl = 13,
    /// ICtCp (BT.2100)
    ICtCp = 14,
}

impl MatrixCoefficients {
    /// Create from raw u8 value
    pub fn from_u8(val: u8) -> Self {
        match val {
            0 => Self::Rgb,
            1 => Self::Bt709,
            2 => Self::Unspecified,
            3 => Self::Reserved,
            4 => Self::Fcc,
            5 => Self::Bt470Bg,
            6 => Self::Bt601,
            7 => Self::Smpte240M,
            8 => Self::YCgCo,
            9 => Self::Bt2020Ncl,
            10 => Self::Bt2020Cl,
            11 => Self::Smpte2085,
            12 => Self::ChromaDerivedNcl,
            13 => Self::ChromaDerivedCl,
            14 => Self::ICtCp,
            _ => Self::Unspecified,
        }
    }
}

impl Default for MatrixCoefficients {
    fn default() -> Self {
        Self::Bt709
    }
}

/// Color space metadata
#[derive(Debug, Clone, Copy)]
pub struct ColorSpace {
    /// Color primaries
    pub primaries: ColorPrimaries,
    /// Transfer characteristics (gamma/EOTF)
    pub transfer: TransferCharacteristics,
    /// Matrix coefficients for YCbCr→RGB
    pub matrix: MatrixCoefficients,
    /// Full range flag (true = 0-255, false = 16-235 for 8-bit)
    pub full_range: bool,
}

impl Default for ColorSpace {
    fn default() -> Self {
        Self {
            primaries: ColorPrimaries::Bt709,
            transfer: TransferCharacteristics::Bt709,
            matrix: MatrixCoefficients::Bt709,
            full_range: false,
        }
    }
}

impl ColorSpace {
    /// Create from VUI parameters
    pub fn from_vui(
        primaries: u8,
        transfer: u8,
        matrix: u8,
        full_range: bool,
    ) -> Self {
        Self {
            primaries: ColorPrimaries::from_u8(primaries),
            transfer: TransferCharacteristics::from_u8(transfer),
            matrix: MatrixCoefficients::from_u8(matrix),
            full_range,
        }
    }

    /// Get YCbCr to RGB conversion matrix coefficients
    /// Returns (Kr, Kb) for the matrix derivation
    fn get_matrix_coefficients(&self) -> (f32, f32) {
        match self.matrix {
            MatrixCoefficients::Bt709 => (0.2126, 0.0722),
            MatrixCoefficients::Bt601 => (0.299, 0.114),
            MatrixCoefficients::Bt2020Ncl => (0.2627, 0.0593),
            MatrixCoefficients::Bt470Bg => (0.299, 0.114),
            MatrixCoefficients::Smpte240M => (0.212, 0.087),
            _ => (0.2126, 0.0722), // Default to BT.709
        }
    }

    /// Convert YCbCr to RGB using appropriate matrix
    ///
    /// Input: Y, Cb, Cr in range [0, 2^bit_depth - 1]
    /// Output: R, G, B in range [0.0, 1.0] (linear light)
    pub fn ycbcr_to_rgb(&self, y: u16, cb: u16, cr: u16, bit_depth: u8) -> (f32, f32, f32) {
        let max_val = (1 << bit_depth) - 1;

        // Normalize to [0.0, 1.0]
        let (y_norm, cb_norm, cr_norm) = if self.full_range {
            // Full range: 0-255 (8-bit) or 0-1023 (10-bit)
            (
                y as f32 / max_val as f32,
                cb as f32 / max_val as f32,
                cr as f32 / max_val as f32,
            )
        } else {
            // Limited range: 16-235 (8-bit) or 64-940 (10-bit) for luma
            //                16-240 (8-bit) or 64-960 (10-bit) for chroma
            let scale = (1 << (bit_depth - 8)) as f32;
            let y_min = 16.0 * scale;
            let y_max = 235.0 * scale;
            let c_min = 16.0 * scale;
            let c_max = 240.0 * scale;

            (
                ((y as f32 - y_min) / (y_max - y_min)).clamp(0.0, 1.0),
                ((cb as f32 - c_min) / (c_max - c_min)).clamp(0.0, 1.0),
                ((cr as f32 - c_min) / (c_max - c_min)).clamp(0.0, 1.0),
            )
        };

        // Center chroma values
        let pb = cb_norm - 0.5;
        let pr = cr_norm - 0.5;

        // Get matrix coefficients
        let (kr, kb) = self.get_matrix_coefficients();
        let kg = 1.0 - kr - kb;

        // YCbCr to RGB matrix derivation (ITU-R BT.709/2020)
        let r = y_norm + 2.0 * (1.0 - kr) * pr;
        let g = y_norm - 2.0 * kb * (1.0 - kb) / kg * pb - 2.0 * kr * (1.0 - kr) / kg * pr;
        let b = y_norm + 2.0 * (1.0 - kb) * pb;

        (r, g, b)
    }

    /// Apply transfer function (OETF inverse / EOTF) to convert to linear light
    ///
    /// Input: Signal value [0.0, 1.0]
    /// Output: Linear light value [0.0, 1.0+] (may exceed 1.0 for HDR)
    pub fn apply_eotf(&self, signal: f32) -> f32 {
        match self.transfer {
            TransferCharacteristics::Linear => signal,

            TransferCharacteristics::Bt709 | TransferCharacteristics::Bt601 => {
                // BT.709 OETF inverse (similar to sRGB but different constants)
                if signal < 0.081 {
                    signal / 4.5
                } else {
                    ((signal + 0.099) / 1.099).powf(1.0 / 0.45)
                }
            }

            TransferCharacteristics::Srgb => {
                // sRGB EOTF
                if signal <= 0.04045 {
                    signal / 12.92
                } else {
                    ((signal + 0.055) / 1.055).powf(2.4)
                }
            }

            TransferCharacteristics::Pq => {
                // SMPTE ST 2084 (PQ) EOTF - HDR10
                // Input: normalized PQ signal [0.0, 1.0]
                // Output: linear light [0.0, 10000.0] nits, normalized to [0.0, 1.0] @ 10000 nits
                Self::pq_eotf(signal)
            }

            TransferCharacteristics::Hlg => {
                // HLG OETF inverse (ITU-R BT.2100)
                Self::hlg_oetf_inverse(signal)
            }

            TransferCharacteristics::Gamma22 => signal.powf(2.2),
            TransferCharacteristics::Gamma28 => signal.powf(2.8),

            _ => {
                // Default to BT.709 for unknown transfer functions
                if signal < 0.081 {
                    signal / 4.5
                } else {
                    ((signal + 0.099) / 1.099).powf(1.0 / 0.45)
                }
            }
        }
    }

    /// PQ (Perceptual Quantizer) EOTF - SMPTE ST 2084
    ///
    /// Converts PQ signal [0.0, 1.0] to linear light
    /// Output is in units of 10,000 nits (1.0 = 10,000 nits)
    fn pq_eotf(signal: f32) -> f32 {
        let signal = signal.max(0.0);

        // PQ constants
        let m1 = 2610.0 / 16384.0;  // 0.1593017578125
        let m2 = 2523.0 / 4096.0 * 128.0;  // 78.84375
        let c1 = 3424.0 / 4096.0;  // 0.8359375
        let c2 = 2413.0 / 4096.0 * 32.0;  // 18.8515625
        let c3 = 2392.0 / 4096.0 * 32.0;  // 18.6875

        let v_pow = signal.powf(1.0 / m2);
        let numerator = (v_pow - c1).max(0.0);
        let denominator = c2 - c3 * v_pow;

        if denominator <= 0.0 {
            return 0.0;
        }

        (numerator / denominator).powf(1.0 / m1)
    }

    /// HLG OETF inverse (ITU-R BT.2100)
    ///
    /// Converts HLG signal [0.0, 1.0] to scene linear light
    fn hlg_oetf_inverse(signal: f32) -> f32 {
        let signal = signal.clamp(0.0, 1.0);

        const A: f32 = 0.17883277;
        const B: f32 = 0.28466892;  // 1 - 4*a
        const C: f32 = 0.55991073;  // 0.5 - a * ln(4*a)

        if signal <= 0.5 {
            (signal * signal) / 3.0
        } else {
            ((signal - C) / A).exp() + B
        }
    }

    /// Apply tone mapping from HDR to SDR
    ///
    /// Input: Linear light value [0.0, 10000.0] nits (for PQ) or scene-referred (for HLG)
    /// Output: Linear light value [0.0, 1.0] suitable for SDR display (~100 nits)
    pub fn tone_map_to_sdr(&self, linear: f32) -> f32 {
        match self.transfer {
            TransferCharacteristics::Pq => {
                // PQ tone mapping: 10000 nits → 100 nits
                // Using Reinhard-like tone mapper
                Self::reinhard_tone_map(linear, 10000.0, 100.0)
            }

            TransferCharacteristics::Hlg => {
                // HLG is scene-referred, apply OOTF for 1000 nit display then map to SDR
                let display_linear = Self::hlg_ootf(linear, 1000.0);
                Self::reinhard_tone_map(display_linear, 1000.0, 100.0)
            }

            _ => {
                // No tone mapping needed for SDR content
                linear.clamp(0.0, 1.0)
            }
        }
    }

    /// Reinhard tone mapping
    fn reinhard_tone_map(linear: f32, peak_nits: f32, target_nits: f32) -> f32 {
        if linear <= 0.0 {
            return 0.0;
        }

        // Simple Reinhard: L_out = L / (1 + L)
        // For better results, use extended Reinhard with white point
        let white_point = peak_nits / target_nits;
        let white_point_sq = white_point * white_point;

        let numerator = linear * (1.0 + linear / white_point_sq);
        let denominator = 1.0 + linear;

        (numerator / denominator).clamp(0.0, 1.0)
    }

    /// HLG OOTF (Opto-Optical Transfer Function)
    /// Converts scene linear to display linear for a given peak luminance
    fn hlg_ootf(scene_linear: f32, peak_nits: f32) -> f32 {
        // OOTF gamma for HLG (ITU-R BT.2100)
        let gamma = 1.2 + 0.42 * (peak_nits / 1000.0).log10();

        // Y_d = (Y_s)^gamma
        scene_linear.powf(gamma)
    }

    /// Apply SDR OETF (Electro-Optical Transfer Function inverse)
    /// Converts linear light [0.0, 1.0] to signal value [0.0, 1.0]
    pub fn apply_sdr_oetf(&self, linear: f32) -> f32 {
        let linear = linear.clamp(0.0, 1.0);

        // Use sRGB OETF for output (standard for computer displays)
        if linear <= 0.0031308 {
            12.92 * linear
        } else {
            1.055 * linear.powf(1.0 / 2.4) - 0.055
        }
    }

    /// Full pipeline: YCbCr → RGB (linear) → tone map → sRGB signal → 8-bit
    pub fn ycbcr_to_rgb8(&self, y: u16, cb: u16, cr: u16, bit_depth: u8) -> (u8, u8, u8) {
        // Convert YCbCr to RGB in signal domain
        let (r_signal, g_signal, b_signal) = self.ycbcr_to_rgb(y, cb, cr, bit_depth);

        // Apply EOTF to get linear light
        let r_linear = self.apply_eotf(r_signal);
        let g_linear = self.apply_eotf(g_signal);
        let b_linear = self.apply_eotf(b_signal);

        // Tone map HDR to SDR if needed
        let r_sdr = self.tone_map_to_sdr(r_linear);
        let g_sdr = self.tone_map_to_sdr(g_linear);
        let b_sdr = self.tone_map_to_sdr(b_linear);

        // Apply sRGB OETF
        let r_out = self.apply_sdr_oetf(r_sdr);
        let g_out = self.apply_sdr_oetf(g_sdr);
        let b_out = self.apply_sdr_oetf(b_sdr);

        // Convert to 8-bit
        (
            (r_out * 255.0).round().clamp(0.0, 255.0) as u8,
            (g_out * 255.0).round().clamp(0.0, 255.0) as u8,
            (b_out * 255.0).round().clamp(0.0, 255.0) as u8,
        )
    }

    /// Full pipeline outputting 16-bit RGB (for high bit depth preservation)
    pub fn ycbcr_to_rgb16(&self, y: u16, cb: u16, cr: u16, bit_depth: u8) -> (u16, u16, u16) {
        // Convert YCbCr to RGB in signal domain
        let (r_signal, g_signal, b_signal) = self.ycbcr_to_rgb(y, cb, cr, bit_depth);

        // Apply EOTF to get linear light
        let r_linear = self.apply_eotf(r_signal);
        let g_linear = self.apply_eotf(g_signal);
        let b_linear = self.apply_eotf(b_signal);

        // For HDR content, we preserve the linear values scaled to 16-bit
        // For SDR content, apply sRGB OETF
        let (r_out, g_out, b_out) = if self.transfer.is_hdr() {
            // HDR: store linear light normalized to peak (PQ: 10000 nits, HLG: scene-referred)
            // Scale to use full 16-bit range
            let scale = match self.transfer {
                TransferCharacteristics::Pq => 10000.0,  // Full PQ range
                TransferCharacteristics::Hlg => 12.0,     // Typical scene range for HLG
                _ => 1.0,
            };
            (
                (r_linear / scale).clamp(0.0, 1.0),
                (g_linear / scale).clamp(0.0, 1.0),
                (b_linear / scale).clamp(0.0, 1.0),
            )
        } else {
            // SDR: apply sRGB OETF
            (
                self.apply_sdr_oetf(r_linear.clamp(0.0, 1.0)),
                self.apply_sdr_oetf(g_linear.clamp(0.0, 1.0)),
                self.apply_sdr_oetf(b_linear.clamp(0.0, 1.0)),
            )
        };

        // Convert to 16-bit
        (
            (r_out * 65535.0).round().clamp(0.0, 65535.0) as u16,
            (g_out * 65535.0).round().clamp(0.0, 65535.0) as u16,
            (b_out * 65535.0).round().clamp(0.0, 65535.0) as u16,
        )
    }
}

/// Batch convert YCbCr frame to RGB8
pub fn convert_frame_to_rgb8(
    y_plane: &[u16],
    cb_plane: &[u16],
    cr_plane: &[u16],
    width: u32,
    height: u32,
    chroma_format: u8,
    bit_depth: u8,
    colorspace: &ColorSpace,
) -> Vec<u8> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);

    for y in 0..height {
        for x in 0..width {
            let y_idx = (y * width + x) as usize;
            let y_val = y_plane[y_idx];

            // Get chroma samples (handle subsampling)
            let (cb_val, cr_val) = match chroma_format {
                1 => {
                    // 4:2:0 - chroma is half resolution in both dimensions
                    let cx = x / 2;
                    let cy = y / 2;
                    let c_width = width / 2;
                    let c_idx = (cy * c_width + cx) as usize;
                    (cb_plane[c_idx], cr_plane[c_idx])
                }
                2 => {
                    // 4:2:2 - chroma is half resolution horizontally
                    let cx = x / 2;
                    let c_width = width / 2;
                    let c_idx = (y * c_width + cx) as usize;
                    (cb_plane[c_idx], cr_plane[c_idx])
                }
                3 => {
                    // 4:4:4 - chroma is full resolution
                    (cb_plane[y_idx], cr_plane[y_idx])
                }
                _ => (cb_plane[y_idx], cr_plane[y_idx]),
            };

            let (r, g, b) = colorspace.ycbcr_to_rgb8(y_val, cb_val, cr_val, bit_depth);
            rgb.push(r);
            rgb.push(g);
            rgb.push(b);
        }
    }

    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bt709_conversion() {
        let cs = ColorSpace::default();

        // Test pure white (Y=235, Cb=128, Cr=128 for 8-bit limited range)
        let (r, g, b) = cs.ycbcr_to_rgb8(235, 128, 128, 8);
        assert_eq!(r, 255);
        assert_eq!(g, 255);
        assert_eq!(b, 255);

        // Test pure black (Y=16, Cb=128, Cr=128 for 8-bit limited range)
        let (r, g, b) = cs.ycbcr_to_rgb8(16, 128, 128, 8);
        assert_eq!(r, 0);
        assert_eq!(g, 0);
        assert_eq!(b, 0);
    }

    #[test]
    fn test_pq_eotf() {
        // Test that PQ EOTF is monotonic
        let cs = ColorSpace {
            transfer: TransferCharacteristics::Pq,
            ..Default::default()
        };

        let mut prev = 0.0;
        for i in 0..=100 {
            let signal = i as f32 / 100.0;
            let linear = cs.apply_eotf(signal);
            assert!(linear >= prev, "PQ EOTF should be monotonic");
            prev = linear;
        }
    }

    #[test]
    fn test_hlg_oetf_inverse() {
        let cs = ColorSpace {
            transfer: TransferCharacteristics::Hlg,
            ..Default::default()
        };

        // Test that HLG OETF inverse is monotonic
        let mut prev = 0.0;
        for i in 0..=100 {
            let signal = i as f32 / 100.0;
            let linear = cs.apply_eotf(signal);
            assert!(linear >= prev, "HLG OETF inverse should be monotonic");
            prev = linear;
        }
    }
}
