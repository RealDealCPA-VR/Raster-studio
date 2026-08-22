//! Pixel formats. v1 exposes 8-bit RGBA; the enum is designed so 16-bit and
//! 32-bit-float workflows (Phase 3) slot in without changing call sites.

use serde::{Deserialize, Serialize};

/// Channel layout + bit depth of a raster surface.
///
/// Compositing is always done in **linear, premultiplied** space internally
/// (see `color` crate); these formats describe *storage*, not the working
/// space used during blending.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PixelFormat {
    /// 8 bits per channel, RGBA, straight (non-premultiplied) alpha. Default.
    #[default]
    Rgba8,
    /// 16 bits per channel, RGBA. Phase 3 (16-bit workflows).
    Rgba16,
    /// 32-bit float per channel, RGBA. Phase 3+ (HDR / high precision).
    RgbaF32,
}

impl PixelFormat {
    /// Bytes occupied by a single pixel in this format.
    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            PixelFormat::Rgba8 => 4,
            PixelFormat::Rgba16 => 8,
            PixelFormat::RgbaF32 => 16,
        }
    }

    /// Number of color+alpha channels (always 4 for now).
    pub const fn channels(self) -> usize {
        4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_per_pixel_matches_depth() {
        assert_eq!(PixelFormat::Rgba8.bytes_per_pixel(), 4);
        assert_eq!(PixelFormat::Rgba16.bytes_per_pixel(), 8);
        assert_eq!(PixelFormat::RgbaF32.bytes_per_pixel(), 16);
    }
}
