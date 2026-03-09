//! zencodec-types trait implementations for heic-decoder.
//!
//! Provides [`HeicDecoderConfig`] that implements the 4-layer decode trait
//! hierarchy from zencodec-types, wrapping the native heic-decoder API.
//!
//! # Trait mapping
//!
//! | zencodec-types | heic-decoder adapter |
//! |----------------|----------------------|
//! | `DecoderConfig` | [`HeicDecoderConfig`] |
//! | `DecodeJob<'a>` | [`HeicDecodeJob`] |
//! | `Decode` | [`HeicDecoder`] |
//! | `StreamingDecode` | [`HeicStreamDecoder`] |
//! | `FullFrameDecoder` | `Unsupported<HeicError>` (HEIC has no animation) |

use alloc::borrow::Cow;

use rgb::{Rgb, Rgba};
use zc::decode::{
    DecodeCapabilities, DecodeOutput, DecodeRowSink, OutputInfo, negotiate_pixel_format,
};
use zc::{ImageFormat, ImageInfo, Orientation, ResourceLimits, ThreadingPolicy, Unsupported};
use zenpixels::{Cicp, ColorPrimaries, PixelBuffer, PixelDescriptor, TransferFunction};

use crate::error::HeicError;

// ── Threading helpers ────────────────────────────────────────────────────

/// Convert a [`ThreadingPolicy`] to a concrete thread count.
///
/// Returns `0` for unlimited (use rayon default / global pool),
/// `1` for single-threaded, or `n` for a specific limit.
fn policy_to_threads(policy: ThreadingPolicy) -> usize {
    match policy {
        ThreadingPolicy::SingleThread => 1,
        ThreadingPolicy::LimitOrSingle { max_threads } => max_threads as usize,
        ThreadingPolicy::LimitOrAny {
            preferred_max_threads,
        } => preferred_max_threads as usize,
        ThreadingPolicy::Balanced => {
            #[cfg(feature = "std")]
            {
                std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1))
            }
            #[cfg(not(feature = "std"))]
            {
                1
            }
        }
        ThreadingPolicy::Unlimited => 0,
        _ => 0, // future variants default to unlimited
    }
}

// ── Capabilities ─────────────────────────────────────────────────────────

static HEIC_DECODE_CAPS: DecodeCapabilities = DecodeCapabilities::new()
    .with_icc(true)
    .with_exif(true)
    .with_xmp(true)
    .with_cicp(true)
    .with_cancel(true)
    .with_cheap_probe(true)
    .with_decode_into(true)
    .with_row_level(true)
    .with_hdr(true)
    .with_native_16bit(true)
    .with_native_alpha(true)
    .with_enforces_max_pixels(true)
    .with_enforces_max_memory(true)
    .with_enforces_max_input_bytes(true);

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

/// HEIC decoder configuration implementing [`zc::decode::DecoderConfig`].
///
/// Wraps [`crate::DecoderConfig`] for use with the zencodec-types trait system.
/// HEIC decoding has no tunable parameters, so this is a thin wrapper.
#[derive(Clone, Debug)]
pub struct HeicDecoderConfig {
    inner: crate::DecoderConfig,
}

impl HeicDecoderConfig {
    /// Create a default HEIC decoder config.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: crate::DecoderConfig::new(),
        }
    }

    /// Access the underlying [`crate::DecoderConfig`].
    #[must_use]
    pub fn inner(&self) -> &crate::DecoderConfig {
        &self.inner
    }
}

impl Default for HeicDecoderConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl zc::decode::DecoderConfig for HeicDecoderConfig {
    type Error = HeicError;
    type Job<'a> = HeicDecodeJob<'a>;

    fn formats() -> &'static [ImageFormat] {
        &[ImageFormat::Heic]
    }

    fn supported_descriptors() -> &'static [PixelDescriptor] {
        DECODE_DESCRIPTORS
    }

    fn capabilities() -> &'static DecodeCapabilities {
        &HEIC_DECODE_CAPS
    }

    fn job(&self) -> HeicDecodeJob<'_> {
        HeicDecodeJob {
            config: self,
            stop: None,
            limits: ResourceLimits::none(),
        }
    }
}

// ── Decode Job ─────────────────────────────────────────────────────────────

/// Per-operation HEIC decode job.
pub struct HeicDecodeJob<'a> {
    config: &'a HeicDecoderConfig,
    stop: Option<&'a dyn zc::enough::Stop>,
    limits: ResourceLimits,
}

impl<'a> HeicDecodeJob<'a> {
    /// Build native limits from zencodec-types ResourceLimits.
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

impl<'a> zc::decode::DecodeJob<'a> for HeicDecodeJob<'a> {
    type Error = HeicError;
    type Dec = HeicDecoder<'a>;
    type StreamDec = HeicStreamDecoder;
    type FullFrameDec = Unsupported<HeicError>;

    fn with_stop(mut self, stop: &'a dyn zc::enough::Stop) -> Self {
        self.stop = Some(stop);
        self
    }

    fn with_limits(mut self, limits: ResourceLimits) -> Self {
        self.limits = limits;
        self
    }

    fn probe(&self, data: &[u8]) -> Result<ImageInfo, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        Ok(build_image_info_lightweight(&native))
    }

    fn probe_full(&self, data: &[u8]) -> Result<ImageInfo, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        // Parse the HEIF container once and extract all metadata from it
        let container = crate::heif::parse(data, &enough::Unstoppable)
            .map_err(|e| e.into_inner())
            .ok();
        Ok(build_image_info_full(
            &native,
            container.as_ref(),
            native.width,
            native.height,
        ))
    }

    fn output_info(&self, data: &[u8]) -> Result<OutputInfo, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        let native = crate::ImageInfo::from_bytes(data).map_err(probe_error_to_heic)?;
        let available = available_descriptors(native.has_alpha, native.bit_depth);
        let base_desc = available[0]; // default for this image
        let desc = cicp_descriptor(
            base_desc,
            native.color_primaries,
            native.transfer_characteristics,
        );
        Ok(OutputInfo::full_decode(native.width, native.height, desc))
    }

    fn decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelDescriptor],
    ) -> Result<HeicDecoder<'a>, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        let thread_count = policy_to_threads(self.limits.threading());
        Ok(HeicDecoder {
            config: self.config,
            data,
            preferred: preferred.to_vec(),
            stop: self.stop,
            limits: self.native_limits(),
            thread_count,
        })
    }

    fn push_decoder(
        self,
        data: Cow<'a, [u8]>,
        sink: &mut dyn DecodeRowSink,
        preferred: &[PixelDescriptor],
    ) -> Result<OutputInfo, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        // Probe for image properties
        let probe_info = crate::ImageInfo::from_bytes(&data).ok();
        let has_alpha = probe_info.as_ref().is_some_and(|pi| pi.has_alpha);
        let bit_depth = probe_info.as_ref().map_or(8, |pi| pi.bit_depth);

        // Negotiate output format
        let available = available_descriptors(has_alpha, bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available);

        if is_16bit(negotiated) {
            // 16-bit: full decode, then push rows
            let dec = self.decoder(data, preferred)?;
            let output = <HeicDecoder<'_> as zc::decode::Decode>::decode(dec)?;
            let ps = output.pixels();
            let desc = ps.descriptor();
            let w = ps.width();
            let h = ps.rows();
            sink.begin(w, h, desc).map_err(HeicError::Sink)?;
            let mut dst = sink
                .provide_next_buffer(0, h, w, desc)
                .map_err(HeicError::Sink)?;
            for row in 0..h {
                dst.row_mut(row).copy_from_slice(ps.row(row));
            }
            drop(dst);
            sink.finish().map_err(HeicError::Sink)?;
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
        if let Some(stop) = self.stop {
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
            .map_err(HeicError::Sink)?;

        let (w, h) = req.decode_rows(&mut adapter).map_err(|e| e.into_inner())?;
        // Check for deferred sink errors from demand() calls
        adapter.take_deferred_error()?;
        // Flush the last strip that was written by the native decoder
        adapter.flush_pending()?;
        adapter.inner.finish().map_err(HeicError::Sink)?;
        Ok(OutputInfo::full_decode(w, h, desc))
    }

    fn streaming_decoder(
        self,
        data: Cow<'a, [u8]>,
        preferred: &[PixelDescriptor],
    ) -> Result<HeicStreamDecoder, HeicError> {
        self.limits
            .check_input_size(data.len() as u64)
            .map_err(|e| HeicError::LimitExceeded(limit_exceeded_msg(e)))?;
        let thread_count = policy_to_threads(self.limits.threading());
        HeicStreamDecoder::new(
            &data,
            preferred,
            self.native_limits().as_ref(),
            self.stop,
            thread_count,
        )
    }

    fn full_frame_decoder(
        self,
        _data: Cow<'a, [u8]>,
        _preferred: &[PixelDescriptor],
    ) -> Result<Unsupported<HeicError>, HeicError> {
        Err(HeicError::Unsupported(
            "HEIC does not support animation decoding",
        ))
    }
}

// ── RowSink adapter ────────────────────────────────────────────────────────

/// Adapts `zc::decode::DecodeRowSink` to the native `crate::RowSink` interface.
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
    deferred_error: Option<HeicError>,
}

impl RowSinkAdapter<'_> {
    /// Flush any pending strip data to the zencodec sink.
    fn flush_pending(&mut self) -> Result<(), HeicError> {
        if let Some(y) = self.pending_y.take() {
            let bpp = self.descriptor.bytes_per_pixel();
            let row_bytes = self.width as usize * bpp;
            let mut dst = self
                .inner
                .provide_next_buffer(y, self.pending_height, self.width, self.descriptor)
                .map_err(HeicError::Sink)?;
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
    fn take_deferred_error(&mut self) -> Result<(), HeicError> {
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
    config: &'a HeicDecoderConfig,
    data: Cow<'a, [u8]>,
    preferred: alloc::vec::Vec<PixelDescriptor>,
    stop: Option<&'a dyn zc::enough::Stop>,
    limits: Option<crate::Limits>,
    /// Thread count from threading policy (0 = unlimited/default).
    thread_count: usize,
}

impl zc::decode::Decode for HeicDecoder<'_> {
    type Error = HeicError;

    fn decode(self) -> Result<DecodeOutput, HeicError> {
        let data: &[u8] = &self.data;
        let preferred = &self.preferred;

        // Probe for image info — best-effort.
        let probe_info = crate::ImageInfo::from_bytes(data).ok();
        let bit_depth = probe_info.as_ref().map_or(8, |pi| pi.bit_depth);
        let has_alpha = probe_info.as_ref().is_some_and(|pi| pi.has_alpha);

        // Negotiate output format
        let available = available_descriptors(has_alpha, bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available);

        let (buf, width, height, has_alpha): (PixelBuffer, u32, u32, bool) = if is_16bit(negotiated)
        {
            // 16-bit path: decode to YCbCr frame, then convert at full precision.
            let mut req = self.config.inner.decode_request(data);
            if let Some(ref limits) = self.limits {
                req = req.with_limits(limits);
            }
            if let Some(stop) = self.stop {
                req = req.with_stop(stop);
            }
            if self.thread_count > 0 {
                req = req.with_max_threads(self.thread_count);
            }
            let frame = req.decode_yuv().map_err(|e| e.into_inner())?;

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
                let rgba_data = frame.to_rgba16();
                let pixels: alloc::vec::Vec<Rgba<u16>> = rgba_data
                    .chunks_exact(4)
                    .map(|c| Rgba {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                        a: c[3],
                    })
                    .collect();
                let pb = PixelBuffer::from_pixels(pixels, w, h)
                    .map_err(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc);
                (pb.into(), w, h, true)
            } else {
                let desc = cicp_descriptor(
                    PixelDescriptor::RGB16_SRGB,
                    frame.color_primaries as u16,
                    frame.transfer_characteristics as u16,
                );
                let rgb_data = frame.to_rgb16();
                let pixels: alloc::vec::Vec<Rgb<u16>> = rgb_data
                    .chunks_exact(3)
                    .map(|c| Rgb {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                    })
                    .collect();
                let pb = PixelBuffer::from_pixels(pixels, w, h)
                    .map_err(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc);
                (pb.into(), w, h, false)
            }
        } else {
            // 8-bit path: use negotiated layout for decode.
            let layout = descriptor_to_layout(negotiated);
            let mut req = self
                .config
                .inner
                .decode_request(data)
                .with_output_layout(layout);
            if let Some(ref limits) = self.limits {
                req = req.with_limits(limits);
            }
            if let Some(stop) = self.stop {
                req = req.with_stop(stop);
            }
            if self.thread_count > 0 {
                req = req.with_max_threads(self.thread_count);
            }
            let native_output = req.decode().map_err(|e| e.into_inner())?;
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
        let container = crate::heif::parse(data, &enough::Unstoppable)
            .map_err(|e| e.into_inner())
            .ok();
        let info = build_image_info_full(
            &probe_info.unwrap_or(crate::ImageInfo {
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
            }),
            container.as_ref(),
            width,
            height,
        );
        Ok(DecodeOutput::new(buf, info))
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
}

impl HeicStreamDecoder {
    /// Default strip height for non-grid fallback.
    const FALLBACK_STRIP_HEIGHT: u32 = 64;

    /// Construct a streaming decoder for the given HEIC data.
    fn new(
        data: &[u8],
        preferred: &[PixelDescriptor],
        limits: Option<&crate::Limits>,
        stop: Option<&dyn zc::enough::Stop>,
        thread_count: usize,
    ) -> Result<Self, HeicError> {
        let stop_ref: &dyn enough::Stop = stop.unwrap_or(&enough::Unstoppable);

        // Probe for metadata
        let probe_info = crate::ImageInfo::from_bytes(data).ok();

        let config = crate::DecoderConfig::new();
        let pi = probe_info
            .as_ref()
            .ok_or(HeicError::InvalidData("cannot probe HEIC header"))?;

        // Parse container once for metadata extraction and grid init
        let container = crate::heif::parse(data, stop_ref)
            .map_err(|e| e.into_inner())
            .ok();

        // Build ImageInfo for the trait (uses pre-parsed container)
        let info = build_image_info_full(pi, container.as_ref(), pi.width, pi.height);

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
            });
        }

        // Non-grid fallback: full decode upfront
        let available = available_descriptors(pi.has_alpha, pi.bit_depth);
        let negotiated = negotiate_pixel_format(preferred, &available);

        let pixels: PixelBuffer = if is_16bit(negotiated) {
            let mut req = config.decode_request(data);
            if let Some(lim) = limits {
                req = req.with_limits(lim);
            }
            if let Some(s) = stop {
                req = req.with_stop(s);
            }
            if thread_count > 0 {
                req = req.with_max_threads(thread_count);
            }
            let frame = req.decode_yuv().map_err(|e| e.into_inner())?;
            let has_alpha = frame.alpha_plane.is_some();

            let wants_alpha = negotiated == PixelDescriptor::RGBA16_SRGB;
            if has_alpha || wants_alpha {
                let desc = cicp_descriptor(
                    PixelDescriptor::RGBA16_SRGB,
                    frame.color_primaries as u16,
                    frame.transfer_characteristics as u16,
                );
                let rgba_data = frame.to_rgba16();
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
                PixelBuffer::from_pixels(pixels, w, h)
                    .map_err(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc)
                    .into()
            } else {
                let desc = cicp_descriptor(
                    PixelDescriptor::RGB16_SRGB,
                    frame.color_primaries as u16,
                    frame.transfer_characteristics as u16,
                );
                let rgb_data = frame.to_rgb16();
                let pixels: alloc::vec::Vec<Rgb<u16>> = rgb_data
                    .chunks_exact(3)
                    .map(|c| Rgb {
                        r: c[0],
                        g: c[1],
                        b: c[2],
                    })
                    .collect();
                let w = frame.cropped_width();
                let h = frame.cropped_height();
                PixelBuffer::from_pixels(pixels, w, h)
                    .map_err(|_| HeicError::InvalidData("pixel count mismatch"))?
                    .with_descriptor(desc)
                    .into()
            }
        } else {
            let layout = descriptor_to_layout(negotiated);
            let mut req = config.decode_request(data).with_output_layout(layout);
            if let Some(lim) = limits {
                req = req.with_limits(lim);
            }
            if let Some(s) = stop {
                req = req.with_stop(s);
            }
            if thread_count > 0 {
                req = req.with_max_threads(thread_count);
            }
            let native_output = req.decode().map_err(|e| e.into_inner())?;
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
    ) -> Result<Option<GridState>, HeicError> {
        use crate::heif::{self, ColorInfo, FourCC, ItemType};

        stop.check().map_err(HeicError::Cancelled)?;

        // Use pre-parsed container or parse now
        let owned;
        let container: &heif::HeifContainer<'_> = match pre_parsed {
            Some(c) => c,
            None => {
                owned = heif::parse(data, stop).map_err(|e| e.into_inner())?;
                &owned
            }
        };
        let primary_item = container.primary_item().ok_or(HeicError::NoPrimaryImage)?;

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
        let grid_data = container
            .get_item_data(primary_item.id)
            .map_err(|e| e.into_inner())?;
        if grid_data.len() < 8 {
            return Err(HeicError::InvalidData("Grid descriptor too short"));
        }

        let flags = grid_data[1];
        let rows = grid_data[2] as u32 + 1;
        let cols = grid_data[3] as u32 + 1;
        let (output_width, output_height) = if (flags & 1) != 0 {
            if grid_data.len() < 12 {
                return Err(HeicError::InvalidData(
                    "Grid descriptor too short for 32-bit dims",
                ));
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

        if let Some(lim) = limits {
            lim.check_dimensions(output_width, output_height)
                .map_err(|e| e.into_inner())?;
        }

        // Get tile info
        let tile_ids = container.get_item_references(primary_item.id, FourCC::DIMG);
        let expected_tiles = (rows * cols) as usize;
        if tile_ids.len() != expected_tiles {
            return Err(HeicError::InvalidData("Grid tile count mismatch"));
        }

        let first_tile = container
            .get_item(tile_ids[0])
            .ok_or(HeicError::InvalidData("Missing tile item"))?;
        let tile_config = first_tile
            .hevc_config
            .as_ref()
            .ok_or(HeicError::InvalidData("Missing tile hvcC config"))?
            .clone();
        let (tile_width, tile_height) = first_tile
            .dimensions
            .ok_or(HeicError::InvalidData("Missing tile dimensions"))?;

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
            .map(|&tid| {
                container
                    .get_item_data(tid)
                    .map(|cow| cow.into_owned())
                    .map_err(|e| e.into_inner())
            })
            .collect::<Result<_, _>>()?;

        // Negotiate 8-bit layout for grid tiles (no alpha, ≤8-bit)
        let available = available_descriptors(false, 8);
        let negotiated = negotiate_pixel_format(preferred, &available);
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
    fn decode_grid_row(&mut self) -> Result<Option<(u32, u32, u32)>, HeicError> {
        let grid = self.grid.as_ref().unwrap();
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
        let strip_bytes = grid.output_width as usize * strip_h as usize * bpp;

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
                    .map_err(HeicError::from)?;

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

impl zc::decode::StreamingDecode for HeicStreamDecoder {
    type Error = HeicError;

    fn next_batch(&mut self) -> Result<Option<(u32, zenpixels::PixelSlice<'_>)>, HeicError> {
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
                    .map_err(|_| HeicError::InvalidData("failed to create pixel slice"))?;
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
) -> Result<PixelBuffer, HeicError> {
    let err = |_| HeicError::InvalidData("pixel buffer size mismatch");
    match layout {
        crate::PixelLayout::Rgb8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with RGB8 descriptor
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGB8_SRGB).map_err(err)?)
        }
        crate::PixelLayout::Rgba8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with RGBA8 descriptor
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGBA8_SRGB).map_err(err)?)
        }
        crate::PixelLayout::Bgr8 => {
            // In-place BGR→RGB swizzle via garb, then zero-copy wrap
            garb::bytes::rgb_to_bgr_inplace(&mut raw)
                .map_err(|_| HeicError::InvalidData("BGR swizzle size mismatch"))?;
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::RGB8_SRGB).map_err(err)?)
        }
        crate::PixelLayout::Bgra8 => {
            // Zero-copy: Vec<u8> → PixelBuffer with BGRA8 descriptor
            Ok(PixelBuffer::from_vec(raw, w, h, PixelDescriptor::BGRA8_SRGB).map_err(err)?)
        }
    }
}

// ── Native → trait metadata conversion ─────────────────────────────────────

/// Build a lightweight `zc::ImageInfo` from probe data only.
///
/// Does NOT parse the HEIF container or extract ICC/EXIF/XMP/gain map.
/// Used by `probe()` for cheap header-only metadata.
fn build_image_info_lightweight(pi: &crate::ImageInfo) -> ImageInfo {
    let mut info = ImageInfo::new(pi.width, pi.height, ImageFormat::Heic)
        .with_frame_count(1) // HEIC is always single-frame
        .with_orientation(Orientation::Normal) // Decoder applies transforms
        .with_alpha(pi.has_alpha)
        .with_bit_depth(pi.bit_depth)
        .with_channel_count(if pi.has_alpha { 4 } else { 3 });

    // Set CICP if we have non-default values
    if pi.color_primaries != 2 || pi.transfer_characteristics != 2 || pi.matrix_coefficients != 2 {
        info = info.with_cicp(Cicp::new(
            pi.color_primaries as u8,
            pi.transfer_characteristics as u8,
            pi.matrix_coefficients as u8,
            pi.video_full_range,
        ));
    }

    info
}

/// Build a complete `zc::ImageInfo` with all metadata from a pre-parsed container.
///
/// Extracts ICC profile, EXIF, XMP, and gain map from the container in a
/// single pass, avoiding the cost of re-parsing the HEIF container for each.
fn build_image_info_full(
    pi: &crate::ImageInfo,
    container: Option<&crate::heif::HeifContainer<'_>>,
    width: u32,
    height: u32,
) -> ImageInfo {
    let mut info = ImageInfo::new(width, height, ImageFormat::Heic)
        .with_frame_count(1) // HEIC is always single-frame
        .with_orientation(Orientation::Normal) // Decoder applies transforms
        .with_alpha(pi.has_alpha)
        .with_bit_depth(pi.bit_depth)
        .with_channel_count(if pi.has_alpha { 4 } else { 3 });

    // Set CICP if we have non-default values
    if pi.color_primaries != 2 || pi.transfer_characteristics != 2 || pi.matrix_coefficients != 2 {
        info = info.with_cicp(Cicp::new(
            pi.color_primaries as u8,
            pi.transfer_characteristics as u8,
            pi.matrix_coefficients as u8,
            pi.video_full_range,
        ));
    }

    // Extract all metadata from the pre-parsed container
    if let Some(container) = container {
        let primary_item = container.primary_item();

        // ICC profile from colr box
        if pi.has_icc_profile
            && let Some(ref item) = primary_item
            && let Some(crate::heif::ColorInfo::IccProfile(icc)) = &item.color_info
        {
            info = info.with_icc_profile(icc.clone());
        }

        // HDR gain map (Apple format)
        if let Some(ref item) = primary_item
            && !container
                .find_auxiliary_items(item.id, "urn:com:apple:photo:2020:aux:hdrgainmap")
                .is_empty()
        {
            info = info.with_gain_map(true);
        }

        // EXIF extraction
        if let Some(exif) = extract_exif_from_container(container) {
            info = info.with_exif(exif);
        }

        // XMP extraction
        if let Some(xmp) = extract_xmp_from_container(container) {
            info = info.with_xmp(xmp);
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

/// Convert a [`zc::LimitExceeded`] to a static error message for [`HeicError::LimitExceeded`].
fn limit_exceeded_msg(_e: zc::LimitExceeded) -> &'static str {
    "input data size exceeds max_input_bytes"
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

/// Convert `ProbeError` to `HeicError` for trait compatibility.
fn probe_error_to_heic(e: crate::ProbeError) -> HeicError {
    match e {
        crate::ProbeError::NeedMoreData => HeicError::InvalidData("not enough data to probe"),
        crate::ProbeError::InvalidFormat => HeicError::InvalidData("not a valid HEIC/HEIF file"),
        crate::ProbeError::Corrupt(inner) => inner,
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_creation() {
        let config = HeicDecoderConfig::new();
        assert_eq!(
            <HeicDecoderConfig as zc::decode::DecoderConfig>::formats(),
            &[ImageFormat::Heic]
        );
        let descriptors = <HeicDecoderConfig as zc::decode::DecoderConfig>::supported_descriptors();
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
            <HeicDecoderConfig as zc::decode::DecoderConfig>::formats(),
            &[ImageFormat::Heic]
        );
        let _ = config;
    }

    #[test]
    fn capabilities_reported() {
        let caps = <HeicDecoderConfig as zc::decode::DecoderConfig>::capabilities();
        assert!(caps.icc());
        assert!(caps.exif());
        assert!(caps.xmp());
        assert!(caps.cicp());
        assert!(caps.cancel());
        assert!(caps.cheap_probe());
        assert!(caps.native_16bit());
        assert!(caps.native_alpha());
        assert!(caps.hdr());
        assert!(caps.enforces_max_input_bytes());
    }

    #[test]
    fn job_creation() {
        use zc::decode::DecoderConfig as _;
        let config = HeicDecoderConfig::new();
        let _job = config.job();
    }

    #[test]
    fn full_frame_decoder_returns_unsupported() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config.job().full_frame_decoder(Cow::Borrowed(&[]), &[]);
        assert!(result.is_err());
    }

    #[test]
    fn probe_invalid_data() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};
        let config = HeicDecoderConfig::new();
        let result = config.job().probe(b"not a heic file");
        assert!(result.is_err());
    }

    #[test]
    fn negotiate_no_preference_no_alpha() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[], &available);
        assert_eq!(desc, PixelDescriptor::RGB8_SRGB);
    }

    #[test]
    fn negotiate_no_preference_with_alpha() {
        let available = available_descriptors(true, 8);
        let desc = negotiate_pixel_format(&[], &available);
        assert_eq!(desc, PixelDescriptor::RGBA8_SRGB);
    }

    #[test]
    fn negotiate_rgba_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelDescriptor::RGBA8_SRGB], &available);
        assert_eq!(desc, PixelDescriptor::RGBA8_SRGB);
    }

    #[test]
    fn negotiate_bgra_preference() {
        let available = available_descriptors(false, 8);
        let desc = negotiate_pixel_format(&[PixelDescriptor::BGRA8_SRGB], &available);
        assert_eq!(desc, PixelDescriptor::BGRA8_SRGB);
    }

    #[test]
    fn negotiate_16bit_source_no_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[], &available);
        // 16-bit source with no preference → default to 16-bit
        assert_eq!(desc, PixelDescriptor::RGB16_SRGB);
    }

    #[test]
    fn negotiate_16bit_source_8bit_preference() {
        let available = available_descriptors(false, 10);
        let desc = negotiate_pixel_format(&[PixelDescriptor::RGB8_SRGB], &available);
        // Caller explicitly prefers 8-bit
        assert_eq!(desc, PixelDescriptor::RGB8_SRGB);
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
        assert!(matches!(e, HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::InvalidFormat);
        assert!(matches!(e, HeicError::InvalidData(_)));

        let e = probe_error_to_heic(crate::ProbeError::Corrupt(HeicError::NoPrimaryImage));
        assert!(matches!(e, HeicError::NoPrimaryImage));
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
    fn policy_to_threads_single() {
        assert_eq!(policy_to_threads(ThreadingPolicy::SingleThread), 1);
    }

    #[test]
    fn policy_to_threads_unlimited() {
        assert_eq!(policy_to_threads(ThreadingPolicy::Unlimited), 0);
    }

    #[test]
    fn policy_to_threads_limit_or_single() {
        assert_eq!(
            policy_to_threads(ThreadingPolicy::LimitOrSingle { max_threads: 4 }),
            4
        );
    }

    #[test]
    fn policy_to_threads_limit_or_any() {
        assert_eq!(
            policy_to_threads(ThreadingPolicy::LimitOrAny {
                preferred_max_threads: 8
            }),
            8
        );
    }

    #[test]
    fn policy_to_threads_balanced() {
        let n = policy_to_threads(ThreadingPolicy::Balanced);
        // Balanced should be at least 1 and at most available parallelism
        assert!(n >= 1);
    }

    /// Verify SingleThread decode produces valid output through the zencodec adapter.
    #[test]
    fn single_thread_decode_via_adapter() {
        use zc::decode::{Decode, DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let limits = ResourceLimits::none().with_threading(ThreadingPolicy::SingleThread);
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
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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
            matches!(err, HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn probe_allows_within_max_input_bytes() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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
            matches!(err, HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    #[test]
    fn probe_full_enforces_max_input_bytes() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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
            matches!(err, HeicError::LimitExceeded(_)),
            "expected LimitExceeded, got {err:?}"
        );
    }

    // ── Fix 3: probe() vs probe_full() behavior tests ───────────────────

    #[test]
    fn probe_returns_lightweight_info() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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
        assert_eq!(info.frame_count, Some(1));

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
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

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

        // probe_full() should return dimensions
        assert_eq!(info.width, 3024);
        assert_eq!(info.height, 4032);
        assert_eq!(info.format, ImageFormat::Heic);

        // probe_full() should extract EXIF (iPhone image has EXIF)
        assert!(
            info.embedded_metadata.exif.is_some(),
            "probe_full() should extract EXIF from iPhone HEIC"
        );
    }

    #[test]
    fn probe_and_probe_full_agree_on_dimensions() {
        use zc::decode::{DecodeJob as _, DecoderConfig as _};

        let path =
            std::env::var("HEIC_TEST_DIR").unwrap_or_else(|_| "/home/lilith/work/heic".into());
        let file = format!("{path}/libheif/examples/example.heic");
        let Ok(data) = std::fs::read(&file) else {
            eprintln!("Skipping test: {file} not found");
            return;
        };

        let config = HeicDecoderConfig::new();
        let job_light = config.job();
        let job_full = config.job();
        let light = job_light.probe(&data).expect("probe");
        let full = job_full.probe_full(&data).expect("probe_full");

        assert_eq!(light.width, full.width);
        assert_eq!(light.height, full.height);
        assert_eq!(light.format, full.format);
        assert_eq!(light.frame_count, full.frame_count);
    }
}
