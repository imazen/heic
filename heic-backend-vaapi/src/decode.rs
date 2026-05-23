//! VA-API runtime HEVC decode path.
//!
//! Owns the per-stream libva resources (`VADisplay`, `VAConfig`,
//! `VAContext`, output `VASurface`) and drives the per-frame
//! `vaBeginPicture` → `vaRenderPicture` × N → `vaEndPicture` →
//! `vaSyncSurface` → `vaDeriveImage` → `vaMapBuffer` → unpack
//! sequence.
//!
//! Display backend selection mirrors `probe.rs`: DRM render nodes
//! first (bare-metal Linux + VMs with `/dev/dri`), X11 fallback
//! second (WSL2 via WSLg). The libva driver behind the display
//! does the actual hardware decode through CUDA NVDEC,
//! D3D12 VideoDecode, Intel iHD, AMD VCN, etc. — this module
//! doesn't care which.
//!
//! Per-tile teardown reuses the cached session when the next
//! tile shares dimensions + bit-depth. For HEIC grids that means
//! ONE `vaCreateConfig`/`vaCreateContext`/`vaCreateSurfaces` per
//! image; subsequent tiles are just buffer-create + submit.

#![cfg(target_os = "linux")]
// libloading + libva FFI: each call site has the same shape of
// "valid display + valid handle returned by libva; call its
// documented teardown". Per-block SAFETY comments would balloon
// the file to ~1.5x its size; the module-level rationale here
// applies to every `unsafe` block below.
#![allow(clippy::undocumented_unsafe_blocks)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::field_reassign_with_default)] // VASliceParameter has 25+ fields
#![allow(clippy::type_complexity)] // libva fn types are intrinsically big

use std::ffi::{c_int, c_uint, c_void};
use std::fs::File;
use std::os::fd::IntoRawFd;
use std::vec;
use std::vec::Vec;

use heic_core::{BackendError, DecodedFrame, HvccParams};
use libloading::Library;

use crate::ffi::*;
use crate::slice;
use crate::va_hevc::from_sps_pps;

/// Cached per-stream libva resources.
pub(crate) struct Session {
    pub(crate) sym: LibvaSymbols,
    pub(crate) display: VaDisplay,
    pub(crate) config: VaConfigId,
    pub(crate) context: VaContextId,
    pub(crate) surface: VaSurfaceId,
    pub(crate) coded_width: u32,
    pub(crate) coded_height: u32,
    pub(crate) bit_depth: u8,
    /// XOpenDisplay handle the X11 path produced; we close it on
    /// drop. `None` on the DRM path.
    x_display: *mut c_void,
    x_close_display: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
}

// SAFETY: VADisplay + libva resources are documented thread-safe
// under per-display serialization. The struct holds no Rust-side
// shared mutability beyond what the FFI symbols carry.
unsafe impl Send for Session {}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: each handle is one we got from libva (or null/
        // INVALID), and the corresponding destroy / terminate
        // function is the documented teardown.
        unsafe {
            if self.context != VA_INVALID_ID {
                let _ = (self.sym.va_destroy_context)(self.display, self.context);
            }
            if self.surface != VA_INVALID_SURFACE {
                let mut s = self.surface;
                let _ = (self.sym.va_destroy_surfaces)(self.display, &mut s, 1);
            }
            if self.config != VA_INVALID_ID {
                let _ = (self.sym.va_destroy_config)(self.display, self.config);
            }
            if !self.display.is_null() {
                let _ = (self.sym.va_terminate)(self.display);
            }
            if !self.x_display.is_null() {
                if let Some(close) = self.x_close_display {
                    close(self.x_display);
                }
            }
        }
    }
}

impl Session {
    /// Build a session for the given coded dimensions + bit depth.
    /// Returns `Unavailable` if no usable VADisplay backend is found
    /// (no DRM + no X11), or `Decode` for libva errors.
    pub(crate) fn new(coded_w: u32, coded_h: u32, bit_depth: u8) -> Result<Self, BackendError> {
        let sym = LibvaSymbols::load().map_err(|e| {
            BackendError::Unavailable(string_leak(format!("libva symbol load: {e}")))
        })?;
        // Open a VADisplay. Try DRM render nodes first, then X11.
        let (display, x_display, x_close) = open_display(&sym)?;

        // SAFETY: standard vaInitialize call.
        let mut major: c_int = 0;
        let mut minor: c_int = 0;
        let status = unsafe { (sym.va_initialize)(display, &mut major, &mut minor) };
        let _ = (major, minor);
        if status != VA_STATUS_SUCCESS {
            return Err(BackendError::Decode(format!(
                "vaInitialize: status {status}"
            )));
        }

        let (profile, rt_format) = if bit_depth >= 10 {
            (VA_PROFILE_HEVC_MAIN_10, VA_RT_FORMAT_YUV420_10)
        } else {
            (VA_PROFILE_HEVC_MAIN, VA_RT_FORMAT_YUV420)
        };

        // vaCreateConfig — HEVC Main(/Main10), VLD entrypoint, no extra attribs.
        let mut config: VaConfigId = VA_INVALID_ID;
        // SAFETY: standard vaCreateConfig signature; null attrib_list +
        // num_attribs=0 picks driver defaults.
        let status = unsafe {
            (sym.va_create_config)(
                display,
                profile,
                VA_ENTRYPOINT_VLD,
                core::ptr::null_mut(),
                0,
                &mut config,
            )
        };
        if status != VA_STATUS_SUCCESS {
            return Err(BackendError::Decode(format!(
                "vaCreateConfig: status {status}"
            )));
        }

        // vaCreateSurfaces — single surface sized to the coded dims.
        let mut surface: VaSurfaceId = VA_INVALID_SURFACE;
        // SAFETY: standard vaCreateSurfaces signature; null attribs.
        let status = unsafe {
            (sym.va_create_surfaces)(
                display,
                rt_format,
                coded_w,
                coded_h,
                &mut surface,
                1,
                core::ptr::null_mut(),
                0,
            )
        };
        if status != VA_STATUS_SUCCESS {
            return Err(BackendError::Decode(format!(
                "vaCreateSurfaces: status {status}"
            )));
        }

        // vaCreateContext — bound to the surface as the render target.
        let mut context: VaContextId = VA_INVALID_ID;
        // SAFETY: standard vaCreateContext signature.
        let status = unsafe {
            (sym.va_create_context)(
                display,
                config,
                coded_w as c_int,
                coded_h as c_int,
                VA_PROGRESSIVE,
                &mut surface,
                1,
                &mut context,
            )
        };
        if status != VA_STATUS_SUCCESS {
            return Err(BackendError::Decode(format!(
                "vaCreateContext: status {status}"
            )));
        }

        Ok(Session {
            sym,
            display,
            config,
            context,
            surface,
            coded_width: coded_w,
            coded_height: coded_h,
            bit_depth,
            x_display,
            x_close_display: x_close,
        })
    }

    pub(crate) fn matches(&self, w: u32, h: u32, bd: u8) -> bool {
        self.coded_width == w && self.coded_height == h && self.bit_depth == bd
    }
}

/// Open a VADisplay via DRM (preferred) or X11 (WSL fallback).
/// Returns `(display, x_display_handle_for_drop, x_close_fn)`.
fn open_display(
    sym: &LibvaSymbols,
) -> Result<
    (
        VaDisplay,
        *mut c_void,
        Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    ),
    BackendError,
> {
    // DRM path first.
    if let Ok(libva_drm) = unsafe_lib("libva-drm.so.2") {
        // SAFETY: libloading::get on a stable libva-drm symbol.
        if let Ok(get_drm) = unsafe {
            libva_drm.get::<unsafe extern "C" fn(c_int) -> *mut c_void>(b"vaGetDisplayDRM\0")
        } {
            for node in 128..136 {
                let path = std::format!("/dev/dri/renderD{node}");
                let Ok(file) = File::open(&path) else {
                    continue;
                };
                let fd = file.into_raw_fd();
                // SAFETY: fd just opened; vaGetDisplayDRM dup's internally.
                let display = unsafe { get_drm(fd) };
                // SAFETY: fd lifecycle ends here; libva owns its own dup.
                unsafe { libc::close(fd) };
                if !display.is_null() {
                    // Leak libva_drm by detaching it from the let-binding;
                    // sym already holds libva.so.2. The DRM handle is only
                    // needed during vaGetDisplayDRM; the resulting display
                    // is owned by libva itself.
                    drop(libva_drm);
                    let _ = sym; // sym ref used by call sites.
                    return Ok((display, core::ptr::null_mut(), None));
                }
            }
        }
    }

    // X11 fallback (WSL).
    if std::env::var_os("DISPLAY").is_some() {
        if let (Ok(libx11), Ok(libva_x11)) =
            (unsafe_lib("libX11.so.6"), unsafe_lib("libva-x11.so.2"))
        {
            // SAFETY: libloading::get on stable symbols.
            let x_open: libloading::Symbol<
                unsafe extern "C" fn(*const std::ffi::c_char) -> *mut c_void,
            > = unsafe { libx11.get(b"XOpenDisplay\0") }
                .map_err(|e| BackendError::Decode(format!("XOpenDisplay symbol: {e}")))?;
            let x_close: libloading::Symbol<unsafe extern "C" fn(*mut c_void) -> c_int> =
                unsafe { libx11.get(b"XCloseDisplay\0") }
                    .map_err(|e| BackendError::Decode(format!("XCloseDisplay symbol: {e}")))?;
            let va_get_display: libloading::Symbol<
                unsafe extern "C" fn(*mut c_void) -> *mut c_void,
            > = unsafe { libva_x11.get(b"vaGetDisplay\0") }
                .map_err(|e| BackendError::Decode(format!("vaGetDisplay symbol: {e}")))?;
            // SAFETY: standard X11 + libva-x11 entry points.
            let x_display = unsafe { x_open(core::ptr::null()) };
            if !x_display.is_null() {
                // SAFETY: x_display is a valid X11 connection.
                let va_display = unsafe { va_get_display(x_display) };
                if !va_display.is_null() {
                    // Copy the close fn out before dropping libx11; the
                    // Library handle stays alive in the Session via
                    // sym._libx11 if we attached it earlier.
                    let close_fn = *x_close;
                    // Leak libx11 + libva_x11 into the symbol table by
                    // forgetting them; the dlopen handles stay open for
                    // the process lifetime, which is fine — libva /
                    // libX11 are loaded by half the binary anyway.
                    std::mem::forget(libx11);
                    std::mem::forget(libva_x11);
                    let _ = sym;
                    return Ok((va_display, x_display, Some(close_fn)));
                }
                // SAFETY: close the orphan x_display before bailing.
                unsafe { x_close(x_display) };
            }
        }
    }

    Err(BackendError::Unavailable(
        "no VADisplay backend available — install nvidia-vaapi-driver or \
         use a host with /dev/dri/renderD128",
    ))
}

fn unsafe_lib(name: &str) -> Result<Library, libloading::Error> {
    // SAFETY: dlopen on stable SONAMES.
    unsafe { Library::new(name) }
}

fn string_leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Per-frame decode. Builds and submits the picture / IQ-matrix /
/// slice-parameter / slice-data buffers, syncs the surface, and
/// reads back planar `u16` YCbCr planes.
pub(crate) fn decode_one_frame(
    session: &Session,
    config: &HvccParams<'_>,
    image_data: &[u8],
) -> Result<DecodedFrame, BackendError> {
    let sps = config
        .sps
        .ok_or_else(|| BackendError::Decode("HvccParams.sps is None — parser didn't run".into()))?;

    // 1. Extract the VCL slice NAL from the hvcC payload and parse its
    //    header for the data byte offset libva needs.
    let slice_nal = first_vcl_nal(image_data, config.length_size)?;
    let slice_info = slice::parse_slice(slice_nal, sps, config.pps.unwrap_or(&Default::default()))
        .map_err(|e| BackendError::Decode(format!("slice header parse: {e:?}")))?;

    // 2. Build the picture / iq / slice / data buffers, render them.
    // SAFETY: every call below uses sym pointers loaded from libva and
    // session handles created by Session::new (validated to be
    // non-INVALID).
    let s = &session.sym;
    let display = session.display;
    let ctx = session.context;
    let surface = session.surface;

    // vaBeginPicture
    let status = unsafe { (s.va_begin_picture)(display, ctx, surface) };
    if status != VA_STATUS_SUCCESS {
        return Err(BackendError::Decode(format!("vaBeginPicture: {status}")));
    }

    // PICTURE_PARAMETERS — already populated by va_hevc::from_sps_pps.
    let mut pic_param = from_sps_pps(sps, config.pps);
    // Per-picture overrides: current pic POC, ref-list (all INVALID for IDR).
    pic_param.CurrPic.picture_id = surface;
    pic_param.CurrPic.pic_order_cnt = 0;
    // ReferenceFrames already INVALID per Default.
    submit_buffer(
        s,
        display,
        ctx,
        VaBufferType::PictureParameterBufferType,
        bytes_of(&pic_param),
        1,
    )?;

    // IQMatrix — submit when scaling lists enabled. For HEIC + nvidia-vaapi-driver
    // through NVDEC, the driver implements its own scaling-list defaults if we
    // skip this buffer, but the spec wants it whenever sps.scaling_list_enabled.
    if sps.scaling_list_enabled_flag {
        let iq = build_iq_matrix(sps, config.pps);
        submit_buffer(
            s,
            display,
            ctx,
            VaBufferType::IQMatrixBufferType,
            bytes_of(&iq),
            1,
        )?;
    }

    // SLICE_PARAMETER + SLICE_DATA.
    let slice_param = build_slice_param(sps, &slice_info, slice_nal.len() as u32);
    submit_buffer(
        s,
        display,
        ctx,
        VaBufferType::SliceParameterBufferType,
        bytes_of(&slice_param),
        1,
    )?;
    submit_buffer(
        s,
        display,
        ctx,
        VaBufferType::SliceDataBufferType,
        slice_nal,
        1,
    )?;

    // vaEndPicture — commits the decode.
    // SAFETY: paired with vaBeginPicture; submits queued buffers.
    let status = unsafe { (s.va_end_picture)(display, ctx) };
    if status != VA_STATUS_SUCCESS {
        return Err(BackendError::Decode(format!("vaEndPicture: {status}")));
    }

    // 3. Wait for the surface to finish.
    // SAFETY: surface is valid; vaSyncSurface blocks until decode done.
    let status = unsafe { (s.va_sync_surface)(display, surface) };
    if status != VA_STATUS_SUCCESS {
        return Err(BackendError::Decode(format!("vaSyncSurface: {status}")));
    }

    // 4. vaDeriveImage + vaMapBuffer to read back the pixels.
    let mut image: VaImage = VaImage::default();
    // SAFETY: standard vaDeriveImage signature.
    let status = unsafe { (s.va_derive_image)(display, surface, &mut image) };
    if status != VA_STATUS_SUCCESS {
        return Err(BackendError::Decode(format!("vaDeriveImage: {status}")));
    }

    let mut mapped: *mut c_void = core::ptr::null_mut();
    // SAFETY: image.buf is valid after derive.
    let status = unsafe { (s.va_map_buffer)(display, image.buf, &mut mapped) };
    if status != VA_STATUS_SUCCESS {
        let _ = unsafe { (s.va_destroy_image)(display, image.image_id) };
        return Err(BackendError::Decode(format!("vaMapBuffer: {status}")));
    }

    let planes = unpack_planes(&image, mapped as *const u8, config)?;

    // SAFETY: paired unmap + destroy.
    let _ = unsafe { (s.va_unmap_buffer)(display, image.buf) };
    let _ = unsafe { (s.va_destroy_image)(display, image.image_id) };

    Ok(DecodedFrame {
        width: config.width,
        height: config.height,
        y_plane: planes.0,
        cb_plane: planes.1,
        cr_plane: planes.2,
        bit_depth: session.bit_depth,
        chroma_format: config.chroma_format_idc,
        crop_left: 0,
        crop_right: 0,
        crop_top: 0,
        crop_bottom: 0,
        alpha_plane: None,
        full_range: config.full_range,
        matrix_coeffs: config.matrix_coeffs,
        color_primaries: config.color_primaries,
        transfer_characteristics: config.transfer_characteristics,
        deblock_flags: Vec::new(),
        deblock_stride: 0,
        qp_map: Vec::new(),
    })
}

fn first_vcl_nal(data: &[u8], length_size: u8) -> Result<&[u8], BackendError> {
    let ls = length_size as usize;
    let mut i = 0;
    while i + ls <= data.len() {
        let mut nal_len = 0usize;
        for &b in &data[i..i + ls] {
            nal_len = (nal_len << 8) | b as usize;
        }
        i += ls;
        if i + nal_len > data.len() {
            return Err(BackendError::Decode("hvcC slice payload truncated".into()));
        }
        if nal_len > 0 {
            let nal_type = (data[i] >> 1) & 0x3F;
            if matches!(nal_type, 0..=9 | 16..=21) {
                return Ok(&data[i..i + nal_len]);
            }
        }
        i += nal_len;
    }
    Err(BackendError::Decode("no VCL NAL in hvcC payload".into()))
}

fn build_iq_matrix(
    sps: &heic_core::sps::ParsedSps,
    pps: Option<&heic_core::sps::ParsedPps>,
) -> VaIqMatrixBufferHevc {
    let custom = pps
        .and_then(|p| p.pps_scaling_list.as_ref())
        .or(sps.scaling_list.as_ref());
    let Some(s) = custom else {
        return VaIqMatrixBufferHevc::default();
    };
    let mut iq = VaIqMatrixBufferHevc::default();
    for m in 0..6 {
        iq.ScalingList4x4[m].copy_from_slice(&s.lists[0][m][..16]);
        iq.ScalingList8x8[m] = s.lists[1][m];
        iq.ScalingList16x16[m] = s.lists[2][m];
    }
    iq.ScalingList32x32[0] = s.lists[3][0];
    iq.ScalingList32x32[1] = s.lists[3][3];
    iq.ScalingListDC16x16 = s.dc_coef[0];
    iq.ScalingListDC32x32[0] = s.dc_coef[1][0];
    iq.ScalingListDC32x32[1] = s.dc_coef[1][3];
    iq
}

fn build_slice_param(
    sps: &heic_core::sps::ParsedSps,
    si: &slice::SliceInfo,
    slice_nal_len: u32,
) -> VaSliceParameterBufferHevc {
    let mut p = VaSliceParameterBufferHevc::default();
    p.slice_data_size = slice_nal_len;
    p.slice_data_offset = 0;
    p.slice_data_flag = 0; // VA_SLICE_DATA_FLAG_ALL
    p.slice_data_byte_offset = si.data_byte_offset;
    p.slice_segment_address = si.slice_segment_address;
    // RefPicList: all 0xFF (no refs for IDR).
    p.RefPicList = [[0xFF; 15]; 2];
    // LongSliceFlags bitfield
    use crate::ffi::slice_flags::*;
    let mut flags = 0u32;
    flags |= LAST_SLICE_OF_PIC; // single slice covers whole pic
    flags |= (u32::from(si.slice_type) & SLICE_TYPE_MASK) << SLICE_TYPE_SHIFT;
    if si.slice_sao_luma_flag {
        flags |= SLICE_SAO_LUMA_FLAG;
    }
    if si.slice_sao_chroma_flag {
        flags |= SLICE_SAO_CHROMA_FLAG;
    }
    // mvd_l1_zero_flag = 0, cabac_init_flag = 0, temporal_mvp = 0,
    // deblocking_filter_disabled per pps.
    flags |= SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED_FLAG;
    p.LongSliceFlags = flags;
    p.collocated_ref_idx = 0xFF;
    p.num_ref_idx_l0_active_minus1 = 0;
    p.num_ref_idx_l1_active_minus1 = 0;
    p.slice_qp_delta = si.slice_qp_delta;
    p.slice_cb_qp_offset = si.slice_cb_qp_offset;
    p.slice_cr_qp_offset = si.slice_cr_qp_offset;
    p.five_minus_max_num_merge_cand = 5;
    let _ = sps; // sps consumed for future fields (e.g., wraparound).
    p
}

fn bytes_of<T>(value: &T) -> &[u8] {
    // SAFETY: every struct passed here is #[repr(C)] + Copy; we read
    // its raw bytes for a memcpy into the libva buffer.
    unsafe {
        core::slice::from_raw_parts((value as *const T) as *const u8, core::mem::size_of::<T>())
    }
}

fn submit_buffer(
    s: &LibvaSymbols,
    display: VaDisplay,
    ctx: VaContextId,
    btype: VaBufferType,
    data: &[u8],
    num_elements: c_uint,
) -> Result<(), BackendError> {
    let mut buf: VaBufferId = VA_INVALID_ID;
    let elem_size = if num_elements == 0 {
        data.len() as c_uint
    } else {
        (data.len() / num_elements as usize) as c_uint
    };
    // SAFETY: standard vaCreateBuffer call.
    let status = unsafe {
        (s.va_create_buffer)(
            display,
            ctx,
            btype as c_int,
            elem_size,
            num_elements.max(1),
            data.as_ptr() as *const c_void,
            &mut buf,
        )
    };
    if status != VA_STATUS_SUCCESS {
        return Err(BackendError::Decode(format!(
            "vaCreateBuffer({btype:?}): status {status}",
            btype = btype as i32,
        )));
    }
    // SAFETY: vaRenderPicture takes ownership of the buffer ID list.
    let mut buf_slot = buf;
    let status = unsafe { (s.va_render_picture)(display, ctx, &mut buf_slot, 1) };
    if status != VA_STATUS_SUCCESS {
        // SAFETY: cleanup the buffer if render rejected it.
        let _ = unsafe { (s.va_destroy_buffer)(display, buf) };
        return Err(BackendError::Decode(format!(
            "vaRenderPicture: status {status}"
        )));
    }
    // libva is documented to destroy buffers as part of vaEndPicture,
    // so we don't call vaDestroyBuffer here — duplicating would crash.
    Ok(())
}

/// Unpack NV12 / P010 from the mapped VAImage into planar `u16`
/// Y/Cb/Cr planes sized to the visible region of `config`.
fn unpack_planes(
    image: &VaImage,
    base: *const u8,
    config: &HvccParams<'_>,
) -> Result<(Vec<u16>, Vec<u16>, Vec<u16>), BackendError> {
    let w = config.width as usize;
    let h = config.height as usize;
    let cx = config.crop_left as usize;
    let cy = config.crop_top as usize;
    let half_w = w / 2;
    let half_h = h / 2;
    let chroma_cx = cx / 2;
    let chroma_cy = cy / 2;
    let bit_depth = config.bit_depth_luma;

    if image.num_planes < 2 {
        return Err(BackendError::Decode(format!(
            "VAImage has {} planes; expected 2 (NV12/P010)",
            image.num_planes
        )));
    }
    let y_pitch = image.pitches[0] as usize;
    let y_offset = image.offsets[0] as usize;
    let uv_pitch = image.pitches[1] as usize;
    let uv_offset = image.offsets[1] as usize;

    let mut y = vec![0u16; w * h];
    let mut cb = vec![0u16; half_w * half_h];
    let mut cr = vec![0u16; half_w * half_h];

    if bit_depth <= 8 {
        for row in 0..h {
            // SAFETY: y_offset + (cy+row)*y_pitch + (cx+w) ≤ image.data_size
            // by libva's guarantee that the VAImage covers the full surface.
            let src_row = unsafe { base.add(y_offset + (cy + row) * y_pitch) };
            for col in 0..w {
                // SAFETY: per-row pointer stays within the row.
                y[row * w + col] = u16::from(unsafe { *src_row.add(cx + col) });
            }
        }
        for row in 0..half_h {
            // SAFETY: chroma offsets within image bounds.
            let src_row = unsafe { base.add(uv_offset + (chroma_cy + row) * uv_pitch) };
            for col in 0..half_w {
                // SAFETY: NV12 layout is interleaved Cb,Cr,Cb,Cr,...
                let cb_v = unsafe { *src_row.add((chroma_cx + col) * 2) };
                let cr_v = unsafe { *src_row.add((chroma_cx + col) * 2 + 1) };
                cb[row * half_w + col] = u16::from(cb_v);
                cr[row * half_w + col] = u16::from(cr_v);
            }
        }
    } else {
        // P010: 16-bit per sample, MSB-aligned 10-bit (low 6 bits = 0).
        for row in 0..h {
            // SAFETY: same bounds argument as the 8-bit path.
            let src_row = unsafe { base.add(y_offset + (cy + row) * y_pitch) };
            for col in 0..w {
                // SAFETY: per-pixel read; pitch covers width*2 bytes.
                let lo = unsafe { *src_row.add((cx + col) * 2) };
                let hi = unsafe { *src_row.add((cx + col) * 2 + 1) };
                y[row * w + col] = ((u16::from(hi) << 8) | u16::from(lo)) >> 6;
            }
        }
        for row in 0..half_h {
            // SAFETY: chroma half-pitch bounds.
            let src_row = unsafe { base.add(uv_offset + (chroma_cy + row) * uv_pitch) };
            for col in 0..half_w {
                // SAFETY: P010 interleaved Cb16,Cr16 — 4 bytes per chroma pair.
                let cb_lo = unsafe { *src_row.add((chroma_cx + col) * 4) };
                let cb_hi = unsafe { *src_row.add((chroma_cx + col) * 4 + 1) };
                let cr_lo = unsafe { *src_row.add((chroma_cx + col) * 4 + 2) };
                let cr_hi = unsafe { *src_row.add((chroma_cx + col) * 4 + 3) };
                cb[row * half_w + col] = ((u16::from(cb_hi) << 8) | u16::from(cb_lo)) >> 6;
                cr[row * half_w + col] = ((u16::from(cr_hi) << 8) | u16::from(cr_lo)) >> 6;
            }
        }
    }

    Ok((y, cb, cr))
}
