//! Auxiliary image types and depth map extraction.
//!
//! HEIF files can contain auxiliary images linked to the primary image via
//! `auxl` item references. Each auxiliary image has an `auxC` property box
//! containing a URN string that identifies its type.
//!
//! This module provides:
//! - [`AuxiliaryImageType`]: enum for known auxiliary image types
//! - [`DepthRepresentationInfo`]: parsed depth representation metadata
//! - Depth map extraction via [`DecoderConfig::decode_depth`](crate::DecoderConfig::decode_depth)

use alloc::string::String;
use alloc::vec::Vec;

/// Identifies the type of an auxiliary image in a HEIF container.
///
/// Auxiliary images are linked to the primary image via `auxl` references
/// and identified by a URN string in their `auxC` property box.
///
/// # Known URNs
///
/// | Type | URN |
/// |------|-----|
/// | Alpha | `urn:mpeg:hevc:2015:auxid:1` or `urn:mpeg:mpegB:cicp:systems:auxiliary:alpha` |
/// | Depth | `urn:mpeg:hevc:2015:auxid:2` or `urn:mpeg:mpegB:cicp:systems:auxiliary:depth` |
/// | Portrait matte | `urn:com:apple:photo:2018:aux:portraiteffectsmatte` |
/// | HDR gain map | `urn:com:apple:photo:2020:aux:hdrgainmap` |
/// | Skin matte | `urn:com:apple:photo:2019:aux:semanticskinmatte` |
/// | Teeth matte | `urn:com:apple:photo:2019:aux:semanticteethmatte` |
/// | Hair matte | `urn:com:apple:photo:2019:aux:semantichairmatte` |
/// | Glasses matte | `urn:com:apple:photo:2020:aux:semanticglassesmatte` |
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum AuxiliaryImageType {
    /// Alpha plane (`urn:mpeg:hevc:2015:auxid:1`)
    Alpha,
    /// Depth map (`urn:mpeg:hevc:2015:auxid:2`)
    Depth,
    /// Portrait effects matte (`urn:com:apple:photo:2018:aux:portraiteffectsmatte`)
    PortraitMatte,
    /// HDR gain map (`urn:com:apple:photo:2020:aux:hdrgainmap`)
    HdrGainMap,
    /// Semantic skin matte (`urn:com:apple:photo:2019:aux:semanticskinmatte`)
    SkinMatte,
    /// Semantic teeth matte (`urn:com:apple:photo:2019:aux:semanticteethmatte`)
    TeethMatte,
    /// Semantic hair matte (`urn:com:apple:photo:2019:aux:semantichairmatte`)
    HairMatte,
    /// Semantic glasses matte (`urn:com:apple:photo:2020:aux:semanticglassesmatte`)
    GlassesMatte,
    /// Unknown auxiliary type with its raw URN string
    Other(String),
}

impl AuxiliaryImageType {
    /// Parse an auxiliary type from a URN string (from `auxC` box).
    #[must_use]
    pub fn from_urn(urn: &str) -> Self {
        match urn {
            "urn:mpeg:hevc:2015:auxid:1" | "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha" => {
                Self::Alpha
            }
            "urn:mpeg:hevc:2015:auxid:2" | "urn:mpeg:mpegB:cicp:systems:auxiliary:depth" => {
                Self::Depth
            }
            "urn:com:apple:photo:2018:aux:portraiteffectsmatte" => Self::PortraitMatte,
            "urn:com:apple:photo:2020:aux:hdrgainmap" => Self::HdrGainMap,
            "urn:com:apple:photo:2019:aux:semanticskinmatte" => Self::SkinMatte,
            "urn:com:apple:photo:2019:aux:semanticteethmatte" => Self::TeethMatte,
            "urn:com:apple:photo:2019:aux:semantichairmatte" => Self::HairMatte,
            "urn:com:apple:photo:2020:aux:semanticglassesmatte" => Self::GlassesMatte,
            other => Self::Other(String::from(other)),
        }
    }

    /// The canonical URN string for this auxiliary type.
    ///
    /// For [`Other`](Self::Other), returns the stored string.
    #[must_use]
    pub fn urn(&self) -> &str {
        match self {
            Self::Alpha => "urn:mpeg:hevc:2015:auxid:1",
            Self::Depth => "urn:mpeg:hevc:2015:auxid:2",
            Self::PortraitMatte => "urn:com:apple:photo:2018:aux:portraiteffectsmatte",
            Self::HdrGainMap => "urn:com:apple:photo:2020:aux:hdrgainmap",
            Self::SkinMatte => "urn:com:apple:photo:2019:aux:semanticskinmatte",
            Self::TeethMatte => "urn:com:apple:photo:2019:aux:semanticteethmatte",
            Self::HairMatte => "urn:com:apple:photo:2019:aux:semantichairmatte",
            Self::GlassesMatte => "urn:com:apple:photo:2020:aux:semanticglassesmatte",
            Self::Other(s) => s.as_str(),
        }
    }
}

impl core::fmt::Display for AuxiliaryImageType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Alpha => write!(f, "Alpha"),
            Self::Depth => write!(f, "Depth"),
            Self::PortraitMatte => write!(f, "PortraitMatte"),
            Self::HdrGainMap => write!(f, "HdrGainMap"),
            Self::SkinMatte => write!(f, "SkinMatte"),
            Self::TeethMatte => write!(f, "TeethMatte"),
            Self::HairMatte => write!(f, "HairMatte"),
            Self::GlassesMatte => write!(f, "GlassesMatte"),
            Self::Other(s) => write!(f, "Other({s})"),
        }
    }
}

/// How the depth values in a depth auxiliary image should be interpreted.
///
/// Defined by ISO/IEC 23008-12 depth representation SEI and auxC subtype data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum DepthRepresentationType {
    /// Uniform inverse Z (type 0) -- default for most iPhone depth maps
    #[default]
    UniformInverseZ,
    /// Uniform disparity (type 1)
    UniformDisparity,
    /// Uniform Z (type 2)
    UniformZ,
    /// Nonuniform disparity (type 3)
    NonuniformDisparity,
}

impl DepthRepresentationType {
    /// Parse from the integer code in the depth representation info.
    #[must_use]
    pub fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::UniformInverseZ),
            1 => Some(Self::UniformDisparity),
            2 => Some(Self::UniformZ),
            3 => Some(Self::NonuniformDisparity),
            _ => None,
        }
    }

    /// Integer code for this representation type.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            Self::UniformInverseZ => 0,
            Self::UniformDisparity => 1,
            Self::UniformZ => 2,
            Self::NonuniformDisparity => 3,
        }
    }
}

impl core::fmt::Display for DepthRepresentationType {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UniformInverseZ => write!(f, "UniformInverseZ"),
            Self::UniformDisparity => write!(f, "UniformDisparity"),
            Self::UniformZ => write!(f, "UniformZ"),
            Self::NonuniformDisparity => write!(f, "NonuniformDisparity"),
        }
    }
}

/// Depth representation information parsed from auxC subtype data.
///
/// The auxC box for depth auxiliary images may contain additional bytes
/// after the null-terminated URN that describe the depth representation.
/// This follows the ISO/IEC 23008-12 depth representation info structure.
///
/// iPhone depth maps typically use `UniformInverseZ` with
/// z_near/z_far describing the scene depth range.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DepthRepresentationInfo {
    /// Near clipping plane distance. `None` if not specified.
    pub z_near: Option<f64>,
    /// Far clipping plane distance. `None` if not specified.
    pub z_far: Option<f64>,
    /// Minimum disparity value. `None` if not specified.
    pub d_min: Option<f64>,
    /// Maximum disparity value. `None` if not specified.
    pub d_max: Option<f64>,
    /// How the depth values should be interpreted.
    pub representation_type: DepthRepresentationType,
}

/// Decoded depth map from a HEIF auxiliary depth image.
///
/// The pixel data is grayscale (single channel), representing depth or
/// disparity values. Interpretation depends on
/// [`DepthRepresentationInfo::representation_type`].
///
/// Bit depths of 8 and 10 are common. The `bit_depth` field indicates
/// how many bits are significant in each `u16` sample.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DepthMap {
    /// Grayscale depth pixels as u16 samples.
    ///
    /// For 8-bit depth maps, values are in `[0, 255]`.
    /// For 10-bit depth maps, values are in `[0, 1023]`.
    pub data: Vec<u16>,
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
    pub height: u32,
    /// Significant bits per sample (typically 8 or 10)
    pub bit_depth: u8,
    /// Depth representation metadata
    pub depth_info: DepthRepresentationInfo,
}

/// Descriptor for an auxiliary image found in the container.
///
/// Returned by [`DecoderConfig::auxiliary_images`](crate::DecoderConfig::auxiliary_images).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AuxiliaryImageDescriptor {
    /// The type of this auxiliary image
    pub aux_type: AuxiliaryImageType,
    /// Item ID in the HEIF container (for advanced use)
    pub item_id: u32,
    /// Image dimensions (width, height) if known from `ispe` box.
    /// May be `None` if the item lacks spatial extent metadata.
    pub dimensions: Option<(u32, u32)>,
}

/// Parse depth representation info from the auxC subtype bytes
/// (the bytes after the null-terminated URN in the auxC box content).
///
/// Format (ISO/IEC 23008-12, A.3):
/// ```text
/// depth_representation_type: u8
/// flags: u8
///   bit 0: z_near_flag
///   bit 1: z_far_flag
///   bit 2: d_min_flag
///   bit 3: d_max_flag
/// If z_near_flag: z_near as DepthRepValueSigned (4 bytes: i16 mantissa + u8 exponent + u8 sign)
/// If z_far_flag:  z_far  as DepthRepValueSigned
/// If d_min_flag:  d_min  as DepthRepValueSigned
/// If d_max_flag:  d_max  as DepthRepValueSigned
/// ```
pub(crate) fn parse_depth_representation_info(subtype_data: &[u8]) -> DepthRepresentationInfo {
    if subtype_data.len() < 2 {
        return DepthRepresentationInfo::default();
    }

    let rep_type_code = subtype_data[0];
    let representation_type = DepthRepresentationType::from_code(rep_type_code)
        .unwrap_or(DepthRepresentationType::UniformInverseZ);

    let flags = subtype_data[1];
    let z_near_flag = (flags & 0x01) != 0;
    let z_far_flag = (flags & 0x02) != 0;
    let d_min_flag = (flags & 0x04) != 0;
    let d_max_flag = (flags & 0x08) != 0;

    let mut pos = 2;

    let z_near = if z_near_flag {
        parse_depth_rep_value(subtype_data, &mut pos)
    } else {
        None
    };

    let z_far = if z_far_flag {
        parse_depth_rep_value(subtype_data, &mut pos)
    } else {
        None
    };

    let d_min = if d_min_flag {
        parse_depth_rep_value(subtype_data, &mut pos)
    } else {
        None
    };

    let d_max = if d_max_flag {
        parse_depth_rep_value(subtype_data, &mut pos)
    } else {
        None
    };

    DepthRepresentationInfo {
        z_near,
        z_far,
        d_min,
        d_max,
        representation_type,
    }
}

/// Parse a single DepthRepValueSigned from a byte slice.
///
/// Format: `mantissa(i16) | exponent(u8) | sign(u8)`
/// Value = (-1)^sign * mantissa * 2^exponent
///
/// Returns `None` if not enough data.
fn parse_depth_rep_value(data: &[u8], pos: &mut usize) -> Option<f64> {
    if *pos + 4 > data.len() {
        return None;
    }

    let mantissa = i16::from_be_bytes([data[*pos], data[*pos + 1]]);
    let exponent = data[*pos + 2];
    let sign = data[*pos + 3];
    *pos += 4;

    // value = (-1)^sign * mantissa * 2^exponent
    let value = (mantissa as f64) * pow2_f64(exponent as i32);
    if sign != 0 { Some(-value) } else { Some(value) }
}

/// Compute 2^exp for a non-negative integer exponent, no_std compatible.
fn pow2_f64(exp: i32) -> f64 {
    if exp == 0 {
        1.0
    } else if exp > 0 && exp < 64 {
        (1u64 << exp as u32) as f64
    } else {
        // Fallback for large or negative exponents
        let mut result = 1.0f64;
        if exp > 0 {
            for _ in 0..exp {
                result *= 2.0;
            }
        } else {
            for _ in 0..(-exp) {
                result *= 0.5;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_auxiliary_type_from_urn_alpha() {
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:mpeg:hevc:2015:auxid:1"),
            AuxiliaryImageType::Alpha
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"),
            AuxiliaryImageType::Alpha
        );
    }

    #[test]
    fn test_auxiliary_type_from_urn_depth() {
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:mpeg:hevc:2015:auxid:2"),
            AuxiliaryImageType::Depth
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:mpeg:mpegB:cicp:systems:auxiliary:depth"),
            AuxiliaryImageType::Depth
        );
    }

    #[test]
    fn test_auxiliary_type_from_urn_apple_types() {
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2018:aux:portraiteffectsmatte"),
            AuxiliaryImageType::PortraitMatte
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2020:aux:hdrgainmap"),
            AuxiliaryImageType::HdrGainMap
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2019:aux:semanticskinmatte"),
            AuxiliaryImageType::SkinMatte
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2019:aux:semanticteethmatte"),
            AuxiliaryImageType::TeethMatte
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2019:aux:semantichairmatte"),
            AuxiliaryImageType::HairMatte
        );
        assert_eq!(
            AuxiliaryImageType::from_urn("urn:com:apple:photo:2020:aux:semanticglassesmatte"),
            AuxiliaryImageType::GlassesMatte
        );
    }

    #[test]
    fn test_auxiliary_type_from_urn_unknown() {
        let t = AuxiliaryImageType::from_urn("urn:example:custom:type");
        assert_eq!(
            t,
            AuxiliaryImageType::Other(String::from("urn:example:custom:type"))
        );
    }

    #[test]
    fn test_auxiliary_type_from_urn_empty() {
        let t = AuxiliaryImageType::from_urn("");
        assert_eq!(t, AuxiliaryImageType::Other(String::from("")));
    }

    #[test]
    fn test_auxiliary_type_roundtrip() {
        let types = [
            AuxiliaryImageType::Alpha,
            AuxiliaryImageType::Depth,
            AuxiliaryImageType::PortraitMatte,
            AuxiliaryImageType::HdrGainMap,
            AuxiliaryImageType::SkinMatte,
            AuxiliaryImageType::TeethMatte,
            AuxiliaryImageType::HairMatte,
            AuxiliaryImageType::GlassesMatte,
        ];
        for t in &types {
            let urn = t.urn();
            let parsed = AuxiliaryImageType::from_urn(urn);
            assert_eq!(&parsed, t, "roundtrip failed for {t}");
        }
    }

    #[test]
    fn test_depth_representation_type_from_code() {
        assert_eq!(
            DepthRepresentationType::from_code(0),
            Some(DepthRepresentationType::UniformInverseZ)
        );
        assert_eq!(
            DepthRepresentationType::from_code(1),
            Some(DepthRepresentationType::UniformDisparity)
        );
        assert_eq!(
            DepthRepresentationType::from_code(2),
            Some(DepthRepresentationType::UniformZ)
        );
        assert_eq!(
            DepthRepresentationType::from_code(3),
            Some(DepthRepresentationType::NonuniformDisparity)
        );
        assert_eq!(DepthRepresentationType::from_code(4), None);
        assert_eq!(DepthRepresentationType::from_code(255), None);
    }

    #[test]
    fn test_depth_representation_type_roundtrip() {
        for code in 0..4 {
            let t = DepthRepresentationType::from_code(code).unwrap();
            assert_eq!(t.code(), code);
        }
    }

    #[test]
    fn test_parse_depth_representation_info_empty() {
        let info = parse_depth_representation_info(&[]);
        assert_eq!(
            info.representation_type,
            DepthRepresentationType::UniformInverseZ
        );
        assert!(info.z_near.is_none());
        assert!(info.z_far.is_none());
        assert!(info.d_min.is_none());
        assert!(info.d_max.is_none());
    }

    #[test]
    fn test_parse_depth_representation_info_type_only() {
        // Type=2 (UniformZ), no flags
        let info = parse_depth_representation_info(&[2, 0]);
        assert_eq!(info.representation_type, DepthRepresentationType::UniformZ);
        assert!(info.z_near.is_none());
    }

    #[test]
    fn test_parse_depth_representation_info_with_z_near_far() {
        // Type=0 (UniformInverseZ), flags=0x03 (z_near + z_far)
        // z_near: mantissa=100 (0x0064), exponent=0, sign=0 => 100.0
        // z_far:  mantissa=500 (0x01F4), exponent=0, sign=0 => 500.0
        let data = [
            0,    // representation_type
            0x03, // flags: z_near + z_far
            0x00, 0x64, 0x00, 0x00, // z_near = 100
            0x01, 0xF4, 0x00, 0x00, // z_far = 500
        ];
        let info = parse_depth_representation_info(&data);
        assert_eq!(
            info.representation_type,
            DepthRepresentationType::UniformInverseZ
        );
        assert!((info.z_near.unwrap() - 100.0).abs() < f64::EPSILON);
        assert!((info.z_far.unwrap() - 500.0).abs() < f64::EPSILON);
        assert!(info.d_min.is_none());
        assert!(info.d_max.is_none());
    }

    #[test]
    fn test_parse_depth_representation_info_with_all_values() {
        // Type=1 (UniformDisparity), flags=0x0F (all four)
        // z_near: mantissa=10, exponent=1, sign=0 => 10 * 2 = 20.0
        // z_far:  mantissa=50, exponent=2, sign=0 => 50 * 4 = 200.0
        // d_min:  mantissa=1,  exponent=0, sign=1 => -1.0
        // d_max:  mantissa=255, exponent=0, sign=0 => 255.0
        let data = [
            1,    // representation_type
            0x0F, // flags: all four
            0x00, 0x0A, 0x01, 0x00, // z_near = 10 * 2^1 = 20
            0x00, 0x32, 0x02, 0x00, // z_far = 50 * 2^2 = 200
            0x00, 0x01, 0x00, 0x01, // d_min = -1
            0x00, 0xFF, 0x00, 0x00, // d_max = 255
        ];
        let info = parse_depth_representation_info(&data);
        assert_eq!(
            info.representation_type,
            DepthRepresentationType::UniformDisparity
        );
        assert!((info.z_near.unwrap() - 20.0).abs() < f64::EPSILON);
        assert!((info.z_far.unwrap() - 200.0).abs() < f64::EPSILON);
        assert!((info.d_min.unwrap() - (-1.0)).abs() < f64::EPSILON);
        assert!((info.d_max.unwrap() - 255.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_depth_representation_info_truncated() {
        // Flags say z_near present but data is truncated
        let data = [0, 0x01, 0x00]; // only 1 byte of the 4-byte value
        let info = parse_depth_representation_info(&data);
        assert!(info.z_near.is_none());
    }

    #[test]
    fn test_pow2_f64() {
        assert!((pow2_f64(0) - 1.0).abs() < f64::EPSILON);
        assert!((pow2_f64(1) - 2.0).abs() < f64::EPSILON);
        assert!((pow2_f64(10) - 1024.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_display_auxiliary_type() {
        assert_eq!(format!("{}", AuxiliaryImageType::Depth), "Depth");
        assert_eq!(
            format!("{}", AuxiliaryImageType::Other(String::from("custom"))),
            "Other(custom)"
        );
    }

    #[test]
    fn test_display_depth_representation_type() {
        assert_eq!(
            format!("{}", DepthRepresentationType::UniformInverseZ),
            "UniformInverseZ"
        );
        assert_eq!(
            format!("{}", DepthRepresentationType::NonuniformDisparity),
            "NonuniformDisparity"
        );
    }
}
