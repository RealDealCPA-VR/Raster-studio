//! Mip level math and a simple box-filter downsample.
//!
//! Every raster source and generated composite gets a mip chain so the render
//! graph can pick a level appropriate to the current zoom (avoids uploading /
//! sampling full-resolution tiles when zoomed out).

/// Number of mip levels for an image of `width` x `height`, down to 1x1.
pub fn level_count(width: u32, height: u32) -> u8 {
    let max_dim = width.max(height).max(1);
    // floor(log2(max_dim)) + 1
    (32 - (max_dim.leading_zeros())) as u8
}

/// Dimensions of `level` given base dimensions (each level halves, min 1).
pub fn level_dimensions(width: u32, height: u32, level: u8) -> (u32, u32) {
    let w = (width >> level).max(1);
    let h = (height >> level).max(1);
    (w, h)
}

/// Box-filter downsample RGBA8 by 2x. Returns half-size pixels.
///
/// Reference implementation used both at import time and by golden-image tests;
/// the GPU path will mirror this for parity checks.
pub fn downsample_rgba8_2x(src: &[u8], width: u32, height: u32) -> (Vec<u8>, u32, u32) {
    let dw = (width / 2).max(1);
    let dh = (height / 2).max(1);
    let mut out = vec![0u8; dw as usize * dh as usize * 4];
    for y in 0..dh {
        for x in 0..dw {
            let mut acc = [0u32; 4];
            for dy in 0..2 {
                for dx in 0..2 {
                    let sx = (x * 2 + dx).min(width - 1);
                    let sy = (y * 2 + dy).min(height - 1);
                    let i = ((sy * width + sx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += src[i + c] as u32;
                    }
                }
            }
            let o = ((y * dw + x) * 4) as usize;
            for c in 0..4 {
                out[o + c] = (acc[c] / 4) as u8;
            }
        }
    }
    (out, dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_count_powers_of_two() {
        assert_eq!(level_count(256, 256), 9); // 256,128,...,1
        assert_eq!(level_count(1, 1), 1);
        assert_eq!(level_count(4096, 2048), 13);
    }

    #[test]
    fn downsample_halves_and_averages() {
        // 2x2 solid gray -> 1x1 same gray
        let src = vec![100u8; 16];
        let (out, w, h) = downsample_rgba8_2x(&src, 2, 2);
        assert_eq!((w, h), (1, 1));
        assert_eq!(out, vec![100, 100, 100, 100]);
    }
}
