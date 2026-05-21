//! NAL-unit stream helpers shared across backends.
//!
//! Native backends accept different NAL formats:
//!
//! * Windows Media Foundation, Android MediaCodec → Annex B (start-code prefix).
//! * Apple VideoToolbox → hvcC length-prefixed (4-byte big-endian length).
//! * Linux VA-API → raw NALs submitted one-by-one with separate parameter
//!   buffers (no framing).
//!
//! HEIF containers store hvcC length-prefixed slice data. The helpers in this
//! module convert between the two formats and probe SPS dimensions without
//! pulling in the full pure-Rust parser.

use alloc::vec::Vec;

/// Convert a length-prefixed hvcC bitstream to Annex B (start-code prefix).
///
/// `data` is a sequence of `(length, payload)` records where the length is
/// `length_size` bytes big-endian.
///
/// Each NAL is prefixed with `00 00 00 01` (the 4-byte start code). Returns
/// `None` if `data` is malformed (truncated length prefix, declared length
/// extends past `data`, or `length_size` is outside 1..=4).
pub fn hvcc_to_annexb(data: &[u8], length_size: u8) -> Option<Vec<u8>> {
    if !(1..=4).contains(&length_size) {
        return None;
    }
    let ls = length_size as usize;
    let mut out = Vec::with_capacity(data.len() + data.len() / 64 + 4);
    let mut i = 0;
    while i < data.len() {
        if i + ls > data.len() {
            return None;
        }
        let mut nal_len: usize = 0;
        for &b in &data[i..i + ls] {
            nal_len = (nal_len << 8) | (b as usize);
        }
        i += ls;
        if i + nal_len > data.len() {
            return None;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(&data[i..i + nal_len]);
        i += nal_len;
    }
    Some(out)
}

/// Concatenate VPS / SPS / PPS NAL payloads as a single Annex B blob.
///
/// Used by Windows Media Foundation (`MF_MT_MPEG_SEQUENCE_HEADER`), Android
/// MediaCodec (CSD-0), and AMF (`AMF_VIDEO_DECODER_EXTRADATA`).
pub fn annexb_parameter_sets(nal_units: &[&[u8]]) -> Vec<u8> {
    let total: usize = nal_units.iter().map(|n| n.len() + 4).sum();
    let mut out = Vec::with_capacity(total);
    for nal in nal_units {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn hvcc_two_nals_4byte_length() {
        // Two NALs of 3 and 5 bytes, 4-byte length prefix
        let mut data = Vec::new();
        data.extend_from_slice(&[0, 0, 0, 3]);
        data.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        data.extend_from_slice(&[0, 0, 0, 5]);
        data.extend_from_slice(&[1, 2, 3, 4, 5]);
        let ab = hvcc_to_annexb(&data, 4).unwrap();
        assert_eq!(
            ab,
            vec![0, 0, 0, 1, 0xAA, 0xBB, 0xCC, 0, 0, 0, 1, 1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn hvcc_truncated_returns_none() {
        let data = [0, 0, 0, 10, 1, 2, 3]; // declared 10 bytes but only 3 follow
        assert!(hvcc_to_annexb(&data, 4).is_none());
    }

    #[test]
    fn hvcc_bad_length_size() {
        assert!(hvcc_to_annexb(&[0; 4], 0).is_none());
        assert!(hvcc_to_annexb(&[0; 4], 5).is_none());
    }

    #[test]
    fn parameter_sets_concat() {
        let vps = [0x40, 0x01, 0x0C];
        let sps = [0x42, 0x01, 0x01];
        let pps = [0x44, 0x01];
        let out = annexb_parameter_sets(&[&vps[..], &sps[..], &pps[..]]);
        assert_eq!(
            out,
            vec![
                0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0, 0, 0, 1, 0x44, 0x01
            ]
        );
    }
}
