//! NV12 / P010 → planar `u16` unpack with SPS conformance-window crop.
//!
//! Split out of `imp.rs` so the buffer-handling logic (IMF2DBuffer locking,
//! negative-stride rebase, aligned-height inference from
//! `GetContiguousLength`) sits next to the per-format byte → u16 loops.
//! Everything here runs after `decode.rs` has dequeued a sample, and the
//! result feeds `DecodedFrame.{y_plane,cb_plane,cr_plane}` directly.
//!
//! NV12 layout: Y plane of `aligned_height × stride` bytes, then
//! interleaved UV plane of `aligned_height/2 × stride` bytes.
//! P010 layout: same shape but each sample is u16 LE with MSB-aligned
//! 10-bit values (low 6 bits zero) — shifted right by 6 to normalize.
//!
//! The output planes are sized for the VISIBLE region, copied starting
//! at coded coordinates `(crop_x, crop_y)`. Chroma offsets are halved
//! because the SPS conf_win is in chroma-subsampling units.

#![cfg(target_os = "windows")]

use std::vec;
use std::vec::Vec;

use heic_core::BackendError;
use windows::Win32::Media::MediaFoundation::{IMF2DBuffer, IMFMediaBuffer, IMFSample};
use windows::core::Interface;

use crate::imp::decode_err;

pub(super) struct OutputPlanes {
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn read_output_planes(
    sample: &IMFSample,
    coded_w: u32,
    coded_h: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    // SAFETY: ConvertToContiguousBuffer returns one IMFMediaBuffer covering
    // all the sample's buffers, allocating if needed.
    let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(decode_err("IMFSample::ConvertToContiguousBuffer"))?;

    if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
        return read_planes_2d(
            &buf2d, coded_w, coded_h, visible_w, visible_h, crop_x, crop_y, bit_depth,
        );
    }

    // Fallback: locked linear access; assume stride == coded_w. Modern MFTs
    // implement IMF2DBuffer; the linear path is for legacy software MFTs.
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut max_len: u32 = 0;
    let mut cur_len: u32 = 0;
    // SAFETY: Lock returns a readable pointer to up to max_len bytes.
    unsafe { buffer.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len)) }
        .map_err(decode_err("IMFMediaBuffer::Lock(out)"))?;
    let stride = coded_w as usize;
    // SAFETY: the lock guarantees the pointer is valid for `cur_len` bytes.
    let planes = unsafe {
        unpack_nv12_or_p010(
            ptr, stride, coded_h, visible_w, visible_h, crop_x, crop_y, bit_depth,
        )
    };
    // SAFETY: pairs with the Lock above.
    unsafe { buffer.Unlock() }.map_err(decode_err("IMFMediaBuffer::Unlock(out)"))?;
    Ok(planes)
}

#[allow(clippy::too_many_arguments)]
fn read_planes_2d(
    buf2d: &IMF2DBuffer,
    coded_w: u32,
    coded_h: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut stride: i32 = 0;
    // SAFETY: Lock2D returns the row-stride and a pointer to the top-left
    // pixel of the buffer per the MF docs.
    unsafe { buf2d.Lock2D(&mut ptr, &mut stride) }.map_err(decode_err("IMF2DBuffer::Lock2D"))?;

    let stride_abs = stride.unsigned_abs() as usize;

    // SAFETY: GetContiguousLength on the locked IMF2DBuffer is a
    // documented inspection call.
    let total_bytes = unsafe { buf2d.GetContiguousLength() }
        .map_err(decode_err("IMF2DBuffer::GetContiguousLength"))? as usize;
    // NV12 / P010 layout: Y = N rows + UV = N/2 rows, total 3N/2 rows.
    // total_bytes / stride_abs ≈ 3N/2, so aligned_height ≈ (total*2/3) / stride.
    let aligned_height = (total_bytes * 2 / 3)
        .checked_div(stride_abs)
        .unwrap_or(0)
        .max(coded_h as usize);

    // Negative-stride buffers (bottom-up): Lock2D's pointer points at the
    // visual top-left even when the underlying memory grows downward.
    // Rebase to the physical first byte and treat stride as positive so
    // unpack sees a normal layout.
    let positive_base = if stride < 0 {
        // SAFETY: ptr is the Lock2D pointer; we walk back within the
        // buffer the lock owns.
        unsafe { ptr.offset(-(stride_abs as isize * (aligned_height as isize - 1))) }
    } else {
        ptr
    };

    // SAFETY: Lock2D + GetContiguousLength guarantee the pointer is valid
    // for total_bytes bytes; unpack reads strictly within
    // [positive_base, positive_base + total_bytes).
    let planes = unsafe {
        unpack_nv12_or_p010(
            positive_base,
            stride_abs,
            aligned_height as u32,
            visible_w,
            visible_h,
            crop_x,
            crop_y,
            bit_depth,
        )
    };
    // SAFETY: pairs with Lock2D.
    unsafe { buf2d.Unlock2D() }.map_err(decode_err("IMF2DBuffer::Unlock2D"))?;
    let _ = coded_w; // stride from Lock2D supersedes the coded_w hint
    Ok(planes)
}

/// SAFETY: caller guarantees `base` points at the start of an NV12 (8-bit) or
/// P010 (16-bit) frame whose Y plane is `aligned_height` rows tall at
/// `row_stride` bytes per row, followed immediately by the interleaved UV
/// plane at `aligned_height/2` rows at `row_stride` bytes per row. The
/// output buffers are sized for the VISIBLE region (`visible_w` ×
/// `visible_h`), copied starting at coded coordinates (`crop_x`,
/// `crop_y`) — the SPS conformance-window crop applied at copy time so
/// callers see exactly the ispe-visible region.
#[allow(clippy::too_many_arguments)]
unsafe fn unpack_nv12_or_p010(
    base: *const u8,
    row_stride: usize,
    aligned_height: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> OutputPlanes {
    let w = visible_w as usize;
    let h = visible_h as usize;
    let h_aligned = aligned_height as usize;
    let cx = crop_x as usize;
    let cy = crop_y as usize;
    let half_h = h / 2;
    let half_w = w / 2;
    // Chroma crop in 4:2:0: every-other luma sample → halve the offsets.
    let chroma_cx = cx / 2;
    let chroma_cy = cy / 2;

    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    if bit_depth <= 8 {
        // 8-bit NV12: visible Y rows are at (crop_y + y) in the coded
        // buffer; each row's visible content starts at byte offset
        // (crop_x) within the row. Store samples as zero-extended u16
        // (0..=255) since the parent's color_convert::convert_420_to_rgb
        // expects samples in source bit-depth range.
        for y in 0..h {
            // SAFETY: row (cy + y) is within the aligned_height-row Y
            // plane per the caller's guarantee.
            let row =
                unsafe { core::slice::from_raw_parts(base.add((cy + y) * row_stride), row_stride) };
            for x in 0..w {
                y_plane[y * w + x] = u16::from(row[cx + x]);
            }
        }
        // UV plane starts at aligned_height * row_stride.
        for y in 0..half_h {
            // SAFETY: UV row (h_aligned + chroma_cy + y) is within
            // [base, base + total_bytes).
            let row = unsafe {
                core::slice::from_raw_parts(
                    base.add((h_aligned + chroma_cy + y) * row_stride),
                    row_stride,
                )
            };
            for x in 0..half_w {
                let off = (chroma_cx + x) * 2;
                cb_plane[y * half_w + x] = u16::from(row[off]);
                cr_plane[y * half_w + x] = u16::from(row[off + 1]);
            }
        }
    } else {
        // 10-bit P010: u16 LE, MSB-aligned, low 6 bits zero. Shift right by 6.
        for y in 0..h {
            // SAFETY: row (cy + y) inside coded Y plane.
            let row =
                unsafe { core::slice::from_raw_parts(base.add((cy + y) * row_stride), row_stride) };
            for x in 0..w {
                let off = (cx + x) * 2;
                let lo = row[off];
                let hi = row[off + 1];
                let v = (u16::from(hi) << 8) | u16::from(lo);
                y_plane[y * w + x] = v >> 6;
            }
        }
        for y in 0..half_h {
            // SAFETY: UV row at (h_aligned + chroma_cy + y).
            let row = unsafe {
                core::slice::from_raw_parts(
                    base.add((h_aligned + chroma_cy + y) * row_stride),
                    row_stride,
                )
            };
            for x in 0..half_w {
                let off = (chroma_cx + x) * 4;
                let cb = (u16::from(row[off + 1]) << 8) | u16::from(row[off]);
                let cr = (u16::from(row[off + 3]) << 8) | u16::from(row[off + 2]);
                cb_plane[y * half_w + x] = cb >> 6;
                cr_plane[y * half_w + x] = cr >> 6;
            }
        }
    }

    OutputPlanes {
        y: y_plane,
        cb: cb_plane,
        cr: cr_plane,
    }
}
