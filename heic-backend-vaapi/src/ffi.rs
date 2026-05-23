//! libva FFI types + dlopen'd symbol table.
//!
//! All libva entry points are loaded via `libloading` so the crate
//! compiles on systems without `libva-dev`. The symbol table is
//! initialized once per [`crate::decode::Session`] and reused for
//! every frame in that session — symbol resolution adds <10 µs
//! per session, negligible vs. the GPU decode itself.
//!
//! Struct layouts mirror `va/va.h` and `va/va_dec_hevc.h` from
//! libva 1.14+. Every field carries the same name + position as
//! the C original so cross-referencing is grep-able. The picture
//! parameter struct lives in [`crate::va_hevc`] because the
//! parent crate's populator (`va_hevc::from_sps_pps`) already
//! produces it from `ParsedSps` + `ParsedPps`.

#![cfg(target_os = "linux")]
#![allow(dead_code)] // not every status code or symbol is wired in
#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
// Buffer-type enum variants intentionally share the `BufferType`
// postfix — they mirror libva's C identifiers 1:1.
#![allow(clippy::enum_variant_names)]

use std::ffi::{c_int, c_uint, c_void};

use libloading::Library;

pub(crate) type VaStatus = c_int;
pub(crate) const VA_STATUS_SUCCESS: VaStatus = 0;

pub(crate) type VaDisplay = *mut c_void;
pub(crate) type VaConfigId = c_uint;
pub(crate) type VaContextId = c_uint;
pub(crate) type VaSurfaceId = c_uint;
pub(crate) type VaBufferId = c_uint;
pub(crate) type VaImageId = c_uint;
pub(crate) const VA_INVALID_ID: c_uint = 0xFFFF_FFFF;
pub(crate) const VA_INVALID_SURFACE: VaSurfaceId = VA_INVALID_ID;

pub(crate) const VA_PROFILE_HEVC_MAIN: i32 = 17;
pub(crate) const VA_PROFILE_HEVC_MAIN_10: i32 = 18;
pub(crate) const VA_ENTRYPOINT_VLD: i32 = 1;

/// `VA_RT_FORMAT_YUV420` — 4:2:0 8-bit chroma.
pub(crate) const VA_RT_FORMAT_YUV420: c_uint = 0x0000_0001;
/// `VA_RT_FORMAT_YUV420_10` — 4:2:0 10-bit chroma (P010 output).
pub(crate) const VA_RT_FORMAT_YUV420_10: c_uint = 0x0010_0000;

/// `VAProgressive` — interlace flag for `vaCreateContext`. HEIC tiles
/// are always progressive single-frame.
pub(crate) const VA_PROGRESSIVE: c_int = 0;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) enum VaBufferType {
    PictureParameterBufferType = 0,
    IQMatrixBufferType = 1,
    SliceParameterBufferType = 3,
    SliceDataBufferType = 4,
}

/// `VAImage` — opaque libva image descriptor populated by
/// `vaDeriveImage` / `vaCreateImage`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(crate) struct VaImage {
    pub image_id: VaImageId,
    pub format: VaImageFormat,
    pub buf: VaBufferId,
    pub width: u16,
    pub height: u16,
    pub data_size: u32,
    pub num_planes: u32,
    pub pitches: [u32; 3],
    pub offsets: [u32; 3],
    pub num_palette_entries: u32,
    pub entry_bytes: u32,
    pub component_order: [i8; 4],
    pub va_reserved: [u32; 4],
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(crate) struct VaImageFormat {
    pub fourcc: c_uint,
    pub byte_order: c_uint,
    pub bits_per_pixel: c_uint,
    pub depth: c_uint,
    pub red_mask: c_uint,
    pub green_mask: c_uint,
    pub blue_mask: c_uint,
    pub alpha_mask: c_uint,
    pub va_reserved: [u32; 4],
}

/// `VAIQMatrixBufferHEVC` — scaling-list buffer for HEVC. Bytes are
/// laid out identically to the spec table 7-3 / 7-4 the
/// `va_hevc::HevcScalingListData` mirrors, so we can just `memcpy`
/// the parsed lists into this struct.
#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct VaIqMatrixBufferHevc {
    pub ScalingList4x4: [[u8; 16]; 6],
    pub ScalingList8x8: [[u8; 64]; 6],
    pub ScalingList16x16: [[u8; 64]; 6],
    pub ScalingList32x32: [[u8; 64]; 2],
    pub ScalingListDC16x16: [u8; 6],
    pub ScalingListDC32x32: [u8; 2],
    pub va_reserved: [u32; 4],
}

impl Default for VaIqMatrixBufferHevc {
    fn default() -> Self {
        // HEVC default scaling list = flat 16s across every matrixId.
        Self {
            ScalingList4x4: [[16; 16]; 6],
            ScalingList8x8: [[16; 64]; 6],
            ScalingList16x16: [[16; 64]; 6],
            ScalingList32x32: [[16; 64]; 2],
            ScalingListDC16x16: [16; 6],
            ScalingListDC32x32: [16; 2],
            va_reserved: [0; 4],
        }
    }
}

/// `VASliceParameterBufferHEVC` — slice-control buffer for HEVC
/// short-format decode (matches libva 1.14 `va_dec_hevc.h`). The
/// bitfields are modeled as integer alternatives.
#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(crate) struct VaSliceParameterBufferHevc {
    pub slice_data_size: u32,
    pub slice_data_offset: u32,
    pub slice_data_flag: u32,
    pub slice_data_byte_offset: u32,
    pub slice_segment_address: u32,
    pub RefPicList: [[u8; 15]; 2],
    pub LongSliceFlags: u32, // bitfield union
    pub collocated_ref_idx: u8,
    pub num_ref_idx_l0_active_minus1: u8,
    pub num_ref_idx_l1_active_minus1: u8,
    pub slice_qp_delta: i8,
    pub slice_cb_qp_offset: i8,
    pub slice_cr_qp_offset: i8,
    pub slice_beta_offset_div2: i8,
    pub slice_tc_offset_div2: i8,
    pub luma_log2_weight_denom: u8,
    pub delta_chroma_log2_weight_denom: i8,
    pub delta_luma_weight_l0: [i8; 15],
    pub luma_offset_l0: [i8; 15],
    pub delta_chroma_weight_l0: [[i8; 2]; 15],
    pub ChromaOffsetL0: [[i8; 2]; 15],
    pub delta_luma_weight_l1: [i8; 15],
    pub luma_offset_l1: [i8; 15],
    pub delta_chroma_weight_l1: [[i8; 2]; 15],
    pub ChromaOffsetL1: [[i8; 2]; 15],
    pub five_minus_max_num_merge_cand: u8,
    pub num_entry_point_offsets: u16,
    pub entry_offset_to_subset_array: u16,
    pub slice_data_num_emu_prevn_bytes: u16,
    pub va_reserved: [u32; 4],
}

/// `LongSliceFlags` bit positions in `VASliceParameterBufferHEVC`
/// (matches libva 1.14's bitfield order). LSB-first per the union.
pub(crate) mod slice_flags {
    pub const LAST_SLICE_OF_PIC: u32 = 1 << 0;
    pub const DEPENDENT_SLICE_SEGMENT_FLAG: u32 = 1 << 1;
    pub const SLICE_TYPE_SHIFT: u32 = 2;
    pub const SLICE_TYPE_MASK: u32 = 0b11; // I=2, P=1, B=0
    pub const COLOR_PLANE_ID_SHIFT: u32 = 4;
    pub const COLOR_PLANE_ID_MASK: u32 = 0b11;
    pub const SLICE_SAO_LUMA_FLAG: u32 = 1 << 6;
    pub const SLICE_SAO_CHROMA_FLAG: u32 = 1 << 7;
    pub const MVD_L1_ZERO_FLAG: u32 = 1 << 8;
    pub const CABAC_INIT_FLAG: u32 = 1 << 9;
    pub const SLICE_TEMPORAL_MVP_ENABLED_FLAG: u32 = 1 << 10;
    pub const SLICE_DEBLOCKING_FILTER_DISABLED_FLAG: u32 = 1 << 11;
    pub const COLLOCATED_FROM_L0_FLAG: u32 = 1 << 12;
    pub const SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED_FLAG: u32 = 1 << 13;
}

/// Bag of dlopen'd libva entry points.
pub(crate) struct LibvaSymbols {
    pub va_initialize: unsafe extern "C" fn(VaDisplay, *mut c_int, *mut c_int) -> VaStatus,
    pub va_terminate: unsafe extern "C" fn(VaDisplay) -> VaStatus,
    pub va_max_num_profiles: unsafe extern "C" fn(VaDisplay) -> c_int,
    pub va_query_config_profiles: unsafe extern "C" fn(VaDisplay, *mut i32, *mut c_int) -> VaStatus,
    pub va_create_config: unsafe extern "C" fn(
        VaDisplay,
        i32,         // profile
        i32,         // entrypoint
        *mut c_void, // attrib_list
        c_int,       // num_attribs
        *mut VaConfigId,
    ) -> VaStatus,
    pub va_destroy_config: unsafe extern "C" fn(VaDisplay, VaConfigId) -> VaStatus,
    pub va_create_surfaces: unsafe extern "C" fn(
        VaDisplay,
        c_uint,           // format
        c_uint,           // width
        c_uint,           // height
        *mut VaSurfaceId, // surfaces
        c_uint,           // num_surfaces
        *mut c_void,      // attrib_list
        c_uint,           // num_attribs
    ) -> VaStatus,
    pub va_destroy_surfaces: unsafe extern "C" fn(VaDisplay, *mut VaSurfaceId, c_int) -> VaStatus,
    pub va_create_context: unsafe extern "C" fn(
        VaDisplay,
        VaConfigId,
        c_int,            // picture_width
        c_int,            // picture_height
        c_int,            // flag (VA_PROGRESSIVE)
        *mut VaSurfaceId, // render_targets
        c_int,            // num_render_targets
        *mut VaContextId,
    ) -> VaStatus,
    pub va_destroy_context: unsafe extern "C" fn(VaDisplay, VaContextId) -> VaStatus,
    pub va_create_buffer: unsafe extern "C" fn(
        VaDisplay,
        VaContextId,
        c_int,         // VaBufferType
        c_uint,        // size
        c_uint,        // num_elements
        *const c_void, // data
        *mut VaBufferId,
    ) -> VaStatus,
    pub va_destroy_buffer: unsafe extern "C" fn(VaDisplay, VaBufferId) -> VaStatus,
    pub va_begin_picture: unsafe extern "C" fn(VaDisplay, VaContextId, VaSurfaceId) -> VaStatus,
    pub va_render_picture:
        unsafe extern "C" fn(VaDisplay, VaContextId, *mut VaBufferId, c_int) -> VaStatus,
    pub va_end_picture: unsafe extern "C" fn(VaDisplay, VaContextId) -> VaStatus,
    pub va_sync_surface: unsafe extern "C" fn(VaDisplay, VaSurfaceId) -> VaStatus,
    pub va_derive_image: unsafe extern "C" fn(VaDisplay, VaSurfaceId, *mut VaImage) -> VaStatus,
    pub va_destroy_image: unsafe extern "C" fn(VaDisplay, VaImageId) -> VaStatus,
    pub va_map_buffer: unsafe extern "C" fn(VaDisplay, VaBufferId, *mut *mut c_void) -> VaStatus,
    pub va_unmap_buffer: unsafe extern "C" fn(VaDisplay, VaBufferId) -> VaStatus,

    // Kept alive so the function pointers above stay valid.
    _libva: Library,
    _libva_x11: Option<Library>,
    _libx11: Option<Library>,
}

impl LibvaSymbols {
    /// Resolve every entry point we need from `libva.so.2`. Returns
    /// an error if any symbol is missing — older libva builds that
    /// predate `vaDeriveImage` aren't supported.
    pub(crate) fn load() -> Result<Self, libloading::Error> {
        // SAFETY: standard dlopen contract; libva.so.2 is the stable SONAME.
        let libva = unsafe { Library::new("libva.so.2") }?;
        macro_rules! sym {
            ($name:literal) => {{
                // SAFETY: libva exports each symbol with the documented C ABI;
                // the type at the call site below mirrors va.h exactly.
                let s: libloading::Symbol<_> = unsafe { libva.get($name) }?;
                *s
            }};
        }
        Ok(LibvaSymbols {
            va_initialize: sym!(b"vaInitialize\0"),
            va_terminate: sym!(b"vaTerminate\0"),
            va_max_num_profiles: sym!(b"vaMaxNumProfiles\0"),
            va_query_config_profiles: sym!(b"vaQueryConfigProfiles\0"),
            va_create_config: sym!(b"vaCreateConfig\0"),
            va_destroy_config: sym!(b"vaDestroyConfig\0"),
            va_create_surfaces: sym!(b"vaCreateSurfaces\0"),
            va_destroy_surfaces: sym!(b"vaDestroySurfaces\0"),
            va_create_context: sym!(b"vaCreateContext\0"),
            va_destroy_context: sym!(b"vaDestroyContext\0"),
            va_create_buffer: sym!(b"vaCreateBuffer\0"),
            va_destroy_buffer: sym!(b"vaDestroyBuffer\0"),
            va_begin_picture: sym!(b"vaBeginPicture\0"),
            va_render_picture: sym!(b"vaRenderPicture\0"),
            va_end_picture: sym!(b"vaEndPicture\0"),
            va_sync_surface: sym!(b"vaSyncSurface\0"),
            va_derive_image: sym!(b"vaDeriveImage\0"),
            va_destroy_image: sym!(b"vaDestroyImage\0"),
            va_map_buffer: sym!(b"vaMapBuffer\0"),
            va_unmap_buffer: sym!(b"vaUnmapBuffer\0"),
            _libva: libva,
            _libva_x11: None,
            _libx11: None,
        })
    }

    /// Attach libva-x11 + libX11 handles so they stay alive for the
    /// session's lifetime (required when the VADisplay was obtained
    /// via the X11 path).
    pub(crate) fn with_x11_libs(mut self, libva_x11: Library, libx11: Library) -> Self {
        self._libva_x11 = Some(libva_x11);
        self._libx11 = Some(libx11);
        self
    }
}
