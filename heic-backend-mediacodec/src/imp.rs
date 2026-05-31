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
//! Pixel formats handled (chroma byte order resolved per color-format key):
//!
//! - `COLOR_FormatYUV420SemiPlanar` (NV12): Y plane + interleaved UV with
//!   Cb before Cr — the OMX contract for this constant. Most common on
//!   hardware decoders.
//! - Vendor NV21 semi-planar (`OMX_QCOM_COLOR_FormatYVU420SemiPlanar`,
//!   `OMX_SEC_COLOR_FormatNV21Linear`): interleaved UV with Cr before Cb;
//!   unpacked with the chroma byte order swapped relative to NV12.
//! - `COLOR_FormatYUV420Planar` / `…PackedPlanar` (I420): Y plane + U plane
//!   + V plane. Software decoder default.
//! - `COLOR_FormatYUVP010` (P010, 10-bit): u16 samples, MSB-aligned.
//!
//! `COLOR_FormatYUV420Flexible` is **rejected** in this ByteBuffer path:
//! its concrete chroma byte order (NV12 vs NV21 vs planar) is unspecified
//! without the AImage plane API (`AMediaCodec_getOutputImage`), which
//! ndk-sys 0.6 does not expose. Guessing NV12 risks a red/blue swap on
//! devices that back FLEXIBLE with NV21, so we fall through instead.
//!
//! For every other (unrecognized) color format we surface
//! `BackendError::Decode(...)` so the dispatcher can fall through to the
//! pure-Rust backend.

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
const COLOR_FORMAT_YUV420_PACKED_PLANAR: i32 = 20;
const COLOR_FORMAT_YUV420_SEMI_PLANAR: i32 = 21;
const COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR: i32 = 39;
const COLOR_FORMAT_YUV420_FLEXIBLE: i32 = 0x7F42_0888;
const COLOR_FORMAT_YUV_P010: i32 = 54;

// Vendor NV21 (Cr-before-Cb interleaved) semi-planar formats. Unlike the
// generic OMX `SemiPlanar` constant (which is NV12 / Cb-before-Cr by the
// OMX contract), these report a reversed chroma byte order. Treating them
// as NV12 swaps Cb/Cr → red/blue swap. We map the well-known vendor values
// to an explicit NV21 unpack; anything else semi-planar we cannot prove the
// ordering of from a raw ByteBuffer is rejected below.
//
// `OMX_QCOM_COLOR_FormatYVU420SemiPlanar` (Qualcomm) and
// `OMX_SEC_COLOR_FormatNV21Linear` (Samsung).
const COLOR_FORMAT_QCOM_YVU420_SEMI_PLANAR: i32 = 0x7FA3_0C00u32 as i32;
const COLOR_FORMAT_SEC_NV21_LINEAR: i32 = 0x7F00_0001;

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

    // The MediaCodec ByteBuffer output is ALWAYS 4:2:0 (NV12 / I420 / P010)
    // — there is no 4:2:2 / 4:4:4 ByteBuffer layout in this path, and the
    // unpack code below produces a half-width/half-height chroma plane
    // unconditionally. Tagging the returned frame with the source
    // `chroma_format_idc` for a non-4:2:0 source would mislabel a 4:2:0
    // buffer as 4:2:2/4:4:4 and corrupt the chroma upsampling. Reject
    // non-4:2:0 sources here so the dispatcher falls through to the
    // pure-Rust backend, which decodes 4:2:2/4:4:4 correctly.
    if config.chroma_format_idc != 1 {
        return Err(BackendError::Decode(format!(
            "MediaCodec ByteBuffer output is 4:2:0 only; source chroma_format_idc={} \
             needs the pure-Rust backend",
            config.chroma_format_idc
        )));
    }

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
                // SAFETY: out_ptr is valid for `size` bytes per the NDK
                // contract. unpack_planes validates every plane read against
                // `bytes.len()` (== size), so a vendor driver that returns a
                // short buffer surfaces as BackendError::Decode, not an OOB —
                // no separate (and previously WRONG) size pre-check needed.
                let bytes = unsafe { core::slice::from_raw_parts(out_ptr, size) };
                // Pass the decoder's REPORTED buffer geometry (stride /
                // slice_height) — NOT max()'d up to the SPS coded size. When the
                // decoder crops to the visible region it reports
                // slice_height < coded_h (example.heic: coded 856 -> emitted
                // 854 rows); demanding coded_h wrongly rejected that legitimate
                // buffer as "undersized". unpack_planes handles the
                // already-cropped-vs-coded distinction.
                let planes = unpack_planes(
                    bytes,
                    last_color_format,
                    stride.max(1) as u32,
                    slice_height.max(1) as u32,
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
                    // Always 4:2:0: the ByteBuffer unpack produces a
                    // half-width/half-height chroma plane, and non-4:2:0
                    // sources were rejected above. Tag literally as 1 so the
                    // frame's chroma_format matches the actual plane layout.
                    chroma_format: 1,
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
    visible_w: u32,
    visible_h: u32,
    crop_x: u32,
    crop_y: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    let s = stride as usize;
    let sh = slice_height as usize;
    let w = visible_w as usize;
    let h = visible_h as usize;

    // ── Geometry validation (untrusted SPS crop + HEIF `ispe`) ───────────
    // The visible region (`width`/`height`), the crop offsets (SPS conformance
    // window), and the decoder's reported `stride`/`slice_height` come from
    // three independent sources in a `.heic`.
    if w == 0 || h == 0 {
        return Err(BackendError::Decode(
            "MediaCodec: zero visible dimension".into(),
        ));
    }

    // MediaCodec may emit either the already-cropped (visible) buffer OR the
    // full coded buffer. If applying the SPS crop offset would run past the
    // buffer's own rows/cols, the decoder already cropped — so the visible
    // image starts at the buffer origin and the offset is 0; otherwise the
    // buffer holds the coded frame and we apply the crop. This makes
    // example.heic (coded 856 -> decoder emits 854 visible rows, slice_height
    // 854) read correctly instead of being rejected or reading shifted pixels.
    // A crop that still doesn't fit after zeroing is genuinely inconsistent
    // geometry (crafted SPS + mismatched ispe) -> reject, and the dispatcher
    // falls through to the pure-Rust backend.
    let cx = if (crop_x as usize).checked_add(w).is_some_and(|r| r <= s) {
        crop_x as usize
    } else {
        0
    };
    let cy = if (crop_y as usize).checked_add(h).is_some_and(|r| r <= sh) {
        crop_y as usize
    } else {
        0
    };
    if cx + w > s || cy + h > sh {
        return Err(BackendError::Decode(format!(
            "MediaCodec: visible region {w}x{h} at ({cx},{cy}) exceeds output buffer {s}x{sh}"
        )));
    }

    let half_w = w / 2;
    let half_h = h / 2;
    let chroma_cx = cx / 2;
    let chroma_cy = cy / 2;

    // Maximal byte index a plane read touches; checked so crafted strides /
    // crops can't wrap. Each closure returns the *last* index (inclusive) for
    // the given access pattern; we require it to be `< bytes.len()`.
    let buf_len = bytes.len();
    // Y plane (8-bit): bytes_per_px = 1; (10-bit P010): 2.
    let y_last = |bpp: usize| -> Option<usize> {
        // (cy + h - 1) * s + (cx + w - 1) * bpp + (bpp - 1)
        (cy + h - 1)
            .checked_mul(s)?
            .checked_add((cx + w - 1).checked_mul(bpp)?)?
            .checked_add(bpp - 1)
    };
    // Semi-planar chroma (NV12 / NV21 / P010 interleaved): base + last UV pair.
    // `unit` = bytes per (Cb,Cr) sample-pair element (2 for 8-bit NV12/NV21,
    // 4 for P010); `hi` = highest byte offset within the last unit touched.
    // Returns `Some(0)` (a no-op index) when there is no chroma to read so the
    // caller's `>= buf_len` check passes trivially for empty chroma planes.
    let uv_semi_last = |unit: usize, hi: usize| -> Option<usize> {
        if half_w == 0 || half_h == 0 {
            return Some(0);
        }
        let uv_base = s.checked_mul(sh)?;
        uv_base
            .checked_add((chroma_cy + half_h - 1).checked_mul(s)?)?
            .checked_add(chroma_cx.checked_mul(unit)?)?
            .checked_add((half_w - 1).checked_mul(unit)?)?
            .checked_add(hi)
    };

    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    // Helper to surface a uniform OOB error.
    let oob = |what: &str| {
        BackendError::Decode(format!(
            "MediaCodec: {what} read would exceed output buffer (len={buf_len})"
        ))
    };

    match color_format {
        COLOR_FORMAT_YUV420_PLANAR | COLOR_FORMAT_YUV420_PACKED_PLANAR => {
            // I420: Y plane, then U, then V (Cb, Cr separate).
            // Stride applies to Y; chroma stride = stride / 2.
            let chroma_stride = s / 2;
            let u_base = s.checked_mul(sh).ok_or_else(|| oob("planar Y"))?;
            let v_base = u_base
                .checked_add(
                    chroma_stride
                        .checked_mul(sh / 2)
                        .ok_or_else(|| oob("planar U"))?,
                )
                .ok_or_else(|| oob("planar U"))?;
            // Bounds: Y last byte, and V plane last byte (>= U plane). When
            // there is no chroma (half_w/half_h == 0) only the Y bound matters.
            let y_max = y_last(1).ok_or_else(|| oob("planar Y"))?;
            let v_max = if half_w == 0 || half_h == 0 {
                0
            } else {
                v_base
                    .checked_add(
                        (chroma_cy + half_h - 1)
                            .checked_mul(chroma_stride)
                            .ok_or_else(|| oob("planar V"))?,
                    )
                    .and_then(|x| x.checked_add(chroma_cx + half_w - 1))
                    .ok_or_else(|| oob("planar V"))?
            };
            if y_max >= buf_len || v_max >= buf_len {
                return Err(oob("planar"));
            }
            for row in 0..h {
                let src = (cy + row) * s + cx;
                for col in 0..w {
                    y_plane[row * w + col] = u16::from(bytes[src + col]);
                }
            }
            for row in 0..half_h {
                let src_u = u_base + (chroma_cy + row) * chroma_stride + chroma_cx;
                let src_v = v_base + (chroma_cy + row) * chroma_stride + chroma_cx;
                for col in 0..half_w {
                    cb_plane[row * half_w + col] = u16::from(bytes[src_u + col]);
                    cr_plane[row * half_w + col] = u16::from(bytes[src_v + col]);
                }
            }
        }
        COLOR_FORMAT_YUV420_SEMI_PLANAR | COLOR_FORMAT_YUV420_PACKED_SEMI_PLANAR => {
            // NV12: Y plane, then interleaved UV at same stride, Cb before Cr.
            let y_max = y_last(1).ok_or_else(|| oob("NV12 Y"))?;
            let uv_max = uv_semi_last(2, 1).ok_or_else(|| oob("NV12 UV"))?;
            if y_max >= buf_len || uv_max >= buf_len {
                return Err(oob("NV12"));
            }
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
        COLOR_FORMAT_QCOM_YVU420_SEMI_PLANAR | COLOR_FORMAT_SEC_NV21_LINEAR => {
            // NV21: Y plane, then interleaved VU at same stride — Cr BEFORE
            // Cb. Identical layout to NV12 except the two chroma bytes are
            // swapped; reading it as NV12 would swap Cb/Cr → red/blue swap.
            let y_max = y_last(1).ok_or_else(|| oob("NV21 Y"))?;
            let uv_max = uv_semi_last(2, 1).ok_or_else(|| oob("NV21 UV"))?;
            if y_max >= buf_len || uv_max >= buf_len {
                return Err(oob("NV21"));
            }
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
                    // NV21: Cr byte first, then Cb.
                    cr_plane[row * half_w + col] = u16::from(bytes[src + col * 2]);
                    cb_plane[row * half_w + col] = u16::from(bytes[src + col * 2 + 1]);
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
            let y_max = y_last(2).ok_or_else(|| oob("P010 Y"))?;
            let uv_max = uv_semi_last(4, 3).ok_or_else(|| oob("P010 UV"))?;
            if y_max >= buf_len || uv_max >= buf_len {
                return Err(oob("P010"));
            }
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
        COLOR_FORMAT_YUV420_FLEXIBLE => {
            // FLEXIBLE's concrete chroma byte order (NV12 / NV21 / planar) is
            // unspecified in this ByteBuffer path and can only be resolved via
            // the AImage plane API (`AMediaCodec_getOutputImage`), which
            // ndk-sys 0.6 does not expose. Guessing NV12 risks a red/blue swap
            // on devices that back FLEXIBLE with NV21, so fall through to the
            // pure-Rust backend instead of producing wrong pixels.
            return Err(BackendError::Decode(
                "MediaCodec returned COLOR_FormatYUV420Flexible; chroma byte order \
                 is undeterminable from a raw ByteBuffer"
                    .into(),
            ));
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
