//! NV12 / P010 → planar `u16` readback from a D3D11 decoder output
//! texture.
//!
//! After `DecoderSession::submit_one_frame` flushes the GPU, the
//! decoded frame lives in `output_texture` (a `D3D11_USAGE_DEFAULT`
//! NV12 / P010 texture with `BindFlags=DECODER`). To pull bytes back
//! to the CPU we need an intermediate staging texture with
//! `D3D11_USAGE_STAGING` + `D3D11_CPU_ACCESS_READ` — the staging
//! texture has no GPU bindings but can be `Map`'d for CPU reads.
//!
//! Pipeline (per
//! `media/gpu/windows/d3d11_h265_accelerator.cc::SubmitDecode` +
//! its readback in `D3D11VideoDecoder::Decode`):
//!
//! 1. Create a one-shot staging texture matching the output format.
//! 2. `CopyResource(staging, output_view's texture)`.
//! 3. `Map(staging, 0, D3D11_MAP_READ)` → row-aligned NV12 / P010.
//! 4. Unpack using the same per-row pattern as the MF backend's
//!    `pixels.rs::unpack_nv12_or_p010`.
//! 5. `Unmap`.
//!
//! The staging texture is freshly allocated per readback — a future
//! optimization caches it on the session, but per-call alloc is fine
//! for HEIC's one-frame-per-decode pattern.

#![cfg(target_os = "windows")]
#![allow(missing_docs)]

use std::vec;
use std::vec::Vec;

use heic_core::BackendError;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CPU_ACCESS_READ, D3D11_MAP_READ, D3D11_MAPPED_SUBRESOURCE, D3D11_RESOURCE_MISC_FLAG,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING, ID3D11Device, ID3D11DeviceContext, ID3D11Resource,
    ID3D11Texture2D, ID3D11VideoContext,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::core::Interface;

/// Three planar `u16` planes returned by [`read_decoded_planes`].
///
/// Same layout as `heic-backend-mediafoundation::pixels::OutputPlanes`
/// so the parent crate's color-conversion path consumes both backends'
/// outputs identically.
pub struct OutputPlanes {
    pub y: Vec<u16>,
    pub cb: Vec<u16>,
    pub cr: Vec<u16>,
}

/// Copy the GPU output into a staging texture, Map it, and unpack
/// NV12 / P010 → planar `u16` honoring the SPS conformance crop.
#[allow(clippy::too_many_arguments)]
pub(crate) fn read_decoded_planes(
    device: &ID3D11Device,
    video_context: &ID3D11VideoContext,
    output_texture: &ID3D11Texture2D,
    output_format: DXGI_FORMAT,
    coded_w: u32,
    coded_h: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    // 1. Build a one-shot staging texture matching the output format.
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: coded_w,
        Height: coded_h,
        MipLevels: 1,
        ArraySize: 1,
        Format: output_format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: D3D11_RESOURCE_MISC_FLAG(0).0 as u32,
    };
    let mut staging: Option<ID3D11Texture2D> = None;
    // SAFETY: staging_desc lives through the call; out pointer non-null.
    unsafe { device.CreateTexture2D(&staging_desc, None, Some(&mut staging)) }
        .map_err(|e| BackendError::Decode(format!("CreateTexture2D(staging): {e}")))?;
    let staging = staging.ok_or(BackendError::Decode("CreateTexture2D returned None".into()))?;

    // 2. Get the immediate device context (we already have the video
    // context, but CopyResource lives on the regular device context).
    // ID3D11VideoContext extends ID3D11DeviceChild + the raw
    // ID3D11DeviceContext, so we can cast back to the parent.
    let ctx: ID3D11DeviceContext = video_context
        .cast()
        .map_err(|e| BackendError::Decode(format!("VideoContext cast to DeviceContext: {e}")))?;
    let src_resource: ID3D11Resource = output_texture
        .cast()
        .map_err(|e| BackendError::Decode(format!("Texture2D cast to Resource (src): {e}")))?;
    let dst_resource: ID3D11Resource = staging
        .cast()
        .map_err(|e| BackendError::Decode(format!("Texture2D cast to Resource (dst): {e}")))?;
    // SAFETY: both resources are live; CopyResource is the documented
    // full-texture copy entry point.
    unsafe { ctx.CopyResource(&dst_resource, &src_resource) };

    // 3. Map the staging texture for CPU read.
    // SAFETY: zeroed MAPPED_SUBRESOURCE is a valid initial state; the
    // API fills it on success.
    let mut mapped: D3D11_MAPPED_SUBRESOURCE = unsafe { core::mem::zeroed() };
    // SAFETY: staging is live; Map blocks until the GPU finishes its
    // pending CopyResource.
    unsafe { ctx.Map(&dst_resource, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }
        .map_err(|e| BackendError::Decode(format!("Map(staging): {e}")))?;

    // 4. Unpack NV12 / P010 from the mapped bytes.
    // SAFETY: mapped.pData is valid for at least RowPitch * Height
    // bytes per the D3D11 docs. Same per-row pattern as
    // heic-backend-mediafoundation/src/pixels.rs.
    let planes_result = unsafe {
        unpack_nv12_or_p010(
            mapped.pData as *const u8,
            mapped.RowPitch as usize,
            coded_h,
            visible_w,
            visible_h,
            crop_x,
            crop_y,
            bit_depth,
            output_format,
        )
    };

    // 5. Unmap (must pair the Map; do it before returning).
    // SAFETY: same staging resource.
    unsafe { ctx.Unmap(&dst_resource, 0) };

    Ok(planes_result)
}

/// SAFETY: caller guarantees `base` points at the staging texture's
/// NV12 / P010 mapped bytes, `row_stride` is the `RowPitch` from
/// `D3D11_MAPPED_SUBRESOURCE`, and the buffer holds at least
/// `row_stride * coded_h * 3 / 2` bytes (Y plane + half-height UV).
///
/// Output planes are sized `visible_w * visible_h` (and the half-size
/// chroma versions) — the SPS conformance crop is applied during the
/// copy so callers see exactly the ispe-visible region.
#[allow(clippy::too_many_arguments)]
unsafe fn unpack_nv12_or_p010(
    base: *const u8,
    row_stride: usize,
    coded_h: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
    format: DXGI_FORMAT,
) -> OutputPlanes {
    let w = visible_w as usize;
    let h = visible_h as usize;
    let coded_h = coded_h as usize;
    let cx = crop_x as usize;
    let cy = crop_y as usize;
    let half_w = w / 2;
    let half_h = h / 2;
    let chroma_cx = cx / 2;
    let chroma_cy = cy / 2;

    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    let is_p010 = format == DXGI_FORMAT_P010 || bit_depth >= 10;

    if !is_p010 {
        // 8-bit NV12. Y rows at byte offset (cy + y)*stride + cx.
        for y in 0..h {
            // SAFETY: row (cy + y) within coded_h per caller guarantee.
            let row =
                unsafe { core::slice::from_raw_parts(base.add((cy + y) * row_stride), row_stride) };
            for x in 0..w {
                y_plane[y * w + x] = u16::from(row[cx + x]);
            }
        }
        for y in 0..half_h {
            // SAFETY: UV plane at (coded_h + chroma_cy + y) rows.
            let row = unsafe {
                core::slice::from_raw_parts(
                    base.add((coded_h + chroma_cy + y) * row_stride),
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
        // P010: u16 LE per pixel, MSB-aligned (10-bit value in the
        // upper 10 bits; low 6 bits zero). Shift right by 6.
        for y in 0..h {
            // SAFETY: row (cy + y) inside coded plane; 2 bytes per pixel.
            let row =
                unsafe { core::slice::from_raw_parts(base.add((cy + y) * row_stride), row_stride) };
            for x in 0..w {
                let off = (cx + x) * 2;
                let v = (u16::from(row[off + 1]) << 8) | u16::from(row[off]);
                y_plane[y * w + x] = v >> 6;
            }
        }
        for y in 0..half_h {
            // SAFETY: UV row at (coded_h + chroma_cy + y), 4 bytes per UV pair.
            let row = unsafe {
                core::slice::from_raw_parts(
                    base.add((coded_h + chroma_cy + y) * row_stride),
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
