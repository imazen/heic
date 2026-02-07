//! Reference sample availability tracking for intra prediction (H.265 8.4.4.2.1)
//!
//! In HEVC, intra prediction reference samples are only "available" if they have
//! already been reconstructed. This module tracks which samples have been written
//! to the frame buffer, allowing `fill_border_samples` to correctly distinguish
//! between "reconstructed as 0" and "not yet decoded".

/// Tracks which samples in the frame have been reconstructed.
/// Uses a bitmap for memory efficiency (1 bit per sample).
pub struct ReconstructionMap {
    /// Bitmap for luma plane
    luma: Vec<u8>,
    /// Bitmap for Cb plane
    cb: Vec<u8>,
    /// Bitmap for Cr plane
    cr: Vec<u8>,
    /// Luma width
    width: u32,
    /// Luma height
    height: u32,
    /// Chroma width (for 4:2:0 = width/2)
    chroma_width: u32,
    /// Chroma height (for 4:2:0 = height/2)
    chroma_height: u32,
}

impl ReconstructionMap {
    /// Create a new reconstruction map for the given frame dimensions
    pub fn new(width: u32, height: u32) -> Self {
        let luma_bits = (width * height) as usize;
        let luma_bytes = luma_bits.div_ceil(8);
        let cw = width.div_ceil(2);
        let ch = height.div_ceil(2);
        let chroma_bits = (cw * ch) as usize;
        let chroma_bytes = chroma_bits.div_ceil(8);

        Self {
            luma: vec![0; luma_bytes],
            cb: vec![0; chroma_bytes],
            cr: vec![0; chroma_bytes],
            width,
            height,
            chroma_width: cw,
            chroma_height: ch,
        }
    }

    /// Mark a rectangular block as reconstructed
    /// c_idx: 0=luma, 1=Cb, 2=Cr
    pub fn mark_reconstructed(&mut self, x: u32, y: u32, size: u32, c_idx: u8) {
        let (map, w, h) = match c_idx {
            0 => (&mut self.luma, self.width, self.height),
            1 => (&mut self.cb, self.chroma_width, self.chroma_height),
            2 => (&mut self.cr, self.chroma_width, self.chroma_height),
            _ => return,
        };

        for dy in 0..size {
            let py = y + dy;
            if py >= h {
                break;
            }
            for dx in 0..size {
                let px = x + dx;
                if px >= w {
                    break;
                }
                let idx = (py * w + px) as usize;
                map[idx / 8] |= 1 << (idx % 8);
            }
        }
    }

    /// Check if a single sample has been reconstructed
    /// c_idx: 0=luma, 1=Cb, 2=Cr
    pub fn is_reconstructed(&self, x: u32, y: u32, c_idx: u8) -> bool {
        let (map, w, h) = match c_idx {
            0 => (&self.luma, self.width, self.height),
            1 => (&self.cb, self.chroma_width, self.chroma_height),
            2 => (&self.cr, self.chroma_width, self.chroma_height),
            _ => return false,
        };

        if x >= w || y >= h {
            return false;
        }

        let idx = (y * w + x) as usize;
        (map[idx / 8] >> (idx % 8)) & 1 != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_marking() {
        let mut map = ReconstructionMap::new(16, 16);
        assert!(!map.is_reconstructed(0, 0, 0));
        map.mark_reconstructed(0, 0, 4, 0);
        assert!(map.is_reconstructed(0, 0, 0));
        assert!(map.is_reconstructed(3, 3, 0));
        assert!(!map.is_reconstructed(4, 0, 0));
    }

    #[test]
    fn test_chroma() {
        let mut map = ReconstructionMap::new(16, 16);
        map.mark_reconstructed(0, 0, 4, 1); // Cb
        assert!(map.is_reconstructed(0, 0, 1));
        assert!(!map.is_reconstructed(0, 0, 2)); // Cr not marked
    }
}
