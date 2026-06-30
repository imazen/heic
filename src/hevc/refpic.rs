//! Reference picture set parsing and list construction (H.265 7.3.7, 8.3.2-8.3.4)
//!
//! Handles short-term and long-term reference picture management:
//! - Parsing `short_term_ref_pic_set()` from SPS and slice header
//! - POC derivation (8.3.1)
//! - Reference picture list construction (8.3.4)

#![allow(dead_code)] // Phase 0-1: types used in subsequent phases

use alloc::vec::Vec;

use super::bitstream::BitstreamReader;
use super::inter::{MAX_NUM_REF_PICS, RefPicLists};
use crate::error::HevcError;

type Result<T> = core::result::Result<T, HevcError>;

/// Maximum number of short-term reference picture sets in SPS
pub const MAX_SHORT_TERM_RPS_COUNT: usize = 64;

/// A single short-term reference picture set (H.265 7.4.8)
///
/// Contains delta-POC values relative to the current picture for
/// past (S0/negative) and future (S1/positive) reference pictures.
#[derive(Clone, Debug, Default)]
pub struct ShortTermRefPicSet {
    /// Delta POC values for negative (past) references
    pub delta_poc_s0: [i32; MAX_NUM_REF_PICS],
    /// Delta POC values for positive (future) references
    pub delta_poc_s1: [i32; MAX_NUM_REF_PICS],
    /// Whether each S0 reference is used by the current picture
    pub used_by_curr_pic_s0: [bool; MAX_NUM_REF_PICS],
    /// Whether each S1 reference is used by the current picture
    pub used_by_curr_pic_s1: [bool; MAX_NUM_REF_PICS],
    /// Number of negative (past) references
    pub num_negative_pics: u8,
    /// Number of positive (future) references
    pub num_positive_pics: u8,
}

impl ShortTermRefPicSet {
    /// Total number of delta POC entries
    pub fn num_delta_pocs(&self) -> u8 {
        self.num_negative_pics + self.num_positive_pics
    }
}

/// Long-term reference picture SPS info
#[derive(Clone, Debug, Default)]
pub struct LongTermRefPicSps {
    /// POC LSB values for long-term reference pictures
    pub lt_ref_pic_poc_lsb: Vec<u32>,
    /// Used-by-current-picture flags
    pub used_by_curr_pic_lt: Vec<bool>,
}

/// Parse short_term_ref_pic_set() syntax (H.265 7.3.7)
///
/// `st_rps_idx` is the index of this RPS (0..num_short_term_ref_pic_sets for SPS,
/// or num_short_term_ref_pic_sets for inline slice header RPS).
/// `prev_sets` contains the already-parsed sets for inter-RPS prediction.
pub fn parse_short_term_rps(
    reader: &mut BitstreamReader<'_>,
    st_rps_idx: u8,
    num_short_term_ref_pic_sets: u8,
    prev_sets: &[ShortTermRefPicSet],
) -> Result<ShortTermRefPicSet> {
    let mut rps = ShortTermRefPicSet::default();

    let inter_ref_pic_set_prediction_flag = if st_rps_idx != 0 {
        reader.read_bit()? != 0
    } else {
        false
    };

    if inter_ref_pic_set_prediction_flag {
        // Inter-RPS prediction: derive from a previous set.
        // delta_idx_minus1 is read as ue(v), so a malformed bitstream can make it
        // far larger than st_rps_idx. Do the index arithmetic in a wider type with
        // checked ops so an out-of-range value yields a decode error instead of an
        // integer-overflow panic (e.g. delta_idx_minus1 == 255 overflowing `+ 1`).
        let delta_idx_minus1 = if st_rps_idx == num_short_term_ref_pic_sets {
            // In slice header: delta_idx_minus1 is present
            reader.read_ue()?
        } else {
            0
        };

        // ref_rps_idx = st_rps_idx - (delta_idx_minus1 + 1)
        let ref_rps_idx = u32::from(st_rps_idx)
            .checked_sub(delta_idx_minus1)
            .and_then(|v| v.checked_sub(1))
            .ok_or(HevcError::InvalidBitstream(
                "inter_ref_pic_set: ref index out of range",
            ))?;

        if ref_rps_idx as usize >= prev_sets.len() {
            return Err(HevcError::InvalidBitstream(
                "inter_ref_pic_set: ref index beyond parsed sets",
            ));
        }

        let delta_rps_sign = reader.read_bit()?;
        let abs_delta_rps_minus1 = reader.read_ue()?;
        let delta_rps = (1i32 - 2 * delta_rps_sign as i32)
            .wrapping_mul((abs_delta_rps_minus1 as i32).wrapping_add(1));

        let ref_set = &prev_sets[ref_rps_idx as usize];
        let ref_num_delta_pocs = ref_set.num_delta_pocs() as usize;

        // Parse used_by_curr_pic_flag and use_delta_flag for each entry + 1
        let mut used_by_curr_pic_flag = [false; 33];
        let mut use_delta_flag = [true; 33]; // default true per spec

        for j in 0..=ref_num_delta_pocs {
            used_by_curr_pic_flag[j] = reader.read_bit()? != 0;
            if !used_by_curr_pic_flag[j] {
                use_delta_flag[j] = reader.read_bit()? != 0;
            }
        }

        // Derive the new RPS from the reference set
        // Collect all delta POC values from the reference set
        let mut ref_delta_pocs = [0i32; 32];
        let mut ref_count = 0;
        for i in 0..ref_set.num_negative_pics as usize {
            ref_delta_pocs[ref_count] = ref_set.delta_poc_s0[i];
            ref_count += 1;
        }
        for i in 0..ref_set.num_positive_pics as usize {
            ref_delta_pocs[ref_count] = ref_set.delta_poc_s1[i];
            ref_count += 1;
        }

        // Derive new delta POC values (H.265 7.4.8 equation)
        let mut neg_count = 0u8;
        let mut pos_count = 0u8;

        // Process the "extra" entry (j == ref_num_delta_pocs corresponds to delta_rps itself)
        if use_delta_flag[ref_num_delta_pocs] || used_by_curr_pic_flag[ref_num_delta_pocs] {
            let d_poc = delta_rps;
            if d_poc < 0 {
                rps.delta_poc_s0[neg_count as usize] = d_poc;
                rps.used_by_curr_pic_s0[neg_count as usize] =
                    used_by_curr_pic_flag[ref_num_delta_pocs];
                neg_count += 1;
            } else if d_poc > 0 {
                rps.delta_poc_s1[pos_count as usize] = d_poc;
                rps.used_by_curr_pic_s1[pos_count as usize] =
                    used_by_curr_pic_flag[ref_num_delta_pocs];
                pos_count += 1;
            }
        }

        // Process existing entries
        for j in 0..ref_count {
            if use_delta_flag[j] || used_by_curr_pic_flag[j] {
                let d_poc = ref_delta_pocs[j].wrapping_add(delta_rps);
                if d_poc < 0 {
                    if (neg_count as usize) < MAX_NUM_REF_PICS {
                        rps.delta_poc_s0[neg_count as usize] = d_poc;
                        rps.used_by_curr_pic_s0[neg_count as usize] = used_by_curr_pic_flag[j];
                        neg_count += 1;
                    }
                } else if d_poc > 0 && (pos_count as usize) < MAX_NUM_REF_PICS {
                    rps.delta_poc_s1[pos_count as usize] = d_poc;
                    rps.used_by_curr_pic_s1[pos_count as usize] = used_by_curr_pic_flag[j];
                    pos_count += 1;
                }
            }
        }

        rps.num_negative_pics = neg_count;
        rps.num_positive_pics = pos_count;

        // Sort S0 in decreasing order (most negative first)
        sort_delta_pocs_decreasing(
            &mut rps.delta_poc_s0,
            &mut rps.used_by_curr_pic_s0,
            neg_count as usize,
        );

        // Sort S1 in increasing order (smallest positive first)
        sort_delta_pocs_increasing(
            &mut rps.delta_poc_s1,
            &mut rps.used_by_curr_pic_s1,
            pos_count as usize,
        );
    } else {
        // Direct specification
        let num_negative_pics = reader.read_ue()? as u8;
        let num_positive_pics = reader.read_ue()? as u8;

        if num_negative_pics as usize > MAX_NUM_REF_PICS
            || num_positive_pics as usize > MAX_NUM_REF_PICS
        {
            return Err(HevcError::InvalidBitstream(
                "too many short-term reference pictures",
            ));
        }

        rps.num_negative_pics = num_negative_pics;
        rps.num_positive_pics = num_positive_pics;

        // Parse S0 (negative/past references)
        let mut prev_delta_poc = 0i32;
        for i in 0..num_negative_pics as usize {
            let delta_poc_s0_minus1 = reader.read_ue()? as i32;
            let used = reader.read_bit()? != 0;
            prev_delta_poc = prev_delta_poc.wrapping_sub(delta_poc_s0_minus1.wrapping_add(1));
            rps.delta_poc_s0[i] = prev_delta_poc;
            rps.used_by_curr_pic_s0[i] = used;
        }

        // Parse S1 (positive/future references)
        prev_delta_poc = 0;
        for i in 0..num_positive_pics as usize {
            let delta_poc_s1_minus1 = reader.read_ue()? as i32;
            let used = reader.read_bit()? != 0;
            prev_delta_poc = prev_delta_poc.wrapping_add(delta_poc_s1_minus1.wrapping_add(1));
            rps.delta_poc_s1[i] = prev_delta_poc;
            rps.used_by_curr_pic_s1[i] = used;
        }
    }

    Ok(rps)
}

/// Derive Picture Order Count for the current picture (H.265 8.3.1)
///
/// Returns the full 32-bit POC value.
pub fn derive_poc(
    slice_pic_order_cnt_lsb: u32,
    log2_max_poc_lsb: u8,
    prev_poc_lsb: u32,
    prev_poc_msb: i32,
    is_irap: bool,
    _no_rasl_output_flag: bool,
) -> (i32, u32, i32) {
    let max_poc_lsb = 1u32 << log2_max_poc_lsb;
    let poc_lsb = slice_pic_order_cnt_lsb;

    let poc_msb = if is_irap {
        // For IRAP pictures, POC MSB is 0
        0i32
    } else if poc_lsb < prev_poc_lsb && (prev_poc_lsb - poc_lsb) >= (max_poc_lsb / 2) {
        prev_poc_msb + max_poc_lsb as i32
    } else if poc_lsb > prev_poc_lsb && (poc_lsb - prev_poc_lsb) > (max_poc_lsb / 2) {
        prev_poc_msb - max_poc_lsb as i32
    } else {
        prev_poc_msb
    };

    let poc = poc_msb + poc_lsb as i32;
    (poc, poc_lsb, poc_msb)
}

/// Build reference picture lists L0 and L1 from the active RPS and DPB (H.265 8.3.4)
///
/// `curr_poc`: POC of the current picture
/// `rps`: the active short-term reference picture set
/// `dpb_slots`: (DPB slot index, POC) pairs for active reference pictures
/// `num_ref_idx_active`: \[L0, L1\] active reference counts from slice header
/// `ref_pic_list_modification`: optional reordering tables \[L0\]\[L1\]
/// `slice_type_is_b`: true if B-slice (L1 is constructed differently)
pub fn build_ref_pic_lists(
    curr_poc: i32,
    rps: &ShortTermRefPicSet,
    dpb_slots: &[(usize, i32)],
    num_ref_idx_active: [u8; 2],
    ref_pic_list_modification: Option<&[[u8; MAX_NUM_REF_PICS]; 2]>,
    modification_flag: [bool; 2],
    slice_type_is_b: bool,
) -> RefPicLists {
    let mut lists = RefPicLists {
        num_ref_idx_active,
        ..RefPicLists::default()
    };

    // Build initial RefPicListTemp for L0:
    // Order: StCurrBefore, StCurrAfter, LtCurr (interleaved/repeated as needed)
    let mut temp_l0 = [(-1i8, 0i32); MAX_NUM_REF_PICS];
    let mut temp_l0_count = 0usize;

    // Collect "StCurrBefore" — negative delta POC entries used by current pic
    for i in 0..rps.num_negative_pics as usize {
        if rps.used_by_curr_pic_s0[i] {
            let ref_poc = curr_poc + rps.delta_poc_s0[i];
            if let Some(dpb_idx) = find_dpb_by_poc(dpb_slots, ref_poc)
                && temp_l0_count < MAX_NUM_REF_PICS
            {
                temp_l0[temp_l0_count] = (dpb_idx as i8, ref_poc);
                temp_l0_count += 1;
            }
        }
    }
    let st_curr_before_count = temp_l0_count;

    // Collect "StCurrAfter" — positive delta POC entries used by current pic
    for i in 0..rps.num_positive_pics as usize {
        if rps.used_by_curr_pic_s1[i] {
            let ref_poc = curr_poc + rps.delta_poc_s1[i];
            if let Some(dpb_idx) = find_dpb_by_poc(dpb_slots, ref_poc)
                && temp_l0_count < MAX_NUM_REF_PICS
            {
                temp_l0[temp_l0_count] = (dpb_idx as i8, ref_poc);
                temp_l0_count += 1;
            }
        }
    }

    // Helper: apply a temp list to the output, with optional modification table
    let apply_list = |out_dpb: &mut [i8; MAX_NUM_REF_PICS],
                      out_poc: &mut [i32; MAX_NUM_REF_PICS],
                      temp: &[(i8, i32); MAX_NUM_REF_PICS],
                      temp_count: usize,
                      active: usize,
                      list_idx: usize,
                      mod_flag: bool| {
        if mod_flag {
            if let Some(mod_table) = ref_pic_list_modification {
                for (i, &mod_entry) in mod_table[list_idx]
                    .iter()
                    .enumerate()
                    .take(active.min(MAX_NUM_REF_PICS))
                {
                    let entry = mod_entry as usize;
                    if entry < temp_count {
                        out_dpb[i] = temp[entry].0;
                        out_poc[i] = temp[entry].1;
                    }
                }
            }
        } else if temp_count > 0 {
            for i in 0..active.min(MAX_NUM_REF_PICS) {
                let idx = i % temp_count;
                out_dpb[i] = temp[idx].0;
                out_poc[i] = temp[idx].1;
            }
        }
    };

    // Apply L0
    let l0_active = num_ref_idx_active[0] as usize;
    apply_list(
        &mut lists.dpb_index[0],
        &mut lists.poc[0],
        &temp_l0,
        temp_l0_count,
        l0_active,
        0,
        modification_flag[0],
    );

    // Build L1 for B-slices: StCurrAfter first, then StCurrBefore
    if slice_type_is_b {
        let mut temp_l1 = [(-1i8, 0i32); MAX_NUM_REF_PICS];
        let mut temp_l1_count = 0usize;

        // StCurrAfter first
        for i in 0..rps.num_positive_pics as usize {
            if rps.used_by_curr_pic_s1[i] {
                let ref_poc = curr_poc + rps.delta_poc_s1[i];
                if let Some(dpb_idx) = find_dpb_by_poc(dpb_slots, ref_poc)
                    && temp_l1_count < MAX_NUM_REF_PICS
                {
                    temp_l1[temp_l1_count] = (dpb_idx as i8, ref_poc);
                    temp_l1_count += 1;
                }
            }
        }

        // Then StCurrBefore
        for entry in temp_l0.iter().take(st_curr_before_count) {
            if temp_l1_count < MAX_NUM_REF_PICS {
                temp_l1[temp_l1_count] = *entry;
                temp_l1_count += 1;
            }
        }

        let l1_active = num_ref_idx_active[1] as usize;
        apply_list(
            &mut lists.dpb_index[1],
            &mut lists.poc[1],
            &temp_l1,
            temp_l1_count,
            l1_active,
            1,
            modification_flag[1],
        );
    }

    lists
}

/// Find a DPB entry by POC value. Returns the DPB slot index.
fn find_dpb_by_poc(dpb_slots: &[(usize, i32)], poc: i32) -> Option<usize> {
    dpb_slots
        .iter()
        .find(|(_, p)| *p == poc)
        .map(|(slot, _)| *slot)
}

/// Sort delta_poc and used_by_curr_pic arrays in decreasing order (for S0)
fn sort_delta_pocs_decreasing(
    delta_poc: &mut [i32; MAX_NUM_REF_PICS],
    used: &mut [bool; MAX_NUM_REF_PICS],
    count: usize,
) {
    // Insertion sort (small arrays, stable)
    for i in 1..count {
        let dp = delta_poc[i];
        let u = used[i];
        let mut j = i;
        while j > 0 && delta_poc[j - 1] < dp {
            delta_poc[j] = delta_poc[j - 1];
            used[j] = used[j - 1];
            j -= 1;
        }
        delta_poc[j] = dp;
        used[j] = u;
    }
}

/// Sort delta_poc and used_by_curr_pic arrays in increasing order (for S1)
fn sort_delta_pocs_increasing(
    delta_poc: &mut [i32; MAX_NUM_REF_PICS],
    used: &mut [bool; MAX_NUM_REF_PICS],
    count: usize,
) {
    for i in 1..count {
        let dp = delta_poc[i];
        let u = used[i];
        let mut j = i;
        while j > 0 && delta_poc[j - 1] > dp {
            delta_poc[j] = delta_poc[j - 1];
            used[j] = used[j - 1];
            j -= 1;
        }
        delta_poc[j] = dp;
        used[j] = u;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poc_derivation_irap() {
        let (poc, lsb, msb) = derive_poc(0, 8, 0, 0, true, false);
        assert_eq!(poc, 0);
        assert_eq!(lsb, 0);
        assert_eq!(msb, 0);
    }

    #[test]
    fn test_poc_derivation_increment() {
        let (poc, _, _) = derive_poc(1, 8, 0, 0, false, false);
        assert_eq!(poc, 1);
    }

    #[test]
    fn test_poc_derivation_wrap() {
        // POC LSB wraps around: prev_lsb=250, curr_lsb=5, max=256
        let (poc, _, _) = derive_poc(5, 8, 250, 0, false, false);
        assert_eq!(poc, 261); // msb becomes 256
    }

    #[test]
    fn test_short_term_rps_default() {
        let rps = ShortTermRefPicSet::default();
        assert_eq!(rps.num_delta_pocs(), 0);
    }
}
