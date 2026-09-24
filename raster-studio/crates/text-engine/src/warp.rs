//! W9-K: Warp Text and Type on a Path, applied to glyph **outlines**.
//!
//! Layout never moves: [`crate::shape`] positions every glyph exactly as it
//! would without a warp, so the caret, the hit test and the selection keep
//! answering in the layer's unwarped space. What changes is the rasteriser:
//! when a [`ShapedText`] carries an active [`TextWarp`] or a [`TextPath`],
//! each glyph's outline is fetched from the font, flattened, pushed through
//! the [`Distortion`] point map and filled by a small anti-aliased scanline
//! rasteriser here, instead of blitting the pre-rendered glyph bitmap.
//!
//! The same outlines, mapped the same way, are what [`outline_svg`] hands a
//! shape layer for Layer > Text > Convert to Shape - so the converted shape
//! covers the pixels the text drew.
//!
//! # The two maps
//!
//! * A **warp** is an envelope over the text block's line-box bounds, in the
//!   Photoshop style set ([`WarpStyle`]). `bend` scales the envelope; the
//!   horizontal and vertical distortions are applied first as a simple
//!   perspective-like taper. The Arc style is a true circular arc: glyph
//!   centres land on a circle, rotated to its tangent.
//! * A **path** is a polyline in layer space. A point `s` pixels right of the
//!   block's left edge and `d` pixels above the first baseline maps to the
//!   point `start + s` along the path, `d` pixels out along its left normal.
//!   Past the end of an open path the last segment is extended; a closed path
//!   wraps.
//!
//! # Limits
//!
//! Synthetic bold is not applied on this route (the outline is the face's
//! own), and anti-alias "None" thresholds the coverage like the bitmap route.

use std::f32::consts::{FRAC_PI_2, PI};

use cosmic_text::{CacheKey, CacheKeyFlags, Command, SwashCache, Weight};
use layer_model::text::{TextPath, TextWarp, WarpStyle};

use crate::font::FontLibrary;
use crate::layout::{Rect, ShapedGlyph, ShapedText};

/// Flattening tolerance for glyph curves, in layer pixels.
const FLATTEN_TOLERANCE: f32 = 0.1;
/// Vertical sub-scanlines per pixel row in [`fill_polygons`].
const SUBSAMPLES: usize = 4;
/// Longest edge (in pixels) a straight outline edge may keep before it is
/// subdivided, so a straight stroke of a glyph bends with the envelope.
const MAX_EDGE_PX: f32 = 2.0;

/// The point map a warped or path-bound text applies to its outlines.
#[derive(Debug, Clone)]
pub enum Distortion {
    /// A Warp Text envelope over `bounds`.
    Warp {
        /// The warp parameters, clamped to `-1..=1`.
        warp: TextWarp,
        /// The text block's line-box bounds the envelope is fitted to.
        bounds: Rect,
    },
    /// Type on a Path.
    Path(PathMap),
}

impl Distortion {
    /// The distortion a shaped text asks for, or `None` when it renders flat.
    ///
    /// A path wins over a warp: Photoshop offers no warp on path type either.
    #[must_use]
    pub fn of(text: &ShapedText) -> Option<Self> {
        if let Some(path) = &text.path {
            let baseline = text.lines.first().map_or(0.0, |l| l.baseline_y);
            return PathMap::new(path, text.bounds.x, baseline).map(Self::Path);
        }
        if !text.warp.is_active() {
            return None;
        }
        let bounds = text.bounds;
        if !(bounds.width > 0.0 && bounds.height > 0.0) {
            return None;
        }
        let clamp = |v: f32| {
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        let warp = TextWarp {
            style: text.warp.style,
            bend: clamp(text.warp.bend),
            horizontal: clamp(text.warp.horizontal),
            vertical: clamp(text.warp.vertical),
        };
        Some(Self::Warp { warp, bounds })
    }

    /// Map one layer-space point.
    #[must_use]
    pub fn map(&self, x: f32, y: f32) -> [f32; 2] {
        match self {
            Self::Warp { warp, bounds } => warp_point(warp, *bounds, x, y),
            Self::Path(path) => path.map(x, y),
        }
    }
}

/// Map `(x, y)` through a warp envelope fitted to `bounds`.
///
/// Public so a test (or a UI preview) can ask where a point lands without
/// rasterising anything.
#[must_use]
pub fn warp_point(warp: &TextWarp, bounds: Rect, x: f32, y: f32) -> [f32; 2] {
    let hw = (bounds.width * 0.5).max(1e-3);
    let hh = (bounds.height * 0.5).max(1e-3);
    let cx = bounds.x + hw;
    let cy = bounds.y + hh;
    let mut u = (x - cx) / hw;
    let mut v = (y - cy) / hh;
    // Distortions first: a taper, like the dialog's perspective sliders.
    v *= 1.0 + warp.horizontal * u * 0.5;
    u *= 1.0 - warp.vertical * v * 0.5;
    let b = warp.bend;
    let (u, v) = match warp.style {
        WarpStyle::None => (u, v),
        WarpStyle::Arc => return arc(cx, cy, hw, u, v * hh, b),
        WarpStyle::ArcLower => (u, v + b * (1.0 - u * u) * (v + 1.0) * 0.5),
        WarpStyle::ArcUpper => (u, v - b * (1.0 - u * u) * (1.0 - v) * 0.5),
        WarpStyle::Arch => (u, v - b * (1.0 - u * u)),
        WarpStyle::Bulge => (u, v * (1.0 + b * (1.0 - u * u))),
        WarpStyle::Flag => (u, v - b * 0.5 * (PI * u).sin()),
        WarpStyle::Wave => (
            u,
            v - b * 0.5 * (PI * u + (v + 1.0) * FRAC_PI_2 * 0.5).sin(),
        ),
        WarpStyle::Fish => (u, v * (1.0 + b * 0.6 * (PI * u).sin())),
        WarpStyle::Rise => (u, v - b * 0.5 * (FRAC_PI_2 * u).sin()),
        WarpStyle::Fisheye => {
            let s = 1.0 + b * 0.5 * (1.0 - (u * u + v * v) * 0.5).max(0.0);
            (u * s, v * s)
        }
        WarpStyle::Inflate => (
            u * (1.0 + b * 0.5 * (1.0 - v * v)),
            v * (1.0 + b * 0.5 * (1.0 - u * u)),
        ),
        WarpStyle::Squeeze => (
            u * (1.0 - b * 0.5 * (1.0 - v * v)),
            v * (1.0 + b * 0.5 * (1.0 - u * u)),
        ),
        WarpStyle::Twist => {
            let r = ((u * u + v * v) * 0.5).sqrt().min(1.0);
            let angle = b * FRAC_PI_2 * (1.0 - r);
            let (px, py) = (u * hw, v * hh);
            let (sin, cos) = angle.sin_cos();
            return [cx + px * cos - py * sin, cy + px * sin + py * cos];
        }
    };
    [cx + u * hw, cy + v * hh]
}

/// The Arc style: the block's horizontal centre line becomes a circular arc
/// of half-angle `bend * 90°`, bulging up for a positive bend. `dy` is the
/// point's pixel offset below the block centre.
fn arc(cx: f32, cy: f32, hw: f32, u: f32, dy: f32, bend: f32) -> [f32; 2] {
    let phi = bend * FRAC_PI_2;
    if phi.abs() < 1e-4 {
        return [cx + u * hw, cy + dy];
    }
    // Signed radius: the centre sits below the block for an upward bulge
    // and above it for a downward one; one formula covers both.
    let radius = hw / phi;
    let theta = u * phi;
    let r = radius - dy;
    [cx + r * theta.sin(), cy + radius - r * theta.cos()]
}

/// Type on a Path: arc-length parametrisation of a polyline.
#[derive(Debug, Clone)]
pub struct PathMap {
    points: Vec<[f32; 2]>,
    /// Cumulative length at each vertex (and at the closing vertex when
    /// closed): `lengths[i]` is the arc length at `points[i]`.
    lengths: Vec<f32>,
    closed: bool,
    start: f32,
    /// Layer x of the text block's left edge (arc length `start`).
    left: f32,
    /// Layer y of the first baseline (distance zero from the path).
    baseline: f32,
}

impl PathMap {
    /// Build the map; `None` for a path with no length.
    #[must_use]
    pub fn new(path: &TextPath, left: f32, baseline: f32) -> Option<Self> {
        let mut points: Vec<[f32; 2]> = path
            .points
            .iter()
            .copied()
            .filter(|p| p[0].is_finite() && p[1].is_finite())
            .collect();
        points.dedup();
        if path.closed && points.len() > 2 {
            let first = points[0];
            points.push(first);
        }
        if points.len() < 2 {
            return None;
        }
        let mut lengths = Vec::with_capacity(points.len());
        let mut total = 0.0;
        lengths.push(0.0);
        for pair in points.windows(2) {
            total += dist(pair[0], pair[1]);
            lengths.push(total);
        }
        (total > 0.0).then(|| Self {
            points,
            lengths,
            closed: path.closed,
            start: if path.start.is_finite() {
                path.start
            } else {
                0.0
            },
            left: if left.is_finite() { left } else { 0.0 },
            baseline: if baseline.is_finite() { baseline } else { 0.0 },
        })
    }

    /// The path's total length.
    #[must_use]
    pub fn length(&self) -> f32 {
        self.lengths.last().copied().unwrap_or(0.0)
    }

    /// The point and unit tangent `s` along the path.
    #[must_use]
    pub fn at(&self, s: f32) -> ([f32; 2], [f32; 2]) {
        let total = self.length();
        let s = if self.closed { s.rem_euclid(total) } else { s };
        // The segment holding `s`; before the start or past the end of an
        // open path, the first or last segment is extended.
        let seg = match self.lengths.iter().position(|&l| l > s) {
            Some(0) => 0,
            Some(i) => i - 1,
            None => self.points.len() - 2,
        };
        let (a, b) = (self.points[seg], self.points[seg + 1]);
        let len = (self.lengths[seg + 1] - self.lengths[seg]).max(1e-6);
        let t = (s - self.lengths[seg]) / len;
        let tangent = [(b[0] - a[0]) / len, (b[1] - a[1]) / len];
        (
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t],
            tangent,
        )
    }

    /// Map a layer point: its distance right of the block's left edge runs
    /// along the path, its height above the first baseline runs out along
    /// the path's left normal (up, for a path drawn left to right).
    #[must_use]
    pub fn map(&self, x: f32, y: f32) -> [f32; 2] {
        let s = self.start + (x - self.left);
        let d = self.baseline - y;
        let (p, t) = self.at(s);
        // y grows downwards, so the "up" normal of tangent (tx, ty) is
        // (ty, -tx).
        [p[0] + t[1] * d, p[1] - t[0] * d]
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt()
}

/// One glyph's outline commands (y up, in pixels at the glyph's size,
/// relative to its pen origin), or `None` for a glyph with no outline.
pub(crate) fn glyph_commands(
    library: &mut FontLibrary,
    swash: &mut SwashCache,
    glyph: &ShapedGlyph,
) -> Option<Box<[Command]>> {
    let flags = if glyph.synthetic_italic {
        CacheKeyFlags::FAKE_ITALIC
    } else {
        CacheKeyFlags::empty()
    };
    let (key, _, _) = CacheKey::new(
        glyph.font.0,
        glyph.glyph_id,
        glyph.size_px,
        (0.0, 0.0),
        Weight(glyph.weight.0),
        flags,
    );
    swash
        .get_outline_commands(library.system_mut(), key)
        .map(Into::into)
}

/// A glyph's contours in layer space, curves flattened, each point pushed
/// through `map` (identity when `None`).
pub(crate) fn glyph_contours(
    commands: &[Command],
    glyph: &ShapedGlyph,
    map: Option<&Distortion>,
) -> Vec<Vec<[f32; 2]>> {
    // The baseline snaps to the pixel grid exactly as the bitmap route's
    // does (`GlyphRasterCache::glyph_image` floors the pen's y), so an
    // outline lands on the pixels the flat text drew.
    let baseline = glyph.draw_y.floor();
    let place = |x: f32, y: f32| -> [f32; 2] {
        [
            glyph.draw_x + x * glyph.scale_x,
            baseline - y * glyph.scale_y,
        ]
    };
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut current: Vec<[f32; 2]> = Vec::new();
    let mut last = [0.0f32; 2];
    let mut first = [0.0f32; 2];
    let finish = |current: &mut Vec<[f32; 2]>, contours: &mut Vec<Vec<[f32; 2]>>| {
        if current.len() >= 3 {
            contours.push(std::mem::take(current));
        } else {
            current.clear();
        }
    };
    for command in commands {
        match *command {
            Command::MoveTo(p) => {
                finish(&mut current, &mut contours);
                last = place(p.x, p.y);
                first = last;
                current.push(last);
            }
            Command::LineTo(p) => {
                let q = place(p.x, p.y);
                line_into(&mut current, last, q, map.is_some());
                last = q;
            }
            Command::QuadTo(c, p) => {
                let c = place(c.x, c.y);
                let q = place(p.x, p.y);
                let n = segments_for(dist(last, c) + dist(c, q));
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let mt = 1.0 - t;
                    current.push([
                        mt * mt * last[0] + 2.0 * mt * t * c[0] + t * t * q[0],
                        mt * mt * last[1] + 2.0 * mt * t * c[1] + t * t * q[1],
                    ]);
                }
                last = q;
            }
            Command::CurveTo(c1, c2, p) => {
                let c1 = place(c1.x, c1.y);
                let c2 = place(c2.x, c2.y);
                let q = place(p.x, p.y);
                let n = segments_for(dist(last, c1) + dist(c1, c2) + dist(c2, q));
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let mt = 1.0 - t;
                    let (a, b, c, d) =
                        (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                    current.push([
                        a * last[0] + b * c1[0] + c * c2[0] + d * q[0],
                        a * last[1] + b * c1[1] + c * c2[1] + d * q[1],
                    ]);
                }
                last = q;
            }
            Command::Close => {
                line_into(&mut current, last, first, map.is_some());
                last = first;
                finish(&mut current, &mut contours);
            }
        }
    }
    finish(&mut current, &mut contours);
    if let Some(map) = map {
        for contour in &mut contours {
            for p in contour.iter_mut() {
                *p = map.map(p[0], p[1]);
            }
        }
    }
    contours
}

/// Segments to flatten a curve whose control polygon is `length` long.
fn segments_for(length: f32) -> usize {
    let n = (length / (FLATTEN_TOLERANCE * 40.0)).ceil();
    if n.is_finite() {
        (n as usize).clamp(2, 64)
    } else {
        2
    }
}

/// Append the straight edge to `q`, subdivided when it will be bent.
fn line_into(current: &mut Vec<[f32; 2]>, from: [f32; 2], q: [f32; 2], bends: bool) {
    let steps = if bends {
        let n = (dist(from, q) / MAX_EDGE_PX).ceil();
        if n.is_finite() {
            (n as usize).clamp(1, 256)
        } else {
            1
        }
    } else {
        1
    };
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        current.push([
            from[0] + (q[0] - from[0]) * t,
            from[1] + (q[1] - from[1]) * t,
        ]);
    }
}

/// A rectangle as a contour, its edges subdivided so a map bends it.
pub(crate) fn rect_contour(rect: &Rect, map: &Distortion) -> Vec<[f32; 2]> {
    let corners = [
        [rect.x, rect.y],
        [rect.right(), rect.y],
        [rect.right(), rect.bottom()],
        [rect.x, rect.bottom()],
    ];
    let mut out = vec![corners[0]];
    for i in 0..4 {
        line_into(&mut out, corners[i], corners[(i + 1) % 4], true);
    }
    out.pop();
    for p in &mut out {
        *p = map.map(p[0], p[1]);
    }
    out
}

/// Nonzero-winding, anti-aliased fill of `contours` into a coverage bitmap.
///
/// Returns `(left, top, width, height, data)` in layer pixels, or `None` when
/// there is nothing to fill.
pub(crate) fn fill_polygons(contours: &[Vec<[f32; 2]>]) -> Option<(i32, i32, u32, u32, Vec<u8>)> {
    const LIMIT: f32 = 1.0e7;
    let mut min = [f32::MAX; 2];
    let mut max = [f32::MIN; 2];
    let mut edges: Vec<([f32; 2], [f32; 2])> = Vec::new();
    for contour in contours {
        if contour.len() < 3 {
            continue;
        }
        for i in 0..contour.len() {
            let a = contour[i];
            let b = contour[(i + 1) % contour.len()];
            if !(a.iter().chain(&b).all(|v| v.is_finite() && v.abs() < LIMIT)) {
                continue;
            }
            for p in [a, b] {
                min[0] = min[0].min(p[0]);
                min[1] = min[1].min(p[1]);
                max[0] = max[0].max(p[0]);
                max[1] = max[1].max(p[1]);
            }
            if a[1] != b[1] {
                edges.push((a, b));
            }
        }
    }
    if edges.is_empty() {
        return None;
    }
    let left = min[0].floor() as i32;
    let top = min[1].floor() as i32;
    let width = (max[0].ceil() as i32 - left).max(1) as u32 + 1;
    let height = (max[1].ceil() as i32 - top).max(1) as u32 + 1;
    // A single glyph never needs more than this; a corrupt map could.
    if u64::from(width) * u64::from(height) > 64 << 20 {
        return None;
    }
    let mut acc = vec![0.0f32; (width * height) as usize];
    let mut crossings: Vec<(f32, i32)> = Vec::new();
    for row in 0..height {
        for sub in 0..SUBSAMPLES {
            let y = top as f32 + row as f32 + (sub as f32 + 0.5) / SUBSAMPLES as f32;
            crossings.clear();
            for (a, b) in &edges {
                let (lo, hi, dir) = if a[1] < b[1] { (a, b, 1) } else { (b, a, -1) };
                if y < lo[1] || y >= hi[1] {
                    continue;
                }
                let t = (y - lo[1]) / (hi[1] - lo[1]);
                crossings.push((lo[0] + (hi[0] - lo[0]) * t, dir));
            }
            crossings.sort_by(|p, q| p.0.total_cmp(&q.0));
            let mut winding = 0;
            let base = (row * width) as usize;
            for pair in crossings.windows(2) {
                winding += pair[0].1;
                if winding == 0 {
                    continue;
                }
                let x0 = pair[0].0 - left as f32;
                let x1 = pair[1].0 - left as f32;
                span(&mut acc[base..base + width as usize], x0, x1);
            }
        }
    }
    let data = acc
        .iter()
        .map(|v| (v / SUBSAMPLES as f32 * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect();
    Some((left, top, width, height, data))
}

/// Add horizontal coverage `x0..x1` (pixel units within the row) to `row`.
fn span(row: &mut [f32], x0: f32, x1: f32) {
    if x1 <= x0 {
        return;
    }
    let width = row.len() as f32;
    let x0 = x0.clamp(0.0, width);
    let x1 = x1.clamp(0.0, width);
    let first = x0.floor() as usize;
    let last = (x1.ceil() as usize).min(row.len());
    for (col, cell) in row.iter_mut().enumerate().take(last).skip(first) {
        let lo = x0.max(col as f32);
        let hi = x1.min(col as f32 + 1.0);
        if hi > lo {
            *cell += hi - lo;
        }
    }
}

/// W9-K: Layer > Text > Convert to Shape - every glyph outline of `text`,
/// in layer space and through the text's warp or path, as SVG path data a
/// shape layer can fill (nonzero).
///
/// Unwarped glyphs keep their quadratic and cubic curves exactly (an affine
/// placement of a curve is a curve); warped or path-bound glyphs are
/// flattened first, since a bent curve is no longer a Bezier. Empty for a
/// text with no outlines at all.
#[must_use]
pub fn outline_svg(library: &mut FontLibrary, text: &ShapedText) -> String {
    use std::fmt::Write as _;
    let distortion = Distortion::of(text);
    let mut swash = SwashCache::new();
    let mut out = String::new();
    let fmt = |v: f32| -> String {
        let r = (v * 1000.0).round() / 1000.0;
        if r == 0.0 {
            "0".to_string()
        } else {
            format!("{r}")
        }
    };
    for glyph in &text.glyphs {
        let Some(commands) = glyph_commands(library, &mut swash, glyph) else {
            continue;
        };
        if let Some(map) = &distortion {
            for contour in glyph_contours(&commands, glyph, Some(map)) {
                for (i, p) in contour.iter().enumerate() {
                    let op = if i == 0 { 'M' } else { 'L' };
                    let _ = write!(out, "{op}{} {} ", fmt(p[0]), fmt(p[1]));
                }
                out.push_str("Z ");
            }
            continue;
        }
        // The bitmap route's snapped baseline, as in `glyph_contours`.
        let baseline = glyph.draw_y.floor();
        let place = |x: f32, y: f32| {
            (
                fmt(glyph.draw_x + x * glyph.scale_x),
                fmt(baseline - y * glyph.scale_y),
            )
        };
        for command in commands.iter() {
            match *command {
                Command::MoveTo(p) => {
                    let (x, y) = place(p.x, p.y);
                    let _ = write!(out, "M{x} {y} ");
                }
                Command::LineTo(p) => {
                    let (x, y) = place(p.x, p.y);
                    let _ = write!(out, "L{x} {y} ");
                }
                Command::QuadTo(c, p) => {
                    let (cx, cy) = place(c.x, c.y);
                    let (x, y) = place(p.x, p.y);
                    let _ = write!(out, "Q{cx} {cy} {x} {y} ");
                }
                Command::CurveTo(c1, c2, p) => {
                    let (ax, ay) = place(c1.x, c1.y);
                    let (bx, by) = place(c2.x, c2.y);
                    let (x, y) = place(p.x, p.y);
                    let _ = write!(out, "C{ax} {ay} {bx} {by} {x} {y} ");
                }
                Command::Close => out.push_str("Z "),
            }
        }
    }
    out.truncate(out.trim_end().len());
    out
}
