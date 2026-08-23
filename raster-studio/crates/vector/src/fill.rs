//! The anti-aliased scanline rasteriser.
//!
//! # How coverage is computed
//! Each pixel row is sampled by [`FillOptions::samples_per_pixel`] evenly
//! spaced sub-scanlines. On each sub-scanline the crossings of every path edge
//! are found, sorted, and turned into inside spans by the fill rule; each span
//! then contributes its **exact** horizontal overlap with each pixel it touches.
//!
//! So coverage is analytically exact horizontally and sampled vertically. That
//! asymmetry is deliberate. Exact horizontal coverage is what makes a
//! near-vertical edge — the case a text or shape rasteriser meets constantly —
//! smooth at any angle, and it costs nothing: the span endpoints are already
//! floating-point numbers. Vertical sampling is what keeps the two fill rules
//! *the same code*: an area-accumulation rasteriser can only produce a signed
//! area, which decides nonzero winding but cannot distinguish "wound twice" from
//! "wound once", so even-odd would need a second, separately-wrong scan
//! converter. One converter with a quantisation of 1/16 of a pixel is a better
//! trade than two converters that disagree.
//!
//! At the default of 16 sub-scanlines the vertical quantisation is 1/16 of a
//! pixel of coverage on horizontal-ish edges, and *zero* on the cases that
//! matter most: any edge whose crossings are symmetric about the pixel centre —
//! all axis-aligned edges and every 45-degree edge — comes out exact, which is
//! what `a_45_degree_edge_is_half_covered_at_the_boundary` and
//! `a_unit_square_covers_exactly_one_pixel` pin.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::error::VectorError;
use crate::mask::{alloc_vec, to_byte, CoverageMask, PixelRect};
use crate::path::{Path, Polyline};

/// Which points a path encloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FillRule {
    /// A point is inside when the signed number of times the path winds around
    /// it is not zero. Overlapping subpaths that wind the same way merge.
    #[default]
    NonZero,
    /// A point is inside when a ray from it crosses the path an odd number of
    /// times. Overlapping subpaths punch holes in each other.
    EvenOdd,
}

/// The largest number of sub-scanlines a caller may ask for.
///
/// Past this the cost grows without a visible return, and the product
/// `height * samples` has to stay well inside a `u32` so the row loop cannot
/// overflow.
pub const MAX_SAMPLES_PER_PIXEL: u32 = 64;

/// How a path should be turned into coverage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FillOptions {
    /// Which points count as inside.
    pub rule: FillRule,
    /// Maximum distance between a curve and the polyline that replaces it.
    pub tolerance: f64,
    /// Sub-scanlines per pixel row. Clamped to `1..=`[`MAX_SAMPLES_PER_PIXEL`].
    pub samples_per_pixel: u32,
    /// Restrict output to these pixels. `None` means "the path's own bounds".
    pub clip: Option<PixelRect>,
}

impl Default for FillOptions {
    fn default() -> Self {
        Self {
            rule: FillRule::NonZero,
            tolerance: crate::DEFAULT_TOLERANCE,
            samples_per_pixel: 16,
            clip: None,
        }
    }
}

impl FillOptions {
    /// Default options with a chosen fill rule.
    pub fn with_rule(rule: FillRule) -> Self {
        Self {
            rule,
            ..Self::default()
        }
    }

    /// A copy restricted to `clip`.
    pub fn clipped_to(mut self, clip: PixelRect) -> Self {
        self.clip = Some(clip);
        self
    }
}

/// One non-horizontal edge, normalised so `y0 < y1`.
#[derive(Debug, Clone, Copy)]
struct Edge {
    x0: f64,
    y0: f64,
    y1: f64,
    dxdy: f64,
    /// `+1` when the original edge ran in +y, `-1` when it ran in -y.
    dir: i32,
}

/// Rasterise a path into a coverage mask.
///
/// Open subpaths are implicitly closed, which is what every fill in every
/// vector editor does: a fill is a question about the region a path bounds, and
/// an open path still bounds one.
pub fn fill(path: &Path, opts: &FillOptions) -> Result<CoverageMask, VectorError> {
    let polys = path.flatten_closed(opts.tolerance);
    fill_polylines(&polys, opts)
}

/// Rasterise already-flattened rings.
///
/// Exposed because stroking and boolean ops already hold polylines and would
/// otherwise pay to rebuild a [`Path`] just to have it flattened again.
pub fn fill_polylines(polys: &[Polyline], opts: &FillOptions) -> Result<CoverageMask, VectorError> {
    let samples = opts.samples_per_pixel.clamp(1, MAX_SAMPLES_PER_PIXEL);

    let mut edges: Vec<Edge> = Vec::new();
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for poly in polys {
        for (a, b) in poly.edges() {
            // A non-finite vertex has no position, so it has no crossings; it
            // is dropped rather than allowed to turn every bound into NaN.
            if !a.is_finite() || !b.is_finite() {
                continue;
            }
            min_x = min_x.min(a.x).min(b.x);
            max_x = max_x.max(a.x).max(b.x);
            min_y = min_y.min(a.y).min(b.y);
            max_y = max_y.max(a.y).max(b.y);
            if a.y == b.y {
                // Horizontal edges cross no sub-scanline; including them would
                // only ever produce a crossing exactly at a sample, which is
                // the one case the sample-at-the-centre rule already excludes.
                continue;
            }
            let (p, q, dir) = if a.y < b.y { (a, b, 1) } else { (b, a, -1) };
            edges.push(Edge {
                x0: p.x,
                y0: p.y,
                y1: q.y,
                dxdy: (q.x - p.x) / (q.y - p.y),
                dir,
            });
        }
    }

    if edges.is_empty() || !min_x.is_finite() {
        return Ok(CoverageMask::empty(IVec2::ZERO));
    }

    let path_rect = PixelRect::enclosing(crate::point::Bounds::new(
        crate::point::Point::new(min_x, min_y),
        crate::point::Point::new(max_x, max_y),
    ));
    let rect = match opts.clip {
        Some(c) => path_rect.intersection(c),
        None => path_rect,
    };
    if rect.is_empty() {
        return Ok(CoverageMask::empty(rect.min()));
    }

    let n_samples = rect.checked_samples()?;
    let width = rect.width() as usize;
    let mut coverage = alloc_vec(n_samples, 0u8)?;
    let mut acc = alloc_vec(width, 0.0f32)?;

    // Only edges that can reach the output rectangle matter.
    let (top, bottom) = (rect.min().y as f64, rect.max().y as f64);
    edges.retain(|e| e.y1 > top && e.y0 < bottom);
    if edges.is_empty() {
        return CoverageMask::new(rect.min(), rect.width(), rect.height(), coverage);
    }
    edges.sort_unstable_by(|a, b| a.y0.total_cmp(&b.y0));

    let weight = 1.0f32 / samples as f32;
    let step = 1.0 / samples as f64;
    let origin_x = rect.min().x as f64;

    let mut active: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut crossings: Vec<(f64, i32)> = Vec::new();

    for row in 0..rect.height() as usize {
        let y_top = rect.min().y as f64 + row as f64;
        let y_bot = y_top + 1.0;

        while next < edges.len() && edges[next].y0 < y_bot {
            active.push(next);
            next += 1;
        }
        active.retain(|&i| edges[i].y1 > y_top);
        if active.is_empty() {
            continue;
        }

        acc.iter_mut().for_each(|v| *v = 0.0);

        for k in 0..samples {
            let ys = y_top + (k as f64 + 0.5) * step;
            crossings.clear();
            for &i in &active {
                let e = &edges[i];
                if ys >= e.y0 && ys < e.y1 {
                    crossings.push((e.x0 + (ys - e.y0) * e.dxdy, e.dir));
                }
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));

            match opts.rule {
                FillRule::NonZero => {
                    let mut winding = 0i32;
                    let mut span_start = 0.0f64;
                    for &(x, dir) in &crossings {
                        let before = winding;
                        winding += dir;
                        if before == 0 && winding != 0 {
                            span_start = x;
                        } else if before != 0 && winding == 0 {
                            add_span(&mut acc, origin_x, span_start, x, weight);
                        }
                    }
                }
                FillRule::EvenOdd => {
                    for pair in crossings.chunks_exact(2) {
                        add_span(&mut acc, origin_x, pair[0].0, pair[1].0, weight);
                    }
                }
            }
        }

        let dst = &mut coverage[row * width..(row + 1) * width];
        for (d, &a) in dst.iter_mut().zip(acc.iter()) {
            *d = to_byte(a);
        }
    }

    CoverageMask::new(rect.min(), rect.width(), rect.height(), coverage)
}

/// Add one inside span's exact horizontal coverage to a row accumulator.
///
/// The partial pixels at each end get their true fractional overlap; the run
/// between them gets the full weight. This is where the "exact horizontally"
/// half of the rasteriser's guarantee lives.
fn add_span(acc: &mut [f32], origin_x: f64, xa: f64, xb: f64, weight: f32) {
    let w = acc.len() as f64;
    let a = (xa - origin_x).clamp(0.0, w);
    let b = (xb - origin_x).clamp(0.0, w);
    if a.is_nan() || b.is_nan() || b <= a {
        return;
    }
    let ia = a.floor() as usize;
    let ib = (b.ceil() as usize).min(acc.len());
    if ia >= acc.len() {
        return;
    }
    if ib <= ia + 1 {
        acc[ia] += weight * (b - a) as f32;
        return;
    }
    acc[ia] += weight * ((ia + 1) as f64 - a) as f32;
    for v in &mut acc[ia + 1..ib - 1] {
        *v += weight;
    }
    acc[ib - 1] += weight * (b - (ib - 1) as f64) as f32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::point;
    use crate::shapes;

    fn square(x: f64, y: f64, s: f64) -> Path {
        let mut p = Path::new();
        p.move_to(point(x, y))
            .line_to(point(x + s, y))
            .line_to(point(x + s, y + s))
            .line_to(point(x, y + s))
            .close();
        p
    }

    #[test]
    fn a_unit_square_covers_exactly_one_pixel() {
        let m = fill(&square(0.0, 0.0, 1.0), &FillOptions::default()).unwrap();
        assert_eq!((m.width(), m.height()), (1, 1));
        assert_eq!(m.coverage(), &[255]);
        assert_eq!(m.area(), 1.0);
    }

    #[test]
    fn a_pixel_aligned_square_covers_exactly_its_own_area() {
        for s in [1.0, 2.0, 7.0, 16.0] {
            let m = fill(&square(0.0, 0.0, s), &FillOptions::default()).unwrap();
            assert_eq!(m.area(), s * s, "a {s}x{s} square");
            assert!(
                m.coverage().iter().all(|&v| v == 255),
                "an axis-aligned square must have no partial pixels"
            );
        }
    }

    #[test]
    fn a_half_pixel_offset_square_splits_its_edge_pixels_evenly() {
        // Area is still exact, and the border pixels are exactly half covered
        // in one axis and quarter-covered at the corners.
        let m = fill(&square(0.5, 0.5, 2.0), &FillOptions::default()).unwrap();
        assert_eq!((m.width(), m.height()), (3, 3));
        assert!((m.area() - 4.0).abs() < 0.02, "area {}", m.area());
        assert_eq!(m.coverage_at(IVec2::new(1, 1)), 255);
        // corner: a quarter of a pixel
        assert!((m.coverage_f32(IVec2::new(0, 0)) - 0.25).abs() < 0.01);
        // edge: half a pixel
        assert!((m.coverage_f32(IVec2::new(1, 0)) - 0.5).abs() < 0.01);
        assert!((m.coverage_f32(IVec2::new(0, 1)) - 0.5).abs() < 0.01);
    }

    #[test]
    fn a_45_degree_edge_is_half_covered_at_the_boundary() {
        // The region x < y inside a 32x32 box: the diagonal passes through the
        // centre of every pixel on the leading diagonal, so each must come out
        // at exactly half coverage — the property that distinguishes real
        // anti-aliasing from a jagged edge with a blur over it.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(32.0, 32.0))
            .line_to(point(0.0, 32.0))
            .close();
        let m = fill(&p, &FillOptions::default()).unwrap();
        for i in 0..32 {
            let c = m.coverage_f32(IVec2::new(i, i));
            assert!(
                (c - 0.5).abs() < 0.01,
                "pixel ({i},{i}) on the 45-degree edge is {c}, not 0.5"
            );
        }
        // Well inside is solid, well outside is empty.
        assert_eq!(m.coverage_at(IVec2::new(2, 20)), 255);
        assert_eq!(m.coverage_at(IVec2::new(20, 2)), 0);
        // And the total area is the triangle's, to within edge quantisation.
        assert!((m.area() - 32.0 * 32.0 / 2.0).abs() < 0.5, "{}", m.area());
    }

    #[test]
    fn nonzero_and_even_odd_differ_on_a_self_intersecting_star() {
        // A five-pointed star drawn as one self-intersecting pentagram: the
        // centre pentagon is wound twice, so nonzero fills it and even-odd
        // leaves it hollow. This is the entire difference between the rules.
        let star = shapes::pentagram(point(64.0, 64.0), 60.0, 0.0);
        let nz = fill(&star, &FillOptions::with_rule(FillRule::NonZero)).unwrap();
        let eo = fill(&star, &FillOptions::with_rule(FillRule::EvenOdd)).unwrap();

        let center = IVec2::new(64, 64);
        assert_eq!(nz.coverage_at(center), 255, "nonzero must fill the centre");
        assert_eq!(eo.coverage_at(center), 0, "even-odd must hollow the centre");

        // The points of the star are wound once, so both rules agree there.
        let tip = IVec2::new(64, 64 - 50);
        assert_eq!(nz.coverage_at(tip), 255);
        assert_eq!(eo.coverage_at(tip), 255);

        // The areas differ by exactly the centre pentagon. For a {5/2} star of
        // circumradius R the spikes alone are 0.776 R^2 and the whole outline is
        // 1.123 R^2, so nonzero must be about 1.45x even-odd.
        let ratio = nz.area() / eo.area();
        assert!(
            (ratio - 1.447).abs() < 0.02,
            "nonzero {} / even-odd {} = {ratio}, expected ~1.447",
            nz.area(),
            eo.area()
        );
    }

    #[test]
    fn a_hole_is_a_hole_under_both_rules_when_it_is_wound_the_other_way() {
        // Outer ring counter-clockwise, inner ring clockwise: a proper hole.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(20.0, 0.0))
            .line_to(point(20.0, 20.0))
            .line_to(point(0.0, 20.0))
            .close();
        p.move_to(point(5.0, 5.0))
            .line_to(point(5.0, 15.0))
            .line_to(point(15.0, 15.0))
            .line_to(point(15.0, 5.0))
            .close();
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let m = fill(&p, &FillOptions::with_rule(rule)).unwrap();
            assert_eq!(m.coverage_at(IVec2::new(10, 10)), 0, "{rule:?}");
            assert_eq!(m.coverage_at(IVec2::new(2, 10)), 255, "{rule:?}");
            assert_eq!(m.area(), 400.0 - 100.0, "{rule:?}");
        }
    }

    #[test]
    fn a_clip_limits_the_mask_without_moving_the_pixels() {
        let big = square(0.0, 0.0, 40.0);
        let full = fill(&big, &FillOptions::default()).unwrap();
        let clipped = fill(
            &big,
            &FillOptions::default().clipped_to(PixelRect::from_xywh(10, 10, 5, 5)),
        )
        .unwrap();
        assert_eq!(clipped.origin(), IVec2::new(10, 10));
        assert_eq!((clipped.width(), clipped.height()), (5, 5));
        assert_eq!(clipped.area(), 25.0);
        for y in 10..15 {
            for x in 10..15 {
                let p = IVec2::new(x, y);
                assert_eq!(clipped.coverage_at(p), full.coverage_at(p));
            }
        }
        // A clip that misses the shape produces nothing, not a panic.
        let miss = fill(
            &big,
            &FillOptions::default().clipped_to(PixelRect::from_xywh(500, 500, 4, 4)),
        )
        .unwrap();
        assert!(miss.is_empty());
    }

    #[test]
    fn the_rasterised_area_of_a_circle_is_pi_r_squared() {
        // A circle has no exact pixel decomposition, so it is the honest test of
        // whether the coverage a shape produces really is its area.
        let c = shapes::ellipse(point(50.0, 50.0), point(30.0, 30.0));
        let exact = std::f64::consts::PI * 900.0;
        let opts = FillOptions {
            samples_per_pixel: 16,
            tolerance: 0.001,
            ..FillOptions::default()
        };
        let got = fill(&c, &opts).unwrap().area();
        assert!(
            (got - exact).abs() / exact < 0.001,
            "rasterised area {got}, analytic {exact}"
        );

        // And sub-scanline count is a real knob, not a decoration: one sample
        // per row quantises every near-horizontal edge to all-or-nothing.
        let coarse = fill(
            &c,
            &FillOptions {
                samples_per_pixel: 1,
                ..opts.clone()
            },
        )
        .unwrap();
        assert_ne!(coarse.coverage(), fill(&c, &opts).unwrap().coverage());
        // Out-of-range sample counts clamp instead of dividing by zero.
        for n in [0u32, u32::MAX] {
            let m = fill(
                &c,
                &FillOptions {
                    samples_per_pixel: n,
                    ..opts.clone()
                },
            )
            .unwrap();
            assert!((m.area() - exact).abs() / exact < 0.05, "{n} samples");
        }
    }

    #[test]
    fn an_open_subpath_fills_as_if_it_were_closed() {
        let mut open = Path::new();
        open.move_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .line_to(point(10.0, 10.0))
            .line_to(point(0.0, 10.0));
        let mut closed = open.clone();
        closed.close();
        assert_eq!(
            fill(&open, &FillOptions::default()).unwrap(),
            fill(&closed, &FillOptions::default()).unwrap()
        );
    }

    #[test]
    fn degenerate_input_produces_an_empty_mask_and_never_panics() {
        let opts = FillOptions::default();
        assert!(fill(&Path::new(), &opts).unwrap().is_empty());

        let mut single = Path::new();
        single.move_to(point(3.0, 3.0));
        assert!(fill(&single, &opts).unwrap().is_empty());

        let mut zero_len = Path::new();
        zero_len
            .move_to(point(1.0, 1.0))
            .line_to(point(1.0, 1.0))
            .close();
        assert!(fill(&zero_len, &opts).unwrap().is_empty());

        // A horizontal-only path bounds no area.
        let mut flat = Path::new();
        flat.move_to(point(0.0, 5.0))
            .line_to(point(10.0, 5.0))
            .close();
        assert!(fill(&flat, &opts).unwrap().is_empty());

        // NaN coordinates are dropped, not propagated into the bounds.
        let mut nan = Path::new();
        nan.move_to(point(f64::NAN, 0.0))
            .line_to(point(1.0, 1.0))
            .close();
        assert!(fill(&nan, &opts).unwrap().is_empty());

        // A shape whose bounds exceed the mask limit is an error, not an abort.
        let huge = square(-1e9, -1e9, 2e9);
        assert!(matches!(
            fill(&huge, &opts),
            Err(VectorError::RegionTooLarge { .. })
        ));
    }

    #[test]
    fn a_shape_far_from_the_origin_allocates_only_its_own_box() {
        let m = fill(&square(100_000.0, 100_000.0, 4.0), &FillOptions::default()).unwrap();
        assert_eq!(m.origin(), IVec2::new(100_000, 100_000));
        assert_eq!(m.coverage().len(), 16);
        assert_eq!(m.area(), 16.0);
    }

    #[test]
    fn coverage_is_monotone_across_a_soft_edge() {
        // A shallow, near-horizontal edge is the worst case for a scanline
        // rasteriser; the ramp must still be monotone, not stippled.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(64.0, 8.0))
            .line_to(point(64.0, 0.0))
            .close();
        let m = fill(&p, &FillOptions::default()).unwrap();
        let mut prev = 0.0;
        for x in 0..64 {
            let col: f32 = (0..8).map(|y| m.coverage_f32(IVec2::new(x, y))).sum();
            assert!(col + 1e-4 >= prev, "column {x} dipped: {col} after {prev}");
            prev = col;
        }
    }
}
