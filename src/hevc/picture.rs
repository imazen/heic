//! Decoded frame representation

use alloc::vec;
use alloc::vec::Vec;
use super::colorspace::ColorSpace;

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
    /// Color space metadata
    pub colorspace: ColorSpace,
    /// Conformance window left offset (in luma samples)
    pub crop_left: u32,
    /// Conformance window right offset (in luma samples)
    pub crop_right: u32,
    /// Conformance window top offset (in luma samples)
    pub crop_top: u32,
    /// Conformance window bottom offset (in luma samples)
    pub crop_bottom: u32,
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
            colorspace: ColorSpace::default(),
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
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
            colorspace: ColorSpace::default(),
            crop_left: 0,
            crop_right: 0,
            crop_top: 0,
            crop_bottom: 0,
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
    #[inline]
    pub fn y_stride(&self) -> usize {
        self.width as usize
    }

    /// Get chroma stride
    #[inline]
    pub fn c_stride(&self) -> usize {
        match self.chroma_format {
            0 => 0,
            1 | 2 => self.width.div_ceil(2) as usize,
            3 => self.width as usize,
            _ => self.width.div_ceil(2) as usize,
        }
    }

    pub fn to_rgb(&self) -> Vec<u8> {
        #[cfg(feature = "parallel")]
        {
            let out_height = self.cropped_height();
            if out_height >= 1000 {
                return self.to_rgb_parallel();
            }
        }
        self.to_rgb_sequential()
    }

    fn to_rgb_sequential(&self) -> Vec<u8> {
        let out_width = self.cropped_width();
        let out_height = self.cropped_height();
        let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);

        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = (y * self.width + x) as usize;
                let y_val = self.y_plane[y_idx];

                let (cb_val, cr_val) = self.get_chroma_u16(x, y);

                let (r, g, b) = self.colorspace.ycbcr_to_rgb8(y_val, cb_val, cr_val, self.bit_depth);
                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
        }

        rgb
    }

    #[cfg(feature = "parallel")]
    fn to_rgb_parallel(&self) -> Vec<u8> {
        use rayon::prelude::*;

        let out_width = self.cropped_width();
        let out_height = self.cropped_height();

        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        let row_data: Vec<Vec<u8>> = (y_start..y_end)
            .into_par_iter()
            .map(|y| {
                let mut row_rgb = Vec::with_capacity((out_width * 3) as usize);
                for x in x_start..x_end {
                    let y_idx = (y * self.width + x) as usize;
                    let y_val = self.y_plane[y_idx];

                    let (cb_val, cr_val) = self.get_chroma_u16(x, y);

                    let (r, g, b) = self.colorspace.ycbcr_to_rgb8(y_val, cb_val, cr_val, self.bit_depth);
                    row_rgb.push(r);
                    row_rgb.push(g);
                    row_rgb.push(b);
                }
                row_rgb
            })
            .collect();

        let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);
        for row in row_data {
            rgb.extend_from_slice(&row);
        }

        rgb
    }

    pub fn to_rgb16(&self) -> Vec<u16> {
        #[cfg(feature = "parallel")]
        {
            let out_height = self.cropped_height();
            if out_height >= 1000 {
                return self.to_rgb16_parallel();
            }
        }
        self.to_rgb16_sequential()
    }

    fn to_rgb16_sequential(&self) -> Vec<u16> {
        let out_width = self.cropped_width();
        let out_height = self.cropped_height();
        let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);

        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        for y in y_start..y_end {
            for x in x_start..x_end {
                let y_idx = (y * self.width + x) as usize;
                let y_val = self.y_plane[y_idx];

                let (cb_val, cr_val) = self.get_chroma_u16(x, y);

                let (r, g, b) = self.colorspace.ycbcr_to_rgb16(y_val, cb_val, cr_val, self.bit_depth);
                rgb.push(r);
                rgb.push(g);
                rgb.push(b);
            }
        }

        rgb
    }

    #[cfg(feature = "parallel")]
    fn to_rgb16_parallel(&self) -> Vec<u16> {
        use rayon::prelude::*;

        let out_width = self.cropped_width();
        let out_height = self.cropped_height();

        let y_start = self.crop_top;
        let y_end = self.height - self.crop_bottom;
        let x_start = self.crop_left;
        let x_end = self.width - self.crop_right;

        let row_data: Vec<Vec<u16>> = (y_start..y_end)
            .into_par_iter()
            .map(|y| {
                let mut row_rgb = Vec::with_capacity((out_width * 3) as usize);
                for x in x_start..x_end {
                    let y_idx = (y * self.width + x) as usize;
                    let y_val = self.y_plane[y_idx];

                    let (cb_val, cr_val) = self.get_chroma_u16(x, y);

                    let (r, g, b) = self.colorspace.ycbcr_to_rgb16(y_val, cb_val, cr_val, self.bit_depth);
                    row_rgb.push(r);
                    row_rgb.push(g);
                    row_rgb.push(b);
                }
                row_rgb
            })
            .collect();

        let mut rgb = Vec::with_capacity((out_width * out_height * 3) as usize);
        for row in row_data {
            rgb.extend_from_slice(&row);
        }

        rgb
    }

    /// Convert YCbCr to RGBA with conformance window cropping
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

    /// Get chroma values at full bit depth (u16)
    fn get_chroma_u16(&self, x: u32, y: u32) -> (u16, u16) {
        // Neutral chroma value at current bit depth
        let neutral = 1 << (self.bit_depth - 1);

        match self.chroma_format {
            0 => (neutral, neutral), // Monochrome - neutral chroma
            1 => {
                // 4:2:0 - both dimensions halved
                let cx = x / 2;
                let cy = y / 2;
                let c_stride = self.c_stride();
                let c_idx = (cy as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx]
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx]
                } else {
                    neutral
                };
                (cb, cr)
            }
            2 => {
                // 4:2:2 - horizontal halved
                let cx = x / 2;
                let c_stride = self.c_stride();
                let c_idx = (y as usize) * c_stride + (cx as usize);
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx]
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx]
                } else {
                    neutral
                };
                (cb, cr)
            }
            3 => {
                // 4:4:4 - full resolution
                let c_idx = (y * self.width + x) as usize;
                let cb = if c_idx < self.cb_plane.len() {
                    self.cb_plane[c_idx]
                } else {
                    neutral
                };
                let cr = if c_idx < self.cr_plane.len() {
                    self.cr_plane[c_idx]
                } else {
                    neutral
                };
                (cb, cr)
            }
            _ => (neutral, neutral),
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
