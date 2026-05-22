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

use windows::Win32::Media::MediaFoundation::{
    IMF2DBuffer, IMFActivate, IMFAttributes, IMFMediaBuffer, IMFMediaType, IMFSample, IMFTransform,
    MF_E_TRANSFORM_NEED_MORE_INPUT, MF_E_TRANSFORM_STREAM_CHANGE, MF_MT_FRAME_SIZE,
    MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE, MF_MT_MPEG_SEQUENCE_HEADER, MF_MT_PIXEL_ASPECT_RATIO,
    MF_MT_SUBTYPE, MF_VERSION, MFCreateMediaType, MFCreateMemoryBuffer, MFCreateSample,
    MFMediaType_Video, MFSTARTUP_NOSOCKET, MFStartup, MFT_CATEGORY_VIDEO_DECODER,
    MFT_ENUM_FLAG_LOCALMFT, MFT_ENUM_FLAG_SORTANDFILTER, MFT_ENUM_FLAG_SYNCMFT,
    MFT_MESSAGE_COMMAND_DRAIN, MFT_MESSAGE_NOTIFY_BEGIN_STREAMING,
    MFT_MESSAGE_NOTIFY_END_OF_STREAM, MFT_OUTPUT_DATA_BUFFER, MFT_OUTPUT_STREAM_INFO,
    MFT_REGISTER_TYPE_INFO, MFTEnumEx, MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoFormat_P010,
    MFVideoInterlace_Progressive,
};
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::core::Interface;

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
            if hr.is_ok() || hr.0 == 0x0000_0001
            /* S_FALSE: already inited */
            {
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

        // Configure the MFT for the bitstream-coded dimensions so its
        // output matches what we'll read back. The visible region is
        // (width, height) cropped from (coded_width, coded_height) via
        // crop_* offsets — we copy out only the visible pixels in the
        // unpack stage below.
        let coded_w = config.coded_width.max(config.width);
        let coded_h = config.coded_height.max(config.height);
        let bit_depth = config.bit_depth_luma;

        let needs_reconfig = self
            .configured
            .as_ref()
            .is_none_or(|c| c.width != coded_w || c.height != coded_h || c.bit_depth != bit_depth);

        if self.transform.is_none() {
            self.transform = Some(activate_hevc_decoder()?);
        }
        let transform = self
            .transform
            .as_ref()
            .expect("transform initialized above");

        if needs_reconfig {
            configure_input_type(transform, config, coded_w, coded_h)?;
            configure_output_type(transform, bit_depth)?;
            // SAFETY: ProcessMessage with NOTIFY_BEGIN_STREAMING is the
            // documented MFT lifecycle call; arguments are constants.
            unsafe { transform.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0) }
                .map_err(decode_err("ProcessMessage(BEGIN_STREAMING)"))?;
            self.configured = Some(ConfiguredFor {
                width: coded_w,
                height: coded_h,
                bit_depth,
            });
        }

        decode_one_frame(transform, config, image_data, coded_w, coded_h, bit_depth)
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
    let transform: IMFTransform =
        unsafe { activate.ActivateObject() }.map_err(decode_err("IMFActivate::ActivateObject"))?;

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
    // Build the access-unit Annex-B blob: parameter sets (VPS+SPS+PPS)
    // followed by the slice NALs. Some HEVC MFTs decode reliably only
    // when parameter sets are prepended inline at each AU even though
    // we also set MF_MT_MPEG_SEQUENCE_HEADER on the input media type.
    let mut annexb = nal::annexb_parameter_sets(config.nal_units);
    let slice_annexb = nal::hvcc_to_annexb(image_data, config.length_size).ok_or_else(|| {
        BackendError::Decode("malformed hvcC length-prefixed slice data".to_string())
    })?;
    annexb.extend_from_slice(&slice_annexb);

    // Wrap Annex B bytes in an IMFSample.
    let sample = build_input_sample(&annexb)?;
    // Some MFTs reject samples without timestamps; pin a zero PTS + 1/60s
    // duration since for a still image the values are irrelevant but the
    // transform may require them to be set at all. SAFETY: API contract.
    // SAFETY: SetSampleTime / SetSampleDuration are documented IMFSample
    // setters; we own `sample` from the MFCreateSample above. Some HEVC
    // MFTs reject inputs without timing; pin a 60Hz frame's worth of
    // 100ns ticks (the values themselves are immaterial for still
    // decode, but they must be set).
    unsafe { sample.SetSampleTime(0) }.map_err(decode_err("IMFSample::SetSampleTime"))?;
    // SAFETY: see above — same contract on the same sample.
    unsafe { sample.SetSampleDuration(166_667) }
        .map_err(decode_err("IMFSample::SetSampleDuration"))?;

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
    let output_sample = poll_output(transform, width, height, bit_depth)?;

    // Unpack the output sample, copying only the visible region per the
    // SPS conformance window. width/height here are the coded
    // dimensions; the visible region is offset by crop_left / crop_top.
    let planes = read_output_planes(
        &output_sample,
        width,
        height,
        config.width,
        config.height,
        config.crop_left,
        config.crop_top,
        bit_depth,
    )?;

    Ok(DecodedFrame {
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
        // VUI color metadata is parsed once by the parent crate from the
        // SPS and threaded in via HvccParams — using the wrong values here
        // produces color-shifted output that's perceptually obvious
        // (BT.601 vs BT.709 vs BT.2020 swap, limited vs full range).
        full_range: config.full_range,
        matrix_coeffs: config.matrix_coeffs,
        color_primaries: config.color_primaries,
        transfer_characteristics: config.transfer_characteristics,
        deblock_flags: Vec::new(),
        deblock_stride: 0,
        qp_map: Vec::new(),
    })
}

fn build_input_sample(annexb: &[u8]) -> Result<IMFSample, BackendError> {
    let size = annexb.len() as u32;
    // SAFETY: MFCreateMemoryBuffer allocates a heap buffer of the requested
    // size and returns an IMFMediaBuffer.
    let buffer: IMFMediaBuffer =
        unsafe { MFCreateMemoryBuffer(size) }.map_err(decode_err("MFCreateMemoryBuffer"))?;

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

fn poll_output(
    transform: &IMFTransform,
    width: u32,
    height: u32,
    _bit_depth: u8,
) -> Result<IMFSample, BackendError> {
    // We need to allocate the output sample ourselves if the MFT doesn't
    // provide one. Query the stream info.
    // SAFETY: GetOutputStreamInfo is a documented MFT inspection call.
    let info: MFT_OUTPUT_STREAM_INFO = unsafe { transform.GetOutputStreamInfo(0) }
        .map_err(decode_err("IMFTransform::GetOutputStreamInfo"))?;

    // If the MFT provides its own samples (PROVIDES_SAMPLES = 0x100), pass
    // a null output buffer pointer; otherwise pre-allocate.
    const PROVIDES_SAMPLES: u32 = 0x100;
    let provides_samples = (info.dwFlags & PROVIDES_SAMPLES) != 0;

    // info.cbSize is sometimes 0 or undersized before the first
    // STREAM_CHANGE; over-allocate to width*height*4 which covers NV12
    // (1.5 bpp) + P010 (3 bpp) + 16-row-alignment slack on any HEVC
    // resolution we'd see in HEIF. The MFT writes the actual length
    // back via SetCurrentLength.
    let buf_size = info
        .cbSize
        .max(width.saturating_mul(height).saturating_mul(4));

    let mut attempts = 0;
    loop {
        attempts += 1;
        if attempts > 32 {
            return Err(BackendError::Decode(
                "MFT didn't produce output after 32 ProcessOutput attempts".to_string(),
            ));
        }

        // Build the output buffer array in place so the MFT's writes to
        // `pSample` / `dwStatus` come back to us. Cloning the buffer before
        // passing it to ProcessOutput would discard those writes (the
        // earlier bug). The array lives in `bufs` until we read bufs[0]
        // back after the call.
        let pre_alloc = if provides_samples {
            core::mem::ManuallyDrop::new(None)
        } else {
            core::mem::ManuallyDrop::new(Some(alloc_output_sample(buf_size)?))
        };
        let mut bufs = [MFT_OUTPUT_DATA_BUFFER {
            dwStreamID: 0,
            pSample: pre_alloc,
            dwStatus: 0,
            pEvents: core::mem::ManuallyDrop::new(None),
        }];
        let mut status: u32 = 0;

        // SAFETY: ProcessOutput with a single output stream buffer slice we
        // hold ownership of for the duration of the call.
        let hr: Result<(), windows::core::Error> =
            unsafe { transform.ProcessOutput(0, &mut bufs, &mut status) };

        match hr {
            Ok(()) => {
                // Take ownership of whatever the MFT wrote into pSample.
                // SAFETY: ManuallyDrop::take is safe because we don't
                // touch bufs[0].pSample after this point (the array is
                // dropped at scope exit; the pEvents ManuallyDrop is None
                // so its Drop is a no-op).
                let taken = unsafe { core::mem::ManuallyDrop::take(&mut bufs[0].pSample) };
                if let Some(sample) = taken {
                    return Ok(sample);
                }
                return Err(BackendError::Decode(
                    "ProcessOutput succeeded but produced no sample".to_string(),
                ));
            }
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                // Renegotiate output type. Walk available types again.
                // For NV12 / P010 path the renegotiation is usually
                // idempotent because the bitstream dimensions match what
                // we set.
                configure_output_type(transform, _bit_depth)?;
                continue;
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => {
                // Drain in progress; loop once more.
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

#[allow(clippy::too_many_arguments)]
fn read_output_planes(
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

    // Fallback: locked linear access; assume stride == coded_w.
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
            ptr, stride, coded_h, visible_w, visible_h, crop_x, crop_y, bit_depth, cur_len,
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
    let aligned_height = (total_bytes * 2 / 3)
        .checked_div(stride_abs)
        .unwrap_or(0)
        .max(coded_h as usize);

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
            total_bytes as u32,
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
/// `crop_y`) — this is the SPS conformance window crop applied at copy
/// time so callers see exactly the ispe-visible region.
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
    _total_len: u32,
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
        // UV plane starts at aligned_height * row_stride; visible UV row
        // is at chroma_cy + uv_y within the UV plane.
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

fn set_u64(
    attrs: &IMFAttributes,
    key: &windows::core::GUID,
    value: u64,
) -> Result<(), BackendError> {
    // SAFETY: SetUINT64 with a valid attribute key.
    unsafe { attrs.SetUINT64(key, value) }.map_err(decode_err("IMFAttributes::SetUINT64"))
}

fn set_u32(
    attrs: &IMFAttributes,
    key: &windows::core::GUID,
    value: u32,
) -> Result<(), BackendError> {
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
