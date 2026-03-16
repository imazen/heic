//! Inter prediction types and candidate derivation
//!
//! Core types for H.265 inter prediction: motion vectors, prediction unit motion,
//! merge/AMVP candidate lists, and motion compensation dispatch.

#![allow(dead_code)] // Phase 0: foundation types, used in subsequent phases

/// Maximum number of merge candidates (H.265 spec 8.5.3.2.2)
pub const MAX_NUM_MERGE_CAND: usize = 5;

/// Maximum number of reference pictures per list
pub const MAX_NUM_REF_PICS: usize = 16;

/// Motion vector in quarter-pel luma sample units
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MotionVector {
    /// Horizontal component (1/4 luma sample precision)
    pub x: i16,
    /// Vertical component (1/4 luma sample precision)
    pub y: i16,
}

impl MotionVector {
    /// Zero motion vector
    pub const ZERO: Self = Self { x: 0, y: 0 };
}

/// Inter prediction direction
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum InterPredIdc {
    /// Prediction from reference list L0 only
    L0 = 1,
    /// Prediction from reference list L1 only
    L1 = 2,
    /// Bi-directional prediction (L0 + L1)
    Bi = 3,
}

impl InterPredIdc {
    /// Create from raw value
    pub fn from_u8(val: u8) -> Option<Self> {
        match val {
            1 => Some(Self::L0),
            2 => Some(Self::L1),
            3 => Some(Self::Bi),
            _ => None,
        }
    }

    /// Whether this direction uses L0
    pub fn uses_l0(self) -> bool {
        matches!(self, Self::L0 | Self::Bi)
    }

    /// Whether this direction uses L1
    pub fn uses_l1(self) -> bool {
        matches!(self, Self::L1 | Self::Bi)
    }
}

/// Decoded motion information for a prediction unit (final result after merge/AMVP)
#[derive(Clone, Copy, Debug, Default)]
pub struct PbMotion {
    /// Prediction flags: which reference lists are used \[L0, L1\]
    pub pred_flag: [bool; 2],
    /// Reference picture index in each list \[L0, L1\] (-1 = unused)
    pub ref_idx: [i8; 2],
    /// Motion vectors \[L0, L1\]
    pub mv: [MotionVector; 2],
}

impl PbMotion {
    /// Unavailable motion (no prediction)
    pub const UNAVAILABLE: Self = Self {
        pred_flag: [false, false],
        ref_idx: [-1, -1],
        mv: [MotionVector::ZERO, MotionVector::ZERO],
    };
}

/// Coded motion syntax for a prediction unit (pre-derivation)
#[derive(Clone, Copy, Debug, Default)]
pub struct PbMotionCoding {
    /// Reference indices \[L0, L1\]
    pub ref_idx: [i8; 2],
    /// Motion vector difference \[L0/L1\]\[x/y\]
    pub mvd: [[i16; 2]; 2],
    /// Inter prediction direction
    pub inter_pred_idc: u8,
    /// MVP flag for L0
    pub mvp_l0_flag: bool,
    /// MVP flag for L1
    pub mvp_l1_flag: bool,
    /// Merge mode flag
    pub merge_flag: bool,
    /// Merge candidate index (0..4)
    pub merge_idx: u8,
}

/// Prediction weight table from slice header (H.265 7.3.6.3)
#[derive(Clone, Debug, Default)]
pub struct PredWeightTable {
    /// Log2 weight denominator for luma
    pub luma_log2_weight_denom: u8,
    /// Log2 weight denominator for chroma (derived)
    pub chroma_log2_weight_denom: u8,
    /// Luma weight flag \[L0/L1\]\[ref_idx\]
    pub luma_weight_flag: [[bool; MAX_NUM_REF_PICS]; 2],
    /// Chroma weight flag \[L0/L1\]\[ref_idx\]
    pub chroma_weight_flag: [[bool; MAX_NUM_REF_PICS]; 2],
    /// Luma weight values \[L0/L1\]\[ref_idx\]
    pub luma_weight: [[i16; MAX_NUM_REF_PICS]; 2],
    /// Luma offset values \[L0/L1\]\[ref_idx\]
    pub luma_offset: [[i16; MAX_NUM_REF_PICS]; 2],
    /// Chroma weight values \[L0/L1\]\[ref_idx\]\[Cb/Cr\]
    pub chroma_weight: [[[i16; 2]; MAX_NUM_REF_PICS]; 2],
    /// Chroma offset values \[L0/L1\]\[ref_idx\]\[Cb/Cr\]
    pub chroma_offset: [[[i16; 2]; MAX_NUM_REF_PICS]; 2],
}

/// Constructed reference picture lists for a slice
#[derive(Clone, Debug, Default)]
pub struct RefPicLists {
    /// Number of active reference pictures per list \[L0, L1\]
    pub num_ref_idx_active: [u8; 2],
    /// DPB indices for each list entry \[L0/L1\]\[entry\] (-1 = unused)
    pub dpb_index: [[i8; MAX_NUM_REF_PICS]; 2],
    /// POC values for each list entry \[L0/L1\]\[entry\]
    pub poc: [[i32; MAX_NUM_REF_PICS]; 2],
}
