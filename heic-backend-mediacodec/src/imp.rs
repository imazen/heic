//! Android NDK MediaCodec HEVC decode pipeline.
//!
//! Compile-gated to `target_os = "android"` by the parent module. Drives
//! `AMediaCodec` synchronously per the documented stateless contract for
//! still-image decode:
//!
//! 1. `AMediaCodec_createDecoderByType("video/hevc")`.
//! 2. `AMediaFormat` with MIME, coded width/height, and `csd-0` set to
//!    Annex-B VPS+SPS+PPS.
//! 3. `AMediaCodec_configure(format, null surface, null crypto, 0)` —
//!    null surface forces ByteBuffer output mode.
//! 4. `AMediaCodec_start`.
//! 5. Input: `dequeueInputBuffer` → `getInputBuffer` → memcpy
//!    Annex-B slice payload → `queueInputBuffer` with
//!    `BUFFER_FLAG_END_OF_STREAM`.
//! 6. Output: `dequeueOutputBuffer` with timeout; handle
//!    `INFO_OUTPUT_FORMAT_CHANGED` (re-query format) and
//!    `INFO_TRY_AGAIN_LATER` (retry); on success, unpack the byte
//!    buffer using the color-format key from `getOutputFormat`.
//! 7. `AMediaCodec_releaseOutputBuffer(idx, false)` + `stop` + `delete`.
//!
//! Pixel formats handled:
//!
//! - `COLOR_FormatYUV420SemiPlanar` (NV12 / NV21): Y plane + interleaved
//!   UV. Most common on hardware decoders. Cb/Cr ordering depends on
//!   `KEY_STRIDE` / `KEY_SLICE_HEIGHT` and color-format-specific
//!   conventions; we follow the NV12 (Cb before Cr) layout reported by
//!   modern devices.
//! - `COLOR_FormatYUV420Planar` (I420): Y plane + U plane + V plane.
//!   Software decoder default.
//! - `COLOR_FormatYUVP010` (P010, 10-bit): u16 samples, MSB-aligned.
//!
//! For everything else we surface `BackendError::Decode("unsupported
//! color format: ...")` so the dispatcher can fall through.

#![cfg(target_os = "android")]

use core::ffi::CStr;
use core::ptr::{self, NonNull};
use std::vec;
use std::vec::Vec;

use heic_core::{BackendError, DecodedFrame, HvccParams};
use ndk_sys as ndk;

// ── Public color-format identifiers (MediaCodec OMX color formats) ────────
//
// These constants are mirrored from
// `android.media.MediaCodecInfo.CodecCapabilities` and OMX_COLOR_FORMATTYPE.
// NDK headers expose them as macros, but ndk-sys 0.6 doesn't re-export them
// — declare locally per the docs.

const COLOR_FORMAT_YUV420_PLANAR: i32 = 19;
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F420888;
const COLOR_FORMAT_YUV_P010: i32 = 54;

// Buffer flags
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;

// dequeueOutputBuffer special return values
const INFO_TRY_AGAIN_LATER: isize = -1;
const INFO_OUTPUT_FORMAT_CHANGED: isize = -2;
const INFO_OUTPUT_BUFFERS_CHANGED: isize = -3;

#[derive(Default)]
pub(super) struct Inner {
    /// Cached codec + format keyed on (width, height, bit_depth). Rebuilt
    /// when the next HvccParams doesn't match.
    cached: Option<Cached>,
}

struct Cached {
    width: u32,
    height: u32,
    bit_depth: u8,
    codec: NonNull<ndk::AMediaCodec>,
}

// SAFETY: AMediaCodec is documented safe to use across threads when the
// instance is owned exclusively — the wrapper enforces single ownership.
unsafe impl Send for Cached {}

impl Drop for Cached {
    fn drop(&mut self) {
        // SAFETY: codec was created by AMediaCodec_createDecoderByType and
        // never released elsewhere. Stop is documented to succeed even on
        // already-stopped codecs.
        unsafe {
            ndk::AMediaCodec_stop(self.codec.as_ptr());
            ndk::AMediaCodec_delete(self.codec.as_ptr());
        }
    }
}

impl Inner {
    pub(super) fn decode(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        let coded_w = config.coded_width.max(config.width);
        let coded_h = config.coded_height.max(config.height);
        let bit_depth = config.bit_depth_luma;

        let needs_reconfig = self
            .cached
            .as_ref()
            .is_none_or(|c| c.width != coded_w || c.height != coded_h || c.bit_depth != bit_depth);
        if needs_reconfig {
            self.cached = None; // drop the previous codec before creating a new one
            self.cached = Some(build_codec(config, coded_w, coded_h, bit_depth)?);
        }

        let cached = self.cached.as_mut().expect("cached set above");
        decode_one_frame(
            cached, config, image_data, coded_w, coded_h, bit_depth, stop,
        )
    }
}

fn build_codec(
    config: &HvccParams<'_>,
    coded_w: u32,
    coded_h: u32,
    bit_depth: u8,
) -> Result<Cached, BackendError> {
    let mime = c"video/hevc";
    // SAFETY: mime is a valid NUL-terminated C string; the NDK copies
    // the bytes internally.
    let codec = NonNull::new(unsafe { ndk::AMediaCodec_createDecoderByType(mime.as_ptr()) })
        .ok_or(BackendError::Unavailable(
            "AMediaCodec_createDecoderByType(\"video/hevc\") returned null",
        ))?;

    // Build the Annex-B CSD-0 (VPS+SPS+PPS).
    let csd0 = heic_core::nal::annexb_parameter_sets(config.nal_units);
    let format = build_format(coded_w, coded_h, bit_depth, &csd0)?;
    // SAFETY: codec + format are both valid handles. configure copies the
    // format's contents; we delete it after the call.
    let status = unsafe {
        ndk::AMediaCodec_configure(
            codec.as_ptr(),
            format.as_ptr(),
            ptr::null_mut(), // ANativeWindow surface — null = ByteBuffer mode
            ptr::null_mut(), // AMediaCrypto — none
            0,               // flags
        )
    };
    // SAFETY: format was created by AMediaFormat_new; the wrapper drops it
    // by calling AMediaFormat_delete in the FormatHandle Drop.
    let _ = format; // explicit ack of RAII drop
    if status != ndk::media_status_t::AMEDIA_OK {
        // SAFETY: codec is alive; delete it before returning the error.
        unsafe { ndk::AMediaCodec_delete(codec.as_ptr()) };
        return Err(BackendError::Decode(format!(
            "AMediaCodec_configure failed: media_status {status:?}"
        )));
    }

    // SAFETY: codec is valid + configured.
    let status = unsafe { ndk::AMediaCodec_start(codec.as_ptr()) };
    if status != ndk::media_status_t::AMEDIA_OK {
        // SAFETY: codec was created but failed to start; delete it.
        unsafe { ndk::AMediaCodec_delete(codec.as_ptr()) };
        return Err(BackendError::Decode(format!(
            "AMediaCodec_start failed: media_status {status:?}"
        )));
    }

    Ok(Cached {
        width: coded_w,
        height: coded_h,
        bit_depth,
        codec,
    })
}

/// RAII wrapper around `AMediaFormat`.
struct FormatHandle(NonNull<ndk::AMediaFormat>);

impl FormatHandle {
    fn as_ptr(&self) -> *mut ndk::AMediaFormat {
        self.0.as_ptr()
    }
}

impl Drop for FormatHandle {
    fn drop(&mut self) {
        // SAFETY: format was created by AMediaFormat_new.
        unsafe {
            ndk::AMediaFormat_delete(self.0.as_ptr());
        }
    }
}

fn build_format(
    coded_w: u32,
    coded_h: u32,
    bit_depth: u8,
    csd0: &[u8],
) -> Result<FormatHandle, BackendError> {
    // SAFETY: AMediaFormat_new always returns a valid pointer or null on OOM.
    let format = NonNull::new(unsafe { ndk::AMediaFormat_new() }).ok_or(BackendError::Decode(
        "AMediaFormat_new returned null".into(),
    ))?;
    let handle = FormatHandle(format);

    let mime = c"video/hevc";

    // SAFETY: All AMediaFormat_set* take valid C strings + the format
    // pointer; KEY_* statics are initialized by the NDK loader.
    unsafe {
        ndk::AMediaFormat_setString(handle.as_ptr(), ndk::AMEDIAFORMAT_KEY_MIME, mime.as_ptr());
        ndk::AMediaFormat_setInt32(handle.as_ptr(), ndk::AMEDIAFORMAT_KEY_WIDTH, coded_w as i32);
        ndk::AMediaFormat_setInt32(
            handle.as_ptr(),
            ndk::AMEDIAFORMAT_KEY_HEIGHT,
            coded_h as i32,
        );
        ndk::AMediaFormat_setBuffer(
            handle.as_ptr(),
            ndk::AMEDIAFORMAT_KEY_CSD_0,
            csd0.as_ptr().cast(),
            csd0.len(),
        );
    }
    let _ = bit_depth; // KEY_COLOR_FORMAT is informational on input; output format from codec
    Ok(handle)
}

#[allow(clippy::too_many_arguments)]
fn decode_one_frame(
    cached: &mut Cached,
    config: &HvccParams<'_>,
    image_data: &[u8],
    coded_w: u32,
    coded_h: u32,
    bit_depth: u8,
    stop: &dyn enough::Stop,
) -> Result<DecodedFrame, BackendError> {
    let codec = cached.codec;

    // Honour cancellation BEFORE any FFI work — the input dequeue
    // can block for 100ms on a busy decoder, and we don't want
    // outer timeouts to be eaten by a hung first call.
    if stop.should_stop() {
        return Err(BackendError::Cancelled);
    }

    // Convert hvcC length-prefixed slices to Annex B for the input buffer.
    let slice_annexb = heic_core::nal::hvcc_to_annexb(image_data, config.length_size).ok_or(
        BackendError::Decode("malformed hvcC length-prefixed slice data".into()),
    )?;

    // ── Input ──────────────────────────────────────────────────────────
    // dequeueInputBuffer with 100ms timeout — image decode is a one-shot
    // submit, so a single attempt is enough for any real decoder.
    // SAFETY: codec is a live AMediaCodec handle.
    let in_idx = unsafe { ndk::AMediaCodec_dequeueInputBuffer(codec.as_ptr(), 100_000) };
    if in_idx < 0 {
        return Err(BackendError::Decode(format!(
            "AMediaCodec_dequeueInputBuffer failed: idx={in_idx}"
        )));
    }
    let mut buf_size: usize = 0;
    // SAFETY: in_idx is a valid input buffer index returned above.
    let buf_ptr =
        unsafe { ndk::AMediaCodec_getInputBuffer(codec.as_ptr(), in_idx as usize, &mut buf_size) };
    if buf_ptr.is_null() || slice_annexb.len() > buf_size {
        return Err(BackendError::Decode(format!(
            "AMediaCodec_getInputBuffer null or undersized: ptr={buf_ptr:?} size={buf_size} need={}",
            slice_annexb.len()
        )));
    }
    // SAFETY: buf_ptr is valid for buf_size bytes per the API contract.
    unsafe {
        ptr::copy_nonoverlapping(slice_annexb.as_ptr(), buf_ptr, slice_annexb.len());
    }
    // SAFETY: in_idx still valid; we're queueing the buffer back with EOS.
    let status = unsafe {
        ndk::AMediaCodec_queueInputBuffer(
            codec.as_ptr(),
            in_idx as usize,
            0,
            slice_annexb.len(),
            0,
            BUFFER_FLAG_END_OF_STREAM,
        )
    };
    if status != ndk::media_status_t::AMEDIA_OK {
        return Err(BackendError::Decode(format!(
            "AMediaCodec_queueInputBuffer failed: {status:?}"
        )));
    }

    // ── Output ─────────────────────────────────────────────────────────
    // Loop on dequeueOutputBuffer handling INFO_OUTPUT_FORMAT_CHANGED and
    // INFO_TRY_AGAIN_LATER. We honor the Stop token between attempts.
    let mut last_color_format: i32 = COLOR_FORMAT_YUV420_FLEXIBLE;
    let mut stride: i32 = coded_w as i32;
    let mut slice_height: i32 = coded_h as i32;
    let mut attempts = 0u32;
    let max_attempts = 200; // ~20s at 100ms/attempt
    loop {
        if stop.should_stop() {
            return Err(BackendError::Cancelled);
        }
        if attempts >= max_attempts {
            return Err(BackendError::Decode("MediaCodec output timeout".into()));
        }
        attempts += 1;

        let mut info: ndk::AMediaCodecBufferInfo = ndk::AMediaCodecBufferInfo {
            offset: 0,
            size: 0,
            presentationTimeUs: 0,
            flags: 0,
        };
        // SAFETY: codec is alive; info is a writable local.
        let idx =
            unsafe { ndk::AMediaCodec_dequeueOutputBuffer(codec.as_ptr(), &mut info, 100_000) };
        match idx {
            INFO_TRY_AGAIN_LATER => continue,
            INFO_OUTPUT_BUFFERS_CHANGED => continue,
            INFO_OUTPUT_FORMAT_CHANGED => {
                // SAFETY: codec is alive.
                let fmt = unsafe { ndk::AMediaCodec_getOutputFormat(codec.as_ptr()) };
                if !fmt.is_null() {
                    let format = FormatHandle(NonNull::new(fmt).unwrap());
                    last_color_format =
                        read_int(format.as_ptr(), b"color-format\0").unwrap_or(last_color_format);
                    stride = read_int(format.as_ptr(), b"stride\0").unwrap_or(stride);
                    slice_height =
                        read_int(format.as_ptr(), b"slice-height\0").unwrap_or(slice_height);
                }
                continue;
            }
            i if i < 0 => {
                return Err(BackendError::Decode(format!(
                    "dequeueOutputBuffer error idx={i}"
                )));
            }
            i => {
                let out_idx = i as usize;
                let mut size: usize = 0;
                // SAFETY: out_idx is a valid output buffer index.
                let out_ptr =
                    unsafe { ndk::AMediaCodec_getOutputBuffer(codec.as_ptr(), out_idx, &mut size) };
                if out_ptr.is_null() {
                    return Err(BackendError::Decode("getOutputBuffer returned null".into()));
                }
                // Defensive: NDK promises the slice is `size` bytes
                // valid, but a misbehaving vendor driver could
                // return an undersized buffer that the unpack
                // expects to be at least
                // `slice_height * stride * (1.5)` bytes for 4:2:0
                // (or `* 3` for P010). Check before constructing the
                // slice so a bug surfaces as `BackendError::Decode`
                // instead of an OOB read in unpack_planes.
                let bytes_per_sample = if bit_depth >= 10 { 2usize } else { 1 };
                let expected_min = (slice_height.max(coded_h as i32) as usize)
                    .saturating_mul(stride.max(coded_w as i32) as usize)
                    .saturating_mul(bytes_per_sample)
                    .saturating_mul(3)
                    / 2;
                if size < expected_min {
                    return Err(BackendError::Decode(format!(
                        "MediaCodec output buffer undersized: size={size} expected≥{expected_min} \
                         (stride={stride}, slice_height={slice_height}, bit_depth={bit_depth})"
                    )));
                }
                // SAFETY: out_ptr is valid for `size` bytes per the NDK
                // contract; we just verified `size` is at least the
                // unpack code's minimum requirement.
                let bytes = unsafe { core::slice::from_raw_parts(out_ptr, size) };
                let planes = unpack_planes(
                    bytes,
                    last_color_format,
                    stride.max(coded_w as i32) as u32,
                    slice_height.max(coded_h as i32) as u32,
                    coded_w,
                    coded_h,
                    config.width,
                    config.height,
                    config.crop_left,
                    config.crop_top,
                    bit_depth,
                )?;
                // SAFETY: out_idx is the buffer we just read.
                unsafe { ndk::AMediaCodec_releaseOutputBuffer(codec.as_ptr(), out_idx, false) };

                return Ok(DecodedFrame {
                    width: config.width,
                    height: config.height,
                    y_plane: planes.y,
                    cb_plane: planes.cb,
                    cr_plane: planes.cr,
                    bit_depth,
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
                });
            }
        }
    }
}

fn read_int(format: *mut ndk::AMediaFormat, key: &[u8]) -> Option<i32> {
    let cstr = CStr::from_bytes_with_nul(key).ok()?;
    let mut out: i32 = 0;
    // SAFETY: format + cstr.as_ptr() + &mut out are all valid pointers
    // per the API contract; the function fills `out` only on success.
    let found = unsafe { ndk::AMediaFormat_getInt32(format, cstr.as_ptr(), &mut out) };
    if found { Some(out) } else { None }
}

struct OutputPlanes {
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
}

#[allow(clippy::too_many_arguments)]
fn unpack_planes(
    bytes: &[u8],
    color_format: i32,
    stride: u32,
    slice_height: u32,
    coded_w: u32,
    coded_h: u32,
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    let s = stride as usize;
    let sh = slice_height as usize;
    let _ = coded_w;
    let _ = coded_h;
    let w = visible_w as usize;
    let h = visible_h as usize;
    let cx = crop_x as usize;
    let cy = crop_y as usize;
    let half_w = w / 2;
    let half_h = h / 2;
    let chroma_cx = cx / 2;
    let chroma_cy = cy / 2;
    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    match color_format {
        COLOR_FORMAT_YUV420_PLANAR => {
            // I420 / YV12: Y plane, then U, then V (Cb, Cr separate).
            // Stride applies to Y; chroma stride = stride / 2.
            for row in 0..h {
                let src = (cy + row) * s + cx;
                for col in 0..w {
                    y_plane[row * w + col] = u16::from(bytes[src + col]);
                }
            }
            let chroma_stride = s / 2;
            let u_base = s * sh;
            let v_base = u_base + chroma_stride * (sh / 2);
            for row in 0..half_h {
                let src_u = u_base + (chroma_cy + row) * chroma_stride + chroma_cx;
                let src_v = v_base + (chroma_cy + row) * chroma_stride + chroma_cx;
                for col in 0..half_w {
                    cb_plane[row * half_w + col] = u16::from(bytes[src_u + col]);
                    cr_plane[row * half_w + col] = u16::from(bytes[src_v + col]);
                }
            }
        }
        COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_FLEXIBLE => {
            // NV12: Y plane, then interleaved UV at same stride. Treat
            // FLEXIBLE as NV12 since modern devices return NV12 for it.
            for row in 0..h {
                let src = (cy + row) * s + cx;
                for col in 0..w {
                    y_plane[row * w + col] = u16::from(bytes[src + col]);
                }
            }
            let uv_base = s * sh;
            for row in 0..half_h {
                let src = uv_base + (chroma_cy + row) * s + chroma_cx * 2;
                for col in 0..half_w {
                    cb_plane[row * half_w + col] = u16::from(bytes[src + col * 2]);
                    cr_plane[row * half_w + col] = u16::from(bytes[src + col * 2 + 1]);
                }
            }
        }
        COLOR_FORMAT_YUV_P010 => {
            // P010: u16 LE, 10-bit value MSB-aligned within the u16
            // (bits 15..6 carry the sample; bits 5..0 are zero).
            // Earlier code masked with `& 0x3FF` which extracted the
            // ZERO low bits — producing garbled output instead of the
            // intended 10-bit sample. `>> 6` is the correct extraction,
            // matching the Windows MF / D3D11VA / VT P010 paths.
            for row in 0..h {
                let src = (cy + row) * s + cx * 2;
                for col in 0..w {
                    let off = src + col * 2;
                    let lo = bytes[off];
                    let hi = bytes[off + 1];
                    y_plane[row * w + col] = ((u16::from(hi) << 8) | u16::from(lo)) >> 6;
                }
            }
            let uv_base = s * sh;
            for row in 0..half_h {
                let src = uv_base + (chroma_cy + row) * s + chroma_cx * 4;
                for col in 0..half_w {
                    let off = src + col * 4;
                    let cb = (u16::from(bytes[off + 1]) << 8) | u16::from(bytes[off]);
                    let cr = (u16::from(bytes[off + 3]) << 8) | u16::from(bytes[off + 2]);
                    cb_plane[row * half_w + col] = cb >> 6;
                    cr_plane[row * half_w + col] = cr >> 6;
                }
            }
        }
        other => {
            return Err(BackendError::Decode(format!(
                "MediaCodec returned unsupported color format: {other:#x}"
            )));
        }
    }
    let _ = bit_depth; // bit_depth informs callers; per-format path above already uses it
    Ok(OutputPlanes {
        y: y_plane,
        cb: cb_plane,
        cr: cr_plane,
    })
}
