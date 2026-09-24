//! W10-E: Image ▸ Vectorize Bitmap — a raster traced into filled paths, one
//! per colour.
//!
//! # The algorithm
//!
//! 1. **Posterize.** The opaque pixels (alpha >= 128; the rest are left
//!    untraced) are reduced to at most [`TraceOptions::colors`] colours by
//!    *median cut*: the colour box with the widest channel range is split at
//!    its pixel-weighted median along that channel until there are enough
//!    boxes, and each box's colour is its pixel-weighted mean. An image with
//!    no more distinct colours than asked keeps them exactly. Every pixel then
//!    takes its nearest palette colour.
//! 2. **Stack.** The colours are ordered by area, largest first, and colour
//!    `i`'s *layer mask* is the union of the regions of colours `i..n` — the
//!    stacking potrace uses for colour images. Painted bottom to top, the top
//!    layer containing a pixel is the pixel's own colour, and the bottom layer
//!    is every opaque pixel, so the stack covers the image with no hairline
//!    gaps where two independently smoothed outlines would otherwise meet.
//! 3. **Trace.** Each layer mask's boundary is followed along the pixel
//!    cracks ([`trace_mask`]): every edge between an inside and an outside
//!    pixel becomes a directed unit edge with the inside on the same hand, so
//!    outer contours and holes come out in opposite orientations and the
//!    nonzero rule fills exactly the mask. Collinear runs collapse to one
//!    edge.
//! 4. **Fit.** A vertex whose two edges are both at least
//!    [`TraceOptions::corner_length`] long is a real corner and is kept; every
//!    other vertex is a stair step, replaced by the midpoints of its edges.
//!    The points between corners are fitted with cubic Béziers by Schneider's
//!    least-squares method ("An Algorithm for Automatically Fitting Digitized
//!    Curves", *Graphics Gems*, 1990): chord-length parameters, a fit along
//!    the end tangents, Newton reparameterisation, and a split at the point of
//!    worst error while that error exceeds [`TraceOptions::tolerance`]. A
//!    contour with no corner is split in two at opposite points with shared
//!    tangents, so the closed curve stays smooth where it is joined.

use crate::error::VectorError;
use crate::path::Path;
use crate::point::Point;

/// The largest raster [`trace`] accepts, in pixels (16 megapixels).
pub const MAX_TRACE_PIXELS: u64 = 1 << 24;
/// The most colours [`trace`] posterizes to.
pub const MAX_TRACE_COLORS: usize = 64;

/// How a raster is traced.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TraceOptions {
    /// Posterize to at most this many colours (`1..=`[`MAX_TRACE_COLORS`]).
    pub colors: usize,
    /// The largest distance, in pixels, a fitted curve may stray from the
    /// traced points before it is split.
    pub tolerance: f64,
    /// Edges at least this long (in pixels) on both sides of a vertex make it
    /// a sharp corner rather than a stair step.
    pub corner_length: f64,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            colors: 8,
            tolerance: 1.0,
            corner_length: 3.0,
        }
    }
}

/// One traced colour: the path of its stacked layer mask (see the module
/// header) and how many pixels are that colour.
#[derive(Clone, Debug, PartialEq)]
pub struct TracedLayer {
    /// Straight RGBA8, alpha 255.
    pub color: [u8; 4],
    /// The layer outline, in pixel coordinates, to fill with the nonzero rule.
    pub path: Path,
    /// Pixels of this exact colour after posterizing.
    pub pixels: u64,
}

/// Trace `rgba` (row-major straight RGBA8, `width * height * 4` bytes) into
/// one filled path per posterized colour, bottom layer first.
pub fn trace(
    rgba: &[u8],
    width: u32,
    height: u32,
    options: &TraceOptions,
) -> Result<Vec<TracedLayer>, VectorError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_TRACE_PIXELS {
        return Err(VectorError::InvalidParameter {
            what: "the traced area",
            expected: "between 1 and 16777216 pixels",
            value: pixels as f64,
        });
    }
    if rgba.len() as u64 != pixels * 4 {
        return Err(VectorError::InvalidParameter {
            what: "the pixel buffer length",
            expected: "width * height * 4",
            value: rgba.len() as f64,
        });
    }
    if options.colors == 0 || options.colors > MAX_TRACE_COLORS {
        return Err(VectorError::InvalidParameter {
            what: "the colour count",
            expected: "between 1 and 64",
            value: options.colors as f64,
        });
    }
    if !(options.tolerance.is_finite() && options.tolerance > 0.0) {
        return Err(VectorError::InvalidParameter {
            what: "the curve tolerance",
            expected: "a positive number of pixels",
            value: options.tolerance,
        });
    }
    let (palette, index) = posterize(rgba, options.colors);
    let mut areas = vec![0u64; palette.len()];
    for &i in &index {
        if i != NONE {
            areas[i as usize] += 1;
        }
    }
    let mut order: Vec<usize> = (0..palette.len()).filter(|&i| areas[i] > 0).collect();
    order.sort_by(|&a, &b| areas[b].cmp(&areas[a]).then(a.cmp(&b)));
    // rank[c] = position of colour c in the stack.
    let mut rank = vec![usize::MAX; palette.len()];
    for (r, &c) in order.iter().enumerate() {
        rank[c] = r;
    }
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(order.len());
    for (r, &c) in order.iter().enumerate() {
        let mask: Vec<bool> = index
            .iter()
            .map(|&i| i != NONE && rank[i as usize] >= r)
            .collect();
        let mut path = Path::new();
        for contour in trace_mask(&mask, w, h) {
            fit_contour(&contour, options, &mut path);
        }
        let [cr, cg, cb] = palette[c];
        out.push(TracedLayer {
            color: [cr, cg, cb, 255],
            path,
            pixels: areas[c],
        });
    }
    Ok(out)
}

/// "No colour": a transparent pixel.
const NONE: u16 = u16::MAX;

/// Median-cut posterization: the palette and each pixel's palette index
/// ([`NONE`] for a pixel under half alpha).
pub fn posterize(rgba: &[u8], colors: usize) -> (Vec<[u8; 3]>, Vec<u16>) {
    use std::collections::HashMap;
    let mut counts: HashMap<[u8; 3], u64> = HashMap::new();
    for px in rgba.as_chunks::<4>().0 {
        if px[3] >= 128 {
            *counts.entry([px[0], px[1], px[2]]).or_insert(0) += 1;
        }
    }
    let mut distinct: Vec<([u8; 3], u64)> = counts.into_iter().collect();
    distinct.sort_unstable();
    let palette: Vec<[u8; 3]> = if distinct.len() <= colors {
        distinct.iter().map(|(c, _)| *c).collect()
    } else {
        median_cut(distinct.clone(), colors)
    };
    let mut nearest: HashMap<[u8; 3], u16> = HashMap::with_capacity(distinct.len());
    for (c, _) in &distinct {
        let best = palette
            .iter()
            .enumerate()
            .min_by_key(|(_, p)| {
                (0..3)
                    .map(|k| {
                        let d = i32::from(c[k]) - i32::from(p[k]);
                        d * d
                    })
                    .sum::<i32>()
            })
            .map_or(0, |(i, _)| i as u16);
        nearest.insert(*c, best);
    }
    let index = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|px| {
            if px[3] >= 128 {
                nearest[&[px[0], px[1], px[2]]]
            } else {
                NONE
            }
        })
        .collect();
    (palette, index)
}

fn median_cut(colors: Vec<([u8; 3], u64)>, target: usize) -> Vec<[u8; 3]> {
    let range = |b: &[([u8; 3], u64)]| -> (usize, u8) {
        (0..3)
            .map(|k| {
                let lo = b.iter().map(|(c, _)| c[k]).min().unwrap_or(0);
                let hi = b.iter().map(|(c, _)| c[k]).max().unwrap_or(0);
                (k, hi - lo)
            })
            .max_by_key(|&(k, r)| (r, std::cmp::Reverse(k)))
            .unwrap_or((0, 0))
    };
    let mut boxes = vec![colors];
    while boxes.len() < target {
        let Some((at, (axis, width))) = boxes
            .iter()
            .enumerate()
            .filter(|(_, b)| b.len() > 1)
            .map(|(i, b)| (i, range(b)))
            .max_by_key(|&(i, (_, r))| (r, std::cmp::Reverse(i)))
        else {
            break;
        };
        if width == 0 {
            break;
        }
        let mut b = boxes.swap_remove(at);
        b.sort_unstable_by_key(|(c, _)| c[axis]);
        let total: u64 = b.iter().map(|(_, n)| n).sum();
        let mut acc = 0;
        let mut split = 1;
        for (i, (_, n)) in b.iter().enumerate() {
            acc += n;
            if acc * 2 >= total {
                split = (i + 1).clamp(1, b.len() - 1);
                break;
            }
        }
        let rest = b.split_off(split);
        boxes.push(b);
        boxes.push(rest);
    }
    boxes
        .iter()
        .map(|b| {
            let total: u64 = b.iter().map(|(_, n)| n).sum::<u64>().max(1);
            let mean = |k: usize| {
                let sum: u64 = b.iter().map(|(c, n)| u64::from(c[k]) * n).sum();
                ((sum + total / 2) / total) as u8
            };
            [mean(0), mean(1), mean(2)]
        })
        .collect()
}

/// The closed crack-following contours of `mask` (`w * h`, row-major), as
/// corner vertices in pixel-grid coordinates. Each pixel is walked
/// clockwise on screen (y down), so outer contours run clockwise and holes
/// counter-clockwise, and the nonzero rule fills exactly the mask.
pub fn trace_mask(mask: &[bool], w: usize, h: usize) -> Vec<Vec<(i64, i64)>> {
    let inside = |x: i64, y: i64| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && mask[y as usize * w + x as usize]
    };
    // Directed unit edges keyed by their start vertex; at most two leave a
    // vertex (a saddle, where two diagonal pixels touch).
    let vid = |x: i64, y: i64| (y as usize) * (w + 1) + x as usize;
    let mut out_edges: Vec<[u32; 2]> = vec![[u32::MAX; 2]; (w + 1) * (h + 1)];
    let mut edges: Vec<((i64, i64), (i64, i64))> = Vec::new();
    let mut push = |from: (i64, i64), to: (i64, i64), edges: &mut Vec<_>| {
        let id = edges.len() as u32;
        edges.push((from, to));
        let slot = &mut out_edges[vid(from.0, from.1)];
        if slot[0] == u32::MAX {
            slot[0] = id;
        } else {
            slot[1] = id;
        }
    };
    for y in 0..h as i64 {
        for x in 0..w as i64 {
            if !inside(x, y) {
                continue;
            }
            if !inside(x, y - 1) {
                push((x, y), (x + 1, y), &mut edges);
            }
            if !inside(x + 1, y) {
                push((x + 1, y), (x + 1, y + 1), &mut edges);
            }
            if !inside(x, y + 1) {
                push((x + 1, y + 1), (x, y + 1), &mut edges);
            }
            if !inside(x - 1, y) {
                push((x, y + 1), (x, y), &mut edges);
            }
        }
    }
    let mut used = vec![false; edges.len()];
    let mut contours = Vec::new();
    for start in 0..edges.len() {
        if used[start] {
            continue;
        }
        let mut points = Vec::new();
        let mut e = start;
        loop {
            used[e] = true;
            let (from, to) = edges[e];
            points.push(from);
            let dir = (to.0 - from.0, to.1 - from.1);
            let candidates = out_edges[vid(to.0, to.1)];
            // At a saddle, turn right (clockwise on screen): the inside is on
            // the right of every edge, so this keeps diagonal neighbours
            // apart, i.e. the contour follows 4-connected regions.
            let right = (-dir.1, dir.0);
            let next = candidates
                .iter()
                .copied()
                .filter(|&c| c != u32::MAX && !used[c as usize])
                .max_by_key(|&c| {
                    let (a, b) = edges[c as usize];
                    let d = (b.0 - a.0, b.1 - a.1);
                    (d == right, d == dir)
                });
            match next {
                Some(n) => e = n as usize,
                None => break,
            }
        }
        contours.push(collapse_collinear(points));
    }
    contours
}

/// Drop every vertex that continues its neighbours' straight line.
fn collapse_collinear(points: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    let n = points.len();
    if n < 3 {
        return points;
    }
    (0..n)
        .filter(|&i| {
            let a = points[(i + n - 1) % n];
            let b = points[i];
            let c = points[(i + 1) % n];
            (b.0 - a.0) * (c.1 - b.1) != (b.1 - a.1) * (c.0 - b.0)
        })
        .map(|i| points[i])
        .collect()
}

/// Append one fitted closed contour to `path`.
fn fit_contour(corners: &[(i64, i64)], options: &TraceOptions, path: &mut Path) {
    let n = corners.len();
    if n < 3 {
        return;
    }
    let p = |i: usize| {
        let (x, y) = corners[i % n];
        Point::new(x as f64, y as f64)
    };
    let sharp: Vec<bool> = (0..n)
        .map(|i| {
            let before = p(i).distance(p(i + n - 1));
            let after = p(i).distance(p(i + 1));
            before >= options.corner_length && after >= options.corner_length
        })
        .collect();
    // The point sequence: each edge's midpoint, with sharp corners kept.
    // Entry k of `seq` is (point, is_corner).
    let mut seq: Vec<(Point, bool)> = Vec::with_capacity(2 * n);
    for (i, &is_sharp) in sharp.iter().enumerate() {
        if is_sharp {
            seq.push((p(i), true));
        }
        seq.push((p(i).lerp(p(i + 1), 0.5), false));
    }
    let m = seq.len();
    let corner_at: Vec<usize> = (0..m).filter(|&k| seq[k].1).collect();
    let tolerance = options.tolerance;
    if corner_at.is_empty() {
        // Smooth all round: split at two opposite points, sharing tangents.
        if m < 4 {
            let pts: Vec<Point> = seq.iter().map(|s| s.0).collect();
            path.move_to(pts[0]);
            for &q in &pts[1..] {
                path.line_to(q);
            }
            path.close();
            return;
        }
        let half = m / 2;
        let tangent = |k: usize| (seq[(k + 1) % m].0 - seq[(k + m - 1) % m].0).normalize();
        let (t0, th) = (tangent(0), tangent(half));
        let first: Vec<Point> = (0..=half).map(|k| seq[k].0).collect();
        let second: Vec<Point> = (half..=m).map(|k| seq[k % m].0).collect();
        path.move_to(first[0]);
        emit(path, &first, t0, -th, tolerance);
        emit(path, &second, th, -t0, tolerance);
        path.close();
        return;
    }
    // Runs between consecutive corners, starting at the first corner.
    let start = corner_at[0];
    path.move_to(seq[start].0);
    for (j, &c) in corner_at.iter().enumerate() {
        let next = corner_at.get(j + 1).copied().unwrap_or(start + m);
        let run: Vec<Point> = (c..=next).map(|k| seq[k % m].0).collect();
        let t_start = (run[1] - run[0]).normalize();
        let t_end = (run[run.len() - 2] - run[run.len() - 1]).normalize();
        emit(path, &run, t_start, t_end, tolerance);
    }
    path.close();
}

/// Fit `points` (the first already the path's current point) and append the
/// curves; a two-point run is a straight line.
fn emit(path: &mut Path, points: &[Point], t_start: Point, t_end: Point, tolerance: f64) {
    if points.len() == 2 || collinear(points) {
        path.line_to(points[points.len() - 1]);
        return;
    }
    for [_, c1, c2, p] in fit_cubic(points, t_start, t_end, tolerance * tolerance) {
        path.curve_to(c1, c2, p);
    }
}

fn collinear(points: &[Point]) -> bool {
    let (a, b) = (points[0], points[points.len() - 1]);
    let d = b - a;
    let len = d.length();
    len > 0.0
        && points
            .iter()
            .all(|&q| ((q - a).cross(d) / len).abs() < 1e-9 && (q - a).dot(d) >= -1e-9)
}

type Bezier = [Point; 4];

/// Schneider's recursive fit of `d` with end tangents `t1` (leaving the
/// start) and `t2` (leaving the end, pointing back), within `error` (squared
/// distance).
fn fit_cubic(d: &[Point], t1: Point, t2: Point, error: f64) -> Vec<Bezier> {
    let n = d.len();
    if n == 2 {
        let dist = d[0].distance(d[1]) / 3.0;
        return vec![[d[0], d[0] + t1 * dist, d[1] + t2 * dist, d[1]]];
    }
    let mut u = chord_length(d);
    let mut bez = generate(d, &u, t1, t2);
    let (mut max, mut split) = max_error(d, &bez, &u);
    if max < error {
        return vec![bez];
    }
    if max < error * 4.0 {
        for _ in 0..4 {
            u = reparameterize(d, &u, &bez);
            bez = generate(d, &u, t1, t2);
            (max, split) = max_error(d, &bez, &u);
            if max < error {
                return vec![bez];
            }
        }
    }
    let split = split.clamp(1, n - 2);
    let centre = (d[split - 1] - d[split + 1]).normalize();
    let centre = if centre == Point::ZERO {
        (d[split - 1] - d[split]).normalize()
    } else {
        centre
    };
    let mut left = fit_cubic(&d[..=split], t1, centre, error);
    left.extend(fit_cubic(&d[split..], -centre, t2, error));
    left
}

fn chord_length(d: &[Point]) -> Vec<f64> {
    let mut u = Vec::with_capacity(d.len());
    u.push(0.0);
    for i in 1..d.len() {
        let prev = u[i - 1];
        u.push(prev + d[i].distance(d[i - 1]));
    }
    let total = *u.last().unwrap_or(&0.0);
    if total > 0.0 {
        for v in &mut u {
            *v /= total;
        }
    }
    u
}

fn bernstein(t: f64) -> [f64; 4] {
    let s = 1.0 - t;
    [s * s * s, 3.0 * t * s * s, 3.0 * t * t * s, t * t * t]
}

fn eval(b: &Bezier, t: f64) -> Point {
    let k = bernstein(t);
    b[0] * k[0] + b[1] * k[1] + b[2] * k[2] + b[3] * k[3]
}

fn generate(d: &[Point], u: &[f64], t1: Point, t2: Point) -> Bezier {
    let (first, last) = (d[0], d[d.len() - 1]);
    let mut c = [[0.0f64; 2]; 2];
    let mut x = [0.0f64; 2];
    for (i, &ui) in u.iter().enumerate() {
        let b = bernstein(ui);
        let a1 = t1 * b[1];
        let a2 = t2 * b[2];
        c[0][0] += a1.dot(a1);
        c[0][1] += a1.dot(a2);
        c[1][1] += a2.dot(a2);
        let tmp = d[i] - (first * (b[0] + b[1]) + last * (b[2] + b[3]));
        x[0] += a1.dot(tmp);
        x[1] += a2.dot(tmp);
    }
    c[1][0] = c[0][1];
    let det = c[0][0] * c[1][1] - c[1][0] * c[0][1];
    let (mut alpha1, mut alpha2) = if det.abs() > 1e-12 {
        (
            (x[0] * c[1][1] - x[1] * c[0][1]) / det,
            (c[0][0] * x[1] - c[1][0] * x[0]) / det,
        )
    } else {
        (0.0, 0.0)
    };
    let seg = first.distance(last);
    let eps = 1e-6 * seg;
    if alpha1 < eps || alpha2 < eps {
        alpha1 = seg / 3.0;
        alpha2 = seg / 3.0;
    }
    [first, first + t1 * alpha1, last + t2 * alpha2, last]
}

fn max_error(d: &[Point], b: &Bezier, u: &[f64]) -> (f64, usize) {
    let mut max = 0.0;
    let mut split = d.len() / 2;
    for i in 1..d.len() - 1 {
        let dist = eval(b, u[i]).distance_squared(d[i]);
        if dist >= max {
            max = dist;
            split = i;
        }
    }
    (max, split)
}

fn reparameterize(d: &[Point], u: &[f64], b: &Bezier) -> Vec<f64> {
    let d1 = [
        (b[1] - b[0]) * 3.0,
        (b[2] - b[1]) * 3.0,
        (b[3] - b[2]) * 3.0,
    ];
    let d2 = [(d1[1] - d1[0]) * 2.0, (d1[2] - d1[1]) * 2.0];
    d.iter()
        .zip(u)
        .map(|(&p, &t)| {
            let q = eval(b, t);
            let s = 1.0 - t;
            let q1 = d1[0] * (s * s) + d1[1] * (2.0 * t * s) + d1[2] * (t * t);
            let q2 = d2[0] * s + d2[1] * t;
            let num = (q - p).dot(q1);
            let den = q1.dot(q1) + (q - p).dot(q2);
            if den.abs() < 1e-12 {
                t
            } else {
                (t - num / den).clamp(0.0, 1.0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{fill, FillOptions};
    use crate::mask::PixelRect;

    /// A `w x h` image, `inner` colour inside a centred disc of radius `r`,
    /// `outer` elsewhere.
    fn disc(w: u32, h: u32, r: f64, inner: [u8; 4], outer: [u8; 4]) -> Vec<u8> {
        let (cx, cy) = (f64::from(w) / 2.0, f64::from(h) / 2.0);
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .flat_map(|(x, y)| {
                let (dx, dy) = (f64::from(x) + 0.5 - cx, f64::from(y) + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    inner
                } else {
                    outer
                }
            })
            .collect()
    }

    fn coverage(path: &Path, w: u32, h: u32) -> Vec<u8> {
        let opts = FillOptions {
            clip: Some(PixelRect::from_xywh(0, 0, w, h)),
            ..FillOptions::default()
        };
        let mask = fill(path, &opts).unwrap();
        let mut out = vec![0u8; (w * h) as usize];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                out[(y as u32 * w + x as u32) as usize] = mask.coverage_at(glam::IVec2::new(x, y));
            }
        }
        out
    }

    #[test]
    fn a_two_colour_image_is_two_layers_whose_union_covers_the_image() {
        let (w, h) = (40u32, 30u32);
        let red = [220, 30, 30, 255];
        let blue = [20, 40, 200, 255];
        let rgba = disc(w, h, 9.0, red, blue);
        let layers = trace(&rgba, w, h, &TraceOptions::default()).unwrap();
        assert_eq!(layers.len(), 2, "one layer per colour");
        // Bottom first: the larger area (blue) is the base.
        assert_eq!(layers[0].color, blue);
        assert_eq!(layers[1].color, red);
        let base = coverage(&layers[0].path, w, h);
        let top = coverage(&layers[1].path, w, h);
        // Union coverage: every pixel of the image is fully covered.
        for (i, (&a, &b)) in base.iter().zip(&top).enumerate() {
            assert!(a.max(b) >= 250, "pixel {i} is covered only {}", a.max(b));
        }
        // And the stack paints the right colour almost everywhere: the top
        // layer's smoothed disc disagrees with the pixels only along its rim.
        let wrong = (0..(w * h) as usize)
            .filter(|&i| {
                let painted = if top[i] >= 128 { red } else { blue };
                painted[..] != rgba[i * 4..i * 4 + 4]
            })
            .count();
        assert!(
            wrong * 100 < (w * h) as usize * 4,
            "{wrong} pixels mispainted"
        );
        // The disc is fitted with curves, not a staircase of lines.
        assert!(layers[1]
            .path
            .elements()
            .iter()
            .any(|e| matches!(e, crate::path::PathEl::CurveTo(..))));
    }

    #[test]
    fn a_rectangle_keeps_its_corners_as_straight_edges() {
        let (w, h) = (20u32, 10u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for y in 2..8 {
            for x in 3..15 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[10, 200, 10, 255]);
            }
        }
        let layers = trace(&rgba, w, h, &TraceOptions::default()).unwrap();
        assert_eq!(layers.len(), 1, "transparent pixels are not a colour");
        let b = layers[0].path.bounds();
        assert_eq!((b.min.x, b.min.y, b.max.x, b.max.y), (3.0, 2.0, 15.0, 8.0));
        let cov = coverage(&layers[0].path, w, h);
        for y in 0..h {
            for x in 0..w {
                let want = (3..15).contains(&x) && (2..8).contains(&y);
                assert_eq!(cov[(y * w + x) as usize] >= 250, want, "({x},{y})");
            }
        }
    }

    #[test]
    fn a_hole_traces_with_the_opposite_orientation_and_stays_empty() {
        let mask: Vec<bool> = (0..25)
            .map(|i| {
                let (x, y) = (i % 5, i / 5);
                !(x == 2 && y == 2)
            })
            .collect();
        let contours = trace_mask(&mask, 5, 5);
        assert_eq!(contours.len(), 2, "outer contour and one hole");
        let area = |c: &Vec<(i64, i64)>| {
            let n = c.len();
            (0..n)
                .map(|i| c[i].0 * c[(i + 1) % n].1 - c[(i + 1) % n].0 * c[i].1)
                .sum::<i64>()
        };
        assert_eq!(area(&contours[0]).signum(), -area(&contours[1]).signum());
        assert_eq!(
            area(&contours[0]).abs() + area(&contours[1]).abs(),
            2 * (25 + 1)
        );
    }

    #[test]
    fn posterize_reduces_to_the_asked_count_and_keeps_few_colours_exact() {
        let rgba: Vec<u8> = (0..256u32)
            .flat_map(|i| [i as u8, (255 - i) as u8, 128, 255])
            .collect();
        let (palette, index) = posterize(&rgba, 4);
        assert_eq!(palette.len(), 4);
        assert!(index.iter().all(|&i| (i as usize) < 4));
        let two = [[1u8, 2, 3, 255], [9, 8, 7, 255]].concat();
        assert_eq!(posterize(&two, 8).0, vec![[1, 2, 3], [9, 8, 7]]);
    }

    #[test]
    fn bad_input_is_refused_not_panicked_on() {
        let opts = TraceOptions::default();
        assert!(trace(&[], 0, 0, &opts).is_err());
        assert!(trace(&[0; 8], 1, 1, &opts).is_err());
        let zero = TraceOptions { colors: 0, ..opts };
        assert!(trace(&[0; 4], 1, 1, &zero).is_err());
        let nan = TraceOptions {
            tolerance: f64::NAN,
            ..opts
        };
        assert!(trace(&[0; 4], 1, 1, &nan).is_err());
        // A fully transparent image traces to nothing.
        assert!(trace(&[0; 16], 2, 2, &opts).unwrap().is_empty());
    }
}
