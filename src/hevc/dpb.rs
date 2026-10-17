//! Decoded Picture Buffer (DPB) management (H.265 C.5)
//!
//! The DPB holds reference frames needed for inter prediction.
//! Each entry contains the decoded frame, its POC, and per-PU motion information
//! for temporal motion vector prediction.

#![allow(dead_code)] // Phase 0: types used in subsequent phases

use crate::error::HevcError;
use alloc::vec::Vec;
use whereat::at;

use super::inter::{MAX_NUM_REF_PICS, PbMotion};
use super::picture::DecodedFrame;
use super::slice::PredMode;

/// Maximum DPB size (H.265 A.4.1: max_dpb_size = MaxDpbSize from level limits)
pub const MAX_DPB_SIZE: usize = 16;

/// A single entry in the Decoded Picture Buffer
pub struct DpbEntry {
    /// The decoded frame (YCbCr planes)
    pub frame: DecodedFrame,
    /// Picture Order Count
    pub poc: i32,
    /// Whether this picture is a reference picture
    pub is_reference: bool,
    /// Whether this picture has been output
    pub is_output: bool,
    /// Motion vector information at min-PU granularity (for temporal MVP)
    /// Indexed as \[y_pu * mv_stride + x_pu\]
    pub mv_info: Vec<PbMotion>,
    /// Stride of mv_info in min-PU units
    pub mv_stride: u32,
    /// Prediction mode map at min-PU granularity (Intra/Inter/Skip)
    pub pred_mode_map: Vec<PredMode>,
    /// Reference picture list POCs from this frame's slice [L0/L1][ref_idx]
    /// Needed for temporal MVP: colPocDiff = ColPic.POC - ColPic.RefPicList[listCol][refIdxCol]
    pub ref_poc: [[i32; MAX_NUM_REF_PICS]; 2],
}

impl DpbEntry {
    /// Create a new DPB entry from a decoded frame
    pub fn new(frame: DecodedFrame, poc: i32, min_pu_size: u32) -> super::Result<Self> {
        let pu_width = frame.width.div_ceil(min_pu_size);
        let pu_height = frame.height.div_ceil(min_pu_size);
        let pu_count = (pu_width * pu_height) as usize;

        Ok(Self {
            frame,
            poc,
            is_reference: true,
            is_output: false,
            mv_info: try_vec![PbMotion::UNAVAILABLE; pu_count]
                .map_err(|_| at!(HevcError::AllocationFailed))?,
            mv_stride: pu_width,
            pred_mode_map: try_vec![PredMode::Intra; pu_count]
                .map_err(|_| at!(HevcError::AllocationFailed))?,
            ref_poc: [[0i32; MAX_NUM_REF_PICS]; 2],
        })
    }

    /// Get the motion info for a PU at luma position (x, y)
    pub fn get_mv_info(&self, x: u32, y: u32, min_pu_size: u32) -> PbMotion {
        let px = x / min_pu_size;
        let py = y / min_pu_size;
        let idx = (py * self.mv_stride + px) as usize;
        if idx < self.mv_info.len() {
            self.mv_info[idx]
        } else {
            PbMotion::UNAVAILABLE
        }
    }

    /// Get the prediction mode for a PU at luma position (x, y)
    pub fn get_pred_mode(&self, x: u32, y: u32, min_pu_size: u32) -> PredMode {
        let px = x / min_pu_size;
        let py = y / min_pu_size;
        let idx = (py * self.mv_stride + px) as usize;
        if idx < self.pred_mode_map.len() {
            self.pred_mode_map[idx]
        } else {
            PredMode::Intra
        }
    }
}

/// Decoded Picture Buffer
///
/// Manages a pool of reference frames. The current picture being decoded
/// lives *outside* the DPB (owned by the caller). After decoding and filtering,
/// it can be inserted into the DPB for use as a reference by future pictures.
pub struct Dpb {
    /// DPB slots (None = empty)
    entries: Vec<Option<DpbEntry>>,
    /// Maximum DPB size (from SPS level limits)
    max_size: usize,
}

impl Dpb {
    /// Create a new empty DPB
    pub fn new(max_size: usize) -> Self {
        let max_size = max_size.min(MAX_DPB_SIZE);
        let mut entries = Vec::with_capacity(max_size);
        entries.resize_with(max_size, || None);
        Self { entries, max_size }
    }

    /// Get a reference to a DPB entry by slot index
    pub fn get(&self, index: usize) -> Option<&DpbEntry> {
        self.entries.get(index)?.as_ref()
    }

    /// Get all active POC values (for reference list construction)
    pub fn active_pocs(&self) -> Vec<i32> {
        self.entries
            .iter()
            .filter_map(|e| e.as_ref().filter(|e| e.is_reference).map(|e| e.poc))
            .collect()
    }

    /// Get active (slot_index, poc) pairs for reference list construction.
    /// The slot index is the DPB entry position, used to look up reference frames.
    pub fn active_slots_and_pocs(&self) -> Vec<(usize, i32)> {
        self.entries
            .iter()
            .enumerate()
            .filter_map(|(i, e)| e.as_ref().filter(|e| e.is_reference).map(|e| (i, e.poc)))
            .collect()
    }

    /// Find a DPB entry by POC value
    pub fn find_by_poc(&self, poc: i32) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| e.poc == poc))
    }

    /// Evict all non-reference, already-output entries to free DPB slots.
    pub fn evict_unneeded(&mut self) {
        for slot in &mut self.entries {
            if let Some(e) = slot
                && !e.is_reference
                && e.is_output
            {
                *slot = None;
            }
        }
    }

    /// Insert a frame into the DPB. Returns the slot index.
    ///
    /// If the DPB is full, the oldest non-reference, already-output entry is evicted.
    pub fn insert(&mut self, entry: DpbEntry) -> Option<usize> {
        // Find an empty slot first
        if let Some(idx) = self.entries.iter().position(|e| e.is_none()) {
            self.entries[idx] = Some(entry);
            return Some(idx);
        }

        // Evict: prefer entries that are not reference and already output
        if let Some(idx) = self
            .entries
            .iter()
            .position(|e| e.as_ref().is_some_and(|e| !e.is_reference && e.is_output))
        {
            self.entries[idx] = Some(entry);
            return Some(idx);
        }

        None // DPB full, cannot insert
    }

    /// Mark pictures as non-reference based on the active RPS.
    ///
    /// Any picture whose POC is not in `ref_pocs` is marked as non-reference.
    pub fn mark_unused(&mut self, ref_pocs: &[i32]) {
        for entry in self.entries.iter_mut().flatten() {
            if !ref_pocs.contains(&entry.poc) {
                entry.is_reference = false;
            }
        }
    }

    /// Mark all entries as output and non-reference (for IRAP flush)
    pub fn flush(&mut self) {
        for entry in self.entries.iter_mut().flatten() {
            entry.is_reference = false;
            entry.is_output = true;
        }
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        for slot in &mut self.entries {
            *slot = None;
        }
    }

    /// Number of currently occupied slots
    pub fn count(&self) -> usize {
        self.entries.iter().filter(|e| e.is_some()).count()
    }

    /// Maximum number of slots
    pub fn capacity(&self) -> usize {
        self.max_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dpb_new_empty() {
        let dpb = Dpb::new(6);
        assert_eq!(dpb.count(), 0);
        assert_eq!(dpb.capacity(), 6);
        assert!(dpb.active_pocs().is_empty());
    }

    #[test]
    fn test_dpb_insert_and_find() {
        let mut dpb = Dpb::new(4);
        let frame = DecodedFrame::with_params(64, 64, 8, 1).unwrap();
        let entry = DpbEntry::new(frame, 42, 4).unwrap();
        let slot = dpb.insert(entry).unwrap();
        assert_eq!(dpb.count(), 1);
        assert_eq!(dpb.find_by_poc(42), Some(slot));
        assert!(dpb.find_by_poc(99).is_none());
    }

    #[test]
    fn test_dpb_active_pocs() {
        let mut dpb = Dpb::new(4);
        for poc in [0, 1, 2] {
            let frame = DecodedFrame::with_params(64, 64, 8, 1).unwrap();
            dpb.insert(DpbEntry::new(frame, poc, 4).unwrap());
        }
        let mut pocs = dpb.active_pocs();
        pocs.sort();
        assert_eq!(pocs, vec![0, 1, 2]);
    }

    #[test]
    fn test_dpb_mark_unused() {
        let mut dpb = Dpb::new(4);
        for poc in [0, 1, 2] {
            let frame = DecodedFrame::with_params(64, 64, 8, 1).unwrap();
            dpb.insert(DpbEntry::new(frame, poc, 4).unwrap());
        }
        dpb.mark_unused(&[0, 2]); // POC 1 is no longer referenced
        let pocs = dpb.active_pocs();
        assert!(pocs.contains(&0));
        assert!(!pocs.contains(&1));
        assert!(pocs.contains(&2));
    }
}
