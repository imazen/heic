//! Sample Adaptive Offset filter (H.265 Section 8.7.3)
//!
//! Applied after deblocking to reduce banding and ringing artifacts.
//! Two modes per CTB: Band Offset (BO) and Edge Offset (EO).

use alloc::vec;
use alloc::vec::Vec;

use super::picture::DecodedFrame;

/// SAO parameters for one CTB
#[derive(Clone, Copy, Debug, Default)]
pub struct SaoInfo {
    /// SAO type per component: 0=off, 1=band offset, 2=edge offset
    /// [0]=Y, [1]=Cb, [2]=Cr
    pub sao_type_idx: [u8; 3],
    /// Edge offset class per component (0-3, only used when type==2)
    /// 0=horizontal, 1=vertical, 2=diagonal 135°, 3=diagonal 45°
    pub sao_eo_class: [u8; 3],
    /// Band position per component (0-31, only used when type==1)
    pub sao_band_position: [u8; 3],
    /// Signed offset values per component, 4 values each
    /// For band offset: offsets for 4 consecutive bands starting at band_position
    /// For edge offset: offsets[0]=cat1(+), [1]=cat2(+), [2]=cat3(-), [3]=cat4(-)
    pub sao_offset_val: [[i8; 4]; 3],
}

/// SAO map for the entire frame, stored at CTB granularity
pub struct SaoMap {
    pub data: Vec<SaoInfo>,
    pub width_ctbs: u32,
    pub height_ctbs: u32,
}

impl SaoMap {
    pub fn new(width_ctbs: u32, height_ctbs: u32) -> Self {
        Self {
            data: vec![SaoInfo::default(); (width_ctbs * height_ctbs) as usize],
            width_ctbs,
            height_ctbs,
        }
    }

    #[inline]
    pub fn get(&self, ctb_x: u32, ctb_y: u32) -> &SaoInfo {
        &self.data[(ctb_y * self.width_ctbs + ctb_x) as usize]
    }

    #[inline]
    pub fn get_mut(&mut self, ctb_x: u32, ctb_y: u32) -> &mut SaoInfo {
        &mut self.data[(ctb_y * self.width_ctbs + ctb_x) as usize]
    }
}

/// Edge offset direction lookup: (dx0, dy0, dx1, dy1) for each eo_class
/// eo_class 0: horizontal (left, right)
/// eo_class 1: vertical (above, below)
/// eo_class 2: diagonal 135° (upper-left, lower-right)
/// eo_class 3: diagonal 45° (upper-right, lower-left)
const EO_OFFSETS: [(i32, i32, i32, i32); 4] = [
    (-1, 0, 1, 0),  // class 0: horizontal
    (0, -1, 0, 1),  // class 1: vertical
    (-1, -1, 1, 1), // class 2: 135° diagonal
    (1, -1, -1, 1), // class 3: 45° diagonal
];

/// Apply SAO filter to the entire frame
pub fn apply_sao(
    frame: &mut DecodedFrame,
    sao_map: &SaoMap,
    ctb_size: u32,
) {
    let width = frame.width;
    let height = frame.height;
    let bit_depth = frame.bit_depth;

    // SAO edge offset reads neighbor pixels that may also be modified.
    // To avoid data dependency, clone the planes and read from clones.
    let orig_y = frame.y_plane.clone();
    let orig_cb = frame.cb_plane.clone();
    let orig_cr = frame.cr_plane.clone();

    let y_stride = frame.y_stride();
    let c_stride = frame.c_stride();

    let (sub_x, sub_y) = match frame.chroma_format {
        1 => (2u32, 2u32),
        2 => (2, 1),
        3 => (1, 1),
        _ => (1, 1),
    };

    // Process each CTB
    for ctb_y in 0..sao_map.height_ctbs {
        for ctb_x in 0..sao_map.width_ctbs {
            let sao = sao_map.get(ctb_x, ctb_y);
            let ctb_x_px = ctb_x * ctb_size;
            let ctb_y_px = ctb_y * ctb_size;

            // Luma
            if sao.sao_type_idx[0] != 0 {
                let x_end = (ctb_x_px + ctb_size).min(width);
                let y_end = (ctb_y_px + ctb_size).min(height);
                apply_sao_component(
                    &orig_y,
                    &mut frame.y_plane,
                    y_stride as u32,
                    width,
                    height,
                    ctb_x_px,
                    ctb_y_px,
                    x_end,
                    y_end,
                    sao.sao_type_idx[0],
                    sao.sao_eo_class[0],
                    sao.sao_band_position[0],
                    &sao.sao_offset_val[0],
                    bit_depth,
                );
            }

            // Chroma (4:2:0: halved coordinates)
            if frame.chroma_format > 0 {
                let cx_start = ctb_x_px / sub_x;
                let cy_start = ctb_y_px / sub_y;
                let cx_end = ((ctb_x_px + ctb_size) / sub_x).min(width / sub_x);
                let cy_end = ((ctb_y_px + ctb_size) / sub_y).min(height / sub_y);
                let c_w = width / sub_x;
                let c_h = height / sub_y;

                // Cb
                if sao.sao_type_idx[1] != 0 {
                    apply_sao_component(
                        &orig_cb,
                        &mut frame.cb_plane,
                        c_stride as u32,
                        c_w,
                        c_h,
                        cx_start,
                        cy_start,
                        cx_end,
                        cy_end,
                        sao.sao_type_idx[1],
                        sao.sao_eo_class[1],
                        sao.sao_band_position[1],
                        &sao.sao_offset_val[1],
                        bit_depth,
                    );
                }

                // Cr
                if sao.sao_type_idx[2] != 0 {
                    apply_sao_component(
                        &orig_cr,
                        &mut frame.cr_plane,
                        c_stride as u32,
                        c_w,
                        c_h,
                        cx_start,
                        cy_start,
                        cx_end,
                        cy_end,
                        sao.sao_type_idx[2],
                        sao.sao_eo_class[2],
                        sao.sao_band_position[2],
                        &sao.sao_offset_val[2],
                        bit_depth,
                    );
                }
            }
        }
    }
}

/// Apply SAO to one component in a rectangular CTB region
fn apply_sao_component(
    src: &[u16],
    dst: &mut [u16],
    stride: u32,
    plane_w: u32,
    plane_h: u32,
    x_start: u32,
    y_start: u32,
    x_end: u32,
    y_end: u32,
    sao_type_idx: u8,
    eo_class: u8,
    band_position: u8,
    offsets: &[i8; 4],
    bit_depth: u8,
) {
    let max_val = (1i32 << bit_depth) - 1;

    match sao_type_idx {
        1 => {
            // Band offset
            let band_shift = bit_depth - 5;

            // Build lookup table for the 32 bands
            let mut band_table = [0i8; 32];
            for k in 0..4u8 {
                let band_idx = (band_position + k) & 31;
                band_table[band_idx as usize] = offsets[k as usize];
            }

            for y in y_start..y_end {
                let row = (y * stride) as usize;
                for x in x_start..x_end {
                    let idx = row + x as usize;
                    let sample = (src[idx] as i32).min(max_val);
                    let band = (sample >> band_shift) as usize;
                    let offset = band_table[band] as i32;
                    if offset != 0 {
                        dst[idx] = (sample + offset).clamp(0, max_val) as u16;
                    }
                }
            }
        }
        2 => {
            // Edge offset
            let (dx0, dy0, dx1, dy1) = EO_OFFSETS[eo_class as usize & 3];

            // Raw edgeIdx = 2 + sign(c-n0) + sign(c-n1), giving 0-4:
            //   0: valley (c < both neighbors) → +offset[0]
            //   1: c < one neighbor → +offset[1]
            //   2: center (no offset)
            //   3: c > one neighbor → -offset[2]
            //   4: peak (c > both neighbors) → -offset[3]
            let offset_table: [i32; 5] = [
                offsets[0] as i32,
                offsets[1] as i32,
                0,
                -(offsets[2] as i32),
                -(offsets[3] as i32),
            ];

            for y in y_start..y_end {
                let row = (y * stride) as usize;
                for x in x_start..x_end {
                    let nx0 = x as i32 + dx0;
                    let ny0 = y as i32 + dy0;
                    let nx1 = x as i32 + dx1;
                    let ny1 = y as i32 + dy1;

                    // Skip if neighbors are outside frame
                    if nx0 < 0
                        || nx0 >= plane_w as i32
                        || ny0 < 0
                        || ny0 >= plane_h as i32
                        || nx1 < 0
                        || nx1 >= plane_w as i32
                        || ny1 < 0
                        || ny1 >= plane_h as i32
                    {
                        continue;
                    }

                    let idx = row + x as usize;
                    let sample = src[idx] as i32;
                    let n0 = src[(ny0 as u32 * stride + nx0 as u32) as usize] as i32;
                    let n1 = src[(ny1 as u32 * stride + nx1 as u32) as usize] as i32;

                    // Classify edge: edgeIdx = 2 + sign(c - n0) + sign(c - n1)
                    let sign0 = (sample - n0).signum();
                    let sign1 = (sample - n1).signum();
                    let edge_idx = (2 + sign0 + sign1) as usize;
                    // edge_idx is in [0..4]

                    let offset = offset_table[edge_idx];
                    if offset != 0 {
                        dst[idx] = (sample + offset).clamp(0, max_val) as u16;
                    }
                }
            }
        }
        _ => {}
    }
}
