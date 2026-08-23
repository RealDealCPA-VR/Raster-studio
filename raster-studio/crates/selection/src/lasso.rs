//! Lasso tools: freehand, polygonal, and magnetic.
//!
//! All three end in the same place — an anti-aliased polygon fill — and differ
//! only in where the vertices come from. Freehand and polygonal take the user's
//! points directly; magnetic takes a rough guide path and pulls it onto the
//! nearest strong image edge before filling.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use color::srgb_luminance;
use editor_core::SelectionMask;
use glam::{IVec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::buf::{
    alloc_f32, alloc_vec, checked_samples, try_extend, try_heap_push, try_push, CoverageBuf,
};
use crate::error::SelectionOpError;
use crate::image::ImageView;
use crate::rect::Rect;
use crate::scan::{RowAccum, SUBSCANLINES};

/// Which points of a self-overlapping outline count as inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FillRule {
    /// A point is inside when the signed crossing count is non-zero. This is
    /// what a freehand loop that crosses itself should do: the overlap stays
    /// selected.
    #[default]
    NonZero,
    /// A point is inside when the crossing count is odd, so a self-overlap
    /// punches a hole.
    EvenOdd,
}

/// Fill a closed polygon with anti-aliased coverage.
///
/// The polygon is implicitly closed — the last point joins the first. Fewer
/// than three points enclose no area and select nothing.
pub fn polygon(points: &[Vec2], rule: FillRule) -> Result<SelectionMask, SelectionOpError> {
    for p in points {
        if !p.x.is_finite() {
            return Err(SelectionOpError::NotFinite {
                what: "polygon vertex x",
                value: p.x,
            });
        }
        if !p.y.is_finite() {
            return Err(SelectionOpError::NotFinite {
                what: "polygon vertex y",
                value: p.y,
            });
        }
    }
    if points.len() < 3 {
        return Ok(SelectionMask::new(IVec2::ZERO, 0, 0, Vec::new())?);
    }
    let mut lo = points[0];
    let mut hi = points[0];
    for p in points {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let bbox = Rect::enclosing(lo, hi);
    if bbox.is_empty() {
        return Ok(SelectionMask::new(bbox.min(), 0, 0, Vec::new())?);
    }

    // Rasterise in the bounding box's local frame: `f32` spacing at a
    // coordinate of 2^29 is 64 pixels, so a small polygon drawn far from the
    // origin would otherwise collapse. See `marquee::localise`.
    let origin = glam::Vec2::new(bbox.min().x as f32, bbox.min().y as f32);
    let local: Vec<Vec2> = points.iter().map(|p| *p - origin).collect();

    let mut buf = CoverageBuf::zeroed(bbox)?;
    let width = bbox.width() as usize;
    let mut accum = RowAccum::new(0, width)?;
    let sub = 1.0 / SUBSCANLINES as f32;
    let mut crossings: Vec<(f32, i32)> = Vec::new();

    for row in 0..bbox.height() as usize {
        for s in 0..SUBSCANLINES {
            let y = row as f32 + (s as f32 + 0.5) * sub;
            crossings.clear();
            for i in 0..local.len() {
                let a = local[i];
                let b = local[(i + 1) % local.len()];
                // A horizontal edge never straddles the scanline, so this also
                // excludes the division by zero.
                if (a.y <= y) == (b.y <= y) {
                    continue;
                }
                let t = (y - a.y) / (b.y - a.y);
                crossings.push((a.x + t * (b.x - a.x), if b.y > a.y { 1 } else { -1 }));
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|l, r| l.0.total_cmp(&r.0));

            let mut wind = 0i32;
            let mut inside = false;
            let mut start = 0.0f32;
            for &(x, dir) in &crossings {
                wind += dir;
                let now = match rule {
                    FillRule::NonZero => wind != 0,
                    FillRule::EvenOdd => wind.rem_euclid(2) != 0,
                };
                if now && !inside {
                    start = x;
                    inside = true;
                } else if !now && inside {
                    accum.add_span(start, x, sub);
                    inside = false;
                }
            }
        }
        accum.finish_into(buf.row_mut(row));
    }
    buf.into_mask()
}

/// A freehand lasso: the sampled cursor path, closed and filled.
pub fn lasso_freehand(path: &[Vec2]) -> Result<SelectionMask, SelectionOpError> {
    polygon(path, FillRule::NonZero)
}

/// A polygonal lasso: the clicked corners, closed and filled.
pub fn lasso_polygonal(corners: &[Vec2]) -> Result<SelectionMask, SelectionOpError> {
    polygon(corners, FillRule::NonZero)
}

/// How hard the magnetic lasso is pulled onto image edges.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MagneticOptions {
    /// How far from the straight line between two anchors the path may wander.
    pub search_radius: u32,
    /// Weight of the "this pixel is not on an edge" penalty. Larger snaps
    /// harder.
    pub edge_weight: f32,
    /// Weight of the "this pixel is far from the guide line" penalty. Larger
    /// keeps the path near what the user drew.
    pub straight_weight: f32,
}

impl Default for MagneticOptions {
    fn default() -> Self {
        Self {
            search_radius: 24,
            edge_weight: 1.0,
            straight_weight: 0.02,
        }
    }
}

/// Gradient magnitude of the image's luminance, normalised to `0..=1`.
///
/// A Sobel gradient is a *filter on intensity*, so it runs on **linear**
/// luminance: on gamma-encoded values the same physical edge would read as a
/// much stronger feature in the shadows than in the highlights, and the lasso
/// would snap to noise in dark areas and slide off real edges in bright ones.
fn edge_strength(img: &ImageView) -> Result<Vec<f32>, SelectionOpError> {
    let (w, h) = (img.width(), img.height());
    let mut lum = alloc_f32(w * h)?;
    let o = img.rect().min();
    for y in 0..h {
        for x in 0..w {
            let px = img.pixel(IVec2::new(o.x + x as i32, o.y + y as i32));
            let l = srgb_luminance([
                px[0] as f32 / 255.0,
                px[1] as f32 / 255.0,
                px[2] as f32 / 255.0,
            ]);
            lum[y * w + x] = l * (px[3] as f32 / 255.0);
        }
    }
    let mut grad = alloc_f32(w * h)?;
    let at = |x: isize, y: isize| -> f32 {
        let cx = x.clamp(0, w as isize - 1) as usize;
        let cy = y.clamp(0, h as isize - 1) as usize;
        lum[cy * w + cx]
    };
    let mut max = 0.0f32;
    for y in 0..h as isize {
        for x in 0..w as isize {
            let gx = (at(x + 1, y - 1) + 2.0 * at(x + 1, y) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x - 1, y) + at(x - 1, y + 1));
            let gy = (at(x - 1, y + 1) + 2.0 * at(x, y + 1) + at(x + 1, y + 1))
                - (at(x - 1, y - 1) + 2.0 * at(x, y - 1) + at(x + 1, y - 1));
            let g = (gx * gx + gy * gy).sqrt();
            grad[y as usize * w + x as usize] = g;
            max = max.max(g);
        }
    }
    if max > 0.0 {
        for g in &mut grad {
            *g /= max;
        }
    }
    Ok(grad)
}

/// Total-orderable cost, so `f32` distances can live in a binary heap.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Cost(f32);

impl Eq for Cost {}

impl PartialOrd for Cost {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Cost {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    if len2 <= f32::EPSILON {
        return (p - a).length();
    }
    let t = ((p - a).dot(ab) / len2).clamp(0.0, 1.0);
    (p - (a + ab * t)).length()
}

/// Least-cost path between two pixels, hugging strong edges.
///
/// The search band is at most the image, but its three Dijkstra tables cost 13
/// bytes per band pixel — more than the image itself — so they are allocated
/// through [`alloc_vec`]: a band this machine cannot hold is an error, not an
/// abort inside `handle_alloc_error`.
fn snap_segment(
    img: &ImageView,
    grad: &[f32],
    from: IVec2,
    to: IVec2,
    guide_a: Vec2,
    guide_b: Vec2,
    opts: &MagneticOptions,
) -> Result<Vec<IVec2>, SelectionOpError> {
    let r = opts.search_radius.min(1 << 20) as i32;
    let band = Rect::new(from.min(to), from.max(to) + IVec2::ONE)
        .inflate(r)
        .intersection(img.rect());
    if band.is_empty() || !band.contains(from) || !band.contains(to) {
        return Ok(vec![from, to]);
    }
    let bw = band.width() as usize;
    let n = checked_samples(band)?;
    let idx =
        |p: IVec2| -> usize { (p.y - band.min().y) as usize * bw + (p.x - band.min().x) as usize };
    let iw = img.width();
    let io = img.rect().min();

    let mut dist = alloc_vec(n, f32::INFINITY)?;
    let mut prev = alloc_vec(n, usize::MAX)?;
    let mut done = alloc_vec(n, false)?;
    let start = idx(from);
    let goal = idx(to);
    dist[start] = 0.0;
    let mut heap: BinaryHeap<std::cmp::Reverse<(Cost, usize)>> = BinaryHeap::new();
    try_heap_push(&mut heap, std::cmp::Reverse((Cost(0.0), start)))?;

    let steps: [(i32, i32); 8] = [
        (1, 0),
        (-1, 0),
        (0, 1),
        (0, -1),
        (1, 1),
        (1, -1),
        (-1, 1),
        (-1, -1),
    ];

    while let Some(std::cmp::Reverse((Cost(d), u))) = heap.pop() {
        if done[u] {
            continue;
        }
        done[u] = true;
        if u == goal {
            break;
        }
        let up = IVec2::new(
            band.min().x + (u % bw) as i32,
            band.min().y + (u / bw) as i32,
        );
        for (dx, dy) in steps {
            let vp = up + IVec2::new(dx, dy);
            if !band.contains(vp) {
                continue;
            }
            let v = idx(vp);
            if done[v] {
                continue;
            }
            let step = if dx != 0 && dy != 0 {
                std::f32::consts::SQRT_2
            } else {
                1.0
            };
            let g = grad[(vp.y - io.y) as usize * iw + (vp.x - io.x) as usize];
            let stray = distance_to_segment(
                Vec2::new(vp.x as f32 + 0.5, vp.y as f32 + 0.5),
                guide_a,
                guide_b,
            );
            let w = step
                * (0.05
                    + opts.edge_weight.max(0.0) * (1.0 - g)
                    + opts.straight_weight.max(0.0) * stray);
            let nd = d + w;
            if nd < dist[v] {
                dist[v] = nd;
                prev[v] = u;
                try_heap_push(&mut heap, std::cmp::Reverse((Cost(nd), v)))?;
            }
        }
    }

    if !dist[goal].is_finite() {
        return Ok(vec![from, to]);
    }
    let mut out = Vec::new();
    let mut cur = goal;
    while cur != usize::MAX {
        try_push(
            &mut out,
            IVec2::new(
                band.min().x + (cur % bw) as i32,
                band.min().y + (cur / bw) as i32,
            ),
        )?;
        if cur == start {
            break;
        }
        cur = prev[cur];
    }
    out.reverse();
    Ok(out)
}

/// Snap a guide path onto the image's edges, returning the snapped pixel path
/// in pixel-centre coordinates.
///
/// The path is **open**: it runs from the first anchor to the last, and does
/// not close back. [`lasso_magnetic`] closes it.
pub fn magnetic_path(
    img: &ImageView,
    anchors: &[Vec2],
    opts: &MagneticOptions,
) -> Result<Vec<Vec2>, SelectionOpError> {
    for a in anchors {
        if !a.x.is_finite() || !a.y.is_finite() {
            return Err(SelectionOpError::NotFinite {
                what: "magnetic anchor",
                value: if a.x.is_finite() { a.y } else { a.x },
            });
        }
    }
    if anchors.len() < 2 || img.rect().is_empty() {
        return Ok(anchors.to_vec());
    }
    let grad = edge_strength(img)?;
    let clamp_to_image = |p: Vec2| -> IVec2 {
        let r = img.rect();
        IVec2::new(
            (p.x.floor() as i64).clamp(r.min().x as i64, r.max().x as i64 - 1) as i32,
            (p.y.floor() as i64).clamp(r.min().y as i64, r.max().y as i64 - 1) as i32,
        )
    };

    let mut out: Vec<IVec2> = Vec::new();
    for pair in anchors.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let seg = snap_segment(img, &grad, clamp_to_image(a), clamp_to_image(b), a, b, opts)?;
        if out.last() == seg.first() {
            try_extend(&mut out, &seg[1..])?;
        } else {
            try_extend(&mut out, &seg)?;
        }
    }
    Ok(out
        .into_iter()
        .map(|p| Vec2::new(p.x as f32 + 0.5, p.y as f32 + 0.5))
        .collect())
}

/// A magnetic lasso: snap the guide path to image edges, close it, and fill.
pub fn lasso_magnetic(
    img: &ImageView,
    anchors: &[Vec2],
    opts: &MagneticOptions,
) -> Result<SelectionMask, SelectionOpError> {
    if anchors.len() < 3 {
        return Ok(SelectionMask::new(IVec2::ZERO, 0, 0, Vec::new())?);
    }
    let mut closed: Vec<Vec2> = anchors.to_vec();
    closed.push(anchors[0]);
    let path = magnetic_path(img, &closed, opts)?;
    polygon(&path, FillRule::NonZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image::ImageBuffer;

    fn total_coverage(m: &SelectionMask) -> f64 {
        m.coverage().iter().map(|&v| v as f64 / 255.0).sum()
    }

    #[test]
    fn a_polygonal_lasso_of_a_rectangle_is_that_rectangle() {
        let m = lasso_polygonal(&[
            Vec2::new(2.0, 3.0),
            Vec2::new(8.0, 3.0),
            Vec2::new(8.0, 7.0),
            Vec2::new(2.0, 7.0),
        ])
        .unwrap();
        assert_eq!(m.bounds(), Some((IVec2::new(2, 3), IVec2::new(8, 7))));
        assert!(
            m.coverage().iter().all(|&v| v == 255),
            "an axis-aligned pixel-aligned polygon has no partial coverage"
        );
    }

    #[test]
    fn a_triangle_covers_half_its_bounding_box() {
        let m = lasso_freehand(&[
            Vec2::new(0.0, 0.0),
            Vec2::new(40.0, 0.0),
            Vec2::new(0.0, 40.0),
        ])
        .unwrap();
        let area = total_coverage(&m);
        assert!(
            (area - 800.0).abs() < 6.0,
            "triangle area should be 800, got {area}"
        );
        // And the hypotenuse is anti-aliased rather than stair-stepped.
        let partial = m.coverage().iter().filter(|&&v| v > 0 && v < 255).count();
        assert!(
            partial > 30,
            "expected a fractional diagonal, got {partial}"
        );
    }

    #[test]
    fn a_shallow_edge_is_antialiased_along_its_whole_length() {
        // A hypotenuse of slope 1/8 crosses a single pixel row over eight
        // columns, so the row must carry a graded ramp. This is what the
        // sub-scanline integration buys: sampling one scanline per row would
        // decide the whole row from the crossing at its centre and produce a
        // hard step at x = 4 instead of a ramp across x = 0..8.
        let m = polygon(
            &[
                Vec2::new(0.0, 0.0),
                Vec2::new(32.0, 0.0),
                Vec2::new(32.0, 4.0),
            ],
            FillRule::NonZero,
        )
        .unwrap();
        let row: Vec<u8> = (0..8).map(|x| m.coverage_at(IVec2::new(x, 0))).collect();
        assert!(
            row.iter().all(|&v| v > 0 && v < 255),
            "every column under the shallow edge should be partial, got {row:?}"
        );
        for pair in row.windows(2) {
            assert!(
                pair[1] > pair[0],
                "the ramp must rise monotonically, got {row:?}"
            );
        }
        assert_eq!(
            m.coverage_at(IVec2::new(20, 0)),
            255,
            "well inside is solid"
        );
    }

    #[test]
    fn a_polygon_rasterises_identically_wherever_it_is_placed() {
        // Rasterising in absolute coordinates fails here: at x = 2^22 the gap
        // between neighbouring f32 values is half a pixel, so the sixteen
        // sub-scanline offsets inside a row would collapse onto two and the
        // anti-aliasing would coarsen with distance from the origin.
        // Half-pixel vertices, exactly representable at 2^22, so the fixture
        // itself loses nothing and only the rasteriser is under test.
        let far = (1i32 << 22) as f32;
        let shape = |o: f32| {
            vec![
                Vec2::new(o + 1.5, o + 0.5),
                Vec2::new(o + 18.5, o + 3.5),
                Vec2::new(o + 6.0, o + 17.5),
            ]
        };
        let here = polygon(&shape(0.0), FillRule::NonZero).unwrap();
        let there = polygon(&shape(far), FillRule::NonZero).unwrap();
        assert_eq!(here.width(), there.width());
        assert_eq!(here.height(), there.height());
        assert_eq!(
            here.coverage(),
            there.coverage(),
            "the same polygon rasterised far from the origin lost precision"
        );
        assert_eq!(
            there.origin(),
            here.origin() + IVec2::splat(far as i32),
            "and it landed where it was asked to"
        );
    }

    #[test]
    fn the_fill_rule_decides_what_a_self_overlap_does() {
        // A five-pointed star: non-zero fills the middle, even-odd hollows it.
        let mut pts = Vec::new();
        for i in 0..5 {
            let a = std::f32::consts::TAU * (i as f32 * 2.0) / 5.0 - std::f32::consts::FRAC_PI_2;
            pts.push(Vec2::new(30.0 + 25.0 * a.cos(), 30.0 + 25.0 * a.sin()));
        }
        let nz = polygon(&pts, FillRule::NonZero).unwrap();
        let eo = polygon(&pts, FillRule::EvenOdd).unwrap();
        assert_eq!(nz.coverage_at(IVec2::new(30, 30)), 255);
        assert_eq!(eo.coverage_at(IVec2::new(30, 30)), 0);
        assert!(total_coverage(&nz) > total_coverage(&eo));
    }

    #[test]
    fn a_lasso_with_fewer_than_three_points_selects_nothing() {
        assert!(lasso_freehand(&[]).unwrap().is_empty());
        assert!(lasso_freehand(&[Vec2::ZERO]).unwrap().is_empty());
        assert!(lasso_freehand(&[Vec2::ZERO, Vec2::new(5.0, 5.0)])
            .unwrap()
            .is_empty());
        assert!(matches!(
            lasso_freehand(&[Vec2::ZERO, Vec2::new(f32::INFINITY, 0.0), Vec2::ONE]),
            Err(SelectionOpError::NotFinite { .. })
        ));
    }

    /// Black on the left of `x = 12`, white on the right.
    fn vertical_edge_image() -> ImageBuffer {
        let (w, h) = (24u32, 24u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = if x < 12 { 0 } else { 255 };
                px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap()
    }

    #[test]
    fn the_magnetic_path_snaps_onto_the_image_edge() {
        let img = vertical_edge_image();
        let opts = MagneticOptions {
            search_radius: 8,
            ..Default::default()
        };
        // Anchors drawn three pixels to the right of the real edge.
        let path = magnetic_path(
            &img.view(),
            &[Vec2::new(15.5, 1.5), Vec2::new(15.5, 22.5)],
            &opts,
        )
        .unwrap();
        assert!(
            path.len() > 10,
            "the path should follow the edge pixel by pixel"
        );
        // The middle of the run must sit on one of the two gradient columns,
        // not on the straight line the user drew.
        let middle: Vec<i32> = path
            .iter()
            .filter(|p| p.y > 6.0 && p.y < 18.0)
            .map(|p| p.x.floor() as i32)
            .collect();
        assert!(!middle.is_empty());
        for x in &middle {
            assert!(
                (11..=12).contains(x),
                "path strayed to column {x}; it should hug the edge at 11-12"
            );
        }
    }

    #[test]
    fn without_an_edge_to_snap_to_the_magnetic_path_stays_on_the_guide() {
        // A flat image has no gradient anywhere, so only the straightness term
        // is left and the path must not wander.
        let px = vec![128u8; 24 * 24 * 4];
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, 24, 24, px).unwrap();
        let path = magnetic_path(
            &img.view(),
            &[Vec2::new(4.5, 4.5), Vec2::new(4.5, 19.5)],
            &MagneticOptions::default(),
        )
        .unwrap();
        for p in &path {
            assert_eq!(p.x.floor() as i32, 4, "wandered off a featureless guide");
        }
    }

    #[test]
    fn a_magnetic_lasso_closes_its_path_into_a_region() {
        let (w, h) = (32u32, 32u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for y in 8..24u32 {
            for x in 8..24u32 {
                let i = ((y * w + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let img = ImageBuffer::from_rgba8(IVec2::ZERO, w, h, px).unwrap();
        let m = lasso_magnetic(
            &img.view(),
            &[
                Vec2::new(6.5, 6.5),
                Vec2::new(25.5, 6.5),
                Vec2::new(25.5, 25.5),
                Vec2::new(6.5, 25.5),
            ],
            &MagneticOptions {
                search_radius: 6,
                ..Default::default()
            },
        )
        .unwrap();
        // The bright square is selected; the loose anchor corners are not.
        assert_eq!(m.coverage_at(IVec2::new(16, 16)), 255);
        assert_eq!(m.coverage_at(IVec2::new(6, 6)), 0);
        let area = total_coverage(&m);
        assert!(
            (200.0..340.0).contains(&area),
            "should hug the 256px square rather than the 361px anchor quad, got {area}"
        );
    }
}
