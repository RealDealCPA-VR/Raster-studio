//! Shape and path editing furniture: anchor points, their control handles, and
//! the direction lines that join the two.
//!
//! A [`vector::Path`] is a list of drawing commands, not a list of anchors, so
//! this module walks it once and produces the editable topology the pen tool
//! and the direct-selection tool both need: which points are on the curve,
//! which are controls, and which control belongs to which anchor. Doing that in
//! the view rather than in `vector` keeps the geometry crate free of editor
//! concepts, and doing it once per frame keeps the hit test and the painter
//! from disagreeing about where an anchor is.

use glam::Vec2;
use vector::{Path, PathEl, Point};

use super::camera::CanvasCamera;
use super::viewport::Viewport;

/// Which side of an anchor a control handle sits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlSide {
    /// Governs the segment arriving at the anchor.
    Incoming,
    /// Governs the segment leaving it.
    Outgoing,
}

/// One editable point of a path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    /// Index into [`PathTopology::anchors`].
    pub index: usize,
    /// Which subpath it belongs to; subpaths are numbered from zero.
    pub subpath: usize,
    /// Position in document space.
    pub doc: Vec2,
    /// `true` when this is the first point of a closed subpath.
    pub closes: bool,
}

/// One control handle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlHandle {
    /// The anchor it belongs to.
    pub anchor: usize,
    pub side: ControlSide,
    /// Position in document space.
    pub doc: Vec2,
}

/// The editable structure of a path.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathTopology {
    pub anchors: Vec<Anchor>,
    pub controls: Vec<ControlHandle>,
}

impl PathTopology {
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// The direction lines: each control joined to the anchor it governs.
    pub fn direction_lines(&self) -> Vec<[Vec2; 2]> {
        self.controls
            .iter()
            .filter_map(|c| self.anchors.get(c.anchor).map(|a| [a.doc, c.doc]))
            .collect()
    }

    /// The control handles belonging to one anchor.
    pub fn controls_of(&self, anchor: usize) -> impl Iterator<Item = &ControlHandle> {
        self.controls.iter().filter(move |c| c.anchor == anchor)
    }
}

/// The most anchors one path may contribute to a frame of overlay.
pub const MAX_ANCHORS: usize = 8192;

fn v(p: Point) -> Vec2 {
    Vec2::new(p.x as f32, p.y as f32)
}

/// Walk a path and pull out its anchors and control handles.
///
/// A quadratic's single control is attributed to *both* ends, as an outgoing
/// handle of the previous anchor and an incoming handle of the next — which is
/// what it geometrically is, and what makes dragging it behave the way the pen
/// tool's users expect.
pub fn topology(path: &Path) -> PathTopology {
    let mut out = PathTopology::default();
    let mut subpath = 0usize;
    let mut subpath_started = false;
    let mut first_of_subpath: Option<usize> = None;
    let mut previous: Option<usize> = None;

    fn push_anchor(out: &mut PathTopology, doc: Vec2, subpath: usize) -> Option<usize> {
        if out.anchors.len() >= MAX_ANCHORS {
            return None;
        }
        let index = out.anchors.len();
        out.anchors.push(Anchor {
            index,
            subpath,
            doc,
            closes: false,
        });
        Some(index)
    }

    for el in path.elements() {
        match *el {
            PathEl::MoveTo(p) => {
                if subpath_started {
                    subpath += 1;
                }
                subpath_started = true;
                let idx = push_anchor(&mut out, v(p), subpath);
                first_of_subpath = idx;
                previous = idx;
            }
            PathEl::LineTo(p) => {
                let idx = push_anchor(&mut out, v(p), subpath);
                previous = idx.or(previous);
            }
            PathEl::QuadTo(c, p) => {
                let control = v(c);
                if let Some(prev) = previous {
                    out.controls.push(ControlHandle {
                        anchor: prev,
                        side: ControlSide::Outgoing,
                        doc: control,
                    });
                }
                if let Some(idx) = push_anchor(&mut out, v(p), subpath) {
                    out.controls.push(ControlHandle {
                        anchor: idx,
                        side: ControlSide::Incoming,
                        doc: control,
                    });
                    previous = Some(idx);
                }
            }
            PathEl::CurveTo(c1, c2, p) => {
                if let Some(prev) = previous {
                    out.controls.push(ControlHandle {
                        anchor: prev,
                        side: ControlSide::Outgoing,
                        doc: v(c1),
                    });
                }
                if let Some(idx) = push_anchor(&mut out, v(p), subpath) {
                    out.controls.push(ControlHandle {
                        anchor: idx,
                        side: ControlSide::Incoming,
                        doc: v(c2),
                    });
                    previous = Some(idx);
                }
            }
            PathEl::ClosePath => {
                if let Some(first) = first_of_subpath {
                    if let Some(a) = out.anchors.get_mut(first) {
                        a.closes = true;
                    }
                }
                previous = first_of_subpath;
            }
        }
    }
    out
}

/// A projected path overlay, in screen points.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PathOverlay {
    /// Anchor squares, paired with whether they are selected.
    pub anchors: Vec<(Vec2, bool)>,
    /// Control handle discs.
    pub controls: Vec<Vec2>,
    /// Lines from each anchor to its controls.
    pub direction_lines: Vec<[Vec2; 2]>,
}

impl PathOverlay {
    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }
}

/// Project a topology into screen points.
///
/// `selected` names the anchors drawn filled. Control handles are shown only
/// for selected anchors — showing every control at once turns a complex path
/// into a hairball, and no editor does it.
pub fn project(
    topology: &PathTopology,
    selected: &[usize],
    camera: &CanvasCamera,
    viewport: &Viewport,
) -> PathOverlay {
    let mut out = PathOverlay::default();
    if viewport.is_degenerate() {
        return out;
    }
    let to_screen = |p: Vec2| camera.screen_pt_of(viewport, p);
    for a in &topology.anchors {
        let p = to_screen(a.doc);
        if p.is_finite() {
            out.anchors.push((p, selected.contains(&a.index)));
        }
    }
    for c in &topology.controls {
        if !selected.contains(&c.anchor) {
            continue;
        }
        let Some(anchor) = topology.anchors.get(c.anchor) else {
            continue;
        };
        let (a, h) = (to_screen(anchor.doc), to_screen(c.doc));
        if a.is_finite() && h.is_finite() {
            out.controls.push(h);
            out.direction_lines.push([a, h]);
        }
    }
    out
}

/// The anchor nearest a screen position, within `tolerance_pt`.
pub fn hit_anchor(
    topology: &PathTopology,
    pos_pt: Vec2,
    camera: &CanvasCamera,
    viewport: &Viewport,
    tolerance_pt: f32,
) -> Option<usize> {
    if !pos_pt.is_finite() {
        return None;
    }
    let mut best: Option<(f32, usize)> = None;
    for a in &topology.anchors {
        let d = (camera.screen_pt_of(viewport, a.doc) - pos_pt).length();
        if d <= tolerance_pt && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, a.index));
        }
    }
    best.map(|(_, i)| i)
}

/// The control handle nearest a screen position, within `tolerance_pt`.
///
/// Only handles belonging to a selected anchor can be hit, matching what is
/// drawn — a hit region with nothing under it is worse than no hit region.
pub fn hit_control(
    topology: &PathTopology,
    selected: &[usize],
    pos_pt: Vec2,
    camera: &CanvasCamera,
    viewport: &Viewport,
    tolerance_pt: f32,
) -> Option<(usize, ControlSide)> {
    if !pos_pt.is_finite() {
        return None;
    }
    let mut best: Option<(f32, usize, ControlSide)> = None;
    for c in &topology.controls {
        if !selected.contains(&c.anchor) {
            continue;
        }
        let d = (camera.screen_pt_of(viewport, c.doc) - pos_pt).length();
        if d <= tolerance_pt && best.as_ref().is_none_or(|(bd, _, _)| d < *bd) {
            best = Some((d, c.anchor, c.side));
        }
    }
    best.map(|(_, anchor, side)| (anchor, side))
}

/// What a pointer is over in a path being edited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathHit {
    Anchor(usize),
    Control(usize, ControlSide),
}

/// What is under the pointer, controls first.
///
/// A control handle sits *on top of* the direction line that joins it to its
/// anchor and is drawn smaller, so when the two overlap — which they do
/// whenever a handle is retracted onto its own anchor — the control has to win,
/// or a retracted handle could never be pulled back out.
pub fn hit_test(
    topology: &PathTopology,
    selected: &[usize],
    pos_pt: Vec2,
    camera: &CanvasCamera,
    viewport: &Viewport,
    anchor_pt: f32,
    control_pt: f32,
) -> Option<PathHit> {
    if let Some((anchor, side)) =
        hit_control(topology, selected, pos_pt, camera, viewport, control_pt)
    {
        return Some(PathHit::Control(anchor, side));
    }
    hit_anchor(topology, pos_pt, camera, viewport, anchor_pt).map(PathHit::Anchor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use vector::point;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam() -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(50.0, 50.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    fn triangle() -> Path {
        Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::LineTo(point(40.0, 0.0)),
            PathEl::LineTo(point(40.0, 30.0)),
            PathEl::ClosePath,
        ])
    }

    fn curved() -> Path {
        Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::CurveTo(point(10.0, -20.0), point(30.0, -20.0), point(40.0, 0.0)),
        ])
    }

    #[test]
    fn a_polygon_yields_one_anchor_per_point_and_no_controls() {
        let t = topology(&triangle());
        assert_eq!(t.anchors.len(), 3);
        assert!(t.controls.is_empty());
        assert_eq!(t.anchors[0].doc, Vec2::ZERO);
        assert_eq!(t.anchors[1].doc, Vec2::new(40.0, 0.0));
        assert!(
            t.anchors[0].closes,
            "ClosePath marks the subpath's first point"
        );
        assert!(!t.anchors[1].closes);
        for a in &t.anchors {
            assert_eq!(a.subpath, 0);
        }
    }

    #[test]
    fn a_cubic_gives_each_end_the_control_that_governs_its_side() {
        let t = topology(&curved());
        assert_eq!(t.anchors.len(), 2);
        assert_eq!(t.controls.len(), 2);
        let out = t
            .controls
            .iter()
            .find(|c| c.side == ControlSide::Outgoing)
            .unwrap();
        assert_eq!(out.anchor, 0);
        assert_eq!(out.doc, Vec2::new(10.0, -20.0));
        let inc = t
            .controls
            .iter()
            .find(|c| c.side == ControlSide::Incoming)
            .unwrap();
        assert_eq!(inc.anchor, 1);
        assert_eq!(inc.doc, Vec2::new(30.0, -20.0));
    }

    #[test]
    fn a_quadratics_single_control_belongs_to_both_of_its_anchors() {
        let p = Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::QuadTo(point(20.0, -20.0), point(40.0, 0.0)),
        ]);
        let t = topology(&p);
        assert_eq!(t.controls.len(), 2);
        assert!(t.controls.iter().all(|c| c.doc == Vec2::new(20.0, -20.0)));
        assert!(t
            .controls
            .iter()
            .any(|c| c.anchor == 0 && c.side == ControlSide::Outgoing));
        assert!(t
            .controls
            .iter()
            .any(|c| c.anchor == 1 && c.side == ControlSide::Incoming));
    }

    #[test]
    fn several_subpaths_are_numbered_separately() {
        let p = Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::LineTo(point(10.0, 0.0)),
            PathEl::MoveTo(point(50.0, 50.0)),
            PathEl::LineTo(point(60.0, 50.0)),
        ]);
        let t = topology(&p);
        assert_eq!(t.anchors.len(), 4);
        assert_eq!(t.anchors[0].subpath, 0);
        assert_eq!(t.anchors[1].subpath, 0);
        assert_eq!(t.anchors[2].subpath, 1);
        assert_eq!(t.anchors[3].subpath, 1);
    }

    #[test]
    fn an_empty_path_yields_nothing() {
        let t = topology(&Path::new());
        assert!(t.is_empty());
        assert!(t.direction_lines().is_empty());
        assert!(project(&t, &[], &cam(), &vp()).is_empty());
    }

    #[test]
    fn a_pathological_path_is_capped() {
        let els: Vec<PathEl> = std::iter::once(PathEl::MoveTo(point(0.0, 0.0)))
            .chain((0..MAX_ANCHORS * 2).map(|i| PathEl::LineTo(point(i as f64, 0.0))))
            .collect();
        let t = topology(&Path::from_elements(els));
        assert_eq!(t.anchors.len(), MAX_ANCHORS);
    }

    #[test]
    fn projection_puts_anchors_where_the_camera_does() {
        let v = vp();
        let c = cam();
        let t = topology(&triangle());
        let o = project(&t, &[0], &c, &v);
        assert_eq!(o.anchors.len(), 3);
        assert_eq!(o.anchors[0].0, c.screen_pt_of(&v, Vec2::ZERO));
        assert!(o.anchors[0].1, "anchor 0 is selected");
        assert!(!o.anchors[1].1);
    }

    #[test]
    fn control_handles_are_shown_only_for_selected_anchors() {
        let v = vp();
        let c = cam();
        let t = topology(&curved());
        let none = project(&t, &[], &c, &v);
        assert!(none.controls.is_empty());
        assert!(none.direction_lines.is_empty());

        let first = project(&t, &[0], &c, &v);
        assert_eq!(first.controls.len(), 1);
        assert_eq!(first.direction_lines.len(), 1);
        // The direction line runs from the anchor to its control.
        let [a, h] = first.direction_lines[0];
        assert_eq!(a, c.screen_pt_of(&v, Vec2::ZERO));
        assert_eq!(h, c.screen_pt_of(&v, Vec2::new(10.0, -20.0)));

        let both = project(&t, &[0, 1], &c, &v);
        assert_eq!(both.controls.len(), 2);
    }

    #[test]
    fn direction_lines_join_every_control_to_its_anchor() {
        let t = topology(&curved());
        let lines = t.direction_lines();
        assert_eq!(lines.len(), 2);
        for (line, control) in lines.iter().zip(&t.controls) {
            assert_eq!(line[1], control.doc);
            assert_eq!(line[0], t.anchors[control.anchor].doc);
        }
        assert_eq!(t.controls_of(0).count(), 1);
        assert_eq!(t.controls_of(1).count(), 1);
        assert_eq!(t.controls_of(9).count(), 0);
    }

    #[test]
    fn clicking_an_anchor_finds_it_and_clicking_empty_space_does_not() {
        let v = vp();
        let c = cam();
        let t = topology(&triangle());
        for a in &t.anchors {
            let at = c.screen_pt_of(&v, a.doc);
            assert_eq!(hit_anchor(&t, at, &c, &v, 6.0), Some(a.index));
            // Just outside the tolerance, nothing is grabbed.
            assert_eq!(hit_anchor(&t, at + Vec2::new(9.0, 0.0), &c, &v, 6.0), None);
        }
        assert_eq!(hit_anchor(&t, Vec2::new(f32::NAN, 0.0), &c, &v, 6.0), None);
    }

    #[test]
    fn the_nearest_anchor_wins_when_two_are_close() {
        let v = vp();
        let c = cam();
        let p = Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::LineTo(point(2.0, 0.0)),
        ]);
        let t = topology(&p);
        // One document pixel apart is two screen points at this camera.
        let near_second = c.screen_pt_of(&v, Vec2::new(1.6, 0.0));
        assert_eq!(hit_anchor(&t, near_second, &c, &v, 8.0), Some(1));
        let near_first = c.screen_pt_of(&v, Vec2::new(0.4, 0.0));
        assert_eq!(hit_anchor(&t, near_first, &c, &v, 8.0), Some(0));
    }

    #[test]
    fn only_visible_control_handles_can_be_grabbed() {
        let v = vp();
        let c = cam();
        let t = topology(&curved());
        let at = c.screen_pt_of(&v, Vec2::new(10.0, -20.0));
        assert_eq!(hit_control(&t, &[], at, &c, &v, 6.0), None);
        assert_eq!(
            hit_control(&t, &[0], at, &c, &v, 6.0),
            Some((0, ControlSide::Outgoing))
        );
        assert_eq!(
            hit_control(&t, &[0], at + Vec2::new(40.0, 0.0), &c, &v, 6.0),
            None
        );
    }

    /// The combined hit test the canvas uses: a control beats the anchor it
    /// belongs to, and an unselected anchor's control is not there to be hit.
    #[test]
    fn the_combined_hit_test_puts_controls_above_anchors() {
        let v = vp();
        let c = cam();
        let t = topology(&curved());
        let anchor = c.screen_pt_of(&v, Vec2::ZERO);
        assert_eq!(
            hit_test(&t, &[0], anchor, &c, &v, 8.0, 6.0),
            Some(PathHit::Anchor(0))
        );
        let control = c.screen_pt_of(&v, Vec2::new(10.0, -20.0));
        assert_eq!(
            hit_test(&t, &[0], control, &c, &v, 8.0, 6.0),
            Some(PathHit::Control(0, ControlSide::Outgoing))
        );
        // With the anchor deselected its control is not drawn, so the anchor
        // itself answers instead of a handle nobody can see.
        assert_eq!(hit_test(&t, &[], control, &c, &v, 8.0, 6.0), None);
        assert_eq!(
            hit_test(&t, &[], anchor, &c, &v, 8.0, 6.0),
            Some(PathHit::Anchor(0))
        );
        // A retracted handle sits on its anchor; the control still wins, or it
        // could never be pulled back out.
        let retracted = topology(&Path::from_elements(vec![
            PathEl::MoveTo(point(0.0, 0.0)),
            PathEl::CurveTo(point(0.0, 0.0), point(30.0, -20.0), point(40.0, 0.0)),
        ]));
        assert_eq!(
            hit_test(&retracted, &[0], anchor, &c, &v, 8.0, 6.0),
            Some(PathHit::Control(0, ControlSide::Outgoing))
        );
        assert_eq!(
            hit_test(&t, &[0], anchor + Vec2::splat(400.0), &c, &v, 8.0, 6.0),
            None
        );
    }

    #[test]
    fn a_collapsed_viewport_projects_nothing() {
        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        let t = topology(&triangle());
        assert!(project(&t, &[0], &cam(), &collapsed).is_empty());
    }
}
