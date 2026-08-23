//! Turning a stroke into a fillable outline.
//!
//! # Why an outline instead of a stroke rasteriser
//! A stroke is not a separate kind of ink. Converting it to a closed path and
//! handing that to the same [`crate::fill()`] rasteriser means a stroke gets
//! anti-aliasing, both fill rules, hit testing, boolean ops and mask conversion
//! for free, and — the part that matters — a stroke and a fill of the same
//! shape are guaranteed to agree along their shared boundary, because they are
//! computed by the same code. A dedicated stroke rasteriser is where the
//! hairline gaps between a shape and its own outline come from.
//!
//! The outline is always emitted in **positive** orientation, with holes wound
//! the other way, so it fills correctly under [`crate::FillRule::NonZero`].
//!
//! # What it is built from
//! Offsetting is done on the *flattened* path, so the result is a polygonal
//! outline rather than a curve-fitted one. That is a deliberate trade: fitting
//! offset curves is where stroking libraries acquire their worst bugs (an
//! offset of a cubic is not a cubic, and the usual fits fail exactly at cusps
//! and inflections), while a polygon flattened to the same tolerance the fill
//! already uses is indistinguishable once rasterised. The tolerance is the
//! caller's, so a stroke that will be scaled up can be built tighter.

use serde::{Deserialize, Serialize};

use crate::error::VectorError;
use crate::path::{dedup_points, Path};
use crate::point::{point, Point};

/// How the two ends of an open subpath are finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Cap {
    /// Stop flat at the endpoint.
    #[default]
    Butt,
    /// A semicircle of half the stroke width.
    Round,
    /// A flat end projected half the stroke width past the endpoint.
    Square,
}

/// How two segments are connected at a corner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Join {
    /// Extend both outer edges until they meet, falling back to
    /// [`Join::Bevel`] when that point is further than the miter limit.
    #[default]
    Miter,
    /// An arc of half the stroke width around the corner.
    Round,
    /// A straight cut across the corner.
    Bevel,
}

/// A dash pattern: alternating on and off lengths, with a phase.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Dash {
    /// Alternating on/off lengths, starting with "on".
    ///
    /// An odd-length pattern is repeated to make it even, so `[5]` means five
    /// on, five off — the SVG rule.
    pub pattern: Vec<f64>,
    /// How far into the pattern the first subpath starts. Negative and
    /// oversized offsets wrap.
    pub offset: f64,
}

impl Dash {
    /// A pattern with no phase offset.
    pub fn new(pattern: impl Into<Vec<f64>>) -> Self {
        Self {
            pattern: pattern.into(),
            offset: 0.0,
        }
    }

    /// The pattern with the odd-length rule applied, or `None` when it is not a
    /// usable pattern (empty, negative, non-finite, or all zeros).
    fn resolved(&self) -> Option<Vec<f64>> {
        if self.pattern.is_empty()
            || !self.offset.is_finite()
            || self.pattern.iter().any(|v| !v.is_finite() || *v < 0.0)
            || self.pattern.iter().sum::<f64>() <= 0.0
        {
            return None;
        }
        let mut p = self.pattern.clone();
        if p.len() % 2 == 1 {
            p.extend_from_within(..);
        }
        Some(p)
    }
}

/// Everything about how a path is stroked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
    /// Total width of the stroke; the outline sits half of it either side.
    pub width: f64,
    /// End treatment for open subpaths.
    pub cap: Cap,
    /// Corner treatment.
    pub join: Join,
    /// Longest miter, as a multiple of half the stroke width, before
    /// [`Join::Miter`] degrades to [`Join::Bevel`]. SVG's default is 4.
    pub miter_limit: f64,
    /// Optional dash pattern applied before offsetting.
    pub dash: Option<Dash>,
    /// Flattening tolerance for curves and for the arcs of round joins and caps.
    pub tolerance: f64,
}

impl Default for StrokeStyle {
    fn default() -> Self {
        Self {
            width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 4.0,
            dash: None,
            tolerance: crate::DEFAULT_TOLERANCE,
        }
    }
}

impl StrokeStyle {
    /// A style of the given width, everything else default.
    pub fn new(width: f64) -> Self {
        Self {
            width,
            ..Self::default()
        }
    }
}

/// The most points one arc may contribute, so a pathological radius/tolerance
/// pair cannot generate an unbounded outline.
const MAX_ARC_STEPS: usize = 1024;

/// The most subpaths a dash operation may produce.
const MAX_DASH_RUNS: usize = 200_000;

/// Convert a stroke into a closed outline that fills to the same ink.
///
/// A width that is zero, negative or non-finite paints nothing and returns an
/// empty path rather than an error: a zero-width stroke is a legitimate way for
/// a UI to say "no stroke".
pub fn stroke(path: &Path, style: &StrokeStyle) -> Result<Path, VectorError> {
    if !style.width.is_finite() || style.width <= 0.0 {
        return Ok(Path::new());
    }
    let r = style.width * 0.5;
    let tol = if style.tolerance.is_finite() && style.tolerance > 0.0 {
        style.tolerance
    } else {
        crate::DEFAULT_TOLERANCE
    };

    let source = match &style.dash {
        Some(d) => dash(path, d, tol)?,
        None => path.clone(),
    };

    let mut out = Path::new();
    for poly in source.flatten(tol) {
        let mut pts = poly.points;
        pts.retain(|p| p.is_finite());
        dedup_points(&mut pts);
        if poly.closed && pts.len() > 1 && pts[0].distance_squared(pts[pts.len() - 1]) == 0.0 {
            pts.pop();
        }
        if pts.is_empty() {
            continue;
        }
        if pts.len() == 1 {
            // A subpath with no length still paints, if the cap has area: a
            // dot from a round cap, a square from a square cap, nothing from a
            // butt cap. That is the SVG rule, and it is what makes a
            // single-click dab of the pen tool visible.
            if let Some(ring) = dot_outline(pts[0], r, style.cap, tol) {
                push_ring(&mut out, ring);
            }
            continue;
        }

        if poly.closed && pts.len() >= 3 {
            // Two rings: the outside wound positively, the inside wound the
            // other way so nonzero filling leaves the middle hollow.
            let mut outer = Vec::new();
            offset_side(&pts, true, r, style, tol, &mut outer);
            push_ring(&mut out, outer);

            let rev: Vec<Point> = pts.iter().rev().copied().collect();
            let mut inner = Vec::new();
            offset_side(&rev, true, r, style, tol, &mut inner);
            push_ring(&mut out, inner);
        } else {
            let mut ring = Vec::new();
            offset_side(&pts, false, r, style, tol, &mut ring);

            let rev: Vec<Point> = pts.iter().rev().copied().collect();
            let mut back = Vec::new();
            offset_side(&rev, false, r, style, tol, &mut back);

            if pts.len() < 2 || ring.is_empty() || back.is_empty() {
                // Not one segment of this subpath had a direction to offset
                // along. Zero-length ones are already gone, so the only way
                // that happens is overflow: `M-1e308 0 L1e308 0` is legal path
                // data whose `b - a` is infinity, `normalize()` answers zero,
                // and `offset_side` emits nothing at all. There is no outline
                // to build, so the subpath falls back to painting what its cap
                // paints — which at that magnitude is usually nothing, because
                // one ulp is far wider than the stroke. Either way it is an
                // answer; indexing into the empty rings was an abort.
                if let Some(dot) = dot_outline(pts[0], r, style.cap, tol) {
                    push_ring(&mut out, dot);
                }
                continue;
            }

            let n = pts.len();
            let end = pts[n - 1];
            let d_end = (end - pts[n - 2]).normalize();
            let (a, b) = (ring[ring.len() - 1], back[0]);
            add_cap(&mut ring, end, a, b, d_end, r, style.cap, tol);
            ring.extend_from_slice(&back);

            let start = pts[0];
            let d_start = (start - pts[1]).normalize();
            let (a, b) = (ring[ring.len() - 1], ring[0]);
            add_cap(&mut ring, start, a, b, d_start, r, style.cap, tol);

            push_ring(&mut out, ring);
        }
    }
    Ok(out)
}

/// Split a path into its dash pattern's "on" runs, one subpath each.
///
/// Exposed on its own because the pattern's geometry is worth being able to
/// inspect and test directly, and because a caller that wants dashed *guides*
/// rather than dashed ink needs the runs, not an outline.
///
/// A pattern that cannot be used — empty, negative, non-finite, or summing to
/// zero — leaves the path alone, which is the SVG rule for an invalid
/// `stroke-dasharray`.
///
/// A *single* zero entry inside an otherwise usable pattern is legal and means
/// exactly what it says. `[5, 0]` is a solid line drawn as a run of abutting
/// five-unit dashes; `[0, 8]` is a row of zero-length runs eight apart, which is
/// how a dotted line is spelled — [`stroke`] gives each one the area of its cap.
/// Neither is an excuse to drop the rest of the path.
///
/// On a **closed** subpath the dash that wraps across the start vertex is one
/// run, not two. When the pattern is "on" both as the walk leaves the first
/// vertex and as it arrives back at it, the trailing run and the leading run are
/// spliced into a single open subpath that passes *through* that vertex — so
/// [`stroke`] draws a join there, as it would at any other corner, instead of
/// two butt caps meeting in a notch (or, under a round or square cap, two caps
/// of doubled ink). A pattern that never toggles at all over the whole ring
/// stays closed for the same reason: it comes back as the ring it went in as.
pub fn dash(path: &Path, d: &Dash, tolerance: f64) -> Result<Path, VectorError> {
    let Some(pattern) = d.resolved() else {
        return Ok(path.clone());
    };
    let period: f64 = pattern.iter().sum();
    let len = pattern.len();

    let mut out = Path::new();
    let mut runs = 0usize;

    for poly in path.flatten(tolerance) {
        let mut pts = poly.points;
        pts.retain(|p| p.is_finite());
        dedup_points(&mut pts);
        if pts.len() < 2 {
            continue;
        }

        // Where in the pattern this subpath starts. Every subpath restarts the
        // phase, which is what SVG specifies and what makes a dashed rectangle
        // look the same however its corners happen to be ordered.
        let mut phase = d.offset % period;
        if phase < 0.0 {
            phase += period;
        }
        let mut idx = 0usize;
        for _ in 0..(len * 2) {
            // Stop *on* a zero-length entry when the phase lands exactly at its
            // start. `stroke-dasharray: 0 8` with a round cap is the standard
            // spelling of a dotted line, and stepping past the zero-length "on"
            // entry loses every dot it asked for. A phase that has genuinely
            // consumed earlier entries is past that start and steps on.
            if phase < pattern[idx] || (phase == 0.0 && pattern[idx] == 0.0) {
                break;
            }
            phase -= pattern[idx];
            idx = (idx + 1) % len;
        }
        let mut remaining = pattern[idx] - phase;
        let mut on = idx % 2 == 0;

        // Whether the pattern was already "on" *at* `pts[0]`. Only then is the
        // run that ends at the closing vertex continuous with the run that
        // started there, and only then may the two be spliced.
        let started_on = on;

        let mut cur: Vec<Point> = if on { vec![pts[0]] } else { Vec::new() };
        // Runs are buffered per subpath rather than emitted as they close,
        // because whether the last one is its own dash or the tail of the first
        // is not known until the walk reaches the closing vertex.
        let mut poly_runs: Vec<Vec<Point>> = Vec::new();
        let edges = super::path::Polyline {
            points: pts,
            closed: poly.closed,
        }
        .edges();

        for (a, b) in edges {
            let seg_len = a.distance(b);
            if !seg_len.is_finite() || seg_len <= 0.0 {
                // Nothing to walk along: a repeated vertex, or an edge so long
                // its own difference overflowed. It advances the pattern by
                // nothing rather than dividing by zero or looping forever.
                if on {
                    cur.push(b);
                }
                continue;
            }
            let mut travelled = 0.0f64;
            // This terminates on its own, with no heuristic bail-out.
            // `Dash::resolved` guarantees the pattern sums to more than zero, so
            // `travelled` gains a whole period every `len` toggles however many
            // zero-length entries the pattern contains, and every second toggle
            // counts a run against `MAX_DASH_RUNS`. A guard that watched for
            // "stuck" instead would have to guess, and guessing wrong deletes
            // the rest of the path silently.
            while seg_len - travelled > remaining {
                travelled += remaining;
                let p = a.lerp(b, travelled / seg_len);
                if on {
                    cur.push(p);
                    runs += 1;
                    if runs > MAX_DASH_RUNS {
                        return Err(VectorError::TooComplex {
                            what: "dash runs",
                            limit: MAX_DASH_RUNS,
                        });
                    }
                    poly_runs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                    cur.push(p);
                }
                on = !on;
                idx = (idx + 1) % len;
                remaining = pattern[idx];
            }
            remaining -= seg_len - travelled;
            if on {
                cur.push(b);
            }
        }
        if on {
            poly_runs.push(std::mem::take(&mut cur));
        }

        // A closed subpath whose pattern was on at the closing vertex from both
        // sides has one dash, not two: the walk arrived back where it started
        // still drawing. Emitting the two halves separately would cap each of
        // them at the closing vertex, so a mitred rectangle would show a notch
        // at its first corner and a round or square cap would double its ink
        // there. `poly_runs.len() == 1` is the case where nothing ever toggled,
        // so the single run *is* the whole ring and simply closes.
        let wraps = poly.closed && started_on && on;
        let mut closed_run = false;
        if wraps {
            if poly_runs.len() == 1 {
                closed_run = true;
            } else if let Some(tail) = poly_runs.pop() {
                let head = std::mem::replace(&mut poly_runs[0], tail);
                poly_runs[0].extend_from_slice(&head);
            }
        }
        for run in &poly_runs {
            emit_run(&mut out, run, closed_run);
        }
    }
    Ok(out)
}

/// Emit one "on" run as a subpath — open, unless the run is a whole closed ring
/// the pattern never interrupted, in which case it stays closed so [`stroke`]
/// joins it rather than capping it.
///
/// A run of a single point is kept, not dropped: a zero-length entry in the
/// pattern is a *dot*, and [`stroke`] turns a one-point subpath into whatever
/// the cap paints there. Dropping it is how `stroke-dasharray: 0 8` ends up
/// drawing nothing at all.
fn emit_run(out: &mut Path, pts: &[Point], closed: bool) {
    let mut run = pts.to_vec();
    dedup_points(&mut run);
    // A run that walked the whole way round arrives with `pts[0]` repeated at
    // the end, and the closing edge of a closed subpath is implied, so keeping
    // that duplicate would only add a zero-length segment for the stroker to
    // trip over. `run.len() > 2` is the ring's own minimum counted *with* the
    // duplicate: `[p0, p1, p0]` is the smallest ring there is, out along one
    // leg and back along the implied closing edge.
    let repeats_start = run.len() > 1 && run[0].distance_squared(run[run.len() - 1]) == 0.0;
    let ring = closed && run.len() > 2;
    if ring && repeats_start {
        run.pop();
    }
    if run.is_empty() {
        return;
    }
    out.extend(&Path::from_polyline(&run, ring));
}

/// Offset one side of a polyline by `r`, to the **right** of travel.
///
/// Right, not left, so that a positively-oriented ring offsets outwards and the
/// outline keeps the same orientation as the shape it outlines.
fn offset_side(
    pts: &[Point],
    closed: bool,
    r: f64,
    style: &StrokeStyle,
    tol: f64,
    out: &mut Vec<Point>,
) {
    let n = pts.len();
    let nseg = if closed { n } else { n - 1 };
    for i in 0..nseg {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        let d = (b - a).normalize();
        if d == Point::ZERO {
            continue;
        }
        let nm = -d.perp() * r;
        out.push(a + nm);
        out.push(b + nm);

        let has_next = closed || i + 1 < nseg;
        if !has_next {
            continue;
        }
        let c = pts[(i + 2) % n];
        let d2 = (c - b).normalize();
        if d2 == Point::ZERO {
            continue;
        }
        add_join(out, b, b + nm, b + (-d2.perp() * r), d, d2, r, style, tol);
    }
}

/// Connect two offset edges at a corner, appending the join geometry and the
/// second edge's starting point.
#[allow(clippy::too_many_arguments)]
fn add_join(
    out: &mut Vec<Point>,
    v: Point,
    from: Point,
    to: Point,
    d1: Point,
    d2: Point,
    r: f64,
    style: &StrokeStyle,
    tol: f64,
) {
    if from.distance_squared(to) == 0.0 {
        return;
    }
    let cross = d1.cross(d2);
    let dot = d1.dot(d2);
    // Offsetting to the right, the right side is the outside of a left turn.
    // An exact reversal (`cross == 0`, `dot < 0`) is outside on both sides and
    // has to be treated as a corner, or the stroke pinches to nothing there.
    let outer = cross > 0.0 || (cross == 0.0 && dot < 0.0);
    if !outer {
        // The inside of the corner: route through the vertex. The offset edges
        // have crossed over each other there, and letting them run to their
        // intersection would push a spur of stroke *outside* the ink on a tight
        // turn. The vertex is always inside the stroke, so this cannot.
        out.push(v);
        out.push(to);
        return;
    }
    match style.join {
        Join::Bevel => out.push(to),
        Join::Round => {
            arc_interior(out, v, from, to, r, tol);
            out.push(to);
        }
        Join::Miter => {
            let n1 = (from - v) / r;
            let n2 = (to - v) / r;
            let mid = (n1 + n2).normalize();
            let cos_half = mid.dot(n1);
            if mid == Point::ZERO || cos_half <= 0.0 || 1.0 / cos_half > style.miter_limit {
                out.push(to);
            } else {
                out.push(v + mid * (r / cos_half));
                out.push(to);
            }
        }
    }
}

/// Append a cap's *interior* points, turning `from` into `to` around `v` with
/// `dir` pointing out of the end of the path.
///
/// The endpoints themselves are already in the ring, so a butt cap appends
/// nothing at all and the ring simply closes across.
#[allow(clippy::too_many_arguments)]
fn add_cap(
    out: &mut Vec<Point>,
    v: Point,
    from: Point,
    to: Point,
    dir: Point,
    r: f64,
    cap: Cap,
    tol: f64,
) {
    match cap {
        Cap::Butt => {}
        Cap::Square => {
            out.push(from + dir * r);
            out.push(to + dir * r);
        }
        Cap::Round => arc_interior(out, v, from, to, r, tol),
    }
}

/// Append the points strictly between `from` and `to` on the counter-clockwise
/// arc of radius `r` about `center`.
///
/// Counter-clockwise always: with right-hand offsets, every outer join and
/// every cap turns that way, so there is one direction rule instead of a sign
/// to get wrong at each call site.
fn arc_interior(out: &mut Vec<Point>, center: Point, from: Point, to: Point, r: f64, tol: f64) {
    if r <= 0.0 || !r.is_finite() {
        return;
    }
    let a0 = (from - center).angle();
    let a1 = (to - center).angle();
    let mut delta = (a1 - a0) % std::f64::consts::TAU;
    if delta < 0.0 {
        delta += std::f64::consts::TAU;
    }
    if delta <= 0.0 || !delta.is_finite() {
        return;
    }
    // The largest turn whose chord stays within `tol` of the true arc.
    let step = if tol > 0.0 && tol < r {
        2.0 * (1.0 - tol / r).acos()
    } else {
        std::f64::consts::FRAC_PI_4
    };
    let steps = ((delta / step.max(1e-4)).ceil().max(1.0) as usize).min(MAX_ARC_STEPS);
    for k in 1..steps {
        let a = a0 + delta * k as f64 / steps as f64;
        out.push(center + point(a.cos(), a.sin()) * r);
    }
}

/// The outline a zero-length subpath paints, if any.
fn dot_outline(v: Point, r: f64, cap: Cap, tol: f64) -> Option<Vec<Point>> {
    match cap {
        Cap::Butt => None,
        Cap::Square => Some(vec![
            v + point(-r, -r),
            v + point(r, -r),
            v + point(r, r),
            v + point(-r, r),
        ]),
        Cap::Round => {
            let start = v + point(r, 0.0);
            let mut ring = vec![start];
            // A full turn: two half-arcs, because a zero sweep is ambiguous.
            let opposite = v + point(-r, 0.0);
            arc_interior(&mut ring, v, start, opposite, r, tol);
            ring.push(opposite);
            arc_interior(&mut ring, v, opposite, start, r, tol);
            Some(ring)
        }
    }
}

fn push_ring(out: &mut Path, mut pts: Vec<Point>) {
    dedup_points(&mut pts);
    if pts.len() > 1 && pts[0].distance_squared(pts[pts.len() - 1]) == 0.0 {
        pts.pop();
    }
    if pts.len() < 3 {
        return;
    }
    out.extend(&Path::from_polyline(&pts, true));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{fill, FillOptions};
    use crate::hit::contains;
    use crate::point::Bounds;
    use crate::{shapes, FillRule};

    fn area(p: &Path) -> f64 {
        fill(p, &FillOptions::default()).unwrap().area()
    }

    #[test]
    fn the_outline_of_a_straight_segment_is_exactly_the_expected_rectangle() {
        let l = shapes::line(point(0.0, 0.0), point(10.0, 0.0));
        let o = stroke(&l, &StrokeStyle::new(4.0)).unwrap();
        let b = o.bounds();
        assert_eq!(b.min, point(0.0, -2.0));
        assert_eq!(b.max, point(10.0, 2.0));
        assert_eq!(b.height(), 4.0, "the outline must be exactly the width");
        assert_eq!(area(&o), 40.0);
        // and positively oriented, like every other closed path in this crate
        assert!(o.signed_area2(0.01) > 0.0);
        // four corners, no more
        assert_eq!(o.flatten(0.01)[0].points.len(), 4);
    }

    #[test]
    fn a_diagonal_segment_is_the_expected_width_measured_across_it() {
        let l = shapes::line(point(0.0, 0.0), point(30.0, 40.0));
        let o = stroke(&l, &StrokeStyle::new(10.0)).unwrap();
        let pts = &o.flatten(0.01)[0].points;
        assert_eq!(pts.len(), 4);
        // Every corner is exactly half the width from the centre line.
        let dir = point(0.6, 0.8);
        for p in pts {
            let along = p.dot(dir);
            let across = p.cross(dir).abs();
            assert!(
                (across - 5.0).abs() < 1e-9,
                "{p:?} is {across} from the axis"
            );
            assert!(
                (-1e-9..=50.0 + 1e-9).contains(&along),
                "{p:?} is off the end"
            );
        }
        assert!((area(&o) - 500.0).abs() < 0.1);
    }

    #[test]
    fn caps_add_exactly_their_own_area() {
        let l = shapes::line(point(0.0, 0.0), point(20.0, 0.0));
        let w = 6.0;
        let butt = stroke(&l, &StrokeStyle::new(w)).unwrap();
        assert_eq!(area(&butt), 120.0);

        let square = stroke(
            &l,
            &StrokeStyle {
                cap: Cap::Square,
                ..StrokeStyle::new(w)
            },
        )
        .unwrap();
        // Two 3x6 blocks projected past the ends.
        assert_eq!(area(&square), 120.0 + 2.0 * 3.0 * 6.0);
        assert_eq!(
            square.bounds(),
            Bounds::new(point(-3.0, -3.0), point(23.0, 3.0))
        );

        let round = stroke(
            &l,
            &StrokeStyle {
                cap: Cap::Round,
                tolerance: 0.001,
                ..StrokeStyle::new(w)
            },
        )
        .unwrap();
        let expected = 120.0 + std::f64::consts::PI * 9.0;
        assert!((area(&round) - expected).abs() < 0.2, "{}", area(&round));
        assert!((round.bounds().width() - 26.0).abs() < 0.01);
    }

    #[test]
    fn a_stroked_ring_is_a_hollow_annulus() {
        let c = shapes::circle(point(50.0, 50.0), 20.0);
        let o = stroke(
            &c,
            &StrokeStyle {
                width: 4.0,
                join: Join::Round,
                tolerance: 0.001,
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        assert_eq!(o.subpaths().len(), 2, "an outer ring and an inner ring");
        // The middle is a hole under the nonzero rule.
        assert!(!contains(&o, point(50.0, 50.0), FillRule::NonZero));
        assert!(contains(&o, point(70.0, 50.0), FillRule::NonZero));
        assert!(!contains(&o, point(75.0, 50.0), FillRule::NonZero));
        // Area of an annulus of mean radius 20 and width 4.
        let expected = std::f64::consts::PI * (22.0f64.powi(2) - 18.0f64.powi(2));
        assert!(
            (area(&o) - expected).abs() / expected < 0.005,
            "{}",
            area(&o)
        );
    }

    #[test]
    fn a_miter_reaches_the_corner_and_the_limit_cuts_it_off() {
        // A right angle: the miter sticks out by sqrt(2) * half-width.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(50.0, 0.0))
            .line_to(point(50.0, 50.0));
        let w = 10.0;
        let mitered = stroke(&p, &StrokeStyle::new(w)).unwrap();
        // Outer corner of a 90-degree miter is at (55, -5).
        assert!(contains(&mitered, point(54.9, -4.9), FillRule::NonZero));
        assert!(!contains(&mitered, point(55.1, -5.1), FillRule::NonZero));

        // A limit below the corner's own ratio (sqrt 2) degrades it to a bevel.
        let bevelled = stroke(
            &p,
            &StrokeStyle {
                miter_limit: 1.2,
                ..StrokeStyle::new(w)
            },
        )
        .unwrap();
        assert!(!contains(&bevelled, point(54.9, -4.9), FillRule::NonZero));
        assert_eq!(
            bevelled,
            stroke(
                &p,
                &StrokeStyle {
                    join: Join::Bevel,
                    ..StrokeStyle::new(w)
                }
            )
            .unwrap(),
            "a miter past its limit must be exactly a bevel"
        );
        // The area ordering follows: miter > bevel.
        assert!(area(&mitered) > area(&bevelled));
    }

    #[test]
    fn round_joins_do_not_produce_cusps() {
        // A deliberately sharp corner, where a naive offset produces a spike or
        // a reversal. The round join must be a genuine arc: every point at
        // exactly the half-width from the corner, turning the same way the
        // whole time, with no doubled-back edge.
        let v = point(100.0, 0.0);
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(v)
            .line_to(point(0.0, 40.0));
        let r = 10.0;
        let o = stroke(
            &p,
            &StrokeStyle {
                width: r * 2.0,
                join: Join::Round,
                tolerance: 0.01,
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        let pts = &o.flatten(0.001)[0].points;

        // Every point at exactly the half-width from the corner. The inner
        // side of the join also touches that circle where it routes through the
        // vertex, so the arc is the longest contiguous run of them.
        let on_arc: Vec<usize> = (0..pts.len())
            .filter(|&i| (pts[i].distance(v) - r).abs() < 1e-9)
            .collect();
        let mut runs: Vec<Vec<usize>> = Vec::new();
        for i in on_arc {
            match runs.last_mut() {
                Some(run) if run[run.len() - 1] + 1 == i => run.push(i),
                _ => runs.push(vec![i]),
            }
        }
        let run = runs
            .into_iter()
            .max_by_key(|r| r.len())
            .expect("a round join must put points on the arc");
        assert!(
            run.len() > 10,
            "a round join must be an arc, not {} points",
            run.len()
        );

        // No cusp: consecutive edges along the arc all turn the same way, and
        // none doubles back. A cusp shows up as a sign change here.
        let arc: Vec<Point> = run.iter().map(|&i| pts[i]).collect();
        for w in arc.windows(3) {
            let (e1, e2) = (w[1] - w[0], w[2] - w[1]);
            assert!(
                e1.length() > 1e-9 && e2.length() > 1e-9,
                "zero-length arc edge"
            );
            assert!(
                e1.cross(e2) > 0.0,
                "the arc reverses at {:?}: cross {}",
                w[1],
                e1.cross(e2)
            );
            assert!(e1.dot(e2) > 0.0, "the arc doubles back at {:?}", w[1]);
        }

        // The arc really does sweep the exterior angle of the corner.
        let d1 = (v - point(0.0, 0.0)).normalize();
        let d2 = (point(0.0, 40.0) - v).normalize();
        let exterior = d1.cross(d2).atan2(d1.dot(d2)).abs();
        let swept: f64 = arc
            .windows(2)
            .map(|w| {
                let (a, b) = (w[0] - v, w[1] - v);
                a.cross(b).atan2(a.dot(b))
            })
            .sum();
        assert!(
            (swept.abs() - exterior).abs() < 0.05,
            "swept {swept} for an exterior angle of {exterior}"
        );
    }

    #[test]
    fn dash_lengths_sum_to_the_pattern() {
        let l = shapes::line(point(0.0, 0.0), point(100.0, 0.0));
        let d = dash(&l, &Dash::new(vec![10.0, 5.0]), 0.01).unwrap();
        // 0-10, 15-25, 30-40, 45-55, 60-70, 75-85, 90-100.
        let subs = d.subpaths();
        assert_eq!(subs.len(), 7);
        assert!((d.length() - 70.0).abs() < 1e-9, "{}", d.length());
        for (i, s) in subs.iter().enumerate() {
            let want_start = i as f64 * 15.0;
            assert!(
                (s.start.x - want_start).abs() < 1e-9,
                "run {i} starts wrong"
            );
            assert!(
                (s.end().x - (want_start + 10.0)).abs() < 1e-9,
                "run {i} is not ten long"
            );
            assert!(!s.closed);
        }

        // An odd-length pattern is doubled: [5] means five on, five off.
        let odd = dash(&l, &Dash::new(vec![5.0]), 0.01).unwrap();
        assert_eq!(odd.subpaths().len(), 10);
        assert!((odd.length() - 50.0).abs() < 1e-9);

        // The offset shifts the phase, and wraps in both directions.
        let shifted = dash(
            &l,
            &Dash {
                pattern: vec![10.0, 5.0],
                offset: 10.0,
            },
            0.01,
        )
        .unwrap();
        // Starts 10 into the pattern: off for 5, then on 5-15, 20-30, ... 95-100.
        assert!(
            (shifted.length() - 65.0).abs() < 1e-9,
            "{}",
            shifted.length()
        );
        assert!((shifted.subpaths()[0].start.x - 5.0).abs() < 1e-9);
        let wrapped = dash(
            &l,
            &Dash {
                pattern: vec![10.0, 5.0],
                offset: -5.0,
            },
            0.01,
        )
        .unwrap();
        assert_eq!(
            wrapped.length(),
            dash(
                &l,
                &Dash {
                    pattern: vec![10.0, 5.0],
                    offset: 40.0,
                },
                0.01,
            )
            .unwrap()
            .length(),
            "offsets a whole number of periods apart must agree"
        );
    }

    #[test]
    fn dashing_conserves_length_across_a_multi_segment_path() {
        // The pattern must carry across corners rather than restarting at each.
        // The corner is deliberately 27 units along - three into a six-unit
        // period - so one "on" run really does straddle it. A corner that
        // happened to land on a period boundary would prove nothing.
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0))
            .line_to(point(27.0, 0.0))
            .line_to(point(27.0, 27.0));
        let d = dash(&p, &Dash::new(vec![4.0, 2.0]), 0.01).unwrap();
        // 54 units of path is exactly nine periods, two thirds of each on.
        assert!((d.length() - 36.0).abs() < 1e-9, "{}", d.length());
        assert_eq!(d.subpaths().len(), 9);
        // One run straddles the corner, so it has three points.
        assert!(d.subpaths().iter().any(|s| s.segments.len() == 2));
    }

    /// A dash that wraps across a closed subpath's start vertex is *one* dash,
    /// not two. Before this, the run arriving at the rectangle's first corner
    /// and the run leaving it were emitted separately, so the corner got two
    /// butt caps facing each other — a notch under [`Join::Miter`], and doubled
    /// overlapping ink under a round or square cap.
    #[test]
    fn a_dash_wrapping_a_closed_subpaths_start_is_one_run_with_a_join() {
        let b = Bounds::from_xywh(0.0, 0.0, 25.0, 25.0);
        let r = shapes::rect(b);
        let pattern = Dash::new(vec![10.0, 5.0]);
        // A perimeter of 100 against a 15-unit period: six whole periods and a
        // ten-unit tail, so the tail runs straight on into the leading dash.
        let d = dash(&r, &pattern, 0.01).unwrap();
        assert_eq!(d.subpaths().len(), 6, "the wrapping dash must not be split");
        assert!((d.length() - 70.0).abs() < 1e-9, "{}", d.length());

        let corner = Point::ZERO;
        let subs = d.subpaths();
        let touching: Vec<_> = subs
            .iter()
            .filter(|s| {
                s.start.distance(corner) < 1e-9
                    || s.segments.iter().any(|g| g.end().distance(corner) < 1e-9)
            })
            .collect();
        assert_eq!(touching.len(), 1, "one run only may touch the start vertex");
        let w = touching[0];
        // It passes *through* the corner instead of starting or ending there:
        // twenty units of dash, ten either side, with a real vertex between.
        assert!(!w.closed);
        assert_eq!(w.segments.len(), 2, "one segment either side of the corner");
        assert!(
            (w.start - point(0.0, 10.0)).length() < 1e-9,
            "{:?}",
            w.start
        );
        assert!(
            (w.end() - point(10.0, 0.0)).length() < 1e-9,
            "{:?}",
            w.end()
        );
        let run_len: f64 = w.segments.iter().map(|g| g.length(1e-6)).sum();
        assert!((run_len - 20.0).abs() < 1e-9, "{run_len}");

        // And the ink follows. Mitred at half-width one, the wrapped corner
        // reaches its outer point at (-1, -1); two butt caps would leave that
        // whole square empty.
        let o = stroke(
            &r,
            &StrokeStyle {
                dash: Some(pattern.clone()),
                ..StrokeStyle::new(2.0)
            },
        )
        .unwrap();
        assert!(
            contains(&o, point(-0.5, -0.5), FillRule::NonZero),
            "the wrapped corner was not joined"
        );
        // A corner the pattern really does interrupt still gets its two caps:
        // (25, 0) falls inside an "off" stretch and stays empty, so this is not
        // just an outline that covers everything.
        assert!(!contains(&o, point(25.5, -0.5), FillRule::NonZero));
    }

    /// The same rule with nothing to splice: a pattern whose first "on" entry
    /// outlasts the whole ring never cuts it, so it must come back as a ring
    /// rather than as an open run whose two ends meet at a pair of caps.
    #[test]
    fn a_closed_subpath_the_pattern_never_interrupts_stays_closed() {
        let b = Bounds::from_xywh(0.0, 0.0, 25.0, 25.0);
        let r = shapes::rect(b);
        let pattern = Dash::new(vec![200.0, 5.0]);
        let d = dash(&r, &pattern, 0.01).unwrap();
        assert_eq!(d.subpaths().len(), 1);
        assert!(d.subpaths()[0].closed, "an uncut ring must stay a ring");
        assert!((d.length() - 100.0).abs() < 1e-9, "{}", d.length());
        assert_eq!(d, r, "an uncut ring is the shape it started as");
        // So stroking it is stroking the rectangle: four joins, no caps.
        let dashed = stroke(
            &r,
            &StrokeStyle {
                dash: Some(pattern),
                ..StrokeStyle::new(2.0)
            },
        )
        .unwrap();
        assert_eq!(dashed, stroke(&r, &StrokeStyle::new(2.0)).unwrap());
    }

    /// The splice is conditional, not automatic. With the phase ten units in,
    /// the walk leaves (0, 0) in an "off" stretch, so the run that arrives back
    /// there is a dash in its own right and keeps both of its caps.
    #[test]
    fn a_closed_dash_that_starts_off_at_the_start_vertex_is_not_spliced() {
        let r = shapes::rect(Bounds::from_xywh(0.0, 0.0, 25.0, 25.0));
        let d = dash(
            &r,
            &Dash {
                pattern: vec![10.0, 5.0],
                offset: 10.0,
            },
            0.01,
        )
        .unwrap();
        // Off 0-5, then six ten-unit dashes, then off 90-95 and on 95-100.
        assert_eq!(d.subpaths().len(), 7);
        assert!((d.length() - 65.0).abs() < 1e-9, "{}", d.length());
        let subs = d.subpaths();
        let corner = Point::ZERO;
        assert_eq!(
            subs.iter()
                .filter(|s| s.end().distance(corner) < 1e-9)
                .count(),
            1,
            "exactly one run ends at the start vertex"
        );
        assert_eq!(
            subs.iter()
                .filter(|s| s.start.distance(corner) < 1e-9)
                .count(),
            0,
            "and none leaves it, so there is nothing to splice onto"
        );
        assert!(subs.iter().all(|s| !s.closed));
    }

    /// The smallest ring the rule has to survive. `M 0 0 L 10 0 Z` is a legal
    /// closed subpath: its ring is twenty units long, out along the segment and
    /// back along the implied closing edge. Walked all the way round it is the
    /// shortest run that can be a ring at all — `[p0, p1, p0]`, exactly the
    /// three points the `run.len() > 2` test admits — so it is where a rule that
    /// demanded one vertex more would quietly hand back an open there-and-back
    /// polyline instead: the same twenty units wound twice, with two butt caps
    /// meeting at (0, 0) rather than a join.
    #[test]
    fn a_two_vertex_closed_subpath_keeps_both_of_its_legs() {
        let p = crate::svg::parse("M 0 0 L 10 0 Z").unwrap();

        // Uncut: the pattern's first "on" entry outlasts the whole ring.
        let whole = dash(&p, &Dash::new(vec![100.0, 5.0]), 0.01).unwrap();
        assert_eq!(whole.subpaths().len(), 1);
        assert!(whole.subpaths()[0].closed, "an uncut ring must stay a ring");
        assert_eq!(
            whole.subpaths()[0].segments.len(),
            2,
            "both legs of the ring survive"
        );
        assert!((whole.length() - 20.0).abs() < 1e-9, "{}", whole.length());
        assert_eq!(whole, p, "an uncut ring is the shape it started as");

        // And the ink follows. Handed back open the run doubles back on itself,
        // so the outline winds the same twenty units twice — 80 in
        // `signed_area2` against the 40 the solid stroke paints.
        assert_eq!(
            stroke(
                &p,
                &StrokeStyle {
                    dash: Some(Dash::new(vec![100.0, 5.0])),
                    ..StrokeStyle::new(2.0)
                }
            )
            .unwrap(),
            stroke(&p, &StrokeStyle::new(2.0)).unwrap(),
            "an uninterrupted pattern must not change the ink"
        );

        // Cut once: on 0-15, off 15-18, on 18-20, and the tail splices through
        // the start vertex onto the head — seventeen units of ink either way.
        let cut = dash(&p, &Dash::new(vec![15.0, 3.0]), 0.01).unwrap();
        assert_eq!(cut.subpaths().len(), 1);
        assert!((cut.length() - 17.0).abs() < 1e-9, "{}", cut.length());
    }

    #[test]
    fn a_dashed_stroke_paints_less_than_a_solid_one() {
        let l = shapes::line(point(0.0, 0.0), point(100.0, 0.0));
        let solid = stroke(&l, &StrokeStyle::new(4.0)).unwrap();
        let dashed = stroke(
            &l,
            &StrokeStyle {
                dash: Some(Dash::new(vec![10.0, 10.0])),
                ..StrokeStyle::new(4.0)
            },
        )
        .unwrap();
        assert_eq!(area(&solid), 400.0);
        assert!((area(&dashed) - 200.0).abs() < 0.5, "{}", area(&dashed));
        assert_eq!(dashed.subpaths().len(), 5);
    }

    #[test]
    fn an_unusable_dash_pattern_leaves_the_path_alone() {
        let l = shapes::line(point(0.0, 0.0), point(10.0, 0.0));
        for bad in [
            Dash::new(vec![]),
            Dash::new(vec![0.0, 0.0]),
            Dash::new(vec![-1.0, 5.0]),
            Dash::new(vec![f64::NAN, 5.0]),
            Dash {
                pattern: vec![1.0],
                offset: f64::INFINITY,
            },
        ] {
            assert_eq!(dash(&l, &bad, 0.01).unwrap(), l, "{bad:?}");
        }
        // A zero entry inside an otherwise usable pattern does not hang, and
        // does not silently swallow the rest of the path either. Over ten units
        // the period 0/2/3/0 runs: a dot at 0, off to 2, on 2-5, a dot at 5,
        // off to 7, on 7-10. Asserted exactly, because an upper bound is
        // satisfied by dropping everything.
        let z = dash(&l, &Dash::new(vec![0.0, 2.0, 3.0, 0.0]), 0.01).unwrap();
        assert_eq!(z.length(), 6.0);
        assert_eq!(z.subpaths().len(), 4);
        let starts: Vec<f64> = z.subpaths().iter().map(|s| s.start.x).collect();
        assert_eq!(starts, vec![0.0, 2.0, 5.0, 7.0]);
        assert_eq!(z.bounds(), Bounds::new(point(0.0, 0.0), point(10.0, 0.0)));
    }

    #[test]
    fn a_zero_off_run_draws_abutting_dashes_not_a_quarter_of_the_path() {
        // `[5, 0]` is five on, nothing off: a solid line delivered as abutting
        // five-unit runs. Every unit of the path has to survive.
        let l = shapes::line(point(0.0, 0.0), point(100.0, 0.0));
        let d = dash(&l, &Dash::new(vec![5.0, 0.0]), 0.01).unwrap();
        assert_eq!(d.length(), 100.0);
        assert_eq!(d.subpaths().len(), 20);
        assert_eq!(d.bounds(), Bounds::new(point(0.0, 0.0), point(100.0, 0.0)));
        for (i, s) in d.subpaths().iter().enumerate() {
            assert!((s.start.x - i as f64 * 5.0).abs() < 1e-9, "run {i} starts");
            assert!(
                (s.end().x - (i + 1) as f64 * 5.0).abs() < 1e-9,
                "run {i} ends"
            );
        }
        // And stroked, it paints the same ink as the undashed line.
        let solid = stroke(&l, &StrokeStyle::new(4.0)).unwrap();
        let dashed = stroke(
            &l,
            &StrokeStyle {
                dash: Some(Dash::new(vec![5.0, 0.0])),
                ..StrokeStyle::new(4.0)
            },
        )
        .unwrap();
        assert_eq!(area(&solid), 400.0);
        assert_eq!(area(&dashed), 400.0);
    }

    #[test]
    fn a_zero_on_run_is_a_dot_not_a_solid_line() {
        // `stroke-dasharray: 0 8` plus a round cap is how a dotted line is
        // spelled. Every "on" run is zero-length, so the path carries no length
        // at all and the ink is entirely the caps'.
        let l = shapes::line(point(0.0, 0.0), point(60.0, 0.0));
        let d = dash(&l, &Dash::new(vec![0.0, 8.0]), 0.01).unwrap();
        assert_eq!(d.length(), 0.0, "every run must be zero-length");
        // Dots at 0, 8, ... 56: eight of them, not one long stripe.
        assert_eq!(d.subpaths().len(), 8);
        let starts: Vec<f64> = d.subpaths().iter().map(|s| s.start.x).collect();
        assert_eq!(starts, vec![0.0, 8.0, 16.0, 24.0, 32.0, 40.0, 48.0, 56.0]);

        let dotted = stroke(
            &l,
            &StrokeStyle {
                width: 4.0,
                cap: Cap::Round,
                dash: Some(Dash::new(vec![0.0, 8.0])),
                tolerance: 0.001,
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        assert_eq!(dotted.subpaths().len(), 8, "eight separate dots");
        assert_eq!(dotted.bounds().min.x, -2.0);
        assert_eq!(dotted.bounds().max.x, 58.0);
        let one_dot = std::f64::consts::PI * 4.0;
        assert!(
            (area(&dotted) - 8.0 * one_dot).abs() < 0.5,
            "{}",
            area(&dotted)
        );
        // A butt cap has no area, so the same pattern paints nothing.
        let butt = stroke(
            &l,
            &StrokeStyle {
                width: 4.0,
                dash: Some(Dash::new(vec![0.0, 8.0])),
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        assert!(butt.is_empty());

        // Longer path, same rule: 100 units at a period of 5 is twenty dots and
        // no length whatsoever - not 75 units of solid line.
        let long = shapes::line(point(0.0, 0.0), point(100.0, 0.0));
        let many = dash(&long, &Dash::new(vec![0.0, 5.0]), 0.01).unwrap();
        assert_eq!(many.length(), 0.0);
        assert_eq!(many.subpaths().len(), 20);
    }

    #[test]
    fn extreme_but_finite_coordinates_stroke_without_panicking() {
        // Legal path data whose `b - a` overflows to infinity, so the segment
        // has no direction and neither side of it can be offset. This came
        // straight out of a file, so it must not abort the process.
        let p = crate::svg::parse("M-1e308 0 L1e308 0").unwrap();
        for join in [Join::Miter, Join::Round, Join::Bevel] {
            for cap in [Cap::Butt, Cap::Round, Cap::Square] {
                let o = stroke(
                    &p,
                    &StrokeStyle {
                        join,
                        cap,
                        ..StrokeStyle::new(2.0)
                    },
                )
                .unwrap();
                assert!(o.is_finite(), "{join:?}/{cap:?} produced NaN geometry");
                // Nothing is painted, and that is the honest answer rather than
                // a shrug: at a magnitude of 1e308 one ulp is about 1e292, so a
                // half-width of 1 does not move a point at all and even a round
                // cap has no representable extent to draw.
                assert!(o.is_empty(), "{join:?}/{cap:?} invented {o:?}");
            }
        }

        // The same overflow inside a closed subpath.
        let closed = crate::svg::parse("M-1e308 0 L1e308 0 L0 1e308 Z").unwrap();
        for cap in [Cap::Butt, Cap::Round, Cap::Square] {
            let o = stroke(
                &closed,
                &StrokeStyle {
                    cap,
                    ..StrokeStyle::new(2.0)
                },
            )
            .unwrap();
            assert!(o.is_finite(), "{cap:?} produced NaN geometry");
        }

        // And dashing it: the overflowing edge has no measurable length to walk
        // along, so the pattern cannot advance across it and it passes through
        // whole rather than being cut at a NaN. The merely-enormous edges of
        // the triangle are refused as too complex for the run limit. Either way
        // it is an answer, not an abort.
        let undashable = dash(&p, &Dash::new(vec![4.0, 2.0]), 0.01).unwrap();
        assert!(undashable.is_finite());
        assert_eq!(undashable.subpaths().len(), 1);
        assert_eq!(undashable.bounds(), p.bounds());
        assert!(matches!(
            dash(&closed, &Dash::new(vec![4.0, 2.0]), 0.01),
            Err(VectorError::TooComplex { .. })
        ));

        // A merely huge - but subtractable - path still strokes to real ink.
        let big = crate::svg::parse("M-1e150 0 L1e150 0").unwrap();
        let o = stroke(&big, &StrokeStyle::new(2.0)).unwrap();
        assert!(o.is_finite());
        assert_eq!(o.bounds().height(), 2.0);
    }

    #[test]
    fn a_dash_pattern_finer_than_the_path_is_an_error_not_a_hang() {
        let l = shapes::line(point(0.0, 0.0), point(1_000_000.0, 0.0));
        assert!(matches!(
            dash(&l, &Dash::new(vec![0.001, 0.001]), 0.01),
            Err(VectorError::TooComplex { .. })
        ));
        // Including patterns with a zero entry, which is where a "the pattern
        // is stuck" heuristic used to fire instead - and where it silently
        // deleted the rest of the path. The run limit is the only bound needed.
        for bad in [vec![0.0, 0.001], vec![0.001, 0.0], vec![0.0, 0.001, 0.0]] {
            assert!(
                matches!(
                    dash(&l, &Dash::new(bad.clone()), 0.01),
                    Err(VectorError::TooComplex { .. })
                ),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn a_zero_length_subpath_paints_only_when_the_cap_has_area() {
        let mut dot = Path::new();
        dot.move_to(point(10.0, 10.0));

        assert!(stroke(&dot, &StrokeStyle::new(8.0)).unwrap().is_empty());

        let sq = stroke(
            &dot,
            &StrokeStyle {
                cap: Cap::Square,
                ..StrokeStyle::new(8.0)
            },
        )
        .unwrap();
        assert_eq!(area(&sq), 64.0);
        assert_eq!(sq.bounds(), Bounds::new(point(6.0, 6.0), point(14.0, 14.0)));

        let rd = stroke(
            &dot,
            &StrokeStyle {
                cap: Cap::Round,
                tolerance: 0.001,
                ..StrokeStyle::new(8.0)
            },
        )
        .unwrap();
        let expected = std::f64::consts::PI * 16.0;
        assert!((area(&rd) - expected).abs() < 0.2, "{}", area(&rd));
        assert!(rd.signed_area2(0.01) > 0.0);
    }

    #[test]
    fn degenerate_stroke_input_never_panics() {
        let l = shapes::line(point(0.0, 0.0), point(10.0, 0.0));
        for w in [0.0, -3.0, f64::NAN, f64::INFINITY] {
            assert!(stroke(&l, &StrokeStyle::new(w)).unwrap().is_empty(), "{w}");
        }
        assert!(stroke(&Path::new(), &StrokeStyle::new(4.0))
            .unwrap()
            .is_empty());

        // A repeated point, a doubled-back segment, and a NaN vertex.
        let mut weird = Path::new();
        weird
            .move_to(point(0.0, 0.0))
            .line_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .line_to(point(0.0, 0.0))
            .line_to(point(f64::NAN, 5.0))
            .line_to(point(5.0, 5.0));
        for join in [Join::Miter, Join::Round, Join::Bevel] {
            for cap in [Cap::Butt, Cap::Round, Cap::Square] {
                let o = stroke(
                    &weird,
                    &StrokeStyle {
                        join,
                        cap,
                        ..StrokeStyle::new(3.0)
                    },
                )
                .unwrap();
                assert!(o.is_finite(), "{join:?}/{cap:?} produced NaN geometry");
                assert!(area(&o) > 0.0);
            }
        }

        // A tolerance that is zero or absurd falls back rather than dividing.
        for tol in [0.0, -1.0, f64::NAN, 1e12] {
            let o = stroke(
                &l,
                &StrokeStyle {
                    tolerance: tol,
                    cap: Cap::Round,
                    ..StrokeStyle::new(4.0)
                },
            )
            .unwrap();
            assert!(o.is_finite() && area(&o) > 0.0, "tolerance {tol}");
        }
    }

    #[test]
    fn a_stroked_shape_surrounds_the_shape_it_outlines() {
        // The property a user actually sees: the stroke straddles the path.
        let r = shapes::rect(Bounds::from_xywh(20.0, 20.0, 60.0, 40.0));
        let o = stroke(
            &r,
            &StrokeStyle {
                width: 8.0,
                join: Join::Miter,
                ..StrokeStyle::default()
            },
        )
        .unwrap();
        assert_eq!(
            o.bounds(),
            Bounds::new(point(16.0, 16.0), point(84.0, 64.0))
        );
        // On the path: painted. Well inside or well outside: not.
        assert!(contains(&o, point(50.0, 20.0), FillRule::NonZero));
        assert!(contains(&o, point(50.0, 22.0), FillRule::NonZero));
        assert!(!contains(&o, point(50.0, 40.0), FillRule::NonZero));
        assert!(!contains(&o, point(50.0, 10.0), FillRule::NonZero));
        // Area of a frame: outer 68x48 minus inner 52x32.
        assert!(
            (area(&o) - (68.0 * 48.0 - 52.0 * 32.0)).abs() < 0.5,
            "{}",
            area(&o)
        );
    }
}
