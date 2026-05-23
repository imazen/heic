//! Apple-only VideoToolbox HEVC decoder implementation.
//!
//! Compile-gated by the apple `target_os` set from the parent module.
//! Uses `objc2-video-toolbox` and `objc2-core-media` bindings to drive
//! `VTDecompressionSession`; every `unsafe` block carries a `SAFETY:`
//! comment justifying the underlying Core Foundation / Core Media
//! contract being upheld.
//!
//! Pipeline (per `decode_hevc` call):
//!
//! 1. `CMVideoFormatDescriptionCreateFromHEVCParameterSets` from the
//!    VPS / SPS / PPS NAL payloads in `HvccParams.nal_units`. Cached on
//!    `Inner` across subsequent decodes with matching dimensions /
//!    bit-depth so the second tile of a grid skips the rebuild.
//! 2. `VTDecompressionSessionCreate` with a synchronous output callback
//!    that captures the produced `CVPixelBuffer` into a thread-local
//!    slot (we drive decode synchronously per the
//!    `VTDecompressionSession::create` contract — passing
//!    `kVTDecodeFrame_EnableAsynchronousDecompression = 0` makes the
//!    callback fire before `DecodeFrame` returns).
//! 3. Wrap `image_data` (already hvcC length-prefixed slice NALs) in a
//!    `CMBlockBuffer` via `CMBlockBufferCreateWithMemoryBlock`, then a
//!    `CMSampleBuffer` via `CMSampleBufferCreate`.
//! 4. `VTDecompressionSessionDecodeFrame` → callback fires →
//!    `VTDecompressionSessionWaitForAsynchronousFrames` as a defensive
//!    fence per the validation pass (some sessions queue work even in
//!    sync mode).
//! 5. Lock the captured `CVPixelBuffer`, copy NV12 / P010 planes into
//!    `Vec<u16>` planes, build the [`DecodedFrame`].

#![cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "tvos",
    target_os = "visionos"
))]

use std::ffi::c_void;
use std::ptr::{self, NonNull};
use std::vec;
use std::vec::Vec;

use heic_core::{BackendError, DecodedFrame, HvccParams};

use objc2_core_foundation::{CFDictionary, CFNumber, CFNumberType, CFRetained, kCFAllocatorNull};
use objc2_core_media::{
    CMBlockBuffer, CMFormatDescription, CMSampleBuffer, CMSampleTimingInfo, CMTime, CMTimeFlags,
    CMVideoFormatDescriptionCreateFromHEVCParameterSets,
};
use objc2_core_video::{
    CVImageBuffer, CVPixelBuffer, CVPixelBufferLockFlags, kCVPixelBufferIOSurfacePropertiesKey,
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
    kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange,
};
use objc2_video_toolbox::{
    VTDecodeFrameFlags, VTDecodeInfoFlags, VTDecompressionOutputCallbackRecord,
    VTDecompressionSession,
};

/// Output state captured by the VT decode callback.
///
/// Lives on the calling thread's stack across one `decode_one_frame`
/// invocation; the callback receives a `*mut FrameOutput` via VT's
/// per-frame `source_frame_refcon`, writes to it, and the caller
/// reads the populated state once `decode_frame` returns and the
/// `wait_for_asynchronous_frames` fence drains any queued work.
///
/// Routing through `source_frame_refcon` rather than a `thread_local!`
/// avoids the case where VT schedules the callback on a different
/// thread than the one calling `decode_frame` — the earlier
/// thread-local approach lost the captured pixel buffer in that case.
struct FrameOutput {
    status: i32,
    pixel_buf: Option<CFRetained<CVPixelBuffer>>,
}

#[derive(Default)]
pub(super) struct Inner {
    /// Cached `VTDecompressionSession` + the format description it was
    /// built from. Rebuilt when the next HvccParams has different
    /// dimensions / bit depth than the cached one.
    cached: Option<Cached>,
}

struct Cached {
    width: u32,
    height: u32,
    bit_depth: u8,
    session: CFRetained<VTDecompressionSession>,
    format_desc: CFRetained<CMFormatDescription>,
}

// SAFETY: see VideoToolboxBackend's unsafe impl Send — CF types here are
// reference-counted and the underlying VT session is thread-safe.
unsafe impl Send for Cached {}
// SAFETY: same as Cached above — Inner holds only Send-safe components.
unsafe impl Send for Inner {}

impl Inner {
    pub(super) fn decode(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        _stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        let width = config.width;
        let height = config.height;
        let bit_depth = config.bit_depth_luma;

        if self
            .cached
            .as_ref()
            .is_none_or(|c| c.width != width || c.height != height || c.bit_depth != bit_depth)
        {
            self.cached = Some(build_cached(config)?);
        }
        let cached = self.cached.as_ref().expect("cached set above");

        decode_one_frame(cached, config, image_data)
    }
}

/// Probe VideoToolbox HEVC decode availability. Returns true on every
/// shipping macOS 10.13+ / iOS 11+ / tvOS 11+ / visionOS 1+ — VT is
/// always present on apple targets; the function exists so future
/// hardware-only checks have a place to live.
pub(super) fn is_available() -> bool {
    // VT is always available on apple targets we compile for. Returning
    // true blindly is fine — the dispatcher's fallthrough catches the
    // (extremely rare) case where DecodeFrame fails on an unsupported
    // bitstream.
    true
}

fn build_cached(config: &HvccParams<'_>) -> Result<Cached, BackendError> {
    let format_desc = build_format_description(config)?;
    let session = build_session(&format_desc, config)?;
    Ok(Cached {
        width: config.width,
        height: config.height,
        bit_depth: config.bit_depth_luma,
        session,
        format_desc,
    })
}

fn build_format_description(
    config: &HvccParams<'_>,
) -> Result<CFRetained<CMFormatDescription>, BackendError> {
    if config.nal_units.is_empty() {
        return Err(BackendError::Decode(
            "no parameter set NALs in HvccParams".into(),
        ));
    }
    // CMVideoFormatDescriptionCreateFromHEVCParameterSets takes a parallel
    // array of pointers + sizes; build them on the stack.
    let mut pointers: Vec<NonNull<u8>> = Vec::with_capacity(config.nal_units.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(config.nal_units.len());
    for nal in config.nal_units {
        if nal.is_empty() {
            continue;
        }
        // SAFETY: nal is a non-empty slice; `as_ptr` is non-null and we
        // immediately use a NonNull wrapper. The pointer is read-only
        // for VT but the CM API signature uses *mut u8 — cast is sound
        // because VT promises not to write through it.
        pointers.push(unsafe { NonNull::new_unchecked(nal.as_ptr() as *mut u8) });
        sizes.push(nal.len());
    }
    if pointers.is_empty() {
        return Err(BackendError::Decode("all parameter set NALs empty".into()));
    }

    let mut format_desc: *const CMFormatDescription = ptr::null();
    // SAFETY: pointers / sizes both have the same length; nal_unit_header_length=4
    // matches hvcC standard. We pass valid non-null out-pointer for
    // format_description_out. extensions=None is documented as accepted.
    let status = unsafe {
        CMVideoFormatDescriptionCreateFromHEVCParameterSets(
            None,
            pointers.len(),
            NonNull::new_unchecked(pointers.as_mut_ptr()),
            NonNull::new_unchecked(sizes.as_mut_ptr()),
            i32::from(config.length_size),
            None,
            NonNull::new_unchecked(&raw mut format_desc),
        )
    };
    if status != 0 {
        return Err(BackendError::Decode(format!(
            "CMVideoFormatDescriptionCreateFromHEVCParameterSets failed: OSStatus {status}"
        )));
    }
    let format_desc = NonNull::new(format_desc.cast_mut()).ok_or_else(|| {
        BackendError::Decode(
            "CMVideoFormatDescriptionCreateFromHEVCParameterSets returned null".into(),
        )
    })?;
    // SAFETY: status == 0 and pointer is non-null per the API contract;
    // CFRetained takes ownership of the +1 refcount returned by the
    // *Create* function (Create Rule).
    Ok(unsafe { CFRetained::from_raw(format_desc) })
}

fn build_session(
    format_desc: &CMFormatDescription,
    config: &HvccParams<'_>,
) -> Result<CFRetained<VTDecompressionSession>, BackendError> {
    // Destination pixel buffer attributes: ask VT for NV12 (8-bit) or
    // P010 (10-bit) in video range. Honoring the bitstream's
    // video_full_range_flag at YCbCr→RGB time means we don't ask VT to
    // transcode here.
    let pixel_fmt = if config.bit_depth_luma >= 10 {
        kCVPixelFormatType_420YpCbCr10BiPlanarVideoRange
    } else {
        kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    };
    let dest_attrs = build_destination_attrs(pixel_fmt)?;

    let callback_record = VTDecompressionOutputCallbackRecord {
        decompressionOutputCallback: Some(decode_callback),
        decompressionOutputRefCon: ptr::null_mut(),
    };

    let mut session: *mut VTDecompressionSession = ptr::null_mut();
    // SAFETY: VTDecompressionSession::create is the documented session
    // constructor; format_desc is a live CFRetained; dest_attrs is a
    // CFDictionary we hold; callback_record points to a valid struct
    // for the lifetime of this call (VT copies it).
    // Up-cast from CFRetained<CFDictionary<CFString, CFType>> to &CFDictionary
    // (the generic-erased form VT expects). CFDictionary<K, V> impls
    // AsRef<CFDictionary> via the cf_type! macro.
    let dest_attrs_base: &objc2_core_foundation::CFDictionary =
        <objc2_core_foundation::CFDictionary<
            objc2_core_foundation::CFString,
            objc2_core_foundation::CFType,
        > as AsRef<objc2_core_foundation::CFDictionary>>::as_ref(&dest_attrs);
    // SAFETY: VTDecompressionSession::create is the documented session
    // constructor; format_desc + dest_attrs_base are live CF handles;
    // callback_record points to a valid struct that VT copies; the out
    // pointer is non-null per `NonNull::new_unchecked` of a `&raw mut`.
    let status = unsafe {
        VTDecompressionSession::create(
            None,
            format_desc,
            None,
            Some(dest_attrs_base),
            &callback_record,
            NonNull::new_unchecked(&raw mut session),
        )
    };
    if status != 0 {
        return Err(BackendError::Decode(format!(
            "VTDecompressionSessionCreate failed: OSStatus {status}"
        )));
    }
    let session = NonNull::new(session)
        .ok_or_else(|| BackendError::Decode("VTDecompressionSessionCreate returned null".into()))?;
    // SAFETY: Create Rule transfer of the +1 refcount.
    Ok(unsafe { CFRetained::from_raw(session) })
}

/// Build the destination pixel buffer attributes dictionary requesting
/// the given `pixel_fmt` plus an empty IOSurface properties dict.
///
/// Without `kCVPixelBufferIOSurfacePropertiesKey` the decoder still works
/// on most paths but some hardware variants require it to share buffers
/// across processes. Including an empty dict is the documented "use
/// defaults" form.
fn build_destination_attrs(
    pixel_fmt: u32,
) -> Result<
    CFRetained<CFDictionary<objc2_core_foundation::CFString, objc2_core_foundation::CFType>>,
    BackendError,
> {
    let pixel_fmt_i32: i32 = pixel_fmt as i32;
    // SAFETY: CFNumber::new is the documented constructor; value_ptr
    // points to a live local of the matching CFNumberType.
    let pixel_fmt_num: CFRetained<CFNumber> = unsafe {
        CFNumber::new(
            None,
            CFNumberType::SInt32Type,
            (&raw const pixel_fmt_i32).cast(),
        )
    }
    .ok_or_else(|| BackendError::Decode("CFNumber::new(SInt32) returned null".into()))?;
    let empty_iosurface = empty_dict();

    // SAFETY: extern statics are unsafe by default; both are read-only
    // CFStringRef constants exported by CoreVideo.
    let keys: [&objc2_core_foundation::CFString; 2] = unsafe {
        [
            kCVPixelBufferPixelFormatTypeKey,
            kCVPixelBufferIOSurfacePropertiesKey,
        ]
    };
    // Each CF concrete type impls AsRef<CFType>; deref the CFRetained
    // wrapper down to the concrete type and call `.as_ref()` to upcast.
    let pixel_fmt_ref: &objc2_core_foundation::CFType =
        <CFNumber as AsRef<objc2_core_foundation::CFType>>::as_ref(&pixel_fmt_num);
    let empty_ref: &objc2_core_foundation::CFType =
        <CFDictionary<objc2_core_foundation::CFType, objc2_core_foundation::CFType> as AsRef<
            objc2_core_foundation::CFType,
        >>::as_ref(&empty_iosurface);
    let values: [&objc2_core_foundation::CFType; 2] = [pixel_fmt_ref, empty_ref];
    Ok(CFDictionary::<
        objc2_core_foundation::CFString,
        objc2_core_foundation::CFType,
    >::from_slices(&keys[..], &values[..]))
}

fn empty_dict()
-> CFRetained<CFDictionary<objc2_core_foundation::CFType, objc2_core_foundation::CFType>> {
    CFDictionary::<objc2_core_foundation::CFType, objc2_core_foundation::CFType>::empty()
}

/// VT output callback. VT hands us:
///   * `source_frame_ref_con` — the per-frame refcon we passed to
///     `decode_frame`; we point this at a stack-allocated
///     [`FrameOutput`] in `decode_one_frame`. Writing to it directly
///     (rather than a `thread_local!`) avoids data loss when VT
///     schedules the callback on a worker thread.
///   * `status` — `OSStatus` 0 on success, error code otherwise.
///   * `image_buffer` — the decoded `CVPixelBuffer` (typedef'd as
///     `CVImageBuffer` in CM headers). May be null on failure.
///
/// SAFETY: the caller (`decode_one_frame`) keeps `*source_frame_ref_con`
/// live across the entire `decode_frame` + `wait_for_asynchronous_frames`
/// sequence; image_buffer is borrowed from VT so we CFRetain it.
unsafe extern "C-unwind" fn decode_callback(
    _decompression_output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: i32,
    _info_flags: VTDecodeInfoFlags,
    image_buffer: *mut CVImageBuffer,
    _presentation_time_stamp: CMTime,
    _presentation_duration: CMTime,
) {
    if source_frame_ref_con.is_null() {
        return;
    }
    // SAFETY: the caller passes a `&mut FrameOutput` cast to `*mut c_void`
    // via `source_frame_refcon` in `decode_frame`; this is the same
    // pointer coming back to us, valid for the duration of the call.
    let out = unsafe { &mut *source_frame_ref_con.cast::<FrameOutput>() };
    out.status = status;
    if status != 0 || image_buffer.is_null() {
        return;
    }
    // SAFETY: image_buffer is a CVImageBufferRef borrowed by VT.
    // CVImageBuffer / CVPixelBuffer are documented aliases in
    // CVImageBuffer.h; the cast is the standard VT pattern.
    // CFRetained::retain bumps the refcount so it outlives the
    // callback.
    let pixel_buf =
        unsafe { CFRetained::retain(NonNull::new_unchecked(image_buffer.cast::<CVPixelBuffer>())) };
    out.pixel_buf = Some(pixel_buf);
}

fn decode_one_frame(
    cached: &Cached,
    config: &HvccParams<'_>,
    image_data: &[u8],
) -> Result<DecodedFrame, BackendError> {
    // Build a CMBlockBuffer wrapping the slice NAL bytes. VT will keep
    // a reference to this buffer for the duration of the decode; we keep
    // the source bytes alive via the &[u8] borrow which outlives this
    // function.
    let block_buffer = build_block_buffer(image_data)?;
    let sample_buffer = build_sample_buffer(&block_buffer, &cached.format_desc, image_data.len())?;

    // Per-call output state; the callback writes into it via the
    // source_frame_refcon we pass to decode_frame. Pinned across the
    // decode + wait fence so the callback (which may fire on a VT
    // worker thread) can safely write through the pointer.
    let mut frame_output = FrameOutput {
        status: 0,
        pixel_buf: None,
    };

    // Decode synchronously. Passing 0 for flags (no
    // EnableAsynchronousDecompression) makes VT call the callback before
    // returning per the documented contract; the
    // WaitForAsynchronousFrames below is a defensive fence for the rare
    // session that queues anyway.
    let mut info_flags = VTDecodeInfoFlags::empty();
    // SAFETY: session and sample_buffer are valid CFRetained handles;
    // source_frame_refcon points at `frame_output` which lives through
    // the wait fence below.
    let status = unsafe {
        VTDecompressionSession::decode_frame(
            &cached.session,
            &sample_buffer,
            VTDecodeFrameFlags::empty(),
            (&raw mut frame_output).cast::<c_void>(),
            &raw mut info_flags,
        )
    };
    if status != 0 {
        return Err(BackendError::Decode(format!(
            "VTDecompressionSessionDecodeFrame failed: OSStatus {status}"
        )));
    }

    // Fence on any queued async work.
    // SAFETY: standard VT API; session is alive.
    let wait_status =
        unsafe { VTDecompressionSession::wait_for_asynchronous_frames(&cached.session) };
    if wait_status != 0 {
        return Err(BackendError::Decode(format!(
            "VTDecompressionSessionWaitForAsynchronousFrames failed: OSStatus {wait_status}"
        )));
    }

    if frame_output.status != 0 {
        return Err(BackendError::Decode(format!(
            "VT decode callback reported OSStatus {}",
            frame_output.status
        )));
    }
    let pixel_buf = frame_output
        .pixel_buf
        .take()
        .ok_or_else(|| BackendError::Decode("VT decode produced no pixel buffer".into()))?;

    read_pixel_buffer(&pixel_buf, config)
}

fn build_block_buffer(data: &[u8]) -> Result<CFRetained<CMBlockBuffer>, BackendError> {
    let mut bb: *mut CMBlockBuffer = ptr::null_mut();
    // SAFETY: `block_allocator = kCFAllocatorNull` is the documented
    // "borrowed memory" allocator — CMBlockBuffer wraps the pointer
    // and calls the null allocator's `deallocate` (a no-op) on
    // release. With `None` (which Rust maps to nullptr →
    // kCFAllocatorDefault), the system tries to `free()` our `&[u8]`
    // backing memory on drop → SIGABRT "Non-aligned pointer being
    // freed" because the slice data was never malloc'd.
    //
    // The CM docs explicitly call this out under
    // `CMBlockBufferCreateWithMemoryBlock`:
    //   > "If blockAllocator is kCFAllocatorNull, the memory block
    //   >  will not be deallocated when the buffer is released."
    let null_allocator =
        unsafe { kCFAllocatorNull }.expect("kCFAllocatorNull is statically present");
    let status = unsafe {
        CMBlockBuffer::create_with_memory_block(
            None,
            data.as_ptr() as *mut c_void,
            data.len(),
            Some(null_allocator),
            ptr::null(),
            0,
            data.len(),
            0,
            NonNull::new_unchecked(&raw mut bb),
        )
    };
    if status != 0 {
        return Err(BackendError::Decode(format!(
            "CMBlockBuffer::create_with_memory_block failed: OSStatus {status}"
        )));
    }
    let bb = NonNull::new(bb)
        .ok_or_else(|| BackendError::Decode("CMBlockBuffer::create returned null".into()))?;
    // SAFETY: Create Rule transfer.
    Ok(unsafe { CFRetained::from_raw(bb) })
}

fn build_sample_buffer(
    block_buffer: &CMBlockBuffer,
    format_desc: &CMFormatDescription,
    data_len: usize,
) -> Result<CFRetained<CMSampleBuffer>, BackendError> {
    // Single-sample, single-size buffer with placeholder timing.
    let sizes = [data_len];
    let timing = [CMSampleTimingInfo {
        duration: CMTime {
            value: 1,
            timescale: 60,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        },
        presentationTimeStamp: CMTime {
            value: 0,
            timescale: 60,
            flags: CMTimeFlags::Valid,
            epoch: 0,
        },
        decodeTimeStamp: CMTime {
            value: 0,
            timescale: 60,
            flags: CMTimeFlags::empty(),
            epoch: 0,
        },
    }];

    let mut sb: *mut CMSampleBuffer = ptr::null_mut();
    // SAFETY: data_buffer is already-ready (data_ready=true), no
    // make_data_ready_callback needed; single timing + single size entries
    // both length 1 matching num_samples.
    let status = unsafe {
        CMSampleBuffer::create(
            None,
            Some(block_buffer),
            true,
            None,
            ptr::null_mut(),
            Some(format_desc),
            1,
            1,
            timing.as_ptr(),
            1,
            sizes.as_ptr(),
            NonNull::new_unchecked(&raw mut sb),
        )
    };
    if status != 0 {
        return Err(BackendError::Decode(format!(
            "CMSampleBufferCreate failed: OSStatus {status}"
        )));
    }
    let sb = NonNull::new(sb)
        .ok_or_else(|| BackendError::Decode("CMSampleBufferCreate returned null".into()))?;
    // SAFETY: Create Rule transfer.
    Ok(unsafe { CFRetained::from_raw(sb) })
}

fn read_pixel_buffer(
    pixel_buf: &CVPixelBuffer,
    config: &HvccParams<'_>,
) -> Result<DecodedFrame, BackendError> {
    use objc2_core_video::{
        CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
        CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress,
        CVPixelBufferUnlockBaseAddress,
    };

    // SAFETY: locking with read-only flag is the documented way to access
    // pixel data; the Unlock pairs at the end of this function.
    let lock_status =
        unsafe { CVPixelBufferLockBaseAddress(pixel_buf, CVPixelBufferLockFlags::ReadOnly) };
    if lock_status != 0 {
        return Err(BackendError::Decode(format!(
            "CVPixelBufferLockBaseAddress failed: CVReturn {lock_status}"
        )));
    }

    // objc2-core-video marks these getters as safe; only the pointer
    // arithmetic reads inside the unpack loops below need `unsafe`.
    let pb_w = CVPixelBufferGetWidth(pixel_buf);
    let pb_h = CVPixelBufferGetHeight(pixel_buf);
    let w = config.width as usize;
    let h = config.height as usize;
    if pb_w < w || pb_h < h {
        // SAFETY: must pair the Lock above.
        unsafe { CVPixelBufferUnlockBaseAddress(pixel_buf, CVPixelBufferLockFlags::ReadOnly) };
        return Err(BackendError::Decode(format!(
            "CVPixelBuffer too small: {pb_w}x{pb_h} < {w}x{h}"
        )));
    }
    let half_w = w / 2;
    let half_h = h / 2;
    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    let y_base = CVPixelBufferGetBaseAddressOfPlane(pixel_buf, 0);
    let y_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buf, 0);
    let uv_base = CVPixelBufferGetBaseAddressOfPlane(pixel_buf, 1);
    let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(pixel_buf, 1);

    if config.bit_depth_luma >= 10 {
        // P010: u16 LE with value in the LOW 10 bits (unlike Windows MF's
        // P010 which is MSB-aligned). Mask to 10 bits.
        for row in 0..h {
            // SAFETY: row inside bounds; y_stride is the per-row byte count.
            let row_ptr = unsafe { (y_base as *const u8).add(row * y_stride) };
            // SAFETY: row covers at least 2*w bytes (u16 per pixel).
            let row_slice = unsafe { std::slice::from_raw_parts(row_ptr, 2 * w) };
            for x in 0..w {
                let v = (u16::from(row_slice[2 * x + 1]) << 8) | u16::from(row_slice[2 * x]);
                y_plane[row * w + x] = v & 0x3FF;
            }
        }
        for row in 0..half_h {
            // SAFETY: UV plane base + per-row stride.
            let row_ptr = unsafe { (uv_base as *const u8).add(row * uv_stride) };
            // SAFETY: 4 bytes per UV pair (u16 Cb + u16 Cr) × half_w.
            let row_slice = unsafe { std::slice::from_raw_parts(row_ptr, 4 * half_w) };
            for x in 0..half_w {
                let cb = (u16::from(row_slice[4 * x + 1]) << 8) | u16::from(row_slice[4 * x]);
                let cr = (u16::from(row_slice[4 * x + 3]) << 8) | u16::from(row_slice[4 * x + 2]);
                cb_plane[row * half_w + x] = cb & 0x3FF;
                cr_plane[row * half_w + x] = cr & 0x3FF;
            }
        }
    } else {
        // NV12 8-bit: zero-extend u8 → u16 in [0, 255] range.
        for row in 0..h {
            // SAFETY: row inside bounds.
            let row_ptr = unsafe { (y_base as *const u8).add(row * y_stride) };
            // SAFETY: row covers w bytes.
            let row_slice = unsafe { std::slice::from_raw_parts(row_ptr, w) };
            for x in 0..w {
                y_plane[row * w + x] = u16::from(row_slice[x]);
            }
        }
        for row in 0..half_h {
            // SAFETY: UV plane row.
            let row_ptr = unsafe { (uv_base as *const u8).add(row * uv_stride) };
            // SAFETY: 2 bytes per UV pair × half_w.
            let row_slice = unsafe { std::slice::from_raw_parts(row_ptr, 2 * half_w) };
            for x in 0..half_w {
                cb_plane[row * half_w + x] = u16::from(row_slice[2 * x]);
                cr_plane[row * half_w + x] = u16::from(row_slice[2 * x + 1]);
            }
        }
    }

    // SAFETY: pairs the Lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(pixel_buf, CVPixelBufferLockFlags::ReadOnly) };

    Ok(DecodedFrame {
        width: config.width,
        height: config.height,
        y_plane,
        cb_plane,
        cr_plane,
        bit_depth: config.bit_depth_luma,
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
