//! Boundary handling, sample interpolation, and the tile-parallel iteration
//! every filter in this crate is built on.
//!
//! Two things live here because getting them wrong is invisible until it
//! shows up as a dark border or a seam:
//!
//! * [`EdgeMode`] is the *only* way a filter reads outside its buffer. No
//!   filter indexes a neighbour directly.
//! * `fill_tiles` / `fill_rows` are the only parallel drivers. They walk
//!   the destination in `raster::TILE_SIZE`-aligned blocks so filter output is
//!   produced in the same order the tile store consumes it.

use raster::TILE_SIZE;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// How a filter reads a pixel outside the buffer's bounds.
///
/// Every filter documents which mode it defaults to and whether the caller can
/// choose. A filter that silently treated out-of-bounds as transparent black
/// would darken every border, so no such mode exists here: the compositor
/// decides what lies outside a layer, not the filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum EdgeMode {
    /// Repeat the nearest edge pixel outwards. The default: it is the only
    /// mode under which a blur of a constant image is exactly that constant
    /// for *any* image size, including 1x1.
    #[default]
    Clamp,
    /// Tile the image, so `-1` reads the last column. Correct for
    /// seamless-texture work and for [`crate::other::offset`].
    Wrap,
    /// Reflect about the boundary with the edge pixel repeated, i.e. period
    /// `2 * n`: for `n = 4` the sequence around 0 is `.. 2 1 0 | 0 1 2 3 |
    /// 3 2 1 ..`. Repeating the edge (rather than the period `2n - 2` variant)
    /// keeps the mapping total for `n == 1`.
    Mirror,
}

impl EdgeMode {
    /// Map a possibly out-of-range coordinate into `0..n`.
    ///
    /// Returns `None` only when `n == 0`, i.e. the buffer has no pixels in
    /// that axis at all; there is no sensible pixel to name and callers skip
    /// the work entirely.
    #[inline]
    pub fn map(self, i: i64, n: u32) -> Option<usize> {
        if n == 0 {
            return None;
        }
        let n64 = n as i64;
        let m = match self {
            EdgeMode::Clamp => i.clamp(0, n64 - 1),
            EdgeMode::Wrap => i.rem_euclid(n64),
            EdgeMode::Mirror => {
                let p = i.rem_euclid(2 * n64);
                if p < n64 {
                    p
                } else {
                    2 * n64 - 1 - p
                }
            }
        };
        Some(m as usize)
    }
}

/// Reconstruction filter used when a distort or blur samples between pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Interpolation {
    /// Nearest neighbour. Aliases badly; useful for pixel-art workflows.
    Nearest,
    /// Bilinear. The default for every resampling filter — never leaves holes
    /// and never overshoots, so premultiplied values stay valid.
    #[default]
    Bilinear,
    /// Catmull-Rom bicubic. Sharper than bilinear at the cost of ringing:
    /// weights are negative on the outer taps, so results can overshoot the
    /// input range. [`crate::FilterBuffer`] re-clamps alpha after sampling.
    Bicubic,
}

/// The pair of choices every resampling filter needs.
///
/// Bundled so a filter signature stays readable and so presets can be
/// serialised as one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Sampling {
    pub edge: EdgeMode,
    pub interp: Interpolation,
}

impl Sampling {
    pub const fn new(edge: EdgeMode, interp: Interpolation) -> Self {
        Self { edge, interp }
    }

    /// Clamp + bilinear: the default every distort filter uses unless told
    /// otherwise.
    pub const fn clamped() -> Self {
        Self::new(EdgeMode::Clamp, Interpolation::Bilinear)
    }
}

/// Fill `out` (a `width * height` pixel plane) by evaluating `f` at every
/// destination pixel, in tile-aligned blocks, in parallel.
///
/// Parallelism is over *tile rows* — bands of [`TILE_SIZE`] scanlines — and
/// within a band the traversal is tile-by-tile, so a filter's memory access
/// pattern matches the tile store's layout. `f` is a pure function of the
/// destination coordinate, which is what makes the decomposition safe: nothing
/// a filter writes is ever read back by another block.
pub(crate) fn fill_tiles<F>(width: u32, height: u32, out: &mut [[f32; 4]], f: F)
where
    F: Fn(u32, u32) -> [f32; 4] + Sync,
{
    if width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let band = TILE_SIZE as usize;
    let tiles_x = width.div_ceil(TILE_SIZE);
    out.par_chunks_mut(band * w)
        .enumerate()
        .for_each(|(bi, rows)| {
            let y0 = (bi * band) as u32;
            let rows_here = rows.len() / w;
            for tx in 0..tiles_x {
                let x0 = tx * TILE_SIZE;
                let x1 = (x0 + TILE_SIZE).min(width);
                for ly in 0..rows_here {
                    let y = y0 + ly as u32;
                    let row = &mut rows[ly * w..ly * w + w];
                    for (x, slot) in row
                        .iter_mut()
                        .enumerate()
                        .take(x1 as usize)
                        .skip(x0 as usize)
                    {
                        *slot = f(x as u32, y);
                    }
                }
            }
        });
}

/// Fill `out` one whole scanline at a time, in parallel over tile-row bands.
///
/// Used by the separable passes, where a sliding window carries state along
/// the row and a per-pixel closure would throw that state away.
pub(crate) fn fill_rows<F>(width: u32, height: u32, out: &mut [[f32; 4]], f: F)
where
    F: Fn(u32, &mut [[f32; 4]]) + Sync,
{
    if width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let band = TILE_SIZE as usize;
    out.par_chunks_mut(band * w)
        .enumerate()
        .for_each(|(bi, rows)| {
            let y0 = (bi * band) as u32;
            for (ly, row) in rows.chunks_mut(w).enumerate() {
                f(y0 + ly as u32, row);
            }
        });
}

/// Fill a single-channel plane a whole tile-row band at a time, in parallel.
///
/// The closure receives the band's first row index and every row in it, so a
/// filter can carry state down the band — which is what the sliding-window
/// median needs: rebuilding its column histograms per row would throw away the
/// entire point of the algorithm.
pub(crate) fn fill_bands<T, F>(width: u32, height: u32, out: &mut [T], f: F)
where
    T: Send,
    F: Fn(u32, &mut [T]) + Sync,
{
    if width == 0 || height == 0 {
        return;
    }
    let w = width as usize;
    let band = TILE_SIZE as usize;
    out.par_chunks_mut(band * w)
        .enumerate()
        .for_each(|(bi, rows)| f((bi * band) as u32, rows));
}

/// `acc += px * weight`, the inner step of every weighted-average filter.
#[inline]
pub(crate) fn accumulate(acc: &mut [f32; 4], px: [f32; 4], weight: f32) {
    for (a, v) in acc.iter_mut().zip(px.iter()) {
        *a += v * weight;
    }
}

/// Multiply every channel by `s`.
#[inline]
pub(crate) fn scale(px: [f32; 4], s: f32) -> [f32; 4] {
    [px[0] * s, px[1] * s, px[2] * s, px[3] * s]
}

/// Per-channel difference, `a - b`.
#[inline]
pub(crate) fn sub(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2], a[3] - b[3]]
}

/// Linear interpolation between two pixels.
#[inline]
pub(crate) fn lerp_px(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

/// Largest absolute per-channel difference between two pixels. Used as the
/// "are these two pixels similar?" test by the edge-preserving filters.
#[inline]
pub(crate) fn max_abs_diff(a: [f32; 4], b: [f32; 4]) -> f32 {
    let mut m = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        m = m.max((x - y).abs());
    }
    m
}

/// Smoothstep, used by several filters to avoid hard thresholds.
#[inline]
pub(crate) fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    if edge1 <= edge0 {
        return if x < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_repeats_the_edge() {
        assert_eq!(EdgeMode::Clamp.map(-5, 4), Some(0));
        assert_eq!(EdgeMode::Clamp.map(9, 4), Some(3));
        assert_eq!(EdgeMode::Clamp.map(2, 4), Some(2));
    }

    #[test]
    fn wrap_is_periodic_in_both_directions() {
        assert_eq!(EdgeMode::Wrap.map(-1, 4), Some(3));
        assert_eq!(EdgeMode::Wrap.map(-4, 4), Some(0));
        assert_eq!(EdgeMode::Wrap.map(-5, 4), Some(3));
        assert_eq!(EdgeMode::Wrap.map(4, 4), Some(0));
        assert_eq!(EdgeMode::Wrap.map(7, 4), Some(3));
    }

    /// The documented sequence, spelled out. A period-`2n-2` mirror would give
    /// `map(-1) == 1`, not `0`, and would divide by zero for `n == 1`.
    #[test]
    fn mirror_repeats_the_edge_pixel() {
        let got: Vec<_> = (-4..8)
            .map(|i| EdgeMode::Mirror.map(i, 4).unwrap())
            .collect();
        assert_eq!(got, vec![3, 2, 1, 0, 0, 1, 2, 3, 3, 2, 1, 0]);
    }

    #[test]
    fn every_mode_is_total_for_a_single_pixel_axis() {
        for mode in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            for i in -3..4 {
                assert_eq!(mode.map(i, 1), Some(0), "{mode:?} at {i}");
            }
        }
    }

    #[test]
    fn a_zero_length_axis_has_no_pixel() {
        for mode in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            assert_eq!(mode.map(0, 0), None);
        }
    }

    /// Extreme coordinates arise from `offset` with a huge shift; `rem_euclid`
    /// on `i64::MIN` must not overflow into a panic.
    #[test]
    fn extreme_coordinates_do_not_panic() {
        for mode in [EdgeMode::Clamp, EdgeMode::Wrap, EdgeMode::Mirror] {
            assert!(mode.map(i64::MIN / 2, 7).is_some());
            assert!(mode.map(i64::MAX / 2, 7).is_some());
        }
    }

    #[test]
    fn fill_tiles_visits_every_pixel_exactly_once() {
        // Larger than one tile in both axes, and not a multiple of TILE_SIZE.
        let (w, h) = (TILE_SIZE + 37, TILE_SIZE + 5);
        let mut out = vec![[0.0f32; 4]; (w * h) as usize];
        fill_tiles(w, h, &mut out, |x, y| [x as f32, y as f32, 1.0, 1.0]);
        for y in 0..h {
            for x in 0..w {
                let px = out[(y * w + x) as usize];
                assert_eq!(px, [x as f32, y as f32, 1.0, 1.0], "at {x},{y}");
            }
        }
    }

    #[test]
    fn fill_rows_visits_every_row_with_the_right_index() {
        let (w, h) = (9u32, TILE_SIZE * 2 + 3);
        let mut out = vec![[0.0f32; 4]; (w * h) as usize];
        fill_rows(w, h, &mut out, |y, row| {
            for (x, slot) in row.iter_mut().enumerate() {
                *slot = [x as f32, y as f32, 0.0, 1.0];
            }
        });
        for y in 0..h {
            assert_eq!(out[(y * w + 4) as usize], [4.0, y as f32, 0.0, 1.0]);
        }
    }

    #[test]
    fn fill_helpers_tolerate_empty_planes() {
        let mut out: Vec<[f32; 4]> = Vec::new();
        fill_tiles(0, 5, &mut out, |_, _| [1.0; 4]);
        fill_rows(5, 0, &mut out, |_, _| unreachable!());
        let mut scalar: Vec<f32> = Vec::new();
        fill_bands(0, 0, &mut scalar, |_, _: &mut [f32]| unreachable!());
        assert!(out.is_empty() && scalar.is_empty());
    }

    #[test]
    fn smoothstep_is_clamped_and_monotonic() {
        assert_eq!(smoothstep(0.0, 1.0, -1.0), 0.0);
        assert_eq!(smoothstep(0.0, 1.0, 2.0), 1.0);
        assert!((smoothstep(0.0, 1.0, 0.5) - 0.5).abs() < 1e-6);
        // A degenerate range is a step, not a division by zero.
        assert_eq!(smoothstep(1.0, 1.0, 0.5), 0.0);
        assert_eq!(smoothstep(1.0, 1.0, 1.5), 1.0);
    }
}
