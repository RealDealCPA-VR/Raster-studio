//! W13-J: Filter ▸ Other ▸ Repeat, Color to Alpha and Dither, ported from
//! Photopea.
//!
//! The controls are Photopea's (its `Rept`, `Ctoa` and `Dthr` descriptors and
//! dialogs), and so are the algorithms, read from Photopea's own filter code:
//!
//! * [`repeat`] tiles the layer's content across the canvas — *Scale* (1 to
//!   300 %), *Row Shift* (-50 to 50 %), *Space X* and *Space Y* (-99 to
//!   200 %), *Auto Color* and *Angle*.
//! * [`color_to_alpha`] removes a colour into transparency — *Color* (black by
//!   default), *Transparency Threshold* and *Opacity Threshold* (0 to 100 %).
//! * [`dither`] reduces the image to a fixed *Palette* (Black & White, RGB
//!   2x2x2, 4x4x4 or 8x8x4) by a *Method* (None, Floyd-Steinberg, Bayer 4x4).
//!
//! Photopea works on 8-bit straight RGBA; these work on the same
//! gamma-encoded, straight values as floats, so they agree with it to within
//! its rounding. Two differences are deliberate and stated where they apply:
//! Dither keeps the layer's alpha (Photopea's writes palette colours, which
//! are opaque), and Repeat samples its copies bilinearly (Photopea's transform
//! resampler is its own).

use color::{linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};
use serde::{Deserialize, Serialize};

use crate::buffer::FilterBuffer;
use crate::support::fill_tiles;

/// Straight, gamma-encoded RGBA of a premultiplied linear pixel.
fn encoded(px: [f32; 4]) -> [f32; 4] {
    let s = unpremultiply(px);
    [
        linear_to_srgb(s[0].clamp(0.0, 1.0)),
        linear_to_srgb(s[1].clamp(0.0, 1.0)),
        linear_to_srgb(s[2].clamp(0.0, 1.0)),
        s[3].clamp(0.0, 1.0),
    ]
}

/// The premultiplied linear pixel of straight, gamma-encoded RGBA.
fn decoded(e: [f32; 4]) -> [f32; 4] {
    premultiply([
        srgb_to_linear(e[0].clamp(0.0, 1.0)),
        srgb_to_linear(e[1].clamp(0.0, 1.0)),
        srgb_to_linear(e[2].clamp(0.0, 1.0)),
        e[3].clamp(0.0, 1.0),
    ])
}

fn finite_or(v: f32, fallback: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        fallback
    }
}

// ---------------------------------------------------------------------------
// Repeat
// ---------------------------------------------------------------------------

/// The part of the layer [`repeat`] tiles: the bounding box of its covered
/// pixels, then trimmed of every border row and column that is exactly the
/// colour of its top-left pixel (Photopea trims both ways). Returns the
/// box `(x0, y0, x1, y1)` (exclusive ends) and that top-left colour, or
/// `None` for a layer with no covered pixel.
/// A pixel rectangle `(x0, y0, x1, y1)`, ends exclusive.
type PixelBox = (u32, u32, u32, u32);

fn repeat_tile(src: &FilterBuffer) -> Option<(PixelBox, [f32; 4])> {
    let (w, h) = src.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for y in 0..h {
        for x in 0..w {
            if src.get(x, y)[3] > 0.0 {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let corner = src.get(x0, y0);
    let row_is_corner = |y: u32, a: u32, b: u32| (a..b).all(|x| src.get(x, y) == corner);
    let col_is_corner = |x: u32, a: u32, b: u32| (a..b).all(|y| src.get(x, y) == corner);
    let (mut t0, mut t1, mut u0, mut u1) = (x0, x1, y0, y1);
    while u0 < u1 && row_is_corner(u0, t0, t1) {
        u0 += 1;
    }
    while u1 > u0 && row_is_corner(u1 - 1, t0, t1) {
        u1 -= 1;
    }
    while t0 < t1 && col_is_corner(t0, u0, u1) {
        t0 += 1;
    }
    while t1 > t0 && col_is_corner(t1 - 1, u0, u1) {
        t1 -= 1;
    }
    // A tile of nothing but the corner colour is kept whole.
    let tile = if t0 < t1 && u0 < u1 {
        (t0, u0, t1, u1)
    } else {
        (x0, y0, x1, y1)
    };
    Some((tile, corner))
}

/// Repeat: tile the layer's content across the whole canvas.
///
/// Photopea's algorithm: the tile is [the trimmed content](repeat_tile); it is
/// scaled by `scale_pct` and turned by `angle_deg`; the copies sit on the
/// lattice spanned by the transformed tile's edges (each truncated to whole
/// pixels), anchored at the canvas origin, with copy `(i, row)` at
/// `((i + row * row_shift) * (1 + space_x)) * edge_x + (row * (1 + space_y)) *
/// edge_y`, rounded to whole pixels. Copies are drawn row by row, left to
/// right, each over the ones before. What shows between them is clear —
/// unless `auto_color` is on, or there is no spacing and the layer is fully
/// opaque, when it is the tile's top-left (trimmed-border) colour.
///
/// Clamped as Photopea's dialog is: scale to `1 ..= 300` %, row shift to
/// `-50 ..= 50` %, spacing to `-99 ..= 200` %. A layer with no covered pixel,
/// or a tile scaled below one pixel, is returned unchanged.
pub fn repeat(
    src: &FilterBuffer,
    scale_pct: f32,
    row_shift_pct: f32,
    space_x_pct: f32,
    space_y_pct: f32,
    auto_color: bool,
    angle_deg: f32,
) -> FilterBuffer {
    let Some(((tx0, ty0, tx1, ty1), corner)) = (if src.is_empty() {
        None
    } else {
        repeat_tile(src)
    }) else {
        return src.clone();
    };
    let (tw, th) = ((tx1 - tx0) as f32, (ty1 - ty0) as f32);
    let s = finite_or(scale_pct, 100.0).clamp(1.0, 300.0) / 100.0;
    let shift = finite_or(row_shift_pct, 0.0).clamp(-50.0, 50.0) / 100.0;
    let gap_x = finite_or(space_x_pct, 0.0).clamp(-99.0, 200.0) / 100.0;
    let gap_y = finite_or(space_y_pct, 0.0).clamp(-99.0, 200.0) / 100.0;
    let theta = finite_or(angle_deg, 0.0).to_radians();
    let (sin, cos) = theta.sin_cos();
    // The transform, tile -> canvas: rotate(theta) . scale(s).
    let m = [s * cos, -s * sin, s * sin, s * cos];
    let inv = [cos / s, sin / s, -sin / s, cos / s];
    // Lattice edges, truncated toward zero as Photopea's are.
    let ex = [(m[0] * tw).trunc(), (m[2] * tw).trunc()];
    let ey = [(m[1] * th).trunc(), (m[3] * th).trunc()];
    let det = ex[0] * ey[1] - ex[1] * ey[0];
    if det.abs() < 0.5 {
        return src.clone();
    }
    let opaque = src.pixels().iter().all(|p| p[3] >= 1.0);
    let background = if auto_color || (gap_x == 0.0 && gap_y == 0.0 && opaque) {
        corner
    } else {
        [0.0; 4]
    };
    let (step_a, step_b) = (1.0 + gap_x, 1.0 + gap_y);
    // Slack, in lattice units, for the truncation, the rounding and the
    // bilinear footprint: two pixels each way.
    let slack_a = 2.0 / ex[0].hypot(ex[1]);
    let slack_b = 2.0 / ey[0].hypot(ey[1]);
    // Bilinear read of the tile, clear outside it.
    let texel = |x: i64, y: i64| -> [f32; 4] {
        if x < 0 || y < 0 || x >= i64::from(tx1 - tx0) || y >= i64::from(ty1 - ty0) {
            return [0.0; 4];
        }
        src.get(tx0 + x as u32, ty0 + y as u32)
    };
    let sample = |qx: f32, qy: f32| -> [f32; 4] {
        let (fx, fy) = (qx - 0.5, qy - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (ax, ay) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let p00 = texel(x0, y0);
        let p10 = texel(x0 + 1, y0);
        let p01 = texel(x0, y0 + 1);
        let p11 = texel(x0 + 1, y0 + 1);
        [0, 1, 2, 3].map(|c| {
            let top = p00[c] + (p10[c] - p00[c]) * ax;
            let bottom = p01[c] + (p11[c] - p01[c]) * ax;
            top + (bottom - top) * ay
        })
    };
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
        // The pixel in lattice coordinates.
        let a = (px * ey[1] - py * ey[0]) / det;
        let b = (ex[0] * py - ex[1] * px) / det;
        let rows =
            ((b - 1.0 - slack_b) / step_b).floor() as i64..=((b + slack_b) / step_b).ceil() as i64;
        // Front to back — the last copy drawn first — so an opaque copy ends
        // the search; "under" compositing gives the same result as painting.
        let mut acc = [0.0f32; 4];
        'rows: for row in rows.rev() {
            let r = row as f32;
            let lo = ((a - 1.0 - slack_a) / step_a - r * shift).floor() as i64;
            let hi = ((a + slack_a) / step_a - r * shift).ceil() as i64;
            for i in (lo..=hi).rev() {
                let fa = (i as f32 + r * shift) * step_a;
                let fb = r * step_b;
                let ox = (fa * ex[0] + fb * ey[0]).round();
                let oy = (fa * ex[1] + fb * ey[1]).round();
                let (dx, dy) = (px - ox, py - oy);
                let q = sample(inv[0] * dx + inv[1] * dy, inv[2] * dx + inv[3] * dy);
                if q[3] <= 0.0 {
                    continue;
                }
                let k = 1.0 - acc[3];
                for c in 0..4 {
                    acc[c] += q[c] * k;
                }
                if acc[3] >= 1.0 {
                    break 'rows;
                }
            }
        }
        let k = 1.0 - acc[3].clamp(0.0, 1.0);
        [0, 1, 2, 3].map(|c| acc[c] + background[c] * k)
    });
    out
}

// ---------------------------------------------------------------------------
// Color to Alpha
// ---------------------------------------------------------------------------

/// Color to Alpha: turn `color` (straight **linear** RGB, as the colour picker
/// hands it over) into transparency.
///
/// Photopea's formula, on encoded values: a pixel's distance from the colour
/// is its largest channel difference, `d = max |p - c|`, divided by the
/// opacity threshold `o` and clamped to `[0, 1]`. Its colour becomes the one
/// that, laid over `c` at coverage `d`, gives back `p` — `(p - c (1 - d)) /
/// d`, clamped — and its alpha becomes the smaller of its own and the
/// transparency ramp `(d - t) / (1 - t)`, with `t` the transparency threshold
/// divided by `o` (clamped; `t = 1` keeps alpha). Both thresholds are
/// fractions in `[0, 1]`. Against black, Photopea's default colour, with the
/// default thresholds, the result composited back over black is exactly the
/// input; against another colour the clamp can lose some of it, as
/// Photopea's does.
pub fn color_to_alpha(
    src: &FilterBuffer,
    color: [f32; 3],
    transparency: f32,
    opacity: f32,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let c = color.map(|v| linear_to_srgb(finite_or(v, 0.0).clamp(0.0, 1.0)));
    let o = finite_or(opacity, 1.0).clamp(0.0, 1.0);
    let t = if o == 0.0 {
        0.0
    } else {
        finite_or(transparency, 0.0).clamp(0.0, 1.0) / o
    };
    let (w, h) = src.dimensions();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let p = encoded(src.get(x, y));
        let diff = (0..3).map(|i| (c[i] - p[i]).abs()).fold(0.0f32, f32::max);
        let d = if o == 0.0 {
            1.0
        } else {
            (diff / o).clamp(0.0, 1.0)
        };
        let inv = if d == 0.0 { 0.0 } else { 1.0 / d };
        let ramp = if t >= 1.0 {
            1.0
        } else {
            ((d - t) / (1.0 - t)).clamp(0.0, 1.0)
        };
        let rgb = [0, 1, 2].map(|i| ((p[i] - c[i] * (1.0 - d)) * inv).clamp(0.0, 1.0));
        decoded([rgb[0], rgb[1], rgb[2], p[3].min(ramp)])
    });
    out
}

// ---------------------------------------------------------------------------
// Dither
// ---------------------------------------------------------------------------

/// Dither's palette (Photopea's *Palette*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DitherPalette {
    /// Black and white.
    BlackWhite,
    /// Two levels a channel: eight colours.
    Rgb222,
    /// Four levels a channel: 64 colours.
    Rgb444,
    /// Eight levels of red and green and four of blue: 256 colours.
    Rgb884,
}

impl DitherPalette {
    /// Every palette, in the dialog's order.
    pub const ALL: [DitherPalette; 4] = [
        DitherPalette::BlackWhite,
        DitherPalette::Rgb222,
        DitherPalette::Rgb444,
        DitherPalette::Rgb884,
    ];

    /// Levels per channel.
    fn levels(self) -> [u32; 3] {
        match self {
            DitherPalette::BlackWhite | DitherPalette::Rgb222 => [2, 2, 2],
            DitherPalette::Rgb444 => [4, 4, 4],
            DitherPalette::Rgb884 => [8, 8, 4],
        }
    }

    /// The palette's colours as 8-bit encoded RGB. Photopea spaces the levels
    /// `floor(255 / (n - 1))` apart, so eight levels top out at 252.
    pub fn colors(self) -> Vec<[u8; 3]> {
        if self == DitherPalette::BlackWhite {
            return vec![[0, 0, 0], [255, 255, 255]];
        }
        let n = self.levels();
        let step = n.map(|k| 255 / (k - 1));
        let mut out = Vec::new();
        for r in 0..n[0] {
            for g in 0..n[1] {
                for b in 0..n[2] {
                    out.push([
                        (r * step[0]) as u8,
                        (g * step[1]) as u8,
                        (b * step[2]) as u8,
                    ]);
                }
            }
        }
        out
    }
}

/// How [`dither`] spreads the error (Photopea's *Method*).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DitherMethod {
    /// Nearest palette colour, no dither.
    None,
    /// Floyd-Steinberg error diffusion (7/16, 3/16, 5/16, 1/16).
    FloydSteinberg,
    /// A 4x4 Bayer threshold pattern.
    Bayer4,
}

impl DitherMethod {
    /// Every method, in the dialog's order.
    pub const ALL: [DitherMethod; 3] = [
        DitherMethod::None,
        DitherMethod::FloydSteinberg,
        DitherMethod::Bayer4,
    ];
}

/// The 4x4 Bayer index matrix Photopea's dither uses.
const BAYER4: [u8; 16] = [0, 8, 2, 10, 12, 4, 14, 6, 3, 11, 1, 9, 15, 7, 13, 5];

/// Dither: map every pixel to the nearest colour of `palette`, spreading the
/// error by `method`.
///
/// Photopea's algorithm (its PNG encoder's dither): the image and the palette
/// are both converted to **linear** light on a 0 to 255 scale; the nearest
/// palette colour is the one at the smallest squared RGB distance; Bayer adds
/// `255 * ((m + 0.5) / 16 - 0.5)` to every channel before the search, `m`
/// being the pixel's entry in the 4x4 matrix; Floyd-Steinberg carries the
/// difference to the right and to the row below, left to right and top to
/// bottom. The output is the chosen palette colour exactly. Alpha is kept (a
/// deliberate difference: Photopea's output is opaque).
pub fn dither(src: &FilterBuffer, palette: DitherPalette, method: DitherMethod) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let colors = palette.colors();
    let lin: Vec<[f32; 3]> = colors
        .iter()
        .map(|c| c.map(|v| 255.0 * srgb_to_linear(f32::from(v) / 255.0)))
        .collect();
    let nearest = |p: [f32; 3]| -> usize {
        let mut best = (f32::INFINITY, 0usize);
        for (i, q) in lin.iter().enumerate() {
            let d = (p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2) + (p[2] - q[2]).powi(2);
            if d < best.0 {
                best = (d, i);
            }
        }
        best.1
    };
    let (w, h) = (src.width() as usize, src.height() as usize);
    let straight: Vec<[f32; 4]> = src.pixels().iter().map(|p| unpremultiply(*p)).collect();
    let mut work: Vec<[f32; 3]> = straight
        .iter()
        .map(|s| [0, 1, 2].map(|c| 255.0 * s[c].clamp(0.0, 1.0)))
        .collect();
    let mut out = vec![[0.0f32; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            let p = work[i].map(|v| v.clamp(0.0, 255.0));
            let probe = if method == DitherMethod::Bayer4 {
                let m = f32::from(BAYER4[(y & 3) * 4 + (x & 3)]);
                let t = 255.0 * (-0.5 + (m + 0.5) / 16.0);
                p.map(|v| (v + t).clamp(0.0, 255.0))
            } else {
                p
            };
            let k = nearest(probe);
            if method == DitherMethod::FloydSteinberg {
                let err = [0, 1, 2].map(|c| p[c] - lin[k][c]);
                let mut spread = |j: usize, weight: f32| {
                    for c in 0..3 {
                        work[j][c] += err[c] * weight / 16.0;
                    }
                };
                if x + 1 < w {
                    spread(i + 1, 7.0);
                }
                if y + 1 < h {
                    if x > 0 {
                        spread(i + w - 1, 3.0);
                    }
                    spread(i + w, 5.0);
                    if x + 1 < w {
                        spread(i + w + 1, 1.0);
                    }
                }
            }
            let e = colors[k].map(|v| f32::from(v) / 255.0);
            out[i] = decoded([e[0], e[1], e[2], encoded(src.pixels()[i])[3]]);
        }
    }
    FilterBuffer::from_pixels(src.width(), src.height(), out).expect("same size as the source")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey(w: u32, h: u32, encoded: f32) -> FilterBuffer {
        let l = srgb_to_linear(encoded);
        FilterBuffer::filled(w, h, [l, l, l, 1.0]).unwrap()
    }

    /// A 6x6 clear layer with a 2x2 tile of four opaque colours at (1, 1).
    fn tile_layer() -> (FilterBuffer, [[f32; 4]; 4]) {
        let c = [
            [1.0, 0.0, 0.0, 1.0],
            [0.0, 1.0, 0.0, 1.0],
            [0.0, 0.0, 1.0, 1.0],
            [0.5, 0.5, 0.0, 1.0],
        ];
        let mut src = FilterBuffer::transparent(6, 6).unwrap();
        src.set(1, 1, c[0]);
        src.set(2, 1, c[1]);
        src.set(1, 2, c[2]);
        src.set(2, 2, c[3]);
        (src, c)
    }

    #[test]
    fn repeat_tiles_the_trimmed_content_from_the_canvas_origin() {
        let (src, c) = tile_layer();
        let tile = |x: u32, y: u32| c[((y % 2) * 2 + x % 2) as usize];
        // Defaults: the 2x2 tile repeated edge to edge from (0, 0).
        let out = repeat(&src, 100.0, 0.0, 0.0, 0.0, false, 0.0);
        for y in 0..6 {
            for x in 0..6 {
                assert_eq!(out.get(x, y), tile(x, y), "({x}, {y})");
            }
        }
        // Space X 100 %: a tile-wide clear gap after every copy.
        let spaced = repeat(&src, 100.0, 0.0, 100.0, 0.0, false, 0.0);
        for y in 0..6 {
            for x in 0..6 {
                let want = if x % 4 < 2 { tile(x, y) } else { [0.0; 4] };
                assert_eq!(spaced.get(x, y), want, "({x}, {y})");
            }
        }
        // Auto Color fills the gap with the tile's top-left colour.
        let filled = repeat(&src, 100.0, 0.0, 100.0, 0.0, true, 0.0);
        assert_eq!(filled.get(2, 0), c[0]);
        // Row Shift 50 %: every other row of copies moves half a tile right.
        let shifted = repeat(&src, 100.0, 50.0, 0.0, 0.0, false, 0.0);
        assert_eq!(shifted.get(0, 0), c[0]);
        assert_eq!(shifted.get(1, 2), c[0], "row 1 starts one pixel in");
        assert_eq!(shifted.get(0, 2), c[1]);
        // Scale 50 %: each copy is one pixel, the bilinear mean of the tile.
        let half = repeat(&src, 50.0, 0.0, 0.0, 0.0, false, 0.0);
        let mean = [0, 1, 2, 3].map(|k| (c[0][k] + c[1][k] + c[2][k] + c[3][k]) / 4.0);
        for p in half.pixels() {
            for k in 0..4 {
                assert!((p[k] - mean[k]).abs() < 1e-6, "{p:?}");
            }
        }
        // Turned a quarter, the pattern is the same four colours rotated.
        let turned = repeat(&src, 100.0, 0.0, 0.0, 0.0, false, 90.0);
        assert_ne!(turned, out);
        for p in turned.pixels() {
            let near = c.iter().any(|q| (0..4).all(|k| (p[k] - q[k]).abs() < 1e-4));
            assert!(near, "{p:?} is not one of the tile's colours");
        }
        // Nothing covered: unchanged.
        let clear = FilterBuffer::transparent(4, 4).unwrap();
        assert_eq!(repeat(&clear, 50.0, 10.0, 10.0, 10.0, true, 30.0), clear);
    }

    #[test]
    fn repeat_trims_a_border_of_the_corner_colour_and_fills_with_it() {
        // An opaque 6x6 white layer with a 2x2 red square at (2, 2): the tile
        // is the red square, the background white (no spacing, opaque).
        let mut src = FilterBuffer::filled(6, 6, [1.0; 4]).unwrap();
        for (x, y) in [(2, 2), (3, 2), (2, 3), (3, 3)] {
            src.set(x, y, [1.0, 0.0, 0.0, 1.0]);
        }
        let out = repeat(&src, 100.0, 0.0, 0.0, 0.0, false, 0.0);
        assert!(out.pixels().iter().all(|p| *p == [1.0, 0.0, 0.0, 1.0]));
        let spaced = repeat(&src, 100.0, 0.0, 50.0, 0.0, false, 0.0);
        // Spacing turns the automatic fill off: the gap column is clear...
        assert_eq!(spaced.get(2, 0), [0.0; 4]);
        // ...unless Auto Color asks for it: then it is the trimmed white.
        let auto = repeat(&src, 100.0, 0.0, 50.0, 0.0, true, 0.0);
        assert_eq!(auto.get(2, 0), [1.0; 4]);
    }

    #[test]
    fn color_to_alpha_is_photopeas_formula() {
        // Default colour black: black vanishes, white is untouched, encoded
        // mid-grey becomes white at 50 %.
        let black = [0.0; 3];
        assert!(color_to_alpha(&grey(2, 2, 0.0), black, 0.0, 1.0)
            .pixels()
            .iter()
            .all(|p| p[3] == 0.0));
        let white = grey(1, 1, 1.0);
        assert_eq!(
            color_to_alpha(&white, black, 0.0, 1.0).to_rgba8(),
            white.to_rgba8()
        );
        let p = encoded(color_to_alpha(&grey(1, 1, 0.5), black, 0.0, 1.0).get(0, 0));
        assert!(
            (p[3] - 0.5).abs() < 1e-5 && (p[0] - 1.0).abs() < 1e-5,
            "{p:?}"
        );
        // Against black, composited back over black the input returns.
        let px = [0.8f32, 0.3, 0.6].map(srgb_to_linear);
        let src = FilterBuffer::filled(1, 1, [px[0], px[1], px[2], 1.0]).unwrap();
        let q = encoded(color_to_alpha(&src, black, 0.0, 1.0).get(0, 0));
        assert!((q[3] - 0.8).abs() < 1e-5, "{q:?}");
        for i in 0..3 {
            assert!((q[i] * q[3] - linear_to_srgb(px[i])).abs() < 1e-4);
        }
        // Against another colour the distance is the largest channel
        // difference: (0.8, 0.3, 0.6) from (0.2, 0.9, 0.5) is 0.6 away, and
        // green unmixes to (0.3 - 0.9 * 0.4) / 0.6 = -0.1, clamped to 0.
        let c = [0.2f32, 0.9, 0.5].map(srgb_to_linear);
        let q = encoded(color_to_alpha(&src, c, 0.0, 1.0).get(0, 0));
        assert!((q[3] - 0.6).abs() < 1e-5, "{q:?}");
        assert!(q[1].abs() < 1e-5, "{q:?}");
        // Opacity threshold 40 %: mid-grey is 0.5 / 0.4 -> clamped 1: opaque,
        // and its colour is itself.
        let solid = encoded(color_to_alpha(&grey(1, 1, 0.5), black, 0.0, 0.4).get(0, 0));
        assert!((solid[3] - 1.0).abs() < 1e-6 && (solid[0] - 0.5).abs() < 1e-5);
        // Transparency threshold 60 %: mid-grey (d = 0.5) is under it, clear.
        let clear = color_to_alpha(&grey(1, 1, 0.5), black, 0.6, 1.0).get(0, 0);
        assert_eq!(clear[3], 0.0);
        // Between the thresholds alpha ramps: d = 0.5, t = 0.25 -> 1/3.
        let ramp = color_to_alpha(&grey(1, 1, 0.5), black, 0.25, 1.0).get(0, 0);
        assert!((ramp[3] - 1.0 / 3.0).abs() < 1e-5, "{ramp:?}");
    }

    #[test]
    fn dither_known_outputs() {
        let count_white = |b: &FilterBuffer| {
            b.to_rgba8()
                .chunks(4)
                .filter(|p| {
                    assert!(p[0] == 0 || p[0] == 255, "{p:?} is not black or white");
                    p[0] == 255
                })
                .count()
        };
        // Encoded 50 % grey is 0.214 linear, 54.6 of 255: nearer black.
        let src = grey(16, 16, 0.5);
        let none = dither(&src, DitherPalette::BlackWhite, DitherMethod::None);
        assert_eq!(count_white(&none), 0);
        // Bayer: white where 54.6 + 255 ((m + 0.5) / 16 - 0.5) > 127.5, that
        // is m >= 13 — three cells in sixteen, 48 of 256.
        let bayer = dither(&src, DitherPalette::BlackWhite, DitherMethod::Bayer4);
        assert_eq!(count_white(&bayer), 48);
        // Floyd-Steinberg keeps the mean light: about 21.4 % white.
        let fs = dither(
            &src,
            DitherPalette::BlackWhite,
            DitherMethod::FloydSteinberg,
        );
        let n = count_white(&fs);
        assert!((50..=60).contains(&n), "{n}");
        assert_eq!(
            fs,
            dither(
                &src,
                DitherPalette::BlackWhite,
                DitherMethod::FloydSteinberg
            )
        );
        // Palettes: Photopea's level spacing.
        assert_eq!(DitherPalette::Rgb222.colors().len(), 8);
        assert_eq!(DitherPalette::Rgb444.colors().len(), 64);
        let p884 = DitherPalette::Rgb884.colors();
        assert_eq!(p884.len(), 256);
        assert_eq!(p884.last(), Some(&[252, 252, 255]));
        // A colour on the palette is kept exactly without a threshold
        // pattern (Bayer's offsets are a full half-range, as Photopea's are,
        // so they move even an on-palette colour).
        let on =
            FilterBuffer::filled(3, 3, decoded([85.0 / 255.0, 170.0 / 255.0, 1.0, 1.0])).unwrap();
        for m in [DitherMethod::None, DitherMethod::FloydSteinberg] {
            let out = dither(&on, DitherPalette::Rgb444, m).to_rgba8();
            assert!(out.chunks(4).all(|p| p == [85, 170, 255, 255]), "{m:?}");
        }
        // Alpha is kept.
        let half = FilterBuffer::filled(2, 2, decoded([0.9, 0.9, 0.9, 0.5])).unwrap();
        let out = dither(&half, DitherPalette::BlackWhite, DitherMethod::None);
        assert!(out.pixels().iter().all(|p| (p[3] - 0.5).abs() < 1e-6));
    }
}
