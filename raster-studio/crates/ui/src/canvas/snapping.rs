//! Snapping, and the smart guides that show why something snapped.
//!
//! A snap is decided per axis and always in **screen points**, never in
//! document pixels. That is the whole trick: a threshold of 8 document pixels
//! is invisible at 5% zoom and enormous at 3200%, whereas 8 points is the same
//! forgiving flick of the wrist at every zoom. The threshold converts through
//! [`CanvasCamera::scale_pt`] once, at the top of [`snap_point`].
//!
//! Candidates are plain numbers with a reason attached ([`SnapKind`]), so the
//! painter can draw the smart guide that explains the snap — the line through
//! the layer edge that caught it — without re-deriving anything.

use design::Space;
use glam::Vec2;

use super::camera::CanvasCamera;
use super::geom::{Axis, DocRect};
use super::grid::GridSettings;
use super::rulers::Guides;
use super::viewport::Viewport;

/// Why a candidate is a candidate. Also the tie-break order: on an exact tie
/// the earlier variant wins, so a guide the user placed by hand beats an
/// incidental layer edge at the same coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SnapKind {
    /// A guide the user dragged out of a ruler.
    Guide,
    /// An edge of the canvas.
    CanvasEdge,
    /// The middle of the canvas.
    CanvasCenter,
    /// An edge of another layer's bounds.
    LayerEdge,
    /// The middle of another layer's bounds.
    LayerCenter,
    /// A line of the document grid.
    GridLine,
    /// A whole-pixel boundary.
    PixelBoundary,
}

impl SnapKind {
    /// Whether this kind draws a smart guide when it catches. Grid and pixel
    /// snaps do not: their line is already on screen.
    pub const fn shows_smart_guide(self) -> bool {
        matches!(
            self,
            SnapKind::LayerEdge | SnapKind::LayerCenter | SnapKind::CanvasCenter
        )
    }
}

/// One thing a coordinate may snap to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapCandidate {
    pub axis: Axis,
    /// The document coordinate to land on.
    pub doc: f32,
    pub kind: SnapKind,
}

impl SnapCandidate {
    pub fn new(axis: Axis, doc: f32, kind: SnapKind) -> Self {
        Self { axis, doc, kind }
    }
}

/// What actually caught, and how far it pulled.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapHit {
    pub candidate: SnapCandidate,
    /// Distance from the original position to the candidate, in screen points.
    pub distance_pt: f32,
}

/// The result of snapping one point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapResult {
    /// The point after snapping. Equal to the input where nothing caught.
    pub point: Vec2,
    pub x: Option<SnapHit>,
    pub y: Option<SnapHit>,
}

impl SnapResult {
    /// A result that changed nothing.
    pub fn unchanged(point: Vec2) -> Self {
        Self {
            point,
            x: None,
            y: None,
        }
    }

    /// Whether either axis caught.
    pub fn snapped(&self) -> bool {
        self.x.is_some() || self.y.is_some()
    }

    /// The hits that want a smart guide drawn through them.
    pub fn smart_guides(&self) -> impl Iterator<Item = SnapHit> + '_ {
        [self.x, self.y]
            .into_iter()
            .flatten()
            .filter(|h| h.candidate.kind.shows_smart_guide())
    }
}

/// Which classes of candidate are live, and how forgiving the snap is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapSettings {
    /// The master toggle. When off, [`snap_point`] returns its input.
    pub enabled: bool,
    /// How close, in screen points, a candidate has to be to catch.
    pub threshold_pt: f32,
    pub to_guides: bool,
    pub to_grid: bool,
    pub to_canvas: bool,
    pub to_layers: bool,
    pub to_pixels: bool,
}

impl Default for SnapSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold_pt: Space::Small.pt(),
            to_guides: true,
            to_grid: true,
            to_canvas: true,
            to_layers: true,
            to_pixels: false,
        }
    }
}

impl SnapSettings {
    /// Largest threshold accepted, in points. Beyond about a centimetre the
    /// snap stops feeling like assistance and starts feeling like a fight.
    pub const MAX_THRESHOLD_PT: f32 = Space::XXLarge.units() * design::UNIT_PT;

    /// The threshold, clamped into something sane.
    pub fn threshold(&self) -> f32 {
        if self.threshold_pt.is_finite() {
            self.threshold_pt.clamp(0.0, Self::MAX_THRESHOLD_PT)
        } else {
            Self::default().threshold_pt
        }
    }

    /// Whether a kind is switched on.
    pub fn accepts(&self, kind: SnapKind) -> bool {
        match kind {
            SnapKind::Guide => self.to_guides,
            SnapKind::GridLine => self.to_grid,
            SnapKind::CanvasEdge | SnapKind::CanvasCenter => self.to_canvas,
            SnapKind::LayerEdge | SnapKind::LayerCenter => self.to_layers,
            SnapKind::PixelBoundary => self.to_pixels,
        }
    }
}

/// Everything the canvas can offer a gesture to snap against.
#[derive(Debug, Clone, Default)]
pub struct SnapSources<'a> {
    pub guides: Option<&'a Guides>,
    pub grid: Option<&'a GridSettings>,
    pub canvas: Option<DocRect>,
    /// Bounds of the layers that are *not* being dragged. Including the dragged
    /// layer would let it snap to itself and freeze in place.
    pub layers: &'a [DocRect],
}

/// The most candidates one collection may produce, so a document with ten
/// thousand layers cannot make a drag quadratic.
pub const MAX_CANDIDATES: usize = 4096;

/// Whether a tool's samples are snapped on their way to it.
///
/// Snapping is for gestures that *place* something — a move, a crop, a
/// marquee, a shape, a transform. A brush stroke, a smudge or an eyedropper
/// sample must land exactly where the hand was; pulling those onto a grid line
/// would be an unusable painting tool rather than assistance.
///
/// What the canvas can snap is the **pointer**. A move that snapped the dragged
/// layer's own edges needs that layer's bounds and its grab offset, which are
/// the tool's state and not the canvas's — a tool that wants edge snapping runs
/// [`super::CanvasView::snap`] itself with its own bounds.
pub fn tool_snaps(tool: tools::ToolId) -> bool {
    use tools::ToolId as T;
    matches!(
        tool,
        T::Move
            | T::RectMarquee
            | T::EllipseMarquee
            | T::SingleRowMarquee
            | T::SingleColumnMarquee
            | T::Crop
            | T::Slice
            | T::Rectangle
            | T::RoundedRectangle
            | T::Ellipse
            | T::Polygon
            | T::Star
            | T::Line
            | T::CustomShape
            | T::FreeTransform
    )
}

/// Gather the candidates near `around`, on both axes.
///
/// `around` bounds the search: grid lines are generated only within a
/// threshold of it, so the count stays proportional to the gesture rather than
/// to the document.
pub fn collect_candidates(
    sources: &SnapSources<'_>,
    settings: &SnapSettings,
    around: DocRect,
    threshold_doc: f32,
) -> Vec<SnapCandidate> {
    let mut out = Vec::new();
    if !settings.enabled {
        return out;
    }
    let reach = around.expanded(threshold_doc.max(0.0));

    if settings.to_guides {
        if let Some(guides) = sources.guides {
            if guides.visible {
                for g in guides.iter() {
                    out.push(SnapCandidate::new(g.axis, g.doc, SnapKind::Guide));
                }
            }
        }
    }
    if settings.to_canvas {
        if let Some(canvas) = sources.canvas {
            for (axis, lo, hi, mid) in [
                (Axis::X, canvas.min.x, canvas.max.x, canvas.center().x),
                (Axis::Y, canvas.min.y, canvas.max.y, canvas.center().y),
            ] {
                out.push(SnapCandidate::new(axis, lo, SnapKind::CanvasEdge));
                out.push(SnapCandidate::new(axis, hi, SnapKind::CanvasEdge));
                out.push(SnapCandidate::new(axis, mid, SnapKind::CanvasCenter));
            }
        }
    }
    if settings.to_layers {
        for b in sources.layers {
            if b.is_empty() {
                continue;
            }
            for (axis, lo, hi, mid) in [
                (Axis::X, b.min.x, b.max.x, b.center().x),
                (Axis::Y, b.min.y, b.max.y, b.center().y),
            ] {
                // Bounded by `reach`, exactly as the grid and pixel branches
                // are. A coordinate outside it can never be the nearest
                // candidate for any point inside `around`, so dropping it
                // changes no outcome — and without the filter the count grew
                // with the *document's* layer count, which meant a busy file
                // hit `MAX_CANDIDATES` on layers alone and truncated the grid
                // and pixel lines away entirely.
                let (lo_reach, hi_reach) = axis.range_of(reach);
                for (doc, kind) in [
                    (lo, SnapKind::LayerEdge),
                    (hi, SnapKind::LayerEdge),
                    (mid, SnapKind::LayerCenter),
                ] {
                    if doc >= lo_reach && doc <= hi_reach {
                        out.push(SnapCandidate::new(axis, doc, kind));
                    }
                }
            }
            if out.len() >= MAX_CANDIDATES {
                break;
            }
        }
    }
    if settings.to_grid {
        if let Some(grid) = sources.grid {
            if grid.visible {
                let step = grid
                    .minor_spacing()
                    .unwrap_or_else(|| grid.major_spacing())
                    .max(super::grid::GridSettings::MIN_SPACING);
                for (axis, lo, hi) in [
                    (Axis::X, reach.min.x, reach.max.x),
                    (Axis::Y, reach.min.y, reach.max.y),
                ] {
                    let first = (lo / step).ceil();
                    let last = (hi / step).floor();
                    let count = (last - first + 1.0).max(0.0);
                    if !count.is_finite() || count > MAX_CANDIDATES as f32 {
                        continue;
                    }
                    for i in 0..count as i64 {
                        out.push(SnapCandidate::new(
                            axis,
                            (first + i as f32) * step,
                            SnapKind::GridLine,
                        ));
                    }
                }
            }
        }
    }
    if settings.to_pixels {
        for (axis, lo, hi) in [
            (Axis::X, reach.min.x, reach.max.x),
            (Axis::Y, reach.min.y, reach.max.y),
        ] {
            let first = lo.ceil();
            let count = (hi.floor() - first + 1.0).max(0.0);
            if !count.is_finite() || count > MAX_CANDIDATES as f32 {
                continue;
            }
            for i in 0..count as i64 {
                out.push(SnapCandidate::new(
                    axis,
                    first + i as f32,
                    SnapKind::PixelBoundary,
                ));
            }
        }
    }
    out.truncate(MAX_CANDIDATES);
    out
}

/// The nearest live candidate on one axis, within the threshold.
fn best_on_axis(
    value: f32,
    axis: Axis,
    candidates: &[SnapCandidate],
    settings: &SnapSettings,
    threshold_doc: f32,
    scale_pt: f32,
) -> Option<SnapHit> {
    let mut best: Option<(f32, SnapKind, SnapCandidate)> = None;
    for c in candidates {
        if c.axis != axis || !settings.accepts(c.kind) || !c.doc.is_finite() {
            continue;
        }
        let d = (c.doc - value).abs();
        if d > threshold_doc {
            continue;
        }
        let better = match &best {
            None => true,
            // Strictly nearer wins; an exact tie falls to the earlier
            // `SnapKind`, which is what makes the outcome deterministic when a
            // guide and a layer edge sit on the same coordinate.
            Some((bd, bk, _)) => d < *bd - 1e-6 || (d <= *bd + 1e-6 && c.kind < *bk),
        };
        if better {
            best = Some((d, c.kind, *c));
        }
    }
    best.map(|(d, _, candidate)| SnapHit {
        candidate,
        distance_pt: d * scale_pt,
    })
}

/// Snap a document-space point.
///
/// Each axis is decided independently: a point may catch a vertical guide
/// without catching anything horizontal. Returns the input unchanged when
/// snapping is off, when the camera is degenerate, or when nothing is close
/// enough.
pub fn snap_point(
    point: Vec2,
    candidates: &[SnapCandidate],
    settings: &SnapSettings,
    scale_pt: f32,
) -> SnapResult {
    if !settings.enabled || !point.is_finite() || !scale_pt.is_finite() || scale_pt <= 0.0 {
        return SnapResult::unchanged(point);
    }
    let threshold_doc = settings.threshold() / scale_pt;
    let x = best_on_axis(
        point.x,
        Axis::X,
        candidates,
        settings,
        threshold_doc,
        scale_pt,
    );
    let y = best_on_axis(
        point.y,
        Axis::Y,
        candidates,
        settings,
        threshold_doc,
        scale_pt,
    );
    let mut snapped = point;
    if let Some(h) = x {
        snapped.x = h.candidate.doc;
    }
    if let Some(h) = y {
        snapped.y = h.candidate.doc;
    }
    SnapResult {
        point: snapped,
        x,
        y,
    }
}

/// Snap a whole rectangle by testing its two edges and its centre on each axis
/// and taking whichever pulls least — the behaviour a move gesture wants, where
/// any part of the dragged object may catch.
pub fn snap_rect(
    rect: DocRect,
    candidates: &[SnapCandidate],
    settings: &SnapSettings,
    scale_pt: f32,
) -> SnapResult {
    if !settings.enabled || rect.is_empty() || !scale_pt.is_finite() || scale_pt <= 0.0 {
        return SnapResult::unchanged(rect.min);
    }
    let threshold_doc = settings.threshold() / scale_pt;
    let center = rect.center();
    let mut offset = Vec2::ZERO;
    let mut hits: [Option<SnapHit>; 2] = [None, None];

    for (index, axis) in Axis::ALL.iter().enumerate() {
        let probes = match axis {
            Axis::X => [rect.min.x, center.x, rect.max.x],
            Axis::Y => [rect.min.y, center.y, rect.max.y],
        };
        let mut best: Option<(f32, SnapHit)> = None;
        for probe in probes {
            let Some(hit) =
                best_on_axis(probe, *axis, candidates, settings, threshold_doc, scale_pt)
            else {
                continue;
            };
            let delta = hit.candidate.doc - probe;
            if best
                .as_ref()
                .is_none_or(|(bd, _)| delta.abs() < bd.abs() - 1e-6)
            {
                best = Some((delta, hit));
            }
        }
        if let Some((delta, hit)) = best {
            axis.set(&mut offset, delta);
            hits[index] = Some(hit);
        }
    }

    SnapResult {
        point: rect.min + offset,
        x: hits[0],
        y: hits[1],
    }
}

/// The screen-point scale a snap threshold converts through, for callers that
/// have a camera rather than a scale to hand.
pub fn scale_for(camera: &CanvasCamera, viewport: &Viewport) -> f32 {
    camera.scale_pt(viewport)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::rulers::Guide;

    const SCALE: f32 = 2.0; // 2 screen points per document pixel

    fn only(x: f32) -> Vec<SnapCandidate> {
        vec![SnapCandidate::new(Axis::X, x, SnapKind::Guide)]
    }

    #[test]
    fn a_candidate_inside_the_threshold_catches_and_one_outside_does_not() {
        let s = SnapSettings::default(); // 8pt / 2 = 4 document pixels
        let cands = only(100.0);

        let near = snap_point(Vec2::new(103.0, 50.0), &cands, &s, SCALE);
        assert!(near.snapped());
        assert_eq!(near.point.x, 100.0);
        assert_eq!(near.point.y, 50.0, "the other axis is untouched");
        assert!((near.x.unwrap().distance_pt - 6.0).abs() < 1e-4);

        let far = snap_point(Vec2::new(105.0, 50.0), &cands, &s, SCALE);
        assert!(!far.snapped());
        assert_eq!(far.point, Vec2::new(105.0, 50.0));
    }

    /// The threshold is in screen points, so zooming in must make it *tighter*
    /// in document pixels, not looser.
    #[test]
    fn the_threshold_is_screen_sized_not_document_sized() {
        let s = SnapSettings::default();
        let cands = only(100.0);
        // At 16 points per document pixel, 8 points is half a document pixel.
        let zoomed = snap_point(Vec2::new(100.4, 0.0), &cands, &s, 16.0);
        assert!(zoomed.snapped());
        let past = snap_point(Vec2::new(100.6, 0.0), &cands, &s, 16.0);
        assert!(!past.snapped());
        // At a quarter point per document pixel it reaches 32 document pixels.
        let out = snap_point(Vec2::new(130.0, 0.0), &cands, &s, 0.25);
        assert!(out.snapped());
        let way_out = snap_point(Vec2::new(133.0, 0.0), &cands, &s, 0.25);
        assert!(!way_out.snapped());
    }

    #[test]
    fn the_nearest_candidate_wins_not_the_first_one_listed() {
        let s = SnapSettings::default();
        let cands = vec![
            SnapCandidate::new(Axis::X, 100.0, SnapKind::Guide),
            SnapCandidate::new(Axis::X, 103.0, SnapKind::Guide),
            SnapCandidate::new(Axis::X, 97.0, SnapKind::Guide),
        ];
        assert_eq!(
            snap_point(Vec2::new(102.5, 0.0), &cands, &s, SCALE).point.x,
            103.0
        );
        assert_eq!(
            snap_point(Vec2::new(99.0, 0.0), &cands, &s, SCALE).point.x,
            100.0
        );
        assert_eq!(
            snap_point(Vec2::new(97.5, 0.0), &cands, &s, SCALE).point.x,
            97.0
        );
    }

    #[test]
    fn an_exact_tie_is_broken_by_kind_so_the_outcome_is_deterministic() {
        let s = SnapSettings::default();
        let cands = vec![
            SnapCandidate::new(Axis::X, 98.0, SnapKind::LayerEdge),
            SnapCandidate::new(Axis::X, 102.0, SnapKind::LayerEdge),
        ];
        // Equidistant: the first-declared kind is the same, so the first
        // encountered wins and the answer is stable across runs.
        let a = snap_point(Vec2::new(100.0, 0.0), &cands, &s, SCALE);
        let b = snap_point(Vec2::new(100.0, 0.0), &cands, &s, SCALE);
        assert_eq!(a.point.x, b.point.x);
        assert_eq!(a.point.x, 98.0);

        // A guide and a layer edge on the same coordinate: the guide is the one
        // reported, because the user put it there on purpose.
        let mixed = vec![
            SnapCandidate::new(Axis::X, 100.0, SnapKind::LayerEdge),
            SnapCandidate::new(Axis::X, 100.0, SnapKind::Guide),
        ];
        let hit = snap_point(Vec2::new(101.0, 0.0), &mixed, &s, SCALE)
            .x
            .unwrap();
        assert_eq!(hit.candidate.kind, SnapKind::Guide);
    }

    #[test]
    fn each_axis_is_decided_on_its_own() {
        let s = SnapSettings::default();
        let cands = vec![
            SnapCandidate::new(Axis::X, 10.0, SnapKind::Guide),
            SnapCandidate::new(Axis::Y, 500.0, SnapKind::Guide),
        ];
        let r = snap_point(Vec2::new(11.0, 20.0), &cands, &s, SCALE);
        assert_eq!(r.point, Vec2::new(10.0, 20.0));
        assert!(r.x.is_some() && r.y.is_none());
    }

    #[test]
    fn the_master_toggle_and_the_per_kind_toggles_are_both_obeyed() {
        let cands = vec![
            SnapCandidate::new(Axis::X, 100.0, SnapKind::LayerEdge),
            SnapCandidate::new(Axis::X, 100.5, SnapKind::GridLine),
        ];
        let off = SnapSettings {
            enabled: false,
            ..SnapSettings::default()
        };
        assert!(!snap_point(Vec2::new(101.0, 0.0), &cands, &off, SCALE).snapped());

        let no_layers = SnapSettings {
            to_layers: false,
            ..SnapSettings::default()
        };
        let r = snap_point(Vec2::new(101.0, 0.0), &cands, &no_layers, SCALE);
        assert_eq!(r.x.unwrap().candidate.kind, SnapKind::GridLine);

        let nothing = SnapSettings {
            to_layers: false,
            to_grid: false,
            ..SnapSettings::default()
        };
        assert!(!snap_point(Vec2::new(101.0, 0.0), &cands, &nothing, SCALE).snapped());
    }

    #[test]
    fn a_zero_threshold_snaps_only_on_an_exact_hit() {
        let s = SnapSettings {
            threshold_pt: 0.0,
            ..SnapSettings::default()
        };
        let cands = only(100.0);
        assert!(snap_point(Vec2::new(100.0, 0.0), &cands, &s, SCALE).snapped());
        assert!(!snap_point(Vec2::new(100.01, 0.0), &cands, &s, SCALE).snapped());
    }

    #[test]
    fn hostile_thresholds_and_scales_cannot_break_the_snap() {
        let cands = only(100.0);
        let nan = SnapSettings {
            threshold_pt: f32::NAN,
            ..SnapSettings::default()
        };
        assert_eq!(nan.threshold(), SnapSettings::default().threshold_pt);
        let huge = SnapSettings {
            threshold_pt: 1e9,
            ..SnapSettings::default()
        };
        assert_eq!(huge.threshold(), SnapSettings::MAX_THRESHOLD_PT);

        let s = SnapSettings::default();
        for scale in [0.0_f32, -1.0, f32::NAN, f32::INFINITY] {
            let r = snap_point(Vec2::new(101.0, 0.0), &cands, &s, scale);
            assert!(!r.snapped(), "scale {scale} produced a snap");
            assert_eq!(r.point, Vec2::new(101.0, 0.0));
        }
        let bad_point = snap_point(Vec2::new(f32::NAN, 0.0), &cands, &s, SCALE);
        assert!(!bad_point.snapped());
        let bad_candidate = vec![SnapCandidate::new(Axis::X, f32::NAN, SnapKind::Guide)];
        assert!(!snap_point(Vec2::new(1.0, 0.0), &bad_candidate, &s, SCALE).snapped());
    }

    #[test]
    fn a_rectangle_snaps_by_whichever_of_its_edges_pulls_least() {
        let s = SnapSettings::default(); // 4 document pixels at SCALE
        let rect = DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(50.0, 30.0));
        // A candidate 1px from the right edge and 9px from the left one.
        let cands = vec![SnapCandidate::new(Axis::X, 51.0, SnapKind::LayerEdge)];
        let r = snap_rect(rect, &cands, &s, SCALE);
        assert_eq!(r.point.x, 11.0, "the whole rect moved by +1, not to x=51");
        assert_eq!(r.point.y, 10.0);
        assert_eq!(r.x.unwrap().candidate.doc, 51.0);
    }

    #[test]
    fn a_rectangle_can_snap_by_its_centre() {
        let s = SnapSettings::default();
        let rect = DocRect::new(Vec2::new(0.0, 0.0), Vec2::new(40.0, 40.0));
        // 21 is 1 away from the centre and 19/21 away from the edges.
        let cands = vec![SnapCandidate::new(Axis::X, 21.0, SnapKind::CanvasCenter)];
        let r = snap_rect(rect, &cands, &s, SCALE);
        assert_eq!(r.point.x, 1.0);
        assert!(r.x.unwrap().candidate.kind.shows_smart_guide());
    }

    #[test]
    fn nothing_within_reach_leaves_a_rectangle_where_it_was() {
        let s = SnapSettings::default();
        let rect = DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(50.0, 30.0));
        let cands = vec![SnapCandidate::new(Axis::X, 500.0, SnapKind::LayerEdge)];
        let r = snap_rect(rect, &cands, &s, SCALE);
        assert_eq!(r.point, rect.min);
        assert!(!r.snapped());
        assert!(snap_rect(DocRect::ZERO, &cands, &s, SCALE).point == Vec2::ZERO);
    }

    #[test]
    fn candidates_come_from_guides_canvas_layers_and_the_grid() {
        let mut guides = Guides::new();
        guides.add(Guide::new(Axis::X, 7.0)).unwrap();
        let grid = GridSettings {
            visible: true,
            spacing_doc: 10.0,
            subdivisions: 1,
            ..GridSettings::default()
        };
        let layers = [DocRect::new(Vec2::new(2.0, 3.0), Vec2::new(12.0, 23.0))];
        let sources = SnapSources {
            guides: Some(&guides),
            grid: Some(&grid),
            canvas: Some(DocRect::of_canvas(Vec2::new(100.0, 80.0))),
            layers: &layers,
        };
        let s = SnapSettings::default();
        let cands = collect_candidates(
            &sources,
            &s,
            DocRect::new(Vec2::new(0.0, 0.0), Vec2::new(30.0, 30.0)),
            4.0,
        );

        let has = |kind: SnapKind, axis: Axis, doc: f32| {
            cands
                .iter()
                .any(|c| c.kind == kind && c.axis == axis && (c.doc - doc).abs() < 1e-4)
        };
        assert!(has(SnapKind::Guide, Axis::X, 7.0));
        assert!(has(SnapKind::CanvasEdge, Axis::X, 0.0));
        assert!(has(SnapKind::CanvasEdge, Axis::X, 100.0));
        assert!(has(SnapKind::CanvasCenter, Axis::Y, 40.0));
        assert!(has(SnapKind::LayerEdge, Axis::X, 2.0));
        assert!(has(SnapKind::LayerEdge, Axis::X, 12.0));
        assert!(has(SnapKind::LayerCenter, Axis::X, 7.0));
        assert!(has(SnapKind::GridLine, Axis::X, 20.0));
        assert!(
            !cands.iter().any(|c| c.kind == SnapKind::PixelBoundary),
            "pixel snapping is off by default"
        );
        assert!(cands.len() <= MAX_CANDIDATES);
    }

    #[test]
    fn hidden_guides_and_hidden_grids_contribute_nothing() {
        let mut guides = Guides::new();
        guides.add(Guide::new(Axis::X, 7.0)).unwrap();
        guides.visible = false;
        let grid = GridSettings {
            visible: false,
            ..GridSettings::default()
        };
        let sources = SnapSources {
            guides: Some(&guides),
            grid: Some(&grid),
            canvas: None,
            layers: &[],
        };
        let cands = collect_candidates(
            &sources,
            &SnapSettings::default(),
            DocRect::of_canvas(Vec2::splat(10.0)),
            4.0,
        );
        assert!(cands.is_empty());
    }

    #[test]
    fn collecting_is_bounded_however_many_layers_there_are() {
        let layers: Vec<DocRect> = (0..5000)
            .map(|i| DocRect::of_canvas(Vec2::splat(i as f32 + 1.0)))
            .collect();
        let sources = SnapSources {
            layers: &layers,
            ..SnapSources::default()
        };
        let cands = collect_candidates(
            &sources,
            &SnapSettings::default(),
            DocRect::of_canvas(Vec2::splat(100.0)),
            4.0,
        );
        assert_eq!(cands.len(), MAX_CANDIDATES);
    }

    /// The cap must bite on *distance*, not on the order the layers happen to
    /// be in. A document with five thousand layers far from the pointer used to
    /// fill the whole budget with coordinates that could never catch, throwing
    /// away both the one near edge and every grid line behind it.
    #[test]
    fn a_near_layer_edge_and_the_grid_survive_a_document_full_of_far_layers() {
        let mut layers: Vec<DocRect> = (0..5000)
            .map(|i| {
                let x = 10_000.0 + i as f32 * 10.0;
                DocRect::new(Vec2::new(x, x), Vec2::new(x + 5.0, x + 5.0))
            })
            .collect();
        // …and one whose left edge is two document pixels from the gesture.
        layers.push(DocRect::new(
            Vec2::new(102.0, 102.0),
            Vec2::new(160.0, 160.0),
        ));
        let grid = GridSettings {
            visible: true,
            spacing_doc: 50.0,
            subdivisions: 1,
            ..GridSettings::default()
        };
        let sources = SnapSources {
            grid: Some(&grid),
            layers: &layers,
            ..SnapSources::default()
        };
        let around = DocRect::from_corners(Vec2::splat(100.0), Vec2::splat(100.0));
        let cands = collect_candidates(&sources, &SnapSettings::default(), around, 4.0);

        assert!(
            cands.len() < MAX_CANDIDATES,
            "the collection is still sized by the document rather than by the gesture: {}",
            cands.len()
        );
        assert!(
            cands.iter().any(|c| c.kind == SnapKind::LayerEdge
                && c.axis == Axis::X
                && (c.doc - 102.0).abs() < 1e-3),
            "the one reachable layer edge was crowded out by unreachable ones"
        );
        assert!(
            cands.iter().any(|c| c.kind == SnapKind::GridLine
                && c.axis == Axis::X
                && (c.doc - 100.0).abs() < 1e-3),
            "the grid was truncated away behind the layer list"
        );
        // Nothing unreachable came along for the ride.
        for c in &cands {
            if c.kind == SnapKind::LayerEdge || c.kind == SnapKind::LayerCenter {
                assert!(c.doc >= 96.0 && c.doc <= 104.0, "{c:?} is out of reach");
            }
        }
    }

    /// Painting tools are never snapped: a brush dab pulled onto a grid line
    /// is a broken brush, not a helpful one.
    #[test]
    fn only_the_tools_that_place_things_are_snapped() {
        use tools::ToolId as T;
        for tool in [
            T::Move,
            T::RectMarquee,
            T::Crop,
            T::Rectangle,
            T::FreeTransform,
        ] {
            assert!(tool_snaps(tool), "{tool:?} positions things and must snap");
        }
        for tool in [
            T::Brush,
            T::Pencil,
            T::Eraser,
            T::Smudge,
            T::Eyedropper,
            T::Lasso,
            T::Hand,
            T::Zoom,
        ] {
            assert!(!tool_snaps(tool), "{tool:?} must not be snapped");
        }
    }

    #[test]
    fn a_disabled_snap_collects_nothing_at_all() {
        let sources = SnapSources {
            canvas: Some(DocRect::of_canvas(Vec2::splat(10.0))),
            ..SnapSources::default()
        };
        let off = SnapSettings {
            enabled: false,
            ..SnapSettings::default()
        };
        assert!(collect_candidates(&sources, &off, DocRect::ZERO, 1.0).is_empty());
    }

    #[test]
    fn pixel_snapping_lands_on_whole_pixels_when_switched_on() {
        let s = SnapSettings {
            to_pixels: true,
            to_canvas: false,
            to_grid: false,
            to_layers: false,
            to_guides: false,
            ..SnapSettings::default()
        };
        let sources = SnapSources::default();
        let cands = collect_candidates(
            &sources,
            &s,
            DocRect::new(Vec2::new(9.0, 9.0), Vec2::new(11.0, 11.0)),
            1.0,
        );
        let r = snap_point(Vec2::new(10.3, 10.9), &cands, &s, 16.0);
        assert_eq!(r.point, Vec2::new(10.0, 11.0));
    }

    #[test]
    fn smart_guides_are_reported_only_for_the_kinds_that_need_one() {
        assert!(SnapKind::LayerEdge.shows_smart_guide());
        assert!(SnapKind::LayerCenter.shows_smart_guide());
        assert!(SnapKind::CanvasCenter.shows_smart_guide());
        assert!(!SnapKind::GridLine.shows_smart_guide());
        assert!(!SnapKind::Guide.shows_smart_guide());
        assert!(!SnapKind::PixelBoundary.shows_smart_guide());

        let s = SnapSettings::default();
        let cands = vec![
            SnapCandidate::new(Axis::X, 100.0, SnapKind::LayerEdge),
            SnapCandidate::new(Axis::Y, 200.0, SnapKind::GridLine),
        ];
        let r = snap_point(Vec2::new(101.0, 201.0), &cands, &s, SCALE);
        let drawn: Vec<SnapKind> = r.smart_guides().map(|h| h.candidate.kind).collect();
        assert_eq!(drawn, vec![SnapKind::LayerEdge]);
    }
}
