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

use zencodec::decode::{
    DecodeCapabilities, DecodeOutput, DecodePolicy, DecodeRowSink, OutputInfo,
    negotiate_pixel_format,
};
use zencodec::{
    ContentLightLevel, GainMapInfo, GainMapPresence, ImageFormat, ImageInfo, ImageSequence,
    MasteringDisplay, Orientation, ResourceLimits, Supplements, ThreadingPolicy, Unsupported,
};
use zenpixels::{
    Cicp, ColorOrigin, HdrMetadata, PixelBuf, PixelFormat, PixelView, SampleLayout, SampleType,
};

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
/// The original HEIF orientation is available as a separate `Orientation` extension.
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
    .with_incremental(true)
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
    .with_inventory(cfg!(feature = "inventory"))
    .with_threads_supported_range(1, if cfg!(feature = "parallel") { 256 } else { 1 });

// ── Supported descriptors ──────────────────────────────────────────────────

/// Pixel formats this decoder can produce natively (8-bit and 16-bit).
static DECODE_DESCRIPTORS: &[PixelFormat] = &[
    PixelFormat::RGB8_SRGB,
    PixelFormat::RGBA8_SRGB,
    PixelFormat::BGRA8_SRGB,
    PixelFormat::RGB16_SRGB,
    PixelFormat::RGBA16_SRGB,
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
    type Error = At<zencodec::CodecError>;
    type Job<'a> = HeicDecodeJob;

    fn formats() -> &'static [ImageFormat] {
        &[ImageFormat::Heic]
    }

    fn supported_pixel_formats() -> &'static [PixelFormat] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &HEIC_DECODE_CAPS
    }

    fn estimate_decode_resources(
        &self,
        image: &zencodec::estimate::ImageCharacteristics,
        compute: &zencodec::estimate::ComputeEnvironment,
    ) -> zencodec::estimate::ResourceEstimate {
        use zencodec::estimate::{ResourceEstimate, ThreadingInformation};

        // Peak working set: the native `estimate_memory` upper bound — full
        // YCbCr planes (4:2:0, u16/sample) + the output buffer at the requested
        // layout + the deblocking metadata. For a grid image this over-counts
        // (tiles are decoded into a tile-sized frame and color-converted into
        // the output strip-by-strip), so it is a safe upper bound. The HEVC
        // AV1-tile/grid assembly buffers it covers are heic's own; the inner
        // HEVC decoder's transient per-CTU scratch is bounded and not modelled.
        let layout = layout_for_descriptor(image.pixel_format());
        let peak = crate::DecoderConfig::estimate_memory(image.width(), image.height(), layout);

        // Wall time: HEVC intra decode is heavy. Measured single-thread on this
        // box ≈ 25 Mpix/s (1.09 MP example.heic ~54 ms; 12.2 MP iPhone still
        // ~451 ms sequential). Conservative, content-independent.
        const DECODE_MPIX_PER_S: u64 = 25;
        let pixels = u64::from(image.width()).saturating_mul(u64::from(image.height()));
        let wall_ms = pixels / (DECODE_MPIX_PER_S * 1_000) + 1;

        // SERIAL for the peak/time model: tile parallelism (the optional
        // `parallel` feature) only shortens grid wall-time, it does not lower
        // the peak, and the serial time is a valid upper bound.
        ResourceEstimate::new(peak, wall_ms)
            .with_threading(ThreadingInformation::SERIAL)
            .at_cores(compute.cores())
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
            pulse: None,
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
    /// Select the base, registered gain-map components, or HDR reconstruction.
    /// The default is `BaseOnly`.
    gain_map_render: zencodec::GainMapRender,
    /// Retained pulse (progress + cancellation). `stop` is derived from it so
    /// every existing cancellation site sees pulse cancellation too.
    pulse: Option<how_far::SharedPulse>,
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
        let alloc_pref = crate::alloc_util::AllocPreference::from_zencodec(
            self.limits.prefer_fallible_allocations,
        );
        // When no size/memory caps are set AND the alloc preference is the
        // default, there is nothing to thread — `None` lets the decode path
        // fall back to `DEFAULT_LIMITS` (which already carries `CodecDefault`).
        // But an explicit Fallible/Infallible must survive even with no caps,
        // so build a Limits in that case too.
        if !self.limits.has_any() && alloc_pref == crate::alloc_util::AllocPreference::CodecDefault
        {
            return None;
        }
        let mut limits = crate::Limits::default();
        limits.max_width = self.limits.max_width.map(u64::from);
        limits.max_height = self.limits.max_height.map(u64::from);
        limits.max_pixels = self.limits.max_pixels;
        limits.max_memory_bytes = self.limits.max_memory_bytes;
        limits.alloc_pref = alloc_pref;
        Some(limits)
    }

    // ── Pattern B: native-error bodies behind the thin trait wrappers ──────
    //
    // Each `*_inner` method is the exact pre-migration trait-method body
    // (returning the native `At<HeicError>`, unchanged internal logic); the
    // `zencodec::decode::DecodeJob` impl above wraps each with
    // `.map_err(zencodec::CodecError::of)` to reach the shared envelope
    // `type Error = At<CodecError>`. This keeps the (previously reviewed,
    // tested) internals untouched and isolates the Pattern-B bridge to one
    // line per method.

    fn probe_inner(&self, data: &[u8]) -> Result<ImageInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
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

    fn probe_full_inner(&self, data: &[u8]) -> Result<ImageInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
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
            .and_then(|c| c.base_rendition_item())
            .map(|it| compose_orientation(&it.transforms))
            .unwrap_or(Orientation::Identity);
        let (w, h, orientation) =
            reported_dims_and_orientation(native.width, native.height, intrinsic, apply);
        Ok(apply_policy(
            self.policy.as_ref(),
            build_image_info_full(&native, container.as_ref(), w, h, orientation),
        ))
    }

    #[cfg(feature = "inventory")]
    fn inventory_inner(
        &self,
        data: &[u8],
    ) -> Result<zencodec::inventory::Inventory, At<HeicError>> {
        use crate::inventory::Options;
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
        let stop_ref: &dyn enough::Stop = match self.stop {
            Some(ref s) => s,
            None => &enough::Unstoppable,
        };
        // Mirrors `HeicDecoder::decode_inner`: ReconstructHdr applies the gain
        // map; Components or `extract_gain_map` decode and attach it, whatever
        // the render mode (both can hold at once).
        let render = self.gain_map_render;
        let policy = self.policy.as_ref();
        let opts = Options {
            apply_gain_map: matches!(render, zencodec::GainMapRender::ReconstructHdr { .. }),
            attach_gain_map: self.extract_gain_map
                || matches!(render, zencodec::GainMapRender::Components),
            decode_depth: self.extract_depth,
            keep_icc: policy.is_none_or(|p| p.resolve_icc(true)),
            keep_exif: policy.is_none_or(|p| p.resolve_exif(true)),
            keep_xmp: policy.is_none_or(|p| p.resolve_xmp(true)),
        };
        crate::inventory::inventory(data, &opts, stop_ref)
    }

    fn output_info_inner(&self, data: &[u8]) -> Result<OutputInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        let stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let container = crate::heif::parse(data, stop)?;
        let primary = container.base_rendition_item();
        let intrinsic = primary.as_ref().map_or(Orientation::Identity, |item| {
            compose_orientation(&item.transforms)
        });
        let apply = will_auto_orient(self.config.orientation);
        let (w, h, pending) =
            reported_dims_and_orientation(native.width, native.height, intrinsic, apply);
        let info = build_image_info_lightweight(&native, w, h, pending);
        let info = apply_policy(
            self.policy.as_ref(),
            with_container_color(info, &native, &container, primary.as_ref()),
        );
        let available = available_descriptors(native.has_alpha, native.bit_depth);
        let spec = decoded_spec(available[0].layout, &info)?;
        Ok(
            OutputInfo::full_decode(w, h, spec).with_orientation_applied(if apply {
                intrinsic
            } else {
                Orientation::Identity
            }),
        )
    }

    fn decoder_inner<'a>(
        mut self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelFormat],
    ) -> Result<HeicDecoder<'a>, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
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

    fn push_decoder_inner<'a>(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelFormat],
    ) -> Result<OutputInfo, At<HeicError>> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| at!(HeicError::ResourceLimit(e)))?;
        let pi = crate::ImageInfo::from_bytes(&data).map_err(probe_error_to_heic)?;
        let stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let container = crate::heif::parse(&data, stop)?;
        let primary = container.base_rendition_item();
        let intrinsic = primary.as_ref().map_or(Orientation::Identity, |item| {
            compose_orientation(&item.transforms)
        });
        let reconstructing = if let zencodec::GainMapRender::ReconstructHdr { target_headroom } =
            self.gain_map_render
            && pi.has_gain_map
        {
            reconstruction_request(&container, target_headroom)?.is_some()
        } else {
            false
        };
        let applied = if will_auto_orient(self.config.orientation) || reconstructing {
            intrinsic
        } else {
            Orientation::Identity
        };

        let available = available_descriptors(pi.has_alpha, pi.bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available).ok_or_else(|| {
            at!(HeicError::UnsupportedOperation(
                zencodec::UnsupportedOperation::PixelFormat
            ))
        })?;
        if is_16bit(&negotiated)
            || primary
                .as_ref()
                .is_some_and(|item| !item.transforms.is_empty())
            || !matches!(self.gain_map_render, zencodec::GainMapRender::BaseOnly)
        {
            let output = self.decoder_inner(data, preferred)?.decode_inner()?;
            let pixels = output.pixels();
            let desc = pixels.format();
            sink.begin(pixels.width(), pixels.height(), desc.clone())
                .map_err(|e| at!(HeicError::Sink(e)))?;
            zencodec::helpers::copy_rows_to_sink(
                sink,
                zencodec::decode::BatchMeta::final_rows(0),
                &pixels,
            )
            .map_err(|e| at!(HeicError::Sink(e)))?;
            sink.finish().map_err(|e| at!(HeicError::Sink(e)))?;
            return Ok(
                OutputInfo::full_decode(pixels.width(), pixels.height(), desc.clone())
                    .with_orientation_applied(applied),
            );
        }

        // 8-bit: use native decode_rows for grid streaming
        let layout = descriptor_to_layout(&negotiated);
        let info = build_image_info_lightweight(&pi, pi.width, pi.height, Orientation::Identity);
        let info = apply_policy(
            self.policy.as_ref(),
            with_item_hdr(
                with_container_color(info, &pi, &container, primary.as_ref()),
                primary.as_ref(),
            ),
        );
        let desc = decoded_spec(layout_to_descriptor(layout).layout, &info)?;

        // Stream decode via native decode_rows, adapting to zencodec sink.
        let mut adapter = RowSinkAdapter {
            inner: sink,
            descriptor: desc.clone(),
            origin: color_origin(&info, native_cicp(&pi, Some(&container))),
            hdr: hdr_metadata(&info),
            width: pi.width,
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

        adapter
            .inner
            .begin(pi.width, pi.height, desc.clone())
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

    fn streaming_decoder_inner<'a>(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelFormat],
    ) -> Result<HeicStreamDecoder, At<HeicError>> {
        HeicStreamDecoder::new(self.decoder_inner(data, preferred)?)
    }
}

/// Build the "available descriptors" list for format negotiation based on
/// image properties (alpha, bit depth).
fn available_descriptors(has_alpha: bool, bit_depth: u8) -> alloc::vec::Vec<PixelFormat> {
    let mut available = alloc::vec::Vec::with_capacity(5);
    if bit_depth > 8 {
        // 16-bit formats first when source is >8-bit
        if has_alpha {
            available.push(PixelFormat::RGBA16_SRGB);
            available.push(PixelFormat::RGB16_SRGB);
        } else {
            available.push(PixelFormat::RGB16_SRGB);
            available.push(PixelFormat::RGBA16_SRGB);
        }
    }
    // 8-bit formats
    if has_alpha {
        available.push(PixelFormat::RGBA8_SRGB);
        available.push(PixelFormat::BGRA8_SRGB);
        available.push(PixelFormat::RGB8_SRGB);
    } else {
        available.push(PixelFormat::RGB8_SRGB);
        available.push(PixelFormat::RGBA8_SRGB);
        available.push(PixelFormat::BGRA8_SRGB);
    }
    available
}

/// Check whether a negotiated descriptor is a 16-bit format.
fn is_16bit(desc: &PixelFormat) -> bool {
    desc.layout.sample == SampleType::U16
}

/// Pick the native [`crate::PixelLayout`] that best bounds the output buffer
/// for a negotiated descriptor, used only by the resource estimate.
///
/// heic's `PixelLayout` is 8-bit-only (3 or 4 Bpp), so a 16-bit output is not
/// representable here — but the estimate's dominant term is the u16 YCbCr
/// working set from `estimate_memory`, which already scales with bit depth, so
/// an alpha-presence choice (Rgb8 vs Rgba8) is a sound conservative bound.
fn layout_for_descriptor(desc: &PixelFormat) -> crate::PixelLayout {
    if desc.layout.order.has_alpha_slot() {
        crate::PixelLayout::Rgba8
    } else {
        crate::PixelLayout::Rgb8
    }
}

/// Map a negotiated PixelFormat to a native PixelLayout for 8-bit decode.
fn descriptor_to_layout(desc: &PixelFormat) -> crate::PixelLayout {
    if desc.layout == SampleLayout::BGRA8 {
        crate::PixelLayout::Bgra8
    } else if desc.layout == SampleLayout::RGBA8 {
        crate::PixelLayout::Rgba8
    } else {
        crate::PixelLayout::Rgb8
    }
}

impl<'a> zencodec::decode::DecodeJob<'a> for HeicDecodeJob {
    type Error = At<zencodec::CodecError>;
    type Dec = HeicDecoder<'a>;
    type StreamDec = HeicStreamDecoder;
    type AnimationFrameDec = Unsupported<At<zencodec::CodecError>>;

    fn with_pulse(mut self, pulse: how_far::SharedPulse) -> Self {
        self.stop = Some(zencodec::StopToken::new(pulse.clone()));
        self.pulse = Some(pulse);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// heic applies gain maps natively (`reconstructs_hdr()` is `true`):
    /// `ReconstructHdr` applies the Apple or ISO gain map and delivers
    /// linear floating-point samples with measured content-light levels.
    fn with_gain_map_render(mut self, render: zencodec::GainMapRender) -> Self {
        self.gain_map_render = render;
        self
    }

    fn try_set_policy(
        &mut self,
        policy: DecodePolicy,
    ) -> Result<(), zencodec::UnsupportedOperation> {
        let mut unsupported = policy;
        unsupported.allow_icc = None;
        unsupported.allow_exif = None;
        unsupported.allow_xmp = None;
        if unsupported != DecodePolicy::none() {
            return Err(zencodec::UnsupportedOperation::DecodePolicy);
        }
        self.policy = Some(policy);
        Ok(())
    }

    fn with_orientation(mut self, hint: zencodec::OrientationHint) -> Self {
        self.config.orientation = hint;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, At<zencodec::CodecError>> {
        self.probe_inner(data).map_err(zencodec::CodecError::of)
    }

    fn probe_full(&self, data: &[u8]) -> Result<ImageInfo, At<zencodec::CodecError>> {
        self.probe_full_inner(data)
            .map_err(zencodec::CodecError::of)
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, At<zencodec::CodecError>> {
        self.output_info_inner(data)
            .map_err(zencodec::CodecError::of)
    }

    /// Every box, item, item extent and property, with what this job's
    /// decode does with it. Follows the job's gain-map, depth and
    /// [`DecodePolicy`] settings. See `src/inventory.rs`.
    #[cfg(feature = "inventory")]
    fn inventory(
        &self,
        data: &[u8],
    ) -> Result<zencodec::inventory::Inventory, At<zencodec::CodecError>> {
        self.inventory_inner(data).map_err(zencodec::CodecError::of)
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelFormat],
    ) -> Result<HeicDecoder<'a>, At<zencodec::CodecError>> {
        self.decoder_inner(data, preferred)
            .map_err(zencodec::CodecError::of)
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelFormat],
    ) -> Result<OutputInfo, At<zencodec::CodecError>> {
        self.push_decoder_inner(data, sink, preferred)
            .map_err(zencodec::CodecError::of)
    }

    fn streaming_decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelFormat],
    ) -> Result<HeicStreamDecoder, At<zencodec::CodecError>> {
        self.streaming_decoder_inner(data, preferred)
            .map_err(zencodec::CodecError::of)
    }

    fn animation_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelFormat],
    ) -> Result<Unsupported<At<zencodec::CodecError>>, At<zencodec::CodecError>> {
        // Invocation-origin, not image-bytes-origin: HEIC has no concept of
        // an animation at all, so this is "the caller asked for an
        // operation this codec doesn't support" (zencodec's
        // `UnsupportedOperation::AnimationDecode`), not a fault in any
        // particular image's bytes.
        Err(HeicError::UnsupportedOperation(zencodec::UnsupportedOperation::AnimationDecode).into())
    }

    /// Incremental (sans-I/O) decode: the header is reported as soon as it
    /// probes, and the image decodes through `push_decoder` once the whole
    /// file is offered. The HEIF container parse and the HEVC tile decode
    /// both need their bytes complete; a source binding that fetches only
    /// the items a decode needs is a follow-up.
    fn dyn_incremental_decoder(
        self,
        preferred: &[PixelFormat],
    ) -> Result<
        alloc::boxed::Box<zencodec::decode::DynIncrementalDecoder>,
        zencodec::decode::BoxedError,
    > {
        Ok(zencodec::decode::erase_incremental(
            zencodec::helpers::WholeFileIncremental::new(self, preferred),
        ))
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
    descriptor: PixelFormat,
    origin: ColorOrigin,
    hdr: HdrMetadata,
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
            let pixels = PixelView::from_bytes(
                &self.strip_buf,
                self.width,
                self.pending_height,
                &self.descriptor,
            )
            .map_err_at(|_| HeicError::InvalidData("invalid decoded strip"))?
            .with_hdr(self.hdr)
            .with_origin(&self.origin);
            zencodec::helpers::copy_rows_to_sink(
                self.inner,
                zencodec::decode::BatchMeta::final_rows(y),
                &pixels,
            )
            .map_err(|e| at!(HeicError::Sink(e)))?;
        }
        Ok(())
    }

    /// Flush pending strip, storing any error for later propagation.
    /// Used inside `demand()` which cannot return `Result`.
    fn flush_pending_deferred(&mut self) {
        if self.deferred_error.is_none() {
            self.deferred_error = self.flush_pending().err();
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
    preferred: alloc::vec::Vec<PixelFormat>,
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
    type Error = At<zencodec::CodecError>;

    fn decode(self) -> Result<DecodeOutput, At<zencodec::CodecError>> {
        self.decode_inner().map_err(zencodec::CodecError::of)
    }
}

impl HeicDecoder<'_> {
    /// Pattern B: the exact pre-migration `decode()` trait-method body,
    /// returning the native `At<HeicError>` unchanged — see the `*_inner`
    /// comment on `impl HeicDecodeJob` for the rationale.
    fn decode_inner(self) -> Result<DecodeOutput, At<HeicError>> {
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
        let negotiated = negotiate_pixel_format(preferred, &available).ok_or_else(|| {
            at!(HeicError::UnsupportedOperation(
                zencodec::UnsupportedOperation::PixelFormat
            ))
        })?;

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
                // Invocation-origin: `GainMapRender` is `#[non_exhaustive]`,
                // so this only fires for a variant added to zencodec after
                // this crate was compiled — the caller's request names a
                // real (to them) render mode this build doesn't recognize,
                // not a fault in the image bytes.
                return Err(at!(HeicError::InvalidRequest(
                    "unrecognized GainMapRender mode"
                )));
            }
        };
        let stop_ref: &dyn enough::Stop = self
            .stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let container = crate::heif::parse(data, stop_ref).ok();
        let reconstruction = if let Some(target) = reconstruct_target
            && probe_info.as_ref().is_some_and(|pi| pi.has_gain_map)
        {
            let container = container
                .as_ref()
                .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;
            reconstruction_request(container, target)?
        } else {
            None
        };
        let reconstructing = reconstruction.is_some();
        let use_wide_base = bit_depth > 8
            && reconstruction
                .as_ref()
                .is_some_and(|(gain, _)| gain.params.iso_channel_count().is_some());
        // Reconstruction runs in the display coordinate space of the gain image.
        let apply_orientation = apply_orientation || reconstructing;

        let (buf, width, height, has_alpha): (PixelBuf, u32, u32, bool) = if use_wide_base
            || (is_16bit(&negotiated) && !reconstructing)
        {
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

            let wants_alpha = use_wide_base || negotiated.layout == SampleLayout::RGBA16;
            let (samples, spec) = if has_alpha || wants_alpha {
                (frame.to_rgba16(), PixelFormat::RGBA16_SRGB)
            } else {
                (frame.to_rgb16(), PixelFormat::RGB16_SRGB)
            };
            let pb = PixelBuf::from_samples(samples.map_err(crate::error::at_core)?, w, h, spec)
                .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))?;
            (
                state_widened_depth(
                    pb,
                    frame.bit_depth,
                    frame.alpha_bit_depth.unwrap_or(frame.bit_depth),
                ),
                w,
                h,
                has_alpha || wants_alpha,
            )
        } else {
            // 8-bit path: use negotiated layout for decode.
            // ReconstructHdr forces RGBA8 — `apply_gainmap` consumes 8-bit
            // SDR input (gain-map HEICs carry an 8-bit SDR base by design).
            let layout = if reconstructing {
                crate::PixelLayout::Rgba8
            } else {
                descriptor_to_layout(&negotiated)
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
            let pb = raw_to_pixel_buffer(native_output.data, w, h, native_output.layout)?;
            (pb, w, h, has_alpha)
        };

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
        let source_orientation = container
            .as_ref()
            .and_then(|c| c.base_rendition_item())
            .map_or(Orientation::Identity, |it| {
                compose_orientation(&it.transforms)
            });
        let report_orientation = if apply_orientation {
            Orientation::Identity
        } else {
            source_orientation
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
        let origin = color_origin(&info, native_cicp(pi_ref, container.as_ref()));
        let spec = decoded_spec(buf.format().layout, &info)?;
        let facts = buf.facts();
        let mut buf = buf
            .relabel_color(spec.color)
            .map_err_at(|_| HeicError::InvalidData("invalid decoded image spec"))?;
        buf.assume_facts(facts);
        let buf = buf.with_origin(origin).with_hdr(hdr_metadata(&info));
        let (buf, info) = if let Some((gain_info, target)) = reconstruction {
            reconstruct_hdr_base(buf, info, data, gain_info, target, preferred, stop_ref)?
        } else {
            (buf, info)
        };

        let mut output =
            DecodeOutput::new(buf, info).with_source_encoding_details(HeicSourceEncoding);

        // This is the source transform even when output pixels are upright.
        output.extensions_mut().insert(source_orientation);

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
            if (self.extract_gain_map || surface_components) && pi.has_gain_map {
                let gain_map = crate::decode::decode_gain_map(data, &[crate::Backend::Rust])?;
                if surface_components {
                    let c = container
                        .as_ref()
                        .ok_or_else(|| at!(HeicError::NoPrimaryImage))?;
                    let gain_info =
                        extract_gain_map_info(c, c.primary_item_id).ok_or_else(|| {
                            at!(HeicError::InvalidData(
                                "missing or invalid gain-map metadata"
                            ))
                        })?;
                    gain_map_alternate_space(&gain_info)?;
                    let mut pixels = gain_map_pixels(
                        gain_map.data.clone(),
                        gain_map.width,
                        gain_map.height,
                        gain_map.channels,
                    )?;
                    // The native gain item is in display coordinates. Register the
                    // generic component to the delivered base's stored coordinates.
                    let (w, h) = report_orientation.output_dimensions(width, height);
                    validate_gain_registration(w, h, pixels.width(), pixels.height())?;
                    if !report_orientation.is_identity() {
                        pixels = zenpixels_convert::orient::apply_orientation(
                            &pixels.as_view(),
                            report_orientation.inverse(),
                        )
                        .map_err_at(|_| HeicError::InvalidData("gain-map registration failed"))?;
                    }
                    let components = zencodec::decode::DecodedGainMap::try_new(pixels, gain_info)
                        .map_err(|_| {
                        at!(HeicError::InvalidData("invalid gain-map components"))
                    })?;
                    output.extensions_mut().insert(components);
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
    descriptor: PixelFormat,
    origin: ColorOrigin,
    y_offset: u32,
    grid: Option<GridState>,
    current_grid_row: u32,
    strip_buffer: alloc::vec::Vec<u8>,
    full_pixels: Option<PixelBuf>,
    stop: Option<zencodec::StopToken>,
}

impl HeicStreamDecoder {
    /// Default strip height for non-grid fallback.
    const FALLBACK_STRIP_HEIGHT: u32 = 64;

    /// Construct a streaming decoder for the given HEIC data.
    fn new(decoder: HeicDecoder<'_>) -> Result<Self, At<HeicError>> {
        let stop = decoder.stop.clone();
        let stop_ref = stop
            .as_ref()
            .map_or(&enough::Unstoppable as &dyn enough::Stop, |s| s);
        let data = &decoder.data;
        let pi = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;

        // Tile strips are available for untransformed 8-bit HEVC grids.
        // Other requests reuse one-shot decode so orientation, color and
        // rendition policy follow the same path.
        if pi.bit_depth <= 8
            && !pi.has_alpha
            && matches!(decoder.gain_map_render, zencodec::GainMapRender::BaseOnly)
        {
            let container = crate::heif::parse(data, stop_ref)?;
            if let Some(grid_state) = Self::try_init_grid(
                &container,
                &decoder.preferred,
                decoder.limits.as_ref(),
                stop_ref,
            )? {
                let info = apply_policy(
                    decoder.policy.as_ref(),
                    build_image_info_full(
                        &pi,
                        Some(&container),
                        pi.width,
                        pi.height,
                        Orientation::Identity,
                    ),
                );
                let descriptor =
                    decoded_spec(layout_to_descriptor(grid_state.layout).layout, &info)?;
                return Ok(Self {
                    origin: color_origin(&info, native_cicp(&pi, Some(&container))),
                    info,
                    descriptor,
                    y_offset: 0,
                    grid: Some(grid_state),
                    current_grid_row: 0,
                    strip_buffer: alloc::vec::Vec::new(),
                    full_pixels: None,
                    stop,
                });
            }
        }

        let output = decoder.decode_inner()?;
        let (pixels, details) = output.into_parts();
        let info = details.info;
        let descriptor = pixels.format().clone();
        Ok(Self {
            origin: ColorOrigin::assumed(),
            info,
            descriptor,
            y_offset: 0,
            grid: None,
            current_grid_row: 0,
            strip_buffer: alloc::vec::Vec::new(),
            full_pixels: Some(pixels),
            stop,
        })
    }

    /// Try to initialize grid streaming state. Returns None if not eligible.
    fn try_init_grid(
        container: &crate::heif::HeifContainer<'_>,
        preferred: &[PixelFormat],
        limits: Option<&crate::Limits>,
        stop: &dyn enough::Stop,
    ) -> Result<Option<GridState>, At<HeicError>> {
        use crate::heif::{ColorInfo, FourCC, ItemType};

        stop.check().map_err(|r| at!(HeicError::Cancelled(r)))?;

        let primary_item = container
            .base_rendition_item()
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
        // try_decode_grid_streaming, which uses limits.unwrap_or(&DEFAULT_LIMITS).
        limits
            .unwrap_or(&crate::decode::DEFAULT_LIMITS)
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
        let color_override = match container.item_nclx(primary_item.id) {
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
        let negotiated = negotiate_pixel_format(preferred, &available).ok_or_else(|| {
            at!(HeicError::UnsupportedOperation(
                zencodec::UnsupportedOperation::PixelFormat
            ))
        })?;
        let layout = descriptor_to_layout(&negotiated);

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
                crate::hevc::decode_with_config(&grid.tile_config, &grid.tile_data[tile_idx])
                    .map_err(crate::error::hevc_at)
                    .at()?;

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

    /// Pattern B: the exact pre-migration `next_batch()` trait-method body,
    /// returning the native `At<HeicError>` unchanged — see the `*_inner`
    /// comment on `impl HeicDecodeJob` for the rationale.
    fn next_batch_inner(&mut self) -> Result<Option<(u32, PixelView<'_>)>, At<HeicError>> {
        if self.grid.is_some() {
            let result = self.decode_grid_row()?;
            match result {
                None => Ok(None),
                Some((y, width, height)) => {
                    let bpp = self.descriptor.layout.bytes_per_pixel();
                    let stride = width as usize * bpp;
                    let slice =
                        PixelView::new(&self.strip_buffer, width, height, stride, &self.descriptor)
                            .map(|view| {
                                view.with_hdr(hdr_metadata(&self.info))
                                    .with_origin(&self.origin)
                            })
                            .map_err(|_| {
                                at!(HeicError::InvalidData("failed to create pixel slice"))
                            })?;
                    Ok(Some((y, slice)))
                }
            }
        } else if let Some(ref pixels) = self.full_pixels {
            let height = pixels.height();
            if self.y_offset >= height {
                return Ok(None);
            }
            let h = Self::FALLBACK_STRIP_HEIGHT.min(height - self.y_offset);
            let slice = pixels
                .as_view()
                .sub_rows(self.y_offset, h)
                .map_err_at(|_| HeicError::InvalidData("invalid decoded strip"))?;
            let y = self.y_offset;
            self.y_offset += h;
            Ok(Some((y, slice)))
        } else {
            Ok(None)
        }
    }
}

impl zencodec::decode::StreamingDecode for HeicStreamDecoder {
    type Error = At<zencodec::CodecError>;

    fn next_batch(&mut self) -> Result<Option<(u32, PixelView<'_>)>, At<zencodec::CodecError>> {
        self.next_batch_inner().map_err(zencodec::CodecError::of)
    }

    fn info(&self) -> &ImageInfo {
        &self.info
    }
}

// ── Pixel conversion helpers ───────────────────────────────────────────────

/// Convert raw `Vec<u8>` pixel data from the native decoder into a [`PixelBuf`].
///
/// Adopts the existing byte allocation, swapping BGR8 channels in place.
fn raw_to_pixel_buffer(
    mut raw: alloc::vec::Vec<u8>,
    w: u32,
    h: u32,
    layout: crate::PixelLayout,
) -> Result<PixelBuf, At<HeicError>> {
    if layout == crate::PixelLayout::Bgr8 {
        let (pixels, remainder) = raw.as_chunks_mut::<3>();
        if !remainder.is_empty() {
            return Err(at!(HeicError::InvalidData("BGR swizzle size mismatch")));
        }
        for pixel in pixels {
            pixel.swap(0, 2);
        }
    }
    PixelBuf::from_bytes(raw, w, h, layout_to_descriptor(layout))
        .map_err_at(|_| HeicError::InvalidData("pixel buffer size mismatch"))
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
            c.base_rendition_item()
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

    info = with_source_cicp(info, pi, native_cicp(pi, None));

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

    info = with_source_cicp(info, pi, native_cicp(pi, None));

    // Extract all metadata from the pre-parsed container
    if let Some(container) = container {
        let primary_item = container.base_rendition_item();

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

        info = with_container_color(info, pi, container, primary_item.as_ref());

        // EXIF extraction
        if let Some(exif) = extract_exif_from_container(container) {
            info = info.with_exif(exif);
        }

        // XMP extraction
        if let Some(xmp) = extract_xmp_from_container(container) {
            info = info.with_xmp(xmp);
        }

        info = with_item_hdr(info, primary_item.as_ref());
    }

    info
}

fn with_item_hdr(mut info: ImageInfo, item: Option<&crate::heif::Item>) -> ImageInfo {
    // Extract Content Light Level (cLLi) from primary item properties
    if let Some(item) = item
        && let Some(clli) = &item.content_light_level
    {
        info = info.with_content_light_level(ContentLightLevel::new(
            clli.max_content_light_level,
            clli.max_frame_average_light_level,
        ));
    }

    // Extract Mastering Display Colour Volume (mDCv) from primary item properties
    if let Some(item) = item
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
    info
}

fn with_container_color(
    mut info: ImageInfo,
    pi: &crate::ImageInfo,
    container: &crate::heif::HeifContainer<'_>,
    primary_item: Option<&crate::heif::Item>,
) -> ImageInfo {
    info = with_source_cicp(info, pi, native_cicp(pi, Some(container)));
    if let Some(item) = primary_item
        && let Some(icc) = container.item_icc(item.id)
    {
        info = info
            .with_icc_profile(icc.to_vec())
            .with_color_authority(zencodec::ColorAuthority::Icc);
        // Keep the resolved convenience fields for callers of ImageInfo.
        // The pixel owner's ColorOrigin records the original declaration.
        if pi.color_primaries == 2
            && let Some(id) = zenpixels::icc::IccIdentification::from_icc(icc)
            && let Some(mut cicp) = id.to_cicp()
        {
            if let Some(source) = info.source_color.cicp {
                cicp.matrix_coefficients = source.matrix_coefficients;
                cicp.full_range = source.full_range;
            }
            info = info.with_cicp(cicp);
        }
    }
    info
}

fn native_cicp(
    pi: &crate::ImageInfo,
    container: Option<&crate::heif::HeifContainer<'_>>,
) -> Option<Cicp> {
    if let Some(cicp) = container.and_then(|c| item_cicp(c, c.base_rendition_item()?.id)) {
        return Some(cicp);
    }
    (pi.color_primaries != 2 || pi.transfer_characteristics != 2 || pi.matrix_coefficients != 2)
        .then(|| {
            Cicp::new(
                pi.color_primaries as u8,
                pi.transfer_characteristics as u8,
                pi.matrix_coefficients as u8,
                pi.video_full_range,
            )
        })
}

fn item_cicp(container: &crate::heif::HeifContainer<'_>, item: u32) -> Option<Cicp> {
    let crate::heif::ColorInfo::Nclx {
        color_primaries,
        transfer_characteristics,
        matrix_coefficients,
        full_range,
    } = container.item_nclx(item)?
    else {
        return None;
    };
    Cicp::try_from_bytes([
        (*color_primaries).try_into().ok()?,
        (*transfer_characteristics).try_into().ok()?,
        (*matrix_coefficients).try_into().ok()?,
        u8::from(*full_range),
    ])
    .ok()
}

fn color_origin(info: &ImageInfo, raw_cicp: Option<Cicp>) -> ColorOrigin {
    match (&info.source_color.icc_profile, raw_cicp) {
        (Some(icc), Some(cicp)) => ColorOrigin::from_icc_and_cicp(icc.clone(), cicp),
        (Some(icc), None) => ColorOrigin::from_icc(icc.clone()),
        (None, Some(cicp)) => ColorOrigin::from_cicp(cicp),
        (None, None) => ColorOrigin::assumed(),
    }
}

fn with_source_cicp(mut info: ImageInfo, pi: &crate::ImageInfo, cicp: Option<Cicp>) -> ImageInfo {
    if let Some(cicp) = cicp {
        info = info.with_cicp(cicp);
        if !pi.has_icc_profile {
            info = info.with_color_authority(zencodec::ColorAuthority::Cicp);
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

/// Apple HDR gain-map parameters from the primary item's EXIF MakerNote
/// headroom (`ultrahdr_core::{parse_exif_for_apple_hdr, from_apple_headroom}`).
///
/// Legacy Apple aux-item HEICs carry no ISO 21496-1 payload and no `hdrgm:`
/// XMP — the MakerNote HDR headroom is their canonical parameter source
/// (validated against real iPhone captures). Returns `None` when the EXIF
/// or headroom is absent.
pub(crate) fn apple_gain_map_params(
    container: &crate::heif::HeifContainer<'_>,
) -> Option<zencodec::GainMapParams> {
    let exif = extract_exif_from_container(container)?;
    let info = zenmeta::apple::parse_exif_for_apple_hdr(&exif)?;
    zenmeta::apple::from_apple_headroom(&info)
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
    iso: Option<&[u8]>,
    container: Option<&crate::heif::HeifContainer<'_>>,
) -> Option<zencodec::GainMapParams> {
    if let Some(iso) = iso {
        let params =
            zencodec::gainmap::parse_iso21496_fmt(iso, zencodec::gainmap::Iso21496Format::AvifTmap)
                .ok()?;
        params.validate().ok()?;
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
    if let Some((tmap_id, gainmap_id, iso)) = crate::decode::find_tmap_gain_map(container) {
        // Present ISO metadata is authoritative. Never hide corruption by
        // substituting a guessed Apple curve.
        let params = gain_map_params_from(Some(&iso), Some(container))?;
        let item = container.get_item(gainmap_id)?;
        let (width, height) = item.dimensions?;
        let channels = if item
            .hevc_config
            .as_ref()
            .is_some_and(|c| c.chroma_format == 0)
            || item.av1_config.as_ref().is_some_and(|c| c.monochrome)
        {
            1
        } else {
            3
        };
        let mut info = GainMapInfo::new(params, width, height, channels);
        if let Some(cicp) = item_cicp(container, tmap_id) {
            info = info.with_alternate_cicp(cicp);
        }
        if let Some(icc) = container.item_icc(tmap_id) {
            info = info.with_alternate_icc(icc);
        }
        return Some(info);
    }

    let aux_ids =
        container.find_auxiliary_items(primary_id, "urn:com:apple:photo:2020:aux:hdrgainmap");
    let &gainmap_id = aux_ids.first()?;
    let aux_item = container.get_item(gainmap_id)?;
    let (width, height) = aux_item.dimensions?;
    let params = gain_map_params_from(None, Some(container))?;
    // Apple HDR gain maps are luma-only (single channel).
    Some(GainMapInfo::new(params, width, height, 1))
}

fn reconstruction_request(
    container: &crate::heif::HeifContainer<'_>,
    target: Option<f32>,
) -> Result<Option<(GainMapInfo, Option<f32>)>, At<HeicError>> {
    if target.is_some_and(|value| !value.is_finite() || value < 1.0) {
        return Err(at!(HeicError::InvalidRequest(
            "gain-map target must be a finite boost >= 1"
        )));
    }
    let gain = extract_gain_map_info(container, container.primary_item_id).ok_or_else(|| {
        at!(HeicError::InvalidData(
            "missing or invalid gain-map metadata"
        ))
    })?;
    if gain.params.direction() == zencodec::gainmap::GainMapDirection::BaseIsHdr {
        if target.is_some_and(|boost| boost < gain.params.linear_base_headroom() as f32) {
            // Lower-rendition output needs an explicit transfer/gamut delivery
            // contract. Refuse it before choosing or delivering a pixel layout.
            return Err(at!(HeicError::UnsupportedOperation(
                zencodec::UnsupportedOperation::PixelFormat
            )));
        }
        return Ok(None);
    }
    gain_map_alternate_space(&gain)?;
    Ok(Some((gain, target)))
}

fn gain_map_pixels(
    data: alloc::vec::Vec<u8>,
    width: u32,
    height: u32,
    channels: u8,
) -> Result<PixelBuf, At<HeicError>> {
    let layout = match channels {
        1 => SampleLayout::GRAY8,
        3 => SampleLayout::RGB8,
        _ => {
            return Err(at!(HeicError::InvalidData(
                "invalid gain-map channel count"
            )));
        }
    };
    let spec = PixelFormat::new(
        layout,
        zenpixels::ColorSpace::Untagged {
            total_channels: channels,
            alpha: zenpixels::AlphaMode::None,
        },
    );
    PixelBuf::from_bytes(data, width, height, spec)
        .map_err_at(|_| HeicError::InvalidData("invalid gain-map pixel buffer"))
}

fn gain_map_alternate_space(
    info: &GainMapInfo,
) -> Result<Option<zenpixels::ColorSpace>, At<HeicError>> {
    use zenpixels::{AlphaMode, ColorSpace, Primaries, SignalRange, Transfer};
    let alternate = if let Some(profile) = &info.alternate_icc {
        let channels = match zenpixels::IccColorSpace::from_icc(profile) {
            Some(zenpixels::IccColorSpace::Rgb) => Some(3),
            Some(zenpixels::IccColorSpace::Gray) => Some(1),
            _ => None,
        };
        channels.map(|color_channels| ColorSpace::Icc {
            profile: profile.clone(),
            color_channels,
            alpha: AlphaMode::None,
            intent: zenpixels::RenderingIntent::RelativeColorimetric,
        })
    } else {
        info.alternate_cicp.and_then(|cicp| {
            Some(ColorSpace::Rgb {
                primaries: Primaries::from_cicp(cicp.color_primaries)?,
                transfer: Transfer::from_cicp(cicp.transfer_characteristics)?,
                range: SignalRange::Full,
                alpha: AlphaMode::None,
                white: None,
            })
        })
    };
    if !info.params.use_base_color_space && alternate.is_none() {
        return Err(at!(HeicError::UnsupportedOperation(
            zencodec::UnsupportedOperation::PixelFormat
        )));
    }
    Ok(alternate)
}

fn validate_gain_registration(w: u32, h: u32, mw: u32, mh: u32) -> Result<(), At<HeicError>> {
    if w != h && mw != mh && (w > h) != (mw > mh) {
        return Err(at!(HeicError::InvalidData(
            "gain map and base orientation mismatch"
        )));
    }
    Ok(())
}

fn reconstruct_hdr_base(
    base: PixelBuf,
    mut info: ImageInfo,
    data: &[u8],
    gain_info: GainMapInfo,
    target: Option<f32>,
    preferred: &[PixelFormat],
    stop: &dyn enough::Stop,
) -> Result<(PixelBuf, ImageInfo), At<HeicError>> {
    use ultrahdr_core::gainmap::{HdrOutputFormat, apply_gainmap};
    use zenpixels::{ColorSpace, DiffuseWhite, Primaries, Transfer};
    use zenpixels_convert::gainmap::{GainMapApplyConfig, apply_gain_map_with};

    let gain = crate::decode::decode_gain_map(data, &[crate::Backend::Rust])?;
    validate_gain_registration(base.width(), base.height(), gain.width, gain.height)?;
    let wants_f16 = preferred.iter().any(|d| d.layout.sample == SampleType::F16);
    let params = &gain_info.params;
    let boost = target.unwrap_or_else(|| ultrahdr_core::full_reconstruction_boost(params));
    let origin = base.origin().clone();
    let mut hdr = if gain.origin == crate::GainMapOrigin::HeifTmap {
        let pixels = gain_map_pixels(gain.data, gain.width, gain.height, gain.channels)?;
        let alternate = gain_map_alternate_space(&gain_info)?;
        let metadata = params
            .exact_metadata()
            .map_err(|_| at!(HeicError::InvalidData("invalid exact gain-map metadata")))?;
        let dual = zenpixels::DualLayerHdrView::new(
            base.as_view(),
            pixels.as_view(),
            metadata,
            alternate.as_ref(),
            Orientation::Identity,
        )
        .map_err_at(|_| HeicError::InvalidData("invalid ISO gain-map layers"))?;
        let mut config = target.map_or_else(
            GainMapApplyConfig::alternate_rendition,
            GainMapApplyConfig::from_display_boost_linear,
        );
        if wants_f16 {
            let applier = zenpixels_convert::gainmap::GainMapApplier::for_view(&dual, &config)
                .map_err_at(|_| {
                    HeicError::UnsupportedOperation(zencodec::UnsupportedOperation::PixelFormat)
                })?;
            config = config.with_destination_format(PixelFormat::new(
                SampleLayout::RGBAF16,
                applier.destination_format().color.clone(),
            ));
        }
        apply_gain_map_with(&dual, &config, stop).map_err_at(|_| {
            HeicError::UnsupportedOperation(zencodec::UnsupportedOperation::PixelFormat)
        })?
    } else {
        // UltraHDR accepts sRGB-coded RGB. Use the recognized ICC gamut
        // for Apple images, while keeping the original profile as provenance.
        let primaries = match &base.format().color {
            ColorSpace::Icc { profile, .. } => zenpixels::icc::IccIdentification::from_icc(profile)
                .filter(|id| id.transfer == Transfer::Srgb)
                .map(|id| id.primaries),
            ColorSpace::Rgb { primaries, .. } => Some(*primaries),
            _ => None,
        }
        .unwrap_or(Primaries::Bt709);
        let color = PixelFormat::RGBA8_SRGB.with_primaries(primaries).color;
        let base = base
            .relabel_color(color)
            .map_err_at(|_| HeicError::InvalidData("invalid Apple gain-map base color"))?;
        let gain = ultrahdr_core::GainMap {
            width: gain.width,
            height: gain.height,
            channels: gain.channels,
            data: gain.data,
        };
        let format = if wants_f16 {
            HdrOutputFormat::LinearF16
        } else {
            HdrOutputFormat::LinearFloat
        };
        apply_gainmap(&base, &gain, params, boost, format, stop)
            .map_err_at(|_| HeicError::InvalidData("Apple gain-map apply failed"))?
    };
    crate::error::check_stop(stop)?;
    let cll = zenpixels_convert::measure_content_light_level(
        &hdr.as_view(),
        DiffuseWhite::BT2408,
        None,
        zenpixels_convert::LightLevelMethod::MaxRgb,
    )
    .map_err_at(|_| HeicError::InvalidData("cannot measure reconstructed HDR light levels"))?;
    // Source mastering information remains descriptive. Nominal capacity
    // remains on GainMapInfo; content-light levels come from the output pixels.
    info.source_color.content_light_level = Some(cll);
    info.source_color.diffuse_white = Some(DiffuseWhite::BT2408);
    if info.source_color.mastering_display.is_none()
        && let ColorSpace::Rgb { primaries, .. } = hdr.format().color
        && let Some((r, g, b)) = primaries.chromaticity()
        && let Some(white) = primaries.white_point()
    {
        let [r, g, b, white] = [r, g, b, white].map(|(x, y)| [x, y]);
        // The nominal alternate capacity describes the mastering envelope.
        // It is independent of the requested display boost and measured CLL.
        info.source_color.mastering_display = Some(MasteringDisplay::new(
            [r, g, b],
            white,
            203.0 * ultrahdr_core::full_reconstruction_boost(params),
            0.0,
        ));
    }
    hdr = hdr.with_origin(origin).with_hdr(hdr_metadata(&info));
    Ok((hdr, info))
}

/// Declare the full-range RGB samples after native YUV matrix/range conversion.
fn decoded_spec(layout: SampleLayout, info: &ImageInfo) -> Result<PixelFormat, At<HeicError>> {
    zencodec::helpers::pixel_format_for_decoded_pixels(layout, &info.source_color, None).map_err_at(
        |_| HeicError::UnsupportedOperation(zencodec::UnsupportedOperation::PixelFormat),
    )
}

fn hdr_metadata(info: &ImageInfo) -> HdrMetadata {
    HdrMetadata {
        content_light_level: info.source_color.content_light_level,
        mastering: info.source_color.mastering_display,
    }
}

/// Map a native `PixelLayout` to a `PixelFormat`.
fn layout_to_descriptor(layout: crate::PixelLayout) -> PixelFormat {
    match layout {
        crate::PixelLayout::Rgb8 => PixelFormat::RGB8_SRGB,
        crate::PixelLayout::Rgba8 => PixelFormat::RGBA8_SRGB,
        crate::PixelLayout::Bgr8 => PixelFormat::RGB8_SRGB,
        crate::PixelLayout::Bgra8 => PixelFormat::BGRA8_SRGB,
    }
}

/// Convert `ProbeError` to `At<HeicError>` for trait compatibility.
///
/// `ProbeError` has its own `zencodec::CategorizedError` impl that maps
/// `NeedMoreData` -> `Image(UnexpectedEof)` and `InvalidFormat` ->
/// `Image(Unsupported(Type))` (see `error.rs`), but callers here go through
/// `HeicError`'s categorization instead (the zencodec trait boundary
/// declares `type Error = At<CodecError>` built from `At<HeicError>`, not
/// from `ProbeError` directly — see the Pattern B envelope bridge doc
/// comment on `From<HeicError> for At<CodecError>`). Preserve the same two
/// categories here via `HeicError::Truncated` / `HeicError::NotHeif` rather
/// than collapsing both into `HeicError::InvalidData` (`Image(Malformed)`),
/// which would misreport a truncated/unrecognized input as a corrupt one.
///
/// `map_error` keeps the probe's origin trace (for `Corrupt`, the container /
/// parameter-set line that rejected the input) instead of restarting it here.
fn probe_error_to_heic(e: At<crate::ProbeError>) -> At<HeicError> {
    e.map_error(|p| match p {
        crate::ProbeError::NeedMoreData => HeicError::Truncated("not enough data to probe"),
        crate::ProbeError::InvalidFormat => HeicError::NotHeif("not a valid HEIC/HEIF file"),
        crate::ProbeError::Corrupt(inner) => inner,
    })
}

/// RGB conversion rounds to the source-depth code grid before widening to U16.
fn state_widened_depth(mut buffer: PixelBuf, color: u8, alpha: u8) -> PixelBuf {
    let depth = zenpixels::sample::ExactDepth::new(
        SampleType::U16,
        zenpixels::SignalRange::Full,
        color,
        alpha,
    )
    .ok();
    buffer.assume_facts(buffer.facts().with_depth(depth));
    buffer
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use rgb::{Rgb, Rgba};
    /// ICC outranks nclx when both exist. Readers measured in image-compat round 14
    /// that use color at all do the same, and AVIF's adapter already does.
    #[test]
    fn icc_is_the_authority_when_nclx_and_a_profile_both_exist() {
        let probe = |has_icc_profile| crate::ImageInfo {
            width: 1,
            height: 1,
            has_alpha: false,
            bit_depth: 8,
            chroma_format: 1,
            has_exif: false,
            has_xmp: false,
            has_thumbnail: false,
            color_primaries: 1,
            transfer_characteristics: 13,
            matrix_coefficients: 6,
            video_full_range: true,
            has_icc_profile,
            has_depth: false,
            has_gain_map: false,
            exif: None,
            xmp: None,
            icc_profile: None,
        };
        use zencodec::ColorAuthority::{Cicp, Icc};
        for (has_icc, want) in [(true, Icc), (false, Cicp)] {
            let pi = probe(has_icc);
            let light =
                super::build_image_info_lightweight(&pi, 1, 1, super::Orientation::Identity);
            let full = super::build_image_info_full(&pi, None, 1, 1, super::Orientation::Identity);
            for info in [light, full] {
                assert_eq!(info.source_color.color_authority, want, "has_icc={has_icc}");
                assert!(info.source_color.cicp.is_some(), "nclx stays available");
            }
        }
    }

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
        )
        .unwrap();
        let gm = crate::HdrGainMap {
            channels: 1,
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
        let params =
            gain_map_params_from(gm.iso21496.as_deref(), None).expect("ISO payload parses");
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
            channels: 1,
            data: vec![0u8; 4],
            width: 2,
            height: 2,
            bit_depth: 8,
            xmp: None,
            iso21496: None,
            origin: crate::GainMapOrigin::AppleAuxItem,
        };
        assert!(gain_map_params_from(gm.iso21496.as_deref(), None).is_none());
    }

    #[test]
    fn corrupt_iso_cannot_fall_back_to_real_apple_headroom() {
        let bytes = include_bytes!("../testdata/apple-hdr/hdr-sample.heic");
        let container = crate::heif::parse(bytes, &enough::Unstoppable).unwrap();
        assert!(
            apple_gain_map_params(&container).is_some(),
            "fixture supplies the legacy fallback"
        );
        let gm = crate::HdrGainMap {
            channels: 1,
            data: vec![0u8; 4],
            width: 2,
            height: 2,
            bit_depth: 8,
            xmp: None,
            iso21496: Some(vec![0xff]),
            origin: crate::GainMapOrigin::HeifTmap,
        };
        assert!(gain_map_params_from(gm.iso21496.as_deref(), Some(&container)).is_none());
    }

    #[test]
    fn config_creation() {
        let config = HeicDecoderConfig::new();
        assert_eq!(
            <HeicDecoderConfig as zencodec::decode::DecoderConfig>::formats(),
            &[ImageFormat::Heic]
        );
        let descriptors =
            <HeicDecoderConfig as zencodec::decode::DecoderConfig>::supported_pixel_formats();
        assert!(!descriptors.is_empty());
        assert!(descriptors.contains(&PixelFormat::RGB8_SRGB));
        assert!(descriptors.contains(&PixelFormat::RGBA8_SRGB));
        assert!(descriptors.contains(&PixelFormat::BGRA8_SRGB));
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
        assert_eq!(desc, Some(PixelFormat::RGB8_SRGB));
    }

    #[test]
    fn negotiate_no_preference_with_alpha() {
        let available = available_descriptors(true, 8);
        let desc = negotiate_pixel_format(&[], &available);
        assert_eq!(desc, Some(PixelFormat::RGBA8_SRGB));
    }

    #[test]
    fn negotiate_rgba_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelFormat::RGBA8_SRGB], &available);
        assert_eq!(desc, Some(PixelFormat::RGBA8_SRGB));
    }

    #[test]
    fn negotiate_bgra_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelFormat::BGRA8_SRGB], &available);
        assert_eq!(desc, Some(PixelFormat::BGRA8_SRGB));
    }

    #[test]
    fn negotiate_16bit_source_no_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[], &available);
        // 16-bit source with no preference → default to 16-bit
        assert_eq!(desc, Some(PixelFormat::RGB16_SRGB));
    }

    #[test]
    fn negotiate_16bit_source_8bit_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[PixelFormat::RGB8_SRGB], &available);
        // Caller explicitly prefers 8-bit
        assert_eq!(desc, Some(PixelFormat::RGB8_SRGB));
    }

    #[test]
    fn raw_to_pixel_buffer_rgb8() {
        let raw = alloc::vec![10, 20, 30, 40, 50, 60];
        let buf = raw_to_pixel_buffer(raw, 2, 1, crate::PixelLayout::Rgb8).unwrap();
        assert_eq!(buf.width(), 2);
        assert_eq!(buf.height(), 1);
        let img: imgref::ImgRef<'_, Rgb<u8>> = buf.as_view().as_imgref().expect("expected RGB8");
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
        let img: imgref::ImgRef<'_, Rgba<u8>> = buf.as_view().as_imgref().expect("expected RGBA8");
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
        // BGR input should be swizzled to RGB in place.
        let raw = alloc::vec![30, 20, 10];
        let buf = raw_to_pixel_buffer(raw, 1, 1, crate::PixelLayout::Bgr8).unwrap();
        let img: imgref::ImgRef<'_, Rgb<u8>> = buf.as_view().as_imgref().expect("expected RGB8");
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
            buf.as_view().as_imgref().expect("expected BGRA8");
        let px = &img.buf()[0];
        assert_eq!(px.b, 30);
        assert_eq!(px.g, 20);
        assert_eq!(px.r, 10);
        assert_eq!(px.a, 255);
    }

    #[test]
    fn probe_error_conversion() {
        use zencodec::{CategorizedError, ErrorCategory, ImageError, UnsupportedImageKind};

        // NeedMoreData -> Truncated -> Image(UnexpectedEof), NOT
        // Image(Malformed): a truncated input is not the same failure as a
        // corrupt one.
        let e = probe_error_to_heic(at!(crate::ProbeError::NeedMoreData));
        assert!(matches!(e.error(), HeicError::Truncated(_)));
        assert_eq!(
            e.error().category(),
            ErrorCategory::Image(ImageError::UnexpectedEof)
        );

        // InvalidFormat -> NotHeif -> Image(Unsupported(Type)): "not a
        // HEIC/HEIF file at all" is not the same failure as "a HEIC/HEIF
        // file with corrupt content".
        let e = probe_error_to_heic(at!(crate::ProbeError::InvalidFormat));
        assert!(matches!(e.error(), HeicError::NotHeif(_)));
        assert_eq!(
            e.error().category(),
            ErrorCategory::Image(ImageError::Unsupported(UnsupportedImageKind::Type))
        );

        let e = probe_error_to_heic(at!(crate::ProbeError::Corrupt(HeicError::NoPrimaryImage)));
        assert!(matches!(e.error(), HeicError::NoPrimaryImage));
    }

    #[test]
    fn descriptor_to_layout_mapping() {
        assert_eq!(
            descriptor_to_layout(&PixelFormat::RGB8_SRGB),
            crate::PixelLayout::Rgb8
        );
        assert_eq!(
            descriptor_to_layout(&PixelFormat::RGBA8_SRGB),
            crate::PixelLayout::Rgba8
        );
        assert_eq!(
            descriptor_to_layout(&PixelFormat::BGRA8_SRGB),
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
            .decoder(Cow::Borrowed(&data), &[PixelFormat::RGB8_SRGB])
            .expect("decoder creation");
        let output = decoder.decode().expect("single-thread decode");

        let info = output.info();
        assert_eq!(info.width, 1280);
        assert_eq!(info.height, 854);

        let pixels = output.pixels();
        assert_eq!(pixels.width(), 1280);
        assert_eq!(pixels.height(), 854);
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
        use zencodec::CategorizedError;
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
        assert_eq!(
            err.category(),
            zencodec::ErrorCategory::Resource(zencodec::ResourceError::Limits(
                zencodec::LimitKind::InputSize
            )),
            "expected a resource-limit category, got {err:?}"
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
        use zencodec::CategorizedError;
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
        let result = job.decoder(Cow::Borrowed(&data), &[PixelFormat::RGB8_SRGB]);
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert_eq!(
            err.category(),
            zencodec::ErrorCategory::Resource(zencodec::ResourceError::Limits(
                zencodec::LimitKind::InputSize
            )),
            "expected a resource-limit category, got {err:?}"
        );
    }

    #[test]
    fn probe_full_enforces_max_input_bytes() {
        use zencodec::CategorizedError;
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
        assert_eq!(
            err.category(),
            zencodec::ErrorCategory::Resource(zencodec::ResourceError::Limits(
                zencodec::LimitKind::InputSize
            )),
            "expected a resource-limit category, got {err:?}"
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
