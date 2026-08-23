//! One piece of a path: a line, a quadratic, or a cubic.
//!
//! A [`Segment`] carries its own start point, so it is meaningful on its own —
//! that is what lets hit testing, flattening and stroking iterate a path
//! without threading a "current point" through every call.
//!
//! # Tight bounds, not control-point bounds
//! The convex hull of a Bezier's control points contains the curve, so hulling
//! them is a valid bound — and it is the *wrong* one to show a user. A curve
//! whose handles are pulled far out has a control box several times the size of
//! the ink. Both are available here ([`Segment::control_bounds`] and
//! [`Segment::bounds`]); the tight one is computed from the roots of the
//! derivative and is exact.

use serde::{Deserialize, Serialize};

use crate::affine::Affine;
use crate::point::{Bounds, Point};

/// Deepest recursion any subdivision in this module will go.
///
/// A bound is required, not a nicety: a curve with non-finite or astronomically
/// separated control points never satisfies a flatness test, and unbounded
/// recursion is a stack overflow — an abort, not an error. 16 levels is 65,536
/// output segments, far past the point where tolerance normally stops it.
const MAX_DEPTH: u32 = 16;

/// One line or Bezier piece of a path, with absolute coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Segment {
    /// A straight line from the first point to the second.
    Line(Point, Point),
    /// A quadratic Bezier: start, control, end.
    Quad(Point, Point, Point),
    /// A cubic Bezier: start, two controls, end.
    Cubic(Point, Point, Point, Point),
}

impl Segment {
    /// First point.
    pub fn start(&self) -> Point {
        match *self {
            Segment::Line(p, _) | Segment::Quad(p, _, _) | Segment::Cubic(p, _, _, _) => p,
        }
    }

    /// Last point.
    pub fn end(&self) -> Point {
        match *self {
            Segment::Line(_, p) | Segment::Quad(_, _, p) | Segment::Cubic(_, _, _, p) => p,
        }
    }

    /// `true` when every control point is finite.
    pub fn is_finite(&self) -> bool {
        self.control_points().iter().all(|p| p.is_finite())
    }

    /// The control points, including the endpoints.
    pub fn control_points(&self) -> Vec<Point> {
        match *self {
            Segment::Line(a, b) => vec![a, b],
            Segment::Quad(a, b, c) => vec![a, b, c],
            Segment::Cubic(a, b, c, d) => vec![a, b, c, d],
        }
    }

    /// This segment as a cubic, exactly (a line and a quadratic both have exact
    /// cubic forms).
    pub fn to_cubic(&self) -> Segment {
        match *self {
            Segment::Line(a, b) => Segment::Cubic(a, a.lerp(b, 1.0 / 3.0), a.lerp(b, 2.0 / 3.0), b),
            Segment::Quad(a, b, c) => {
                Segment::Cubic(a, a + (b - a) * (2.0 / 3.0), c + (b - c) * (2.0 / 3.0), c)
            }
            cubic @ Segment::Cubic(..) => cubic,
        }
    }

    /// The same geometry traced backwards.
    pub fn reversed(&self) -> Segment {
        match *self {
            Segment::Line(a, b) => Segment::Line(b, a),
            Segment::Quad(a, b, c) => Segment::Quad(c, b, a),
            Segment::Cubic(a, b, c, d) => Segment::Cubic(d, c, b, a),
        }
    }

    /// A copy with every control point transformed.
    pub fn transform(&self, t: &Affine) -> Segment {
        match *self {
            Segment::Line(a, b) => Segment::Line(t.apply(a), t.apply(b)),
            Segment::Quad(a, b, c) => Segment::Quad(t.apply(a), t.apply(b), t.apply(c)),
            Segment::Cubic(a, b, c, d) => {
                Segment::Cubic(t.apply(a), t.apply(b), t.apply(c), t.apply(d))
            }
        }
    }

    /// The point at Bezier parameter `t`, clamped to `0..=1`.
    ///
    /// This is the *parametric* position, not the arc-length one — Bezier
    /// parameter and distance along the curve are not the same thing. Use
    /// [`crate::Path::point_at`] when you want an evenly-spaced walk.
    pub fn eval(&self, t: f64) -> Point {
        let t = t.clamp(0.0, 1.0);
        let mt = 1.0 - t;
        match *self {
            Segment::Line(a, b) => a.lerp(b, t),
            Segment::Quad(a, b, c) => a * (mt * mt) + b * (2.0 * mt * t) + c * (t * t),
            Segment::Cubic(a, b, c, d) => {
                a * (mt * mt * mt)
                    + b * (3.0 * mt * mt * t)
                    + c * (3.0 * mt * t * t)
                    + d * (t * t * t)
            }
        }
    }

    /// The derivative at `t`: the tangent vector, unnormalised.
    pub fn derivative(&self, t: f64) -> Point {
        let t = t.clamp(0.0, 1.0);
        let mt = 1.0 - t;
        match *self {
            Segment::Line(a, b) => b - a,
            Segment::Quad(a, b, c) => (b - a) * (2.0 * mt) + (c - b) * (2.0 * t),
            Segment::Cubic(a, b, c, d) => {
                (b - a) * (3.0 * mt * mt) + (c - b) * (6.0 * mt * t) + (d - c) * (3.0 * t * t)
            }
        }
    }

    /// Unit tangent at `t`, looking ahead for a usable direction when the
    /// derivative vanishes (which it does at a cusp, and whenever two control
    /// points coincide).
    pub fn tangent(&self, t: f64) -> Point {
        let d = self.derivative(t).normalize();
        if d != Point::ZERO {
            return d;
        }
        // Degenerate derivative: sample a nearby chord instead of returning a
        // zero direction that would collapse a stroke's normal.
        for eps in [1e-6, 1e-4, 1e-2, 0.1] {
            let (a, b) = ((t - eps).max(0.0), (t + eps).min(1.0));
            if a < b {
                let d = (self.eval(b) - self.eval(a)).normalize();
                if d != Point::ZERO {
                    return d;
                }
            }
        }
        Point::ZERO
    }

    /// Split at parameter `t` into the piece before and the piece after.
    pub fn split(&self, t: f64) -> (Segment, Segment) {
        let t = t.clamp(0.0, 1.0);
        match *self {
            Segment::Line(a, b) => {
                let m = a.lerp(b, t);
                (Segment::Line(a, m), Segment::Line(m, b))
            }
            Segment::Quad(a, b, c) => {
                let ab = a.lerp(b, t);
                let bc = b.lerp(c, t);
                let m = ab.lerp(bc, t);
                (Segment::Quad(a, ab, m), Segment::Quad(m, bc, c))
            }
            Segment::Cubic(a, b, c, d) => {
                let ab = a.lerp(b, t);
                let bc = b.lerp(c, t);
                let cd = c.lerp(d, t);
                let abc = ab.lerp(bc, t);
                let bcd = bc.lerp(cd, t);
                let m = abc.lerp(bcd, t);
                (Segment::Cubic(a, ab, abc, m), Segment::Cubic(m, bcd, cd, d))
            }
        }
    }

    /// The piece between two parameters.
    pub fn subsegment(&self, t0: f64, t1: f64) -> Segment {
        let (t0, t1) = (t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0));
        if t1 <= t0 {
            let p = self.eval(t0);
            return Segment::Line(p, p);
        }
        let tail = self.split(t0).1;
        // Re-parameterise t1 into the tail's own 0..1 range.
        let t = if t0 >= 1.0 {
            1.0
        } else {
            (t1 - t0) / (1.0 - t0)
        };
        tail.split(t).0
    }

    /// Box of the control points: cheap, conservative, and bigger than the ink.
    pub fn control_bounds(&self) -> Bounds {
        Bounds::from_points(self.control_points())
    }

    /// The exact box of the curve itself, from the roots of the derivative.
    pub fn bounds(&self) -> Bounds {
        let mut b = Bounds::from_points([self.start(), self.end()]);
        for t in self.extrema() {
            b = b.union_point(self.eval(t));
        }
        b
    }

    /// Parameters strictly inside `0..1` where the curve reaches an axis
    /// extreme. Sorted ascending.
    pub fn extrema(&self) -> Vec<f64> {
        let mut ts: Vec<f64> = Vec::new();
        match *self {
            Segment::Line(..) => {}
            Segment::Quad(p0, p1, p2) => {
                for axis in 0..2 {
                    let (a, b, c) = (axis_of(p0, axis), axis_of(p1, axis), axis_of(p2, axis));
                    let denom = a - 2.0 * b + c;
                    if denom != 0.0 {
                        push_if_inside(&mut ts, (a - b) / denom);
                    }
                }
            }
            Segment::Cubic(p0, p1, p2, p3) => {
                for axis in 0..2 {
                    let (v0, v1, v2, v3) = (
                        axis_of(p0, axis),
                        axis_of(p1, axis),
                        axis_of(p2, axis),
                        axis_of(p3, axis),
                    );
                    // B'(t)/3 = A t^2 + B t + C with A = a-2b+c, B = 2(b-a), C = a
                    // where a = v1-v0, b = v2-v1, c = v3-v2.
                    let (a, b, c) = (v1 - v0, v2 - v1, v3 - v2);
                    let qa = a - 2.0 * b + c;
                    let qb = 2.0 * (b - a);
                    let qc = a;
                    for t in solve_quadratic(qa, qb, qc) {
                        push_if_inside(&mut ts, t);
                    }
                }
            }
        }
        ts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        ts.dedup();
        ts
    }

    /// Arc length, computed to roughly `accuracy` by subdivision.
    ///
    /// The chord underestimates a curve and the control polygon overestimates
    /// it; when they agree the true length is between them, so the midpoint is
    /// accurate to half their difference. Otherwise split and recurse.
    pub fn length(&self, accuracy: f64) -> f64 {
        let accuracy = if accuracy.is_finite() && accuracy > 0.0 {
            accuracy
        } else {
            1e-4
        };
        if !self.is_finite() {
            return 0.0;
        }
        match *self {
            Segment::Line(a, b) => a.distance(b),
            _ => self.length_rec(accuracy, 0),
        }
    }

    fn length_rec(&self, accuracy: f64, depth: u32) -> f64 {
        let pts = self.control_points();
        let chord = pts[0].distance(pts[pts.len() - 1]);
        let poly: f64 = pts.windows(2).map(|w| w[0].distance(w[1])).sum();
        if depth >= MAX_DEPTH || poly - chord <= accuracy {
            return (poly + chord) * 0.5;
        }
        let (l, r) = self.split(0.5);
        l.length_rec(accuracy * 0.5, depth + 1) + r.length_rec(accuracy * 0.5, depth + 1)
    }

    /// The parameter at which this segment has covered `s` units of arc length.
    ///
    /// Bisection on [`Segment::length`] of the prefix. Monotone, so bisection
    /// converges; 60 halvings is well past `f64` resolution.
    pub fn param_at_length(&self, s: f64, accuracy: f64) -> f64 {
        let total = self.length(accuracy);
        if s.is_nan() || s <= 0.0 || total <= 0.0 {
            return 0.0;
        }
        if s >= total {
            return 1.0;
        }
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..60 {
            let mid = (lo + hi) * 0.5;
            if self.subsegment(0.0, mid).length(accuracy) < s {
                lo = mid;
            } else {
                hi = mid;
            }
            if hi - lo < 1e-12 {
                break;
            }
        }
        (lo + hi) * 0.5
    }

    /// Append this segment's flattened approximation to `out`, **excluding** the
    /// start point and including the end point.
    ///
    /// The caller owns the start point, which is what lets a whole subpath be
    /// flattened into one polyline without duplicated vertices at the joins.
    pub fn flatten_into(&self, tolerance: f64, out: &mut Vec<Point>) {
        let tol = if tolerance.is_finite() && tolerance > 0.0 {
            tolerance
        } else {
            crate::DEFAULT_TOLERANCE
        };
        if !self.is_finite() {
            // Nothing sensible to emit; drop the piece rather than poison the
            // polyline with NaN, which would make every later bound NaN too.
            return;
        }
        match *self {
            Segment::Line(_, b) => out.push(b),
            _ => self.flatten_rec(tol, 0, out),
        }
    }

    fn flatten_rec(&self, tol: f64, depth: u32, out: &mut Vec<Point>) {
        if depth >= MAX_DEPTH || self.is_flat(tol) {
            out.push(self.end());
            return;
        }
        let (l, r) = self.split(0.5);
        l.flatten_rec(tol, depth + 1, out);
        r.flatten_rec(tol, depth + 1, out);
    }

    /// `true` when no control point is further than `tol` from the chord.
    fn is_flat(&self, tol: f64) -> bool {
        let pts = self.control_points();
        let (a, b) = (pts[0], pts[pts.len() - 1]);
        let chord = b - a;
        let chord_len = chord.length();
        if chord_len <= f64::EPSILON {
            // A closed loop of a chord: fall back to the raw handle distance,
            // otherwise a curve that returns to its start looks perfectly flat.
            return pts[1..pts.len() - 1]
                .iter()
                .all(|p| (*p - a).length() <= tol);
        }
        pts[1..pts.len() - 1]
            .iter()
            .all(|p| (chord.cross(*p - a) / chord_len).abs() <= tol)
    }
}

fn axis_of(p: Point, axis: usize) -> f64 {
    if axis == 0 {
        p.x
    } else {
        p.y
    }
}

fn push_if_inside(ts: &mut Vec<f64>, t: f64) {
    if t.is_finite() && t > 0.0 && t < 1.0 {
        ts.push(t);
    }
}

/// Real roots of `a t^2 + b t + c`, degenerate cases included.
pub(crate) fn solve_quadratic(a: f64, b: f64, c: f64) -> Vec<f64> {
    if a.abs() < 1e-14 {
        if b.abs() < 1e-14 {
            return Vec::new();
        }
        return vec![-c / b];
    }
    let disc = b * b - 4.0 * a * c;
    if disc < 0.0 {
        return Vec::new();
    }
    let sq = disc.sqrt();
    // The numerically stable pair: computing both roots from `-b + sq` loses
    // all precision in one of them when `b` and `sq` nearly cancel.
    let q = -0.5 * (b + if b >= 0.0 { sq } else { -sq });
    let mut roots = vec![q / a];
    if q.abs() > 1e-300 {
        roots.push(c / q);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::point;

    fn close(a: Point, b: Point) -> bool {
        a.distance(b) < 1e-9
    }

    #[test]
    fn eval_hits_the_endpoints_and_the_midpoint() {
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(0.0, 1.0),
            point(1.0, 1.0),
            point(1.0, 0.0),
        );
        assert!(close(c.eval(0.0), point(0.0, 0.0)));
        assert!(close(c.eval(1.0), point(1.0, 0.0)));
        assert!(close(c.eval(0.5), point(0.5, 0.75)));
        // out-of-range parameters clamp rather than extrapolating to nonsense
        assert!(close(c.eval(-5.0), c.eval(0.0)));
        assert!(close(c.eval(5.0), c.eval(1.0)));
    }

    #[test]
    fn tight_bounds_are_smaller_than_control_bounds_and_still_contain_the_curve() {
        // Handles pulled to y = 1 but the curve only reaches y = 0.75.
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(0.0, 1.0),
            point(1.0, 1.0),
            point(1.0, 0.0),
        );
        let tight = c.bounds();
        let ctrl = c.control_bounds();
        assert!((tight.max.y - 0.75).abs() < 1e-12, "{tight:?}");
        assert!((ctrl.max.y - 1.0).abs() < 1e-12);
        assert!(tight.height() < ctrl.height());
        // and it really does contain the curve
        for i in 0..=100 {
            assert!(tight.contains(c.eval(i as f64 / 100.0)));
        }
    }

    #[test]
    fn a_line_has_no_extrema_and_an_exact_length() {
        let l = Segment::Line(point(0.0, 0.0), point(3.0, 4.0));
        assert!(l.extrema().is_empty());
        assert_eq!(l.length(1e-9), 5.0);
        assert_eq!(l.bounds(), l.control_bounds());
    }

    #[test]
    fn subdivision_preserves_the_curve() {
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(10.0, 30.0),
            point(40.0, -20.0),
            point(50.0, 10.0),
        );
        let (l, r) = c.split(0.375);
        for i in 0..=50 {
            let t = i as f64 / 50.0;
            assert!(close(l.eval(t), c.eval(t * 0.375)), "left at {t}");
            assert!(close(r.eval(t), c.eval(0.375 + t * 0.625)), "right at {t}");
        }
        let mid = c.subsegment(0.2, 0.8);
        assert!(close(mid.start(), c.eval(0.2)));
        assert!(close(mid.end(), c.eval(0.8)));
        // an inverted or empty range is a point, not a panic
        assert!(close(c.subsegment(0.8, 0.2).start(), c.eval(0.8)));
        assert_eq!(c.subsegment(0.5, 0.5).length(1e-9), 0.0);
    }

    #[test]
    fn cubic_length_matches_a_dense_polyline() {
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(10.0, 30.0),
            point(40.0, -20.0),
            point(50.0, 10.0),
        );
        let mut ref_len = 0.0;
        let n = 200_000;
        let mut prev = c.eval(0.0);
        for i in 1..=n {
            let p = c.eval(i as f64 / n as f64);
            ref_len += prev.distance(p);
            prev = p;
        }
        let got = c.length(1e-9);
        assert!(
            (got - ref_len).abs() < 1e-4,
            "length {got} vs reference {ref_len}"
        );
    }

    #[test]
    fn a_line_converted_to_a_cubic_is_still_the_same_line() {
        let l = Segment::Line(point(1.0, 2.0), point(5.0, -3.0));
        let c = l.to_cubic();
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            assert!(close(l.eval(t), c.eval(t)));
        }
        let q = Segment::Quad(point(0.0, 0.0), point(1.0, 2.0), point(2.0, 0.0));
        let qc = q.to_cubic();
        for i in 0..=20 {
            let t = i as f64 / 20.0;
            assert!(close(q.eval(t), qc.eval(t)), "quad->cubic at {t}");
        }
    }

    #[test]
    fn flattening_gets_tighter_as_the_tolerance_does() {
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(0.0, 100.0),
            point(100.0, 100.0),
            point(100.0, 0.0),
        );
        let mut coarse = vec![c.start()];
        c.flatten_into(2.0, &mut coarse);
        let mut fine = vec![c.start()];
        c.flatten_into(0.01, &mut fine);
        assert!(fine.len() > coarse.len());
        assert!(close(*fine.last().unwrap(), c.end()));

        // Every flattened vertex is within tolerance of the true curve, and the
        // polyline length converges on the analytic one.
        let poly: f64 = fine.windows(2).map(|w| w[0].distance(w[1])).sum();
        assert!((poly - c.length(1e-9)).abs() < 0.01, "poly {poly}");
    }

    #[test]
    fn degenerate_segments_do_not_panic_or_produce_nan() {
        let p = point(3.0, 3.0);
        let zero_line = Segment::Line(p, p);
        assert_eq!(zero_line.length(1e-9), 0.0);
        assert_eq!(zero_line.tangent(0.5), Point::ZERO);
        let mut out = vec![p];
        zero_line.flatten_into(0.1, &mut out);
        assert_eq!(out, vec![p, p]);

        let all_same = Segment::Cubic(p, p, p, p);
        assert_eq!(all_same.length(1e-9), 0.0);
        assert_eq!(all_same.bounds(), Bounds::from_point(p));
        let mut out = vec![p];
        all_same.flatten_into(0.1, &mut out);
        assert!(out.iter().all(|q| *q == p));

        // A cusp: derivative vanishes at t = 0 but the curve does move.
        let cusp = Segment::Cubic(p, p, point(9.0, 3.0), point(9.0, 9.0));
        assert!(cusp.tangent(0.0).length() > 0.9);

        // Non-finite input is dropped, never propagated.
        let nan = Segment::Cubic(p, point(f64::NAN, 0.0), p, p);
        assert!(!nan.is_finite());
        assert_eq!(nan.length(1e-9), 0.0);
        let mut out = Vec::new();
        nan.flatten_into(0.1, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn param_at_length_walks_the_curve_evenly() {
        let c = Segment::Cubic(
            point(0.0, 0.0),
            point(0.0, 60.0),
            point(60.0, 60.0),
            point(60.0, 0.0),
        );
        let total = c.length(1e-9);
        let half = c.param_at_length(total * 0.5, 1e-9);
        assert!((c.subsegment(0.0, half).length(1e-9) - total * 0.5).abs() < 1e-6);

        // On a deliberately lopsided curve, the parameter that reaches half the
        // length is nowhere near 0.5 — which is the whole reason this function
        // exists rather than callers using `eval(0.5)`.
        let lop = Segment::Cubic(
            point(0.0, 0.0),
            point(90.0, 0.0),
            point(100.0, 0.0),
            point(100.0, 0.0),
        );
        let t = lop.param_at_length(50.0, 1e-9);
        assert!(
            t < 0.25,
            "the parameter at half the length is {t}, not far from 0.5"
        );
        assert!((lop.eval(t).x - 50.0).abs() < 1e-6, "{}", lop.eval(t).x);
        assert!((lop.eval(0.5).x - 83.75).abs() < 1e-9);

        assert_eq!(c.param_at_length(-1.0, 1e-9), 0.0);
        assert_eq!(c.param_at_length(total * 2.0, 1e-9), 1.0);
        let p = point(1.0, 1.0);
        assert_eq!(Segment::Line(p, p).param_at_length(1.0, 1e-9), 0.0);
    }

    #[test]
    fn the_quadratic_solver_is_stable_when_the_roots_are_far_apart() {
        // x^2 - 1e8 x + 1 has roots ~1e8 and ~1e-8; the naive formula loses the
        // small one entirely.
        let mut r = solve_quadratic(1.0, -1e8, 1.0);
        r.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((r[0] - 1e-8).abs() < 1e-16, "{r:?}");
        assert!((r[1] - 1e8).abs() < 1.0, "{r:?}");
        assert!(solve_quadratic(1.0, 0.0, 1.0).is_empty());
        assert!(solve_quadratic(0.0, 0.0, 5.0).is_empty());
        assert_eq!(solve_quadratic(0.0, 2.0, -4.0), vec![2.0]);
    }
}
