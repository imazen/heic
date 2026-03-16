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

// ── Merge candidate list derivation (H.265 8.5.3.2.2) ──────────────────

/// Context needed to derive motion vector candidates from the current picture.
/// This borrows the per-PU motion/pred-mode maps from SliceContext.
pub struct MvContext<'a> {
    /// Per-PU motion info (indexed by min_pu grid)
    pub mv_info: &'a [PbMotion],
    /// Per-PU prediction mode (indexed by min_pu grid)
    pub pred_mode: &'a [super::slice::PredMode],
    /// Stride of the PU maps (width / min_pu_size)
    pub pu_stride: u32,
    /// Minimum PU size in luma samples
    pub min_pu_size: u32,
    /// Picture width in luma samples
    pub pic_width: u32,
    /// Picture height in luma samples
    pub pic_height: u32,
    /// Current picture POC
    pub curr_poc: i32,
    /// Reference picture list POCs
    pub ref_pic_lists: &'a RefPicLists,
    /// Is this a B-slice?
    pub is_b_slice: bool,
    /// log2_parallel_merge_level
    pub log2_parallel_merge_level: u8,
}

impl MvContext<'_> {
    /// Get motion info at a luma sample position, or UNAVAILABLE if out of bounds
    fn get_motion(&self, x: i32, y: i32) -> PbMotion {
        if x < 0 || y < 0 || x as u32 >= self.pic_width || y as u32 >= self.pic_height {
            return PbMotion::UNAVAILABLE;
        }
        let px = x as u32 / self.min_pu_size;
        let py = y as u32 / self.min_pu_size;
        let idx = (py * self.pu_stride + px) as usize;
        if idx < self.mv_info.len() {
            self.mv_info[idx]
        } else {
            PbMotion::UNAVAILABLE
        }
    }

    /// Check if the position is inter-predicted (not intra/unavailable)
    fn is_inter(&self, x: i32, y: i32) -> bool {
        if x < 0 || y < 0 || x as u32 >= self.pic_width || y as u32 >= self.pic_height {
            return false;
        }
        let px = x as u32 / self.min_pu_size;
        let py = y as u32 / self.min_pu_size;
        let idx = (py * self.pu_stride + px) as usize;
        if idx < self.pred_mode.len() {
            matches!(
                self.pred_mode[idx],
                super::slice::PredMode::Inter | super::slice::PredMode::Skip
            )
        } else {
            false
        }
    }

    /// Check if two positions are in the same parallel merge region
    fn same_merge_region(&self, x0: u32, y0: u32, x1: i32, y1: i32) -> bool {
        if x1 < 0 || y1 < 0 {
            return false;
        }
        let shift = self.log2_parallel_merge_level;
        (x0 >> shift) == (x1 as u32 >> shift) && (y0 >> shift) == (y1 as u32 >> shift)
    }
}

/// Merge PU location parameters
pub struct MergePuParams {
    /// PU position x
    pub xp: u32,
    /// PU position y
    pub yp: u32,
    /// PU width
    pub w: u32,
    /// PU height
    pub h: u32,
    /// Partition index within CU
    pub part_idx: u8,
    /// Partition mode of the CU
    pub part_mode: super::slice::PartMode,
    /// Maximum merge candidates (from slice header)
    pub max_num_merge_cand: u8,
}

/// Derive merge candidate list (H.265 8.5.3.2.2)
///
/// Returns up to `max_num_merge_cand` candidates.
#[allow(clippy::too_many_lines)]
pub fn derive_merge_candidates(
    ctx: &MvContext<'_>,
    pu: &MergePuParams,
) -> [PbMotion; MAX_NUM_MERGE_CAND] {
    let mut cand = [PbMotion::UNAVAILABLE; MAX_NUM_MERGE_CAND];
    let mut count = 0usize;
    let max = pu.max_num_merge_cand as usize;
    let (xp, yp, w, h) = (pu.xp, pu.yp, pu.w, pu.h);

    // Spatial candidates: A1, B1, B0, A0, B2
    let a1_pos = (xp as i32 - 1, yp as i32 + h as i32 - 1);
    let b1_pos = (xp as i32 + w as i32 - 1, yp as i32 - 1);
    let b0_pos = (xp as i32 + w as i32, yp as i32 - 1);
    let a0_pos = (xp as i32 - 1, yp as i32 + h as i32);
    let b2_pos = (xp as i32 - 1, yp as i32 - 1);

    // A1: left-bottom
    let a1_avail = ctx.is_inter(a1_pos.0, a1_pos.1)
        && !ctx.same_merge_region(xp, yp, a1_pos.0, a1_pos.1)
        && !is_second_pu_vertical(pu.part_idx, pu.part_mode);
    if a1_avail && count < max {
        cand[count] = ctx.get_motion(a1_pos.0, a1_pos.1);
        count += 1;
    }

    // B1: above-right
    let b1_avail = ctx.is_inter(b1_pos.0, b1_pos.1)
        && !ctx.same_merge_region(xp, yp, b1_pos.0, b1_pos.1)
        && !is_second_pu_horizontal(pu.part_idx, pu.part_mode);
    if b1_avail && count < max {
        let b1_motion = ctx.get_motion(b1_pos.0, b1_pos.1);
        if !a1_avail || !motion_eq(&cand[0], &b1_motion) {
            cand[count] = b1_motion;
            count += 1;
        }
    }

    // B0: above-right corner
    let b0_avail = ctx.is_inter(b0_pos.0, b0_pos.1)
        && !ctx.same_merge_region(xp, yp, b0_pos.0, b0_pos.1);
    if b0_avail && count < max {
        let b0_motion = ctx.get_motion(b0_pos.0, b0_pos.1);
        if !b1_avail || !motion_eq(&cand[count - 1], &b0_motion) {
            cand[count] = b0_motion;
            count += 1;
        }
    }

    // A0: left-bottom corner
    let a0_avail = ctx.is_inter(a0_pos.0, a0_pos.1)
        && !ctx.same_merge_region(xp, yp, a0_pos.0, a0_pos.1);
    if a0_avail && count < max {
        let a0_motion = ctx.get_motion(a0_pos.0, a0_pos.1);
        let a1_idx = if a1_avail { Some(0) } else { None };
        let dup = a1_idx.is_some_and(|i| motion_eq(&cand[i], &a0_motion));
        if !dup {
            cand[count] = a0_motion;
            count += 1;
        }
    }

    // B2: above-left corner (only if < 4 candidates so far)
    if count < 4 && count < max {
        let b2_avail = ctx.is_inter(b2_pos.0, b2_pos.1)
            && !ctx.same_merge_region(xp, yp, b2_pos.0, b2_pos.1);
        if b2_avail {
            let b2_motion = ctx.get_motion(b2_pos.0, b2_pos.1);
            let dup = (a1_avail && motion_eq(&cand[0], &b2_motion))
                || (b1_avail && count > 1 && motion_eq(&cand[1], &b2_motion));
            if !dup {
                cand[count] = b2_motion;
                count += 1;
            }
        }
    }

    // TODO Phase 4b: temporal MVP candidate would go here
    // For now, skip temporal (requires DPB access with collocated frame)

    // Combined bipredictive candidates (B-slices only, if count > 1 and < max)
    if ctx.is_b_slice && count > 1 && count < max {
        derive_combined_bipred_inplace(&mut cand, count, max, &mut count);
    }

    // Zero motion vector padding
    while count < max {
        let ref_idx = count.min(
            ctx.ref_pic_lists.num_ref_idx_active[0]
                .max(if ctx.is_b_slice {
                    ctx.ref_pic_lists.num_ref_idx_active[1]
                } else {
                    0
                })
                .saturating_sub(1) as usize,
        ) as i8;
        cand[count] = PbMotion {
            pred_flag: [true, ctx.is_b_slice],
            ref_idx: [
                ref_idx.min(ctx.ref_pic_lists.num_ref_idx_active[0] as i8 - 1),
                if ctx.is_b_slice {
                    ref_idx.min(ctx.ref_pic_lists.num_ref_idx_active[1] as i8 - 1)
                } else {
                    -1
                },
            ],
            mv: [MotionVector::ZERO, MotionVector::ZERO],
        };
        count += 1;
    }

    cand
}

/// Derive AMVP candidates (H.265 8.5.3.2.6)
///
/// Returns exactly 2 MVP candidates for the given reference list.
pub fn derive_amvp_candidates(
    ctx: &MvContext<'_>,
    xp: u32,
    yp: u32,
    w: u32,
    h: u32,
    ref_idx: i8,
    list_idx: u8,
) -> [MotionVector; 2] {
    let mut mvp = [MotionVector::ZERO; 2];
    let mut mvp_count = 0usize;

    let target_poc = if (list_idx as usize) < 2
        && (ref_idx as usize) < ctx.ref_pic_lists.num_ref_idx_active[list_idx as usize] as usize
    {
        ctx.ref_pic_lists.poc[list_idx as usize][ref_idx as usize]
    } else {
        ctx.curr_poc
    };

    // Candidate A: left neighbors (A0, A1)
    let a0 = (xp as i32 - 1, yp as i32 + h as i32);
    let a1 = (xp as i32 - 1, yp as i32 + h as i32 - 1);
    let mut is_scaled_flag = false;

    // Try same-POC match first
    for &(ax, ay) in &[a0, a1] {
        if ctx.is_inter(ax, ay) {
            let m = ctx.get_motion(ax, ay);
            if let Some(mv) = extract_mv_for_ref(&m, list_idx, target_poc, ctx) {
                mvp[0] = mv;
                mvp_count = 1;
                is_scaled_flag = true;
                break;
            }
        }
    }
    // Try scaled match (different POC)
    if mvp_count == 0 {
        for &(ax, ay) in &[a0, a1] {
            if ctx.is_inter(ax, ay) {
                let m = ctx.get_motion(ax, ay);
                if let Some(mv) = extract_mv_scaled(&m, target_poc, ctx) {
                    mvp[0] = mv;
                    mvp_count = 1;
                    is_scaled_flag = true;
                    break;
                }
            }
        }
    }

    // Candidate B: above neighbors (B0, B1, B2)
    let b0 = (xp as i32 + w as i32, yp as i32 - 1);
    let b1 = (xp as i32 + w as i32 - 1, yp as i32 - 1);
    let b2 = (xp as i32 - 1, yp as i32 - 1);

    if !is_scaled_flag {
        // No A candidate: try same-POC B
        for &(bx, by) in &[b0, b1, b2] {
            if ctx.is_inter(bx, by) {
                let m = ctx.get_motion(bx, by);
                if let Some(mv) = extract_mv_for_ref(&m, list_idx, target_poc, ctx) {
                    if mvp_count == 0 {
                        mvp[0] = mv;
                        mvp_count = 1;
                    } else if mv != mvp[0] {
                        mvp[1] = mv;
                        mvp_count = 2;
                    }
                    break;
                }
            }
        }
    }

    // B with scaling (if A was found, B must be different and scaled)
    if mvp_count < 2 {
        for &(bx, by) in &[b0, b1, b2] {
            if ctx.is_inter(bx, by) {
                let m = ctx.get_motion(bx, by);
                let mv_opt = if is_scaled_flag {
                    extract_mv_scaled(&m, target_poc, ctx)
                } else {
                    extract_mv_for_ref(&m, list_idx, target_poc, ctx)
                        .or_else(|| extract_mv_scaled(&m, target_poc, ctx))
                };
                if let Some(mv) = mv_opt
                    && (mvp_count == 0 || mv != mvp[0])
                {
                    mvp[mvp_count] = mv;
                    mvp_count += 1;
                    if mvp_count >= 2 {
                        break;
                    }
                }
            }
        }
    }

    // TODO: temporal MVP candidate would go here (Phase 4b)

    // Pad with zero MVs
    // (mvp is already initialized to ZERO, so nothing to do)

    mvp
}

/// Scale a motion vector by POC distance ratio (H.265 8.5.3.2.8)
pub fn scale_mv(mv: MotionVector, dist_src: i32, dist_dst: i32) -> MotionVector {
    if dist_src == 0 || dist_src == dist_dst {
        return mv;
    }
    let td = dist_src.clamp(-128, 127);
    let tb = dist_dst.clamp(-128, 127);
    let tx = (16384 + (td.abs() / 2)) / td;
    let scale = (tb * tx + 32) >> 6;
    let scale = scale.clamp(-4096, 4095);
    MotionVector {
        x: ((scale as i64 * mv.x as i64 + 127 + (if scale * (mv.x as i32) < 0 { 1 } else { 0 }))
            >> 8)
            .clamp(-32768, 32767) as i16,
        y: ((scale as i64 * mv.y as i64 + 127 + (if scale * (mv.y as i32) < 0 { 1 } else { 0 }))
            >> 8)
            .clamp(-32768, 32767) as i16,
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

/// Check if two PbMotion entries are identical (same pred flags, ref indices, MVs)
fn motion_eq(a: &PbMotion, b: &PbMotion) -> bool {
    a.pred_flag == b.pred_flag
        && a.ref_idx == b.ref_idx
        && a.mv[0] == b.mv[0]
        && a.mv[1] == b.mv[1]
}

/// Extract MV from a neighbor for AMVP: same list, same POC (unscaled)
fn extract_mv_for_ref(
    m: &PbMotion,
    list_idx: u8,
    target_poc: i32,
    ctx: &MvContext<'_>,
) -> Option<MotionVector> {
    // Check same list first
    let li = list_idx as usize;
    if m.pred_flag[li] && m.ref_idx[li] >= 0 {
        let ref_poc = ctx.ref_pic_lists.poc[li][m.ref_idx[li] as usize];
        if ref_poc == target_poc {
            return Some(m.mv[li]);
        }
    }
    // Check opposite list
    let oi = 1 - li;
    if m.pred_flag[oi] && m.ref_idx[oi] >= 0 {
        let ref_poc = ctx.ref_pic_lists.poc[oi][m.ref_idx[oi] as usize];
        if ref_poc == target_poc {
            return Some(m.mv[oi]);
        }
    }
    None
}

/// Extract MV from a neighbor with POC scaling
fn extract_mv_scaled(m: &PbMotion, target_poc: i32, ctx: &MvContext<'_>) -> Option<MotionVector> {
    for li in 0..2 {
        if m.pred_flag[li] && m.ref_idx[li] >= 0 {
            let ref_poc = ctx.ref_pic_lists.poc[li][m.ref_idx[li] as usize];
            let dist_src = ctx.curr_poc - ref_poc;
            let dist_dst = ctx.curr_poc - target_poc;
            if dist_src != 0 {
                return Some(scale_mv(m.mv[li], dist_src, dist_dst));
            }
        }
    }
    None
}

/// Check if this is the second PU of a vertical split (for A1 discard)
fn is_second_pu_vertical(part_idx: u8, part_mode: super::slice::PartMode) -> bool {
    part_idx == 1
        && matches!(
            part_mode,
            super::slice::PartMode::PartNx2N
                | super::slice::PartMode::PartnLx2N
                | super::slice::PartMode::PartnRx2N
        )
}

/// Check if this is the second PU of a horizontal split (for B1 discard)
fn is_second_pu_horizontal(part_idx: u8, part_mode: super::slice::PartMode) -> bool {
    part_idx == 1
        && matches!(
            part_mode,
            super::slice::PartMode::Part2NxN
                | super::slice::PartMode::Part2NxnU
                | super::slice::PartMode::Part2NxnD
        )
}

/// Combined bipredictive merge candidates (H.265 8.5.3.2.4, Table 8-19)
/// Works in-place: `cand[0..orig_count]` are the spatial candidates,
/// new combined candidates are appended starting at `*count`.
fn derive_combined_bipred_inplace(
    cand: &mut [PbMotion; MAX_NUM_MERGE_CAND],
    orig_count: usize,
    max: usize,
    count: &mut usize,
) {
    // Combination table (l0_idx, l1_idx) from H.265 Table 8-19
    const COMB: [(usize, usize); 12] = [
        (0, 1),
        (1, 0),
        (0, 2),
        (2, 0),
        (1, 2),
        (2, 1),
        (0, 3),
        (3, 0),
        (1, 3),
        (3, 1),
        (2, 3),
        (3, 2),
    ];

    let max_comb = orig_count * (orig_count - 1);
    for &(l0i, l1i) in &COMB {
        if *count >= max {
            break;
        }
        if l0i >= orig_count || l1i >= orig_count {
            continue;
        }
        // l0Cand must have L0, l1Cand must have L1
        if !cand[l0i].pred_flag[0] || !cand[l1i].pred_flag[1] {
            continue;
        }
        // Different POCs or different MVs
        let same = cand[l0i].ref_idx[0] == cand[l1i].ref_idx[1]
            && cand[l0i].mv[0] == cand[l1i].mv[1];
        if same {
            continue;
        }
        cand[*count] = PbMotion {
            pred_flag: [true, true],
            ref_idx: [cand[l0i].ref_idx[0], cand[l1i].ref_idx[1]],
            mv: [cand[l0i].mv[0], cand[l1i].mv[1]],
        };
        *count += 1;
        if *count >= max || (l0i * orig_count + l1i) >= max_comb {
            break;
        }
    }
}
