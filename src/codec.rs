//! zencodec trait implementations for heic.
//!
//! Provides [`HeicDecoderConfig`] that implements the 4-layer decode trait
//! hierarchy from zencodec, wrapping the native heic API.
//!
//! # Trait mapping
//!
//! | zencodec | heic adapter |
//! |----------------|----------------------|
//! | `DecoderConfig` | [`HeicDecoderConfig`] |
//! | `DecodeJob<'a>` | [`HeicDecodeJob`] |
//! | `Decode` | [`HeicDecoder`] |
//! | `StreamingDecode` | [`HeicStreamDecoder`] |
//! | `AnimationFrameDecoder` | `Unsupported<HeicError>` (HEIC has no animation) |

use alloc::borrow::Cow;

use rgb::{Rgb, Rgba};
use zencodec::decode::{
    DecodeCapabilities, DecodeOutput, DecodePolicy, DecodeRowSink, OutputInfo,
    negotiate_pixel_format,
};
use zencodec::{
    ContentLightLevel, GainMapInfo, GainMapPresence, ImageFormat, ImageInfo, ImageSequence,
    MasteringDisplay, Orientation, ResourceLimits, Supplements, ThreadingPolicy, Unsupported,
};
use zenpixels::{Cicp, ColorPrimaries, PixelBuffer, PixelDescriptor, TransferFunction};

use enough::Stop as _;
use whereat::{At, ResultAtExt, at};

use crate::auxiliary::AuxiliaryImageType;
use crate::error::HeicError;

/// Metadata about auxiliary images in a HEIC file.
///
/// This is attached to the zencodec [`DecodeOutput::extensions()`] when the
/// decoded HEIC file contains auxiliary images (depth maps, gain maps, etc.).
///
/// Access via `output.extensions().get::<HeicAuxiliaryInfo>()`.
#[derive(Debug, Clone)]
pub struct HeicAuxiliaryInfo {
    /// Whether the file contains a depth auxiliary image.
    pub has_depth: bool,
    /// Whether the file contains an HDR gain map auxiliary image.
    pub has_gain_map: bool,
    /// Types of all auxiliary images present.
    pub auxiliary_types: alloc::vec::Vec<AuxiliaryImageType>,
}

/// Source encoding details for HEIC files.
///
/// HEIC is always lossy (HEVC-compressed), and the original quality
/// setting cannot be recovered from the bitstream headers.
#[derive(Debug, Clone, Copy)]
pub struct HeicSourceEncoding;

impl zencodec::SourceEncodingDetails for HeicSourceEncoding {
    fn source_generic_quality(&self) -> Option<f32> {
        None // Cannot recover quality from HEVC headers
    }

    fn is_lossless(&self) -> bool {
        false // HEIC is always lossy (HEVC-compressed)
    }
}

// ── Threading helpers ────────────────────────────────────────────────────

/// Convert a [`ThreadingPolicy`] to a concrete thread count for rav1d.
fn policy_to_threads(policy: ThreadingPolicy) -> usize {
    if policy.is_parallel() { 0 } else { 1 }
}

// ── Capabilities ─────────────────────────────────────────────────────────

static HEIC_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_cicp(true)
    .with_stop(true)
    .with_cheap_probe(true)
    .with_decode_into(true)
    .with_streaming(true)
    .with_hdr(true)
    .with_native_16bit(true)
    .with_native_alpha(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true)
    .with_enforces_max_input_bytes(true)
    .with_gain_map(true)
    .with_reconstructs_hdr(true)
    .with_threads_supported_range(1, if cfg!(feature = "parallel") { 256 } else { 1 });

// ── Supported descriptors ──────────────────────────────────────────────────

/// Pixel formats this decoder can produce natively (8-bit and 16-bit).
static DECODE_DESCRIPTORS: &[PixelDescriptor] = &[
    PixelDescriptor::RGB8_SRGB,
    PixelDescriptor::RGBA8_SRGB,
    PixelDescriptor::BGRA8_SRGB,
    PixelDescriptor::RGB16_SRGB,
    PixelDescriptor::RGBA16_SRGB,
];

// ── Decoder Config ─────────────────────────────────────────────────────────

/// HEIC decoder configuration implementing [`zencodec::decode::DecoderConfig`].
///
/// Wraps [`crate::DecoderConfig`] for use with the zencodec trait system.
///
/// Supplement extraction (gain map, depth map) is **opt-in**: both default
/// to `false`. Enable via [`with_extract_gain_map`](Self::with_extract_gain_map)
/// or [`with_extract_depth`](Self::with_extract_depth). The flags propagate
/// to every [`HeicDecodeJob`] created by its `job()` factory.
#[derive(Clone, Debug)]
pub struct HeicDecoderConfig {
    inner: crate::DecoderConfig,
    /// Whether to decode the HDR gain map auxiliary image when present.
    ///
    /// Default: `false`. Container metadata (`ImageInfo.supplements.gain_map`,
    /// `GainMapPresence`) is always populated cheaply during probe regardless
    /// of this flag — only the pixel decode is gated.
    pub extract_gain_map: bool,
    /// Whether to decode the depth map auxiliary image when present.
    ///
    /// Default: `false`. Container metadata (`ImageInfo.supplements.depth_map`)
    /// is always populated during probe regardless of this flag.
    pub extract_depth: bool,
    /// How to handle the image's stored orientation (`irot`/`imir`).
    ///
    /// Default: [`OrientationHint::Preserve`](zencodec::OrientationHint::Preserve)
    /// — the zencodec ecosystem default. Under `Preserve` the decoder does
    /// **not** bake the orientation into the pixels:
    /// [`decode`](zencodec::decode::DecoderConfig::job)
    /// returns pixels in stored orientation and [`ImageInfo`] reports the
    /// stored dimensions plus the intrinsic [`Orientation`]. Under
    /// [`Correct`](zencodec::OrientationHint::Correct) the decoder applies the
    /// orientation and reports display dimensions with
    /// [`Orientation::Identity`]. Either way
    /// [`ImageInfo::display_width`]/[`display_height`](ImageInfo::display_height)
    /// yield the upright dimensions.
    ///
    /// Set per-job via
    /// [`DecodeJob::with_orientation`](zencodec::decode::DecodeJob::with_orientation).
    pub orientation: zencodec::OrientationHint,
}

impl HeicDecoderConfig {
    /// Create a default HEIC decoder config.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: crate::DecoderConfig::new(),
            extract_gain_map: false,
            extract_depth: false,
            orientation: zencodec::OrientationHint::Preserve,
        }
    }

    /// Access the underlying [`crate::DecoderConfig`].
    #[must_use]
    pub fn inner(&self) -> &crate::DecoderConfig {
        &self.inner
    }

    /// Enable or disable gain map extraction.
    #[must_use]
    pub fn with_extract_gain_map(mut self, extract: bool) -> Self {
        self.extract_gain_map = extract;
        self
    }

    /// Enable or disable depth map extraction.
    #[must_use]
    pub fn with_extract_depth(mut self, extract: bool) -> Self {
        self.extract_depth = extract;
        self
    }

    /// Set how the decoder handles the image's stored orientation. See
    /// [`orientation`](Self::orientation) for semantics. Default
    /// [`OrientationHint::Preserve`](zencodec::OrientationHint::Preserve).
    #[must_use]
    pub fn with_orientation(mut self, hint: zencodec::OrientationHint) -> Self {
        self.orientation = hint;
        self
    }
}

impl Default for HeicDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl zencodec::decode::DecoderConfig for HeicDecoderConfig {
    type Error = At<HeicError>;
    type Job<'a> = HeicDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        &[ImageFormat::Heic]
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &HEIC_DECODE_CAPS
    }

    fn job<'a>(self) -> Self::Job<'a> {
        let extract_gain_map = self.extract_gain_map;
        let extract_depth = self.extract_depth;
        HeicDecodeJob {
            config: self,
            stop: None,
            limits: ResourceLimits::none(),
            policy: None,
            extract_gain_map,
            extract_depth,
            gain_map_render: zencodec::GainMapRender::default(),
        }
    }
}

// ── Decode Job ─────────────────────────────────────────────────────────────

/// Per-operation HEIC decode job.
///
/// Supplement extraction flags (`extract_gain_map`, `extract_depth`) are
/// inherited from [`HeicDecoderConfig`] and can be overridden per-job.
pub struct HeicDecodeJob {
    config: HeicDecoderConfig,
    stop: Option<zencodec::StopToken>,
    limits: ResourceLimits,
    policy: Option<DecodePolicy>,
    /// Whether to decode the HDR gain map when present (default: `false`).
    pub extract_gain_map: bool,
    /// Whether to decode the depth map when present (default: `false`).
    pub extract_depth: bool,
    /// Gain-map rendition intent (zencodec 0.1.21). `Components` (and
    /// `ReconstructHdr`, downgraded — heic surfaces, it does not apply)
    /// additionally surfaces the decoded gain map as a
    /// [`zencodec::decode::DecodedGainMap`]. Default `BaseOnly`.
    gain_map_render: zencodec::GainMapRender,
}

/// Apply [`DecodePolicy`] to an [`ImageInfo`], stripping metadata fields
/// that the policy disallows. Returns the filtered info.
fn apply_policy(policy: Option<&DecodePolicy>, mut info: ImageInfo) -> ImageInfo {
    if let Some(policy) = policy {
        if !policy.resolve_icc(true) {
            info.source_color.icc_profile = None;
        }
        if !policy.resolve_exif(true) {
            info.embedded_metadata.exif = None;
        }
        if !policy.resolve_xmp(true) {
            info.embedded_metadata.xmp = None;
        }
    }
    info
}

impl HeicDecodeJob {
    /// Build native limits from zencodec ResourceLimits.
    fn native_limits(&self) -> Option<crate::Limits> {
        if !self.limits.has_any() {
            return None;
        }
        let mut limits = crate::Limits::default();
        limits.max_width = self.limits.max_width.map(u64::from);
        limits.max_height = self.limits.max_height.map(u64::from);
        limits.max_pixels = self.limits.max_pixels;
        limits.max_memory_bytes = self.limits.max_memory_bytes;
        Some(limits)
    }
}

/// Build the "available descriptors" list for format negotiation based on
/// image properties (alpha, bit depth).
fn available_descriptors(has_alpha: bool, bit_depth: u8) -> alloc::vec::Vec<PixelDescriptor> {
    let mut available = alloc::vec::Vec::with_capacity(5);
    if bit_depth > 8 {
        // 16-bit formats first when source is >8-bit
        if has_alpha {
            available.push(PixelDescriptor::RGBA16_SRGB);
            available.push(PixelDescriptor::RGB16_SRGB);
        } else {
            available.push(PixelDescriptor::RGB16_SRGB);
            available.push(PixelDescriptor::RGBA16_SRGB);
        }
    }
    // 8-bit formats
    if has_alpha {
        available.push(PixelDescriptor::RGBA8_SRGB);
        available.push(PixelDescriptor::BGRA8_SRGB);
        available.push(PixelDescriptor::RGB8_SRGB);
    } else {
        available.push(PixelDescriptor::RGB8_SRGB);
        available.push(PixelDescriptor::RGBA8_SRGB);
        available.push(PixelDescriptor::BGRA8_SRGB);
    }
    available
}

/// Check whether a negotiated descriptor is a 16-bit format.
fn is_16bit(desc: PixelDescriptor) -> bool {
    desc == PixelDescriptor::RGB16_SRGB || desc == PixelDescriptor::RGBA16_SRGB
}

/// Map a negotiated PixelDescriptor to a native PixelLayout for 8-bit decode.
fn descriptor_to_layout(desc: PixelDescriptor) -> crate::PixelLayout {
    if desc.pixel_format() == PixelDescriptor::BGRA8_SRGB.pixel_format() {
        crate::PixelLayout::Bgra8
    } else if desc.pixel_format() == PixelDescriptor::RGBA8_SRGB.pixel_format() {
        crate::PixelLayout::Rgba8
    } else {
        crate::PixelLayout::Rgb8
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for HeicDecodeJob {
    type Error = At<HeicError>;
    type Dec = HeicDecoder<'a>;
    type StreamDec = HeicStreamDecoder;
    type AnimationFrameDec = Unsupported<At<HeicError>>;

    fn with_stop(mut self, stop: zencodec::StopToken) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// heic applies gain maps natively (`reconstructs_hdr()` is `true`):
    /// `ReconstructHdr` decodes the SDR base, applies the Apple HDR gain map
    /// via ultrahdr-core, and returns linear f32/f16 HDR pixels with the
    /// content-light-level / mastering-display envelope populated.
    fn with_gain_map_render(mut self, render: zencodec::GainMapRender) -> Self {
        self.gain_map_render = render;
        self
    }

    fn with_policy(mut self, policy: DecodePolicy) -> Self {
        self.policy = Some(policy);
        self
    }

    fn with_orientation(mut self, hint: zencodec::OrientationHint) -> Self {
        self.config.orientation = hint;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        let apply = will_auto_orient(self.config.orientation);
        let (w, h, orientation) = reported_dims_and_orientation(
            native.width,
            native.height,
            primary_orientation(data),
            apply,
        );
        Ok(apply_policy(
            self.policy.as_ref(),
            build_image_info_lightweight(&native, w, h, orientation),
        ))
    }

    fn probe_full(&self, data: &[u8]) -> Result<ImageInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        // Parse the HEIF container once and extract all metadata from it
        let stop_ref: &dyn enough::Stop = match self.stop {
            Some(ref s) => s,
            None => &enough::Unstoppable,
        };
        let container = crate::heif::parse(data, stop_ref).ok();
        let apply = will_auto_orient(self.config.orientation);
        let intrinsic = container
            .as_ref()
            .and_then(|c| c.primary_item())
            .map(|it| compose_orientation(&it.transforms))
            .unwrap_or(Orientation::Identity);
        let (w, h, orientation) =
            reported_dims_and_orientation(native.width, native.height, intrinsic, apply);
        Ok(apply_policy(
            self.policy.as_ref(),
            build_image_info_full(&native, container.as_ref(), w, h, orientation),
        ))
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        let available = available_descriptors(native.has_alpha, native.bit_depth);
        let base_desc = available[0]; // default for this image
        let desc = cicp_descriptor(
            base_desc,
            native.color_primaries,
            native.transfer_characteristics,
        );
        // Report the post-orientation output dims + what the decoder applies:
        // `Correct` bakes the intrinsic orientation (output = display dims);
        // `Preserve` applies nothing (output = stored dims, caller orients).
        let apply = will_auto_orient(self.config.orientation);
        let intrinsic = primary_orientation(data);
        let (w, h, _) =
            reported_dims_and_orientation(native.width, native.height, intrinsic, apply);
        let orientation_applied = if apply {
            intrinsic
        } else {
            Orientation::Identity
        };
        Ok(OutputInfo::full_decode(w, h, desc).with_orientation_applied(orientation_applied))
    }

    fn decoder(
        mut self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelDescriptor],
    ) -> Result<HeicDecoder<'a>, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        let thread_count = policy_to_threads(self.limits.threading());
        let stop = self.stop.take();
        let limits = self.native_limits();
        Ok(HeicDecoder {
            config: self.config,
            data,
            preferred: preferred.to_vec(),
            stop,
            limits,
            thread_count,
            policy: self.policy,
            extract_gain_map: self.extract_gain_map,
            extract_depth: self.extract_depth,
            gain_map_render: self.gain_map_render,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        // Probe for image properties
        let probe_info = crate::ImageInfo::from_bytes(&data).ok();
        let has_alpha = probe_info.as_ref().is_some_and(|pi| pi.has_alpha);
        let bit_depth = probe_info.as_ref().map_or(8, |pi| pi.bit_depth);

        // Negotiate output format
        let available = available_descriptors(has_alpha, bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available)
            .ok_or_else(|| at!(HeicError::InvalidData("pixel format negotiation failed")))?;

        if is_16bit(negotiated) {
            // 16-bit: full decode, then push rows
            let dec = self.decoder(data, preferred)?;
            let output = <HeicDecoder<'_> as zencodec::decode::Decode>::decode(dec)?;
            let ps = output.pixels();
            let desc = ps.descriptor();
            let w = ps.width();
            let h = ps.rows();
            sink.begin(w, h, desc)
                .map_err(|e| at!(HeicError::Sink(e)))?;
            let mut dst = sink
                .provide_next_buffer(0, h, w, desc)
                .map_err(|e| at!(HeicError::Sink(e)))?;
            for row in 0..h {
                dst.row_mut(row).copy_from_slice(ps.row(row));
            }
            drop(dst);
            sink.finish().map_err(|e| at!(HeicError::Sink(e)))?;
            let info = output.info();
            return Ok(OutputInfo::full_decode(info.width, info.height, desc));
        }

        // 8-bit: use native decode_rows for grid streaming
        let layout = descriptor_to_layout(negotiated);
        let desc = if let Some(ref pi) = probe_info {
            cicp_descriptor(
                layout_to_descriptor(layout),
                pi.color_primaries,
                pi.transfer_characteristics,
            )
        } else {
            layout_to_descriptor(layout)
        };

        // Stream decode via native decode_rows, adapting to zencodec sink
        let probe_width = probe_info.as_ref().map_or(0, |pi| pi.width);
        let mut adapter = RowSinkAdapter {
            inner: sink,
            descriptor: desc,
            width: probe_width,
            strip_buf: alloc::vec::Vec::new(),
            pending_y: None,
            pending_height: 0,
            deferred_error: None,
        };

        let thread_count = policy_to_threads(self.limits.threading());
        let native_limits = self.native_limits();
        let mut req = self
            .config
            .inner
            .decode_request(&data)
            .with_output_layout(layout);
        if let Some(ref limits) = native_limits {
            req = req.with_limits(limits);
        }
        if let Some(ref stop) = self.stop {
            req = req.with_stop(stop);
        }
        if thread_count > 0 {
            req = req.with_max_threads(thread_count);
        }

        // Call begin with probe dimensions if available
        let probe_height = probe_info.as_ref().map_or(0, |pi| pi.height);
        adapter
            .inner
            .begin(probe_width, probe_height, desc)
            .map_err(|e| at!(HeicError::Sink(e)))?;

        let (w, h) = req.decode_rows(&mut adapter)?;
        // Check for deferred sink errors from demand() calls
        adapter.take_deferred_error()?;
        // Flush the last strip that was written by the native decoder
        adapter.flush_pending()?;
        adapter
            .inner
            .finish()
            .map_err(|e| at!(HeicError::Sink(e)))?;
        Ok(OutputInfo::full_decode(w, h, desc))
    }

    fn streaming_decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelDescriptor],
    ) -> Result<HeicStreamDecoder, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::LimitExceeded(limit_exceeded_msg(e))))?;
        let thread_count = policy_to_threads(self.limits.threading());
        HeicStreamDecoder::new(
            &data,
            preferred,
            self.native_limits().as_ref(),
            self.stop,
            thread_count,
        )
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<Unsupported<At<HeicError>>, At<HeicError>> {
        Err(at!(HeicError::Unsupported(
            "HEIC does not support animation decoding",
        )))
    }
}

// ── RowSink adapter ────────────────────────────────────────────────────────

/// Adapts `zencodec::decode::DecodeRowSink` to the native `crate::RowSink` interface.
///
/// The native decoder writes tightly packed pixels into a flat buffer returned
/// by `RowSink::demand()`. This adapter buffers one strip at a time, then
/// flushes it to the zencodec sink on the next `demand()` call.
struct RowSinkAdapter<'a> {
    inner: &'a mut dyn DecodeRowSink,
    descriptor: PixelDescriptor,
    width: u32,
    strip_buf: alloc::vec::Vec<u8>,
    /// Pending strip that was written by the native decoder but not yet
    /// flushed to the zencodec sink.
    pending_y: Option<u32>,
    pending_height: u32,
    /// Deferred sink error from within `demand()` (which can't return Result).
    deferred_error: Option<At<HeicError>>,
}

impl RowSinkAdapter<'_> {
    /// Flush any pending strip data to the zencodec sink.
    fn flush_pending(&mut self) -> Result<(), At<HeicError>> {
        if let Some(y) = self.pending_y.take() {
            let bpp = self.descriptor.bytes_per_pixel();
            let row_bytes = self.width as usize * bpp;
            let mut dst = self
                .inner
                .provide_next_buffer(y, self.pending_height, self.width, self.descriptor)
                .map_err(|e| at!(HeicError::Sink(e)))?;
            for row in 0..self.pending_height {
                let src_start = row as usize * row_bytes;
                dst.row_mut(row)
                    .copy_from_slice(&self.strip_buf[src_start..src_start + row_bytes]);
            }
        }
        Ok(())
    }

    /// Flush pending strip, storing any error for later propagation.
    /// Used inside `demand()` which cannot return `Result`.
    fn flush_pending_deferred(&mut self) {
        if let Err(e) = self.flush_pending() {
            self.deferred_error = Some(e);
        }
    }

    /// Take any deferred error from a prior `demand()` call.
    fn take_deferred_error(&mut self) -> Result<(), At<HeicError>> {
        match self.deferred_error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl crate::RowSink for RowSinkAdapter<'_> {
    fn demand(&mut self, y: u32, height: u32, min_bytes: usize) -> &mut [u8] {
        // Infer width if not set from probe
        if self.width == 0 {
            let bpp = self.descriptor.bytes_per_pixel();
            if height > 0 && bpp > 0 {
                self.width = (min_bytes / height as usize / bpp) as u32;
            }
        }

        // Flush the previous strip to the zencodec sink
        self.flush_pending_deferred();

        // Record this strip as pending
        self.pending_y = Some(y);
        self.pending_height = height;

        // Return buffer for the native decoder to write into
        self.strip_buf.resize(min_bytes, 0);
        &mut self.strip_buf[..min_bytes]
    }
}

// ── Decoder ────────────────────────────────────────────────────────────────

/// Single-image HEIC decoder.
pub struct HeicDecoder<'a> {
    config: HeicDecoderConfig,
    data: Cow<'a, [u8]>,
    preferred: alloc::vec::Vec<PixelDescriptor>,
    stop: Option<zencodec::StopToken>,
    limits: Option<crate::Limits>,
    /// Thread count from threading policy (0 = unlimited/default).
    thread_count: usize,
    policy: Option<DecodePolicy>,
    /// Whether to decode the HDR gain map when present.
    extract_gain_map: bool,
    /// Whether to decode the depth map when present.
    extract_depth: bool,
    gain_map_render: zencodec::GainMapRender,
}

impl zencodec::decode::Decode for HeicDecoder<'_> {
    type Error = At<HeicError>;

    fn decode(self) -> Result<DecodeOutput, At<HeicError>> {
        let data: &[u8] = &self.data;
        let preferred = &self.preferred;

        // Probe for image info — best-effort.
        let probe_info = crate::ImageInfo::from_bytes(data).ok();
        let bit_depth = probe_info.as_ref().map_or(8, |pi| pi.bit_depth);
        let has_alpha = probe_info.as_ref().is_some_and(|pi| pi.has_alpha);

        // Orientation policy: `Correct` bakes orientation into the pixels and
        // reports `Identity`; `Preserve` (default) keeps stored orientation and
        // reports the intrinsic orientation on the output `ImageInfo`.
        let apply_orientation = will_auto_orient(self.config.orientation);

        // Negotiate output format
        let available = available_descriptors(has_alpha, bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available)
            .ok_or_else(|| at!(HeicError::InvalidData("pixel format negotiation failed")))?;

        // Gain-map rendition intent (zencodec contract): BaseOnly decodes the
        // SDR base; Components surfaces the decoded gain map alongside the
        // base; ReconstructHdr applies the gain map natively. Unknown future
        // modes are refused, never mis-rendered.
        let (surface_components, reconstruct_target) = match self.gain_map_render {
            zencodec::GainMapRender::BaseOnly => (false, None),
            zencodec::GainMapRender::Components => (true, None),
            zencodec::GainMapRender::ReconstructHdr { target_headroom } => {
                (false, Some(target_headroom))
            }
            _ => {
                return Err(at!(HeicError::Unsupported(
                    "unrecognized GainMapRender mode"
                )));
            }
        };
        let reconstructing =
            reconstruct_target.is_some() && probe_info.as_ref().is_some_and(|pi| pi.has_gain_map);
        // HEIC gain-map items are stored display-oriented (they carry no
        // irot/imir of their own), while the primary may. Reconstruct in
        // display space so the gain map and the base align — otherwise the
        // apply would stretch a portrait gain map across a landscape base.
        // `report_orientation` below then correctly reports Identity.
        let apply_orientation = apply_orientation || reconstructing;

        let (buf, width, height, has_alpha): (PixelBuffer, u32, u32, bool) =
            if is_16bit(negotiated) && !reconstructing {
                // 16-bit path: decode to YCbCr frame, then convert at full precision.
                let mut req = self
                    .config
                    .inner
                    .decode_request(data)
                    .with_apply_orientation(apply_orientation);
                if let Some(ref limits) = self.limits {
                    req = req.with_limits(limits);
                }
                if let Some(ref stop) = self.stop {
                    req = req.with_stop(stop);
                }
                if self.thread_count > 0 {
                    req = req.with_max_threads(self.thread_count);
                }
                let frame = req.decode_yuv()?;

                let has_alpha = frame.alpha_plane.is_some();
                let w = frame.cropped_width();
                let h = frame.cropped_height();

                let wants_alpha = negotiated == PixelDescriptor::RGBA16_SRGB;
                if has_alpha || wants_alpha {
                    let desc = cicp_descriptor(
                        PixelDescriptor::RGBA16_SRGB,
                        frame.color_primaries as u16,
                        frame.transfer_characteristics as u16,
                    );
                    let rgba_data = frame.to_rgba16().map_err(crate::error::at_core)?;
                    let pixels = u16_vec_to_rgba(rgba_data);
                    let pb = PixelBuffer::from_pixels_erased(pixels, w, h)
                        .map_err_at(|_| HeicError::InvalidData("pixel count mismatch"))?
                        .with_descriptor(desc);
                    (pb, w, h, true)
                } else {
                    let desc = cicp_descriptor(
                        PixelDescriptor::RGB16_SRGB,
                        frame.color_primaries as u16,
                        frame.transfer_characteristics as u16,
                    );
                    let rgb_data = frame.to_rgb16().map_err(crate::error::at_core)?;
                    let pixels = u16_vec_to_rgb(rgb_data);
                    let pb = PixelBuffer::from_pixels_erased(pixels, w, h)
                        .map_err_at(|_| HeicError::InvalidData("pixel count mismatch"))?
                        .with_descriptor(desc);
                    (pb, w, h, false)
                }
            } else {
                // 8-bit path: use negotiated layout for decode.
                // ReconstructHdr forces RGBA8 — `apply_gainmap` consumes 8-bit
                // SDR input (gain-map HEICs carry an 8-bit SDR base by design).
                let layout = if reconstructing {
                    crate::PixelLayout::Rgba8
                } else {
                    descriptor_to_layout(negotiated)
                };
                let mut req = self
                    .config
                    .inner
                    .decode_request(data)
                    .with_output_layout(layout)
                    .with_apply_orientation(apply_orientation);
                if let Some(ref limits) = self.limits {
                    req = req.with_limits(limits);
                }
                if let Some(ref stop) = self.stop {
                    req = req.with_stop(stop);
                }
                if self.thread_count > 0 {
                    req = req.with_max_threads(self.thread_count);
                }
                let native_output = req.decode()?;
                let has_alpha =
                    layout == crate::PixelLayout::Rgba8 || layout == crate::PixelLayout::Bgra8;
                let w = native_output.width;
                let h = native_output.height;
                let mut pb = raw_to_pixel_buffer(native_output.data, w, h, native_output.layout)?;
                // Apply CICP from probe
                if let Some(ref pi) = probe_info {
                    let desc = cicp_descriptor(
                        pb.descriptor(),
                        pi.color_primaries,
                        pi.transfer_characteristics,
                    );
                    pb = pb.with_descriptor(desc);
                }
                (pb, w, h, has_alpha)
            };

        // Build ImageInfo with all available metadata.
        // Parse the HEIF container once for all metadata extraction.
        let stop_ref: &dyn enough::Stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let container = crate::heif::parse(data, stop_ref).ok();
        let fallback_info = crate::ImageInfo {
            width,
            height,
            has_alpha,
            bit_depth: 8,
            chroma_format: 1,
            has_exif: false,
            has_xmp: false,
            has_thumbnail: false,
            color_primaries: 2,
            transfer_characteristics: 2,
            matrix_coefficients: 2,
            video_full_range: false,
            has_icc_profile: false,
            has_depth: false,
            has_gain_map: false,
            exif: None,
            xmp: None,
            icc_profile: None,
        };
        let pi_ref = probe_info.as_ref().unwrap_or(&fallback_info);
        // `width`/`height` are the decoded frame's dims — stored orientation when
        // `Preserve` (not baked), display orientation when `Correct` (baked) —
        // so they ARE the dims to report. Tag with the intrinsic orientation
        // unless it was baked in.
        let report_orientation = if apply_orientation {
            Orientation::Identity
        } else {
            container
                .as_ref()
                .and_then(|c| c.primary_item())
                .map(|it| compose_orientation(&it.transforms))
                .unwrap_or(Orientation::Identity)
        };
        let info = apply_policy(
            self.policy.as_ref(),
            build_image_info_full(
                pi_ref,
                container.as_ref(),
                width,
                height,
                report_orientation,
            ),
        );
        // Native HDR reconstruction: apply the gain map to the SDR base
        // before packaging the output.
        let (buf, info) = if reconstructing {
            reconstruct_hdr_base(
                buf,
                info,
                data,
                container.as_ref(),
                reconstruct_target.flatten(),
                preferred,
                stop_ref,
            )?
        } else {
            (buf, info)
        };

        let mut output =
            DecodeOutput::new(buf, info).with_source_encoding_details(HeicSourceEncoding);

        // Attach auxiliary image metadata as an extension if available.
        if let Some(ref pi) = probe_info {
            let aux_types = if let Some(ref c) = container {
                let primary_id = c.primary_item_id;
                c.find_all_auxiliary_items(primary_id)
                    .into_iter()
                    .map(|(_id, urn)| AuxiliaryImageType::from_urn(&urn))
                    .collect()
            } else {
                alloc::vec::Vec::new()
            };
            output.extensions_mut().insert(HeicAuxiliaryInfo {
                has_depth: pi.has_depth,
                has_gain_map: pi.has_gain_map,
                auxiliary_types: aux_types,
            });

            // Decode and attach the HDR gain map if requested and present.
            // (ReconstructHdr consumed the gain map above — `reconstructing`
            // and `surface_components` are never both true.)
            if (self.extract_gain_map || surface_components)
                && pi.has_gain_map
                && let Ok(gain_map) = crate::decode::decode_gain_map(data, &[crate::Backend::Rust])
            {
                if surface_components {
                    // heic surfaces gain maps as luma-only gray8; params come
                    // from the ISO 21496-1 tmap payload when present, else
                    // the Apple EXIF MakerNote headroom.
                    let params =
                        gain_map_params_from(&gain_map, container.as_ref()).unwrap_or_default();
                    let gm_info = zencodec::gainmap::GainMapInfo::new(
                        params,
                        gain_map.width,
                        gain_map.height,
                        1,
                    );
                    if let Ok(pixels) = zenpixels::PixelBuffer::from_vec(
                        gain_map.data.clone(),
                        gain_map.width,
                        gain_map.height,
                        zenpixels::PixelDescriptor::GRAY8_SRGB,
                    ) {
                        output
                            .extensions_mut()
                            .insert(zencodec::decode::DecodedGainMap::new(pixels, gm_info));
                    }
                }
                output.extensions_mut().insert(gain_map);
            }

            // Decode and attach the depth map if requested and present.
            if self.extract_depth
                && pi.has_depth
                && let Ok(depth_map) = crate::decode::decode_depth(data, &[crate::Backend::Rust])
            {
                output.extensions_mut().insert(depth_map);
            }
        }

        Ok(output)
    }
}

// ── Streaming Decoder ──────────────────────────────────────────────────

/// Grid image state for tile-row streaming.
struct GridState {
    tile_data: alloc::vec::Vec<alloc::vec::Vec<u8>>,
    tile_config: crate::heif::HevcDecoderConfig,
    rows: u32,
    cols: u32,
    tile_width: u32,
    tile_height: u32,
    output_width: u32,
    output_height: u32,
    color_override: Option<(bool, u8)>,
    layout: crate::PixelLayout,
}

/// HEIC streaming decoder: emits one tile-row per `next_batch()` for grid
/// images (real streaming with memory savings), or the full image as a
/// single strip for non-grid images.
pub struct HeicStreamDecoder {
    info: ImageInfo,
    descriptor: PixelDescriptor,
    y_offset: u32,
    grid: Option<GridState>,
    current_grid_row: u32,
    strip_buffer: alloc::vec::Vec<u8>,
    full_pixels: Option<PixelBuffer>,
    stop: Option<zencodec::StopToken>,
}

impl HeicStreamDecoder {
    /// Default strip height for non-grid fallback.
    const FALLBACK_STRIP_HEIGHT: u32 = 64;

    /// Construct a streaming decoder for the given HEIC data.
    fn new(
        data: &[u8],
        preferred: &[PixelDescriptor],
        limits: Option<&crate::Limits>,
        owned_stop: Option<zencodec::StopToken>,
        thread_count: usize,
    ) -> Result<Self, At<HeicError>> {
        let stop_ref: &dyn enough::Stop = match owned_stop {
            Some(ref s) => s,
            None => &enough::Unstoppable,
        };

        // Probe for metadata
        let probe_info = crate::ImageInfo::from_bytes(data).ok();

        let config = crate::DecoderConfig::new();
        let pi = probe_info
            .as_ref()
            .ok_or_else(|| at!(HeicError::InvalidData("cannot probe HEIC header")))?;

        // Parse container once for metadata extraction and grid init
        let container = crate::heif::parse(data, stop_ref).ok();

        // Build ImageInfo for the trait (uses pre-parsed container). The
        // streaming path only handles transform-free grid images (grid
        // eligibility bails on any `irot`/`imir`/`clap`), so the intrinsic
        // orientation is always Identity here and stored == display dims.
        let info = build_image_info_full(
            pi,
            container.as_ref(),
            pi.width,
            pi.height,
            Orientation::Identity,
        );

        // Try grid path for 8-bit, no-alpha images
        if pi.bit_depth <= 8
            && !pi.has_alpha
            && let Some(grid_state) =
                Self::try_init_grid(container.as_ref(), data, preferred, limits, stop_ref, pi)?
        {
            let descriptor = cicp_descriptor(
                layout_to_descriptor(grid_state.layout),
                pi.color_primaries,
                pi.transfer_characteristics,
            );
            return Ok(Self {
                info,
                descriptor,
                y_offset: 0,
                grid: Some(grid_state),
                current_grid_row: 0,
                strip_buffer: alloc::vec::Vec::new(),
                full_pixels: None,
                stop: owned_stop,
            });
        }

        // Non-grid fallback: full decode upfront
        let available = available_descriptors(pi.has_alpha, pi.bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available)
            .ok_or_else(|| at!(HeicError::InvalidData("pixel format negotiation failed")))?;

        let pixels: PixelBuffer = if is_16bit(negotiated) {
            let mut req = config.decode_request(data);
            if let Some(lim) = limits {
                req = req.with_limits(lim);
            }
            req = req.with_stop(stop_ref);
            if thread_count > 0 {
                req = req.with_max_threads(thread_count);
            }
            let frame = req.decode_yuv()?;
            let has_alpha = frame.alpha_plane.is_some();

            let wants_alpha = negotiated == PixelDescriptor::RGBA16_SRGB;
            if has_alpha || wants_alpha {
                let desc = cicp_descriptor(
                    PixelDescriptor::RGBA16_SRGB,
                    frame.color_primaries as u16,
                    frame.transfer_characteristics as u16,
                );
                let rgba_data = frame.to_rgba16().map_err(crate::error::at_core)?;
                let pixels: alloc::vec::Vec<Rgba<u16>> = rgba_data
                    .chunks_exact(4)
                    .map(|c| Rgba {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    })
                    .collect();
                let w = frame.cropped_width();
                let h = frame.cropped_height();
                PixelBuffer::from_pixels_erased(pixels, w, h)
                    .map_err_at(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc)
            } else {
                let desc = cicp_descriptor(
                    PixelDescriptor::RGB16_SRGB,
                    frame.color_primaries as u16,
                    frame.transfer_characteristics as u16,
                );
                let rgb_data = frame.to_rgb16().map_err(crate::error::at_core)?;
                let pixels = u16_vec_to_rgb(rgb_data);
                let w = frame.cropped_width();
                let h = frame.cropped_height();
                PixelBuffer::from_pixels_erased(pixels, w, h)
                    .map_err_at(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc)
            }
        } else {
            let layout = descriptor_to_layout(negotiated);
            let mut req = config.decode_request(data).with_output_layout(layout);
            if let Some(lim) = limits {
                req = req.with_limits(lim);
            }
            req = req.with_stop(stop_ref);
            if thread_count > 0 {
                req = req.with_max_threads(thread_count);
            }
            let native_output = req.decode()?;
            let mut pb = raw_to_pixel_buffer(
                native_output.data,
                native_output.width,
                native_output.height,
                native_output.layout,
            )?;
            let desc = cicp_descriptor(
                pb.descriptor(),
                pi.color_primaries,
                pi.transfer_characteristics,
            );
            pb = pb.with_descriptor(desc);
            pb
        };

        let descriptor = pixels.descriptor();
        Ok(Self {
            info,
            descriptor,
            y_offset: 0,
            grid: None,
            current_grid_row: 0,
            strip_buffer: alloc::vec::Vec::new(),
            full_pixels: Some(pixels),
            stop: owned_stop,
        })
    }

    /// Try to initialize grid streaming state. Returns None if not eligible.
    ///
    /// Accepts a pre-parsed container if available, otherwise parses from `data`.
    fn try_init_grid(
        pre_parsed: Option<&crate::heif::HeifContainer<'_>>,
        data: &[u8],
        preferred: &[PixelDescriptor],
        limits: Option<&crate::Limits>,
        stop: &dyn enough::Stop,
        _probe_info: &crate::ImageInfo,
    ) -> Result<Option<GridState>, At<HeicError>> {
        use crate::heif::{self, ColorInfo, FourCC, ItemType};

        stop.check().map_err(|r| at!(HeicError::Cancelled(r)))?;

        // Use pre-parsed container or parse now
        let owned;
        let container: &heif::HeifContainer<'_> = match pre_parsed {
            Some(c) => c,
            None => {
                owned = heif::parse(data, stop)?;
                &owned
            }
        };
        let primary_item = container
            .primary_item()
            .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;

        // Must be a grid with no transforms and no alpha
        if primary_item.item_type != ItemType::Grid {
            return Ok(None);
        }
        if !primary_item.transforms.is_empty() {
            return Ok(None);
        }
        let has_alpha = !container
            .find_auxiliary_items(primary_item.id, "urn:mpeg:hevc:2015:auxid:1")
            .is_empty()
            || !container
                .find_auxiliary_items(
                    primary_item.id,
                    "urn:mpeg:mpegB:cicp:systems:auxiliary:alpha",
                )
                .is_empty();
        if has_alpha {
            return Ok(None);
        }

        // Parse grid descriptor
        let grid_data = container.get_item_data(primary_item.id)?;
        if grid_data.len() < 8 {
            return Err(at!(HeicError::InvalidData("Grid descriptor too short")));
        }

        let flags = grid_data[1];
        let rows = grid_data[2] as u32 + 1;
        let cols = grid_data[3] as u32 + 1;
        let (output_width, output_height) = if (flags & 1) != 0 {
            if grid_data.len() < 12 {
                return Err(at!(HeicError::InvalidData(
                    "Grid descriptor too short for 32-bit dims",
                )));
            }
            (
                u32::from_be_bytes([grid_data[4], grid_data[5], grid_data[6], grid_data[7]]),
                u32::from_be_bytes([grid_data[8], grid_data[9], grid_data[10], grid_data[11]]),
            )
        } else {
            (
                u16::from_be_bytes([grid_data[4], grid_data[5]]) as u32,
                u16::from_be_bytes([grid_data[6], grid_data[7]]) as u32,
            )
        };

        // Always enforce the default resource ceiling when the caller passed no
        // explicit Limits. The old `if let Some(lim)` skipped the check entirely
        // in the common default-construction case, so a crafted grid with the
        // 32-bit-dims flag and output_width/height up to 0xFFFFFFFF drove an
        // uncapped allocation (OOM on 64-bit; usize-overflow undersized strip
        // buffer + OOB index on 32-bit/wasm). Mirror the parent's
        // try_decode_grid_streaming, which uses limits.unwrap_or(&NO_LIMITS).
        limits
            .unwrap_or(&crate::decode::NO_LIMITS)
            .check_dimensions(output_width, output_height)?;

        // Get tile info
        let tile_ids = container.get_item_references(primary_item.id, FourCC::DIMG);
        let expected_tiles = (rows * cols) as usize;
        if tile_ids.len() != expected_tiles {
            return Err(at!(HeicError::InvalidData("Grid tile count mismatch")));
        }

        let first_tile = container
            .get_item(tile_ids[0])
            .ok_or_else(|| at!(HeicError::InvalidData("Missing tile item")))?;
        let tile_config = first_tile
            .hevc_config
            .as_ref()
            .ok_or_else(|| at!(HeicError::InvalidData("Missing tile hvcC config")))?
            .clone();
        let (tile_width, tile_height) = first_tile
            .dimensions
            .ok_or_else(|| at!(HeicError::InvalidData("Missing tile dimensions")))?;

        // Color override from grid item's colr nclx
        let color_override = match &primary_item.color_info {
            Some(ColorInfo::Nclx {
                full_range,
                matrix_coefficients,
                ..
            }) => Some((*full_range, *matrix_coefficients as u8)),
            _ => None,
        };

        // Extract tile data
        let tile_data: alloc::vec::Vec<alloc::vec::Vec<u8>> = tile_ids
            .iter()
            .map(|&tid| container.get_item_data(tid).map(|cow| cow.into_owned()))
            .collect::<Result<_, _>>()?;

        // Negotiate 8-bit layout for grid tiles (no alpha, ≤8-bit)
        let available = available_descriptors(false, 8);
        let negotiated = negotiate_pixel_format(preferred, &available)
            .ok_or_else(|| at!(HeicError::InvalidData("pixel format negotiation failed")))?;
        let layout = descriptor_to_layout(negotiated);

        Ok(Some(GridState {
            tile_data,
            tile_config,
            rows,
            cols,
            tile_width,
            tile_height,
            output_width,
            output_height,
            color_override,
            layout,
        }))
    }

    /// Decode one grid tile-row into `self.strip_buffer`.
    fn decode_grid_row(&mut self) -> Result<Option<(u32, u32, u32)>, At<HeicError>> {
        // Check for cancellation before decoding a tile row
        if let Some(ref stop) = self.stop {
            stop.check().map_err(|r| at!(HeicError::Cancelled(r)))?;
        }

        let grid = self.grid.as_ref().ok_or_else(|| {
            at!(HeicError::InvalidData(
                "grid not initialized for streaming decode"
            ))
        })?;
        let row = self.current_grid_row;
        if row >= grid.rows {
            return Ok(None);
        }

        let strip_h = grid
            .tile_height
            .min(grid.output_height.saturating_sub(row * grid.tile_height));
        if strip_h == 0 {
            return Ok(None);
        }

        let y_offset = row * grid.tile_height;
        let bpp = grid.layout.bytes_per_pixel();
        // checked_mul: even with the dimension cap applied in setup_grid this
        // guards against usize overflow on 32-bit/wasm when a caller passes
        // explicit large limits.
        let strip_bytes = (grid.output_width as usize)
            .checked_mul(strip_h as usize)
            .and_then(|v| v.checked_mul(bpp))
            .ok_or_else(|| at!(HeicError::InvalidData("grid strip size overflow")))?;

        self.strip_buffer.resize(strip_bytes, 0);

        let cols = grid.cols as usize;
        let row_start = row as usize * cols;

        for col in 0..cols {
            let tile_idx = row_start + col;
            if tile_idx >= grid.tile_data.len() {
                break;
            }
            let mut tile_frame =
                crate::hevc::decode_with_config(&grid.tile_config, &grid.tile_data[tile_idx])?;

            if let Some((fr, mc)) = grid.color_override {
                tile_frame.full_range = fr;
                tile_frame.matrix_coeffs = mc;
            }

            let dst_x = col as u32 * grid.tile_width;
            let copy_w = tile_frame
                .cropped_width()
                .min(grid.output_width.saturating_sub(dst_x));
            let copy_h = tile_frame.cropped_height().min(strip_h);

            crate::decode::convert_tile_to_output(
                &tile_frame,
                &mut self.strip_buffer,
                grid.layout,
                dst_x,
                0,
                copy_w,
                copy_h,
                grid.output_width,
            );
        }

        self.current_grid_row += 1;
        Ok(Some((y_offset, grid.output_width, strip_h)))
    }
}

impl zencodec::decode::StreamingDecode for HeicStreamDecoder {
    type Error = At<HeicError>;

    fn next_batch(&mut self) -> Result<Option<(u32, zenpixels::PixelSlice<'_>)>, At<HeicError>> {
        if self.grid.is_some() {
            let result = self.decode_grid_row()?;
            match result {
                None => Ok(None),
                Some((y, width, height)) => {
                    let bpp = self.descriptor.bytes_per_pixel();
                    let stride = width as usize * bpp;
                    let slice = zenpixels::PixelSlice::new(
                        &self.strip_buffer,
                        width,
                        height,
                        stride,
                        self.descriptor,
                    )
                    .map_err(|_| at!(HeicError::InvalidData("failed to create pixel slice")))?;
                    Ok(Some((y, slice)))
                }
            }
        } else if let Some(ref pixels) = self.full_pixels {
            let height = pixels.height();
            if self.y_offset >= height {
                return Ok(None);
            }
            let h = Self::FALLBACK_STRIP_HEIGHT.min(height - self.y_offset);
            let slice = pixels.rows(self.y_offset, h).erase();
            let y = self.y_offset;
            self.y_offset += h;
            Ok(Some((y, slice)))
        } else {
            Ok(None)
        }
    }

    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

// ── Pixel conversion helpers ───────────────────────────────────────────────

/// Convert raw `Vec<u8>` pixel data from the native decoder into a [`PixelBuffer`].
///
/// Uses `PixelBuffer::from_vec()` for zero-copy when possible.
/// For BGR8 layout, uses garb for SIMD-accelerated in-place BGR→RGB swizzle.
fn raw_to_pixel_buffer(
    mut raw: alloc::vec::Vec<u8>,
    w: u32,
    h: u32,
    layout: crate::PixelLayout,
) -> Result<PixelBuffer, At<HeicError>> {
    match layout {
        crate::PixelLayout::Rgb8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with RGB8 descriptor
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGB8_SRGB)
                .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))?)
        }
        crate::PixelLayout::Rgba8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with RGBA8 descriptor
            Ok(
                PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGBA8_SRGB)
                    .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))?,
            )
        }
        crate::PixelLayout::Bgr8 => {
            // In-place BGR→RGB swizzle via garb, then zero-copy wrap
            garb::bytes::rgb_to_bgr_inplace(&mut raw)
                .map_err(|_| at!(HeicError::InvalidData("BGR swizzle size mismatch")))?;
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGB8_SRGB)
                .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))?)
        }
        crate::PixelLayout::Bgra8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with BGRA8 descriptor
            Ok(
                PixelBuffer::from_vec(raw, w, h, PixelDescriptor::BGRA8_SRGB)
                    .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))?,
            )
        }
    }
}

// ── Orientation handling ────────────────────────────────────────────────────

/// Whether the orientation hint requests baking the image's orientation into
/// the decoded pixels. `Correct`/`CorrectAndTransform` bake; `Preserve`,
/// `ExactTransform`, and any future variant do not (the safe default — keep
/// pixels in stored orientation and report the orientation on `ImageInfo`).
/// Mirrors zenjpeg's policy so the two codecs agree.
fn will_auto_orient(hint: zencodec::OrientationHint) -> bool {
    use zencodec::OrientationHint;
    matches!(
        hint,
        OrientationHint::Correct | OrientationHint::CorrectAndTransform(_)
    )
}

/// Compose the net intrinsic orientation from an item's HEIF transforms
/// (`irot` rotation + `imir` mirror) in ipma listing order. `clap`
/// (clean-aperture crop) is a crop, not an orientation, and is ignored.
///
/// The mapping mirrors the pixel ops applied in [`crate::decode`]: `irot`
/// `angle` is clockwise degrees (`rotate_*_cw`); `imir` axis 0 is a top↔bottom
/// flip (`mirror_vertical` ⇒ [`Orientation::FlipV`]) and axis 1 a left↔right
/// flip (`mirror_horizontal` ⇒ [`Orientation::FlipH`]). Composed with
/// [`Orientation::then`] in apply order so the result, applied to stored
/// pixels, equals what a baking decode produces.
fn compose_orientation(transforms: &[crate::heif::Transform]) -> Orientation {
    use crate::heif::Transform;
    let mut o = Orientation::Identity;
    for t in transforms {
        let step = match t {
            Transform::Rotation(r) => match r.angle {
                90 => Orientation::Rotate90,
                180 => Orientation::Rotate180,
                270 => Orientation::Rotate270,
                _ => Orientation::Identity,
            },
            Transform::Mirror(m) => match m.axis {
                0 => Orientation::FlipV,
                1 => Orientation::FlipH,
                _ => Orientation::Identity,
            },
            Transform::CleanAperture(_) => Orientation::Identity,
        };
        o = o.then(step);
    }
    o
}

/// Parse the container and compose the primary item's intrinsic orientation.
/// Returns [`Orientation::Identity`] if parsing fails or there is no primary
/// item. Used by the lightweight probe path, which has no container in hand.
fn primary_orientation(data: &[u8]) -> Orientation {
    crate::heif::parse(data, &enough::Unstoppable)
        .ok()
        .and_then(|c| {
            c.primary_item()
                .map(|it| compose_orientation(&it.transforms))
        })
        .unwrap_or(Orientation::Identity)
}

/// Resolve the dimensions and orientation tag to report on `ImageInfo`, given
/// the display dimensions (as `crate::ImageInfo` reports them — orientation
/// already folded into the dims), the composed intrinsic `orientation`, and
/// whether the decoder will bake the orientation (`apply`).
///
/// - `apply == true` (e.g. `Correct`): the pixels are in display orientation,
///   so report display dims + [`Orientation::Identity`].
/// - `apply == false` (e.g. `Preserve`): the pixels stay in stored orientation,
///   so report the stored dims (un-swap display dims for 90/270 rotations) plus
///   the intrinsic orientation. `display_width`/`display_height` recover the
///   upright dims.
fn reported_dims_and_orientation(
    display_w: u32,
    display_h: u32,
    orientation: Orientation,
    apply: bool,
) -> (u32, u32, Orientation) {
    if apply {
        (display_w, display_h, Orientation::Identity)
    } else if orientation.swaps_axes() {
        (display_h, display_w, orientation)
    } else {
        (display_w, display_h, orientation)
    }
}

// ── Native → trait metadata conversion ─────────────────────────────────────

/// Build a lightweight `zencodec::ImageInfo` from probe data only.
///
/// Does NOT parse the HEIF container or extract ICC/EXIF/XMP/gain map.
/// Used by `probe()` for cheap header-only metadata.
fn build_image_info_lightweight(
    pi: &crate::ImageInfo,
    width: u32,
    height: u32,
    orientation: Orientation,
) -> ImageInfo {
    let mut info = ImageInfo::new(width, height, ImageFormat::Heic)
        .with_sequence(ImageSequence::Multi {
            image_count: Some(1),
            random_access: true,
        })
        .with_orientation(orientation)
        .with_alpha(pi.has_alpha)
        .with_bit_depth(pi.bit_depth)
        .with_channel_count(if pi.has_alpha { 4 } else { 3 })
        .with_source_encoding_details(HeicSourceEncoding)
        .with_supplements({
            let mut s = Supplements::default();
            s.gain_map = pi.has_gain_map;
            s.depth_map = pi.has_depth;
            s
        });

    // Set CICP if we have non-default values
    if pi.color_primaries != 2 || pi.transfer_characteristics != 2 || pi.matrix_coefficients != 2 {
        info = info
            .with_cicp(Cicp::new(
                pi.color_primaries as u8,
                pi.transfer_characteristics as u8,
                pi.matrix_coefficients as u8,
                pi.video_full_range,
            ))
            .with_color_authority(zencodec::ColorAuthority::Cicp);
    }

    // Set gain map presence based on probe info
    if pi.has_gain_map {
        // Probe-only: we know a gain map exists but don't have parsed metadata yet
        info.gain_map = GainMapPresence::Unknown;
    } else {
        info.gain_map = GainMapPresence::Absent;
    }

    info
}

/// Build a complete `zencodec::ImageInfo` with all metadata from a pre-parsed container.
///
/// Extracts ICC profile, EXIF, XMP, and gain map from the container in a
/// single pass, avoiding the cost of re-parsing the HEIF container for each.
fn build_image_info_full(
    pi: &crate::ImageInfo,
    container: Option<&crate::heif::HeifContainer<'_>>,
    width: u32,
    height: u32,
    orientation: Orientation,
) -> ImageInfo {
    let mut info = ImageInfo::new(width, height, ImageFormat::Heic)
        .with_sequence(ImageSequence::Multi {
            image_count: Some(1),
            random_access: true,
        })
        .with_orientation(orientation)
        .with_alpha(pi.has_alpha)
        .with_bit_depth(pi.bit_depth)
        .with_channel_count(if pi.has_alpha { 4 } else { 3 })
        .with_source_encoding_details(HeicSourceEncoding)
        .with_supplements({
            let mut s = Supplements::default();
            s.gain_map = pi.has_gain_map;
            s.depth_map = pi.has_depth;
            s
        });

    // Set gain map presence based on probe info
    if pi.has_gain_map {
        info.gain_map = GainMapPresence::Unknown;
    } else {
        info.gain_map = GainMapPresence::Absent;
    }

    // Set CICP if we have non-default values
    if pi.color_primaries != 2 || pi.transfer_characteristics != 2 || pi.matrix_coefficients != 2 {
        info = info
            .with_cicp(Cicp::new(
                pi.color_primaries as u8,
                pi.transfer_characteristics as u8,
                pi.matrix_coefficients as u8,
                pi.video_full_range,
            ))
            .with_color_authority(zencodec::ColorAuthority::Cicp);
    }

    // Extract all metadata from the pre-parsed container
    if let Some(container) = container {
        let primary_item = container.primary_item();

        // Upgrade gain map presence from Unknown to Available when the
        // container yields real parameters (ISO 21496-1 tmap payload, else
        // Apple EXIF MakerNote headroom). Falls back to Unknown if neither
        // the metadata nor the gain-map item dimensions are present.
        if pi.has_gain_map
            && let Some(ref pri) = primary_item
            && let Some(gm_info) = extract_gain_map_info(container, pri.id)
        {
            info.gain_map = GainMapPresence::Available(alloc::boxed::Box::new(gm_info));
        }

        // ICC profile from colr box
        if pi.has_icc_profile
            && let Some(ref item) = primary_item
            && let Some(crate::heif::ColorInfo::IccProfile(icc)) = &item.color_info
        {
            info = info.with_icc_profile(icc.clone());
        }

        // EXIF extraction
        if let Some(exif) = extract_exif_from_container(container) {
            info = info.with_exif(exif);
        }

        // XMP extraction
        if let Some(xmp) = extract_xmp_from_container(container) {
            info = info.with_xmp(xmp);
        }

        // Extract Content Light Level (cLLi) from primary item properties
        if let Some(ref item) = primary_item
            && let Some(clli) = &item.content_light_level
        {
            info = info.with_content_light_level(ContentLightLevel::new(
                clli.max_content_light_level,
                clli.max_frame_average_light_level,
            ));
        }

        // Extract Mastering Display Colour Volume (mDCv) from primary item properties
        if let Some(ref item) = primary_item
            && let Some(mdcv) = &item.mastering_display
        {
            // Convert from 0.00002 units to float CIE xy (divide by 50000)
            let xy = |v: u16| v as f32 / 50_000.0;
            let primaries_xy = [
                [xy(mdcv.primaries_xy[0].0), xy(mdcv.primaries_xy[0].1)],
                [xy(mdcv.primaries_xy[1].0), xy(mdcv.primaries_xy[1].1)],
                [xy(mdcv.primaries_xy[2].0), xy(mdcv.primaries_xy[2].1)],
            ];
            let white_point_xy = [xy(mdcv.white_point_xy.0), xy(mdcv.white_point_xy.1)];
            // Convert from 0.0001 cd/m² units to float (divide by 10000)
            let max_luminance = mdcv.max_luminance as f32 / 10_000.0;
            let min_luminance = mdcv.min_luminance as f32 / 10_000.0;
            info = info.with_mastering_display(MasteringDisplay::new(
                primaries_xy,
                white_point_xy,
                max_luminance,
                min_luminance,
            ));
        }
    }

    info
}

/// Extract EXIF data from a pre-parsed HEIF container.
fn extract_exif_from_container(
    container: &crate::heif::HeifContainer<'_>,
) -> Option<alloc::vec::Vec<u8>> {
    use crate::heif::FourCC;
    for item_info in &container.item_infos {
        if item_info.item_type != FourCC(*b"Exif") {
            continue;
        }
        let Ok(exif_data) = container.get_item_data(item_info.item_id) else {
            continue;
        };
        if exif_data.len() < 4 {
            continue;
        }
        let tiff_offset =
            u32::from_be_bytes([exif_data[0], exif_data[1], exif_data[2], exif_data[3]]) as usize;
        let tiff_start = 4 + tiff_offset;
        if tiff_start < exif_data.len() {
            return Some(exif_data[tiff_start..].to_vec());
        }
    }
    None
}

/// Extract XMP data from a pre-parsed HEIF container.
fn extract_xmp_from_container(
    container: &crate::heif::HeifContainer<'_>,
) -> Option<alloc::vec::Vec<u8>> {
    use crate::heif::FourCC;
    for item_info in &container.item_infos {
        if item_info.item_type == FourCC(*b"mime")
            && (item_info.content_type.contains("xmp")
                || item_info.content_type.contains("rdf+xml")
                || item_info.content_type == "application/rdf+xml")
            && let Ok(xmp_data) = container.get_item_data(item_info.item_id)
        {
            return Some(xmp_data.into_owned());
        }
    }
    None
}

/// Convert a [`zencodec::LimitExceeded`] to a static error message for [`HeicError::LimitExceeded`].
fn limit_exceeded_msg(_e: zencodec::LimitExceeded) -> &'static str {
    "input data size exceeds max_input_bytes"
}

/// Apple HDR gain-map parameters from the primary item's EXIF MakerNote
/// headroom (`ultrahdr_core::{parse_exif_for_apple_hdr, from_apple_headroom}`).
///
/// Legacy Apple aux-item HEICs carry no ISO 21496-1 payload and no `hdrgm:`
/// XMP — the MakerNote HDR headroom is their canonical parameter source
/// (validated against real iPhone captures). Returns `None` when the EXIF
/// or headroom is absent.
fn apple_gain_map_params(
    container: &crate::heif::HeifContainer<'_>,
) -> Option<zencodec::GainMapParams> {
    let exif = extract_exif_from_container(container)?;
    let info = ultrahdr_core::parse_exif_for_apple_hdr(&exif)?;
    ultrahdr_core::from_apple_headroom(&info)
}

/// Gain-map parameters for a decoded [`crate::HdrGainMap`].
///
/// The ISO 21496-1 binary payload (HEIF Amendment 1 `tmap` — iOS 18+
/// "Adaptive HDR" and Samsung HDR files) is authoritative when present: it
/// carries the producer's real per-channel gain curve (gamma, min/max,
/// offsets), where the MakerNote fallback can only synthesize a curve from
/// the scalar headroom. Legacy Apple aux-item files have no ISO payload and
/// use the MakerNote.
fn gain_map_params_from(
    gain_map: &crate::HdrGainMap,
    container: Option<&crate::heif::HeifContainer<'_>>,
) -> Option<zencodec::GainMapParams> {
    if let Some(iso) = gain_map.iso21496.as_deref()
        && let Ok(params) =
            zencodec::gainmap::parse_iso21496_fmt(iso, zencodec::gainmap::Iso21496Format::AvifTmap)
    {
        return Some(params);
    }
    container.and_then(apple_gain_map_params)
}

/// Build a [`GainMapInfo`] from the container's gain-map metadata without
/// decoding pixels.
///
/// Mirrors `decode_gain_map`'s precedence: the ISO 21496-1 `tmap` derived
/// item first (params from its binary payload, dimensions from the
/// referenced gain-map image item), then the legacy Apple aux item (`ispe`
/// dimensions + EXIF MakerNote headroom). Returns `None` if neither yields
/// parameters — caller falls back to `GainMapPresence::Unknown`.
fn extract_gain_map_info(
    container: &crate::heif::HeifContainer<'_>,
    primary_id: u32,
) -> Option<GainMapInfo> {
    if let Some((_tmap_id, gainmap_id, iso)) = crate::decode::find_tmap_gain_map(container)
        && let Ok(params) =
            zencodec::gainmap::parse_iso21496_fmt(&iso, zencodec::gainmap::Iso21496Format::AvifTmap)
        && let Some(item) = container.get_item(gainmap_id)
        && let Some((width, height)) = item.dimensions
    {
        // heic surfaces gain maps as luma-only (single channel).
        return Some(GainMapInfo::new(params, width, height, 1));
    }

    let aux_ids =
        container.find_auxiliary_items(primary_id, "urn:com:apple:photo:2020:aux:hdrgainmap");
    let &gainmap_id = aux_ids.first()?;
    let aux_item = container.get_item(gainmap_id)?;
    let (width, height) = aux_item.dimensions?;
    let params = apple_gain_map_params(container)?;
    // Apple HDR gain maps are luma-only (single channel).
    Some(GainMapInfo::new(params, width, height, 1))
}

/// Apply the HDR gain map to the decoded SDR base (`GainMapRender::ReconstructHdr`).
///
/// The caller forced the base decode to RGBA8 (`apply_gainmap` consumes 8-bit
/// SDR input). `None` target reconstructs at the gain map's encoded maximum
/// (`alternate_hdr_headroom` is log2 of the alternate/SDR peak ratio). A
/// present-but-undecodable gain map or missing headroom metadata is an error —
/// the caller asked for an HDR rendition, and silently returning SDR would
/// misrepresent the image.
fn reconstruct_hdr_base(
    sdr: PixelBuffer,
    mut info: ImageInfo,
    data: &[u8],
    container: Option<&crate::heif::HeifContainer<'_>>,
    target_headroom: Option<f32>,
    preferred: &[PixelDescriptor],
    stop: &dyn enough::Stop,
) -> Result<(PixelBuffer, ImageInfo), At<HeicError>> {
    use ultrahdr_core::gainmap::{HdrOutputFormat, apply_gainmap};

    /// SDR reference white (cd/m²) — 1.0 in the linear output maps here.
    const SDR_WHITE_NITS: f32 = 203.0;

    let gain_map = crate::decode::decode_gain_map(data, &[crate::Backend::Rust])?;
    let params = gain_map_params_from(&gain_map, container).ok_or_else(|| {
        at!(HeicError::InvalidData(
            "ReconstructHdr: no gain-map parameters (neither ISO 21496-1 tmap \
             metadata nor Apple EXIF MakerNote headroom)"
        ))
    })?;

    if params.direction() == zencodec::gainmap::GainMapDirection::BaseIsHdr {
        // The base image IS the HDR rendition (alternate is SDR) — nothing
        // to apply; the base decode already carries the HDR signaling.
        return Ok((sdr, info));
    }

    let gm = ultrahdr_core::GainMap {
        width: gain_map.width,
        height: gain_map.height,
        channels: 1, // heic decodes gain maps luma-only (both origins)
        data: gain_map.data,
    };

    // Both buffers are display-oriented (the caller decoded the base with
    // orientation applied; gain-map items are stored display-oriented).
    // Refuse a producer whose aspects still disagree rather than silently
    // stretching gain across the wrong axis.
    let transposed = (sdr.width() > sdr.height()) != (gm.width > gm.height);
    if transposed && sdr.width() != sdr.height() && gm.width != gm.height {
        return Err(at!(HeicError::InvalidData(
            "ReconstructHdr: gain map and base orientation mismatch"
        )));
    }

    // Output form: honor an f16 preference; default linear f32 RGBA.
    let wants_f16 = preferred
        .iter()
        .any(|d| d.channel_type() == zenpixels::ChannelType::F16);
    let format = if wants_f16 {
        HdrOutputFormat::LinearF16
    } else {
        HdrOutputFormat::LinearFloat
    };

    // `None` = full reconstruction at the gain map's encoded maximum, via
    // the canonical rounding route shared across adapters (heic#20).
    let capacity_max = ultrahdr_core::full_reconstruction_boost(&params);
    let display_boost = target_headroom.unwrap_or(capacity_max).max(1.0);

    let hdr = apply_gainmap(&sdr, &gm, &params, display_boost, format, stop).map_err(|_| {
        at!(HeicError::InvalidData(
            "ReconstructHdr: gain-map apply failed"
        ))
    })?;

    // Envelope: derived peak (capped at the reconstruction boost) + mastering
    // display matching the base image's primaries (`apply_gainmap` preserves
    // them; Apple HEIC bases are typically Display P3).
    let peak_nits = SDR_WHITE_NITS * capacity_max.min(display_boost);
    let primaries = match info.source_color.cicp.map(|c| c.color_primaries) {
        Some(12) => [[0.680, 0.320], [0.265, 0.690], [0.150, 0.060]], // Display P3
        Some(9) => [[0.708, 0.292], [0.170, 0.797], [0.131, 0.046]],  // BT.2020
        _ => [[0.640, 0.330], [0.300, 0.600], [0.150, 0.060]],        // BT.709/sRGB
    };
    info.source_color.content_light_level = Some(ContentLightLevel::new(peak_nits as u16, 0));
    info.source_color.mastering_display = Some(zencodec::MasteringDisplay::new(
        primaries,
        [0.3127, 0.3290],
        peak_nits,
        0.005,
    ));
    Ok((hdr, info))
}

/// Derive TransferFunction and ColorPrimaries from native CICP values.
fn cicp_descriptor(
    base: PixelDescriptor,
    color_primaries: u16,
    transfer_characteristics: u16,
) -> PixelDescriptor {
    let tf = TransferFunction::from_cicp(transfer_characteristics as u8).unwrap_or(base.transfer());
    let primaries = ColorPrimaries::from_cicp(color_primaries as u8).unwrap_or(base.primaries);
    base.with_transfer(tf).with_primaries(primaries)
}

/// Map a native `PixelLayout` to a `PixelDescriptor`.
fn layout_to_descriptor(layout: crate::PixelLayout) -> PixelDescriptor {
    match layout {
        crate::PixelLayout::Rgb8 => PixelDescriptor::RGB8_SRGB,
        crate::PixelLayout::Rgba8 => PixelDescriptor::RGBA8_SRGB,
        crate::PixelLayout::Bgr8 => PixelDescriptor::RGB8_SRGB,
        crate::PixelLayout::Bgra8 => PixelDescriptor::BGRA8_SRGB,
    }
}

/// Convert `ProbeError` to `At<HeicError>` for trait compatibility.
fn probe_error_to_heic(e: crate::ProbeError) -> At<HeicError> {
    match e {
        crate::ProbeError::NeedMoreData => at!(HeicError::InvalidData("not enough data to probe")),
        crate::ProbeError::InvalidFormat => {
            at!(HeicError::InvalidData("not a valid HEIC/HEIF file"))
        }
        crate::ProbeError::Corrupt(inner) => inner,
    }
}

/// Reinterpret `Vec<u16>` as `Vec<Rgb<u16>>` via bytemuck.
/// Zero-copy when alignment is compatible (always for u16→Rgb<u16>),
/// falls back to a single memcpy otherwise.
fn u16_vec_to_rgb(data: alloc::vec::Vec<u16>) -> alloc::vec::Vec<Rgb<u16>> {
    match bytemuck::try_cast_vec(data) {
        Ok(pixels) => pixels,
        Err((_err, data)) => bytemuck::cast_slice::<u16, Rgb<u16>>(&data).to_vec(),
    }
}

/// Reinterpret `Vec<u16>` as `Vec<Rgba<u16>>` via bytemuck.
/// Zero-copy when alignment is compatible (always for u16→Rgba<u16>),
/// falls back to a single memcpy otherwise.
fn u16_vec_to_rgba(data: alloc::vec::Vec<u16>) -> alloc::vec::Vec<Rgba<u16>> {
    match bytemuck::try_cast_vec(data) {
        Ok(pixels) => pixels,
        Err((_err, data)) => bytemuck::cast_slice::<u16, Rgba<u16>>(&data).to_vec(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// ISO 21496-1 tmap payload is the authoritative params source and
    /// round-trips through `gain_map_params_from`.
    #[test]
    fn gain_map_params_prefer_iso21496() {
        let mut expected = zencodec::GainMapParams::default();
        expected.alternate_hdr_headroom = 1.5;
        expected.channels[0].max = 1.5;
        expected.channels[0].gamma = 0.7;
        let iso = zencodec::gainmap::serialize_iso21496_fmt(
            &expected,
            zencodec::gainmap::Iso21496Format::AvifTmap,
        );
        let gm = crate::HdrGainMap {
            data: vec![0u8; 4],
            width: 2,
            height: 2,
            bit_depth: 8,
            xmp: None,
            iso21496: Some(iso),
            origin: crate::GainMapOrigin::HeifTmap,
        };
        // No container: the MakerNote fallback is unavailable, so success
        // proves the ISO payload alone supplies the params.
        let params = gain_map_params_from(&gm, None).expect("ISO payload parses");
        assert!((params.alternate_hdr_headroom - 1.5).abs() < 1e-3);
        assert!((params.channels[0].gamma - 0.7).abs() < 1e-3);
        assert_eq!(
            params.direction(),
            zencodec::gainmap::GainMapDirection::BaseIsSdr
        );
    }

    /// Neither ISO payload nor container metadata → no params (the
    /// ReconstructHdr caller turns this into an error, never silent SDR).
    #[test]
    fn gain_map_params_none_without_sources() {
        let gm = crate::HdrGainMap {
            data: vec![0u8; 4],
            width: 2,
            height: 2,
            bit_depth: 8,
            xmp: None,
            iso21496: None,
            origin: crate::GainMapOrigin::AppleAuxItem,
        };
        assert!(gain_map_params_from(&gm, None).is_none());
    }

    #[test]
    fn config_creation() {
        let config = HeicDecoderConfig::new();
        assert_eq!(
            <HeicDecoderConfig as zencodec::decode::DecoderConfig>::formats(),
            &[ImageFormat::Heic]
        );
        let descriptors =
            <HeicDecoderConfig as zencodec::decode::DecoderConfig>::supported_descriptors();
        assert!(!descriptors.is_empty());
        assert!(descriptors.contains(&PixelDescriptor::RGB8_SRGB));
        assert!(descriptors.contains(&PixelDescriptor::RGBA8_SRGB));
        assert!(descriptors.contains(&PixelDescriptor::BGRA8_SRGB));
        let _ = config;
    }

    #[test]
    fn default_config() {
        let config = HeicDecoderConfig::default();
        assert_eq!(
            <HeicDecoderConfig as zencodec::decode::DecoderConfig>::formats(),
            &[ImageFormat::Heic]
        );
        let _ = config;
    }

    #[test]
    fn capabilities_reported() {
        let caps = <HeicDecoderConfig as zencodec::decode::DecoderConfig>::capabilities();
        assert!(caps.icc());
        assert!(caps.exif());
        assert!(caps.xmp());
        assert!(caps.cicp());
        assert!(caps.stop());
        assert!(caps.cheap_probe());
        assert!(caps.native_16bit());
        assert!(caps.native_alpha());
        assert!(caps.hdr());
        assert!(caps.enforces_max_input_bytes());
    }

    #[test]
    fn job_creation() {
        use zencodec::decode::DecoderConfig as _;
        let config = HeicDecoderConfig::new();
        let _job = config.job();
    }

    #[test]
    fn animation_frame_decoder_returns_unsupported() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config
            .job()
            .animation_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn probe_invalid_data() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config.job().probe(b"not a heic file");
        assert!(result.is_err());
    }

    #[test]
    fn negotiate_no_preference_no_alpha() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[], &available);
        assert_eq!(desc, Some(PixelDescriptor::RGB8_SRGB));
    }

    #[test]
    fn negotiate_no_preference_with_alpha() {
        let available = available_descriptors(true, 8);
        let desc = negotiate_pixel_format(&[], &available);
        assert_eq!(desc, Some(PixelDescriptor::RGBA8_SRGB));
    }

    #[test]
    fn negotiate_rgba_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelDescriptor::RGBA8_SRGB], &available);
        assert_eq!(desc, Some(PixelDescriptor::RGBA8_SRGB));
    }

    #[test]
    fn negotiate_bgra_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelDescriptor::BGRA8_SRGB], &available);
        assert_eq!(desc, Some(PixelDescriptor::BGRA8_SRGB));
    }

    #[test]
    fn negotiate_16bit_source_no_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[], &available);
        // 16-bit source with no preference → default to 16-bit
        assert_eq!(desc, Some(PixelDescriptor::RGB16_SRGB));
    }

    #[test]
    fn negotiate_16bit_source_8bit_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[PixelDescriptor::RGB8_SRGB], &available);
        // Caller explicitly prefers 8-bit
        assert_eq!(desc, Some(PixelDescriptor::RGB8_SRGB));
    }

    #[test]
    fn raw_to_pixel_buffer_rgb8() {
        let raw = alloc::vec![10, 20, 30, 40, 50, 60];
        let buf = raw_to_pixel_buffer(raw, 2, 1, crate::PixelLayout::Rgb8).unwrap();
        assert_eq!(buf.width(), 2);
        assert_eq!(buf.height(), 1);
        let img: imgref::ImgRef<'_, Rgb<u8>> = buf.try_as_imgref().expect("expected RGB8");
        assert_eq!(
            img.buf()[0],
            Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
        assert_eq!(
            img.buf()[1],
            Rgb {
                r: 40,
                g: 50,
                b: 60
            }
        );
    }

    #[test]
    fn raw_to_pixel_buffer_rgba8() {
        let raw = alloc::vec![10, 20, 30, 255, 40, 50, 60, 128];
        let buf = raw_to_pixel_buffer(raw, 2, 1, crate::PixelLayout::Rgba8).unwrap();
        assert_eq!(buf.width(), 2);
        assert_eq!(buf.height(), 1);
        let img: imgref::ImgRef<'_, Rgba<u8>> = buf.try_as_imgref().expect("expected RGBA8");
        assert_eq!(
            img.buf()[0],
            Rgba {
                r: 10,
                g: 20,
                b: 30,
                a: 255
            }
        );
    }

    #[test]
    fn raw_to_pixel_buffer_bgr8() {
        // BGR input should be swizzled to RGB via garb.
        let raw = alloc::vec![30, 20, 10];
        let buf = raw_to_pixel_buffer(raw, 1, 1, crate::PixelLayout::Bgr8).unwrap();
        let img: imgref::ImgRef<'_, Rgb<u8>> = buf.try_as_imgref().expect("expected RGB8");
        assert_eq!(
            img.buf()[0],
            Rgb {
                r: 10,
                g: 20,
                b: 30
            }
        );
    }

    #[test]
    fn raw_to_pixel_buffer_bgra8() {
        let raw = alloc::vec![30, 20, 10, 255];
        let buf = raw_to_pixel_buffer(raw, 1, 1, crate::PixelLayout::Bgra8).unwrap();
        let img: imgref::ImgRef<'_, rgb::alt::BGRA<u8>> =
            buf.try_as_imgref().expect("expected BGRA8");
        let px = &img.buf()[0];
        assert_eq!(px.b, 30);
        assert_eq!(px.g, 20);
        assert_eq!(px.r, 10);
        assert_eq!(px.a, 255);
    }

    #[test]
    fn probe_error_conversion() {
        let e = probe_error_to_heic(crate::ProbeError::NeedMoreData);
        assert!(matches!(e.error(), HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::InvalidFormat);
        assert!(matches!(e.error(), HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::Corrupt(at!(HeicError::NoPrimaryImage)));
        assert!(matches!(e.error(), HeicError::NoPrimaryImage));
    }

    #[test]
    fn descriptor_to_layout_mapping() {
        assert_eq!(
            descriptor_to_layout(PixelDescriptor::RGB8_SRGB),
            crate::PixelLayout::Rgb8
        );
        assert_eq!(
            descriptor_to_layout(PixelDescriptor::RGBA8_SRGB),
            crate::PixelLayout::Rgba8
        );
        assert_eq!(
            descriptor_to_layout(PixelDescriptor::BGRA8_SRGB),
            crate::PixelLayout::Bgra8
        );
    }

    #[test]
    fn policy_to_threads_sequential() {
        assert_eq!(policy_to_threads(ThreadingPolicy::Sequential), 1);
    }

    #[test]
    fn policy_to_threads_parallel() {
        assert_eq!(policy_to_threads(ThreadingPolicy::Parallel), 0);
    }

    #[test]
    fn single_thread_decode_via_adapter() {
        use zencodec::decode::{Decode, DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let limits = ResourceLimits::none().with_threading(ThreadingPolicy::Sequential);
        let job = config.job().with_limits(limits);
        let decoder = job
            .decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB])
            .expect("decoder creation");
        let output = decoder.decode().expect("single-thread decode");

        let info = output.info();
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 854);

        let pixels = output.pixels();
        assert_eq!(pixels.width(), 1280);
        assert_eq!(pixels.rows(), 854);
        // Verify non-zero data (actual image was decoded)
        assert!(pixels.row(0).iter().any(|&b| b != 0));
    }

    /// Verify SingleThread native decode via with_max_threads on DecodeRequest.
    #[test]
    fn single_thread_native_decode() {
        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = crate::DecoderConfig::new();
        let output = config
            .decode_request(&data)
            .with_output_layout(crate::PixelLayout::Rgb8)
            .with_max_threads(1)
            .decode()
            .expect("single-thread native decode");

        assert_eq!(output.width, 1280);
        assert_eq!(output.height, 854);
        assert!(output.data.iter().any(|&b| b != 0));
    }

    // ── Fix 2: max_input_bytes enforcement tests ───────────────────────

    #[test]
    fn probe_enforces_max_input_bytes() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        // Set max_input_bytes to 100 bytes — much smaller than the file
        let limits = ResourceLimits::none().with_max_input_bytes(100);
        let job = config.job().with_limits(limits);
        let result = job.probe(&data);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err.error(), HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn probe_allows_within_max_input_bytes() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        // Set max_input_bytes large enough to allow the file
        let limits = ResourceLimits::none().with_max_input_bytes(data.len() as u64 + 1000);
        let job = config.job().with_limits(limits);
        let result = job.probe(&data);
        assert!(result.is_ok());
    }

    #[test]
    fn decoder_enforces_max_input_bytes() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let limits = ResourceLimits::none().with_max_input_bytes(100);
        let job = config.job().with_limits(limits);
        let result = job.decoder(Cow::Borrowed(&data), &[PixelDescriptor::RGB8_SRGB]);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(
            matches!(err.error(), HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn probe_full_enforces_max_input_bytes() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let limits = ResourceLimits::none().with_max_input_bytes(100);
        let job = config.job().with_limits(limits);
        let result = job.probe_full(&data);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err.error(), HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    // ── Fix 3: probe() vs probe_full() behavior tests ───────────────────

    #[test]
    fn probe_returns_lightweight_info() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let job = config.job();
        let info = job.probe(&data).expect("probe should succeed");

        // probe() should return dimensions and basic info
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 854);
        assert_eq!(info.format, ImageFormat::Heic);
        assert_eq!(info.frame_count(), Some(1));

        // probe() should NOT extract EXIF/XMP/ICC (those require full container parse)
        assert!(
            info.embedded_metadata.exif.is_none(),
            "probe() should not extract EXIF"
        );
        assert!(
            info.embedded_metadata.xmp.is_none(),
            "probe() should not extract XMP"
        );
        assert!(
            info.source_color.icc_profile.is_none(),
            "probe() should not extract ICC profile"
        );
    }

    #[test]
    fn probe_full_returns_complete_info() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        // Use iPhone test file which has EXIF metadata
        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/test-images/classic-car-iphone12pro.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let job = config.job();
        let info = job.probe_full(&data).expect("probe_full should succeed");

        // Default config == OrientationHint::Preserve. classic-car is a portrait
        // iPhone photo: stored (coded) landscape 4032×3024 with an `irot`,
        // displayed portrait 3024×4032. Preserve reports the STORED dims + the
        // intrinsic orientation tag; display_width/height recover the upright dims.
        assert_eq!(info.width, 4032);
        assert_eq!(info.height, 3024);
        assert!(
            info.orientation.swaps_axes(),
            "portrait iPhone HEIC must report a 90/270 orientation under Preserve, got {:?}",
            info.orientation
        );
        assert_eq!(info.display_width(), 3024);
        assert_eq!(info.display_height(), 4032);
        assert_eq!(info.format, ImageFormat::Heic);

        // probe_full() should extract EXIF (iPhone image has EXIF)
        assert!(
            info.embedded_metadata.exif.is_some(),
            "probe_full() should extract EXIF from iPhone HEIC"
        );
    }

    #[test]
    fn probe_and_probe_full_agree_on_dimensions() {
        use zencodec::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let job_light = config.clone().job();
        let job_full = config.job();
        let light = job_light.probe(&data).expect("probe");
        let full = job_full.probe_full(&data).expect("probe_full");

        assert_eq!(light.width, full.width);
        assert_eq!(light.height, full.height);
        assert_eq!(light.format, full.format);
        assert_eq!(light.frame_count(), full.frame_count());
    }
}
