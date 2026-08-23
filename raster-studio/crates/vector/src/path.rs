//! The path model: subpaths of lines and Bezier curves, open or closed.
//!
//! # Why elements rather than a tree of subpaths
//! A path is stored as a flat list of [`PathEl`] drawing commands, the same
//! shape SVG path data has. Every other representation — a `Vec<SubPath>` of
//! `Vec<Segment>` — has to answer "what does an element after `Z` mean?" with a
//! special case, and has to invent a canonical form the moment you append one
//! path to another. The flat list has one rule instead: `MoveTo` starts a
//! subpath, `ClosePath` finishes one, and everything else extends the current
//! one. [`Path::subpaths`] derives the structured view on demand, so nothing
//! else in the crate has to re-implement those rules.
//!
//! # No path can be in an invalid state
//! Drawing to a path with no current point is not an error and not a panic: it
//! implicitly opens a subpath at that point, exactly as if `move_to` had been
//! called. A `d` string is caller input and so is the pen tool, and neither
//! should be able to produce a path object that later operations have to guard
//! against.

use serde::{Deserialize, Serialize};

use crate::affine::Affine;
use crate::point::{point, Bounds, Point};
use crate::segment::Segment;

/// One drawing command.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum PathEl {
    /// Start a new subpath at this point.
    MoveTo(Point),
    /// Straight line from the current point.
    LineTo(Point),
    /// Quadratic Bezier: control, end.
    QuadTo(Point, Point),
    /// Cubic Bezier: two controls, end.
    CurveTo(Point, Point, Point),
    /// Close the current subpath back to its start.
    ClosePath,
}

impl PathEl {
    /// The point this command ends at, if it has one.
    pub fn end_point(&self) -> Option<Point> {
        match *self {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => Some(p),
            PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => Some(p),
            PathEl::ClosePath => None,
        }
    }

    /// A copy with every point transformed.
    pub fn transform(&self, t: &Affine) -> PathEl {
        match *self {
            PathEl::MoveTo(p) => PathEl::MoveTo(t.apply(p)),
            PathEl::LineTo(p) => PathEl::LineTo(t.apply(p)),
            PathEl::QuadTo(c, p) => PathEl::QuadTo(t.apply(c), t.apply(p)),
            PathEl::CurveTo(a, b, p) => PathEl::CurveTo(t.apply(a), t.apply(b), t.apply(p)),
            PathEl::ClosePath => PathEl::ClosePath,
        }
    }
}

/// One connected run of segments, derived from a [`Path`].
///
/// A closed subpath's `segments` already include the closing line back to
/// `start`, so a ring is a ring with no special case at the end of the loop.
#[derive(Debug, Clone, PartialEq)]
pub struct SubPath {
    /// Where the run begins.
    pub start: Point,
    /// The segments, in order.
    pub segments: Vec<Segment>,
    /// Whether it was explicitly closed.
    pub closed: bool,
}

impl SubPath {
    /// The last point, which equals `start` when closed.
    pub fn end(&self) -> Point {
        self.segments.last().map_or(self.start, |s| s.end())
    }

    /// `true` when this subpath draws nothing at all.
    pub fn is_degenerate(&self) -> bool {
        self.segments.is_empty()
            || self.segments.iter().all(|s| {
                s.start().distance_squared(s.end()) == 0.0 && matches!(s, Segment::Line(..))
            })
    }
}

/// A flattened subpath: a run of straight segments through `points`.
#[derive(Debug, Clone, PartialEq)]
pub struct Polyline {
    /// Vertices in order. A closed polyline does **not** repeat its first point
    /// at the end; the closing edge is implied.
    pub points: Vec<Point>,
    /// Whether the last vertex connects back to the first.
    pub closed: bool,
}

impl Polyline {
    /// Total length along the vertices, including the closing edge when closed.
    pub fn length(&self) -> f64 {
        let mut total: f64 = self.points.windows(2).map(|w| w[0].distance(w[1])).sum();
        if self.closed && self.points.len() > 1 {
            total += self.points[self.points.len() - 1].distance(self.points[0]);
        }
        total
    }

    /// Twice the signed area enclosed, by the shoelace formula. Positive for a
    /// counter-clockwise ring in a y-up frame.
    pub fn signed_area2(&self) -> f64 {
        let n = self.points.len();
        if n < 3 {
            return 0.0;
        }
        let mut acc = 0.0;
        for i in 0..n {
            let a = self.points[i];
            let b = self.points[(i + 1) % n];
            acc += a.cross(b);
        }
        acc
    }

    /// The edges as `(from, to)` pairs, including the closing edge when closed.
    pub fn edges(&self) -> Vec<(Point, Point)> {
        let n = self.points.len();
        if n < 2 {
            return Vec::new();
        }
        let last = if self.closed { n } else { n - 1 };
        (0..last)
            .map(|i| (self.points[i], self.points[(i + 1) % n]))
            .collect()
    }
}

/// A sequence of subpaths made of lines and Bezier curves.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Path {
    els: Vec<PathEl>,
}

impl Path {
    /// An empty path.
    pub fn new() -> Self {
        Self { els: Vec::new() }
    }

    /// Adopt a list of drawing commands as-is.
    pub fn from_elements(els: Vec<PathEl>) -> Self {
        Self { els }
    }

    /// The drawing commands.
    pub fn elements(&self) -> &[PathEl] {
        &self.els
    }

    /// `true` when nothing has been drawn.
    pub fn is_empty(&self) -> bool {
        self.els.is_empty()
    }

    /// Append a raw command.
    pub fn push(&mut self, el: PathEl) -> &mut Self {
        self.els.push(el);
        self
    }

    /// Append every command of another path.
    pub fn extend(&mut self, other: &Path) -> &mut Self {
        self.els.extend_from_slice(&other.els);
        self
    }

    /// Start a new subpath.
    pub fn move_to(&mut self, p: Point) -> &mut Self {
        self.push(PathEl::MoveTo(p))
    }

    /// Straight line from the current point.
    pub fn line_to(&mut self, p: Point) -> &mut Self {
        self.ensure_current();
        self.push(PathEl::LineTo(p))
    }

    /// Quadratic curve from the current point.
    pub fn quad_to(&mut self, c: Point, p: Point) -> &mut Self {
        self.ensure_current();
        self.push(PathEl::QuadTo(c, p))
    }

    /// Cubic curve from the current point.
    pub fn curve_to(&mut self, c1: Point, c2: Point, p: Point) -> &mut Self {
        self.ensure_current();
        self.push(PathEl::CurveTo(c1, c2, p))
    }

    /// Close the current subpath.
    ///
    /// A no-op when there is no open subpath, so `close()` can be called
    /// defensively without producing a stray element.
    pub fn close(&mut self) -> &mut Self {
        if matches!(self.els.last(), None | Some(PathEl::ClosePath)) {
            return self;
        }
        self.push(PathEl::ClosePath)
    }

    /// A drawing command with no current point opens a subpath at the origin
    /// rather than being dropped or panicking.
    fn ensure_current(&mut self) {
        if self.current_point().is_none() {
            self.els.push(PathEl::MoveTo(Point::ZERO));
        }
    }

    /// Where the next command would start from.
    pub fn current_point(&self) -> Option<Point> {
        let mut cur = None;
        let mut sub_start = None;
        for el in &self.els {
            match *el {
                PathEl::MoveTo(p) => {
                    cur = Some(p);
                    sub_start = Some(p);
                }
                PathEl::LineTo(p) => cur = Some(p),
                PathEl::QuadTo(_, p) | PathEl::CurveTo(_, _, p) => cur = Some(p),
                PathEl::ClosePath => cur = sub_start,
            }
        }
        cur
    }

    /// An elliptical arc to `end`, in SVG's endpoint parameterisation.
    ///
    /// `x_rotation` is in **radians**, unlike the SVG attribute, which is in
    /// degrees; the parser converts. The arc is emitted as cubics — at most one
    /// per quarter turn, which keeps the maximum radial error under about
    /// 0.02% of the radius — because a path has no arc primitive to store.
    /// Every out-of-range input follows the SVG error rules: zero radii become
    /// a line, negative radii are taken as positive, and radii too small to
    /// span the endpoints are scaled up until they fit.
    pub fn arc_to(
        &mut self,
        rx: f64,
        ry: f64,
        x_rotation: f64,
        large_arc: bool,
        sweep: bool,
        end: Point,
    ) -> &mut Self {
        self.ensure_current();
        let p0 = self.current_point().unwrap_or(Point::ZERO);
        if !end.is_finite() || !rx.is_finite() || !ry.is_finite() || !x_rotation.is_finite() {
            return self;
        }
        if p0.distance_squared(end) == 0.0 {
            // SVG: an arc whose endpoints coincide is omitted entirely.
            return self;
        }
        let (rx, ry) = (rx.abs(), ry.abs());
        if rx == 0.0 || ry == 0.0 {
            return self.line_to(end);
        }
        for seg in arc_segments(p0, rx, ry, x_rotation, large_arc, sweep, end) {
            if let Segment::Cubic(_, c1, c2, p) = seg {
                self.push(PathEl::CurveTo(c1, c2, p));
            }
        }
        self
    }

    /// The structured view: one entry per subpath.
    ///
    /// A `ClosePath` ends its subpath and leaves only a *pending* start point
    /// behind, not an empty subpath. That distinction is load-bearing: a path
    /// of `n` closed rings has to report `n` subpaths, and a version that
    /// opened a stub after each `Z` reports `2n - 1` — which silently doubles
    /// the ring count of every boolean result and every stroke outline.
    pub fn subpaths(&self) -> Vec<SubPath> {
        let mut out: Vec<SubPath> = Vec::new();
        let mut cur: Option<SubPath> = None;
        // Where a drawing command after a `Z` restarts, per the SVG rule that
        // it begins a fresh subpath at the closed one's start.
        let mut pending: Option<Point> = None;

        for el in &self.els {
            match *el {
                PathEl::MoveTo(p) => {
                    if let Some(sp) = cur.take() {
                        out.push(sp);
                    }
                    pending = None;
                    cur = Some(new_sub(p));
                }
                PathEl::LineTo(p) => {
                    let sp = open_sub(&mut cur, &mut pending);
                    let from = sp.segments.last().map_or(sp.start, |s| s.end());
                    sp.segments.push(Segment::Line(from, p));
                }
                PathEl::QuadTo(c, p) => {
                    let sp = open_sub(&mut cur, &mut pending);
                    let from = sp.segments.last().map_or(sp.start, |s| s.end());
                    sp.segments.push(Segment::Quad(from, c, p));
                }
                PathEl::CurveTo(c1, c2, p) => {
                    let sp = open_sub(&mut cur, &mut pending);
                    let from = sp.segments.last().map_or(sp.start, |s| s.end());
                    sp.segments.push(Segment::Cubic(from, c1, c2, p));
                }
                PathEl::ClosePath => {
                    if let Some(mut sp) = cur.take() {
                        let from = sp.segments.last().map_or(sp.start, |s| s.end());
                        if from.distance_squared(sp.start) > 0.0 {
                            sp.segments.push(Segment::Line(from, sp.start));
                        }
                        sp.closed = true;
                        pending = Some(sp.start);
                        out.push(sp);
                    }
                }
            }
        }
        if let Some(sp) = cur {
            out.push(sp);
        }
        out
    }

    /// Every segment of every subpath, closing lines included.
    pub fn segments(&self) -> Vec<Segment> {
        self.subpaths()
            .into_iter()
            .flat_map(|s| s.segments)
            .collect()
    }

    /// A copy with every point transformed.
    pub fn transform(&self, t: &Affine) -> Path {
        Path {
            els: self.els.iter().map(|e| e.transform(t)).collect(),
        }
    }

    /// Box of every control point: cheap, conservative, larger than the ink.
    pub fn control_bounds(&self) -> Bounds {
        let mut b = Bounds::EMPTY;
        for el in &self.els {
            match *el {
                PathEl::MoveTo(p) | PathEl::LineTo(p) => b = b.union_point(p),
                PathEl::QuadTo(c, p) => b = b.union_point(c).union_point(p),
                PathEl::CurveTo(c1, c2, p) => b = b.union_point(c1).union_point(c2).union_point(p),
                PathEl::ClosePath => {}
            }
        }
        b
    }

    /// The exact box of the drawn curve.
    pub fn bounds(&self) -> Bounds {
        let subs = self.subpaths();
        let mut b = Bounds::EMPTY;
        for sp in &subs {
            if sp.segments.is_empty() {
                b = b.union_point(sp.start);
            }
            for seg in &sp.segments {
                b = b.union(seg.bounds());
            }
        }
        b
    }

    /// Total arc length of every subpath.
    pub fn length(&self) -> f64 {
        self.segments()
            .iter()
            .map(|s| s.length(crate::DEFAULT_TOLERANCE * 0.01))
            .sum()
    }

    /// The point `t` of the way along the path by **arc length**, `t` in
    /// `0..=1`.
    ///
    /// Arc length, not Bezier parameter: `point_at(0.5)` is the half-way point
    /// you could measure with a piece of string, which is what a "text on a
    /// path" or an evenly-spaced dash needs. `None` only for a path that draws
    /// nothing.
    pub fn point_at(&self, t: f64) -> Option<Point> {
        let total = self.length();
        if total <= 0.0 {
            // A path of zero length is still somewhere, if it has a point.
            return self.subpaths().first().map(|s| s.start);
        }
        self.point_at_length(t.clamp(0.0, 1.0) * total)
    }

    /// The point `s` units along the path, clamped to its ends.
    pub fn point_at_length(&self, s: f64) -> Option<Point> {
        let segs = self.segments();
        if segs.is_empty() {
            return self.subpaths().first().map(|s| s.start);
        }
        let acc = crate::DEFAULT_TOLERANCE * 0.01;
        if s.is_nan() || s <= 0.0 {
            return Some(segs[0].start());
        }
        let mut remaining = s;
        for seg in &segs {
            let len = seg.length(acc);
            if remaining <= len {
                return Some(seg.eval(seg.param_at_length(remaining, acc)));
            }
            remaining -= len;
        }
        segs.last().map(|s| s.end())
    }

    /// Flatten to polylines with an adaptive tolerance.
    ///
    /// `tolerance` is the maximum distance, in path units, between the true
    /// curve and the polyline that replaces it. Subdivision is adaptive, so a
    /// nearly-straight curve costs one segment and a tight one costs as many as
    /// it needs.
    pub fn flatten(&self, tolerance: f64) -> Vec<Polyline> {
        let mut out = Vec::new();
        for sp in self.subpaths() {
            let mut pts = vec![sp.start];
            for seg in &sp.segments {
                seg.flatten_into(tolerance, &mut pts);
            }
            if sp.closed {
                // The closing edge is implied by `closed`; drop the duplicated
                // vertex so every edge of the ring is distinct.
                if pts.len() > 1 && pts[pts.len() - 1].distance_squared(pts[0]) == 0.0 {
                    pts.pop();
                }
            }
            dedup_points(&mut pts);
            if !pts.is_empty() {
                out.push(Polyline {
                    points: pts,
                    closed: sp.closed,
                });
            }
        }
        out
    }

    /// Flatten treating every subpath as closed — the view a fill takes.
    pub(crate) fn flatten_closed(&self, tolerance: f64) -> Vec<Polyline> {
        self.flatten(tolerance)
            .into_iter()
            .filter(|p| p.points.len() >= 3)
            .map(|mut p| {
                p.closed = true;
                p
            })
            .collect()
    }

    /// The same geometry drawn backwards, subpath by subpath.
    pub fn reversed(&self) -> Path {
        let mut out = Path::new();
        for sp in self.subpaths() {
            if sp.segments.is_empty() {
                out.move_to(sp.start);
                continue;
            }
            out.move_to(sp.segments[sp.segments.len() - 1].end());
            for seg in sp.segments.iter().rev() {
                match seg.reversed() {
                    Segment::Line(_, p) => {
                        out.push(PathEl::LineTo(p));
                    }
                    Segment::Quad(_, c, p) => {
                        out.push(PathEl::QuadTo(c, p));
                    }
                    Segment::Cubic(_, c1, c2, p) => {
                        out.push(PathEl::CurveTo(c1, c2, p));
                    }
                }
            }
            if sp.closed {
                out.close();
            }
        }
        out
    }

    /// A path from a run of points.
    pub fn from_polyline(points: &[Point], closed: bool) -> Path {
        let mut p = Path::new();
        let Some((first, rest)) = points.split_first() else {
            return p;
        };
        p.move_to(*first);
        for q in rest {
            p.push(PathEl::LineTo(*q));
        }
        if closed {
            p.close();
        }
        p
    }

    /// Twice the total signed area, flattened to `tolerance`.
    ///
    /// The sign is the orientation: positive counter-clockwise in a y-up frame.
    pub fn signed_area2(&self, tolerance: f64) -> f64 {
        self.flatten_closed(tolerance)
            .iter()
            .map(|p| p.signed_area2())
            .sum()
    }

    /// `true` when every coordinate in the path is finite.
    pub fn is_finite(&self) -> bool {
        self.els.iter().all(|el| match *el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => p.is_finite(),
            PathEl::QuadTo(a, b) => a.is_finite() && b.is_finite(),
            PathEl::CurveTo(a, b, c) => a.is_finite() && b.is_finite() && c.is_finite(),
            PathEl::ClosePath => true,
        })
    }
}

/// The subpath a drawing command extends, opening one at the pending start
/// point (or the origin) when there is none.
fn open_sub<'a>(cur: &'a mut Option<SubPath>, pending: &mut Option<Point>) -> &'a mut SubPath {
    if cur.is_none() {
        *cur = Some(new_sub(pending.take().unwrap_or(Point::ZERO)));
    }
    cur.as_mut().expect("just inserted")
}

fn new_sub(start: Point) -> SubPath {
    SubPath {
        start,
        segments: Vec::new(),
        closed: false,
    }
}

/// Drop vertices that repeat their predecessor exactly.
///
/// Zero-length edges are not merely wasteful: they have no direction, so a
/// stroke offsets them by a zero normal and a boolean op cannot classify them.
pub(crate) fn dedup_points(pts: &mut Vec<Point>) {
    pts.dedup_by(|a, b| a.distance_squared(*b) == 0.0);
}

/// The cubics approximating one SVG endpoint arc.
///
/// Split so no piece spans more than a quarter turn, which is where the
/// standard `4/3 * tan(dtheta/4)` handle length stays accurate.
fn arc_segments(
    p0: Point,
    rx: f64,
    ry: f64,
    phi: f64,
    large_arc: bool,
    sweep: bool,
    p1: Point,
) -> Vec<Segment> {
    let (sin_phi, cos_phi) = phi.sin_cos();
    // F.6.5.1: endpoints into the ellipse's own frame.
    let dx2 = (p0.x - p1.x) * 0.5;
    let dy2 = (p0.y - p1.y) * 0.5;
    let x1 = cos_phi * dx2 + sin_phi * dy2;
    let y1 = -sin_phi * dx2 + cos_phi * dy2;

    // F.6.6: radii too small to reach are scaled up until they just do.
    let mut rx = rx;
    let mut ry = ry;
    let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }

    // F.6.5.2: centre in the ellipse frame.
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut coef = if den > 0.0 {
        (num / den).max(0.0).sqrt()
    } else {
        0.0
    };
    if large_arc == sweep {
        coef = -coef;
    }
    let cx1 = coef * rx * y1 / ry;
    let cy1 = -coef * ry * x1 / rx;

    // F.6.5.3: centre back in user space.
    let cx = cos_phi * cx1 - sin_phi * cy1 + (p0.x + p1.x) * 0.5;
    let cy = sin_phi * cx1 + cos_phi * cy1 + (p0.y + p1.y) * 0.5;

    // F.6.5.5/6: start angle and sweep.
    let ux = (x1 - cx1) / rx;
    let uy = (y1 - cy1) / ry;
    let vx = (-x1 - cx1) / rx;
    let vy = (-y1 - cy1) / ry;
    let theta1 = uy.atan2(ux);
    let mut dtheta = (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let tau = std::f64::consts::TAU;
    if !sweep && dtheta > 0.0 {
        dtheta -= tau;
    } else if sweep && dtheta < 0.0 {
        dtheta += tau;
    }

    if !dtheta.is_finite() || dtheta == 0.0 {
        return vec![Segment::Line(p0, p1)];
    }

    let pieces = (dtheta.abs() / std::f64::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = dtheta / pieces as f64;
    let alpha = (4.0 / 3.0) * (step * 0.25).tan();

    let at = |t: f64| -> (Point, Point) {
        let (s, c) = t.sin_cos();
        let p = point(
            cx + rx * c * cos_phi - ry * s * sin_phi,
            cy + rx * c * sin_phi + ry * s * cos_phi,
        );
        let d = point(
            -rx * s * cos_phi - ry * c * sin_phi,
            -rx * s * sin_phi + ry * c * cos_phi,
        );
        (p, d)
    };

    let mut out = Vec::with_capacity(pieces);
    for i in 0..pieces {
        let t0 = theta1 + step * i as f64;
        let t1 = t0 + step;
        let (a, da) = at(t0);
        let (b, db) = at(t1);
        // Pin the true endpoints so the arc closes exactly on `p1`.
        let a = if i == 0 { p0 } else { a };
        let b = if i == pieces - 1 { p1 } else { b };
        out.push(Segment::Cubic(a, a + da * alpha, b - db * alpha, b));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn a_closed_subpath_carries_its_own_closing_line() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(4.0, 0.0))
            .line_to(point(4.0, 3.0))
            .close();
        let subs = p.subpaths();
        assert_eq!(subs.len(), 1);
        assert!(subs[0].closed);
        assert_eq!(subs[0].segments.len(), 3, "the closing line is a segment");
        assert!(close(subs[0].segments[2].end(), point(0.0, 0.0)));
        assert_eq!(p.length(), 12.0);
    }

    #[test]
    fn an_element_after_close_starts_a_new_subpath_at_the_old_start() {
        // The SVG rule. Getting it wrong silently welds two shapes together.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(1.0, 0.0))
            .close()
            .line_to(point(0.0, 5.0));
        let subs = p.subpaths();
        assert_eq!(subs.len(), 2);
        assert!(subs[0].closed);
        assert!(!subs[1].closed);
        assert!(close(subs[1].start, point(0.0, 0.0)));
        assert!(close(subs[1].end(), point(0.0, 5.0)));
        assert_eq!(p.current_point(), Some(point(0.0, 5.0)));
    }

    #[test]
    fn drawing_without_a_move_opens_a_subpath_instead_of_panicking() {
        let mut p = Path::new();
        p.line_to(point(3.0, 4.0));
        assert_eq!(p.elements()[0], PathEl::MoveTo(Point::ZERO));
        assert_eq!(p.length(), 5.0);

        // and a bare close on an empty path changes nothing
        let mut q = Path::new();
        q.close();
        assert!(q.is_empty());
        q.move_to(point(1.0, 1.0)).close().close();
        assert_eq!(q.elements().len(), 2);
    }

    #[test]
    fn an_empty_path_answers_every_query_without_panicking() {
        let p = Path::new();
        assert!(p.is_empty());
        assert_eq!(p.bounds(), Bounds::EMPTY);
        assert_eq!(p.control_bounds(), Bounds::EMPTY);
        assert_eq!(p.length(), 0.0);
        assert_eq!(p.point_at(0.5), None);
        assert_eq!(p.point_at_length(3.0), None);
        assert!(p.segments().is_empty());
        assert!(p.flatten(0.1).is_empty());
        assert!(p.reversed().is_empty());
        assert_eq!(p.signed_area2(0.1), 0.0);
    }

    #[test]
    fn a_single_point_subpath_survives_every_query() {
        let mut p = Path::new();
        p.move_to(point(7.0, -2.0));
        assert_eq!(p.subpaths().len(), 1);
        assert!(p.subpaths()[0].segments.is_empty());
        assert!(p.subpaths()[0].is_degenerate());
        assert_eq!(p.bounds(), Bounds::from_point(point(7.0, -2.0)));
        assert_eq!(p.length(), 0.0);
        assert_eq!(p.point_at(0.5), Some(point(7.0, -2.0)));
        assert_eq!(p.flatten(0.1).len(), 1);
        assert_eq!(p.flatten(0.1)[0].points, vec![point(7.0, -2.0)]);
    }

    #[test]
    fn point_at_walks_by_arc_length_not_by_parameter() {
        // Two very unequal legs: the parametric half-way point is the corner,
        // the arc-length one is well inside the long leg.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(1.0, 0.0))
            .line_to(point(1.0, 99.0));
        assert_eq!(p.length(), 100.0);
        assert!(close(p.point_at(0.0).unwrap(), point(0.0, 0.0)));
        assert!(close(p.point_at(0.5).unwrap(), point(1.0, 49.0)));
        assert!(close(p.point_at(1.0).unwrap(), point(1.0, 99.0)));
        assert!(close(p.point_at_length(0.25).unwrap(), point(0.25, 0.0)));
        // out of range clamps rather than returning None
        assert!(close(p.point_at(-1.0).unwrap(), point(0.0, 0.0)));
        assert!(close(p.point_at(9.0).unwrap(), point(1.0, 99.0)));

        // The case straight lines cannot show: a curve whose Bezier parameter
        // runs at a wildly non-uniform speed. Both handles are bunched at the
        // far end, so the curve covers most of its length in the first quarter
        // of its parameter. Every control point is on the x axis, so the arc
        // length *is* the x coordinate and the right answer is exact.
        let mut c = Path::new();
        c.move_to(point(0.0, 0.0))
            .curve_to(point(90.0, 0.0), point(100.0, 0.0), point(100.0, 0.0));
        assert!((c.length() - 100.0).abs() < 1e-6, "{}", c.length());
        let half = c.point_at(0.5).unwrap();
        assert!(
            (half.x - 50.0).abs() < 1e-3,
            "half way along by arc length is x = {}, not 50",
            half.x
        );
        // The parametric midpoint is a completely different place, which is
        // what makes this a real test and not a restatement of `eval`.
        let parametric = c.segments()[0].eval(0.5);
        assert!((parametric.x - 83.75).abs() < 1e-9, "{}", parametric.x);
        for frac in [0.1, 0.25, 0.75, 0.9] {
            let q = c.point_at(frac).unwrap();
            assert!((q.x - frac * 100.0).abs() < 1e-3, "at {frac}: {}", q.x);
        }
    }

    #[test]
    fn transform_moves_the_geometry_and_the_bounds_with_it() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .curve_to(point(0.0, 1.0), point(1.0, 1.0), point(1.0, 0.0))
            .close();
        let t = Affine::translate(10.0, 20.0).then(Affine::scale(2.0, 2.0));
        let moved = p.transform(&t);
        assert_eq!(moved.bounds(), t.apply_bounds(p.bounds()));
        assert!((moved.length() - p.length() * 2.0).abs() < 1e-6);
    }

    /// `bounds()` is the box of the ink; `control_bounds()` is the box of the
    /// handles. Most shapes cannot tell them apart, because their control
    /// points already sit inside the curve's own box — so this uses the one
    /// case that can: a cubic whose two handles overshoot the extremum the
    /// curve actually reaches.
    #[test]
    fn path_bounds_are_tight_where_control_bounds_are_not() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0)).curve_to(
            point(0.0, 100.0),
            point(100.0, 100.0),
            point(100.0, 0.0),
        );

        // y(t) = 3(1-t)^2 t*100 + 3(1-t)t^2*100 = 300 t (1 - t), which peaks at
        // t = 1/2 with y = 75 — three quarters of the way to the handles.
        let tight = p.bounds();
        let ctrl = p.control_bounds();
        assert!((tight.max.y - 75.0).abs() < 1e-9, "{tight:?}");
        assert!((ctrl.max.y - 100.0).abs() < 1e-9, "{ctrl:?}");
        assert!(tight.height() < ctrl.height(), "{tight:?} vs {ctrl:?}");
        // Everywhere else the two agree: the curve does reach x = 0 and x = 100
        // and starts on y = 0, so only the overshoot separates them.
        assert_eq!(tight.min, point(0.0, 0.0));
        assert_eq!(ctrl.min, point(0.0, 0.0));
        assert!((tight.max.x - 100.0).abs() < 1e-9);
        assert!((ctrl.max.x - 100.0).abs() < 1e-9);
        // The tight box is always inside the conservative one, never the other
        // way round.
        assert_eq!(tight.union(ctrl), ctrl);

        // The same for a quadratic, whose single handle it reaches half way to,
        // and across more than one subpath so the union really is over tight
        // boxes rather than one lucky segment.
        let mut q = Path::new();
        q.move_to(point(0.0, 0.0))
            .quad_to(point(50.0, 100.0), point(100.0, 0.0))
            .move_to(point(200.0, 0.0))
            .line_to(point(200.0, 10.0));
        assert!((q.bounds().max.y - 50.0).abs() < 1e-9, "{:?}", q.bounds());
        assert!((q.control_bounds().max.y - 100.0).abs() < 1e-9);
        assert!((q.bounds().max.x - 200.0).abs() < 1e-9);
    }

    #[test]
    fn reversing_twice_returns_the_original_geometry() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .curve_to(point(0.0, 10.0), point(10.0, 10.0), point(10.0, 0.0))
            .quad_to(point(15.0, -5.0), point(20.0, 0.0))
            .close();
        let back = p.reversed().reversed();
        assert_eq!(back.subpaths().len(), 1);
        for (a, b) in p.segments().iter().zip(back.segments().iter()) {
            assert!(close(a.start(), b.start()) && close(a.end(), b.end()));
        }
        // one reversal flips the orientation sign
        assert!(p.signed_area2(0.01) * p.reversed().signed_area2(0.01) < 0.0);
    }

    #[test]
    fn an_arc_lands_exactly_on_its_endpoint_and_stays_on_the_circle() {
        for (large, sweep) in [(false, false), (false, true), (true, false), (true, true)] {
            let mut p = Path::new();
            p.move_to(point(100.0, 0.0));
            p.arc_to(100.0, 100.0, 0.0, large, sweep, point(0.0, 100.0));
            let end = p.current_point().unwrap();
            assert!(close(end, point(0.0, 100.0)), "{large} {sweep} -> {end:?}");
            // Two circles of radius 100 pass through both endpoints; which one
            // the arc rides is exactly what the flags choose. Every vertex must
            // sit on the same one.
            let pts: Vec<Point> = p
                .flatten(0.01)
                .into_iter()
                .flat_map(|pl| pl.points)
                .collect();
            let center = [point(0.0, 0.0), point(100.0, 100.0)]
                .into_iter()
                .find(|c| pts.iter().all(|q| (q.distance(*c) - 100.0).abs() < 0.05))
                .unwrap_or_else(|| panic!("{large}/{sweep} is not on either circle"));
            assert!(pts
                .iter()
                .all(|q| (q.distance(center) - 100.0).abs() < 0.05));
            // Large-arc really is the long way round.
            let quarter = std::f64::consts::PI * 100.0 * 0.5;
            let len = p.length();
            if large {
                assert!(len > quarter * 2.0, "{len}");
            } else {
                assert!((len - quarter).abs() < 0.5, "{len}");
            }
        }
    }

    #[test]
    fn degenerate_arcs_follow_the_svg_error_rules() {
        // Zero radius becomes a straight line.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .arc_to(0.0, 10.0, 0.0, false, true, point(10.0, 0.0));
        assert_eq!(p.length(), 10.0);
        assert!(matches!(p.elements()[1], PathEl::LineTo(_)));

        // Coincident endpoints emit nothing at all.
        let mut q = Path::new();
        q.move_to(point(5.0, 5.0))
            .arc_to(10.0, 10.0, 0.0, true, true, point(5.0, 5.0));
        assert_eq!(q.elements().len(), 1);

        // Radii too small to span the endpoints are scaled up until they fit,
        // so the arc still lands on the endpoint.
        let mut r = Path::new();
        r.move_to(point(0.0, 0.0))
            .arc_to(1.0, 1.0, 0.0, false, true, point(100.0, 0.0));
        assert!(close(r.current_point().unwrap(), point(100.0, 0.0)));
        assert!(r.is_finite());

        // Non-finite input is dropped, not propagated.
        let mut s = Path::new();
        s.move_to(point(0.0, 0.0))
            .arc_to(f64::NAN, 1.0, 0.0, false, true, point(1.0, 1.0));
        assert_eq!(s.elements().len(), 1);
        assert!(s.is_finite());
    }

    /// `x_rotation` tilts the ellipse, and the only way to see that is with
    /// unequal radii — on a circle the rotation is a no-op by definition, so an
    /// arc test that uses `rx == ry` holds nothing.
    ///
    /// The invariant is the definition of the parameter itself, and it avoids
    /// re-deriving the centre solve or F.6.6's radius scaling: an arc drawn
    /// with rotation `phi` between two endpoints is the *unrotated* arc drawn
    /// between those endpoints rotated back by `phi`, and then turned forward
    /// by `phi`. If the rotation is dropped anywhere on the way through, the
    /// two stop agreeing.
    #[test]
    fn an_arcs_x_rotation_tilts_the_ellipse_it_travels_on() {
        let p0 = point(10.0, -4.0);
        let p1 = point(60.0, 25.0);

        for phi in [0.3, std::f64::consts::FRAC_PI_4, 1.9, -0.8] {
            let back = Affine::rotate(-phi);
            let fwd = Affine::rotate(phi);
            for (rx, ry) in [(50.0, 20.0), (13.0, 40.0), (30.0, 30.0)] {
                for (large, sweep) in [(false, false), (false, true), (true, false), (true, true)] {
                    let mut tilted = Path::new();
                    tilted.move_to(p0).arc_to(rx, ry, phi, large, sweep, p1);

                    let mut upright = Path::new();
                    upright.move_to(back.apply(p0)).arc_to(
                        rx,
                        ry,
                        0.0,
                        large,
                        sweep,
                        back.apply(p1),
                    );
                    let upright = upright.transform(&fwd);

                    let what = format!("phi {phi} r {rx}x{ry} {large}/{sweep}");
                    assert_eq!(
                        tilted.elements().len(),
                        upright.elements().len(),
                        "{what}: different cubic counts"
                    );
                    for (a, b) in tilted.segments().iter().zip(upright.segments().iter()) {
                        for (u, v) in [
                            (a.start(), b.start()),
                            (a.end(), b.end()),
                            (a.eval(0.25), b.eval(0.25)),
                            (a.eval(0.75), b.eval(0.75)),
                        ] {
                            // 1e-6, not 1e-12: the centre solve divides by a
                            // quantity that goes to zero as the radii approach
                            // the smallest ellipse that spans the chord, so the
                            // two routes to the same arc can disagree in the
                            // eighth digit there. Dropping the rotation moves
                            // these points by tens of units, not by 1e-8.
                            assert!(u.distance(v) < 1e-6, "{what}: {u:?} vs {v:?}");
                        }
                    }

                    // And the tilt is not vacuous: with unequal radii the
                    // rotated arc is nowhere near the unrotated one.
                    if rx != ry {
                        let mut flat = Path::new();
                        flat.move_to(p0).arc_to(rx, ry, 0.0, large, sweep, p1);
                        let a = tilted.point_at(0.5).unwrap();
                        let b = flat.point_at(0.5).unwrap();
                        assert!(a.distance(b) > 1.0, "{what}: rotation changed nothing");
                    }
                }
            }
        }
    }

    #[test]
    fn flattening_a_closed_subpath_does_not_repeat_the_first_vertex() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .line_to(point(10.0, 10.0))
            .close();
        let pls = p.flatten(0.1);
        assert_eq!(pls.len(), 1);
        assert!(pls[0].closed);
        assert_eq!(pls[0].points.len(), 3);
        assert_eq!(pls[0].edges().len(), 3);
        assert_eq!(pls[0].length(), 10.0 + 10.0 + (200f64).sqrt());
        assert_eq!(pls[0].signed_area2(), 100.0);
    }

    #[test]
    fn flatten_tolerance_is_adaptive() {
        let mut p = Path::new();
        // A nearly straight curve and a violently curved one, same element count.
        p.move_to(point(0.0, 0.0)).curve_to(
            point(33.0, 0.01),
            point(66.0, -0.01),
            point(100.0, 0.0),
        );
        let straightish = p.flatten(0.05)[0].points.len();
        let mut q = Path::new();
        q.move_to(point(0.0, 0.0)).curve_to(
            point(0.0, 100.0),
            point(100.0, 100.0),
            point(100.0, 0.0),
        );
        let curvy = q.flatten(0.05)[0].points.len();
        assert!(
            curvy > straightish * 4,
            "adaptive flattening spent {straightish} on the flat curve and {curvy} on the tight one"
        );
    }
}
