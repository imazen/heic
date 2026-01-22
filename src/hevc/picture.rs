//! Decoded frame representation

use alloc::vec;
use alloc::vec::Vec;

/// Decoded video frame
#[derive(Debug)]
pub struct DecodedFrame {
    /// Width in pixels
    pub width: u32,
    /// Height in pixels
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
}

impl DecodedFrame {
    /// Create a new frame buffer
    pub fn new(width: u32, height: u32) -> Self {
        let luma_size = (width * height) as usize;
        // Assume 4:2:0 chroma subsampling
        let chroma_width = width.div_ceil(2);
        let chroma_height = height.div_ceil(2);
        let chroma_size = (chroma_width * chroma_height) as usize;

        Self {
            width,
            height,
            y_plane: vec![0; luma_size],
            cb_plane: vec![0; chroma_size],
            cr_plane: vec![0; chroma_size],
            bit_depth: 8,
            chroma_format: 1, // 4:2:0
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

        Self {
            width,
            height,
            y_plane: vec![0; luma_size],
            cb_plane: vec![0; chroma_size],
            cr_plane: vec![0; chroma_size],
            bit_depth,
            chroma_format,
        }
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

    /// Convert YCbCr to RGB
    pub fn to_rgb(&self) -> Vec<u8> {
        let mut rgb = Vec::with_capacity((self.width * self.height * 3) as usize);
        let shift = self.bit_depth - 8;

        for y in 0..self.height {
            for x in 0..self.width {
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

    /// Convert YCbCr to RGBA
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
        let shift = self.bit_depth - 8;

        for y in 0..self.height {
            for x in 0..self.width {
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
                rgba.push(255); // Alpha
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
}
