//! A path as the Pen and Direct Selection tools see it: anchors with handles.
//!
//! [`crate::Path`] stores drawing commands, where a control point belongs to a
//! *segment*. An editor thinks in *anchors*: points the path passes through,
//! each with an incoming and an outgoing handle. Adding, deleting and
//! converting an anchor are one-line edits in that form and awkward ones in
//! command form, so the anchor edits live here and convert on the way in and
//! out.
//!
//! The conversion is exact: a line is an anchor pair with zero handles on the
//! facing sides, a quadratic is carried as its exact cubic, and a cubic keeps
//! its two controls as handle offsets. Going back, a segment whose facing
//! handles are both zero is written as a line and anything else as a cubic.

use crate::path::{Path, PathEl};
use crate::point::Point;
use crate::segment::Segment;

/// One anchor: where the path passes, and its handles as offsets from there.
/// A zero offset is "no handle" — the segment on that side is straight there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub pos: Point,
    pub handle_in: Point,
    pub handle_out: Point,
}

impl Anchor {
    /// An anchor with no handles.
    pub fn corner(pos: Point) -> Self {
        Self {
            pos,
            handle_in: Point::ZERO,
            handle_out: Point::ZERO,
        }
    }

    /// `true` when either handle is non-zero.
    pub fn is_smooth(&self) -> bool {
        self.handle_in != Point::ZERO || self.handle_out != Point::ZERO
    }
}

/// One subpath as a run of anchors.
#[derive(Debug, Clone, PartialEq)]
pub struct AnchorPath {
    pub anchors: Vec<Anchor>,
    pub closed: bool,
}

/// Where an anchor sits: which subpath, which anchor of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorRef {
    pub subpath: usize,
    pub index: usize,
}

/// The fewest anchors an open subpath keeps after a delete (a line).
pub const MIN_OPEN_ANCHORS: usize = 2;
/// The fewest anchors a closed subpath keeps after a delete (a triangle).
pub const MIN_CLOSED_ANCHORS: usize = 3;

/// Every subpath of `path` as anchors.
pub fn from_path(path: &Path) -> Vec<AnchorPath> {
    let mut out = Vec::new();
    for sp in path.subpaths() {
        let mut anchors = vec![Anchor::corner(sp.start)];
        let count = sp.segments.len();
        for (k, seg) in sp.segments.iter().enumerate() {
            let Segment::Cubic(a, c1, c2, b) = seg.to_cubic() else {
                continue;
            };
            let (out_h, in_h) = match seg {
                Segment::Line(..) => (Point::ZERO, Point::ZERO),
                _ => (c1 - a, c2 - b),
            };
            if let Some(last) = anchors.last_mut() {
                last.handle_out = out_h;
            }
            // The last segment of a closed ring arrives back at the start: it
            // shapes the first anchor's incoming handle rather than adding a
            // duplicate anchor on top of it.
            if sp.closed && k + 1 == count && b.distance_squared(sp.start) <= 1e-18 {
                anchors[0].handle_in = in_h;
            } else {
                anchors.push(Anchor {
                    pos: b,
                    handle_in: in_h,
                    handle_out: Point::ZERO,
                });
            }
        }
        out.push(AnchorPath {
            anchors,
            closed: sp.closed,
        });
    }
    out
}

fn join(path: &mut Path, a: &Anchor, b: &Anchor) {
    if a.handle_out == Point::ZERO && b.handle_in == Point::ZERO {
        path.line_to(b.pos);
    } else {
        path.curve_to(a.pos + a.handle_out, b.pos + b.handle_in, b.pos);
    }
}

/// The subpaths back as one path.
pub fn to_path(subpaths: &[AnchorPath]) -> Path {
    let mut path = Path::new();
    for sp in subpaths {
        let Some(first) = sp.anchors.first() else {
            continue;
        };
        path.move_to(first.pos);
        for pair in sp.anchors.windows(2) {
            join(&mut path, &pair[0], &pair[1]);
        }
        if sp.closed {
            if let Some(last) = sp.anchors.last() {
                if sp.anchors.len() > 1
                    && (last.handle_out != Point::ZERO || first.handle_in != Point::ZERO)
                {
                    // A curved closing segment is spelled out; a straight one
                    // is what `Z` draws.
                    join(&mut path, last, first);
                }
            }
            path.close();
        }
    }
    path
}

/// The anchor nearest `p`, if one lies within `radius`.
pub fn anchor_near(subpaths: &[AnchorPath], p: Point, radius: f64) -> Option<AnchorRef> {
    let mut best: Option<(AnchorRef, f64)> = None;
    for (s, sp) in subpaths.iter().enumerate() {
        for (i, a) in sp.anchors.iter().enumerate() {
            let d = a.pos.distance(p);
            if d <= radius && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((
                    AnchorRef {
                        subpath: s,
                        index: i,
                    },
                    d,
                ));
            }
        }
    }
    best.map(|(r, _)| r)
}

/// Remove an anchor. Refused (`false`, nothing changed) when the subpath
/// would drop below [`MIN_OPEN_ANCHORS`] / [`MIN_CLOSED_ANCHORS`].
pub fn delete_anchor(subpaths: &mut [AnchorPath], at: AnchorRef) -> bool {
    let Some(sp) = subpaths.get_mut(at.subpath) else {
        return false;
    };
    let min = if sp.closed {
        MIN_CLOSED_ANCHORS
    } else {
        MIN_OPEN_ANCHORS
    };
    if at.index >= sp.anchors.len() || sp.anchors.len() <= min {
        return false;
    }
    sp.anchors.remove(at.index);
    true
}

/// Toggle an anchor between corner and smooth.
///
/// A smooth anchor loses both handles. A corner grows a symmetric pair along
/// the line through its neighbours, each a third of the distance to the
/// neighbour on its side — the tangent a Catmull-Rom curve would give it.
/// An end of an open path takes its one neighbour's direction.
pub fn convert_anchor(subpaths: &mut [AnchorPath], at: AnchorRef) -> bool {
    let Some(sp) = subpaths.get_mut(at.subpath) else {
        return false;
    };
    let n = sp.anchors.len();
    if at.index >= n {
        return false;
    }
    let a = sp.anchors[at.index];
    if a.is_smooth() {
        sp.anchors[at.index].handle_in = Point::ZERO;
        sp.anchors[at.index].handle_out = Point::ZERO;
        return true;
    }
    let neighbour = |i: isize| -> Option<Point> {
        if sp.closed {
            let j = (i.rem_euclid(n as isize)) as usize;
            (j != at.index).then(|| sp.anchors[j].pos)
        } else if i >= 0 && (i as usize) < n {
            Some(sp.anchors[i as usize].pos)
        } else {
            None
        }
    };
    let i = at.index as isize;
    let prev = neighbour(i - 1);
    let next = neighbour(i + 1);
    let dir = match (prev, next) {
        (Some(p), Some(q)) => q - p,
        (Some(p), None) => a.pos - p,
        (None, Some(q)) => q - a.pos,
        (None, None) => return false,
    }
    .normalize();
    if dir == Point::ZERO {
        return false;
    }
    let out_len = next.map_or(0.0, |q| q.distance(a.pos)) / 3.0;
    let in_len = prev.map_or(0.0, |p| p.distance(a.pos)) / 3.0;
    let len_out = if out_len > 0.0 { out_len } else { in_len };
    let len_in = if in_len > 0.0 { in_len } else { out_len };
    sp.anchors[at.index].handle_out = dir * len_out;
    sp.anchors[at.index].handle_in = -(dir * len_in);
    true
}

/// The segment leaving anchor `i` of `sp`, if there is one.
fn segment_from(sp: &AnchorPath, i: usize) -> Option<Segment> {
    let n = sp.anchors.len();
    let j = if i + 1 < n {
        i + 1
    } else if sp.closed && n > 1 {
        0
    } else {
        return None;
    };
    let (a, b) = (sp.anchors[i], sp.anchors[j]);
    Some(
        if a.handle_out == Point::ZERO && b.handle_in == Point::ZERO {
            Segment::Line(a.pos, b.pos)
        } else {
            Segment::Cubic(a.pos, a.pos + a.handle_out, b.pos + b.handle_in, b.pos)
        },
    )
}

/// Insert an anchor where the path passes within `tolerance` of `p`, splitting
/// that segment so the outline does not move. Returns where it went.
pub fn insert_anchor(subpaths: &mut [AnchorPath], p: Point, tolerance: f64) -> Option<AnchorRef> {
    // The nearest (subpath, segment start, parameter, distance).
    let mut best: Option<(usize, usize, f64, f64)> = None;
    for (s, sp) in subpaths.iter().enumerate() {
        for i in 0..sp.anchors.len() {
            let Some(seg) = segment_from(sp, i) else {
                continue;
            };
            let t = nearest_param(&seg, p);
            let d = seg.eval(t).distance(p);
            if d <= tolerance && best.is_none_or(|(.., bd)| d < bd) {
                best = Some((s, i, t, d));
            }
        }
    }
    let (s, i, t, _) = best?;
    // Splitting on an existing anchor would stack two anchors on one point.
    if !(1e-6..=1.0 - 1e-6).contains(&t) {
        return None;
    }
    let sp = &mut subpaths[s];
    let seg = segment_from(sp, i)?;
    let n = sp.anchors.len();
    let j = if i + 1 < n { i + 1 } else { 0 };
    let new = match seg.split(t) {
        (Segment::Cubic(_, c1, c2, m), Segment::Cubic(_, d1, d2, _)) => {
            sp.anchors[i].handle_out = c1 - sp.anchors[i].pos;
            let end = sp.anchors[j].pos;
            sp.anchors[j].handle_in = d2 - end;
            Anchor {
                pos: m,
                handle_in: c2 - m,
                handle_out: d1 - m,
            }
        }
        (Segment::Line(_, m), _) => Anchor::corner(m),
        _ => return None,
    };
    let index = i + 1;
    sp.anchors.insert(index, new);
    Some(AnchorRef { subpath: s, index })
}

/// The parameter on `seg` nearest `p`: a coarse scan, then a bisection-style
/// refine around the best sample.
fn nearest_param(seg: &Segment, p: Point) -> f64 {
    const SAMPLES: usize = 64;
    let mut best_t = 0.0;
    let mut best_d = f64::INFINITY;
    for k in 0..=SAMPLES {
        let t = k as f64 / SAMPLES as f64;
        let d = seg.eval(t).distance_squared(p);
        if d < best_d {
            best_d = d;
            best_t = t;
        }
    }
    let mut step = 1.0 / SAMPLES as f64;
    for _ in 0..30 {
        step *= 0.5;
        for t in [best_t - step, best_t + step] {
            let t = t.clamp(0.0, 1.0);
            let d = seg.eval(t).distance_squared(p);
            if d < best_d {
                best_d = d;
                best_t = t;
            }
        }
    }
    best_t
}

/// Every on-curve anchor position in `path`, in order — what an editor draws
/// as squares.
pub fn anchor_points(path: &Path) -> Vec<Point> {
    from_path(path)
        .iter()
        .flat_map(|sp| sp.anchors.iter().map(|a| a.pos))
        .collect()
}

/// `true` when two paths have the same elements up to `eps` per coordinate.
pub fn same_outline(a: &Path, b: &Path, eps: f64) -> bool {
    let (ea, eb) = (a.elements(), b.elements());
    ea.len() == eb.len()
        && ea.iter().zip(eb).all(|(x, y)| {
            let pts = |e: &PathEl| -> Vec<Point> {
                match *e {
                    PathEl::MoveTo(p) | PathEl::LineTo(p) => vec![p],
                    PathEl::QuadTo(c, p) => vec![c, p],
                    PathEl::CurveTo(c1, c2, p) => vec![c1, c2, p],
                    PathEl::ClosePath => vec![],
                }
            };
            std::mem::discriminant(x) == std::mem::discriminant(y)
                && pts(x).iter().zip(pts(y)).all(|(p, q)| p.distance(q) <= eps)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::point::point;

    fn square() -> Path {
        Path::from_polyline(
            &[
                point(0.0, 0.0),
                point(100.0, 0.0),
                point(100.0, 100.0),
                point(0.0, 100.0),
            ],
            true,
        )
    }

    #[test]
    fn a_path_round_trips_through_anchors() {
        let sq = square();
        let back = to_path(&from_path(&sq));
        assert!(same_outline(&sq, &back, 1e-9), "{back:?}");

        let mut curvy = Path::new();
        curvy.move_to(point(0.0, 0.0));
        curvy.curve_to(point(10.0, 30.0), point(40.0, 30.0), point(50.0, 0.0));
        curvy.line_to(point(80.0, 0.0));
        let back = to_path(&from_path(&curvy));
        assert!(same_outline(&curvy, &back, 1e-9), "{back:?}");
        assert_eq!(from_path(&curvy)[0].anchors.len(), 3);
    }

    #[test]
    fn a_closed_ring_with_a_curved_closing_segment_has_no_duplicate_anchor() {
        let mut p = Path::new();
        p.move_to(point(0.0, 0.0));
        p.line_to(point(50.0, 0.0));
        p.line_to(point(50.0, 50.0));
        p.curve_to(point(20.0, 60.0), point(-10.0, 30.0), point(0.0, 0.0));
        p.close();
        let sps = from_path(&p);
        assert_eq!(sps[0].anchors.len(), 3);
        assert!(sps[0].anchors[0].is_smooth());
        assert!(same_outline(&p, &to_path(&sps), 1e-9));
    }

    #[test]
    fn delete_removes_an_anchor_and_refuses_below_the_minimum() {
        let mut sps = from_path(&square());
        let at = AnchorRef {
            subpath: 0,
            index: 1,
        };
        assert!(delete_anchor(&mut sps, at));
        assert_eq!(sps[0].anchors.len(), 3);
        assert!(!delete_anchor(&mut sps, at), "a triangle lost a corner");
        assert_eq!(sps[0].anchors.len(), 3);
    }

    #[test]
    fn convert_toggles_corner_and_smooth() {
        let mut sps = from_path(&square());
        let at = AnchorRef {
            subpath: 0,
            index: 1,
        };
        assert!(convert_anchor(&mut sps, at));
        let a = sps[0].anchors[1];
        assert!(a.is_smooth());
        // Symmetric in direction: a tangent, not a cusp.
        assert!(a.handle_in.normalize().distance(-a.handle_out.normalize()) < 1e-9);
        assert!(to_path(&sps)
            .elements()
            .iter()
            .any(|e| matches!(e, PathEl::CurveTo(..))));
        assert!(convert_anchor(&mut sps, at));
        assert!(!sps[0].anchors[1].is_smooth());
        assert!(same_outline(&square(), &to_path(&sps), 1e-9));
    }

    #[test]
    fn insert_splits_a_segment_without_moving_the_outline() {
        let mut sps = from_path(&square());
        let at = insert_anchor(&mut sps, point(50.0, 2.0), 5.0).expect("near the top edge");
        assert_eq!(at.index, 1);
        assert_eq!(sps[0].anchors.len(), 5);
        assert!(sps[0].anchors[1].pos.distance(point(50.0, 0.0)) < 1e-6);
        // Far from every edge: nothing.
        assert!(insert_anchor(&mut sps, point(50.0, 50.0), 5.0).is_none());

        // On a curve, the split keeps the shape: every sample of the old
        // curve lies on the new one.
        let mut curvy = Path::new();
        curvy.move_to(point(0.0, 0.0));
        curvy.curve_to(point(0.0, 60.0), point(100.0, 60.0), point(100.0, 0.0));
        let mut sps = from_path(&curvy);
        let mid = curvy.segments()[0].eval(0.5);
        insert_anchor(&mut sps, mid, 1.0).expect("on the curve");
        let after = to_path(&sps);
        assert_eq!(anchor_points(&after).len(), 3);
        for k in 0..=20 {
            let q = curvy.segments()[0].eval(k as f64 / 20.0);
            let d = crate::hit::distance_to_outline(&after, q, 0.01);
            assert!(d < 0.05, "the split moved the curve by {d}");
        }
    }
}
