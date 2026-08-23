//! The shape primitives, as paths.
//!
//! Every shape tool in the editor produces a [`Path`] here rather than its own
//! private geometry, so a rectangle and a hand-drawn pen stroke are the same
//! kind of object from the moment they exist: both can be stroked, filled,
//! booleaned, hit-tested and turned into a selection by the same code, and a
//! shape stays editable as a shape because its parameters regenerate the path.
//!
//! # Orientation
//! Every closed primitive here is emitted in **positive** orientation — a
//! counter-clockwise ring in a y-up frame, so [`Path::signed_area2`] is
//! positive. That matters because nonzero filling is defined on winding
//! direction: two primitives that disagreed about orientation would punch a
//! hole in each other when combined into one path instead of merging.
//! [`primitives_all_wind_the_same_way`] holds it.
//!
//! [`primitives_all_wind_the_same_way`]: #

use std::f64::consts::{FRAC_PI_2, PI, TAU};

use serde::{Deserialize, Serialize};

use crate::path::{Path, PathEl};
use crate::point::{point, Bounds, Point};

/// The handle length, as a fraction of the radius, that makes a cubic Bezier
/// approximate a quarter circle.
///
/// `4/3 * tan(pi/8)`. The maximum radial error of the resulting arc is about
/// 0.027% of the radius — well under a thousandth of a pixel on any circle an
/// editor draws.
pub const KAPPA: f64 = 0.552_284_749_830_793_4;

/// The four corner radii of a rounded rectangle, named by screen position with
/// y pointing down.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct CornerRadii {
    /// Corner at `(min.x, min.y)`.
    pub top_left: f64,
    /// Corner at `(max.x, min.y)`.
    pub top_right: f64,
    /// Corner at `(max.x, max.y)`.
    pub bottom_right: f64,
    /// Corner at `(min.x, max.y)`.
    pub bottom_left: f64,
}

impl CornerRadii {
    /// The same radius on all four corners.
    pub fn uniform(r: f64) -> Self {
        Self {
            top_left: r,
            top_right: r,
            bottom_right: r,
            bottom_left: r,
        }
    }

    /// Radii per corner, clockwise from the top left.
    pub fn new(top_left: f64, top_right: f64, bottom_right: f64, bottom_left: f64) -> Self {
        Self {
            top_left,
            top_right,
            bottom_right,
            bottom_left,
        }
    }

    /// Negative and non-finite radii clamped to zero, then scaled down
    /// uniformly until no pair of radii overflows the side they share.
    ///
    /// Uniform scaling, not per-corner clamping: shrinking only the offending
    /// corner changes the shape's proportions asymmetrically, which is visibly
    /// wrong when a user drags a corner-radius handle past the halfway point.
    /// This is the rule CSS uses for the same reason.
    fn resolved(self, w: f64, h: f64) -> Self {
        let c = |v: f64| if v.is_finite() && v > 0.0 { v } else { 0.0 };
        let (tl, tr, br, bl) = (
            c(self.top_left),
            c(self.top_right),
            c(self.bottom_right),
            c(self.bottom_left),
        );
        let ratio = |sum: f64, side: f64| if sum > 0.0 { side / sum } else { f64::INFINITY };
        let f = ratio(tl + tr, w)
            .min(ratio(bl + br, w))
            .min(ratio(tl + bl, h))
            .min(ratio(tr + br, h))
            .min(1.0);
        Self {
            top_left: tl * f,
            top_right: tr * f,
            bottom_right: br * f,
            bottom_left: bl * f,
        }
    }
}

/// How an arrow's head and shaft are proportioned.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ArrowStyle {
    /// Width of the shaft.
    pub shaft_width: f64,
    /// Length of the head along the arrow's axis.
    pub head_length: f64,
    /// Width of the head across the axis.
    pub head_width: f64,
}

impl Default for ArrowStyle {
    fn default() -> Self {
        Self {
            shaft_width: 2.0,
            head_length: 10.0,
            head_width: 8.0,
        }
    }
}

/// An axis-aligned rectangle.
pub fn rect(b: Bounds) -> Path {
    let mut p = Path::new();
    if b.is_empty() || !b.min.is_finite() || !b.max.is_finite() {
        return p;
    }
    p.move_to(b.min)
        .line_to(point(b.max.x, b.min.y))
        .line_to(b.max)
        .line_to(point(b.min.x, b.max.y))
        .close();
    p
}

/// A rectangle with independently rounded corners.
pub fn rounded_rect(b: Bounds, radii: CornerRadii) -> Path {
    if b.is_empty() || !b.min.is_finite() || !b.max.is_finite() {
        return Path::new();
    }
    let (w, h) = (b.width(), b.height());
    let r = radii.resolved(w, h);
    if r == CornerRadii::default() {
        return rect(b);
    }
    let (x0, y0, x1, y1) = (b.min.x, b.min.y, b.max.x, b.max.y);
    let mut p = Path::new();
    p.move_to(point(x0 + r.top_left, y0));
    // top edge, then the top-right corner
    p.line_to(point(x1 - r.top_right, y0));
    corner(
        &mut p,
        point(x1 - r.top_right, y0),
        point(x1, y0 + r.top_right),
        point(x1, y0),
        r.top_right,
    );
    // right edge, bottom-right corner
    p.line_to(point(x1, y1 - r.bottom_right));
    corner(
        &mut p,
        point(x1, y1 - r.bottom_right),
        point(x1 - r.bottom_right, y1),
        point(x1, y1),
        r.bottom_right,
    );
    // bottom edge, bottom-left corner
    p.line_to(point(x0 + r.bottom_left, y1));
    corner(
        &mut p,
        point(x0 + r.bottom_left, y1),
        point(x0, y1 - r.bottom_left),
        point(x0, y1),
        r.bottom_left,
    );
    // left edge, top-left corner
    p.line_to(point(x0, y0 + r.top_left));
    corner(
        &mut p,
        point(x0, y0 + r.top_left),
        point(x0 + r.top_left, y0),
        point(x0, y0),
        r.top_left,
    );
    p.close();
    p
}

/// One quarter-circle corner from `from` to `to`, bulging towards `pivot`.
fn corner(p: &mut Path, from: Point, to: Point, pivot: Point, r: f64) {
    if r <= 0.0 {
        p.line_to(to);
        return;
    }
    p.push(PathEl::CurveTo(
        from.lerp(pivot, KAPPA),
        to.lerp(pivot, KAPPA),
        to,
    ));
}

/// An axis-aligned ellipse, as four cubic quarter-arcs.
pub fn ellipse(center: Point, radii: Point) -> Path {
    let (rx, ry) = (radii.x.abs(), radii.y.abs());
    let mut p = Path::new();
    if !center.is_finite() || !rx.is_finite() || !ry.is_finite() || rx == 0.0 || ry == 0.0 {
        return p;
    }
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    let (cx, cy) = (center.x, center.y);
    p.move_to(point(cx + rx, cy));
    p.push(PathEl::CurveTo(
        point(cx + rx, cy + ky),
        point(cx + kx, cy + ry),
        point(cx, cy + ry),
    ));
    p.push(PathEl::CurveTo(
        point(cx - kx, cy + ry),
        point(cx - rx, cy + ky),
        point(cx - rx, cy),
    ));
    p.push(PathEl::CurveTo(
        point(cx - rx, cy - ky),
        point(cx - kx, cy - ry),
        point(cx, cy - ry),
    ));
    p.push(PathEl::CurveTo(
        point(cx + kx, cy - ry),
        point(cx + rx, cy - ky),
        point(cx + rx, cy),
    ));
    p.close();
    p
}

/// A circle: the ellipse whose radii are equal.
pub fn circle(center: Point, radius: f64) -> Path {
    ellipse(center, point(radius, radius))
}

/// The ellipse inscribed in a box — what a shape tool drags out.
pub fn ellipse_in(b: Bounds) -> Path {
    if b.is_empty() {
        return Path::new();
    }
    ellipse(b.center(), point(b.width() * 0.5, b.height() * 0.5))
}

/// A regular polygon with `sides` vertices on a circle of `radius`.
///
/// `rotation` is in radians; at zero the first vertex points straight up (in
/// screen terms, with y down), which is what every shape tool's polygon does.
/// Fewer than three sides encloses nothing, so it returns an empty path rather
/// than a degenerate sliver.
pub fn regular_polygon(center: Point, radius: f64, sides: u32, rotation: f64) -> Path {
    if sides < 3 || !radius.is_finite() || radius <= 0.0 || !center.is_finite() {
        return Path::new();
    }
    let verts: Vec<Point> = (0..sides)
        .map(|i| {
            let a = rotation - FRAC_PI_2 + TAU * i as f64 / sides as f64;
            center + point(a.cos(), a.sin()) * radius
        })
        .collect();
    Path::from_polyline(&verts, true)
}

/// A star with `points` spikes, alternating between two radii.
pub fn star(
    center: Point,
    outer_radius: f64,
    inner_radius: f64,
    points: u32,
    rotation: f64,
) -> Path {
    if points < 2
        || !outer_radius.is_finite()
        || !inner_radius.is_finite()
        || outer_radius <= 0.0
        || inner_radius < 0.0
        || !center.is_finite()
    {
        return Path::new();
    }
    let n = points as usize * 2;
    let verts: Vec<Point> = (0..n)
        .map(|i| {
            let r = if i % 2 == 0 {
                outer_radius
            } else {
                inner_radius
            };
            let a = rotation - FRAC_PI_2 + PI * i as f64 / points as f64;
            center + point(a.cos(), a.sin()) * r
        })
        .collect();
    Path::from_polyline(&verts, true)
}

/// The self-intersecting five-pointed star, `{5/2}`: a pentagon's vertices
/// joined every second one.
///
/// Kept as its own primitive because it is the canonical shape on which the two
/// fill rules disagree — its centre pentagon is wound twice, so nonzero fills it
/// and even-odd leaves it hollow.
pub fn pentagram(center: Point, radius: f64, rotation: f64) -> Path {
    if !radius.is_finite() || radius <= 0.0 || !center.is_finite() {
        return Path::new();
    }
    let verts: Vec<Point> = (0..5)
        .map(|k| {
            let i = (k * 2) % 5;
            let a = rotation - FRAC_PI_2 + TAU * i as f64 / 5.0;
            center + point(a.cos(), a.sin()) * radius
        })
        .collect();
    Path::from_polyline(&verts, true)
}

/// A single open segment — the line tool.
pub fn line(a: Point, b: Point) -> Path {
    let mut p = Path::new();
    if !a.is_finite() || !b.is_finite() {
        return p;
    }
    p.move_to(a).line_to(b);
    p
}

/// A filled arrow from `from` to `to`, as one closed outline.
///
/// The head is clamped to the arrow's own length, so a short arrow becomes all
/// head rather than folding back through itself.
pub fn arrow(from: Point, to: Point, style: ArrowStyle) -> Path {
    let mut p = Path::new();
    if !from.is_finite() || !to.is_finite() {
        return p;
    }
    let axis = to - from;
    let len = axis.length();
    if len <= 0.0 {
        return p;
    }
    let dir = axis / len;
    // `-perp` rather than `perp`, so the outline is traced with the interior on
    // the left and comes out positively oriented like every other primitive
    // here. With `perp` the arrow would wind backwards and punch a hole in any
    // path it was combined with.
    let normal = -dir.perp();
    let head_len = style.head_length.max(0.0).min(len);
    let head_w = style.head_width.max(0.0);
    let shaft_w = style.shaft_width.max(0.0).min(head_w);
    let base = to - dir * head_len;
    let (hs, hh) = (shaft_w * 0.5, head_w * 0.5);

    p.move_to(from + normal * hs)
        .line_to(base + normal * hs)
        .line_to(base + normal * hh)
        .line_to(to)
        .line_to(base - normal * hh)
        .line_to(base - normal * hs)
        .line_to(from - normal * hs)
        .close();
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{fill, FillOptions};
    use crate::hit::contains;
    use crate::FillRule;

    /// Rasterised area, flattened far tighter than the default so the
    /// measurement is of the shape and not of the polygon standing in for it.
    fn area(p: &Path) -> f64 {
        fill(
            p,
            &FillOptions {
                tolerance: 0.001,
                ..FillOptions::default()
            },
        )
        .unwrap()
        .area()
    }

    #[test]
    fn a_rectangle_is_its_own_bounds_and_its_own_area() {
        let b = Bounds::from_xywh(2.0, 3.0, 10.0, 6.0);
        let p = rect(b);
        assert_eq!(p.bounds(), b);
        assert_eq!(area(&p), 60.0);
        assert_eq!(p.subpaths().len(), 1);
        assert!(p.subpaths()[0].closed);
    }

    #[test]
    fn primitives_all_wind_the_same_way() {
        // If two primitives disagreed, combining them into one path under the
        // nonzero rule would make one punch a hole in the other.
        let shapes = [
            rect(Bounds::from_xywh(0.0, 0.0, 10.0, 10.0)),
            rounded_rect(
                Bounds::from_xywh(0.0, 0.0, 10.0, 10.0),
                CornerRadii::uniform(2.0),
            ),
            ellipse(point(0.0, 0.0), point(5.0, 3.0)),
            regular_polygon(point(0.0, 0.0), 5.0, 6, 0.0),
            star(point(0.0, 0.0), 5.0, 2.0, 5, 0.0),
            pentagram(point(0.0, 0.0), 5.0, 0.0),
            arrow(point(0.0, 0.0), point(10.0, 0.0), ArrowStyle::default()),
        ];
        for (i, s) in shapes.iter().enumerate() {
            assert!(
                s.signed_area2(0.01) > 0.0,
                "primitive {i} is wound the other way"
            );
        }
    }

    #[test]
    fn rounded_corners_shrink_the_area_by_the_corner_squares() {
        let b = Bounds::from_xywh(0.0, 0.0, 40.0, 40.0);
        let r = 10.0;
        let p = rounded_rect(b, CornerRadii::uniform(r));
        assert_eq!(p.bounds(), b);
        // Each corner loses (1 - pi/4) r^2.
        let expected = 1600.0 - 4.0 * (1.0 - PI / 4.0) * r * r;
        assert!((area(&p) - expected).abs() < 0.5, "{}", area(&p));
        // A zero radius is exactly the sharp rectangle.
        assert_eq!(rounded_rect(b, CornerRadii::default()), rect(b));
    }

    #[test]
    fn per_corner_radii_round_only_the_corners_they_name() {
        let b = Bounds::from_xywh(0.0, 0.0, 40.0, 40.0);
        let p = rounded_rect(b, CornerRadii::new(20.0, 0.0, 0.0, 0.0));
        assert_eq!(p.bounds(), b);
        // The top-left corner point is cut away; the other three survive.
        assert!(!contains(&p, point(0.5, 0.5), FillRule::NonZero));
        assert!(contains(&p, point(39.5, 0.5), FillRule::NonZero));
        assert!(contains(&p, point(39.5, 39.5), FillRule::NonZero));
        assert!(contains(&p, point(0.5, 39.5), FillRule::NonZero));
    }

    #[test]
    fn oversized_radii_are_scaled_down_uniformly_rather_than_folding_over() {
        let b = Bounds::from_xywh(0.0, 0.0, 20.0, 10.0);
        // Radii that would each want half the width, on a box only half as tall.
        let p = rounded_rect(b, CornerRadii::uniform(50.0));
        assert_eq!(p.bounds(), b, "the shape must still fit its box exactly");
        // Clamped to h/2 = 5 on every corner: a stadium, so its area is the
        // rectangle minus four (1 - pi/4) * 25 corners.
        let expected = 200.0 - 4.0 * (1.0 - PI / 4.0) * 25.0;
        assert!((area(&p) - expected).abs() < 0.5, "{}", area(&p));
        // Negative and non-finite radii are treated as zero, not as a panic.
        let sharp = rounded_rect(b, CornerRadii::new(-5.0, f64::NAN, f64::INFINITY, 0.0));
        assert_eq!(sharp, rect(b));
    }

    #[test]
    fn an_ellipse_fills_its_box_and_encloses_pi_a_b() {
        let e = ellipse(point(50.0, 40.0), point(30.0, 20.0));
        let b = e.bounds();
        assert!((b.min.x - 20.0).abs() < 1e-9 && (b.max.x - 80.0).abs() < 1e-9);
        assert!((b.min.y - 20.0).abs() < 1e-9 && (b.max.y - 60.0).abs() < 1e-9);
        let expected = PI * 30.0 * 20.0;
        assert!(
            (area(&e) - expected).abs() / expected < 0.002,
            "{} vs {expected}",
            area(&e)
        );
        assert_eq!(ellipse_in(Bounds::from_xywh(20.0, 20.0, 60.0, 40.0)), e);
    }

    #[test]
    fn a_regular_polygon_has_its_vertices_on_the_circle() {
        let c = point(10.0, 10.0);
        let p = regular_polygon(c, 7.0, 8, 0.3);
        let pl = &p.flatten(0.001)[0];
        assert_eq!(pl.points.len(), 8);
        for v in &pl.points {
            assert!((v.distance(c) - 7.0).abs() < 1e-9);
        }
        // Area of a regular n-gon: (n/2) r^2 sin(2 pi / n).
        let expected = 4.0 * 49.0 * (TAU / 8.0).sin();
        assert!((pl.signed_area2() * 0.5 - expected).abs() < 1e-9);
    }

    #[test]
    fn a_star_alternates_between_its_two_radii() {
        let c = point(0.0, 0.0);
        let s = star(c, 10.0, 4.0, 5, 0.0);
        let pl = &s.flatten(0.001)[0];
        assert_eq!(pl.points.len(), 10);
        for (i, v) in pl.points.iter().enumerate() {
            let want = if i % 2 == 0 { 10.0 } else { 4.0 };
            assert!((v.distance(c) - want).abs() < 1e-9, "vertex {i}");
        }
        // The first spike points "up" in screen terms.
        assert!((pl.points[0] - point(0.0, -10.0)).length() < 1e-9);
    }

    #[test]
    fn a_pentagram_is_the_shape_the_fill_rules_disagree_on() {
        let g = pentagram(point(0.0, 0.0), 10.0, 0.0);
        assert_eq!(g.flatten(0.001)[0].points.len(), 5);
        assert!(contains(&g, Point::ZERO, FillRule::NonZero));
        assert!(!contains(&g, Point::ZERO, FillRule::EvenOdd));
    }

    /// The `rotation` argument must actually rotate. Every other test of these
    /// three primitives asserts something rotation-invariant — vertices on the
    /// circle, the n-gon area formula, the winding sign, or the shape against a
    /// rasterisation of itself — so deleting the term outright would leave them
    /// all green.
    #[test]
    fn rotation_turns_the_vertices_off_the_top() {
        let c = point(3.0, -7.0);
        let r = 10.0;
        // Documented rule: at rotation zero the first vertex points straight up,
        // which with y down is the angle -pi/2 from the +x axis. At `rot` it is
        // therefore at angle `rot - pi/2`, i.e. `(sin rot, -cos rot) * r`.
        // Derived from the doc comment, not read off the expression under test.
        let first_vertex_at = |rot: f64| c + point(rot.sin(), -rot.cos()) * r;

        for rot in [0.0, FRAC_PI_2, 0.3, -1.4, TAU + 0.75] {
            let want = first_vertex_at(rot);
            for (name, p) in [
                ("regular_polygon", regular_polygon(c, r, 4, rot)),
                ("star", star(c, r, 4.0, 5, rot)),
                ("pentagram", pentagram(c, r, rot)),
            ] {
                let got = p.flatten(0.001)[0].points[0];
                assert!(
                    (got - want).length() < 1e-9,
                    "{name} at rotation {rot}: got {got:?}, wanted {want:?}"
                );
            }
        }

        // Concretely, and independent of the closure: a quarter turn takes the
        // vertex at the top of a square to the right of it.
        let quarter = regular_polygon(Point::ZERO, 10.0, 4, FRAC_PI_2);
        let v0 = quarter.flatten(0.001)[0].points[0];
        assert!((v0 - point(10.0, 0.0)).length() < 1e-9, "{v0:?}");

        // And it turns *every* vertex, not only the first. 0.3 rad is not a
        // symmetry of any of these shapes, so the whole vertex set has to move;
        // a quarter turn maps a square onto itself and would prove nothing.
        for (name, a, b) in [
            (
                "regular_polygon",
                regular_polygon(c, r, 4, 0.0),
                regular_polygon(c, r, 4, 0.3),
            ),
            ("star", star(c, r, 4.0, 5, 0.0), star(c, r, 4.0, 5, 0.3)),
            ("pentagram", pentagram(c, r, 0.0), pentagram(c, r, 0.3)),
        ] {
            let unrotated = a.flatten(0.001)[0].points.clone();
            let rotated = b.flatten(0.001)[0].points.clone();
            assert_eq!(unrotated.len(), rotated.len(), "{name}");
            for (i, (x, y)) in unrotated.iter().zip(rotated.iter()).enumerate() {
                assert!(x.distance(*y) > 1e-6, "{name} vertex {i} did not move");
                // Rotation about the centre keeps every radius exactly.
                assert!(
                    (x.distance(c) - y.distance(c)).abs() < 1e-9,
                    "{name} vertex {i} changed radius"
                );
            }
        }
    }

    #[test]
    fn a_line_is_open_and_as_long_as_it_looks() {
        let l = line(point(0.0, 0.0), point(3.0, 4.0));
        assert_eq!(l.length(), 5.0);
        assert!(!l.subpaths()[0].closed);
        assert_eq!(l.subpaths()[0].segments.len(), 1);
    }

    #[test]
    fn an_arrow_reaches_its_tip_and_is_widest_at_the_head() {
        let style = ArrowStyle {
            shaft_width: 4.0,
            head_length: 12.0,
            head_width: 16.0,
        };
        let a = arrow(point(0.0, 50.0), point(100.0, 50.0), style);
        let b = a.bounds();
        assert!((b.max.x - 100.0).abs() < 1e-9, "the tip must be reached");
        assert!((b.min.x - 0.0).abs() < 1e-9);
        assert!(
            (b.height() - 16.0).abs() < 1e-9,
            "head width sets the height"
        );
        // Shaft is thin, head is wide.
        assert!(contains(&a, point(10.0, 51.0), FillRule::NonZero));
        assert!(!contains(&a, point(10.0, 56.0), FillRule::NonZero));
        assert!(contains(&a, point(90.0, 55.0), FillRule::NonZero));
        // A head longer than the arrow is clamped, not folded back.
        let stub = arrow(
            point(0.0, 0.0),
            point(5.0, 0.0),
            ArrowStyle {
                head_length: 100.0,
                ..style
            },
        );
        assert!((stub.bounds().width() - 5.0).abs() < 1e-9);
        assert!(stub.signed_area2(0.01) > 0.0);
    }

    #[test]
    fn degenerate_shape_parameters_return_an_empty_path_not_a_panic() {
        assert!(rect(Bounds::EMPTY).is_empty());
        assert!(rounded_rect(Bounds::EMPTY, CornerRadii::uniform(3.0)).is_empty());
        assert!(ellipse(point(0.0, 0.0), point(0.0, 5.0)).is_empty());
        assert!(ellipse(point(f64::NAN, 0.0), point(1.0, 1.0)).is_empty());
        assert!(regular_polygon(point(0.0, 0.0), 5.0, 2, 0.0).is_empty());
        assert!(regular_polygon(point(0.0, 0.0), -5.0, 6, 0.0).is_empty());
        assert!(star(point(0.0, 0.0), 5.0, 2.0, 1, 0.0).is_empty());
        assert!(pentagram(point(0.0, 0.0), f64::INFINITY, 0.0).is_empty());
        assert!(line(point(f64::NAN, 0.0), point(1.0, 1.0)).is_empty());
        assert!(arrow(point(1.0, 1.0), point(1.0, 1.0), ArrowStyle::default()).is_empty());

        // A NaN size collapses to a zero-extent box rather than producing NaN
        // geometry that would poison every later bound.
        let nan_rect = rect(Bounds::from_xywh(0.0, 0.0, f64::NAN, 1.0));
        assert!(nan_rect.is_finite());
        assert_eq!(area(&nan_rect), 0.0);
        // A zero-width rectangle encloses nothing, and says so without panicking.
        assert_eq!(area(&rect(Bounds::from_xywh(1.0, 1.0, 0.0, 5.0))), 0.0);
    }
}
