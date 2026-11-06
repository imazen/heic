//! HEVC depth representation SEI carried by auxC (ISO/IEC 23008-12).
//! Syntax: ITU-T H.265 G.14.2.4. Independent oracle: libheif's
//! libheif/codecs/hevc_boxes.cc, decode_hevc_aux_sei_messages.
use super::{DepthRepresentationInfo, DepthRepresentationType};
use crate::{HeicError, Result};
use alloc::vec::Vec;
use whereat::at;

fn invalid() -> whereat::At<HeicError> {
    at!(HeicError::InvalidData("invalid depth auxC SEI"))
}

struct Bits<'a> {
    data: &'a [u8],
    bit: usize,
}
impl Bits<'_> {
    fn read(&mut self, n: usize) -> Option<u32> {
        if n > 32 || self.bit.checked_add(n)? > self.data.len().checked_mul(8)? {
            return None;
        }
        let mut value = 0;
        for _ in 0..n {
            value = (value << 1) | u32::from((self.data[self.bit / 8] >> (7 - self.bit % 8)) & 1);
            self.bit += 1;
        }
        Some(value)
    }
    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return None;
            }
        }
        Some(((1u32 << zeros) - 1) + self.read(zeros)?)
    }
    fn value(&mut self) -> Option<f64> {
        let sign = self.read(1)?;
        let exponent = self.read(7)?;
        if exponent == 127 {
            return None;
        } // reserved/unspecified value
        let count = self.read(5)? as usize + 1;
        let mantissa = f64::from(self.read(count)?);
        let pow2 = |exponent: i32| f64::from_bits(((exponent + 1023) as u64) << 52);
        let value = if exponent == 0 {
            pow2(-(30 + count as i32)) * mantissa
        } else {
            pow2(exponent as i32 - 31) * (1.0 + mantissa * pow2(-(count as i32)))
        };
        Some(if sign != 0 { -value } else { value })
    }
}

fn payload(data: &[u8]) -> Option<DepthRepresentationInfo> {
    let mut bits = Bits { data, bit: 0 };
    let flags = [bits.read(1)?, bits.read(1)?, bits.read(1)?, bits.read(1)?];
    let code = u8::try_from(bits.ue()?).ok()?;
    let representation_type = DepthRepresentationType::from_code(code)?;
    if flags[2] != 0 || flags[3] != 0 {
        bits.ue()?;
    }
    let mut value = |flag| {
        if flag != 0 {
            bits.value().map(Some)
        } else {
            Some(None)
        }
    };
    let info = DepthRepresentationInfo {
        z_near: value(flags[0])?,
        z_far: value(flags[1])?,
        d_min: value(flags[2])?,
        d_max: value(flags[3])?,
        representation_type,
    };
    // The nonlinear model is not represented by this API. Refuse rather than
    // silently exposing its codes as uniform disparity.
    if representation_type == DepthRepresentationType::NonuniformDisparity {
        return None;
    }
    Some(info)
}

/// Strict auxC framing: u32 byte length, then u32 NAL length + HEVC NAL.
/// No inference of a representation from missing or malformed metadata.
pub(crate) fn parse(data: &[u8]) -> Result<DepthRepresentationInfo> {
    let length =
        u32::from_be_bytes(data.get(..4).ok_or_else(invalid)?.try_into().unwrap()) as usize;
    let mut nals = data
        .get(4..)
        .filter(|b| b.len() == length)
        .ok_or_else(invalid)?;
    let mut found = None;
    while !nals.is_empty() {
        let size =
            u32::from_be_bytes(nals.get(..4).ok_or_else(invalid)?.try_into().unwrap()) as usize;
        let end = size.checked_add(4).ok_or_else(invalid)?;
        let nal = nals.get(4..end).ok_or_else(invalid)?;
        nals = &nals[end..];
        if nal.len() < 2 || nal[0] & 128 != 0 || nal[1] & 7 == 0 {
            return Err(invalid());
        }
        if !matches!((nal[0] >> 1) & 63, 39 | 40) {
            continue;
        }
        let mut rbsp = Vec::new();
        rbsp.try_reserve_exact(nal.len() - 2)
            .map_err(|_| at!(HeicError::LimitExceeded("depth SEI allocation")))?;
        let mut zeros = 0;
        for &b in &nal[2..] {
            if zeros == 2 && b == 3 {
                zeros = 0;
                continue;
            }
            rbsp.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        let mut messages = rbsp.as_slice();
        while !messages.is_empty() && messages != [128] {
            let mut number = || -> Option<usize> {
                let mut n = 0usize;
                loop {
                    let (&b, rest) = messages.split_first()?;
                    messages = rest;
                    n = n.checked_add(b as usize)?;
                    if b != 255 {
                        return Some(n);
                    }
                }
            };
            let kind = number().ok_or_else(invalid)?;
            let len = number().ok_or_else(invalid)?;
            let bytes = messages.get(..len).ok_or_else(invalid)?;
            if kind == 177 {
                if found.is_some() {
                    return Err(invalid());
                }
                found = Some(payload(bytes).ok_or_else(invalid)?);
            }
            messages = &messages[len..];
        }
    }
    found.ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reads_length_prefixed_sei_not_legacy_byte_fields() {
        // The real auxC layout reproduced in libheif's source. Flags d_min /
        // d_max, ue(v) type 1 (uniform disparity), not a byte-valued type.
        let bytes = [
            0, 0, 0, 17, 0, 0, 0, 13, 0x4e, 1, 177, 9, 0x35, 0x1e, 0x78, 0xc8, 1, 3, 0xc5, 0xd0,
            0x20,
        ];
        let info = parse(&bytes).unwrap();
        assert_eq!(
            info.representation_type,
            DepthRepresentationType::UniformDisparity
        );
        assert!(info.z_near.is_none() && info.z_far.is_none());
        assert!(info.d_min.unwrap().is_finite() && info.d_max.unwrap().is_finite());
        for len in 0..bytes.len() {
            assert!(parse(&bytes[..len]).is_err());
        }
    }
}
