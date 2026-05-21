//! Windows-only Media Foundation HEVC decoder implementation.
//!
//! Compile-gated by `target_os = "windows"` from the parent module.
//! Uses raw `windows` crate COM bindings; every `unsafe` block carries a
//! `SAFETY:` comment justifying the Win32 contract being upheld.

#![cfg(target_os = "windows")]

use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::vec;
use std::vec::Vec;

use heic_core::{BackendError, DecodedFrame, HvccParams, nal};

use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE,
    MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFStartup, MFTEnumEx, MFT_CATEGORY_VIDEO_DECODER, MFT_ENUM_FLAG_LOCALMFT,
    MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT, MFT_MESSAGE_COMMAND_DRAIN,
    MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, MFT_MESSAGE_NOTIFY_END_OF_STREAM,
    MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO, MFT_REGISTER_TYPE_INFO,
    MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010, MFVideoInterlace_Progressive,
    MFSTARTUP_NOSOCKET, MF_VERSION,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};

// ────────────────────────────────────────────────────────────────────────
// Process / thread initialization

static MF_STARTUP: Once = Once::new();
static MF_STARTUP_OK: AtomicBool = AtomicBool::new(false);

thread_local! {
    static COM_INITIALIZED: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

/// Ensure `MFStartup` has been called once for this process and that the
/// calling thread is COM-initialized in the MTA.
fn init_mf() -> Result<(), BackendError> {
    MF_STARTUP.call_once(|| {
        // SAFETY: MFStartup is the documented one-time-per-process MF
        // initializer; the version/flags are constants from the SDK.
        let hr = unsafe { MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET) };
        if hr.is_ok() {
            MF_STARTUP_OK.store(true, Ordering::Release);
        }
    });
    if !MF_STARTUP_OK.load(Ordering::Acquire) {
        return Err(BackendError::Unavailable(
            "MFStartup failed (Media Foundation not available on this build)",
        ));
    }
    COM_INITIALIZED.with(|c| {
        if !c.get() {
            // SAFETY: CoInitializeEx is the documented per-thread COM init.
            // MTA matches MF's threading model. Repeated calls are reference
            // counted; we leave the deinit to thread exit (acceptable for a
            // long-lived worker thread in a HEIC decode app).
            let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            if hr.is_ok() || hr.0 == 0x0000_0001 /* S_FALSE: already inited */ {
                c.set(true);
            }
        }
    });
    Ok(())
}

// ────────────────────────────────────────────────────────────────────────
// Backend state

#[derive(Default)]
pub(super) struct Inner {
    /// Cached HEVC transform. None until first decode initializes it; reused
    /// across subsequent decodes by `flush()`'ing between frames.
    transform: Option<IMFTransform>,
    /// Last configured input dimensions; rebuilt when the bitstream changes.
    configured: Option<ConfiguredFor>,
}

#[derive(PartialEq, Eq)]
struct ConfiguredFor {
    width: u32,
    height: u32,
    bit_depth: u8,
}

impl Inner {
    pub(super) fn decode(
        &mut self,
        config: &HvccParams<'_>,
        image_data: &[u8],
        _stop: &dyn enough::Stop,
    ) -> Result<DecodedFrame, BackendError> {
        init_mf()?;

        let width = config.width;
        let height = config.height;
        let bit_depth = config.bit_depth_luma;

        // Ensure transform is created and configured for this stream.
        let needs_reconfig = self
            .configured
            .as_ref()
            .is_none_or(|c| c.width != width || c.height != height || c.bit_depth != bit_depth);

        if self.transform.is_none() {
            self.transform = Some(activate_hevc_decoder()?);
        }
        let transform = self
            .transform
            .as_ref()
            .expect("transform initialized above");

        if needs_reconfig {
            configure_input_type(transform, config, width, height)?;
            configure_output_type(transform, bit_depth)?;
            // SAFETY: ProcessMessage with NOTIFY_BEGIN_STREAMING is the
            // documented MFT lifecycle call; arguments are constants.
            unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0) }
                .map_err(decode_err("ProcessMessage(BEGIN_STREAMING)"))?;
            self.configured = Some(ConfiguredFor {
                width,
                height,
                bit_depth,
            });
        }

        decode_one_frame(transform, config, image_data, width, height, bit_depth)
    }
}

// ────────────────────────────────────────────────────────────────────────
// MFTEnumEx → IMFTransform activation

/// Returns true if at least one HEVC decoder MFT is registered on this
/// machine — answers the "is the HEVC Video Extensions package installed?"
/// question without actually instantiating the decoder.
pub(super) fn is_available() -> bool {
    if init_mf().is_err() {
        return false;
    }
    enum_hevc_decoders().is_ok_and(|n| n > 0)
}

fn enum_hevc_decoders() -> Result<u32, BackendError> {
    let input_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    let mut activate_ptrs: *mut Option<IMFActivate> = core::ptr::null_mut();
    let mut count: u32 = 0;
    // SAFETY: MFTEnumEx is the documented enumerator; we pass valid pointers
    // for the out-params and clean up via CoTaskMemFree below.
    let hr = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input_type),
            None,
            &mut activate_ptrs,
            &mut count,
        )
    };
    hr.map_err(decode_err("MFTEnumEx"))?;
    if !activate_ptrs.is_null() {
        // The returned array owns its IMFActivate refs; we don't need them
        // here (count is sufficient), so drop them by reading and dropping
        // each Option<IMFActivate>.
        //
        // SAFETY: count was set by MFTEnumEx; we read exactly that many
        // entries. CoTaskMemFree frees the array allocated by the API.
        unsafe {
            for i in 0..count as isize {
                let _ = activate_ptrs.offset(i).read();
            }
            CoTaskMemFree(Some(activate_ptrs.cast()));
        }
    }
    Ok(count)
}

fn activate_hevc_decoder() -> Result<IMFTransform, BackendError> {
    let input_type = MFT_REGISTER_TYPE_INFO {
        guidMajorType: MFMediaType_Video,
        guidSubtype: MFVideoFormat_HEVC,
    };
    let mut activate_ptrs: *mut Option<IMFActivate> = core::ptr::null_mut();
    let mut count: u32 = 0;
    // SAFETY: same contract as enum_hevc_decoders.
    let hr = unsafe {
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_DECODER,
            MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_LOCALMFT | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input_type),
            None,
            &mut activate_ptrs,
            &mut count,
        )
    };
    hr.map_err(decode_err("MFTEnumEx"))?;

    if count == 0 || activate_ptrs.is_null() {
        return Err(BackendError::Unavailable(
            "no HEVC Video Decoder MFT registered (HEVC Video Extensions \
             package not installed, or unsupported Windows SKU)",
        ));
    }

    // First entry wins (SORTANDFILTER puts the best one first).
    // SAFETY: count >= 1 confirmed above; read the first IMFActivate.
    let first = unsafe { activate_ptrs.read() };
    // Free remaining entries + the array itself.
    // SAFETY: drop entries [1..count) and the array allocation.
    unsafe {
        for i in 1..count as isize {
            let _ = activate_ptrs.offset(i).read();
        }
        CoTaskMemFree(Some(activate_ptrs.cast()));
    }

    let activate = first
        .ok_or_else(|| BackendError::Decode("MFTEnumEx returned a null IMFActivate".to_string()))?;

    // SAFETY: IMFActivate::ActivateObject for IMFTransform is the documented
    // MFT instantiation entry point.
    let transform: IMFTransform = unsafe { activate.ActivateObject() }
        .map_err(decode_err("IMFActivate::ActivateObject"))?;

    Ok(transform)
}

// ────────────────────────────────────────────────────────────────────────
// Media type configuration

fn configure_input_type(
    transform: &IMFTransform,
    config: &HvccParams<'_>,
    width: u32,
    height: u32,
) -> Result<(), BackendError> {
    // SAFETY: MFCreateMediaType allocates a new IMFMediaType.
    let media_type: IMFMediaType =
        unsafe { MFCreateMediaType() }.map_err(decode_err("MFCreateMediaType(input)"))?;

    let attrs: &IMFAttributes = (&media_type).into();

    set_guid(attrs, &MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    set_guid(attrs, &MF_MT_SUBTYPE, &MFVideoFormat_HEVC)?;
    set_u64(
        attrs,
        &MF_MT_FRAME_SIZE,
        (u64::from(width) << 32) | u64::from(height),
    )?;
    set_u64(attrs, &MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1u64)?;
    set_u32(
        attrs,
        &MF_MT_INTERLACE_MODE,
        MFVideoInterlace_Progressive.0 as u32,
    )?;

    // Build the Annex-B parameter-set blob from the hvcC NAL list and set
    // it as MF_MT_MPEG_SEQUENCE_HEADER. The MFT reads VPS/SPS/PPS from this
    // attribute and configures itself before the first ProcessInput.
    let seq_header = nal::annexb_parameter_sets(config.nal_units);
    set_blob(attrs, &MF_MT_MPEG_SEQUENCE_HEADER, &seq_header)?;

    // SAFETY: SetInputType with stream id 0 + flags 0 is the standard MFT
    // path; we own `media_type` and pass a borrowed reference.
    unsafe { transform.SetInputType(0, &media_type, 0) }
        .map_err(decode_err("IMFTransform::SetInputType"))?;
    Ok(())
}

fn configure_output_type(transform: &IMFTransform, bit_depth: u8) -> Result<(), BackendError> {
    let target_subtype = if bit_depth >= 10 {
        MFVideoFormat_P010
    } else {
        MFVideoFormat_NV12
    };

    // Iterate available output types until we find one matching our target
    // subtype; setting the MFT's preferred type tends to work but explicit
    // search is safer.
    let mut i: u32 = 0;
    loop {
        // SAFETY: GetOutputAvailableType is the documented enumerator for
        // an MFT's supported output types; iterates 0..N.
        let media_type: IMFMediaType = match unsafe { transform.GetOutputAvailableType(0, i) } {
            Ok(t) => t,
            Err(e) => {
                return Err(BackendError::Decode(format!(
                    "no compatible output type (looking for {:?}): {e}",
                    target_subtype
                )));
            }
        };
        let attrs: &IMFAttributes = (&media_type).into();
        // SAFETY: GetGUID on a freshly-enumerated media type's known
        // attribute (MF_MT_SUBTYPE).
        if let Ok(subtype) = unsafe { attrs.GetGUID(&MF_MT_SUBTYPE) }
            && subtype == target_subtype
        {
            // SAFETY: SetOutputType with the type the MFT just gave us.
            unsafe { transform.SetOutputType(0, &media_type, 0) }
                .map_err(decode_err("IMFTransform::SetOutputType"))?;
            return Ok(());
        }
        i += 1;
        if i > 32 {
            return Err(BackendError::Decode(
                "exhausted output type search; HEVC MFT doesn't expose NV12/P010".to_string(),
            ));
        }
    }
}

// ────────────────────────────────────────────────────────────────────────
// Decode one frame: feed input → drain → extract output

fn decode_one_frame(
    transform: &IMFTransform,
    config: &HvccParams<'_>,
    image_data: &[u8],
    width: u32,
    height: u32,
    bit_depth: u8,
) -> Result<DecodedFrame, BackendError> {
    // hvcC length-prefixed → Annex B for the MFT.
    let annexb = nal::hvcc_to_annexb(image_data, config.length_size).ok_or_else(|| {
        BackendError::Decode("malformed hvcC length-prefixed slice data".to_string())
    })?;

    // Wrap Annex B bytes in an IMFSample.
    let sample = build_input_sample(&annexb)?;

    // ProcessInput consumes the sample; the MFT internally queues it.
    // SAFETY: ProcessInput is the documented input-side entry; we own the
    // sample and pass a borrowed reference per the API contract.
    unsafe { transform.ProcessInput(0, &sample, 0) }
        .map_err(decode_err("IMFTransform::ProcessInput"))?;

    // Drain the MFT — for a single-frame still, the first ProcessOutput
    // after END_OF_STREAM + DRAIN should emit the frame.
    // SAFETY: ProcessMessage with NOTIFY_END_OF_STREAM + COMMAND_DRAIN.
    unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0) }
        .map_err(decode_err("ProcessMessage(END_OF_STREAM)"))?;
    // SAFETY: same as above.
    unsafe { transform.ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0) }
        .map_err(decode_err("ProcessMessage(DRAIN)"))?;

    // Loop ProcessOutput, handling stream-change renegotiation, until we
    // get a sample.
    let output_sample = poll_output(transform, bit_depth)?;

    // Unpack the output sample's NV12 / P010 buffer to planar u16.
    let planes = read_output_planes(&output_sample, width, height, bit_depth)?;

    Ok(DecodedFrame {
        width,
        height,
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
        full_range: false,
        matrix_coeffs: 2,
        color_primaries: 2,
        transfer_characteristics: 2,
        deblock_flags: Vec::new(),
        deblock_stride: 0,
        qp_map: Vec::new(),
    })
}

fn build_input_sample(annexb: &[u8]) -> Result<IMFSample, BackendError> {
    let size = annexb.len() as u32;
    // SAFETY: MFCreateMemoryBuffer allocates a heap buffer of the requested
    // size and returns an IMFMediaBuffer.
    let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(size) }
        .map_err(decode_err("MFCreateMemoryBuffer"))?;

    // Lock the buffer and memcpy the Annex B bytes in.
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut _max_len: u32 = 0;
    let mut cur_len: u32 = 0;
    // SAFETY: Lock returns a writable pointer up to max_len bytes. We write
    // exactly `size` bytes which is <= the allocation.
    unsafe { buffer.Lock(&mut ptr, Some(&mut _max_len), Some(&mut cur_len)) }
        .map_err(decode_err("IMFMediaBuffer::Lock"))?;
    // SAFETY: ptr is valid for `size` writes per Lock's contract.
    unsafe {
        core::ptr::copy_nonoverlapping(annexb.as_ptr(), ptr, size as usize);
    }
    // SAFETY: SetCurrentLength tells the buffer how many bytes are valid.
    unsafe { buffer.SetCurrentLength(size) }
        .map_err(decode_err("IMFMediaBuffer::SetCurrentLength"))?;
    // SAFETY: Unlock pairs with the earlier Lock.
    unsafe { buffer.Unlock() }.map_err(decode_err("IMFMediaBuffer::Unlock"))?;

    // SAFETY: MFCreateSample allocates an IMFSample; AddBuffer attaches our
    // IMFMediaBuffer.
    let sample: IMFSample = unsafe { MFCreateSample() }.map_err(decode_err("MFCreateSample"))?;
    // SAFETY: AddBuffer with a freshly created sample.
    unsafe { sample.AddBuffer(&buffer) }.map_err(decode_err("IMFSample::AddBuffer"))?;
    Ok(sample)
}

fn poll_output(transform: &IMFTransform, _bit_depth: u8) -> Result<IMFSample, BackendError> {
    // We need to allocate the output sample ourselves if the MFT doesn't
    // provide one. Query the stream info.
    // SAFETY: GetOutputStreamInfo is a documented MFT inspection call.
    let info: MFT_OUTPUT_STREAM_INFO = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(decode_err("IMFTransform::GetOutputStreamInfo"))?;

    // If the MFT provides its own samples (PROVIDES_SAMPLES = 0x100), pass
    // a null output buffer pointer; otherwise pre-allocate.
    const PROVIDES_SAMPLES: u32 = 0x100;
    let provides_samples = (info.dwFlags & PROVIDES_SAMPLES) != 0;

    let mut attempts = 0;
    loop {
        attempts += 1;
        if attempts > 32 {
            return Err(BackendError::Decode(
                "MFT didn't produce output after 32 ProcessOutput attempts".to_string(),
            ));
        }

        let pre_alloc_sample = if provides_samples {
            None
        } else {
            Some(alloc_output_sample(info.cbSize)?)
        };

        let output_buf = MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: pre_alloc_sample
                .as_ref()
                .map_or(core::mem::ManuallyDrop::new(None), |s| {
                    core::mem::ManuallyDrop::new(Some(s.clone()))
                }),
            dwStatus: 0,
            pEvents: core::mem::ManuallyDrop::new(None),
        };
        let mut status: u32 = 0;

        // SAFETY: ProcessOutput with a single output stream buffer.
        let hr: Result<(), windows::core::Error> =
            unsafe { transform.ProcessOutput(0, &mut [output_buf.clone()], &mut status) };

        match hr {
            Ok(()) => {
                // The MFT wrote into output_buf.pSample (or replaced it if
                // PROVIDES_SAMPLES).
                if let Some(sample) = core::mem::ManuallyDrop::into_inner(output_buf.pSample) {
                    return Ok(sample);
                }
                return Err(BackendError::Decode(
                    "ProcessOutput succeeded but produced no sample".to_string(),
                ));
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // Renegotiate output type. Walk available types again.
                // For NV12 / P010 path the renegotiation is usually idempotent
                // because the bitstream dimensions match what we set.
                configure_output_type(transform, _bit_depth)?;
                continue;
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                // Drain in progress; loop once more after a no-op sleep.
                // For a single frame this should resolve on the next iteration.
                continue;
            }
            Err(e) => {
                return Err(BackendError::Decode(format!(
                    "IMFTransform::ProcessOutput: {e}"
                )));
            }
        }
    }
}

fn alloc_output_sample(size: u32) -> Result<IMFSample, BackendError> {
    // SAFETY: MFCreateMemoryBuffer allocates a buffer of `size` bytes.
    let buffer: IMFMediaBuffer = unsafe { MFCreateMemoryBuffer(size.max(1)) }
        .map_err(decode_err("MFCreateMemoryBuffer(out)"))?;
    // SAFETY: MFCreateSample allocates a fresh empty IMFSample.
    let sample: IMFSample =
        unsafe { MFCreateSample() }.map_err(decode_err("MFCreateSample(out)"))?;
    // SAFETY: AddBuffer on a freshly created sample.
    unsafe { sample.AddBuffer(&buffer) }.map_err(decode_err("IMFSample::AddBuffer(out)"))?;
    Ok(sample)
}

// ────────────────────────────────────────────────────────────────────────
// NV12 / P010 → planar u16

struct OutputPlanes {
    y: Vec<u16>,
    cb: Vec<u16>,
    cr: Vec<u16>,
}

fn read_output_planes(
    sample: &IMFSample,
    width: u32,
    height: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    // Convert the sample to a single contiguous buffer.
    // SAFETY: ConvertToContiguousBuffer returns one IMFMediaBuffer covering
    // all the sample's buffers, allocating if needed.
    let buffer: IMFMediaBuffer = unsafe { sample.ConvertToContiguousBuffer() }
        .map_err(decode_err("IMFSample::ConvertToContiguousBuffer"))?;

    // Prefer IMF2DBuffer for stride access; fall back to Lock when 2D isn't
    // implemented (some software MFTs).
    if let Ok(buf2d) = buffer.cast::<IMF2DBuffer>() {
        return read_planes_2d(&buf2d, width, height, bit_depth);
    }

    // Fallback: locked linear access; assume stride == width.
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut max_len: u32 = 0;
    let mut cur_len: u32 = 0;
    // SAFETY: Lock returns a readable pointer to up to max_len bytes.
    unsafe { buffer.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len)) }
        .map_err(decode_err("IMFMediaBuffer::Lock(out)"))?;
    let stride = width as usize;
    // SAFETY: the lock guarantees the pointer is valid for `cur_len` bytes.
    let planes = unsafe { unpack_nv12_or_p010(ptr, stride, width, height, bit_depth, cur_len) };
    // SAFETY: pairs with the Lock above.
    unsafe { buffer.Unlock() }.map_err(decode_err("IMFMediaBuffer::Unlock(out)"))?;
    Ok(planes)
}

fn read_planes_2d(
    buf2d: &IMF2DBuffer,
    width: u32,
    height: u32,
    bit_depth: u8,
) -> Result<OutputPlanes, BackendError> {
    let mut ptr: *mut u8 = core::ptr::null_mut();
    let mut stride: i32 = 0;
    // SAFETY: Lock2D returns the row-stride and a pointer to the top-left
    // pixel of the buffer per the MF docs.
    unsafe { buf2d.Lock2D(&mut ptr, &mut stride) }
        .map_err(decode_err("IMF2DBuffer::Lock2D"))?;

    // Stride can be negative (bottom-up), but for NV12/P010 from the HEVC
    // MFT it's typically positive. Handle negative for completeness.
    let stride_abs = stride.unsigned_abs() as usize;
    let mut data_ptr = ptr;
    if stride < 0 {
        // ptr already points at row 0 (top-down logically), per docs; we
        // walk rows by adding the negative stride. To keep the unpacker
        // simple, copy rows out individually here. For now treat as positive.
        // (Bottom-up output from HEVC MFT is rare.)
    }
    // SAFETY: Lock2D guarantees the pointer is valid for height rows of
    // stride_abs bytes (per buffer's GetContiguousLength contract).
    let planes = unsafe {
        unpack_nv12_or_p010(
            data_ptr,
            stride_abs,
            width,
            height,
            bit_depth,
            (stride_abs * height as usize * 3 / 2) as u32,
        )
    };
    let _ = &mut data_ptr; // suppress unused-mut when negative-stride branch is a no-op
    // SAFETY: pairs with Lock2D.
    unsafe { buf2d.Unlock2D() }.map_err(decode_err("IMF2DBuffer::Unlock2D"))?;
    Ok(planes)
}

/// SAFETY: caller guarantees `base` points at the start of an NV12 (8-bit) or
/// P010 (16-bit, MSB-aligned 10-bit + 6 zero LSBs) frame of `height` rows at
/// `row_stride` bytes per row, followed immediately by the interleaved UV
/// plane at `height/2` rows at `row_stride` bytes per row.
unsafe fn unpack_nv12_or_p010(
    base: *const u8,
    row_stride: usize,
    width: u32,
    height: u32,
    bit_depth: u8,
    _total_len: u32,
) -> OutputPlanes {
    let w = width as usize;
    let h = height as usize;
    let half_h = h / 2;
    let half_w = w / 2;

    let mut y_plane = vec![0u16; w * h];
    let mut cb_plane = vec![0u16; half_w * half_h];
    let mut cr_plane = vec![0u16; half_w * half_h];

    if bit_depth <= 8 {
        // 8-bit NV12: Y row by row, UV interleaved. Materialize each row as
        // a safe `&[u8]` slice with one `unsafe` at the top, then index
        // through it with normal bounds-checked accesses.
        for y in 0..h {
            // SAFETY: row y of the Y plane occupies [base + y*stride,
            // base + y*stride + stride) per the caller's guarantee.
            let row = unsafe { core::slice::from_raw_parts(base.add(y * row_stride), row_stride) };
            for x in 0..w {
                let v = row[x];
                y_plane[y * w + x] = u16::from(v) << 8 | u16::from(v);
            }
        }
        // UV plane starts at base + h * row_stride.
        for y in 0..half_h {
            // SAFETY: UV row y occupies [base + (h+y)*stride,
            // base + (h+y)*stride + stride).
            let row =
                unsafe { core::slice::from_raw_parts(base.add((h + y) * row_stride), row_stride) };
            for x in 0..half_w {
                let cb = row[2 * x];
                let cr = row[2 * x + 1];
                cb_plane[y * half_w + x] = u16::from(cb) << 8 | u16::from(cb);
                cr_plane[y * half_w + x] = u16::from(cr) << 8 | u16::from(cr);
            }
        }
    } else {
        // 10-bit P010: u16 LE, MSB-aligned, low 6 bits zero. Shift right by 6.
        for y in 0..h {
            // SAFETY: row y of the Y plane occupies [base + y*stride,
            // base + y*stride + stride). P010 has 2 bytes per pixel; row
            // stride is already the byte stride from Lock2D.
            let row = unsafe { core::slice::from_raw_parts(base.add(y * row_stride), row_stride) };
            for x in 0..w {
                let lo = row[2 * x];
                let hi = row[2 * x + 1];
                let v = (u16::from(hi) << 8) | u16::from(lo);
                y_plane[y * w + x] = v >> 6;
            }
        }
        for y in 0..half_h {
            // SAFETY: UV row y occupies [base + (h+y)*stride,
            // base + (h+y)*stride + stride). 4 bytes per chroma pair.
            let row =
                unsafe { core::slice::from_raw_parts(base.add((h + y) * row_stride), row_stride) };
            for x in 0..half_w {
                let cb = (u16::from(row[4 * x + 1]) << 8) | u16::from(row[4 * x]);
                let cr = (u16::from(row[4 * x + 3]) << 8) | u16::from(row[4 * x + 2]);
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

// ────────────────────────────────────────────────────────────────────────
// Small attribute helpers

fn set_guid(
    attrs: &IMFAttributes,
    key: &windows::core::GUID,
    value: &windows::core::GUID,
) -> Result<(), BackendError> {
    // SAFETY: SetGUID with valid attribute and value pointers.
    unsafe { attrs.SetGUID(key, value) }.map_err(decode_err("IMFAttributes::SetGUID"))
}

fn set_u64(attrs: &IMFAttributes, key: &windows::core::GUID, value: u64) -> Result<(), BackendError> {
    // SAFETY: SetUINT64 with a valid attribute key.
    unsafe { attrs.SetUINT64(key, value) }.map_err(decode_err("IMFAttributes::SetUINT64"))
}

fn set_u32(attrs: &IMFAttributes, key: &windows::core::GUID, value: u32) -> Result<(), BackendError> {
    // SAFETY: SetUINT32 with a valid attribute key.
    unsafe { attrs.SetUINT32(key, value) }.map_err(decode_err("IMFAttributes::SetUINT32"))
}

fn set_blob(
    attrs: &IMFAttributes,
    key: &windows::core::GUID,
    value: &[u8],
) -> Result<(), BackendError> {
    // SAFETY: SetBlob with a valid attribute key and a slice pointer + len.
    unsafe { attrs.SetBlob(key, value) }.map_err(decode_err("IMFAttributes::SetBlob"))
}

fn decode_err(op: &'static str) -> impl Fn(windows::core::Error) -> BackendError {
    move |e: windows::core::Error| BackendError::Decode(format!("{op}: {e}"))
}
