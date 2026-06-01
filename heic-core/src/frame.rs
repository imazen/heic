//! Decoded YCbCr frame produced by every backend.

use alloc::vec::Vec;

use crate::color_convert;
use crate::error::HevcError;
use crate::try_vec;

type Result<T> = core::result::Result<T, HevcError>;

/// Sentinel value for uninitialized pixels.
///
/// The pure-Rust decoder uses this during decoding to distinguish decoded
/// samples from uninitialized ones for reference sample availability
/// (H.265 8.4.4.2.2). Native backends overwrite every pixel before returning
/// so they don't observe this value.
#[doc(hidden)]
pub const UNINIT_SAMPLE: u16 = u16::MAX;

/// Deblocking edge flags per 4x4 block — transform block boundary (vertical).
///
/// Pure-Rust-decoder internal; used by the deblocking pass. Native backends
/// leave the `deblock_flags` field empty.
#[doc(hidden)]
pub const DEBLOCK_FLAG_VERT: u8 = 1;
/// Transform block boundary (horizontal).
#[doc(hidden)]
pub const DEBLOCK_FLAG_HORIZ: u8 = 2;
/// Prediction block boundary (vertical) — distinct from transform boundary.
/// Used for bS derivation: CBF check only applies at transform block edges (H.265 8.7.2.4).
#[doc(hidden)]
pub const DEBLOCK_PB_EDGE_VERT: u8 = 4;
/// Prediction block boundary (horizontal).
#[doc(hidden)]
pub const DEBLOCK_PB_EDGE_HORIZ: u8 = 8;

/// Decoded video frame with YCbCr plane data.
///
/// Returned by the `heic` crate's `DecoderConfig::decode_to_frame` and
/// `DecodeRequest::decode_yuv` for direct YCbCr access before color
/// conversion.
///
/// Backends produce this struct directly. The internal fields
/// (`deblock_flags`, `deblock_stride`, `qp_map`) are doc-hidden and used
/// only by the pure-Rust decoder for its deblocking pass; native backends
/// should leave them at default (empty vecs / zero) values.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    /// Width in pixels (full frame, before cropping)
    pub width: u32,
    /// Height in pixels (full frame, before cropping)
    pub height: u32,
    /// Luma (Y) plane — `u16` samples, `bit_depth` bits significant
    pub y_plane: Vec<u16>,
    /// Cb chroma plane (subsampled per `chroma_format`)
    pub cb_plane: Vec<u16>,
    /// Cr chroma plane (subsampled per `chroma_format`)
    pub cr_plane: Vec<u16>,
    /// Bit depth (8 or 10)
    pub bit_depth: u8,
    /// Chroma format (1=4:2:0, 2=4:2:2, 3=4:4:4)
    pub chroma_format: u8,
    /// Conformance window left offset (in luma samples)
    pub crop_left: u32,
    /// Conformance window right offset (in luma samples)
    pub crop_right: u32,
    /// Conformance window top offset (in luma samples)
    pub crop_top: u32,
    /// Conformance window bottom offset (in luma samples)
    pub crop_bottom: u32,
    /// Alpha plane (optional, from auxiliary alpha image)
    pub alpha_plane: Option<Vec<u16>>,
    /// Video full range flag (from SPS VUI). true = full \[0,255\], false = limited \[16,235\]
    pub full_range: bool,
    /// Matrix coefficients (from SPS VUI). 1=BT.709, 5/6=BT.601, 9=BT.2020, 2=unspecified
    pub matrix_coeffs: u8,
    /// Color primaries (CICP). 1=BT.709, 9=BT.2020, 12=Display P3, 2=unspecified
    pub color_primaries: u8,
    /// Transfer characteristics (CICP). 1=BT.709, 13=sRGB, 16=PQ, 18=HLG, 2=unspecified
    pub transfer_characteristics: u8,
    // -- Internal fields (not part of public API) --
    /// Deblocking edge flags at 4x4 block granularity
    #[doc(hidden)]
    pub deblock_flags: Vec<u8>,
    /// Stride for deblock_flags (width / 4)
    #[doc(hidden)]
    pub deblock_stride: u32,
    /// QP map at 4x4 block granularity (for deblocking)
    #[doc(hidden)]
    pub qp_map: Vec<i8>,
}

impl DecodedFrame {
    /// Create a frame with specific parameters.
    ///
    /// Returns an error if dimensions overflow or allocation fails.
    ///
    /// Pure-Rust-decoder construction primitive. Native backends construct
    /// their own [`DecodedFrame`] directly with already-decoded planes.
    #[doc(hidden)]
    pub fn with_params(width: u32, height: u32, bit_depth: u8, chroma_format: u8) -> Result<Self> {
        let luma_size = width
            .checked_mul(height)
            .ok_or(HevcError::DimensionOverflow)? as usize;

        let (chroma_width, chroma_height) = match chroma_format {
            0 => (0, 0),                                  // Monochrome
            1 => (width.div_ceil(2), height.div_ceil(2)), // 4:2:0
            2 => (width.div_ceil(2), height),             // 4:2:2
            3 => (width, height),                         // 4:4:4
            _ => (width.div_ceil(2), height.div_ceil(2)),
        };

        let chroma_size = chroma_width
            .checked_mul(chroma_height)
            .ok_or(HevcError::DimensionOverflow)? as usize;

        let deblock_stride = width.div_ceil(4);
        let deblock_height = height.div_ceil(4);
        let deblock_size = deblock_stride
            .checked_mul(deblock_height)
            .ok_or(HevcError::DimensionOverflow)? as usize;

        let y_plane = try_vec![UNINIT_SAMPLE; luma_size]?;
        let cb_plane = try_vec![UNINIT_SAMPLE; chroma_size]?;
        let cr_plane = try_vec![UNINIT_SAMPLE; chroma_size]?;
        let deblock_flags = try_vec![0u8; deblock_size]?;
        let qp_map = try_vec![0i8; deblock_size]?;

        Ok(Self {
            width,
            height,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth,
            chroma_format,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            deblock_flags,
            deblock_stride,
            qp_map,
            alpha_plane: None,
            full_range: false,
            matrix_coeffs: 2,
            color_primaries: 2,
            transfer_characteristics: 2,
        })
    }

    /// Create a frame from raw plane data (for fuzz testing).
    ///
    /// Fills planes from provided data, with defaults for deblock/qp maps.
    #[cfg(fuzzing)]
    pub fn from_planes(
        width: u32,
        height: u32,
        bit_depth: u8,
        chroma_format: u8,
        y_plane: Vec<u16>,
        cb_plane: Vec<u16>,
        cr_plane: Vec<u16>,
        full_range: bool,
        matrix_coeffs: u8,
    ) -> Self {
        Self {
            width,
            height,
            y_plane,
            cb_plane,
            cr_plane,
            bit_depth,
            chroma_format,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            deblock_flags: Vec::new(),
            deblock_stride: 0,
            qp_map: Vec::new(),
            alpha_plane: None,
            full_range,
            matrix_coeffs,
            color_primaries: 1,
            transfer_characteristics: 1,
        }
    }

    /// Mark a vertical TU/CU boundary at luma position (x, y) with given size.
    ///
    /// Pure-Rust-decoder-internal; populates [`Self::deblock_flags`] during
    /// CTU decoding so the deblocking pass knows where TU/CU edges fall.
    #[doc(hidden)]
    pub fn mark_tu_boundary(&mut self, x: u32, y: u32, size: u32) {
        let bx = x / 4;
        let by = y / 4;
        let bs = size / 4;

        // Mark vertical edge at x (left edge of TU)
        if x > 0 {
            for j in 0..bs {
                let idx = ((by + j) * self.deblock_stride + bx) as usize;
                if idx < self.deblock_flags.len() {
                    self.deblock_flags[idx] |= DEBLOCK_FLAG_VERT;
                }
            }
        }

        // Mark horizontal edge at y (top edge of TU)
        if y > 0 {
            for i in 0..bs {
                let idx = (by * self.deblock_stride + bx + i) as usize;
                if idx < self.deblock_flags.len() {
                    self.deblock_flags[idx] |= DEBLOCK_FLAG_HORIZ;
                }
            }
        }
    }

    /// Mark a prediction block boundary (H.265 8.7.2.3). Pure-Rust-decoder internal.
    ///
    /// For non-2Nx2N inter partition modes, the internal PB boundary must be marked
    /// separately from transform block boundaries. The bS derivation (8.7.2.4) checks
    /// CBF only at transform block edges, not at PB-only edges.
    #[doc(hidden)]
    pub fn mark_pb_boundary(&mut self, x: u32, y: u32, width: u32, height: u32, vertical: bool) {
        if vertical {
            // Mark vertical PB edge at column x, spanning height rows from y
            if x == 0 {
                return;
            }
            let bx = x / 4;
            let by = y / 4;
            let bs = height / 4;
            for j in 0..bs {
                let idx = ((by + j) * self.deblock_stride + bx) as usize;
                if idx < self.deblock_flags.len() {
                    self.deblock_flags[idx] |= DEBLOCK_PB_EDGE_VERT;
                }
            }
        } else {
            // Mark horizontal PB edge at row y, spanning width columns from x
            if y == 0 {
                return;
            }
            let bx = x / 4;
            let by = y / 4;
            let bs = width / 4;
            for i in 0..bs {
                let idx = (by * self.deblock_stride + bx + i) as usize;
                if idx < self.deblock_flags.len() {
                    self.deblock_flags[idx] |= DEBLOCK_PB_EDGE_HORIZ;
                }
            }
        }
    }

    /// Store QP for a block region at 4x4 granularity. Pure-Rust-decoder internal.
    #[doc(hidden)]
    pub fn store_block_qp(&mut self, x: u32, y: u32, size: u32, qp: i8) {
        let bx = x / 4;
        let by = y / 4;
        let bs = size / 4;
        for j in 0..bs {
            for i in 0..bs {
                let idx = ((by + j) * self.deblock_stride + bx + i) as usize;
                if idx < self.qp_map.len() {
                    self.qp_map[idx] = qp;
                }
            }
        }
    }

    /// Set conformance window cropping. Crop offsets that would underflow
    /// the frame are clamped to leave a non-empty cropped region, so
    /// downstream `cropped_width`/`cropped_height` cannot wrap around even
    /// if a caller bypasses the SPS-parse-time validation.
    ///
    /// Backends call this once they know the SPS conformance window offsets.
    #[doc(hidden)]
    pub fn set_crop(&mut self, left: u32, right: u32, top: u32, bottom: u32) {
        let horiz_room = self.width.saturating_sub(1);
        let vert_room = self.height.saturating_sub(1);
        let left = left.min(horiz_room);
        let right = right.min(horiz_room.saturating_sub(left));
        let top = top.min(vert_room);
        let bottom = bottom.min(vert_room.saturating_sub(top));
        self.crop_left = left;
        self.crop_right = right;
        self.crop_top = top;
        self.crop_bottom = bottom;
    }

    /// Width after conformance window cropping. This is the visible image width.
    ///
    /// Saturates to zero if the crop offsets ever exceed `width` — the SPS
    /// parser rejects such input, but the getters use `saturating_sub` as
    /// defence in depth so a stray crop assignment cannot produce a
    /// near-`u32::MAX` cropped value that downstream `Vec::with_capacity`
    /// callers would treat as a multi-GiB allocation request.
    pub fn cropped_width(&self) -> u32 {
        self.width
            .saturating_sub(self.crop_left)
            .saturating_sub(self.crop_right)
    }

    /// Height after conformance window cropping. This is the visible image height.
    ///
    /// See [`cropped_width`](Self::cropped_width) for saturation rationale.
    pub fn cropped_height(&self) -> u32 {
        self.height
            .saturating_sub(self.crop_top)
            .saturating_sub(self.crop_bottom)
    }

    /// Luma plane stride in pixels (equal to the un-cropped `width`).
    pub fn y_stride(&self) -> usize {
        self.width as usize
    }

    /// Chroma plane stride in pixels. Depends on chroma format:
    /// `width/2` for 4:2:0 and 4:2:2, `width` for 4:4:4, 0 for monochrome.
    pub fn c_stride(&self) -> usize {
        match self.chroma_format {
            0 => 0,
            1 | 2 => self.width.div_ceil(2) as usize,
            3 => self.width as usize,
            _ => self.width.div_ceil(2) as usize,
        }
    }

    /// Convert a single YCbCr pixel to RGB.
    /// y_val, cb_val, cr_val are 8-bit values (0-255).
    /// Selects coefficient matrix based on `matrix_coeffs` field.
    ///
    /// Both full-range and limited-range use integer fixed-point arithmetic.
    /// Full-range: ×256, limited-range: ×2048 with combined Y/C scale factors.
    #[inline(always)]
    fn ycbcr_to_rgb(&self, y_val: i32, cb_val: i32, cr_val: i32) -> (u8, u8, u8) {
        if self.matrix_coeffs == 0 {
            // Identity / GBR (H.273 matrix_coefficients == 0): planes are
            // G(Y) B(Cb) R(Cr) directly — no matrix, no chroma offset.
            return (
                cr_val.clamp(0, 255) as u8,
                y_val.clamp(0, 255) as u8,
                cb_val.clamp(0, 255) as u8,
            );
        }
        let cb = cb_val - 128;
        let cr = cr_val - 128;

        if self.full_range {
            // Full-range: ×256 fixed-point, matches libheif Op_YCbCr420_to_RGB24.
            let (cr_r, cb_g, cr_g, cb_b) = match self.matrix_coeffs {
                1 => (403, -48, -120, 475), // BT.709
                9 => (377, -42, -146, 482), // BT.2020
                _ => (359, -88, -183, 454), // BT.601 (default/unspecified)
            };
            let r = y_val + ((cr_r * cr + 128) >> 8);
            let g = y_val + ((cb_g * cb + cr_g * cr + 128) >> 8);
            let b = y_val + ((cb_b * cb + 128) >> 8);
            (
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                b.clamp(0, 255) as u8,
            )
        } else {
            // Limited-range: ×8192 fixed-point with pre-combined scale factors.
            // Y_scale = 256/219 ≈ 1.1689, C_scale = 256/224 ≈ 1.1429
            // Combined coefficients = round(matrix_coeff * C_scale * 8192)
            let (cr_r, cb_g, cr_g, cb_b) = match self.matrix_coeffs {
                1 => (14744, -1754, -4383, 17373), // BT.709
                9 => (13806, -1541, -5349, 17615), // BT.2020
                _ => (13126, -3222, -6686, 16591), // BT.601 (default/unspecified)
            };
            // Y_coeff = round(1.1689 * 8192) = 9576
            let yv = (y_val - 16) * 9576;
            let r = (yv + cr_r * cr + 4096) >> 13;
            let g = (yv + cb_g * cb + cr_g * cr + 4096) >> 13;
            let b = (yv + cb_b * cb + 4096) >> 13;
            (
                r.clamp(0, 255) as u8,
                g.clamp(0, 255) as u8,
                b.clamp(0, 255) as u8,
            )
        }
    }

    /// Compute `cropped_width * cropped_height` as `usize` with overflow check.
    ///
    /// Dimensions are validated during construction, so overflow here is
    /// not reachable for properly constructed frames.
    fn total_cropped_pixels(&self) -> usize {
        let w = self.cropped_width() as u64;
        let h = self.cropped_height() as u64;
        // Cropped dimensions are always <= full dimensions, which were
        // validated by with_params(), so this cannot overflow.
        let total = w.saturating_mul(h);
        usize::try_from(total).unwrap_or(usize::MAX)
    }

    /// Compute `cropped_width * cropped_height * bpp` as `usize` with overflow check.
    ///
    /// Dimensions are validated during construction, so overflow here is
    /// not reachable for properly constructed frames.
    fn total_cropped_bytes(&self, bytes_per_pixel: usize) -> usize {
        let total = self.total_cropped_pixels();
        total.saturating_mul(bytes_per_pixel)
    }

    /// Convert YCbCr to interleaved RGB bytes with conformance window cropping.
    ///
    /// Returns `cropped_width * cropped_height * 3` bytes in R, G, B order.
    /// Selects the color matrix from [`matrix_coeffs`](Self::matrix_coeffs)
    /// (BT.601, BT.709, or BT.2020) and range from [`full_range`](Self::full_range).
    ///
    /// Uses SIMD-accelerated conversion for 4:2:0 chroma (AVX2 on x86-64).
    pub fn to_rgb(&self) -> Result<Vec<u8>> {
        let mut rgb = try_vec![0u8; self.total_cropped_bytes(3)]?;
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);
        let w = self.width as usize;

        let mut out_idx = 0;

        if self.matrix_coeffs == 0 {
            // Identity / GBR (H.273 matrix_coefficients == 0): the three planes
            // are G (Y), B (Cb), R (Cr) directly — no matrix. Output R=Cr, G=Y,
            // B=Cb with the bit-depth shift. Implies 4:4:4 (full-res chroma).
            let c_stride = self.c_stride();
            for y in y_start..y_end {
                let y_row = y as usize * w;
                let c_row = y as usize * c_stride;
                for x in x_start..x_end {
                    let g = (self.y_plane[y_row + x as usize] >> shift) as i32;
                    let b = (self.cb_plane[c_row + x as usize] >> shift) as i32;
                    let r = (self.cr_plane[c_row + x as usize] >> shift) as i32;
                    rgb[out_idx] = r.clamp(0, 255) as u8;
                    rgb[out_idx + 1] = g.clamp(0, 255) as u8;
                    rgb[out_idx + 2] = b.clamp(0, 255) as u8;
                    out_idx += 3;
                }
            }
            return Ok(rgb);
        }

        if self.chroma_format == 1 {
            // SIMD-accelerated 4:2:0 path (AVX2 when available, scalar fallback)
            let c_stride = self.c_stride();
            color_convert::convert_420_to_rgb(
                &self.y_plane,
                &self.cb_plane,
                &self.cr_plane,
                w,
                c_stride,
                y_start,
                y_end,
                x_start,
                x_end,
                shift as u32,
                self.full_range,
                self.matrix_coeffs,
                &mut rgb,
            );
        } else if self.chroma_format == 3 {
            // SIMD-accelerated 4:4:4 path (full-resolution chroma, no upsampling).
            color_convert::convert_444_to_rgb(
                &self.y_plane,
                &self.cb_plane,
                &self.cr_plane,
                w,
                self.c_stride(),
                y_start,
                y_end,
                x_start,
                x_end,
                shift as u32,
                self.full_range,
                self.matrix_coeffs,
                &mut rgb,
            );
        } else {
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let y_idx = y as usize * w + x as usize;
                    let y_val = (self.y_plane[y_idx] >> shift) as i32;
                    let (cb_val, cr_val) = self.get_chroma(x, y, shift);
                    let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                    rgb[out_idx] = r;
                    rgb[out_idx + 1] = g;
                    rgb[out_idx + 2] = b;
                    out_idx += 3;
                }
            }
        }

        Ok(rgb)
    }

    /// Convert YCbCr to interleaved BGRA bytes with conformance window cropping.
    ///
    /// Returns `cropped_width * cropped_height * 4` bytes in B, G, R, A order.
    /// Uses real alpha from [`alpha_plane`](Self::alpha_plane) if present, otherwise 255.
    pub fn to_bgra(&self) -> Result<Vec<u8>> {
        let mut bgra = Vec::with_capacity(self.total_cropped_bytes(4));
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let mut pixel_idx = 0usize;
        let w = self.width as usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * w + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;

                let (cb_val, cr_val) = self.get_chroma(x, y, shift);

                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                bgra.push(b);
                bgra.push(g);
                bgra.push(r);

                let alpha = if let Some(ref alpha) = self.alpha_plane {
                    if pixel_idx < alpha.len() {
                        (alpha[pixel_idx] >> shift).min(255) as u8
                    } else {
                        255
                    }
                } else {
                    255
                };
                bgra.push(alpha);

                pixel_idx += 1;
            }
        }

        Ok(bgra)
    }

    /// Convert YCbCr to interleaved BGR bytes with conformance window cropping.
    ///
    /// Returns `cropped_width * cropped_height * 3` bytes in B, G, R order.
    pub fn to_bgr(&self) -> Result<Vec<u8>> {
        let mut bgr = Vec::with_capacity(self.total_cropped_bytes(3));
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);
        let w = self.width as usize;

        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * w + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;
                let (cb_val, cr_val) = self.get_chroma(x, y, shift);

                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                bgr.push(b);
                bgr.push(g);
                bgr.push(r);
            }
        }

        Ok(bgr)
    }

    /// Write cropped pixels into a pre-allocated buffer in RGB format.
    ///
    /// The buffer must be at least `cropped_width * cropped_height * 3` bytes.
    /// Returns the number of bytes written (always `cropped_width * cropped_height * 3`).
    pub fn write_rgb_into(&self, output: &mut [u8]) -> usize {
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);
        let w = self.width as usize;

        let mut offset = 0;
        if self.chroma_format == 1 {
            // SIMD-accelerated 4:2:0 path
            let c_stride = self.c_stride();
            let needed = self.total_cropped_bytes(3);
            if output.len() >= needed {
                color_convert::convert_420_to_rgb(
                    &self.y_plane,
                    &self.cb_plane,
                    &self.cr_plane,
                    w,
                    c_stride,
                    y_start,
                    y_end,
                    x_start,
                    x_end,
                    shift as u32,
                    self.full_range,
                    self.matrix_coeffs,
                    output,
                );
            }
        } else {
            for y in y_start..y_end {
                for x in x_start..x_end {
                    let y_idx = y as usize * w + x as usize;
                    let y_val = (self.y_plane[y_idx] >> shift) as i32;
                    let (cb_val, cr_val) = self.get_chroma(x, y, shift);
                    let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                    if offset + 3 <= output.len() {
                        output[offset] = r;
                        output[offset + 1] = g;
                        output[offset + 2] = b;
                        offset += 3;
                    }
                }
            }
        }
        self.total_cropped_bytes(3)
    }

    /// Write cropped pixels into a pre-allocated buffer in RGBA format.
    ///
    /// The buffer must be at least `cropped_width * cropped_height * 4` bytes.
    /// Returns the number of bytes written. Uses real alpha if present, otherwise 255.
    pub fn write_rgba_into(&self, output: &mut [u8]) -> usize {
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let mut offset = 0;
        let mut pixel_idx = 0usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * self.width as usize + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;
                let (cb_val, cr_val) = self.get_chroma(x, y, shift);
                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                let alpha = if let Some(ref alpha) = self.alpha_plane {
                    if pixel_idx < alpha.len() {
                        (alpha[pixel_idx] >> shift).min(255) as u8
                    } else {
                        255
                    }
                } else {
                    255
                };
                if offset + 4 <= output.len() {
                    output[offset] = r;
                    output[offset + 1] = g;
                    output[offset + 2] = b;
                    output[offset + 3] = alpha;
                    offset += 4;
                }
                pixel_idx += 1;
            }
        }
        self.total_cropped_bytes(4)
    }

    /// Write cropped pixels into a pre-allocated buffer in BGRA format.
    ///
    /// The buffer must be at least `cropped_width * cropped_height * 4` bytes.
    /// Returns the number of bytes written. Uses real alpha if present, otherwise 255.
    pub fn write_bgra_into(&self, output: &mut [u8]) -> usize {
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let mut offset = 0;
        let mut pixel_idx = 0usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * self.width as usize + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;
                let (cb_val, cr_val) = self.get_chroma(x, y, shift);
                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                let alpha = if let Some(ref alpha) = self.alpha_plane {
                    if pixel_idx < alpha.len() {
                        (alpha[pixel_idx] >> shift).min(255) as u8
                    } else {
                        255
                    }
                } else {
                    255
                };
                if offset + 4 <= output.len() {
                    output[offset] = b;
                    output[offset + 1] = g;
                    output[offset + 2] = r;
                    output[offset + 3] = alpha;
                    offset += 4;
                }
                pixel_idx += 1;
            }
        }
        self.total_cropped_bytes(4)
    }

    /// Write cropped pixels into a pre-allocated buffer in BGR format.
    ///
    /// The buffer must be at least `cropped_width * cropped_height * 3` bytes.
    /// Returns the number of bytes written.
    pub fn write_bgr_into(&self, output: &mut [u8]) -> usize {
        let shift = self.bit_depth - 8;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let mut offset = 0;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * self.width as usize + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;
                let (cb_val, cr_val) = self.get_chroma(x, y, shift);
                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                if offset + 3 <= output.len() {
                    output[offset] = b;
                    output[offset + 1] = g;
                    output[offset + 2] = r;
                    offset += 3;
                }
            }
        }
        self.total_cropped_bytes(3)
    }

    /// Convert YCbCr to interleaved RGBA bytes with conformance window cropping.
    ///
    /// Returns `cropped_width * cropped_height * 4` bytes in R, G, B, A order.
    /// Uses real alpha from [`alpha_plane`](Self::alpha_plane) if present, otherwise 255.
    pub fn to_rgba(&self) -> Result<Vec<u8>> {
        let mut rgba = Vec::with_capacity(self.total_cropped_bytes(4));
        let shift = self.bit_depth - 8;

        // Iterate over cropped region
        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let mut pixel_idx = 0usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * self.width as usize + x as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;

                let (cb_val, cr_val) = self.get_chroma(x, y, shift);

                let (r, g, b) = self.ycbcr_to_rgb(y_val, cb_val, cr_val);
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);

                let alpha = if let Some(ref alpha) = self.alpha_plane {
                    if pixel_idx < alpha.len() {
                        (alpha[pixel_idx] >> shift).min(255) as u8
                    } else {
                        255
                    }
                } else {
                    255
                };
                rgba.push(alpha);

                pixel_idx += 1;
            }
        }

        Ok(rgba)
    }

    /// Get chroma values for a pixel position
    fn get_chroma(&self, x: u32, y: u32, shift: u8) -> (i32, i32) {
        match self.chroma_format {
            0 => (128, 128), // Monochrome - neutral chroma
            1 => {
                // 4:2:0 - both dimensions halved
                let cx = x / 2;
                let cy = y / 2;
                let c_stride = self.c_stride();
                let c_idx = (cy as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    (self.cb_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                let cr = if c_idx < self.cr_plane.len() {
                    (self.cr_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                (cb, cr)
            }
            2 => {
                // 4:2:2 - horizontal halved
                let cx = x / 2;
                let c_stride = self.c_stride();
                let c_idx = (y as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    (self.cb_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                let cr = if c_idx < self.cr_plane.len() {
                    (self.cr_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                (cb, cr)
            }
            3 => {
                // 4:4:4 - full resolution
                let c_idx = y as usize * self.width as usize + x as usize;
                let cb = if c_idx < self.cb_plane.len() {
                    (self.cb_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                let cr = if c_idx < self.cr_plane.len() {
                    (self.cr_plane[c_idx] >> shift) as i32
                } else {
                    128
                };
                (cb, cr)
            }
            _ => (128, 128),
        }
    }

    /// Get a luma (Y) sample at full-frame coordinates `(x, y)`.
    ///
    /// Coordinates are in the un-cropped frame. Returns 0 if out of bounds.
    /// The returned value has `bit_depth` significant bits.
    #[inline]
    pub fn get_y(&self, x: u32, y: u32) -> u16 {
        // Promote to usize before multiplication so a near-u32::MAX width
        // cannot wrap and produce a wrong-but-in-bounds index.
        let idx = y as usize * self.width as usize + x as usize;
        if idx < self.y_plane.len() {
            self.y_plane[idx]
        } else {
            0
        }
    }

    /// Get a Cb chroma sample at chroma-plane coordinates `(x, y)`.
    ///
    /// Coordinates are in the chroma plane's resolution (see [`c_stride`](Self::c_stride)).
    /// Returns neutral chroma (128 << (bit_depth - 8)) if out of bounds.
    #[inline]
    pub fn get_cb(&self, x: u32, y: u32) -> u16 {
        let stride = self.c_stride();
        let idx = (y as usize) * stride + (x as usize);
        if idx < self.cb_plane.len() {
            self.cb_plane[idx]
        } else {
            128 << (self.bit_depth - 8)
        }
    }

    /// Get a Cr chroma sample at chroma-plane coordinates `(x, y)`.
    ///
    /// Coordinates are in the chroma plane's resolution (see [`c_stride`](Self::c_stride)).
    /// Returns neutral chroma (128 << (bit_depth - 8)) if out of bounds.
    #[inline]
    pub fn get_cr(&self, x: u32, y: u32) -> u16 {
        let stride = self.c_stride();
        let idx = (y as usize) * stride + (x as usize);
        if idx < self.cr_plane.len() {
            self.cr_plane[idx]
        } else {
            128 << (self.bit_depth - 8)
        }
    }

    /// Get a mutable plane slice and stride for a given component.
    ///
    /// Returns `(plane, stride)` where `plane` is the raw pixel data
    /// and `stride` is the number of pixels per row.
    ///
    /// Pure-Rust-decoder internal — used during CTU decoding to write
    /// reconstructed samples directly into the frame planes.
    #[doc(hidden)]
    #[inline]
    pub fn plane_mut(&mut self, c_idx: u8) -> (&mut [u16], usize) {
        match c_idx {
            0 => (&mut self.y_plane, self.width as usize),
            1 => {
                let stride = self.c_stride();
                (&mut self.cb_plane, stride)
            }
            2 => {
                let stride = self.c_stride();
                (&mut self.cr_plane, stride)
            }
            _ => (&mut self.y_plane, self.width as usize),
        }
    }

    /// Get an immutable plane slice and stride for a given component index.
    ///
    /// - `c_idx = 0`: luma (Y), stride = `width`
    /// - `c_idx = 1`: Cb chroma, stride = `c_stride()`
    /// - `c_idx = 2`: Cr chroma, stride = `c_stride()`
    ///
    /// Returns `(plane_data, stride_in_pixels)`.
    #[inline]
    pub fn plane(&self, c_idx: u8) -> (&[u16], usize) {
        match c_idx {
            0 => (&self.y_plane, self.width as usize),
            1 => {
                let stride = self.c_stride();
                (&self.cb_plane, stride)
            }
            2 => {
                let stride = self.c_stride();
                (&self.cr_plane, stride)
            }
            _ => (&self.y_plane, self.width as usize),
        }
    }

    /// Get chroma plane dimensions (width, height).
    ///
    /// Pure-Rust-decoder helper; available across the workspace boundary
    /// because the decoder still lives in a different crate from this type.
    #[doc(hidden)]
    pub fn chroma_dims(&self) -> (u32, u32) {
        match self.chroma_format {
            0 => (0, 0),
            1 => (self.width.div_ceil(2), self.height.div_ceil(2)),
            2 => (self.width.div_ceil(2), self.height),
            3 => (self.width, self.height),
            _ => (self.width.div_ceil(2), self.height.div_ceil(2)),
        }
    }

    // ── 16-bit output methods ────────────────────────────────────────────

    /// Convert YCbCr to interleaved RGB u16 with conformance window cropping.
    ///
    /// Preserves native bit depth precision. Output samples are scaled to
    /// full u16 range: `val * 65535 / ((1 << bit_depth) - 1)`.
    ///
    /// For 8-bit sources, this is equivalent to `to_rgb()` with values upscaled.
    /// For 10-bit sources (iPhone HEIC), this preserves the full 10-bit precision
    /// instead of truncating to 8-bit.
    pub fn to_rgb16(&self) -> Result<Vec<u16>> {
        let total_elems = self.total_cropped_bytes(3); // pixels * 3 channels
        let mut rgb = try_vec![0u16; total_elems]?;

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);
        let w = self.width as usize;

        let max_val = ((1u32 << self.bit_depth) - 1) as i32;
        let neutral = 1i32 << (self.bit_depth - 1);

        let mut out_idx = 0;

        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * w + x as usize;
                let y_val = self.y_plane[y_idx] as i32;
                let (cb_val, cr_val) = self.get_chroma_native(x, y);
                let (r, g, b) = self.ycbcr_to_rgb_native(y_val, cb_val, cr_val, max_val, neutral);
                rgb[out_idx] = scale_to_u16(r, max_val);
                rgb[out_idx + 1] = scale_to_u16(g, max_val);
                rgb[out_idx + 2] = scale_to_u16(b, max_val);
                out_idx += 3;
            }
        }

        Ok(rgb)
    }

    /// Convert YCbCr to interleaved RGBA u16 with conformance window cropping.
    ///
    /// Same precision preservation as [`to_rgb16`](Self::to_rgb16), with alpha
    /// from [`alpha_plane`](Self::alpha_plane) (or max value if absent).
    pub fn to_rgba16(&self) -> Result<Vec<u16>> {
        let total_elems = self.total_cropped_bytes(4); // pixels * 4 channels
        let mut rgba = Vec::with_capacity(total_elems);

        let y_start = self.crop_top;
        let y_end = self.height.saturating_sub(self.crop_bottom);
        let x_start = self.crop_left;
        let x_end = self.width.saturating_sub(self.crop_right);

        let max_val = ((1u32 << self.bit_depth) - 1) as i32;
        let neutral = 1i32 << (self.bit_depth - 1);

        let mut pixel_idx = 0usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = y as usize * self.width as usize + x as usize;
                let y_val = self.y_plane[y_idx] as i32;
                let (cb_val, cr_val) = self.get_chroma_native(x, y);
                let (r, g, b) = self.ycbcr_to_rgb_native(y_val, cb_val, cr_val, max_val, neutral);
                rgba.push(scale_to_u16(r, max_val));
                rgba.push(scale_to_u16(g, max_val));
                rgba.push(scale_to_u16(b, max_val));

                let alpha = if let Some(ref alpha) = self.alpha_plane {
                    if pixel_idx < alpha.len() {
                        scale_to_u16(alpha[pixel_idx] as i32, max_val)
                    } else {
                        u16::MAX
                    }
                } else {
                    u16::MAX
                };
                rgba.push(alpha);

                pixel_idx += 1;
            }
        }

        Ok(rgba)
    }

    /// Get chroma values at native bit depth (no shift).
    fn get_chroma_native(&self, x: u32, y: u32) -> (i32, i32) {
        let neutral = 1i32 << (self.bit_depth - 1);
        match self.chroma_format {
            0 => (neutral, neutral), // Monochrome
            1 => {
                let cx = x / 2;
                let cy = y / 2;
                let c_stride = self.c_stride();
                let c_idx = (cy as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx] as i32
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx] as i32
                } else {
                    neutral
                };
                (cb, cr)
            }
            2 => {
                let cx = x / 2;
                let c_stride = self.c_stride();
                let c_idx = (y as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx] as i32
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx] as i32
                } else {
                    neutral
                };
                (cb, cr)
            }
            3 => {
                let c_idx = y as usize * self.width as usize + x as usize;
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx] as i32
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx] as i32
                } else {
                    neutral
                };
                (cb, cr)
            }
            _ => (neutral, neutral),
        }
    }

    /// YCbCr to RGB at native bit depth. Returns clamped [0, max_val] values.
    ///
    /// Works at any bit depth by using the neutral chroma point and max value
    /// as parameters. Same matrix coefficients as `ycbcr_to_rgb`, but arithmetic
    /// is scaled for the native bit depth.
    fn ycbcr_to_rgb_native(
        &self,
        y_val: i32,
        cb_val: i32,
        cr_val: i32,
        max_val: i32,
        neutral: i32,
    ) -> (i32, i32, i32) {
        // Clamp samples to the native range up front: out-of-range values can't
        // be legitimate luma/chroma, and clamping keeps the fixed-point products
        // below (notably the limited-range Y term scaled by `scale`) within i32.
        let y_val = y_val.clamp(0, max_val);
        let cb = cb_val.clamp(0, max_val) - neutral;
        let cr = cr_val.clamp(0, max_val) - neutral;

        if self.full_range {
            // Full-range: same coefficients as 8-bit, but scaled for bit depth.
            // Coefficients are ×256 fixed-point of the matrix values.
            // For N-bit input, we shift by 8 + (bit_depth - 8) = bit_depth.
            // But the coefficients are designed for 8-bit neutral=128 range,
            // so we need to adjust: coefficients work on cb/cr centered at 0
            // with magnitude ±128 (8-bit) or ±512 (10-bit), etc.
            // The key: coefficient * cb_native / neutral * 128 = coefficient * cb_8bit
            // So: result = y_val + (coeff * cr + neutral) >> 8
            // But cr is at native scale, not 8-bit. The coefficients assume 8-bit
            // cb/cr range. Since neutral = 1 << (bit_depth-1), and for 8-bit
            // neutral = 128, the cb/cr values are already 4x larger for 10-bit.
            // The fixed-point coefficients need no change — the shift compensates.
            let shift = self.bit_depth as i32;
            let half = 1 << (shift - 1);
            let (cr_r, cb_g, cr_g, cb_b) = match self.matrix_coeffs {
                1 => (403, -48, -120, 475), // BT.709
                9 => (377, -42, -146, 482), // BT.2020
                _ => (359, -88, -183, 454), // BT.601 (default/unspecified)
            };
            let r = y_val + ((cr_r * cr + half) >> shift);
            let g = y_val + ((cb_g * cb + cr_g * cr + half) >> shift);
            let b = y_val + ((cb_b * cb + half) >> shift);
            (
                r.clamp(0, max_val),
                g.clamp(0, max_val),
                b.clamp(0, max_val),
            )
        } else {
            // Limited-range at native bit depth.
            // The limited range for N-bit is [16 << (N-8), 235 << (N-8)] for Y,
            // [16 << (N-8), 240 << (N-8)] for Cb/Cr.
            let scale = 1 << (self.bit_depth - 8);
            let y_offset = 16 * scale;
            // Y_scale = max_val / (219 * scale), C_scale = max_val / (224 * scale)
            // Using ×8192 fixed-point like the 8-bit version, but with bit_depth shift.
            let shift = 13 + (self.bit_depth as i32 - 8);
            let half = 1 << (shift - 1);
            let (cr_r, cb_g, cr_g, cb_b) = match self.matrix_coeffs {
                1 => (14744, -1754, -4383, 17373), // BT.709
                9 => (13806, -1541, -5349, 17615), // BT.2020
                _ => (13126, -3222, -6686, 16591), // BT.601 (default/unspecified)
            };
            // The Y scale MUST grow with bit depth. 9576 = round((256/219)·2^13)
            // is the *8-bit* fixed-point Y scale, valid only at shift==13. Here
            // `shift = 13 + (bit_depth-8)` grows with depth but 9576 was NOT
            // rescaled, so luma came out divided by an extra 2^(bit_depth-8):
            // 10/12-bit limited-range white clipped at ~25% brightness. Multiply
            // by `scale` (=2^(bit_depth-8)) so white (Y=235·scale) maps to
            // max_val. The chroma coefficients need no change — native chroma is
            // already `scale`× larger and the larger `shift` compensates it.
            let yv = (y_val - y_offset) * 9576 * scale;
            let r = (yv + cr_r * cr + half) >> shift;
            let g = (yv + cb_g * cb + cr_g * cr + half) >> shift;
            let b = (yv + cb_b * cb + half) >> shift;
            (
                r.clamp(0, max_val),
                g.clamp(0, max_val),
                b.clamp(0, max_val),
            )
        }
    }
}

/// Scale a value from [0, max_native] to [0, 65535].
#[inline]
fn scale_to_u16(val: i32, max_native: i32) -> u16 {
    if max_native == 255 {
        // 8-bit: fast path, shift left by 8 and OR
        let v = val.clamp(0, 255) as u16;
        (v << 8) | v
    } else if max_native == 1023 {
        // 10-bit: multiply by 65535/1023 ≈ 64.06...
        // Use: (val * 65535 + 512) / 1023 for proper rounding
        let v = val.clamp(0, 1023) as u32;
        ((v * 65535 + 512) / 1023) as u16
    } else if max_native == 4095 {
        // 12-bit: multiply by 65535/4095 ≈ 16.003...
        let v = val.clamp(0, 4095) as u32;
        ((v * 65535 + 2048) / 4095) as u16
    } else {
        // Generic path
        let v = val.clamp(0, max_native) as u64;
        ((v * 65535 + (max_native as u64 / 2)) / max_native as u64) as u16
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// Build a minimal frame with the given cropped dimensions.
    /// Internal planes are empty — only used to test overflow checks
    /// in the conversion methods, NOT actual pixel data.
    fn frame_with_cropped_dims(w: u32, h: u32) -> DecodedFrame {
        DecodedFrame {
            width: w,
            height: h,
            y_plane: vec![],
            cb_plane: vec![],
            cr_plane: vec![],
            bit_depth: 8,
            chroma_format: 1,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            deblock_flags: vec![],
            deblock_stride: 0,
            qp_map: vec![],
            alpha_plane: None,
            full_range: false,
            matrix_coeffs: 1,
            color_primaries: 1,
            transfer_characteristics: 1,
        }
    }

    /// Verify that total_cropped_pixels uses u64 arithmetic, not u32.
    /// Before the fix, `(65536u32 * 65536u32)` would overflow u32 to 0.
    /// After the fix, it correctly computes 4,294,967,296 via u64.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn total_cropped_pixels_no_u32_overflow() {
        let frame = frame_with_cropped_dims(65536, 65536);
        // This would have been 0 with u32 arithmetic (65536 * 65536 wraps to 0)
        let total = frame.total_cropped_pixels();
        assert_eq!(total, 65536 * 65536); // 4_294_967_296 on 64-bit
    }

    /// Verify that total_cropped_bytes correctly handles dimensions that
    /// would overflow u32 when multiplied by bytes_per_pixel.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn total_cropped_bytes_no_u32_overflow() {
        let frame = frame_with_cropped_dims(65536, 65536);
        let total_rgb = frame.total_cropped_bytes(3);
        assert_eq!(total_rgb, 65536usize * 65536 * 3); // 12_884_901_888

        let total_rgba = frame.total_cropped_bytes(4);
        assert_eq!(total_rgba, 65536usize * 65536 * 4); // 17_179_869_184
    }

    /// Verify total_cropped_pixels handles large u32 values near u32::MAX.
    /// u32::MAX * 2 = 8_589_934_590 which overflows u32 but fits in u64.
    #[test]
    #[cfg(target_pointer_width = "64")]
    fn total_cropped_pixels_near_u32_max() {
        let frame = frame_with_cropped_dims(u32::MAX, 2);
        let total = frame.total_cropped_pixels();
        assert_eq!(total, u32::MAX as usize * 2);
    }

    /// Verify that a small frame works correctly (regression sanity check).
    #[test]
    fn total_cropped_pixels_small_frame() {
        let frame = frame_with_cropped_dims(100, 200);
        assert_eq!(frame.total_cropped_pixels(), 20_000);
        assert_eq!(frame.total_cropped_bytes(3), 60_000);
        assert_eq!(frame.total_cropped_bytes(4), 80_000);
    }

    /// Build a 2×2 4:2:0 limited-range BT.709 frame at `bit_depth` with every
    /// luma sample = `y` and the single chroma sample at the neutral point.
    fn solid_frame(bit_depth: u8, y: u16) -> DecodedFrame {
        let neutral = 1u16 << (bit_depth - 1);
        DecodedFrame {
            width: 2,
            height: 2,
            y_plane: vec![y; 4],
            cb_plane: vec![neutral; 1],
            cr_plane: vec![neutral; 1],
            bit_depth,
            ..frame_with_cropped_dims(2, 2)
        }
    }

    /// Regression: limited-range YCbCr→RGB at native bit depth must map
    /// full-scale white (Y=235·2^(bd-8)) to 0xFFFF and black (Y=16·2^(bd-8))
    /// to 0 at every bit depth. Before the fix the Y-scale constant (9576, the
    /// 8-bit fixed-point value) was not rescaled for bit_depth>8, so 10/12-bit
    /// limited-range white clipped at ~25% (Y=940 → RGB 256 instead of 1023 →
    /// ~16400/65535). The 8-bit path (shift==13) was unaffected, which is why
    /// the 8-bit RGB comparison tests never caught it.
    #[test]
    fn limited_range_white_black_all_depths() {
        for &(bd, white_y, black_y) in &[(8u8, 235u16, 16u16), (10, 940, 64), (12, 3760, 256)] {
            let white = solid_frame(bd, white_y).to_rgb16().unwrap();
            assert_eq!(
                &white[0..3],
                &[0xFFFF, 0xFFFF, 0xFFFF],
                "bit_depth {bd}: limited-range white must be full 0xFFFF, got {:?}",
                &white[0..3]
            );
            let black = solid_frame(bd, black_y).to_rgb16().unwrap();
            assert_eq!(
                &black[0..3],
                &[0, 0, 0],
                "bit_depth {bd}: limited-range black must be 0, got {:?}",
                &black[0..3]
            );
        }
    }

    /// Regression for CR-1: a frame where the conformance crop offsets
    /// would underflow `width - crop_left - crop_right` must NOT panic
    /// on subtraction. `cropped_width` saturates to zero rather than
    /// wrapping to `~u32::MAX`.
    #[test]
    fn cropped_width_saturates_on_oversized_crop() {
        let mut frame = frame_with_cropped_dims(16, 16);
        // Bypass `set_crop` clamping and assign the raw fields so the
        // getters' saturation behaviour itself is verified.
        frame.crop_left = 0;
        frame.crop_right = 200;
        frame.crop_top = 0;
        frame.crop_bottom = 0;
        // `width - crop_left - crop_right` would underflow; we must get
        // 0, not `u32::MAX - 183`.
        assert_eq!(frame.cropped_width(), 0);
        // total_cropped_pixels and bytes must therefore be 0, not many GiB.
        assert_eq!(frame.total_cropped_pixels(), 0);
        assert_eq!(frame.total_cropped_bytes(4), 0);
    }

    /// Regression for CR-1 (companion): same as above but vertical.
    #[test]
    fn cropped_height_saturates_on_oversized_crop() {
        let mut frame = frame_with_cropped_dims(16, 16);
        frame.crop_top = 100;
        frame.crop_bottom = 100;
        assert_eq!(frame.cropped_height(), 0);
    }

    /// Regression for CR-1: `set_crop` itself clamps so callers cannot
    /// install offsets that wrap the cropped getters even if the SPS
    /// parse-time validation is bypassed.
    #[test]
    fn set_crop_clamps_oversized_offsets() {
        let mut frame = frame_with_cropped_dims(16, 16);
        frame.set_crop(100, 100, 100, 100);
        // After clamping: cropped width >= 1 and height >= 1, never wraps.
        assert!(frame.cropped_width() >= 1);
        assert!(frame.cropped_height() >= 1);
        assert!(frame.cropped_width() <= frame.width);
        assert!(frame.cropped_height() <= frame.height);
    }
}
