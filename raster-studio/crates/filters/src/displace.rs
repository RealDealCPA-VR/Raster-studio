//! Displace: move every pixel by an amount read from a second image.
//!
//! The map's straight red channel drives the horizontal shift and its green
//! channel the vertical one, so a grey map shifts along the diagonal and a
//! colour map can shift the two axes independently — the Photoshop
//! convention. A value of one half is "no shift"; black is the full negative
//! scale, white the full positive scale.
//!
//! The map is in **straight** linear values, so a half-transparent map still
//! reads as the colour it is; and a shift of exactly zero copies the source
//! pixel through untouched rather than resampling it, so a neutral map is the
//! identity bit for bit.

use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::{fill_tiles, Sampling};

/// How a map of a different size is laid over the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DisplaceFit {
    /// Stretch the map to the source's size.
    #[default]
    Stretch,
    /// Repeat the map from the top-left corner.
    Tile,
}

/// What Displace reads where a shifted pixel lands outside the image —
/// Photoshop's "Undefined Areas" choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DisplaceEdges {
    /// Wrap Around: read from the opposite side, as if the image tiled.
    WrapAround,
    /// Repeat Edge Pixels: read the nearest edge pixel.
    #[default]
    RepeatEdgePixels,
}

impl DisplaceEdges {
    /// The [`Sampling`] that implements this choice, bilinear.
    pub const fn sampling(self) -> Sampling {
        let edge = match self {
            DisplaceEdges::WrapAround => crate::support::EdgeMode::Wrap,
            DisplaceEdges::RepeatEdgePixels => crate::support::EdgeMode::Clamp,
        };
        Sampling::new(edge, crate::support::Interpolation::Bilinear)
    }
}

impl DisplaceFit {
    /// Both choices, in Photoshop's order.
    pub const ALL: [DisplaceFit; 2] = [DisplaceFit::Stretch, DisplaceFit::Tile];

    /// The choice as Photoshop's Displace dialog names it.
    pub const fn label(self) -> &'static str {
        match self {
            DisplaceFit::Stretch => "Stretch to Fit",
            DisplaceFit::Tile => "Tile",
        }
    }
}

impl DisplaceEdges {
    /// Both choices, in Photoshop's order.
    pub const ALL: [DisplaceEdges; 2] =
        [DisplaceEdges::WrapAround, DisplaceEdges::RepeatEdgePixels];

    /// The choice as Photoshop's Displace dialog names it.
    pub const fn label(self) -> &'static str {
        match self {
            DisplaceEdges::WrapAround => "Wrap Around",
            DisplaceEdges::RepeatEdgePixels => "Repeat Edge Pixels",
        }
    }
}

/// An external displacement map — another image, an open document or a file
/// — as the buffer [`displace`] reads.
///
/// Photoshop reads a displacement map's *stored* 8-bit values, not light:
/// 128 is no shift, 0 the full negative scale and 255 the full positive
/// scale. So the bytes are mapped linearly onto `[0, 1]` with 128 landing on
/// exactly one half (0..=128 onto 0..=0.5, 128..=255 onto 0.5..=1), bypassing
/// the sRGB decode [`FilterBuffer::from_rgba8`] would apply — a mid-grey map
/// is then the identity bit for bit. A map is a flat picture, so partial
/// transparency is flattened over neutral grey: a transparent map pixel
/// shifts nothing.
///
/// `rgba8` is straight RGBA8, row-major. `None` for an empty image or a
/// buffer of the wrong length.
pub fn encoded_map(width: u32, height: u32, rgba8: &[u8]) -> Option<FilterBuffer> {
    let n = (width as usize).checked_mul(height as usize)?;
    if n == 0 || rgba8.len() != n.checked_mul(4)? {
        return None;
    }
    let level = |v: u8| -> f32 {
        if v >= 128 {
            0.5 + 0.5 * f32::from(v - 128) / 127.0
        } else {
            0.5 * f32::from(v) / 128.0
        }
    };
    let px = rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| {
            let a = f32::from(p[3]) / 255.0;
            let flat = |v: u8| 0.5 + (level(v) - 0.5) * a;
            [flat(p[0]), flat(p[1]), flat(p[2]), 1.0]
        })
        .collect();
    FilterBuffer::from_pixels(width, height, px).ok()
}

/// Displace `src` by `map`.
///
/// * `scale_x`, `scale_y` — the shift, in pixels, that a fully white map
///   value produces (black produces the negative of it).
/// * `fit` — see [`DisplaceFit`].
///
/// An empty map, or two zero scales, is the identity. Non-finite scales are
/// treated as zero.
pub fn displace(
    src: &FilterBuffer,
    map: &FilterBuffer,
    scale_x: f32,
    scale_y: f32,
    fit: DisplaceFit,
    sampling: Sampling,
) -> FilterBuffer {
    let sx = if scale_x.is_finite() { scale_x } else { 0.0 };
    let sy = if scale_y.is_finite() { scale_y } else { 0.0 };
    if src.is_empty() || map.is_empty() || (sx == 0.0 && sy == 0.0) {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let (mw, mh) = map.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (mx, my) = match fit {
            DisplaceFit::Stretch => (
                ((u64::from(x) * u64::from(mw)) / u64::from(w)) as u32,
                ((u64::from(y) * u64::from(mh)) / u64::from(h)) as u32,
            ),
            DisplaceFit::Tile => (x % mw, y % mh),
        };
        let m = color::unpremultiply(map.get(mx.min(mw - 1), my.min(mh - 1)));
        let dx = (m[0].clamp(0.0, 1.0) - 0.5) * 2.0 * sx;
        let dy = (m[1].clamp(0.0, 1.0) - 0.5) * 2.0 * sy;
        if dx.abs() < 1e-6 && dy.abs() < 1e-6 {
            return src.get(x, y);
        }
        src.sample(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy, sampling)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::support::{EdgeMode, Interpolation};

    fn ramp(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                px.push([x as f32 / w as f32, y as f32 / h as f32, 0.3, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn a_zero_map_is_the_identity_bit_for_bit() {
        let src = ramp(20, 12);
        let neutral = FilterBuffer::filled(20, 12, [0.5, 0.5, 0.5, 1.0]).unwrap();
        let out = displace(
            &src,
            &neutral,
            40.0,
            40.0,
            DisplaceFit::Stretch,
            Sampling::clamped(),
        );
        assert_eq!(out.pixels(), src.pixels());
        // A half-transparent neutral map is still neutral: the map is read
        // straight, not premultiplied.
        let faint = FilterBuffer::filled(7, 5, [0.25, 0.25, 0.25, 0.5]).unwrap();
        let out = displace(
            &src,
            &faint,
            40.0,
            40.0,
            DisplaceFit::Tile,
            Sampling::clamped(),
        );
        assert_eq!(out.pixels(), src.pixels());
        // And so are two zero scales, whatever the map says.
        let loud = FilterBuffer::filled(20, 12, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let out = displace(
            &src,
            &loud,
            0.0,
            0.0,
            DisplaceFit::Stretch,
            Sampling::clamped(),
        );
        assert_eq!(out.pixels(), src.pixels());
    }

    #[test]
    fn a_white_map_shifts_by_the_full_scale_along_each_axis() {
        let src = ramp(32, 32);
        let white = FilterBuffer::filled(1, 1, [1.0, 1.0, 1.0, 1.0]).unwrap();
        let out = displace(
            &src,
            &white,
            3.0,
            5.0,
            DisplaceFit::Stretch,
            Sampling::new(EdgeMode::Clamp, Interpolation::Nearest),
        );
        // Interior pixel (10, 10) now shows what was at (13, 15).
        assert_eq!(out.get(10, 10), src.get(13, 15));
        // Red only drives x: a red map leaves y alone.
        let red = FilterBuffer::filled(1, 1, [1.0, 0.5, 0.5, 1.0]).unwrap();
        let out = displace(
            &src,
            &red,
            3.0,
            5.0,
            DisplaceFit::Tile,
            Sampling::new(EdgeMode::Clamp, Interpolation::Nearest),
        );
        assert_eq!(out.get(10, 10), src.get(13, 10));
    }

    #[test]
    fn stretch_and_tile_lay_a_small_map_differently() {
        let src = ramp(16, 16);
        // Left half white, right half black: stretched it splits the image
        // in two; tiled it alternates every pixel.
        let map = FilterBuffer::from_pixels(2, 1, vec![[1.0, 1.0, 1.0, 1.0], [0.0, 0.0, 0.0, 1.0]])
            .unwrap();
        let nearest = Sampling::new(EdgeMode::Clamp, Interpolation::Nearest);
        let stretched = displace(&src, &map, 2.0, 0.0, DisplaceFit::Stretch, nearest);
        let tiled = displace(&src, &map, 2.0, 0.0, DisplaceFit::Tile, nearest);
        assert_eq!(stretched.get(4, 4), src.get(6, 4));
        assert_eq!(stretched.get(12, 4), src.get(10, 4));
        assert_eq!(tiled.get(4, 4), src.get(6, 4));
        assert_eq!(tiled.get(5, 4), src.get(3, 4));
        assert_ne!(stretched.pixels(), tiled.pixels());
    }

    #[test]
    fn wrap_around_and_repeat_edge_differ_only_where_the_shift_leaves_the_image() {
        let src = ramp(16, 8);
        let white = FilterBuffer::filled(1, 1, [1.0, 0.5, 0.5, 1.0]).unwrap();
        let run = |edges: DisplaceEdges| {
            displace(&src, &white, 4.0, 0.0, DisplaceFit::Tile, edges.sampling())
        };
        let wrap = run(DisplaceEdges::WrapAround);
        let repeat = run(DisplaceEdges::RepeatEdgePixels);
        // Inside: both read four pixels to the right.
        assert_eq!(wrap.get(3, 2), src.get(7, 2));
        assert_eq!(repeat.get(3, 2), src.get(7, 2));
        // Past the right edge: wrap reads the left side, repeat the last
        // column.
        assert_eq!(wrap.get(14, 2), src.get(2, 2));
        assert_eq!(repeat.get(14, 2), src.get(15, 2));
    }

    #[test]
    fn an_encoded_map_reads_stored_bytes_with_128_as_no_shift() {
        let src = ramp(12, 10);
        let bytes = |r: u8, g: u8, a: u8| -> Vec<u8> {
            (0..12 * 10).flat_map(|_| [r, g, 128, a]).collect()
        };
        let nearest = Sampling::new(EdgeMode::Clamp, Interpolation::Nearest);
        // A zero displacement map (every byte 128) is the identity, bit for
        // bit even through bilinear sampling (any sub-pixel shift would blend).
        let zero = encoded_map(12, 10, &bytes(128, 128, 255)).unwrap();
        let bilinear = Sampling::new(EdgeMode::Clamp, Interpolation::Bilinear);
        let out = displace(&src, &zero, 50.0, 50.0, DisplaceFit::Stretch, bilinear);
        assert_eq!(out.pixels(), src.pixels());
        // So is a fully transparent map, whatever its colour.
        let clear = encoded_map(12, 10, &bytes(255, 0, 0)).unwrap();
        let out = displace(&src, &clear, 50.0, 50.0, DisplaceFit::Stretch, nearest);
        assert_eq!(out.pixels(), src.pixels());
        // A known map shifts pixels by the scale: 255 red is +scale in x, 0
        // green is -scale in y.
        let known = encoded_map(12, 10, &bytes(255, 0, 255)).unwrap();
        let out = displace(&src, &known, 3.0, 2.0, DisplaceFit::Stretch, nearest);
        assert_eq!(out.get(4, 5), src.get(7, 3));
        // The wrong length or an empty image is refused.
        assert!(encoded_map(12, 10, &[0; 7]).is_none());
        assert!(encoded_map(0, 10, &[]).is_none());
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        let src = ramp(3, 3);
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        assert_eq!(
            displace(
                &src,
                &empty,
                5.0,
                5.0,
                DisplaceFit::Tile,
                Sampling::clamped()
            )
            .pixels(),
            src.pixels()
        );
        let _ = displace(
            &empty,
            &src,
            5.0,
            5.0,
            DisplaceFit::Tile,
            Sampling::clamped(),
        );
        let _ = displace(
            &src,
            &src,
            f32::NAN,
            f32::INFINITY,
            DisplaceFit::Stretch,
            Sampling::clamped(),
        );
    }
}
