//! Decoded frame representation

use alloc::vec;
use alloc::vec::Vec;

/// Sentinel value for uninitialized pixels.
/// Used during decoding to distinguish decoded samples from uninitialized ones
/// for reference sample availability (H.265 8.4.4.2.2).
pub const UNINIT_SAMPLE: u16 = u16::MAX;

/// Deblocking edge flags per 4x4 block
pub const DEBLOCK_FLAG_VERT: u8 = 1;
/// Horizontal edge flag
pub const DEBLOCK_FLAG_HORIZ: u8 = 2;

/// Decoded video frame
#[derive(Debug)]
pub struct DecodedFrame {
    /// Width in pixels (full frame, before cropping)
    pub width: u32,
    /// Height in pixels (full frame, before cropping)
    pub height: u32,
    /// Luma (Y) plane
    pub y_plane: Vec<u16>,
    /// Cb chroma plane (half resolution for 4:2:0)
    pub cb_plane: Vec<u16>,
    /// Cr chroma plane (half resolution for 4:2:0)
    pub cr_plane: Vec<u16>,
    /// Bit depth
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
    /// Deblocking edge flags at 4x4 block granularity
    /// Bit 0 = vertical edge, Bit 1 = horizontal edge
    pub deblock_flags: Vec<u8>,
    /// Stride for deblock_flags (width / 4)
    pub deblock_stride: u32,
    /// QP map at 4x4 block granularity (for deblocking)
    pub qp_map: Vec<i8>,
    /// Alpha plane (optional, from auxiliary alpha image)
    pub alpha_plane: Option<Vec<u16>>,
}

impl DecodedFrame {
    /// Create a new frame buffer
    pub fn new(width: u32, height: u32) -> Self {
        let luma_size = (width * height) as usize;
        // Assume 4:2:0 chroma subsampling
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let chroma_size = (chroma_width * chroma_height) as usize;
        let deblock_stride = width.div_ceil(4);
        let deblock_height = height.div_ceil(4);
        let deblock_size = (deblock_stride * deblock_height) as usize;

        Self {
            width,
            height,
            y_plane: vec![UNINIT_SAMPLE; luma_size],
            cb_plane: vec![UNINIT_SAMPLE; chroma_size],
            cr_plane: vec![UNINIT_SAMPLE; chroma_size],
            bit_depth: 8,
            chroma_format: 1, // 4:2:0
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            deblock_flags: vec![0; deblock_size],
            deblock_stride,
            qp_map: vec![0; deblock_size],
            alpha_plane: None,
        }
    }

    /// Create a frame with specific parameters
    pub fn with_params(width: u32, height: u32, bit_depth: u8, chroma_format: u8) -> Self {
        let luma_size = (width * height) as usize;

        let (chroma_width, chroma_height) = match chroma_format {
            0 => (0, 0),                                  // Monochrome
            1 => (width.div_ceil(2), height.div_ceil(2)), // 4:2:0
            2 => (width.div_ceil(2), height),             // 4:2:2
            3 => (width, height),                         // 4:4:4
            _ => (width.div_ceil(2), height.div_ceil(2)),
        };

        let chroma_size = (chroma_width * chroma_height) as usize;

        let deblock_stride = width.div_ceil(4);
        let deblock_height = height.div_ceil(4);
        let deblock_size = (deblock_stride * deblock_height) as usize;

        Self {
            width,
            height,
            y_plane: vec![UNINIT_SAMPLE; luma_size],
            cb_plane: vec![UNINIT_SAMPLE; chroma_size],
            cr_plane: vec![UNINIT_SAMPLE; chroma_size],
            bit_depth,
            chroma_format,
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
            deblock_flags: vec![0; deblock_size],
            deblock_stride,
            qp_map: vec![0; deblock_size],
            alpha_plane: None,
        }
    }

    /// Mark a vertical TU/CU boundary at luma position (x, y) with given size
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

    /// Store QP for a block region at 4x4 granularity
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

    /// Set conformance window cropping
    pub fn set_crop(&mut self, left: u32, right: u32, top: u32, bottom: u32) {
        self.crop_left = left;
        self.crop_right = right;
        self.crop_top = top;
        self.crop_bottom = bottom;
    }

    /// Get cropped width
    pub fn cropped_width(&self) -> u32 {
        self.width - self.crop_left - self.crop_right
    }

    /// Get cropped height
    pub fn cropped_height(&self) -> u32 {
        self.height - self.crop_top - self.crop_bottom
    }

    /// Get luma stride (width)
    pub fn y_stride(&self) -> usize {
        self.width as usize
    }

    /// Get chroma stride
    pub fn c_stride(&self) -> usize {
        match self.chroma_format {
            0 => 0,
            1 | 2 => self.width.div_ceil(2) as usize,
            3 => self.width as usize,
            _ => self.width.div_ceil(2) as usize,
        }
    }

    /// Convert YCbCr to RGB with conformance window cropping
    pub fn to_rgb(&self) -> Vec<u8> {
        let out_width = self.cropped_width();
        let out_height = self.cropped_height();
        let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);
        let shift = self.bit_depth - 8;

        // Iterate over cropped region
        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = (y * self.width + x) as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;

                // Get chroma values based on format
                let (cb_val, cr_val) = self.get_chroma(x, y, shift);

                // BT.601 YCbCr to RGB conversion
                // R = Y + 1.402 * (Cr - 128)
                // G = Y - 0.344136 * (Cb - 128) - 0.714136 * (Cr - 128)
                // B = Y + 1.772 * (Cb - 128)

                let cb = cb_val - 128;
                let cr = cr_val - 128;

                let r = y_val + ((1436 * cr) >> 10);
                let g = y_val - ((352 * cb + 731 * cr) >> 10);
                let b = y_val + ((1815 * cb) >> 10);

                rgb.push(r.clamp(0, 255) as u8);
                rgb.push(g.clamp(0, 255) as u8);
                rgb.push(b.clamp(0, 255) as u8);
            }
        }

        rgb
    }

    /// Convert YCbCr to RGBA with conformance window cropping.
    /// Uses real alpha values from `alpha_plane` if present, otherwise alpha=255.
    pub fn to_rgba(&self) -> Vec<u8> {
        let out_width = self.cropped_width();
        let out_height = self.cropped_height();
        let mut rgba = Vec::with_capacity((out_width * out_height * 4) as usize);
        let shift = self.bit_depth - 8;

        // Iterate over cropped region
        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        let mut pixel_idx = 0usize;
        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = (y * self.width + x) as usize;
                let y_val = (self.y_plane[y_idx] >> shift) as i32;

                let (cb_val, cr_val) = self.get_chroma(x, y, shift);

                let cb = cb_val - 128;
                let cr = cr_val - 128;

                let r = y_val + ((1436 * cr) >> 10);
                let g = y_val - ((352 * cb + 731 * cr) >> 10);
                let b = y_val + ((1815 * cb) >> 10);

                rgba.push(r.clamp(0, 255) as u8);
                rgba.push(g.clamp(0, 255) as u8);
                rgba.push(b.clamp(0, 255) as u8);

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

        rgba
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
                let c_idx = (y * self.width + x) as usize;
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

    /// Set a luma sample
    #[inline]
    pub fn set_y(&mut self, x: u32, y: u32, value: u16) {
        let idx = (y * self.width + x) as usize;
        if idx < self.y_plane.len() {
            self.y_plane[idx] = value;
        }
    }

    /// Set a Cb chroma sample
    #[inline]
    pub fn set_cb(&mut self, x: u32, y: u32, value: u16) {
        let stride = self.c_stride();
        let idx = (y as usize) * stride + (x as usize);
        if idx < self.cb_plane.len() {
            self.cb_plane[idx] = value;
        }
    }

    /// Set a Cr chroma sample
    #[inline]
    pub fn set_cr(&mut self, x: u32, y: u32, value: u16) {
        let stride = self.c_stride();
        let idx = (y as usize) * stride + (x as usize);
        if idx < self.cr_plane.len() {
            self.cr_plane[idx] = value;
        }
    }

    /// Get a luma sample
    #[inline]
    pub fn get_y(&self, x: u32, y: u32) -> u16 {
        let idx = (y * self.width + x) as usize;
        if idx < self.y_plane.len() {
            self.y_plane[idx]
        } else {
            0
        }
    }

    /// Get a Cb chroma sample
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

    /// Get a Cr chroma sample
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

    /// Get chroma plane dimensions (width, height)
    fn chroma_dims(&self) -> (u32, u32) {
        match self.chroma_format {
            0 => (0, 0),
            1 => (self.width.div_ceil(2), self.height.div_ceil(2)),
            2 => (self.width.div_ceil(2), self.height),
            3 => (self.width, self.height),
            _ => (self.width.div_ceil(2), self.height.div_ceil(2)),
        }
    }

    /// Rotate the frame 90° clockwise, returning a new frame
    pub fn rotate_90_cw(&self) -> Self {
        let ow = self.width;
        let oh = self.height;
        let nw = oh;
        let nh = ow;

        // Rotate luma: dst(dx, dy) = src(dy, oh-1-dx)
        let mut y_plane = vec![0u16; (nw * nh) as usize];
        for dy in 0..nh {
            for dx in 0..nw {
                y_plane[(dy * nw + dx) as usize] =
                    self.y_plane[((oh - 1 - dx) * ow + dy) as usize];
            }
        }

        // Rotate alpha plane (same transform as luma)
        let alpha_plane = self.alpha_plane.as_ref().map(|alpha| {
            let mut rotated = vec![0u16; (nw * nh) as usize];
            for dy in 0..nh {
                for dx in 0..nw {
                    rotated[(dy * nw + dx) as usize] =
                        alpha[((oh - 1 - dx) * ow + dy) as usize];
                }
            }
            rotated
        });

        // Rotate chroma planes
        let (ocw, och) = self.chroma_dims();
        if ocw > 0 && och > 0 {
            let ncw = och;
            let nch = ocw;
            let csz = (ncw * nch) as usize;
            let mut cb_plane = vec![0u16; csz];
            let mut cr_plane = vec![0u16; csz];
            for dy in 0..nch {
                for dx in 0..ncw {
                    let si = (och - 1 - dx) as usize * ocw as usize + dy as usize;
                    let di = dy as usize * ncw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb_plane[di] = self.cb_plane[si];
                        cr_plane[di] = self.cr_plane[si];
                    }
                }
            }

            Self {
                width: nw,
                height: nh,
                y_plane,
                cb_plane,
                cr_plane,
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_bottom,
                crop_right: self.crop_top,
                crop_top: self.crop_left,
                crop_bottom: self.crop_right,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        } else {
            Self {
                width: nw,
                height: nh,
                y_plane,
                cb_plane: Vec::new(),
                cr_plane: Vec::new(),
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_bottom,
                crop_right: self.crop_top,
                crop_top: self.crop_left,
                crop_bottom: self.crop_right,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        }
    }

    /// Rotate the frame 180°, returning a new frame
    pub fn rotate_180(&self) -> Self {
        let w = self.width;
        let h = self.height;

        // Rotate luma: dst(dx, dy) = src(w-1-dx, h-1-dy)
        let mut y_plane = vec![0u16; (w * h) as usize];
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] =
                    self.y_plane[((h - 1 - dy) * w + (w - 1 - dx)) as usize];
            }
        }

        // Rotate alpha plane
        let alpha_plane = self.alpha_plane.as_ref().map(|alpha| {
            let mut rotated = vec![0u16; (w * h) as usize];
            for dy in 0..h {
                for dx in 0..w {
                    rotated[(dy * w + dx) as usize] =
                        alpha[((h - 1 - dy) * w + (w - 1 - dx)) as usize];
                }
            }
            rotated
        });

        // Rotate chroma planes
        let (cw, ch) = self.chroma_dims();
        if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb_plane = vec![0u16; csz];
            let mut cr_plane = vec![0u16; csz];
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = (ch - 1 - dy) as usize * cw as usize + (cw - 1 - dx) as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb_plane[di] = self.cb_plane[si];
                        cr_plane[di] = self.cr_plane[si];
                    }
                }
            }

            Self {
                width: w,
                height: h,
                y_plane,
                cb_plane,
                cr_plane,
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_right,
                crop_right: self.crop_left,
                crop_top: self.crop_bottom,
                crop_bottom: self.crop_top,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        } else {
            Self {
                width: w,
                height: h,
                y_plane,
                cb_plane: Vec::new(),
                cr_plane: Vec::new(),
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_right,
                crop_right: self.crop_left,
                crop_top: self.crop_bottom,
                crop_bottom: self.crop_top,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        }
    }

    /// Rotate the frame 270° clockwise (= 90° counter-clockwise), returning a new frame
    pub fn rotate_270_cw(&self) -> Self {
        let ow = self.width;
        let oh = self.height;
        let nw = oh;
        let nh = ow;

        // Rotate luma: dst(dx, dy) = src(ow-1-dy, dx)
        let mut y_plane = vec![0u16; (nw * nh) as usize];
        for dy in 0..nh {
            for dx in 0..nw {
                y_plane[(dy * nw + dx) as usize] =
                    self.y_plane[(dx * ow + (ow - 1 - dy)) as usize];
            }
        }

        // Rotate alpha plane
        let alpha_plane = self.alpha_plane.as_ref().map(|alpha| {
            let mut rotated = vec![0u16; (nw * nh) as usize];
            for dy in 0..nh {
                for dx in 0..nw {
                    rotated[(dy * nw + dx) as usize] =
                        alpha[(dx * ow + (ow - 1 - dy)) as usize];
                }
            }
            rotated
        });

        // Rotate chroma planes
        let (ocw, och) = self.chroma_dims();
        if ocw > 0 && och > 0 {
            let ncw = och;
            let nch = ocw;
            let csz = (ncw * nch) as usize;
            let mut cb_plane = vec![0u16; csz];
            let mut cr_plane = vec![0u16; csz];
            for dy in 0..nch {
                for dx in 0..ncw {
                    let si = dx as usize * ocw as usize + (ocw - 1 - dy) as usize;
                    let di = dy as usize * ncw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb_plane[di] = self.cb_plane[si];
                        cr_plane[di] = self.cr_plane[si];
                    }
                }
            }

            Self {
                width: nw,
                height: nh,
                y_plane,
                cb_plane,
                cr_plane,
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_top,
                crop_right: self.crop_bottom,
                crop_top: self.crop_right,
                crop_bottom: self.crop_left,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        } else {
            Self {
                width: nw,
                height: nh,
                y_plane,
                cb_plane: Vec::new(),
                cr_plane: Vec::new(),
                bit_depth: self.bit_depth,
                chroma_format: self.chroma_format,
                crop_left: self.crop_top,
                crop_right: self.crop_bottom,
                crop_top: self.crop_right,
                crop_bottom: self.crop_left,
                deblock_flags: Vec::new(),
                deblock_stride: 0,
                qp_map: Vec::new(),
                alpha_plane,
            }
        }
    }

    /// Mirror the frame about the vertical axis (left-right flip)
    pub fn mirror_horizontal(&self) -> Self {
        let w = self.width;
        let h = self.height;

        let mut y_plane = vec![0u16; (w * h) as usize];
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] =
                    self.y_plane[(dy * w + (w - 1 - dx)) as usize];
            }
        }

        let alpha_plane = self.alpha_plane.as_ref().map(|alpha| {
            let mut mirrored = vec![0u16; (w * h) as usize];
            for dy in 0..h {
                for dx in 0..w {
                    mirrored[(dy * w + dx) as usize] =
                        alpha[(dy * w + (w - 1 - dx)) as usize];
                }
            }
            mirrored
        });

        let (cw, ch) = self.chroma_dims();
        if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb_plane = vec![0u16; csz];
            let mut cr_plane = vec![0u16; csz];
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = dy as usize * cw as usize + (cw - 1 - dx) as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb_plane[di] = self.cb_plane[si];
                        cr_plane[di] = self.cr_plane[si];
                    }
                }
            }
            Self {
                width: w, height: h, y_plane, cb_plane, cr_plane,
                bit_depth: self.bit_depth, chroma_format: self.chroma_format,
                crop_left: self.crop_right, crop_right: self.crop_left,
                crop_top: self.crop_top, crop_bottom: self.crop_bottom,
                deblock_flags: Vec::new(), deblock_stride: 0, qp_map: Vec::new(),
                alpha_plane,
            }
        } else {
            Self {
                width: w, height: h, y_plane,
                cb_plane: Vec::new(), cr_plane: Vec::new(),
                bit_depth: self.bit_depth, chroma_format: self.chroma_format,
                crop_left: self.crop_right, crop_right: self.crop_left,
                crop_top: self.crop_top, crop_bottom: self.crop_bottom,
                deblock_flags: Vec::new(), deblock_stride: 0, qp_map: Vec::new(),
                alpha_plane,
            }
        }
    }

    /// Mirror the frame about the horizontal axis (top-bottom flip)
    pub fn mirror_vertical(&self) -> Self {
        let w = self.width;
        let h = self.height;

        let mut y_plane = vec![0u16; (w * h) as usize];
        for dy in 0..h {
            for dx in 0..w {
                y_plane[(dy * w + dx) as usize] =
                    self.y_plane[((h - 1 - dy) * w + dx) as usize];
            }
        }

        let alpha_plane = self.alpha_plane.as_ref().map(|alpha| {
            let mut mirrored = vec![0u16; (w * h) as usize];
            for dy in 0..h {
                for dx in 0..w {
                    mirrored[(dy * w + dx) as usize] =
                        alpha[((h - 1 - dy) * w + dx) as usize];
                }
            }
            mirrored
        });

        let (cw, ch) = self.chroma_dims();
        if cw > 0 && ch > 0 {
            let csz = (cw * ch) as usize;
            let mut cb_plane = vec![0u16; csz];
            let mut cr_plane = vec![0u16; csz];
            for dy in 0..ch {
                for dx in 0..cw {
                    let si = (ch - 1 - dy) as usize * cw as usize + dx as usize;
                    let di = dy as usize * cw as usize + dx as usize;
                    if si < self.cb_plane.len() {
                        cb_plane[di] = self.cb_plane[si];
                        cr_plane[di] = self.cr_plane[si];
                    }
                }
            }
            Self {
                width: w, height: h, y_plane, cb_plane, cr_plane,
                bit_depth: self.bit_depth, chroma_format: self.chroma_format,
                crop_left: self.crop_left, crop_right: self.crop_right,
                crop_top: self.crop_bottom, crop_bottom: self.crop_top,
                deblock_flags: Vec::new(), deblock_stride: 0, qp_map: Vec::new(),
                alpha_plane,
            }
        } else {
            Self {
                width: w, height: h, y_plane,
                cb_plane: Vec::new(), cr_plane: Vec::new(),
                bit_depth: self.bit_depth, chroma_format: self.chroma_format,
                crop_left: self.crop_left, crop_right: self.crop_right,
                crop_top: self.crop_bottom, crop_bottom: self.crop_top,
                deblock_flags: Vec::new(), deblock_stride: 0, qp_map: Vec::new(),
                alpha_plane,
            }
        }
    }
}
