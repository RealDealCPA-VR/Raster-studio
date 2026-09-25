//! W13-J: Filter ▸ Pixelate ▸ Shape Mosaic, ported from Photopea.
//!
//! Photopea's controls (its `ShMs` descriptor and dialog): *Cell Size* (2 to
//! 200 px), *Shape* (Square, Circle, Star), *Spread* (XY, X, Y),
//! *Monochromatic* and *Invert*.
//!
//! Photopea's algorithm, which this follows: the image is cut into square
//! cells, and every cell draws **one shape per colour channel** — red, green
//! and blue — added together over black. Each shape is sized by that
//! channel's mean in the cell (gamma-encoded, then converted to linear light,
//! so the shape's area tracks the light it stands for): with Spread XY both
//! sides are `sqrt(mean) * cell`, with X only the width is `mean * cell` and
//! the height is `cell - 1`, and with Y the other way round. Monochromatic
//! sizes all three shapes by the cell's luma (0.3 R + 0.59 G + 0.11 B), so
//! they coincide in white. Invert draws cyan, magenta and yellow shapes sized
//! by `1 - mean`, multiplied together over white.
//!
//! Shape edges are anti-aliased by 4x4 supersampling (Photopea draws on a
//! canvas, whose anti-aliasing is the browser's). The result is opaque.

use color::{linear_to_srgb, srgb_to_linear, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::fill_tiles;

/// Smallest cell Photopea's dialog accepts, in pixels.
pub const MIN_SHAPE_MOSAIC_SIZE: u32 = 2;
/// Largest cell Photopea's dialog accepts, in pixels.
pub const MAX_SHAPE_MOSAIC_SIZE: u32 = 200;

/// The shape each cell is drawn as (Photopea's *Shape*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MosaicShape {
    /// The unit square.
    Square,
    /// A circle of 0.6 units radius.
    Circle,
    /// Photopea's ten-point star.
    Star,
}

impl MosaicShape {
    /// Every shape, in the dialog's order.
    pub const ALL: [MosaicShape; 3] = [MosaicShape::Square, MosaicShape::Circle, MosaicShape::Star];

    /// Whether the shape-local point `(u, v)` (the shape is centred on the
    /// origin, one unit a side before scaling) is inside.
    fn contains(self, u: f32, v: f32) -> bool {
        match self {
            MosaicShape::Square => u.abs() <= 0.5 && v.abs() <= 0.5,
            MosaicShape::Circle => u * u + v * v <= 0.36,
            MosaicShape::Star => {
                // Even-odd rule over Photopea's polygon.
                let mut inside = false;
                let n = STAR.len();
                for i in 0..n {
                    let (xi, yi) = star_point(i);
                    let (xj, yj) = star_point((i + n - 1) % n);
                    if (yi > v) != (yj > v) && u < (xj - xi) * (v - yi) / (yj - yi) + xi {
                        inside = !inside;
                    }
                }
                inside
            }
        }
    }
}

/// Photopea's star, in its 32-unit design grid (x, y pairs).
const STAR: [(f32, f32); 10] = [
    (27.865, 31.83),
    (17.615, 26.209),
    (7.462, 32.009),
    (9.553, 20.362),
    (0.99, 12.335),
    (12.532, 10.758),
    (17.394, 0.0),
    (22.436, 10.672),
    (34.0, 12.047),
    (25.574, 20.22),
];

/// A star vertex mapped to Photopea's `-0.65 + p * 1.3 / 32`.
fn star_point(i: usize) -> (f32, f32) {
    let (x, y) = STAR[i];
    (-0.65 + x * (1.3 / 32.0), -0.65 + y * (1.3 / 32.0))
}

/// Which sides of the shape the cell's value sizes (Photopea's *Spread*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MosaicSpread {
    /// Both sides, `sqrt(value) * cell` each.
    XY,
    /// The width only, `value * cell`; the height is `cell - 1`.
    X,
    /// The height only, `value * cell`; the width is `cell - 1`.
    Y,
}

impl MosaicSpread {
    /// Every spread, in the dialog's order.
    pub const ALL: [MosaicSpread; 3] = [MosaicSpread::XY, MosaicSpread::X, MosaicSpread::Y];
}

/// One drawn shape: its centre and its width and height, in pixels.
#[derive(Clone, Copy)]
struct Stamp {
    cx: f32,
    cy: f32,
    sx: f32,
    sy: f32,
}

/// Shape Mosaic: see the module documentation.
///
/// `cell` is clamped to `2 ..= 200`.
pub fn shape_mosaic(
    src: &FilterBuffer,
    cell: u32,
    shape: MosaicShape,
    spread: MosaicSpread,
    monochromatic: bool,
    invert: bool,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let size = cell.clamp(MIN_SHAPE_MOSAIC_SIZE, MAX_SHAPE_MOSAIC_SIZE);
    let (w, h) = src.dimensions();
    let (cols, rows) = (w.div_ceil(size), h.div_ceil(size));
    let s = size as f32;
    // Per cell, per channel: the shape's size on each axis.
    let mut stamps = vec![
        [Stamp {
            cx: 0.0,
            cy: 0.0,
            sx: 0.0,
            sy: 0.0
        }; 3];
        (cols * rows) as usize
    ];
    for (i, cell_stamps) in stamps.iter_mut().enumerate() {
        let (ci, cj) = (i as u32 % cols, i as u32 / cols);
        let (x0, y0) = (ci * size, cj * size);
        let (x1, y1) = ((x0 + size).min(w), (y0 + size).min(h));
        let mut acc = [0.0f64; 3];
        for y in y0..y1 {
            for x in x0..x1 {
                let p = unpremultiply(src.get(x, y));
                let e = [0, 1, 2].map(|c| linear_to_srgb(p[c].clamp(0.0, 1.0)));
                let e = if monochromatic {
                    let l = 0.3 * e[0] + 0.59 * e[1] + 0.11 * e[2];
                    [l; 3]
                } else {
                    e
                };
                for c in 0..3 {
                    acc[c] += f64::from(e[c]);
                }
            }
        }
        let n = f64::from((x1 - x0) * (y1 - y0));
        for c in 0..3 {
            let mean = (acc[c] / n) as f32;
            let mut v = srgb_to_linear(mean.clamp(0.0, 1.0));
            if invert {
                v = 1.0 - v;
            }
            let f = match spread {
                MosaicSpread::XY => v.sqrt(),
                _ => v,
            } * s;
            let (sx, sy) = match spread {
                MosaicSpread::XY => (f, f),
                MosaicSpread::X => (f, s - 1.0),
                MosaicSpread::Y => (s - 1.0, f),
            };
            // Photopea translates to the cell centre plus half a pixel.
            cell_stamps[c] = Stamp {
                cx: x0 as f32 + s * 0.5 + 0.5,
                cy: y0 as f32 + s * 0.5 + 0.5,
                sx,
                sy,
            };
        }
    }
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (ci, cj) = ((x / size) as i64, (y / size) as i64);
        // Additive light (or multiplied ink when inverted), per channel.
        let mut light = [0.0f32; 3];
        let mut ink = [1.0f32; 3];
        // A shape reaches at most 0.65 of its size past its centre, so the
        // neighbouring cells are the only other ones that can cover a pixel.
        for nj in (cj - 1)..=(cj + 1) {
            for ni in (ci - 1)..=(ci + 1) {
                if ni < 0 || nj < 0 || ni >= i64::from(cols) || nj >= i64::from(rows) {
                    continue;
                }
                let cell_stamps = &stamps[(nj as u32 * cols + ni as u32) as usize];
                for c in 0..3 {
                    let st = cell_stamps[c];
                    if st.sx <= 0.0 || st.sy <= 0.0 {
                        continue;
                    }
                    let mut inside = 0u32;
                    for sy in 0..4 {
                        for sx in 0..4 {
                            let px = x as f32 + (sx as f32 + 0.5) / 4.0;
                            let py = y as f32 + (sy as f32 + 0.5) / 4.0;
                            if shape.contains((px - st.cx) / st.sx, (py - st.cy) / st.sy) {
                                inside += 1;
                            }
                        }
                    }
                    let cover = inside as f32 / 16.0;
                    light[c] += cover;
                    ink[c] *= 1.0 - cover;
                }
            }
        }
        let e = if invert {
            ink
        } else {
            light.map(|v| v.min(1.0))
        };
        [
            srgb_to_linear(e[0]),
            srgb_to_linear(e[1]),
            srgb_to_linear(e[2]),
            1.0,
        ]
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(w: u32, h: u32, encoded: f32) -> FilterBuffer {
        let l = srgb_to_linear(encoded);
        FilterBuffer::filled(w, h, [l, l, l, 1.0]).unwrap()
    }

    fn encoded_red(b: &FilterBuffer, x: u32, y: u32) -> f32 {
        linear_to_srgb(b.get(x, y)[0])
    }

    #[test]
    fn a_white_cell_is_a_full_square_offset_half_a_pixel() {
        // One 8-pixel cell of white: every channel's square is 8 px a side,
        // centred on 4.5, so it covers [0.5, 8.5]: pixel 0 is half inside.
        let out = shape_mosaic(
            &grey(8, 8, 1.0),
            8,
            MosaicShape::Square,
            MosaicSpread::XY,
            false,
            false,
        );
        assert!(
            (encoded_red(&out, 0, 0) - 0.25).abs() < 1e-5,
            "a quarter at the corner"
        );
        assert!((encoded_red(&out, 0, 4) - 0.5).abs() < 1e-5);
        assert!((encoded_red(&out, 4, 4) - 1.0).abs() < 1e-5);
        // Black draws nothing; inverted, black is a full square of ink.
        let black = shape_mosaic(
            &grey(8, 8, 0.0),
            8,
            MosaicShape::Square,
            MosaicSpread::XY,
            false,
            false,
        );
        assert!(black
            .pixels()
            .iter()
            .all(|p| p[..3] == [0.0; 3] && p[3] == 1.0));
        let ink = shape_mosaic(
            &grey(8, 8, 0.0),
            8,
            MosaicShape::Square,
            MosaicSpread::XY,
            false,
            true,
        );
        assert!((encoded_red(&ink, 4, 4)).abs() < 1e-5);
        assert!((encoded_red(&ink, 0, 4) - 0.5).abs() < 1e-5);
    }

    #[test]
    fn the_shape_side_is_the_square_root_of_linear_light() {
        // Encoded 0.5 grey is 0.214 linear: the XY square is sqrt of that,
        // 0.4626 of the 16-px cell = 7.40 px wide, centred on 8.5, so it
        // covers [4.80, 12.20]: of column 4's supersamples only the one at
        // 4.875 is inside, so it is a quarter covered.
        let v = srgb_to_linear(0.5).sqrt() * 16.0;
        let (lo, hi) = (8.5 - v / 2.0, 8.5 + v / 2.0);
        let out = shape_mosaic(
            &grey(16, 16, 0.5),
            16,
            MosaicShape::Square,
            MosaicSpread::XY,
            false,
            false,
        );
        for x in 0..16u32 {
            let inside = (0..4)
                .filter(|s| {
                    let px = x as f32 + (*s as f32 + 0.5) / 4.0;
                    px >= lo && px <= hi
                })
                .count() as f32;
            let want = inside / 4.0;
            assert!((encoded_red(&out, x, 8) - want).abs() < 1e-5, "column {x}");
        }
        // Spread X: the height is the cell less one pixel, the width is
        // linear * cell = 3.43 px.
        let x_only = shape_mosaic(
            &grey(16, 16, 0.5),
            16,
            MosaicShape::Square,
            MosaicSpread::X,
            false,
            false,
        );
        assert!(
            (encoded_red(&x_only, 8, 2) - 1.0).abs() < 1e-5,
            "full height"
        );
        assert!(encoded_red(&x_only, 4, 8) < 1e-5, "narrow");
        let y_only = shape_mosaic(
            &grey(16, 16, 0.5),
            16,
            MosaicShape::Square,
            MosaicSpread::Y,
            false,
            false,
        );
        assert!(
            (encoded_red(&y_only, 2, 8) - 1.0).abs() < 1e-5,
            "full width"
        );
    }

    #[test]
    fn channels_draw_their_own_shapes_unless_monochromatic() {
        // Pure red: only the red shape is drawn.
        let red = FilterBuffer::filled(12, 12, [1.0, 0.0, 0.0, 1.0]).unwrap();
        let out = shape_mosaic(
            &red,
            12,
            MosaicShape::Circle,
            MosaicSpread::XY,
            false,
            false,
        );
        assert!(out.pixels().iter().all(|p| p[1] == 0.0 && p[2] == 0.0));
        assert!(out.get(6, 6)[0] > 0.99);
        // Monochromatic: luma 0.3 sizes a grey shape in every channel.
        let mono = shape_mosaic(&red, 12, MosaicShape::Circle, MosaicSpread::XY, true, false);
        let p = mono.get(6, 6);
        assert!(p[0] == p[1] && p[1] == p[2] && p[0] > 0.99, "{p:?}");
        // Every shape is deterministic and differs from the others.
        let src = grey(20, 20, 0.7);
        let outs: Vec<_> = MosaicShape::ALL
            .iter()
            .map(|s| shape_mosaic(&src, 10, *s, MosaicSpread::XY, false, false))
            .collect();
        assert_ne!(outs[0], outs[1]);
        assert_ne!(outs[1], outs[2]);
        assert_eq!(
            outs[2],
            shape_mosaic(&src, 10, MosaicShape::Star, MosaicSpread::XY, false, false)
        );
        // The star's centre is inside it and its concave notch is not.
        assert!(MosaicShape::Star.contains(0.0, 0.0));
        assert!(!MosaicShape::Star.contains(-0.62, -0.6));
        let one = FilterBuffer::filled(1, 1, [0.2, 0.2, 0.2, 1.0]).unwrap();
        assert_eq!(
            shape_mosaic(&one, 0, MosaicShape::Circle, MosaicSpread::XY, false, false).dimensions(),
            (1, 1)
        );
    }
}
