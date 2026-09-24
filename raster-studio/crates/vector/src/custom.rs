//! The built-in custom-shape library.
//!
//! The Custom Shape tool fits a stored path into the box the user drags, so
//! every entry here is a **normalised** path: closed, finite, and with bounds
//! exactly the unit square `(0, 0)–(1, 1)`, y down. The tool's own fit is a
//! translate-and-scale of those bounds, so a shape that filled only part of
//! the unit square would draw smaller than the box the user dragged — the
//! normalisation in [`normalised`] is what makes "the box you drag is the box
//! you get" true for every entry, hand-authored or generated.
//!
//! The library is a fixed enum rather than a file format: the options bar
//! offers it as a `Choice`, whose labels are `&'static` const data, and
//! [`CUSTOM_SHAPE_NAMES`] is built from [`CustomShape::ALL`] in a const block
//! so a new entry reaches the bar with no edit outside this file.

use std::f64::consts::FRAC_PI_2;

use crate::affine::Affine;
use crate::path::Path;
use crate::point::{point, Bounds, Point};
use crate::shapes;

/// One entry of the library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CustomShape {
    Heart,
    Star,
    Arrow,
    SpeechBubble,
    Check,
    Cross,
    Diamond,
    Hexagon,
    Lightning,
    Crescent,
}

impl CustomShape {
    /// Every entry, in the order the options bar lists them.
    pub const ALL: [CustomShape; 10] = [
        CustomShape::Heart,
        CustomShape::Star,
        CustomShape::Arrow,
        CustomShape::SpeechBubble,
        CustomShape::Check,
        CustomShape::Cross,
        CustomShape::Diamond,
        CustomShape::Hexagon,
        CustomShape::Lightning,
        CustomShape::Crescent,
    ];

    /// The label the options bar shows.
    pub const fn name(self) -> &'static str {
        match self {
            CustomShape::Heart => "Heart",
            CustomShape::Star => "Star",
            CustomShape::Arrow => "Arrow",
            CustomShape::SpeechBubble => "Speech Bubble",
            CustomShape::Check => "Check",
            CustomShape::Cross => "Cross",
            CustomShape::Diamond => "Diamond",
            CustomShape::Hexagon => "Hexagon",
            CustomShape::Lightning => "Lightning",
            CustomShape::Crescent => "Crescent",
        }
    }

    /// The entry a `Choice` index names, in [`Self::ALL`]'s order.
    pub fn from_index(index: usize) -> Option<Self> {
        Self::ALL.get(index).copied()
    }

    /// This entry's position in [`Self::ALL`] — the `Choice` index.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .expect("every variant is in ALL")
    }

    /// The shape as a closed path with bounds exactly the unit square.
    pub fn path(self) -> Path {
        let raw = match self {
            CustomShape::Heart => heart(),
            CustomShape::Star => shapes::star(point(0.5, 0.5), 0.5, 0.5 * 0.382, 5, -FRAC_PI_2),
            CustomShape::Arrow => polygon(&[
                (0.0, 0.3),
                (0.6, 0.3),
                (0.6, 0.0),
                (1.0, 0.5),
                (0.6, 1.0),
                (0.6, 0.7),
                (0.0, 0.7),
            ]),
            CustomShape::SpeechBubble => speech_bubble(),
            CustomShape::Check => polygon(&[
                (0.0, 0.55),
                (0.15, 0.4),
                (0.38, 0.65),
                (0.85, 0.1),
                (1.0, 0.25),
                (0.38, 0.95),
            ]),
            CustomShape::Cross => polygon(&[
                (0.2, 0.0),
                (0.5, 0.3),
                (0.8, 0.0),
                (1.0, 0.2),
                (0.7, 0.5),
                (1.0, 0.8),
                (0.8, 1.0),
                (0.5, 0.7),
                (0.2, 1.0),
                (0.0, 0.8),
                (0.3, 0.5),
                (0.0, 0.2),
            ]),
            CustomShape::Diamond => polygon(&[(0.5, 0.0), (1.0, 0.5), (0.5, 1.0), (0.0, 0.5)]),
            CustomShape::Hexagon => shapes::regular_polygon(point(0.5, 0.5), 0.5, 6, 0.0),
            CustomShape::Lightning => polygon(&[
                (0.55, 0.0),
                (0.15, 0.58),
                (0.42, 0.58),
                (0.3, 1.0),
                (0.85, 0.38),
                (0.55, 0.38),
                (0.7, 0.0),
            ]),
            CustomShape::Crescent => crescent(),
        };
        normalised(&raw)
    }
}

/// Every entry's label, in [`CustomShape::ALL`]'s order — the `Choice` list
/// the options bar offers. Built from the enum in a const block so the two
/// cannot disagree.
pub const CUSTOM_SHAPE_NAMES: [&str; CustomShape::ALL.len()] = {
    let mut out = [""; CustomShape::ALL.len()];
    let mut i = 0;
    while i < CustomShape::ALL.len() {
        out[i] = CustomShape::ALL[i].name();
        i += 1;
    }
    out
};

/// The unit square every library entry is fitted to.
pub fn unit_box() -> Bounds {
    Bounds::new(point(0.0, 0.0), point(1.0, 1.0))
}

/// `path` translated and scaled so its bounds are exactly the unit square.
///
/// A path with no width or no height cannot be fitted and comes back as it
/// was; nothing in the library is that thin, and `every_entry_fills_the_unit_box`
/// pins it.
pub fn normalised(path: &Path) -> Path {
    let b = path.bounds();
    let (w, h) = (b.width(), b.height());
    if !(w > 0.0 && h > 0.0 && w.is_finite() && h.is_finite()) {
        return path.clone();
    }
    let t = Affine::translate(-b.min.x, -b.min.y).then(Affine::scale(1.0 / w, 1.0 / h));
    path.transform(&t)
}

/// W10-A: one knot of a user-defined custom shape — the incoming control
/// point, the anchor and the outgoing control point, in that order. A
/// straight side's controls sit on its anchors.
pub type Knot = [Point; 3];

/// W10-A: one subpath of a user-defined custom shape: closed, and its knots.
pub type KnotRun = (bool, Vec<Knot>);

/// W10-A: `path` as cubic Bézier knots, one run per subpath — the form a
/// custom-shape library entry is stored in (Edit ▸ Define Custom Shape keeps
/// it in the shape presets beside the `.csh` imports). Lines and quadratics
/// are raised to cubics exactly; a subpath with no segments is dropped.
/// [`path_from_knots`] reads it back.
pub fn knots_of(path: &Path) -> Vec<KnotRun> {
    let mut out = Vec::new();
    for sub in path.subpaths() {
        if sub.segments.is_empty() {
            continue;
        }
        let cubics: Vec<[Point; 4]> = sub
            .segments
            .iter()
            .map(|s| match s.to_cubic() {
                crate::Segment::Cubic(a, b, c, d) => [a, b, c, d],
                // `to_cubic` answers a cubic for every kind; a line is its
                // own controls should that ever change.
                other => [other.start(), other.start(), other.end(), other.end()],
            })
            .collect();
        // A closed run's last segment ends where it began, so its end is not
        // a knot of its own: it is the first knot's incoming side.
        let anchors = if sub.closed {
            cubics.len()
        } else {
            cubics.len() + 1
        };
        let mut knots: Vec<Knot> = Vec::with_capacity(anchors);
        knots.push([sub.start; 3]);
        for c in &cubics[..anchors - 1] {
            knots.push([c[3]; 3]);
        }
        for (i, c) in cubics.iter().enumerate() {
            let to = (i + 1) % anchors;
            knots[i][2] = c[1];
            knots[to][0] = c[2];
        }
        out.push((sub.closed, knots));
    }
    out
}

/// W10-A: the path [`knots_of`] describes — each run a move to its first
/// anchor, a cubic per pair of neighbouring knots, and a close when it is
/// closed.
pub fn path_from_knots(runs: &[KnotRun]) -> Path {
    let mut p = Path::new();
    for (closed, knots) in runs {
        let Some(first) = knots.first() else {
            continue;
        };
        p.move_to(first[1]);
        for pair in knots.windows(2) {
            p.curve_to(pair[0][2], pair[1][0], pair[1][1]);
        }
        if *closed {
            let last = knots[knots.len() - 1];
            if knots.len() > 1 {
                p.curve_to(last[2], first[0], first[1]);
            }
            p.close();
        }
    }
    p
}

fn polygon(points: &[(f64, f64)]) -> Path {
    let pts: Vec<Point> = points.iter().map(|(x, y)| point(*x, *y)).collect();
    Path::from_polyline(&pts, true)
}

fn heart() -> Path {
    let mut p = Path::new();
    p.move_to(point(0.5, 0.95));
    p.curve_to(point(0.15, 0.70), point(0.0, 0.5), point(0.0, 0.3));
    p.curve_to(point(0.0, 0.1), point(0.15, 0.0), point(0.28, 0.0));
    p.curve_to(point(0.4, 0.0), point(0.5, 0.1), point(0.5, 0.22));
    p.curve_to(point(0.5, 0.1), point(0.6, 0.0), point(0.72, 0.0));
    p.curve_to(point(0.85, 0.0), point(1.0, 0.1), point(1.0, 0.3));
    p.curve_to(point(1.0, 0.5), point(0.85, 0.70), point(0.5, 0.95));
    p.close();
    p
}

fn speech_bubble() -> Path {
    let mut p = Path::new();
    p.move_to(point(0.15, 0.0));
    p.line_to(point(0.85, 0.0));
    p.curve_to(point(0.93, 0.0), point(1.0, 0.07), point(1.0, 0.15));
    p.line_to(point(1.0, 0.6));
    p.curve_to(point(1.0, 0.68), point(0.93, 0.75), point(0.85, 0.75));
    p.line_to(point(0.4, 0.75));
    p.line_to(point(0.2, 1.0));
    p.line_to(point(0.25, 0.75));
    p.line_to(point(0.15, 0.75));
    p.curve_to(point(0.07, 0.75), point(0.0, 0.68), point(0.0, 0.6));
    p.line_to(point(0.0, 0.15));
    p.curve_to(point(0.0, 0.07), point(0.07, 0.0), point(0.15, 0.0));
    p.close();
    p
}

fn crescent() -> Path {
    let mut p = Path::new();
    p.move_to(point(0.5, 0.0));
    p.curve_to(point(0.22, 0.0), point(0.0, 0.22), point(0.0, 0.5));
    p.curve_to(point(0.0, 0.78), point(0.22, 1.0), point(0.5, 1.0));
    p.curve_to(point(0.62, 1.0), point(0.72, 0.95), point(0.8, 0.88));
    p.curve_to(point(0.5, 0.85), point(0.3, 0.7), point(0.3, 0.5));
    p.curve_to(point(0.3, 0.3), point(0.5, 0.15), point(0.8, 0.12));
    p.curve_to(point(0.72, 0.05), point(0.62, 0.0), point(0.5, 0.0));
    p.close();
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{fill, FillOptions};
    use glam::IVec2;

    #[test]
    fn the_library_has_at_least_eight_entries_and_the_names_match_the_enum() {
        assert!(CustomShape::ALL.len() >= 8);
        assert_eq!(CUSTOM_SHAPE_NAMES.len(), CustomShape::ALL.len());
        for (i, shape) in CustomShape::ALL.iter().enumerate() {
            assert_eq!(CUSTOM_SHAPE_NAMES[i], shape.name());
            assert_eq!(shape.index(), i);
            assert_eq!(CustomShape::from_index(i), Some(*shape));
        }
        assert_eq!(CustomShape::from_index(CustomShape::ALL.len()), None);
        let mut names: Vec<&str> = CUSTOM_SHAPE_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            CustomShape::ALL.len(),
            "two entries share a name"
        );
    }

    #[test]
    fn every_entry_fills_the_unit_box() {
        for shape in CustomShape::ALL {
            let p = shape.path();
            assert!(!p.is_empty() && p.is_finite(), "{shape:?} is empty");
            let b = p.bounds();
            for (got, want) in [
                (b.min.x, 0.0),
                (b.min.y, 0.0),
                (b.max.x, 1.0),
                (b.max.y, 1.0),
            ] {
                assert!(
                    (got - want).abs() < 1e-9,
                    "{shape:?} bounds {b:?} are not the unit square"
                );
            }
            assert!(
                p.subpaths().iter().all(|sp| sp.closed),
                "{shape:?} has an open subpath"
            );
        }
    }

    /// Each entry, fitted into a 100x100 box the way the tool fits it, puts
    /// ink inside that box and none outside it.
    #[test]
    fn every_entry_renders_non_empty_inside_its_box() {
        for shape in CustomShape::ALL {
            let fitted = shape
                .path()
                .transform(&Affine::scale(100.0, 100.0).then(Affine::translate(20.0, 30.0)));
            let m = fill(&fitted, &FillOptions::default()).unwrap();
            assert!(
                m.area() > 100.0 * 100.0 * 0.1,
                "{shape:?} covers too little of its box: {}",
                m.area()
            );
            assert!(
                m.area() < 100.0 * 100.0,
                "{shape:?} is a plain square, not a shape: {}",
                m.area()
            );
            let (lo, hi) = m.bounds().expect("non-empty");
            assert!(lo.x >= 20 && lo.y >= 30, "{shape:?} inked before its box");
            assert!(
                hi.x <= 120 && hi.y <= 130,
                "{shape:?} inked past its box: {hi:?}"
            );
            assert_eq!(
                m.coverage_at(IVec2::new(5, 5)),
                0,
                "{shape:?} inked outside its box"
            );
        }
    }

    #[test]
    fn a_path_with_no_area_is_left_alone_by_normalisation() {
        let mut flat = Path::new();
        flat.move_to(point(0.0, 3.0));
        flat.line_to(point(10.0, 3.0));
        assert_eq!(normalised(&flat), flat);
    }

    /// W10-A: every library entry, a square with a hole and an open run
    /// survive the knot form a defined custom shape is stored in: the same
    /// subpaths, the same bounds, the same pixels.
    #[test]
    fn a_path_round_trips_through_its_knots() {
        let mut cases: Vec<Path> = CustomShape::ALL.iter().map(|s| s.path()).collect();
        let mut ring = shapes::rect(Bounds::from_xywh(0.0, 0.0, 40.0, 40.0));
        ring.extend(&shapes::circle(point(20.0, 20.0), 8.0).reversed());
        cases.push(ring);
        let mut open = Path::new();
        open.move_to(point(0.0, 0.0))
            .line_to(point(10.0, 0.0))
            .quad_to(point(15.0, 5.0), point(10.0, 10.0));
        cases.push(open);
        for path in cases {
            let runs = knots_of(&path);
            assert_eq!(runs.len(), path.subpaths().len());
            let back = path_from_knots(&runs);
            assert_eq!(back.subpaths().len(), path.subpaths().len());
            let (a, b) = (path.bounds(), back.bounds());
            assert!(
                (a.min - b.min).length() < 1e-9 && (a.max - b.max).length() < 1e-9,
                "{a:?} vs {b:?}"
            );
            if path.subpaths().iter().all(|s| s.closed) {
                let scale = Affine::scale(100.0, 100.0);
                let m1 = fill(&path.transform(&scale), &FillOptions::default()).unwrap();
                let m2 = fill(&back.transform(&scale), &FillOptions::default()).unwrap();
                assert!(
                    (m1.area() - m2.area()).abs() < 1.0,
                    "{} vs {}",
                    m1.area(),
                    m2.area()
                );
            }
        }
    }
}
