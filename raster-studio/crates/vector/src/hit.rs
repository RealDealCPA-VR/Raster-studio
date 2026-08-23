//! Hit testing: is this point in the shape, and is it on the outline?
//!
//! The selection tools need two different questions answered, and confusing
//! them is how a vector editor ends up feeling wrong. Clicking *inside* a
//! filled shape selects it; clicking *near its outline* selects it too, even
//! when the shape has no fill at all — otherwise an unfilled rectangle is
//! impossible to click. [`contains`] answers the first, [`hit_stroke`] the
//! second, and a tool normally asks both.
//!
//! # Where the boundary belongs
//! A point exactly on the outline counts as **inside**. That is a decision, not
//! an accident: the alternative is a half-open rule where a shape's left edge
//! is clickable and its right edge is not, which is invisible in a test suite
//! and infuriating in a UI. [`contains_with`] exposes the epsilon for callers
//! that need the strict topological answer.
//!
//! # Curves are tested against their flattened form
//! Distance to a Bezier has no closed form, so both questions are answered
//! against a polyline flattened to a tolerance the caller controls. The answer
//! is therefore correct to within that tolerance near the boundary, and exact
//! everywhere else.

use crate::fill::FillRule;
use crate::path::{Path, Polyline};
use crate::point::Point;

/// How close to the outline still counts as "on" it, in path units.
///
/// Small enough to be exact arithmetic rather than a fudge factor: it exists so
/// a point computed as being on an edge is not excluded by a one-ulp rounding
/// difference, not to give the boundary a thickness.
pub const EDGE_EPSILON: f64 = 1e-9;

/// `true` when `p` is inside the region the path fills under `rule`.
///
/// A point on the outline counts as inside; see [`contains_with`] to change
/// that.
pub fn contains(path: &Path, p: Point, rule: FillRule) -> bool {
    contains_with(path, p, rule, EDGE_EPSILON, crate::DEFAULT_TOLERANCE)
}

/// [`contains`] with the boundary epsilon and flattening tolerance chosen.
///
/// Passing `edge_epsilon = 0.0` (or negative) gives the strict rule: the
/// boundary itself is decided by the winding test alone, which is what a
/// scan converter does.
pub fn contains_with(
    path: &Path,
    p: Point,
    rule: FillRule,
    edge_epsilon: f64,
    tolerance: f64,
) -> bool {
    if !p.is_finite() {
        return false;
    }
    let rings = path.flatten_closed(tolerance);
    if rings.is_empty() {
        return false;
    }
    if edge_epsilon > 0.0 && distance_to_rings(&rings, p) <= edge_epsilon {
        return true;
    }
    match rule {
        FillRule::NonZero => winding_number_of(&rings, p) != 0,
        FillRule::EvenOdd => crossing_parity(&rings, p),
    }
}

/// The signed number of times the path winds around `p`.
///
/// Zero means outside under the nonzero rule; the magnitude is what
/// distinguishes a pentagram's twice-wound centre from its once-wound spikes.
pub fn winding_number(path: &Path, p: Point, tolerance: f64) -> i32 {
    winding_number_of(&path.flatten_closed(tolerance), p)
}

/// `true` when `p` is within `tolerance` of the path's outline.
///
/// Open subpaths are tested as drawn — an open path has an outline but no
/// closing edge — which is exactly the geometry a stroke would paint.
pub fn hit_stroke(path: &Path, p: Point, tolerance: f64) -> bool {
    if !p.is_finite() || tolerance.is_nan() || tolerance < 0.0 {
        return false;
    }
    distance_to_outline(path, p, crate::DEFAULT_TOLERANCE) <= tolerance
}

/// Distance from `p` to the nearest point of the path's outline, open subpaths
/// left open.
///
/// `f64::INFINITY` for a path with no geometry at all.
pub fn distance_to_outline(path: &Path, p: Point, tolerance: f64) -> f64 {
    let polys = path.flatten(tolerance);
    distance_to_rings(&polys, p)
}

/// The point on the path's outline nearest `p`, and its distance.
pub fn nearest_point(path: &Path, p: Point, tolerance: f64) -> Option<(Point, f64)> {
    let polys = path.flatten(tolerance);
    let mut best: Option<(Point, f64)> = None;
    for poly in &polys {
        if poly.points.len() == 1 {
            let d = p.distance(poly.points[0]);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((poly.points[0], d));
            }
            continue;
        }
        for (a, b) in poly.edges() {
            let q = closest_on_segment(p, a, b);
            let d = p.distance(q);
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((q, d));
            }
        }
    }
    best
}

/// Point-in-region on already-flattened rings.
///
/// The boolean ops classify thousands of edge midpoints against the same two
/// ring sets, so they need the test without re-flattening the path each time.
pub(crate) fn point_in_rings(rings: &[Polyline], p: Point, rule: FillRule) -> bool {
    match rule {
        FillRule::NonZero => winding_number_of(rings, p) != 0,
        FillRule::EvenOdd => crossing_parity(rings, p),
    }
}

fn distance_to_rings(polys: &[Polyline], p: Point) -> f64 {
    let mut best = f64::INFINITY;
    for poly in polys {
        if poly.points.len() == 1 {
            best = best.min(p.distance(poly.points[0]));
            continue;
        }
        for (a, b) in poly.edges() {
            best = best.min(p.distance(closest_on_segment(p, a, b)));
        }
    }
    best
}

fn closest_on_segment(p: Point, a: Point, b: Point) -> Point {
    let ab = b - a;
    let l2 = ab.length_squared();
    if l2 <= 0.0 {
        return a;
    }
    let t = ((p - a).dot(ab) / l2).clamp(0.0, 1.0);
    a + ab * t
}

/// The winding-number algorithm: every edge that crosses the horizontal line
/// through `p` on the correct side contributes its own direction.
///
/// The `<=` / `>` asymmetry is what makes a vertex lying exactly on that line
/// count once rather than zero or twice, which is the classic source of
/// single-pixel holes at a polygon's horizontal tangents. `(b - a).cross(p - a)`
/// is positive when `p` lies to the left of the directed edge.
fn winding_number_of(polys: &[Polyline], p: Point) -> i32 {
    let mut w = 0;
    for poly in polys {
        for (a, b) in poly.edges() {
            if a.y <= p.y {
                // upward crossing, with `p` to its left
                if b.y > p.y && (b - a).cross(p - a) > 0.0 {
                    w += 1;
                }
            } else if b.y <= p.y && (b - a).cross(p - a) < 0.0 {
                // downward crossing, with `p` to its right
                w -= 1;
            }
        }
    }
    w
}

/// Even-odd: parity of the crossings of a ray in +x.
fn crossing_parity(polys: &[Polyline], p: Point) -> bool {
    let mut inside = false;
    for poly in polys {
        for (a, b) in poly.edges() {
            if (a.y > p.y) != (b.y > p.y) {
                let x = a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x);
                if p.x < x {
                    inside = !inside;
                }
            }
        }
    }
    inside
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::{point, Bounds};
    use crate::shapes;

    #[test]
    fn a_rectangle_is_hit_inside_missed_outside_and_hit_exactly_on_its_edge() {
        let r = shapes::rect(Bounds::from_xywh(0.0, 0.0, 10.0, 10.0));
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            // inside
            assert!(contains(&r, point(5.0, 5.0), rule), "{rule:?} inside");
            assert!(
                contains(&r, point(0.001, 9.999), rule),
                "{rule:?} corner-ish"
            );
            // outside
            assert!(!contains(&r, point(-0.001, 5.0), rule), "{rule:?} left");
            assert!(!contains(&r, point(10.001, 5.0), rule), "{rule:?} right");
            assert!(!contains(&r, point(5.0, -3.0), rule), "{rule:?} above");
            assert!(!contains(&r, point(100.0, 100.0), rule), "{rule:?} far");
            // exactly on every edge and every corner
            for p in [
                point(0.0, 5.0),
                point(10.0, 5.0),
                point(5.0, 0.0),
                point(5.0, 10.0),
                point(0.0, 0.0),
                point(10.0, 0.0),
                point(10.0, 10.0),
                point(0.0, 10.0),
            ] {
                assert!(contains(&r, p, rule), "{rule:?} on the edge at {p:?}");
            }
        }
    }

    #[test]
    fn the_strict_rule_is_available_and_really_is_different() {
        // With the epsilon switched off, the boundary is decided by the winding
        // test alone: the left and top edges land inside, the right and bottom
        // outside. That asymmetry is why it is not the default.
        let r = shapes::rect(Bounds::from_xywh(0.0, 0.0, 10.0, 10.0));
        let strict = |p| contains_with(&r, p, FillRule::NonZero, 0.0, crate::DEFAULT_TOLERANCE);
        assert!(strict(point(0.0, 5.0)));
        assert!(!strict(point(10.0, 5.0)));
        assert!(contains(&r, point(10.0, 5.0), FillRule::NonZero));
    }

    #[test]
    fn winding_number_counts_a_pentagrams_centre_twice() {
        let g = shapes::pentagram(point(0.0, 0.0), 50.0, 0.0);
        assert_eq!(winding_number(&g, Point::ZERO, 0.01).abs(), 2);
        // A spike is wound once...
        assert_eq!(winding_number(&g, point(0.0, -45.0), 0.01).abs(), 1);
        // ...and outside is zero.
        assert_eq!(winding_number(&g, point(100.0, 100.0), 0.01), 0);
        // which is exactly where the two rules part company
        assert!(contains(&g, Point::ZERO, FillRule::NonZero));
        assert!(!contains(&g, Point::ZERO, FillRule::EvenOdd));
        assert!(contains(&g, point(0.0, -45.0), FillRule::EvenOdd));
    }

    #[test]
    fn a_hole_is_outside_under_both_rules() {
        let mut p = shapes::rect(Bounds::from_xywh(0.0, 0.0, 20.0, 20.0));
        p.extend(&shapes::rect(Bounds::from_xywh(5.0, 5.0, 10.0, 10.0)).reversed());
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            assert!(!contains(&p, point(10.0, 10.0), rule), "{rule:?}");
            assert!(contains(&p, point(2.0, 10.0), rule), "{rule:?}");
        }
    }

    #[test]
    fn a_circles_boundary_is_hit_to_the_flattening_tolerance() {
        let c = shapes::circle(point(0.0, 0.0), 100.0);
        assert!(contains(&c, point(0.0, 0.0), FillRule::NonZero));
        assert!(contains(&c, point(99.0, 0.0), FillRule::NonZero));
        assert!(!contains(&c, point(101.0, 0.0), FillRule::NonZero));
        // Distance to the outline is the radial distance.
        for p in [point(150.0, 0.0), point(0.0, -150.0), point(30.0, 40.0)] {
            let d = distance_to_outline(&c, p, 0.001);
            assert!(
                (d - (p.length() - 100.0).abs()).abs() < 0.01,
                "{p:?} -> {d}"
            );
        }
    }

    #[test]
    fn an_open_path_has_an_outline_to_click_but_no_closing_edge() {
        // Three sides of a square. The stroke hit test must follow the drawn
        // sides only; the fill test still closes it, because a fill is a
        // question about the region a path bounds.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .line_to(point(10.0, 10.0))
            .line_to(point(0.0, 10.0));

        assert!(hit_stroke(&p, point(5.0, 0.2), 0.5));
        assert!(!hit_stroke(&p, point(5.0, 2.0), 0.5));
        // The missing fourth side is not part of the outline...
        assert!(!hit_stroke(&p, point(0.0, 5.0), 0.5));
        // ...but the region it would enclose still fills.
        assert!(contains(&p, point(5.0, 5.0), FillRule::NonZero));
    }

    #[test]
    fn hit_stroke_measures_real_distance_and_respects_its_tolerance() {
        let l = shapes::line(point(0.0, 0.0), point(100.0, 0.0));
        assert!(hit_stroke(&l, point(50.0, 0.0), 0.0));
        assert!(hit_stroke(&l, point(50.0, 2.9), 3.0));
        assert!(!hit_stroke(&l, point(50.0, 3.1), 3.0));
        // Past the ends it is distance to the endpoint, not to the infinite line.
        assert!(!hit_stroke(&l, point(-5.0, 0.0), 3.0));
        assert!(hit_stroke(&l, point(-2.0, 0.0), 3.0));
        assert!((distance_to_outline(&l, point(-3.0, 4.0), 0.01) - 5.0).abs() < 1e-9);

        let (q, d) = nearest_point(&l, point(30.0, 7.0), 0.01).unwrap();
        assert_eq!(q, point(30.0, 0.0));
        assert_eq!(d, 7.0);
    }

    #[test]
    fn degenerate_input_answers_false_rather_than_panicking() {
        let empty = Path::new();
        assert!(!contains(&empty, Point::ZERO, FillRule::NonZero));
        assert!(!hit_stroke(&empty, Point::ZERO, 100.0));
        assert_eq!(distance_to_outline(&empty, Point::ZERO, 0.1), f64::INFINITY);
        assert_eq!(nearest_point(&empty, Point::ZERO, 0.1), None);

        let mut dot = Path::new();
        dot.move_to(point(4.0, 4.0));
        assert!(!contains(&dot, point(4.0, 4.0), FillRule::NonZero));
        assert!(hit_stroke(&dot, point(4.0, 4.5), 1.0));
        assert!(!hit_stroke(&dot, point(4.0, 9.0), 1.0));
        assert_eq!(
            nearest_point(&dot, point(4.0, 9.0), 0.1),
            Some((point(4.0, 4.0), 5.0))
        );

        let r = shapes::rect(Bounds::from_xywh(0.0, 0.0, 4.0, 4.0));
        assert!(!contains(&r, point(f64::NAN, 1.0), FillRule::NonZero));
        assert!(!hit_stroke(&r, point(f64::NAN, 1.0), 1.0));
        assert!(!hit_stroke(&r, point(1.0, 1.0), f64::NAN));
    }

    #[test]
    fn hit_testing_agrees_with_the_rasteriser() {
        // The two must not drift apart: a click that selects a shape has to be a
        // click on a pixel the shape actually painted.
        use crate::fill::{fill, FillOptions};
        use glam::IVec2;

        let shape = shapes::star(point(60.0, 60.0), 50.0, 20.0, 7, 0.4);
        for rule in [FillRule::NonZero, FillRule::EvenOdd] {
            let m = fill(&shape, &FillOptions::with_rule(rule)).unwrap();
            let at = |x: i32, y: i32| m.coverage_at(IVec2::new(x, y));
            // A pixel only has an unambiguous answer when its whole
            // neighbourhood agrees: an edge pixel legitimately straddles, and a
            // pixel rounded to 255 may still be a fraction short.
            let all = |x: i32, y: i32, v: u8| {
                (-1..=1).all(|dy| (-1..=1).all(|dx| at(x + dx, y + dy) == v))
            };
            let mut checked = 0;
            for y in 0..120i32 {
                for x in 0..120i32 {
                    let p = point(x as f64 + 0.5, y as f64 + 0.5);
                    if all(x, y, 255) {
                        assert!(contains(&shape, p, rule), "{rule:?} solid pixel {p:?}");
                        checked += 1;
                    } else if all(x, y, 0) {
                        assert!(!contains(&shape, p, rule), "{rule:?} empty pixel {p:?}");
                        checked += 1;
                    }
                }
            }
            assert!(checked > 10_000, "only {checked} unambiguous pixels");
        }
    }
}
