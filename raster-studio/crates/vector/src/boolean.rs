//! Boolean operations on paths: union, intersection, difference and xor.
//!
//! # The method, and its price
//! Both operands are flattened to polygons, every crossing between them is
//! found and both are split there, each resulting edge is classified as inside
//! or outside the *other* operand, and the surviving edges are chained back
//! into rings. Edges that lie exactly on top of each other are matched up and
//! resolved by a rule table rather than by classification, because a point on a
//! boundary has no inside/outside answer.
//!
//! The price is stated plainly: **the result is polygonal**. Curves are
//! flattened to the caller's tolerance and are not re-fitted afterwards, so
//! taking the union of two circles gives a many-sided polygon rather than two
//! arcs. Curve-exact boolean ops need the intersections of two cubics, which is
//! a root-finding problem with genuinely hard degenerate cases (tangency,
//! overlap, cusps); a polygon at a tolerance the rasteriser cannot resolve is
//! the same picture with none of that risk. Callers that need the curve back
//! should keep the original path and use the boolean only for the region.
//!
//! # Orientation, in and out
//! Chaining follows `edge.end == next.start`, so it needs both operands to
//! agree about which way round a boundary runs. Nothing guarantees that: this
//! crate's own [`crate::shapes`] are counter-clockwise, but a path pasted in
//! from a file is often clockwise — Illustrator emits it routinely — and
//! [`Path::reversed`] produces it on purpose. So each operand is **normalised
//! first**: if its rings enclose a negative total signed area, every one of them
//! is reversed. Reversing *all* of an operand's rings negates its winding number
//! everywhere, which leaves both the nonzero and the even-odd reading of that
//! operand exactly as it was — the region is untouched, only its handedness is.
//!
//! With both operands normalised, kept edges keep their original direction
//! except where the rule table calls for a reversal, so the outer rings of a
//! result come out positively oriented and holes come out negatively oriented.
//! The result is therefore meant to be filled with [`FillRule::NonZero`],
//! whatever rule was used to interpret the inputs.
//!
//! # Bounded work, not fallible allocation
//! The rasteriser reserves its buffers with `try_reserve` because their size is
//! a caller's extent multiplied out, and there is no cheaper way to know it is
//! affordable. This stage takes the other route: the buffers here are all sized
//! by an edge count, so the edge counts themselves are capped —
//! [`MAX_EDGES`] per operand and on the split total, [`MAX_PAIR_TESTS`] on the
//! crossing search — and each cap is checked *before* the buffers it governs
//! are allocated. An input past a cap is a [`VectorError::TooComplex`], with the
//! limit it hit; nothing here reports [`VectorError::OutOfMemory`].
//!
//! # Known limits
//! * Self-intersections *within* one operand are interpreted by the fill rule
//!   rather than resolved, so the result of `union(pentagram, x)` is correct as
//!   a region under nonzero filling but is not a simple polygon.
//! * Normalisation is per *operand*, not per ring, because only a whole-operand
//!   flip is region-preserving. An operand whose own rings are wound
//!   inconsistently while *overlapping* each other — as opposed to nesting,
//!   which is the ordinary outline-and-hole case and is handled — is the same
//!   unresolved self-intersection as the point above.
//! * Two edges that overlap only partially — collinear and sharing a stretch
//!   rather than sharing both endpoints — are only matched at their crossing
//!   points. In the cases this crate creates (shapes sharing a whole edge) the
//!   split points coincide and the match succeeds; a hand-built path with a
//!   half-overlapping edge can still produce a seam.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::VectorError;
use crate::fill::FillRule;
use crate::hit::point_in_rings;
use crate::path::{Path, Polyline};
use crate::point::Point;

/// Which region of two paths to keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoolOp {
    /// Everything covered by either path.
    Union,
    /// Only what both paths cover.
    Intersection,
    /// The first path minus the second.
    Difference,
    /// Covered by exactly one of the two.
    Xor,
}

/// The most edges one operand may contribute, and the most the split stage may
/// produce, before the operation is refused.
///
/// Two paths of `n` and `m` edges can cross `n * m` times, so a pair of
/// finely-flattened spirals really can blow up. A refusal is an error the
/// editor can report; running out of memory is not.
///
/// The cap is checked **per operand as well as on the total**, and the
/// per-operand check comes first, before any buffer sized by edge count is
/// allocated. [`MAX_PAIR_TESTS`] alone would not do it: it bounds the *product*
/// of the two edge counts, so a single operand flattening to ten million edges
/// against a triangle passes the product test and would allocate hundreds of
/// megabytes of per-edge cut lists before the total-edge check could refuse it.
pub const MAX_EDGES: usize = 200_000;

/// The largest number of edge pairs that will be tested for crossings.
pub const MAX_PAIR_TESTS: usize = 40_000_000;

/// Combine two paths.
///
/// `rule` says how each *input* is to be interpreted — which points each path
/// considers inside. The output is always meant to be filled with
/// [`FillRule::NonZero`].
pub fn boolean(
    a: &Path,
    b: &Path,
    op: BoolOp,
    rule: FillRule,
    tolerance: f64,
) -> Result<Path, VectorError> {
    let tol = if tolerance.is_finite() && tolerance > 0.0 {
        tolerance
    } else {
        crate::DEFAULT_TOLERANCE
    };
    let mut ra = a.flatten_closed(tol);
    let mut rb = b.flatten_closed(tol);
    normalise_orientation(&mut ra);
    normalise_orientation(&mut rb);

    // An empty operand has a closed-form answer, and going through the general
    // machinery for it would only invent chances to be wrong.
    match (ra.is_empty(), rb.is_empty()) {
        (true, true) => return Ok(Path::new()),
        (true, false) => {
            return Ok(match op {
                BoolOp::Union | BoolOp::Xor => rings_to_path(&rb),
                BoolOp::Intersection | BoolOp::Difference => Path::new(),
            })
        }
        (false, true) => {
            return Ok(match op {
                BoolOp::Union | BoolOp::Xor | BoolOp::Difference => rings_to_path(&ra),
                BoolOp::Intersection => Path::new(),
            })
        }
        (false, false) => {}
    }

    let eps = snap_epsilon(&ra, &rb);
    let mut snapper = Snapper::new(eps);

    let mut ea = collect_edges(&ra, &mut snapper);
    let mut eb = collect_edges(&rb, &mut snapper);
    // Per operand first, and before anything sized by edge count is allocated:
    // the pair-test cap below bounds only the *product*, so one enormous
    // operand against a triangle would sail past it and then reserve a cut list
    // per edge on the way to the total-edge check.
    if ea.len() > MAX_EDGES || eb.len() > MAX_EDGES {
        return Err(VectorError::TooComplex {
            what: "edges in one boolean operand",
            limit: MAX_EDGES,
        });
    }
    if ea.len().saturating_mul(eb.len()) > MAX_PAIR_TESTS {
        return Err(VectorError::TooComplex {
            what: "edge-pair intersection tests",
            limit: MAX_PAIR_TESTS,
        });
    }

    // Where each edge has to be cut.
    let mut cuts_a: Vec<Vec<f64>> = vec![Vec::new(); ea.len()];
    let mut cuts_b: Vec<Vec<f64>> = vec![Vec::new(); eb.len()];
    for (i, x) in ea.iter().enumerate() {
        for (j, y) in eb.iter().enumerate() {
            if !boxes_overlap(*x, *y, eps) {
                continue;
            }
            if let Some((t, u)) = segment_intersection(x.0, x.1, y.0, y.1) {
                cuts_a[i].push(t);
                cuts_b[j].push(u);
            }
        }
    }

    // `true` marks an edge that came from `a`.
    let mut edges: Vec<(Point, Point, bool)> = Vec::new();
    split_edges(&mut ea, &cuts_a, true, &mut snapper, &mut edges);
    split_edges(&mut eb, &cuts_b, false, &mut snapper, &mut edges);
    if edges.len() > MAX_EDGES {
        return Err(VectorError::TooComplex {
            what: "boolean edges",
            limit: MAX_EDGES,
        });
    }

    // Edges lying exactly on top of each other have no inside/outside answer,
    // so they are resolved by rule instead of by classification.
    let mut resolved: Vec<Option<bool>> = vec![None; edges.len()];
    let mut by_pair: HashMap<(u64, u64, u64, u64), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        by_pair.entry(undirected_key(e.0, e.1)).or_default().push(i);
    }
    let mut coincident = vec![false; edges.len()];
    for ids in by_pair.values() {
        let (Some(&ia), Some(&ib)) = (
            ids.iter().find(|&&i| edges[i].2),
            ids.iter().find(|&&i| !edges[i].2),
        ) else {
            continue;
        };
        coincident[ia] = true;
        coincident[ib] = true;
        let same_direction = key(edges[ia].0) == key(edges[ib].0);
        let (keep_a, keep_b) = coincident_rule(op, same_direction);
        resolved[ia] = keep_a;
        resolved[ib] = keep_b;
    }

    let mut kept: Vec<(Point, Point)> = Vec::new();
    for (i, &(p0, p1, from_a)) in edges.iter().enumerate() {
        let action = if coincident[i] {
            resolved[i]
        } else {
            let mid = p0.lerp(p1, 0.5);
            let inside = if from_a {
                point_in_rings(&rb, mid, rule)
            } else {
                point_in_rings(&ra, mid, rule)
            };
            classify(op, from_a, inside)
        };
        match action {
            Some(false) => kept.push((p0, p1)),
            Some(true) => kept.push((p1, p0)),
            None => {}
        }
    }

    Ok(chain(kept))
}

/// `a` or `b`.
pub fn union(a: &Path, b: &Path) -> Result<Path, VectorError> {
    boolean(
        a,
        b,
        BoolOp::Union,
        FillRule::NonZero,
        crate::DEFAULT_TOLERANCE,
    )
}

/// `a` and `b`.
pub fn intersection(a: &Path, b: &Path) -> Result<Path, VectorError> {
    boolean(
        a,
        b,
        BoolOp::Intersection,
        FillRule::NonZero,
        crate::DEFAULT_TOLERANCE,
    )
}

/// `a` minus `b`.
pub fn difference(a: &Path, b: &Path) -> Result<Path, VectorError> {
    boolean(
        a,
        b,
        BoolOp::Difference,
        FillRule::NonZero,
        crate::DEFAULT_TOLERANCE,
    )
}

/// Exactly one of `a` and `b`.
pub fn xor(a: &Path, b: &Path) -> Result<Path, VectorError> {
    boolean(
        a,
        b,
        BoolOp::Xor,
        FillRule::NonZero,
        crate::DEFAULT_TOLERANCE,
    )
}

/// `Some(false)` keep as drawn, `Some(true)` keep reversed, `None` discard.
fn classify(op: BoolOp, from_a: bool, inside_other: bool) -> Option<bool> {
    match (op, from_a, inside_other) {
        // Union: the parts of each boundary that are not swallowed by the other.
        (BoolOp::Union, _, false) => Some(false),
        (BoolOp::Union, _, true) => None,
        // Intersection: the parts of each boundary that are inside the other.
        (BoolOp::Intersection, _, true) => Some(false),
        (BoolOp::Intersection, _, false) => None,
        // Difference: `a` outside `b`, plus `b` inside `a` reversed, which is
        // what turns the bite into a hole rather than a second outline.
        (BoolOp::Difference, true, false) => Some(false),
        (BoolOp::Difference, true, true) => None,
        (BoolOp::Difference, false, true) => Some(true),
        (BoolOp::Difference, false, false) => None,
        // Xor keeps everything, reversing whatever lies inside the other.
        (BoolOp::Xor, _, false) => Some(false),
        (BoolOp::Xor, _, true) => Some(true),
    }
}

/// What to do with a pair of edges that lie on top of one another.
fn coincident_rule(op: BoolOp, same_direction: bool) -> (Option<bool>, Option<bool>) {
    match (op, same_direction) {
        // Same direction: the two shapes agree about which side is inside, so
        // this stretch of boundary survives once (union, intersection) or is
        // cancelled entirely (difference, xor).
        (BoolOp::Union, true) | (BoolOp::Intersection, true) => (Some(false), None),
        (BoolOp::Difference, true) | (BoolOp::Xor, true) => (None, None),
        // Opposite directions: the shapes touch back-to-back along this edge.
        // It is interior to a union and to an xor, and there is no intersection
        // there at all; only a difference keeps it, as `a`'s own boundary.
        (BoolOp::Difference, false) => (Some(false), None),
        (_, false) => (None, None),
    }
}

/// Turn an operand's rings the right way round, without touching the region
/// they describe.
///
/// Every edge of the boundary has to run the same way round for chaining to
/// follow it, so an operand drawn clockwise has to be flipped before it meets
/// one drawn counter-clockwise. The flip is all-or-nothing on purpose: negating
/// *every* ring negates the winding number at every point, which changes
/// neither the nonzero nor the even-odd reading of the operand, whereas
/// flipping rings individually would turn holes into solid ink.
///
/// A total of exactly zero — a figure-eight, or two equal rings drawn against
/// each other — has no handedness to prefer, and is left alone. So is anything
/// non-finite, since `< 0.0` is false for NaN.
fn normalise_orientation(rings: &mut [Polyline]) {
    let total: f64 = rings.iter().map(|r| r.signed_area2()).sum();
    if total < 0.0 {
        for r in rings.iter_mut() {
            r.points.reverse();
        }
    }
}

fn rings_to_path(rings: &[Polyline]) -> Path {
    let mut out = Path::new();
    for r in rings {
        if r.points.len() >= 3 {
            out.extend(&Path::from_polyline(&r.points, true));
        }
    }
    out
}

fn collect_edges(rings: &[Polyline], snapper: &mut Snapper) -> Vec<(Point, Point)> {
    let mut out = Vec::new();
    for r in rings {
        for (a, b) in r.edges() {
            if !a.is_finite() || !b.is_finite() {
                continue;
            }
            let (a, b) = (snapper.snap(a), snapper.snap(b));
            if a != b {
                out.push((a, b));
            }
        }
    }
    out
}

fn split_edges(
    edges: &mut [(Point, Point)],
    cuts: &[Vec<f64>],
    from_a: bool,
    snapper: &mut Snapper,
    out: &mut Vec<(Point, Point, bool)>,
) {
    for (i, &(a, b)) in edges.iter().enumerate() {
        let mut ts: Vec<f64> = cuts[i]
            .iter()
            .copied()
            .filter(|t| t.is_finite() && *t > 0.0 && *t < 1.0)
            .collect();
        ts.sort_by(f64::total_cmp);
        let mut prev = a;
        for t in ts {
            let p = snapper.snap(a.lerp(b, t));
            if p != prev {
                out.push((prev, p, from_a));
                prev = p;
            }
        }
        if b != prev {
            out.push((prev, b, from_a));
        }
    }
}

/// Follow the kept edges back into closed rings.
fn chain(edges: Vec<(Point, Point)>) -> Path {
    let mut starts: HashMap<(u64, u64), Vec<usize>> = HashMap::new();
    for (i, e) in edges.iter().enumerate() {
        starts.entry(key(e.0)).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut out = Path::new();

    for seed in 0..edges.len() {
        if used[seed] {
            continue;
        }
        used[seed] = true;
        let start = edges[seed].0;
        let mut ring = vec![start, edges[seed].1];
        let mut cursor = edges[seed].1;

        for _ in 0..edges.len() {
            if cursor == start {
                break;
            }
            let Some(next) = starts
                .get(&key(cursor))
                .and_then(|ids| ids.iter().copied().find(|&i| !used[i]))
            else {
                // A dead end: the classification could not produce a closed
                // ring here. Closing what was traced degrades gracefully rather
                // than dropping geometry the user drew.
                break;
            };
            used[next] = true;
            cursor = edges[next].1;
            ring.push(cursor);
        }

        if ring.len() > 1 && ring[ring.len() - 1] == ring[0] {
            ring.pop();
        }
        if ring.len() < 3 {
            continue;
        }
        let poly = Polyline {
            points: ring,
            closed: true,
        };
        if poly.signed_area2().abs() <= f64::EPSILON {
            continue;
        }
        out.extend(&Path::from_polyline(&poly.points, true));
    }
    out
}

/// Parameters at which two segments cross, if they do.
///
/// Parallel and collinear pairs return `None` on purpose: they have no single
/// crossing point, and the ones that matter — edges lying on top of each other
/// — are handled by the coincidence table instead.
fn segment_intersection(p1: Point, p2: Point, p3: Point, p4: Point) -> Option<(f64, f64)> {
    let d1 = p2 - p1;
    let d2 = p4 - p3;
    let denom = d1.cross(d2);
    if denom == 0.0 || !denom.is_finite() {
        return None;
    }
    let d3 = p3 - p1;
    let t = d3.cross(d2) / denom;
    let u = d3.cross(d1) / denom;
    if (0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u) {
        Some((t, u))
    } else {
        None
    }
}

fn boxes_overlap(x: (Point, Point), y: (Point, Point), eps: f64) -> bool {
    let (ax0, ax1) = minmax(x.0.x, x.1.x);
    let (ay0, ay1) = minmax(x.0.y, x.1.y);
    let (bx0, bx1) = minmax(y.0.x, y.1.x);
    let (by0, by1) = minmax(y.0.y, y.1.y);
    ax0 - eps <= bx1 && bx0 - eps <= ax1 && ay0 - eps <= by1 && by0 - eps <= ay1
}

fn minmax(a: f64, b: f64) -> (f64, f64) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

fn key(p: Point) -> (u64, u64) {
    (p.x.to_bits(), p.y.to_bits())
}

fn undirected_key(a: Point, b: Point) -> (u64, u64, u64, u64) {
    let (ka, kb) = (key(a), key(b));
    if ka <= kb {
        (ka.0, ka.1, kb.0, kb.1)
    } else {
        (kb.0, kb.1, ka.0, ka.1)
    }
}

/// How close two points have to be before they are treated as the same one.
///
/// Scaled to the geometry: an absolute epsilon that is right for a 10-unit
/// shape merges half a 0.001-unit one, and one that is right for the small
/// shape never fires on a 100,000-unit one.
fn snap_epsilon(ra: &[Polyline], rb: &[Polyline]) -> f64 {
    let mut extent: f64 = 1.0;
    for r in ra.iter().chain(rb.iter()) {
        for p in &r.points {
            if p.is_finite() {
                extent = extent.max(p.x.abs()).max(p.y.abs());
            }
        }
    }
    extent * 1e-9
}

/// Merges points that are within `eps` of one another into one representative,
/// so the chaining stage can compare them exactly.
///
/// Without this, an intersection point computed from `a`'s edge and the same
/// point computed from `b`'s edge differ in the last bit or two and the ring
/// never closes.
struct Snapper {
    eps: f64,
    cells: HashMap<(i64, i64), Vec<Point>>,
}

impl Snapper {
    fn new(eps: f64) -> Self {
        Self {
            eps: if eps.is_finite() && eps > 0.0 {
                eps
            } else {
                1e-9
            },
            cells: HashMap::new(),
        }
    }

    fn snap(&mut self, p: Point) -> Point {
        if !p.is_finite() {
            return p;
        }
        let cx = (p.x / self.eps).floor() as i64;
        let cy = (p.y / self.eps).floor() as i64;
        // A cell is exactly `eps` across, so anything within `eps` is at most
        // one cell away on each axis.
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(v) = self.cells.get(&(cx + dx, cy + dy)) {
                    for q in v {
                        if q.distance(p) <= self.eps {
                            return *q;
                        }
                    }
                }
            }
        }
        self.cells.entry((cx, cy)).or_default().push(p);
        p
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{fill, FillOptions};
    use crate::hit::contains;
    use crate::point::{point, Bounds};
    use crate::shapes;

    fn area(p: &Path) -> f64 {
        fill(p, &FillOptions::default()).unwrap().area()
    }

    fn sq(x: f64, y: f64, s: f64) -> Path {
        shapes::rect(Bounds::from_xywh(x, y, s, s))
    }

    #[test]
    fn overlapping_squares_give_the_four_expected_regions() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(5.0, 5.0, 10.0);
        // The overlap is 5x5 = 25.
        assert_eq!(area(&union(&a, &b).unwrap()), 175.0);
        assert_eq!(area(&intersection(&a, &b).unwrap()), 25.0);
        assert_eq!(area(&difference(&a, &b).unwrap()), 75.0);
        assert_eq!(area(&xor(&a, &b).unwrap()), 150.0);
        // The union is one ring, not two overlapping ones.
        assert_eq!(union(&a, &b).unwrap().subpaths().len(), 1);
        assert_eq!(intersection(&a, &b).unwrap().subpaths().len(), 1);
        // Difference really removed the corner.
        let d = difference(&a, &b).unwrap();
        assert!(contains(&d, point(2.0, 2.0), FillRule::NonZero));
        assert!(!contains(&d, point(7.0, 7.0), FillRule::NonZero));
        // Xor is the two lunes, and excludes the shared middle.
        let x = xor(&a, &b).unwrap();
        assert!(!contains(&x, point(7.0, 7.0), FillRule::NonZero));
        assert!(contains(&x, point(2.0, 2.0), FillRule::NonZero));
        assert!(contains(&x, point(13.0, 13.0), FillRule::NonZero));
    }

    #[test]
    fn disjoint_shapes_keep_both_outlines_or_none() {
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(50.0, 50.0, 10.0);
        let u = union(&a, &b).unwrap();
        assert_eq!(u.subpaths().len(), 2);
        assert_eq!(area(&u), 200.0);
        assert!(intersection(&a, &b).unwrap().is_empty());
        assert_eq!(area(&difference(&a, &b).unwrap()), 100.0);
        assert_eq!(area(&xor(&a, &b).unwrap()), 200.0);
    }

    #[test]
    fn a_contained_shape_becomes_a_hole_only_where_it_should() {
        let outer = sq(0.0, 0.0, 20.0);
        let inner = sq(5.0, 5.0, 10.0);

        assert_eq!(area(&union(&outer, &inner).unwrap()), 400.0);
        assert_eq!(union(&outer, &inner).unwrap().subpaths().len(), 1);

        assert_eq!(area(&intersection(&outer, &inner).unwrap()), 100.0);

        let d = difference(&outer, &inner).unwrap();
        assert_eq!(d.subpaths().len(), 2, "an outline and a hole");
        assert_eq!(area(&d), 300.0);
        assert!(!contains(&d, point(10.0, 10.0), FillRule::NonZero));
        assert!(contains(&d, point(2.0, 10.0), FillRule::NonZero));

        // Xor of nested shapes is the same ring-with-a-hole.
        assert_eq!(area(&xor(&outer, &inner).unwrap()), 300.0);
        // And the reverse difference is empty: the inner is wholly consumed.
        assert!(difference(&inner, &outer).unwrap().is_empty());
    }

    #[test]
    fn identical_shapes_collapse_correctly() {
        // The degenerate case every edge-classification scheme gets wrong: every
        // edge lies exactly on top of another, so no midpoint has an
        // inside/outside answer at all.
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(0.0, 0.0, 10.0);
        assert_eq!(area(&union(&a, &b).unwrap()), 100.0);
        assert_eq!(union(&a, &b).unwrap().subpaths().len(), 1);
        assert_eq!(area(&intersection(&a, &b).unwrap()), 100.0);
        assert!(difference(&a, &b).unwrap().is_empty());
        assert!(xor(&a, &b).unwrap().is_empty());
    }

    #[test]
    fn shapes_that_share_an_edge_merge_without_a_seam() {
        // Back-to-back rectangles: the shared edge is interior to the union and
        // has to disappear, or the result is two rings with a crack between.
        let a = shapes::rect(Bounds::from_xywh(0.0, 0.0, 10.0, 10.0));
        let b = shapes::rect(Bounds::from_xywh(10.0, 0.0, 10.0, 10.0));
        let u = union(&a, &b).unwrap();
        assert_eq!(u.subpaths().len(), 1, "the shared edge left a seam");
        assert_eq!(area(&u), 200.0);
        assert_eq!(u.bounds(), Bounds::from_xywh(0.0, 0.0, 20.0, 10.0));
        assert!(intersection(&a, &b).unwrap().is_empty());
        assert_eq!(area(&difference(&a, &b).unwrap()), 100.0);
        assert_eq!(area(&xor(&a, &b).unwrap()), 200.0);
    }

    #[test]
    fn curved_shapes_combine_to_their_analytic_areas() {
        let r = 40.0;
        let a = shapes::circle(point(0.0, 0.0), r);
        let b = shapes::circle(point(r, 0.0), r);
        let u = boolean(&a, &b, BoolOp::Union, FillRule::NonZero, 0.01).unwrap();
        let i = boolean(&a, &b, BoolOp::Intersection, FillRule::NonZero, 0.01).unwrap();

        // Two unit circles whose centres are one radius apart overlap in
        // 2 r^2 (pi/3 - sqrt(3)/4).
        let lens = 2.0 * r * r * (std::f64::consts::PI / 3.0 - 3f64.sqrt() / 4.0);
        let circle_area = std::f64::consts::PI * r * r;
        assert!(
            (area(&i) - lens).abs() / lens < 0.01,
            "intersection {} vs {lens}",
            area(&i)
        );
        assert!(
            (area(&u) - (2.0 * circle_area - lens)).abs() / circle_area < 0.01,
            "union {}",
            area(&u)
        );
        assert_eq!(u.subpaths().len(), 1);
        assert_eq!(i.subpaths().len(), 1);
        // The result is polygonal, as documented.
        assert!(u
            .elements()
            .iter()
            .all(|e| !matches!(e, crate::PathEl::CurveTo(..))));
    }

    #[test]
    fn the_ops_agree_with_set_algebra_on_the_rasteriser() {
        // The strongest check available: build the four results, rasterise all
        // of them plus the operands, and confirm every pixel obeys the
        // set-theoretic identity it should.
        use glam::IVec2;
        let a = shapes::circle(point(30.0, 30.0), 22.0);
        let b = shapes::star(point(45.0, 40.0), 25.0, 11.0, 5, 0.6);
        let opts = FillOptions::default().clipped_to(crate::PixelRect::from_xywh(0, 0, 90, 90));

        let ma = fill(&a, &opts).unwrap();
        let mb = fill(&b, &opts).unwrap();
        let mu = fill(&union(&a, &b).unwrap(), &opts).unwrap();
        let mi = fill(&intersection(&a, &b).unwrap(), &opts).unwrap();
        let md = fill(&difference(&a, &b).unwrap(), &opts).unwrap();
        let mx = fill(&xor(&a, &b).unwrap(), &opts).unwrap();

        let mut checked = 0;
        for y in 0..90i32 {
            for x in 0..90i32 {
                let p = IVec2::new(x, y);
                let (ia, ib) = (ma.coverage_at(p), mb.coverage_at(p));
                // Only pixels both operands answer unambiguously.
                if (ia != 0 && ia != 255) || (ib != 0 && ib != 255) {
                    continue;
                }
                let (ia, ib) = (ia == 255, ib == 255);
                let want = |v: bool| if v { 255 } else { 0 };
                assert_eq!(mu.coverage_at(p), want(ia || ib), "union at {p:?}");
                assert_eq!(mi.coverage_at(p), want(ia && ib), "intersect at {p:?}");
                assert_eq!(md.coverage_at(p), want(ia && !ib), "difference at {p:?}");
                assert_eq!(mx.coverage_at(p), want(ia != ib), "xor at {p:?}");
                checked += 1;
            }
        }
        assert!(checked > 6000, "only {checked} unambiguous pixels");
    }

    #[test]
    fn an_empty_operand_has_the_algebraic_answer() {
        let a = sq(0.0, 0.0, 10.0);
        let e = Path::new();
        assert_eq!(area(&union(&a, &e).unwrap()), 100.0);
        assert_eq!(area(&union(&e, &a).unwrap()), 100.0);
        assert!(intersection(&a, &e).unwrap().is_empty());
        assert!(intersection(&e, &a).unwrap().is_empty());
        assert_eq!(area(&difference(&a, &e).unwrap()), 100.0);
        assert!(difference(&e, &a).unwrap().is_empty());
        assert_eq!(area(&xor(&a, &e).unwrap()), 100.0);
        assert_eq!(area(&xor(&e, &a).unwrap()), 100.0);
        for op in [
            BoolOp::Union,
            BoolOp::Intersection,
            BoolOp::Difference,
            BoolOp::Xor,
        ] {
            assert!(boolean(&e, &e, op, FillRule::NonZero, 0.1)
                .unwrap()
                .is_empty());
        }
    }

    #[test]
    fn degenerate_operands_do_not_panic() {
        let a = sq(0.0, 0.0, 10.0);
        let mut dot = Path::new();
        dot.move_to(point(5.0, 5.0));
        let hairline = shapes::line(point(-5.0, 5.0), point(15.0, 5.0));
        let mut nan = Path::new();
        nan.move_to(point(f64::NAN, 0.0))
            .line_to(point(3.0, 3.0))
            .line_to(point(6.0, 0.0))
            .close();

        for other in [&dot, &hairline, &nan] {
            for op in [
                BoolOp::Union,
                BoolOp::Intersection,
                BoolOp::Difference,
                BoolOp::Xor,
            ] {
                let r = boolean(&a, other, op, FillRule::NonZero, 0.1).unwrap();
                assert!(r.is_finite(), "{op:?} produced NaN geometry");
                let r = boolean(other, &a, op, FillRule::NonZero, 0.1).unwrap();
                assert!(r.is_finite(), "reversed {op:?} produced NaN geometry");
            }
        }
        // A bad tolerance falls back rather than dividing by zero.
        for tol in [0.0, -1.0, f64::NAN] {
            assert_eq!(
                area(&boolean(&a, &a, BoolOp::Union, FillRule::NonZero, tol).unwrap()),
                100.0,
                "tolerance {tol}"
            );
        }
    }

    #[test]
    fn the_even_odd_reading_of_an_input_is_respected() {
        // A pentagram's centre is inside under nonzero and outside under
        // even-odd, so intersecting a small square with it must differ.
        let g = shapes::pentagram(point(0.0, 0.0), 50.0, 0.0);
        let s = shapes::rect(Bounds::from_xywh(-6.0, -6.0, 12.0, 12.0));
        let nz = boolean(&s, &g, BoolOp::Intersection, FillRule::NonZero, 0.01).unwrap();
        let eo = boolean(&s, &g, BoolOp::Intersection, FillRule::EvenOdd, 0.01).unwrap();
        assert!(area(&nz) > 100.0, "{}", area(&nz));
        assert!(area(&eo) < 1.0, "{}", area(&eo));
    }

    #[test]
    fn an_operands_winding_direction_does_not_change_the_answer() {
        // Classification never cared about orientation, but chaining follows
        // `edge.end == next.start`, so edges inherited from an oppositely-wound
        // operand used to run backwards, dead-end, and get closed across into a
        // plausible-looking ring around the wrong region. This is reachable the
        // moment a `shapes::` primitive (always counter-clockwise) meets a path
        // pasted in from a file - Illustrator emits clockwise routinely - or the
        // output of the public `Path::reversed`.
        let ops = [
            BoolOp::Union,
            BoolOp::Intersection,
            BoolOp::Difference,
            BoolOp::Xor,
        ];

        // Two 10x10 squares overlapping in 25.
        let a = sq(0.0, 0.0, 10.0);
        let b = sq(5.0, 5.0, 10.0);
        let (ar, br) = (a.reversed(), b.reversed());
        assert!(a.signed_area2(0.01) > 0.0 && ar.signed_area2(0.01) < 0.0);
        for (op, want) in ops.into_iter().zip([175.0, 25.0, 75.0, 150.0]) {
            for (x, y) in [(&a, &b), (&a, &br), (&ar, &b), (&ar, &br)] {
                let got = area(&boolean(x, y, op, FillRule::NonZero, 0.01).unwrap());
                assert_eq!(got, want, "{op:?}");
            }
        }
        // The union of the mixed pair is one ring, not three fragments.
        assert_eq!(union(&a, &br).unwrap().subpaths().len(), 1);

        // Two circles of radius 40 whose centres are one radius apart, against
        // the closed forms rather than against this crate's own output.
        let r = 40.0;
        let c1 = shapes::circle(point(0.0, 0.0), r);
        let c2 = shapes::circle(point(r, 0.0), r);
        let (c1r, c2r) = (c1.reversed(), c2.reversed());
        let lens = 2.0 * r * r * (std::f64::consts::PI / 3.0 - 3f64.sqrt() / 4.0);
        let disc = std::f64::consts::PI * r * r;
        let want = [
            2.0 * disc - lens,
            lens,
            disc - lens,
            2.0 * disc - 2.0 * lens,
        ];
        for (op, want) in ops.into_iter().zip(want) {
            for (x, y) in [(&c1, &c2), (&c1, &c2r), (&c1r, &c2), (&c1r, &c2r)] {
                let got = area(&boolean(x, y, op, FillRule::NonZero, 0.01).unwrap());
                assert!(
                    (got - want).abs() / want < 0.01,
                    "{op:?} gave {got}, not {want}"
                );
            }
        }
    }

    #[test]
    fn the_result_is_positively_oriented_however_the_inputs_were_wound() {
        // The invariant `shapes` and `stroke` both lean on, and the one the
        // module doc promises: outer rings positive, holes negative, whatever
        // the operands were.
        let cw_a = sq(0.0, 0.0, 10.0).reversed();
        let cw_b = sq(5.0, 5.0, 10.0).reversed();
        let u = union(&cw_a, &cw_b).unwrap();
        assert_eq!(u.subpaths().len(), 1);
        assert_eq!(u.signed_area2(0.01), 350.0, "twice the 175-unit union");

        // Two clockwise squares that do not touch: both rings come out positive.
        let apart = union(&cw_a, &sq(50.0, 50.0, 10.0).reversed()).unwrap();
        let rings = apart.flatten_closed(0.01);
        assert_eq!(rings.len(), 2);
        assert!(rings.iter().all(|r| r.signed_area2() > 0.0));

        // An outline with a hole, built from a clockwise outer shape: the
        // outline positive, the hole negative.
        let d = difference(&sq(0.0, 0.0, 20.0).reversed(), &sq(5.0, 5.0, 10.0)).unwrap();
        let mut areas: Vec<f64> = d
            .flatten_closed(0.01)
            .iter()
            .map(|r| r.signed_area2())
            .collect();
        areas.sort_by(f64::total_cmp);
        assert_eq!(areas, vec![-200.0, 800.0], "an 800 ring with a 200 hole");
        assert_eq!(area(&d), 300.0);

        // Every op, not just those two: twice the ink, positive, always.
        for (op, want) in [
            (BoolOp::Union, 350.0),
            (BoolOp::Intersection, 50.0),
            (BoolOp::Difference, 150.0),
            (BoolOp::Xor, 300.0),
        ] {
            let r = boolean(&cw_a, &cw_b, op, FillRule::NonZero, 0.01).unwrap();
            assert_eq!(r.signed_area2(0.01), want, "{op:?}");
        }
    }

    #[test]
    fn a_pathological_pair_is_refused_rather_than_exhausting_memory() {
        // Two combs with thousands of teeth each cross a huge number of times.
        let mut a = Path::new();
        let mut b = Path::new();
        let n = 8000;
        a.move_to(point(0.0, 0.0));
        b.move_to(point(0.0, 0.0));
        for i in 0..n {
            let t = i as f64;
            a.line_to(point(t, if i % 2 == 0 { 0.0 } else { 100.0 }));
            b.line_to(point(t, if i % 2 == 0 { 100.0 } else { 0.0 }));
        }
        a.close();
        b.close();
        assert!(matches!(
            boolean(&a, &b, BoolOp::Union, FillRule::NonZero, 0.1),
            Err(VectorError::TooComplex { .. })
        ));
    }

    /// The other shape of the same problem, and the one the pair-test cap
    /// cannot see: *one* enormous operand against a tiny one. The product of
    /// the edge counts stays well under [`MAX_PAIR_TESTS`], so without a cap on
    /// each operand on its own this would allocate a cut list per edge — and a
    /// split-edge list to match — and only then discover it was too big.
    #[test]
    fn one_oversized_operand_against_a_triangle_is_refused_before_it_allocates() {
        let n = MAX_EDGES + 6;
        let mut big = Path::new();
        big.move_to(point(0.0, 0.0));
        for i in 1..n {
            big.line_to(point(i as f64, if i % 2 == 0 { 0.0 } else { 1.0 }));
        }
        big.close();

        let mut small = Path::new();
        small
            .move_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .line_to(point(5.0, 8.0))
            .close();

        // The pair-test cap really does not fire here: three edges times even
        // this many is two orders of magnitude below it.
        assert!(n.saturating_mul(3) < MAX_PAIR_TESTS);

        for op in [
            BoolOp::Union,
            BoolOp::Intersection,
            BoolOp::Difference,
            BoolOp::Xor,
        ] {
            // Either way round: the cap is on each operand, not on the first.
            for (x, y) in [(&big, &small), (&small, &big)] {
                assert_eq!(
                    boolean(x, y, op, FillRule::NonZero, 0.1),
                    Err(VectorError::TooComplex {
                        what: "edges in one boolean operand",
                        limit: MAX_EDGES,
                    }),
                    "{op:?}"
                );
            }
        }

        // And it is a cap, not a ban: the same comb one edge under the limit
        // goes through and produces a real result.
        let mut ok = Path::new();
        ok.move_to(point(0.0, 0.0));
        for i in 1..1000 {
            ok.line_to(point(i as f64, if i % 2 == 0 { 0.0 } else { 1.0 }));
        }
        ok.close();
        assert!(boolean(&ok, &small, BoolOp::Union, FillRule::NonZero, 0.1).is_ok());
    }
}
