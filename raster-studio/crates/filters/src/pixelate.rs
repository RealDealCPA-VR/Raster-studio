//! Pixelate: filters that replace detail with a coarser structure.
//!
//! [`mosaic`], [`crystallize`] and [`pointillize`] average premultiplied
//! linear pixels, so a cell's colour is the correct composite of what it
//! covers and a constant image survives all three exactly.
//!
//! [`color_halftone`] is the exception, and says so in its own documentation:
//! screening is an ink-coverage model defined on **gamma-encoded** values.
//!
//! Cell layouts are anchored to the buffer origin and are fully determined by
//! the cell size and the seed, so the same call always produces the same
//! image.

use color::{linear_to_srgb, premultiply, srgb_to_linear, unpremultiply};

use crate::buffer::FilterBuffer;
use crate::rng::hash_unit;
use crate::support::{fill_bands, fill_tiles};

/// Largest cell size accepted by the pixelate filters.
pub const MAX_CELL_SIZE: u32 = 4096;

/// Mosaic: replace each `cell_size` square with its average colour.
///
/// The average is over premultiplied linear pixels, which is the only average
/// that composites correctly: averaging straight-alpha colour would pull the
/// colour of fully transparent pixels into the cell.
///
/// Cells are anchored at the buffer origin; the last row and column of cells
/// are partial when the size does not divide evenly, and average only the
/// pixels they actually cover. There is no boundary sampling at all, so no
/// [`crate::EdgeMode`] applies.
///
/// A cell size of zero or one is the identity.
pub fn mosaic(src: &FilterBuffer, cell_size: u32) -> FilterBuffer {
    if src.is_empty() || cell_size <= 1 {
        return src.clone();
    }
    let cell = cell_size.min(MAX_CELL_SIZE);
    let (w, h) = src.dimensions();
    let (cx, cy) = (w.div_ceil(cell), h.div_ceil(cell));
    let mut sums = vec![[0.0f64; 4]; (cx as usize) * (cy as usize)];
    let mut counts = vec![0u32; sums.len()];
    for y in 0..h {
        for x in 0..w {
            let idx = (y / cell) as usize * cx as usize + (x / cell) as usize;
            let p = src.get(x, y);
            for (acc, v) in sums[idx].iter_mut().zip(p.iter()) {
                *acc += *v as f64;
            }
            counts[idx] += 1;
        }
    }
    let averages: Vec<[f32; 4]> = sums
        .iter()
        .zip(counts.iter())
        .map(|(s, &n)| {
            if n == 0 {
                [0.0; 4]
            } else {
                let k = 1.0 / n as f64;
                [
                    (s[0] * k) as f32,
                    (s[1] * k) as f32,
                    (s[2] * k) as f32,
                    (s[3] * k) as f32,
                ]
            }
        })
        .collect();
    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        averages[(y / cell) as usize * cx as usize + (x / cell) as usize]
    });
    out
}

/// One jittered site per cell, shared by [`crystallize`] and [`pointillize`].
struct SiteGrid {
    cells_x: u32,
    cells_y: u32,
    /// Site positions in pixel coordinates, row-major by cell.
    sites: Vec<(f32, f32)>,
}

impl SiteGrid {
    fn new(width: u32, height: u32, cell: u32, seed: u64) -> Self {
        let cells_x = width.div_ceil(cell).max(1);
        let cells_y = height.div_ceil(cell).max(1);
        let mut sites = Vec::with_capacity((cells_x * cells_y) as usize);
        for j in 0..cells_y {
            for i in 0..cells_x {
                let jx = hash_unit(seed, i as i64, j as i64);
                let jy = hash_unit(seed ^ 0x1234_5678_9ABC_DEF0, i as i64, j as i64);
                sites.push(((i as f32 + jx) * cell as f32, (j as f32 + jy) * cell as f32));
            }
        }
        Self {
            cells_x,
            cells_y,
            sites,
        }
    }

    /// Index of the site nearest `(x, y)`.
    ///
    /// Searching the 5x5 block of cells around the point is provably enough,
    /// and 3x3 is not. A site never leaves its own cell, so the site of the
    /// point's own cell is at most `sqrt(2) * cell` away; a site `k` cells
    /// away is at least `(k - 1) * cell` away, which beats `sqrt(2) * cell`
    /// only for `k < 2.42`. So `k = 2` can win and `k >= 3` cannot.
    fn nearest(&self, x: f32, y: f32, cell: u32) -> usize {
        let ci = (x / cell as f32) as i64;
        let cj = (y / cell as f32) as i64;
        let mut best = 0usize;
        let mut best_d = f32::INFINITY;
        for j in (cj - 2).max(0)..=(cj + 2).min(self.cells_y as i64 - 1) {
            for i in (ci - 2).max(0)..=(ci + 2).min(self.cells_x as i64 - 1) {
                let idx = j as usize * self.cells_x as usize + i as usize;
                let (sx, sy) = self.sites[idx];
                let d = (sx - x) * (sx - x) + (sy - y) * (sy - y);
                if d < best_d {
                    best_d = d;
                    best = idx;
                }
            }
        }
        best
    }
}

/// Crystallize: partition the image into Voronoi cells around jittered sites
/// and flood each cell with the average colour of the pixels inside it.
///
/// Unlike [`mosaic`] the cells are irregular polygons, which is what gives the
/// characteristic crystal facets. Because each cell is filled with the mean of
/// its own members, a constant image is returned unchanged.
///
/// The site layout is a pure function of `(seed, cell index)`, so the result is
/// reproducible. No pixel outside the buffer is ever read, so no [`crate::EdgeMode`]
/// applies. A cell size of zero or one is the identity.
pub fn crystallize(src: &FilterBuffer, cell_size: u32, seed: u64) -> FilterBuffer {
    if src.is_empty() || cell_size <= 1 {
        return src.clone();
    }
    let cell = cell_size.min(MAX_CELL_SIZE);
    let (w, h) = src.dimensions();
    let grid = SiteGrid::new(w, h, cell, seed);

    // Assignment is a pure function of position, so it parallelises; the
    // accumulation that follows is a single cheap sequential pass.
    let mut assign = vec![0u32; src.len()];
    fill_bands(w, h, &mut assign, |y0, band| {
        for (ly, row) in band.chunks_mut(w as usize).enumerate() {
            let y = (y0 + ly as u32) as f32 + 0.5;
            for (x, slot) in row.iter_mut().enumerate() {
                *slot = grid.nearest(x as f32 + 0.5, y, cell) as u32;
            }
        }
    });

    let mut sums = vec![[0.0f64; 4]; grid.sites.len()];
    let mut counts = vec![0u32; grid.sites.len()];
    for (i, px) in src.pixels().iter().enumerate() {
        let s = assign[i] as usize;
        for (acc, v) in sums[s].iter_mut().zip(px.iter()) {
            *acc += *v as f64;
        }
        counts[s] += 1;
    }
    let averages: Vec<[f32; 4]> = sums
        .iter()
        .zip(counts.iter())
        .map(|(s, &n)| {
            if n == 0 {
                [0.0; 4]
            } else {
                let k = 1.0 / n as f64;
                [
                    (s[0] * k) as f32,
                    (s[1] * k) as f32,
                    (s[2] * k) as f32,
                    (s[3] * k) as f32,
                ]
            }
        })
        .collect();

    let mut out = src.same_size_blank();
    let sw = w as usize;
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        averages[assign[y as usize * sw + x as usize] as usize]
    });
    out
}

/// Pointillize: scatter one dot per cell, coloured by the source under its
/// centre, over a flat background.
///
/// Dot radii vary between 35% and 85% of the cell size, driven by the seed, so
/// the coverage looks hand-stippled rather than gridded. Where two dots
/// overlap the nearer centre wins.
///
/// `background` is a premultiplied linear pixel — pass `[0.0; 4]` for
/// transparent, or the document's paper colour. A constant image over a
/// background of that same constant comes back unchanged, which is the
/// invariance test this filter admits.
///
/// A cell size of zero or one is the identity.
pub fn pointillize(
    src: &FilterBuffer,
    cell_size: u32,
    seed: u64,
    background: [f32; 4],
) -> FilterBuffer {
    if src.is_empty() || cell_size <= 1 {
        return src.clone();
    }
    let cell = cell_size.min(MAX_CELL_SIZE);
    let (w, h) = src.dimensions();
    let grid = SiteGrid::new(w, h, cell, seed);
    // Dot colours are sampled once, at each site, rather than per pixel.
    let colours: Vec<[f32; 4]> = grid
        .sites
        .iter()
        .map(|(sx, sy)| {
            let px = (*sx as i64).clamp(0, w as i64 - 1) as u32;
            let py = (*sy as i64).clamp(0, h as i64 - 1) as u32;
            src.get(px, py)
        })
        .collect();
    let radii: Vec<f32> = (0..grid.sites.len())
        .map(|i| {
            let j = hash_unit(seed ^ 0x0F1E_2D3C_4B5A_6978, i as i64, 0);
            cell as f32 * 0.5 * (0.35 + 0.5 * j)
        })
        .collect();

    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let idx = grid.nearest(fx, fy, cell);
        let (sx, sy) = grid.sites[idx];
        let d2 = (sx - fx) * (sx - fx) + (sy - fy) * (sy - fy);
        if d2 <= radii[idx] * radii[idx] {
            colours[idx]
        } else {
            background
        }
    });
    out
}

/// Colour halftone: re-screen each colour channel as a grid of ink dots.
///
/// **Defined on gamma-encoded values.** Halftoning models ink coverage, and
/// coverage is proportional to the *encoded* tone a printer is asked for, not
/// to linear light. Each channel is encoded to sRGB, screened, and decoded
/// again.
///
/// Each channel gets its own screen rotated by its own angle — that rotation
/// is what stops the three grids beating against each other into a moiré. Dot
/// radius is `max_radius * sqrt(1 - value)`, so dot *area* is proportional to
/// ink coverage: a white area gets no dot at all and a black one gets the
/// largest the screen allows.
///
/// Alpha is passed through untouched. Screening alpha would shred the layer's
/// silhouette into dots, which is never what the filter is for.
///
/// A non-positive `max_radius` is the identity. No pixel outside the buffer is
/// read — screen cells whose centre falls outside are clamped — so no
/// [`crate::EdgeMode`] applies.
pub fn color_halftone(src: &FilterBuffer, max_radius: f32, angles_deg: [f32; 3]) -> FilterBuffer {
    if src.is_empty() || !max_radius.is_finite() || max_radius <= 0.0 {
        return src.clone();
    }
    let r_max = max_radius.min(512.0);
    // Neighbouring dots touch at full coverage.
    let spacing = r_max * 2.0;
    let (w, h) = src.dimensions();
    let screens: Vec<(f32, f32)> = angles_deg
        .iter()
        .map(|a| {
            let t = if a.is_finite() { a.to_radians() } else { 0.0 };
            t.sin_cos()
        })
        .collect();

    let mut out = src.same_size_blank();
    fill_tiles(w, h, out.pixels_mut(), |x, y| {
        let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
        let alpha = src.get(x, y)[3];
        let mut encoded = [0.0f32; 3];
        for (c, &(sin_t, cos_t)) in screens.iter().enumerate() {
            // Rotate into the screen's frame.
            let u = fx * cos_t + fy * sin_t;
            let v = -fx * sin_t + fy * cos_t;
            let (ci, cj) = ((u / spacing).round(), (v / spacing).round());
            let mut inked = false;
            for dj in -1i32..=1 {
                for di in -1i32..=1 {
                    let (cu, cv) = ((ci + di as f32) * spacing, (cj + dj as f32) * spacing);
                    // Back to image space to read the cell's tone.
                    let px = cu * cos_t - cv * sin_t;
                    let py = cu * sin_t + cv * cos_t;
                    let sx = (px as i64).clamp(0, w as i64 - 1) as u32;
                    let sy = (py as i64).clamp(0, h as i64 - 1) as u32;
                    let tone = linear_to_srgb(unpremultiply(src.get(sx, sy))[c].clamp(0.0, 1.0));
                    // Ink covers the *complement* of the tone.
                    let radius = r_max * (1.0 - tone).max(0.0).sqrt();
                    let (ddu, ddv) = (u - cu, v - cv);
                    // Strictly inside: a zero radius must never ink, even
                    // for a pixel that lands exactly on a screen centre.
                    if ddu * ddu + ddv * ddv < radius * radius {
                        inked = true;
                    }
                }
            }
            encoded[c] = if inked { 0.0 } else { 1.0 };
        }
        premultiply([
            srgb_to_linear(encoded[0]),
            srgb_to_linear(encoded[1]),
            srgb_to_linear(encoded[2]),
            alpha,
        ])
    });
    out
}

/// Mean straight-alpha luminance of a buffer, used by the halftone tests to
/// check monotonicity.
#[cfg(test)]
fn mean_channel(buf: &FilterBuffer, c: usize) -> f64 {
    if buf.is_empty() {
        return 0.0;
    }
    buf.pixels()
        .iter()
        .map(|p| unpremultiply(*p)[c] as f64)
        .sum::<f64>()
        / buf.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONST_PX: [f32; 4] = [0.21, 0.34, 0.55, 0.8];

    fn constant(w: u32, h: u32) -> FilterBuffer {
        FilterBuffer::filled(w, h, CONST_PX).unwrap()
    }

    fn ramp(w: u32, h: u32) -> FilterBuffer {
        let mut px = Vec::new();
        for _ in 0..h {
            for x in 0..w {
                let v = (x as f32 / w as f32).clamp(0.0, 1.0);
                px.push([v, v * 0.7, 1.0 - v, 1.0]);
            }
        }
        FilterBuffer::from_pixels(w, h, px).unwrap()
    }

    fn assert_constant(buf: &FilterBuffer, what: &str) {
        for (i, px) in buf.pixels().iter().enumerate() {
            for c in 0..4 {
                assert!(
                    (px[c] - CONST_PX[c]).abs() < 1e-5,
                    "{what}: pixel {i} channel {c}: {px:?}"
                );
            }
        }
    }

    #[test]
    fn averaging_pixelators_preserve_a_constant_image() {
        let src = constant(37, 29);
        assert_constant(&mosaic(&src, 6), "mosaic");
        assert_constant(&crystallize(&src, 7, 3), "crystallize");
        assert_constant(&pointillize(&src, 5, 3, CONST_PX), "pointillize");
    }

    /// The partial cells along the right and bottom edges must average only
    /// the pixels they cover. Averaging a padded cell darkens the border.
    #[test]
    fn mosaic_handles_partial_edge_cells() {
        // 10 wide with cells of 4: the last cell is 2 pixels wide.
        let src = ramp(10, 4);
        let out = mosaic(&src, 4);
        let expect_last: f32 = (8..10).map(|x| src.get(x, 0)[0]).sum::<f32>() / 2.0;
        assert!(
            (out.get(9, 1)[0] - expect_last).abs() < 1e-6,
            "{} vs {expect_last}",
            out.get(9, 1)[0]
        );
        // And the full cells are the mean of their four columns.
        let expect_first: f32 = (0..4).map(|x| src.get(x, 0)[0]).sum::<f32>() / 4.0;
        assert!((out.get(2, 2)[0] - expect_first).abs() < 1e-6);
    }

    #[test]
    fn mosaic_cells_are_flat_and_the_mean_is_preserved() {
        let src = ramp(24, 16);
        let out = mosaic(&src, 8);
        // Every pixel in a cell is identical.
        for cy in 0..2u32 {
            for cx in 0..3u32 {
                let first = out.get(cx * 8, cy * 8);
                for y in 0..8 {
                    for x in 0..8 {
                        assert_eq!(out.get(cx * 8 + x, cy * 8 + y), first);
                    }
                }
            }
        }
        // Averaging cells cannot change the image mean.
        let before = mean_channel(&src, 0);
        let after = mean_channel(&out, 0);
        assert!((before - after).abs() < 1e-5, "{before} vs {after}");
    }

    #[test]
    fn crystallize_is_deterministic_and_produces_irregular_cells() {
        let src = ramp(48, 48);
        let a = crystallize(&src, 8, 21);
        let b = crystallize(&src, 8, 21);
        let c = crystallize(&src, 8, 22);
        assert_eq!(a.pixels(), b.pixels());
        assert_ne!(a.pixels(), c.pixels(), "a new seed must move the sites");
        // Not a square grid: crystallize must differ from mosaic.
        assert_ne!(a.pixels(), mosaic(&src, 8).pixels());
    }

    /// Every output pixel must be one of the cell averages — no pixel may be
    /// left unassigned.
    #[test]
    fn crystallize_assigns_every_pixel_to_a_cell() {
        let src = ramp(33, 27);
        let out = crystallize(&src, 6, 4);
        for (i, px) in out.pixels().iter().enumerate() {
            assert!(px[3] > 0.0, "unassigned pixel {i}: {px:?}");
        }
        // Cell averaging cannot change the image mean.
        let before = mean_channel(&src, 0);
        let after = mean_channel(&out, 0);
        assert!((before - after).abs() < 1e-5, "{before} vs {after}");
    }

    #[test]
    fn pointillize_paints_dots_over_the_background() {
        let src = ramp(40, 40);
        let out = pointillize(&src, 6, 8, [0.0; 4]);
        let background = out.pixels().iter().filter(|p| p[3] == 0.0).count();
        let dots = out.len() - background;
        assert!(background > 0, "no background survived");
        assert!(dots > 0, "no dots were painted");
        // Determinism.
        assert_eq!(out.pixels(), pointillize(&src, 6, 8, [0.0; 4]).pixels());
        assert_ne!(out.pixels(), pointillize(&src, 6, 9, [0.0; 4]).pixels());
    }

    /// A white image asks for no ink at all, so the screen must leave it
    /// exactly white. Any dot here means the coverage maths is inverted.
    #[test]
    fn halftone_of_white_stays_white() {
        let white = FilterBuffer::filled(64, 64, [1.0, 1.0, 1.0, 1.0]).unwrap();
        let out = color_halftone(&white, 4.0, [15.0, 75.0, 0.0]);
        for (i, px) in out.pixels().iter().enumerate() {
            for c in 0..3 {
                assert!((px[c] - 1.0).abs() < 1e-5, "pixel {i}: {px:?}");
            }
            assert_eq!(px[3], 1.0);
        }
    }

    /// A black image asks for full ink; a screen of touching circles covers
    /// about pi/4 of the plane, so most — but not all — of it must be inked.
    #[test]
    fn halftone_of_black_is_mostly_inked() {
        let black = FilterBuffer::filled(96, 96, [0.0, 0.0, 0.0, 1.0]).unwrap();
        let out = color_halftone(&black, 4.0, [15.0, 75.0, 0.0]);
        let inked = out
            .pixels()
            .iter()
            .filter(|p| linear_to_srgb(p[0]) < 0.5)
            .count();
        let coverage = inked as f64 / out.len() as f64;
        assert!(
            (0.6..0.95).contains(&coverage),
            "coverage {coverage} is not a plausible screen"
        );
    }

    /// The dot size must be driven by the **encoded** tone, which is the
    /// filter's headline claim. Encoded 0.5 is linear 0.214, so the two spaces
    /// predict dot radii of `r_max * sqrt(0.5) = 0.707 r_max` and
    /// `r_max * sqrt(0.786) = 0.887 r_max` — a 25% difference in radius and a
    /// 57% difference in area, which shows up directly as inked coverage.
    ///
    /// White and black are fixed points of both transfer curves and a
    /// monotonicity check holds under either, so this is the only test that
    /// can tell the two apart.
    #[test]
    fn halftone_dot_area_follows_the_encoded_tone_not_linear_light() {
        let mid = srgb_to_linear(0.5);
        let r_max = 8.0f32;
        // 160 is exactly ten 16-pixel screen cells on each axis, so the
        // measured coverage is the coverage of one whole cell whatever the
        // phase of the grid.
        let src = FilterBuffer::filled(160, 160, [mid, mid, mid, 1.0]).unwrap();
        let out = color_halftone(&src, r_max, [0.0, 0.0, 0.0]);
        let inked = out.pixels().iter().filter(|p| p[0] < 0.5).count();
        let coverage = inked as f64 / out.len() as f64;
        // Dots sit on a grid of pitch `2 * r_max`, so the covered fraction is
        // `pi * radius^2 / (2 * r_max)^2 = pi * (1 - tone) / 4`.
        let predict = |tone: f64| core::f64::consts::PI * (1.0 - tone) / 4.0;
        let from_encoded = predict(0.5);
        let from_linear = predict(mid as f64);
        assert!(
            (coverage - from_encoded).abs() < 0.03,
            "coverage {coverage} does not match the encoded-tone prediction {from_encoded} \
             (measured 0.375: rasterising a disc of radius 5.66 onto pixel centres \
             costs about 0.018 of coverage, and the linear-tone prediction is 0.22 away)"
        );
        assert!(
            (coverage - from_linear).abs() > 0.15,
            "coverage {coverage} matches the linear-light prediction {from_linear}: \
             the screen ran on linear values instead of encoded ones"
        );
    }

    /// Brighter input must mean less ink. This is the property that separates
    /// a halftone from a random dither.
    #[test]
    fn halftone_coverage_falls_as_the_image_brightens() {
        let mut previous = -1.0f64;
        for step in 0..5 {
            let tone = srgb_to_linear(step as f32 / 4.0);
            let src = FilterBuffer::filled(96, 96, [tone, tone, tone, 1.0]).unwrap();
            let out = color_halftone(&src, 5.0, [15.0, 75.0, 0.0]);
            let mean = mean_channel(&out, 0);
            assert!(
                mean > previous,
                "step {step}: mean {mean} did not rise above {previous}"
            );
            previous = mean;
        }
    }

    #[test]
    fn halftone_preserves_alpha_and_is_deterministic() {
        let src = FilterBuffer::filled(32, 32, [0.15, 0.1, 0.05, 0.5]).unwrap();
        let out = color_halftone(&src, 3.0, [15.0, 75.0, 0.0]);
        for px in out.pixels() {
            assert!((px[3] - 0.5).abs() < 1e-6, "{px:?}");
            for c in 0..3 {
                assert!(px[c] >= -1e-6 && px[c] <= px[3] + 1e-6, "{px:?}");
            }
        }
        assert_eq!(
            out.pixels(),
            color_halftone(&src, 3.0, [15.0, 75.0, 0.0]).pixels()
        );
    }

    /// Different screen angles are the whole point; the same angle on every
    /// channel would beat into a moiré.
    #[test]
    fn halftone_screens_are_actually_rotated_apart() {
        let grey = srgb_to_linear(0.5);
        let src = FilterBuffer::filled(64, 64, [grey, grey, grey, 1.0]).unwrap();
        let out = color_halftone(&src, 5.0, [15.0, 75.0, 0.0]);
        let mut differs = 0;
        for px in out.pixels() {
            if px[0] != px[1] || px[1] != px[2] {
                differs += 1;
            }
        }
        assert!(
            differs > out.len() / 10,
            "the three screens landed on top of each other ({differs} differing pixels)"
        );
    }

    #[test]
    fn zero_and_one_pixel_cells_are_the_identity() {
        let src = ramp(12, 9);
        assert_eq!(mosaic(&src, 0), src);
        assert_eq!(mosaic(&src, 1), src);
        assert_eq!(crystallize(&src, 0, 1), src);
        assert_eq!(crystallize(&src, 1, 1), src);
        assert_eq!(pointillize(&src, 1, 1, [0.0; 4]), src);
        assert_eq!(color_halftone(&src, 0.0, [0.0; 3]), src);
        assert_eq!(color_halftone(&src, f32::NAN, [0.0; 3]), src);
    }

    #[test]
    fn pixelators_survive_one_pixel_and_empty_images() {
        let one = constant(1, 1);
        assert_constant(&mosaic(&one, 64), "1x1 mosaic");
        assert_constant(&crystallize(&one, 64, 1), "1x1 crystallize");
        assert_constant(&pointillize(&one, 64, 1, CONST_PX), "1x1 pointillize");
        assert!(!color_halftone(&one, 8.0, [15.0, 75.0, 0.0]).is_empty());

        let empty = FilterBuffer::transparent(0, 5).unwrap();
        assert!(mosaic(&empty, 4).is_empty());
        assert!(crystallize(&empty, 4, 1).is_empty());
        assert!(pointillize(&empty, 4, 1, [0.0; 4]).is_empty());
        assert!(color_halftone(&empty, 4.0, [0.0; 3]).is_empty());
    }

    #[test]
    fn absurd_cell_sizes_are_clamped_not_fatal() {
        let src = ramp(8, 8);
        assert!(!mosaic(&src, u32::MAX).is_empty());
        assert!(!crystallize(&src, u32::MAX, 1).is_empty());
        assert!(!pointillize(&src, u32::MAX, 1, [0.0; 4]).is_empty());
        assert!(!color_halftone(&src, 1e30, [f32::NAN; 3]).is_empty());
    }

    /// A site never leaves its own cell, which is the premise of the 5x5
    /// search in [`SiteGrid::nearest`] — see the bound in its own doc. Pinned
    /// so a future jitter change cannot silently break the search.
    #[test]
    fn jittered_sites_stay_inside_their_own_cell() {
        let cell = 9u32;
        let grid = SiteGrid::new(90, 63, cell, 4242);
        for j in 0..grid.cells_y {
            for i in 0..grid.cells_x {
                let (sx, sy) = grid.sites[(j * grid.cells_x + i) as usize];
                assert!(
                    sx >= (i * cell) as f32 && sx < ((i + 1) * cell) as f32,
                    "site {i},{j} escaped in x: {sx}"
                );
                assert!(
                    sy >= (j * cell) as f32 && sy < ((j + 1) * cell) as f32,
                    "site {i},{j} escaped in y: {sy}"
                );
            }
        }
    }
}
