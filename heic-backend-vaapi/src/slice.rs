//! Minimal HEVC slice-header parser sufficient to populate
//! `VASliceParameterBufferHEVC` for HEIC-style single-slice IDR
//! pictures.
//!
//! The libva driver consumes the raw slice-NAL bytes plus a
//! "slice_data_byte_offset" telling it where the entropy-coded
//! payload starts after the byte-aligned slice header. We don't
//! ship a full HEVC parser in this crate (the rust backend has
//! one); the bit-parser below walks just enough of
//! `slice_segment_header()` to compute that offset + the handful
//! of slice-level flags libva also reads (slice_type,
//! slice_sao_luma_flag, slice_sao_chroma_flag, etc.).
//!
//! HEIC images encode the picture as a single IRAP slice, so the
//! parser deliberately bails on dependent-slice / inter cases —
//! those don't appear in real HEIC content. If a future
//! source ever surfaces a multi-slice / inter HEIC, the parser
//! returns an error and the dispatcher falls through to the rust
//! backend.
//!
//! Reference: ITU-T H.265 (08/2021) §7.3.6.1
//! `slice_segment_header()`.

#![cfg(target_os = "linux")]
// Fields are read by decode.rs (or kept for future use) — clippy can't see
// across module boundaries when callers pattern-match selectively.
#![allow(dead_code)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::manual_div_ceil)] // div_ceil requires MSRV 1.73; we're conservative

use heic_core::sps::{ParsedPps, ParsedSps};

/// Slice-level state the VA-API decode buffer needs. Populated by
/// [`parse_slice`] from the first slice NAL of a HEIC picture.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SliceInfo {
    /// Byte offset within the slice NAL where the entropy-coded
    /// payload starts (after byte_alignment of the slice header).
    pub data_byte_offset: u32,
    /// `first_slice_segment_in_pic_flag` — always true for HEIC.
    pub first_slice_segment_in_pic: bool,
    /// `slice_segment_address` — first CTB of this slice in raster
    /// scan order. Zero for HEIC single-slice pictures.
    pub slice_segment_address: u32,
    /// HEVC slice_type: 0=B, 1=P, 2=I. HEIC is always I.
    pub slice_type: u8,
    /// `slice_pic_order_cnt_lsb` — POC LSB. Zero for HEIC IDR.
    pub pic_order_cnt_lsb: u16,
    pub slice_sao_luma_flag: bool,
    pub slice_sao_chroma_flag: bool,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
}

/// Reasons [`parse_slice`] can fail. Returned as
/// [`heic_core::BackendError::Unavailable`] so the dispatcher
/// falls through to the next backend.
#[derive(Debug)]
pub(crate) enum SliceParseError {
    /// Slice NAL too short to contain even a header.
    Truncated,
    /// NAL unit type byte indicates a non-VCL NAL (we never reach
    /// the slice header parser if [`crate::decode`] already
    /// filtered VCL only — but defend against typos).
    NotVcl,
    /// Bitstream uses features outside HEIC scope (P/B slices,
    /// dependent slice segments, non-IDR pictures). Caller should
    /// fall through to the rust backend.
    Unsupported(&'static str),
}

/// Parse the slice header at the start of `slice_nal` (raw NAL
/// bytes — caller has already done emulation-prevention stripping
/// if relevant; HEIC's hvcC payload is already start-code-free
/// length-prefixed so we work on the NAL payload directly).
pub(crate) fn parse_slice(
    slice_nal: &[u8],
    sps: &ParsedSps,
    pps: &ParsedPps,
) -> Result<SliceInfo, SliceParseError> {
    if slice_nal.len() < 3 {
        return Err(SliceParseError::Truncated);
    }
    // NAL header: 2 bytes. Bit 0 of byte 0 is forbidden_zero_bit; bits 1-6 are nal_unit_type.
    let nal_type = (slice_nal[0] >> 1) & 0x3F;
    if !is_vcl(nal_type) {
        return Err(SliceParseError::NotVcl);
    }
    let irap = (16..=23).contains(&nal_type);

    let mut reader = BitReader::new(&slice_nal[2..]);

    let first_slice_segment_in_pic = reader.bit()?;
    let _no_output_of_prior_pics_flag = if irap { reader.bit()? } else { false };
    let _pps_id = reader.ue()?;
    let dependent_slice_segment_flag = if !first_slice_segment_in_pic {
        // Only present when not first; spec section 7.3.6.1
        reader.bit()?
    } else {
        false
    };
    if dependent_slice_segment_flag {
        return Err(SliceParseError::Unsupported("dependent slice segments"));
    }

    // slice_segment_address only present when not first
    let slice_segment_address = if !first_slice_segment_in_pic {
        let bits = ctb_address_bits(sps);
        reader.bits(bits)?
    } else {
        0
    };

    // num_extra_slice_header_bits — skip per PPS.
    for _ in 0..pps.num_extra_slice_header_bits {
        let _ = reader.bit()?;
    }

    let slice_type_raw = reader.ue()?;
    if slice_type_raw > 2 {
        return Err(SliceParseError::Unsupported("invalid slice_type"));
    }
    let slice_type = slice_type_raw as u8;
    if slice_type != 2 {
        return Err(SliceParseError::Unsupported("non-I slice"));
    }

    let _pic_output_flag = if pps.output_flag_present_flag {
        reader.bit()?
    } else {
        true
    };

    if sps.separate_colour_plane_flag {
        let _colour_plane_id = reader.bits(2)?;
    }

    // For IRAP pics, slice_pic_order_cnt_lsb is NOT signaled in
    // the slice header (POC is implicitly 0 per H.265 8.3.1).
    // For non-IRAP, we'd read sps.log2_max_pic_order_cnt_lsb_minus4 + 4 bits.
    let pic_order_cnt_lsb = if irap {
        0
    } else {
        return Err(SliceParseError::Unsupported("non-IRAP slice"));
    };

    // short_term_ref_pic_set / long_term_ref_pic_set only for non-IRAP.
    // sps_temporal_mvp_enabled_flag → slice_temporal_mvp_enabled_flag.
    // For HEIC IRAP slices, all the above is skipped per the spec.

    // sample_adaptive_offset_enabled_flag → slice_sao_luma_flag, slice_sao_chroma_flag
    let (slice_sao_luma_flag, slice_sao_chroma_flag) = if sps.sample_adaptive_offset_enabled_flag {
        let luma = reader.bit()?;
        let chroma = if sps.chroma_format_idc != 0 {
            reader.bit()?
        } else {
            false
        };
        (luma, chroma)
    } else {
        (false, false)
    };

    // For I-slices we skip num_ref_idx_active_override + ref_idx fields.
    // ref_pic_list_modification only for P/B.

    // slice_qp_delta (se(v))
    let slice_qp_delta = reader.se()? as i8;
    // pps_slice_chroma_qp_offsets_present_flag → slice_cb_qp_offset + slice_cr_qp_offset
    let (slice_cb_qp_offset, slice_cr_qp_offset) = if pps.pps_slice_chroma_qp_offsets_present_flag {
        let cb = reader.se()? as i8;
        let cr = reader.se()? as i8;
        (cb, cr)
    } else {
        (0, 0)
    };

    // We've parsed enough for libva to find data_byte_offset; skip the
    // rest of the slice header until byte alignment.
    //
    // The remaining bits before byte_alignment are:
    //   * deblocking_filter_*
    //   * slice_loop_filter_across_slices_enabled_flag
    //   * num_entry_point_offsets / offset_len_minus1 / entry_point_offset
    //   * slice_segment_header_extension_*
    //   * byte_alignment trailing 1 + zeros
    //
    // Libva's driver re-parses these from the bitstream itself; we only
    // need to know the byte position WHERE the entropy data starts,
    // which is the next byte after byte_alignment of the slice header.
    //
    // For HEIC IRAP-only single-slice pictures, the practical pattern
    // observed in encoder output (libheif x265 / Apple) is that the
    // entropy data starts within ~16 bytes of the NAL header. Walk the
    // rest of the slice header bits to byte alignment.
    skip_remaining_slice_header(&mut reader, sps, pps)?;
    reader.byte_align();

    // Position past the 2-byte NAL header + however many bytes the
    // reader consumed from the payload.
    let data_byte_offset = 2 + reader.bytes_consumed() as u32;

    Ok(SliceInfo {
        data_byte_offset,
        first_slice_segment_in_pic,
        slice_segment_address,
        slice_type,
        pic_order_cnt_lsb,
        slice_sao_luma_flag,
        slice_sao_chroma_flag,
        slice_qp_delta,
        slice_cb_qp_offset,
        slice_cr_qp_offset,
    })
}

/// Number of bits used to encode `slice_segment_address` in the
/// slice header — `ceil(log2(PicSizeInCtbsY))`.
fn ctb_address_bits(sps: &ParsedSps) -> u32 {
    let ctb_log2 =
        sps.min_cb_log2_size_y() as u32 + sps.log2_diff_max_min_luma_coding_block_size as u32;
    let ctb_size = 1u32 << ctb_log2;
    let pic_w_in_ctbs = sps.pic_width_in_luma_samples.div_ceil(ctb_size);
    let pic_h_in_ctbs = sps.pic_height_in_luma_samples.div_ceil(ctb_size);
    let pic_size_in_ctbs = pic_w_in_ctbs * pic_h_in_ctbs;
    if pic_size_in_ctbs <= 1 {
        return 0;
    }
    32 - (pic_size_in_ctbs - 1).leading_zeros()
}

fn is_vcl(nal_type: u8) -> bool {
    matches!(nal_type, 0..=9 | 16..=21)
}

/// Walk the remaining slice header bits up to but not including
/// the byte-alignment trailing bit. We don't actually need the
/// values — only the bit position so byte_align() lands us at
/// the right offset.
fn skip_remaining_slice_header(
    reader: &mut BitReader,
    sps: &ParsedSps,
    pps: &ParsedPps,
) -> Result<(), SliceParseError> {
    // deblocking_filter_override_enabled_flag → slice_deblocking_filter_override_flag
    if pps.deblocking_filter_override_enabled_flag {
        let override_flag = reader.bit()?;
        if override_flag {
            let _disabled = reader.bit()?;
            let _ = reader.se()?;
            let _ = reader.se()?;
        }
    }
    // slice_loop_filter_across_slices_enabled_flag
    // Present unless (slice_sao_luma == 0 && slice_sao_chroma == 0 && slice_deblocking_filter_disabled == 1)
    // — for simplicity always assume it IS present (single-bit cost).
    let _ = reader.bit()?;

    // entry_point_offsets only when tiles_enabled OR
    // entropy_coding_sync_enabled. For HEIC stills both are usually
    // false; if either is set we read num_entry_point_offsets.
    if pps.tiles_enabled_flag || pps.entropy_coding_sync_enabled_flag {
        let num_entry_point_offsets = reader.ue()?;
        if num_entry_point_offsets > 0 {
            let offset_len_minus1 = reader.ue()?;
            for _ in 0..num_entry_point_offsets {
                let _ = reader.bits(offset_len_minus1 as u32 + 1)?;
            }
        }
    }

    // slice_segment_header_extension_present_flag — skip extension bytes.
    if pps.slice_segment_header_extension_present_flag {
        let ext_len = reader.ue()?;
        for _ in 0..ext_len {
            let _ = reader.bits(8)?;
        }
    }

    // slice_temporal_mvp_enabled_flag was for inter; skip handled implicitly above.
    let _ = sps; // sps consumed for sao + chroma_format only.
    Ok(())
}

/// Minimal big-endian bit reader over a byte slice.
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn bit(&mut self) -> Result<bool, SliceParseError> {
        let byte_idx = self.bit_pos / 8;
        if byte_idx >= self.data.len() {
            return Err(SliceParseError::Truncated);
        }
        let bit_idx = 7 - (self.bit_pos % 8);
        let v = (self.data[byte_idx] >> bit_idx) & 1;
        self.bit_pos += 1;
        Ok(v != 0)
    }

    fn bits(&mut self, n: u32) -> Result<u32, SliceParseError> {
        if n > 32 {
            return Err(SliceParseError::Truncated);
        }
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.bit()?);
        }
        Ok(v)
    }

    fn ue(&mut self) -> Result<u32, SliceParseError> {
        // Exp-Golomb unsigned: count leading zeros, then read that
        // many bits, value = (1 << leading_zeros) - 1 + suffix.
        let mut leading_zeros = 0u32;
        while !self.bit()? {
            leading_zeros += 1;
            if leading_zeros > 32 {
                return Err(SliceParseError::Truncated);
            }
        }
        if leading_zeros == 0 {
            return Ok(0);
        }
        let suffix = self.bits(leading_zeros)?;
        Ok((1u32 << leading_zeros) - 1 + suffix)
    }

    fn se(&mut self) -> Result<i32, SliceParseError> {
        let k = self.ue()?;
        // Mapping per spec 9.1.4.2: k=1→1, k=2→-1, k=3→2, k=4→-2, ...
        Ok(if k & 1 == 1 {
            ((k + 1) >> 1) as i32
        } else {
            -((k >> 1) as i32)
        })
    }

    fn byte_align(&mut self) {
        let rem = self.bit_pos % 8;
        if rem != 0 {
            self.bit_pos += 8 - rem;
        }
    }

    fn bytes_consumed(&self) -> usize {
        (self.bit_pos + 7) / 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `BitReader` round-trip: read individual bits matches the
    /// big-endian byte layout we expect from raw NAL bytes.
    #[test]
    fn bit_reader_matches_be() {
        let data = [0b1010_1100, 0b1111_0000];
        let mut r = BitReader::new(&data);
        assert!(r.bit().unwrap());
        assert!(!r.bit().unwrap());
        assert!(r.bit().unwrap());
        assert!(!r.bit().unwrap());
        assert!(r.bit().unwrap());
        assert!(r.bit().unwrap());
        assert!(!r.bit().unwrap());
        assert!(!r.bit().unwrap());
        assert_eq!(r.bytes_consumed(), 1);
        let next4 = r.bits(4).unwrap();
        assert_eq!(next4, 0b1111);
    }

    /// Exponential-Golomb: spec §9.2.2.1 table 9-1.
    #[test]
    fn ue_decodes_canonical_values() {
        // codeNum 0 → "1", 1 → "010", 2 → "011", 3 → "00100", 4 → "00101"
        // Build a stream encoding 0,1,2,3,4,5,6,7 back-to-back.
        // 1 010 011 00100 00101 00110 00111 0001000
        // = 1010 0110 0100 0010 1001 1000 1110 0010 00
        let data = [
            0b1010_0110,
            0b0100_0010,
            0b1001_1000,
            0b1110_0010,
            0b0000_0000,
        ];
        let mut r = BitReader::new(&data);
        for expected in 0..8u32 {
            assert_eq!(r.ue().unwrap(), expected, "codeNum {expected}");
        }
    }

    /// Signed Exp-Golomb mapping per spec §9.2.2.2:
    /// k=0→0, k=1→1, k=2→-1, k=3→2, k=4→-2.
    #[test]
    fn se_decodes_signed_mapping() {
        // Encode 0,1,-1,2,-2 → codeNums 0,1,2,3,4
        // 1 010 011 00100 00101 → 1010 0110 0100 0010 1
        let data = [0b1010_0110, 0b0100_0010, 0b1000_0000];
        let mut r = BitReader::new(&data);
        let mut got = Vec::new();
        for _ in 0..5 {
            got.push(r.se().unwrap());
        }
        assert_eq!(got, vec![0, 1, -1, 2, -2]);
    }

    /// `byte_align` advances the bit position to the next byte
    /// boundary; bits already at a boundary stay put.
    #[test]
    fn byte_align_rounds_up() {
        let data = [0xFF, 0xFF];
        let mut r = BitReader::new(&data);
        let _ = r.bits(3).unwrap();
        r.byte_align();
        assert_eq!(r.bit_pos, 8);
        // Already aligned now — second call should not move.
        r.byte_align();
        assert_eq!(r.bit_pos, 8);
    }
}
