//! Stylize ▸ Extrude, Tiles and Trace Contour.
//!
//! * [`extrude`] and [`tiles`] rearrange and shade premultiplied linear
//!   pixels. Extrude's face shading multiplies a cell's mean by a factor and
//!   re-clamps, since a lit face can overshoot. Tiles copies pixels, so it
//!   needs no clamp at all; its solid fill is premultiplied on the way in.
//! * [`trace_contour`] compares each straight channel with a level on the
//!   **gamma-encoded** scale — the level is the Photoshop `0..255` slider
//!   normalised to `0..1`, and a user setting it to one half means mid grey
//!   as seen, not as linear light. It says so here like the other encoded-
//!   space filters in this crate.

use serde::{Deserialize, Serialize};

use color::{linear_srgb_luminance, linear_to_srgb, premultiply, unpremultiply};

use crate::buffer::{clamp_premultiplied, FilterBuffer};
use crate::rng::hash_unit;
use crate::support::{fill_tiles, EdgeMode};

/// Largest extrude cell and tile count accepted.
pub const MAX_EXTRUDE_SIZE: u32 = 512;

/// The solid [`extrude`] builds out of each cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExtrudeType {
    /// A block whose top face is inset by the height, with four bevelled sides.
    #[default]
    Blocks,
    /// Four triangular faces meeting at the cell's centre.
    Pyramids,
}

/// Where [`extrude`] takes each cell's height from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ExtrudeDepth {
    /// A random height per cell, seeded.
    #[default]
    Random,
    /// The cell's own brightness: light cells stand out further.
    LevelBased,
}

/// The largest fraction of a block's half-width its top face gives up to the
/// sides, at full depth.
const MAX_BLOCK_INSET: f32 = 0.4;

/// Shading for the faces, as a multiplier of the cell colour at height one,
/// for a light from the upper left.
const TOP_LIT: f32 = 0.3;
const LEFT_LIT: f32 = 0.1;
const TOP_SIDE_SHADE: f32 = -0.05;
const RIGHT_SHADE: f32 = -0.3;
const BOTTOM_SHADE: f32 = -0.45;

/// Extrude: the image as a field of shaded blocks or pyramids.
///
/// * `kind` — see [`ExtrudeType`].
/// * `size` — the cell side in pixels; clamped to `2..=MAX_EXTRUDE_SIZE`.
/// * `depth` — `0..=100`, how tall the solids are. Zero draws flat cells, so
///   the result is a mosaic.
/// * `depth_mode`, `seed` — see [`ExtrudeDepth`]; the seed matters only for
///   `Random`.
///
/// Each cell takes its mean premultiplied colour; the faces multiply it by a
/// per-face factor scaled by the cell's height and re-clamp.
pub fn extrude(
    src: &FilterBuffer,
    kind: ExtrudeType,
    size: u32,
    depth: f32,
    depth_mode: ExtrudeDepth,
    seed: u64,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let cell = size.clamp(2, MAX_EXTRUDE_SIZE);
    let depth = if depth.is_finite() {
        depth.clamp(0.0, 100.0) / 100.0
    } else {
        0.0
    };
    let (w, h) = src.dimensions();
    let (cx, cy) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut sums = vec![[0.0f64; 4]; (cx as usize) * (cy as usize)];
    let mut counts = vec![0u32; sums.len()];
    for y in 0..h {
        for x in 0..w {
            let idx = (y / cell) as usize * cx as usize + (x / cell) as usize;
            let p = src.get(x, y);
            for (acc, v) in sums[idx].iter_mut().zip(p.iter()) {
                *acc += f64::from(*v);
            }
            counts[idx] += 1;
        }
    }
    let cells: Vec<([f32; 4], f32)> = sums
        .iter()
        .zip(counts.iter())
        .enumerate()
        .map(|(i, (s, &n))| {
            let mean = if n == 0 {
                [0.0; 4]
            } else {
                let inv = 1.0 / f64::from(n);
                [
                    (s[0] * inv) as f32,
                    (s[1] * inv) as f32,
                    (s[2] * inv) as f32,
                    (s[3] * inv) as f32,
                ]
            };
            let height = match depth_mode {
                ExtrudeDepth::Random => {
                    hash_unit(seed, (i % cx as usize) as i64, (i / cx as usize) as i64)
                }
                ExtrudeDepth::LevelBased => {
                    let straight = unpremultiply(mean);
                    linear_to_srgb(
                        linear_srgb_luminance([straight[0], straight[1], straight[2]])
                            .clamp(0.0, 1.0),
                    )
                }
            };
            (mean, height * depth)
        })
        .collect();
    let inv_cell = 1.0 / cell as f32;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let idx = (y / cell) as usize * cx as usize + (x / cell) as usize;
        let (mean, height) = cells[idx];
        // Local coordinates in `-0.5..0.5`, from the cell's centre.
        let u = ((x % cell) as f32 + 0.5) * inv_cell - 0.5;
        let v = ((y % cell) as f32 + 0.5) * inv_cell - 0.5;
        let factor = match kind {
            ExtrudeType::Blocks => {
                let inset = 0.5 * MAX_BLOCK_INSET * height;
                if u.abs() < 0.5 - inset && v.abs() < 0.5 - inset {
                    1.0 + TOP_LIT * height
                } else {
                    side_shade(u, v, height)
                }
            }
            ExtrudeType::Pyramids => side_shade(u, v, height),
        };
        clamp_premultiplied([
            mean[0] * factor,
            mean[1] * factor,
            mean[2] * factor,
            mean[3],
        ])
    });
    out
}

/// Which of the four side faces `(u, v)` falls on, and how it is lit.
fn side_shade(u: f32, v: f32, height: f32) -> f32 {
    let shade = if u.abs() >= v.abs() {
        if u < 0.0 {
            LEFT_LIT
        } else {
            RIGHT_SHADE
        }
    } else if v < 0.0 {
        TOP_SIDE_SHADE
    } else {
        BOTTOM_SHADE
    };
    1.0 + shade * height
}

/// What shows through the gaps [`tiles`] opens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum TilesFill {
    /// A solid straight linear RGBA.
    Color([f32; 4]),
    /// The image, inverted.
    Inverse,
    /// The image as it was.
    Unaltered,
}

impl Default for TilesFill {
    fn default() -> Self {
        TilesFill::Color([1.0, 1.0, 1.0, 1.0])
    }
}

/// Tiles: cut the image into a grid of square tiles and shift each a little.
///
/// * `count` — tiles across the shorter side; clamped to `1..=MAX_EXTRUDE_SIZE`.
/// * `max_offset_pct` — the furthest a tile moves, as a percentage of its
///   side. Every tile moves at least one pixel when this is above zero, so a
///   small preview shows the effect the full image will have.
/// * `fill` — see [`TilesFill`].
/// * `seed` — the per-tile shifts are a pure function of it.
///
/// Tiles are drawn in row-major order and a later tile covers an earlier one
/// where two overlap.
pub fn tiles(
    src: &FilterBuffer,
    count: u32,
    max_offset_pct: f32,
    fill: TilesFill,
    seed: u64,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let (w, h) = src.dimensions();
    let count = count.clamp(1, MAX_EXTRUDE_SIZE);
    let side = w.min(h).div_ceil(count).max(1) as i64;
    let pct = if max_offset_pct.is_finite() {
        max_offset_pct.clamp(0.0, 100.0)
    } else {
        0.0
    };
    let (tiles_x, tiles_y) = (
        w.div_ceil(side as u32) as i64,
        h.div_ceil(side as u32) as i64,
    );
    let offset_of = |tx: i64, ty: i64| -> (i64, i64) {
        if pct <= 0.0 {
            return (0, 0);
        }
        let reach = pct / 100.0 * side as f32;
        let ax = (hash_unit(seed, tx, ty) * 2.0 - 1.0) * reach;
        let ay = (hash_unit(seed ^ 0x5bd1_e995, tx, ty) * 2.0 - 1.0) * reach;
        let px = ax.round() as i64;
        let py = ay.round() as i64;
        let px = if px == 0 { ax.signum() as i64 } else { px };
        let py = if py == 0 { ay.signum() as i64 } else { py };
        (px, py)
    };
    let solid = match fill {
        TilesFill::Color(c) => Some(premultiply(c)),
        _ => None,
    };
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (xi, yi) = (x as i64, y as i64);
        let (home_x, home_y) = (xi / side, yi / side);
        let mut hit: Option<[f32; 4]> = None;
        // A tile moves at most one side length, so only the home tile and
        // its eight neighbours can cover this pixel. Later tiles win.
        for ty in (home_y - 1)..=(home_y + 1) {
            if ty < 0 || ty >= tiles_y {
                continue;
            }
            for tx in (home_x - 1)..=(home_x + 1) {
                if tx < 0 || tx >= tiles_x {
                    continue;
                }
                let (ox, oy) = offset_of(tx, ty);
                let sx = xi - ox;
                let sy = yi - oy;
                let in_tile = sx / side == tx
                    && sy / side == ty
                    && sx >= 0
                    && sy >= 0
                    && sx < w as i64
                    && sy < h as i64;
                if in_tile {
                    hit = Some(src.get(sx as u32, sy as u32));
                }
            }
        }
        if let Some(px) = hit {
            return px;
        }
        match fill {
            TilesFill::Color(_) => solid.unwrap_or([0.0; 4]),
            TilesFill::Inverse => {
                let s = unpremultiply(src.get(x, y));
                premultiply([
                    1.0 - s[0].clamp(0.0, 1.0),
                    1.0 - s[1].clamp(0.0, 1.0),
                    1.0 - s[2].clamp(0.0, 1.0),
                    s[3],
                ])
            }
            TilesFill::Unaltered => src.get(x, y),
        }
    });
    out
}

/// Which side of the level [`trace_contour`] outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ContourEdge {
    /// Outline the pixels at or above the level that touch one below it.
    #[default]
    Lower,
    /// Outline the pixels below the level that touch one at or above it.
    Upper,
}

/// Trace Contour: a thin outline, per channel, where the channel crosses
/// `level`.
///
/// Every channel of every pixel becomes one (white) unless the pixel is on
/// the traced side of the level and one of its four neighbours is on the
/// other side, in which case that channel becomes zero. On a colour image the
/// three channels trace three contours in three colours; a flat image has no
/// crossing and comes out white. `level` is on the encoded `0..=1` scale and
/// is clamped there. Alpha is passed through and the result premultiplied.
pub fn trace_contour(
    src: &FilterBuffer,
    level: f32,
    side: ContourEdge,
    edge: EdgeMode,
) -> FilterBuffer {
    if src.is_empty() {
        return src.clone();
    }
    let level = if level.is_finite() {
        level.clamp(0.0, 1.0)
    } else {
        0.5
    };
    let (w, h) = src.dimensions();
    let encoded: Vec<[f32; 3]> = src
        .pixels()
        .iter()
        .map(|p| {
            let s = unpremultiply(*p);
            [
                linear_to_srgb(s[0].clamp(0.0, 1.0)),
                linear_to_srgb(s[1].clamp(0.0, 1.0)),
                linear_to_srgb(s[2].clamp(0.0, 1.0)),
            ]
        })
        .collect();
    let sw = w as usize;
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let alpha = src.get(x, y)[3];
        let here = encoded[y as usize * sw + x as usize];
        let mut straight = [1.0f32, 1.0, 1.0, alpha];
        for c in 0..3 {
            let above = here[c] >= level;
            let traced = match side {
                ContourEdge::Lower => above,
                ContourEdge::Upper => !above,
            };
            if !traced {
                continue;
            }
            let crosses = [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)]
                .iter()
                .any(
                    |(ox, oy)| match (edge.map(x as i64 + ox, w), edge.map(y as i64 + oy, h)) {
                        (Some(sx), Some(sy)) => (encoded[sy * sw + sx][c] >= level) != above,
                        _ => false,
                    },
                );
            if crosses {
                straight[c] = 0.0;
            }
        }
        premultiply(straight)
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gradient(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let t = x as f32 / (w - 1) as f32;
                let s = y as f32 / (h - 1) as f32;
                px.push([t, 0.5 * (t + s), s, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    #[test]
    fn extrude_is_not_the_identity_on_a_gradient_and_shades_its_faces() {
        let src = gradient(32, 32);
        for kind in [ExtrudeType::Blocks, ExtrudeType::Pyramids] {
            for mode in [ExtrudeDepth::Random, ExtrudeDepth::LevelBased] {
                let out = extrude(&src, kind, 8, 60.0, mode, 3);
                assert_eq!(out.dimensions(), (32, 32));
                assert_ne!(out.pixels(), src.pixels(), "{kind:?}/{mode:?}");
                // Within a cell the faces differ: the lit upper-left corner
                // is brighter than the shaded lower-right one wherever the
                // cell has any height, and never darker.
                let mut lit = 0;
                for cy in 0..4u32 {
                    for cx in 0..4u32 {
                        let ul = out.get(cx * 8, cy * 8);
                        let lr = out.get(cx * 8 + 7, cy * 8 + 7);
                        assert!(ul[0] >= lr[0], "{kind:?}/{mode:?}: {ul:?} vs {lr:?}");
                        if ul[0] > lr[0] {
                            lit += 1;
                        }
                    }
                }
                assert!(
                    lit >= 8,
                    "{kind:?}/{mode:?}: only {lit} of 16 cells are shaded"
                );
                for p in out.pixels() {
                    assert!(p[0] <= p[3] + 1e-6 && p[0] >= 0.0, "{kind:?}: {p:?}");
                }
            }
        }
        // Depth zero is a plain mosaic of the cells.
        let flat_cells = extrude(&src, ExtrudeType::Blocks, 8, 0.0, ExtrudeDepth::Random, 3);
        assert_eq!(
            flat_cells.pixels(),
            crate::pixelate::mosaic(&src, 8).pixels()
        );
        // The seed only matters for random depth.
        let a = extrude(&src, ExtrudeType::Blocks, 8, 60.0, ExtrudeDepth::Random, 1);
        let b = extrude(&src, ExtrudeType::Blocks, 8, 60.0, ExtrudeDepth::Random, 2);
        assert_ne!(a.pixels(), b.pixels());
        let a = extrude(
            &src,
            ExtrudeType::Blocks,
            8,
            60.0,
            ExtrudeDepth::LevelBased,
            1,
        );
        let b = extrude(
            &src,
            ExtrudeType::Blocks,
            8,
            60.0,
            ExtrudeDepth::LevelBased,
            2,
        );
        assert_eq!(a.pixels(), b.pixels());
    }

    #[test]
    fn tiles_shift_every_tile_and_show_the_fill_in_the_gaps() {
        let src = gradient(40, 40);
        let red = TilesFill::Color([1.0, 0.0, 0.0, 1.0]);
        let out = tiles(&src, 4, 20.0, red, 7);
        assert_ne!(out.pixels(), src.pixels());
        let gaps = out
            .pixels()
            .iter()
            .filter(|p| **p == [1.0, 0.0, 0.0, 1.0])
            .count();
        assert!(gaps > 0, "no gap shows the fill colour");
        // Every non-gap pixel is a source pixel: tiles copy, never blend.
        for p in out.pixels().iter().filter(|p| **p != [1.0, 0.0, 0.0, 1.0]) {
            assert!(src.pixels().contains(p), "{p:?} was not in the source");
        }
        // The other two fills change exactly the gap pixels.
        let unaltered = tiles(&src, 4, 20.0, TilesFill::Unaltered, 7);
        let inverse = tiles(&src, 4, 20.0, TilesFill::Inverse, 7);
        for (i, p) in out.pixels().iter().enumerate() {
            if *p == [1.0, 0.0, 0.0, 1.0] {
                assert_eq!(unaltered.pixels()[i], src.pixels()[i]);
                let s = src.pixels()[i];
                let inv = inverse.pixels()[i];
                assert!((inv[0] - (1.0 - s[0])).abs() < 1e-6);
            } else {
                assert_eq!(unaltered.pixels()[i], *p);
                assert_eq!(inverse.pixels()[i], *p);
            }
        }
        // Zero offset lays every tile back where it was.
        assert_eq!(tiles(&src, 4, 0.0, red, 7).pixels(), src.pixels());
        // Determinism, and the seed's say.
        assert_eq!(tiles(&src, 4, 20.0, red, 7).pixels(), out.pixels());
        assert_ne!(tiles(&src, 4, 20.0, red, 8).pixels(), out.pixels());
    }

    #[test]
    fn trace_contour_outlines_a_step_once_per_channel_and_leaves_flat_white() {
        // Left half dark, right half light: one contour column per channel.
        let mut step = FilterBuffer::filled(20, 6, [0.05, 0.05, 0.05, 1.0]).unwrap();
        for y in 0..6 {
            for x in 10..20 {
                step.set(x, y, [0.8, 0.8, 0.8, 1.0]);
            }
        }
        let lower = trace_contour(&step, 0.5, ContourEdge::Lower, EdgeMode::Clamp);
        let upper = trace_contour(&step, 0.5, ContourEdge::Upper, EdgeMode::Clamp);
        for y in 0..6u32 {
            for x in 0..20u32 {
                let expect_lower = x == 10;
                let expect_upper = x == 9;
                let l = lower.get(x, y);
                let u = upper.get(x, y);
                assert_eq!(
                    l,
                    if expect_lower {
                        [0.0, 0.0, 0.0, 1.0]
                    } else {
                        [1.0; 4]
                    }
                );
                assert_eq!(
                    u,
                    if expect_upper {
                        [0.0, 0.0, 0.0, 1.0]
                    } else {
                        [1.0; 4]
                    }
                );
            }
        }
        let flat = FilterBuffer::filled(8, 8, [0.3, 0.4, 0.5, 1.0]).unwrap();
        for p in trace_contour(&flat, 0.5, ContourEdge::Lower, EdgeMode::Wrap).pixels() {
            assert_eq!(*p, [1.0; 4]);
        }
        let src = gradient(24, 24);
        let out = trace_contour(&src, 0.5, ContourEdge::Lower, EdgeMode::Clamp);
        assert_ne!(out.pixels(), src.pixels());
        assert!(out.pixels().iter().any(|p| p[0] == 0.0 || p[2] == 0.0));
        // Alpha rides through.
        let faint = FilterBuffer::filled(4, 4, [0.1, 0.1, 0.1, 0.5]).unwrap();
        for p in trace_contour(&faint, 0.5, ContourEdge::Lower, EdgeMode::Clamp).pixels() {
            assert_eq!(*p, [0.5, 0.5, 0.5, 0.5]);
        }
    }

    #[test]
    fn degenerate_buffers_and_parameters_do_not_panic() {
        let empty = FilterBuffer::transparent(0, 0).unwrap();
        let one = FilterBuffer::filled(1, 1, [0.4, 0.3, 0.2, 1.0]).unwrap();
        for src in [&empty, &one] {
            let _ = extrude(
                src,
                ExtrudeType::Pyramids,
                0,
                f32::NAN,
                ExtrudeDepth::LevelBased,
                0,
            );
            let _ = extrude(
                src,
                ExtrudeType::Blocks,
                9999,
                500.0,
                ExtrudeDepth::Random,
                0,
            );
            let _ = tiles(src, 0, f32::INFINITY, TilesFill::Inverse, 0);
            let _ = tiles(src, 9999, 100.0, TilesFill::default(), 0);
            let _ = trace_contour(src, f32::NAN, ContourEdge::Upper, EdgeMode::Mirror);
        }
    }
}
