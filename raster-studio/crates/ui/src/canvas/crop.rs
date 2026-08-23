//! The crop overlay: the kept rectangle, the darkened surround, and the
//! composition guides drawn inside it.
//!
//! The surround is drawn as four rectangles rather than as one shape with a
//! hole, because egui has no even-odd fill: a hole would need a self-
//! intersecting path, and the four-band decomposition is exact, cheap, and
//! degenerates cleanly when the crop touches an edge.

use glam::Vec2;

use super::camera::CanvasCamera;
use super::cursor::CanvasCursor;
use super::geom::DocRect;
use super::handles;
use super::viewport::Viewport;

/// Which composition guide to draw inside the crop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CropGuide {
    None,
    /// Two lines each way at the thirds — the default.
    #[default]
    Thirds,
    /// The golden-section lines: two each way, at `φ⁻²` and `1 − φ⁻²` of the
    /// crop, which is the same shape as [`CropGuide::Thirds`] a little further
    /// out. No diagonals — [`CropGuide::Diagonals`] is those.
    GoldenRatio,
    /// Both diagonals only.
    Diagonals,
    /// A dense grid, for straightening.
    Grid,
}

impl CropGuide {
    pub const ALL: &'static [CropGuide] = &[
        CropGuide::None,
        CropGuide::Thirds,
        CropGuide::GoldenRatio,
        CropGuide::Diagonals,
        CropGuide::Grid,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            CropGuide::None => "None",
            CropGuide::Thirds => "Rule of Thirds",
            CropGuide::GoldenRatio => "Golden Ratio",
            CropGuide::Diagonals => "Diagonal",
            CropGuide::Grid => "Grid",
        }
    }
}

/// The inverse of the golden ratio: where the golden-section lines fall.
const GOLDEN: f32 = 0.381_966_02;

/// How many divisions the dense grid uses per axis.
const GRID_DIVISIONS: u32 = 8;

/// Everything the painter needs for one frame of the crop overlay, in screen
/// points.
#[derive(Debug, Clone, PartialEq)]
pub struct CropOverlay {
    /// The kept region.
    pub keep: egui::Rect,
    /// The four bands outside it, to be filled with the scrim. Empty bands are
    /// omitted, so a crop flush against an edge produces three, not four.
    pub scrim: Vec<egui::Rect>,
    /// The composition guide lines.
    pub guides: Vec<[Vec2; 2]>,
    /// The corner and edge grips.
    pub grips: Vec<egui::Rect>,
}

impl Default for CropOverlay {
    fn default() -> Self {
        Self {
            // `Rect::NOTHING` is the empty rectangle; `Rect` has no `Default`.
            keep: egui::Rect::NOTHING,
            scrim: Vec::new(),
            guides: Vec::new(),
            grips: Vec::new(),
        }
    }
}

impl CropOverlay {
    pub fn is_empty(&self) -> bool {
        !(self.keep.width() > 0.0 && self.keep.height() > 0.0)
    }
}

/// Build the overlay for `crop_doc` inside `viewport`.
///
/// The scrim covers the whole content area minus the crop, so the parts of the
/// image that would be thrown away recede. `grip_pt` is the edge length of the
/// corner and edge grips.
///
/// Only meaningful while the view is axis-aligned, which is the only state the
/// crop tool runs in: a rotated view would need the crop drawn as a quad, and
/// the straighten gesture rotates the *crop*, not the view.
pub fn build(
    crop_doc: DocRect,
    camera: &CanvasCamera,
    viewport: &Viewport,
    guide: CropGuide,
    grip_pt: f32,
) -> CropOverlay {
    if viewport.is_degenerate() || crop_doc.is_empty() {
        return CropOverlay::default();
    }
    let a = camera.screen_pt_of(viewport, crop_doc.min);
    let b = camera.screen_pt_of(viewport, crop_doc.max);
    if !a.is_finite() || !b.is_finite() {
        return CropOverlay::default();
    }
    let keep_bounds = DocRect::from_corners(a, b);
    let keep = super::geom::to_egui_rect(keep_bounds.min, keep_bounds.max);
    let outer = viewport.content_bounds_pt();

    CropOverlay {
        keep,
        scrim: scrim_bands(outer, keep_bounds),
        guides: guide_lines(keep_bounds, guide),
        grips: grips(keep_bounds, grip_pt.clamp(MIN_GRIP_PT, MAX_GRIP_PT)),
    }
}

/// The legal range for a grip's edge length, in screen points: half a grid unit
/// up to sixteen of them, so no setting can hide the grips or let one swallow
/// the crop it belongs to.
pub const MIN_GRIP_PT: f32 = design::Space::Hair.units() * design::UNIT_PT;
pub const MAX_GRIP_PT: f32 = design::UNIT_PT * 16.0;

/// The four bands of `outer` that `inner` does not cover.
///
/// Shared with [`crate::canvas::paint::backdrop`], which paints the surround
/// around the *image* with the same decomposition and for the same reason:
/// egui has no even-odd fill, so a hole is four rectangles.
pub(crate) fn scrim_bands(outer: DocRect, inner: DocRect) -> Vec<egui::Rect> {
    let clipped = inner.intersect(&outer);
    if clipped.is_empty() {
        // The crop is entirely off screen: everything visible is thrown away.
        return vec![super::geom::to_egui_rect(outer.min, outer.max)];
    }
    let bands = [
        // Top, bottom, left, right — the left and right bands stop at the
        // crop's own top and bottom so no pixel is darkened twice.
        DocRect::new(outer.min, Vec2::new(outer.max.x, clipped.min.y)),
        DocRect::new(Vec2::new(outer.min.x, clipped.max.y), outer.max),
        DocRect::new(
            Vec2::new(outer.min.x, clipped.min.y),
            Vec2::new(clipped.min.x, clipped.max.y),
        ),
        DocRect::new(
            Vec2::new(clipped.max.x, clipped.min.y),
            Vec2::new(outer.max.x, clipped.max.y),
        ),
    ];
    bands
        .into_iter()
        .filter(|b| !b.is_empty())
        .map(|b| super::geom::to_egui_rect(b.min, b.max))
        .collect()
}

/// The composition guide lines inside `keep`.
fn guide_lines(keep: DocRect, guide: CropGuide) -> Vec<[Vec2; 2]> {
    let (w, h) = (keep.width(), keep.height());
    let mut out = Vec::new();
    let vertical = |t: f32, out: &mut Vec<[Vec2; 2]>| {
        let x = keep.min.x + w * t;
        out.push([Vec2::new(x, keep.min.y), Vec2::new(x, keep.max.y)]);
    };
    let horizontal = |t: f32, out: &mut Vec<[Vec2; 2]>| {
        let y = keep.min.y + h * t;
        out.push([Vec2::new(keep.min.x, y), Vec2::new(keep.max.x, y)]);
    };
    match guide {
        CropGuide::None => {}
        CropGuide::Thirds => {
            for t in [1.0 / 3.0, 2.0 / 3.0] {
                vertical(t, &mut out);
                horizontal(t, &mut out);
            }
        }
        CropGuide::GoldenRatio => {
            for t in [GOLDEN, 1.0 - GOLDEN] {
                vertical(t, &mut out);
                horizontal(t, &mut out);
            }
        }
        CropGuide::Diagonals => {
            out.push([keep.min, keep.max]);
            out.push([
                Vec2::new(keep.max.x, keep.min.y),
                Vec2::new(keep.min.x, keep.max.y),
            ]);
        }
        CropGuide::Grid => {
            for i in 1..GRID_DIVISIONS {
                let t = i as f32 / GRID_DIVISIONS as f32;
                vertical(t, &mut out);
                horizontal(t, &mut out);
            }
        }
    }
    out
}

/// The eight grips: four corners and four edge midpoints.
fn grips(keep: DocRect, size_pt: f32) -> Vec<egui::Rect> {
    let c = keep.center();
    let points = [
        keep.min,
        Vec2::new(c.x, keep.min.y),
        Vec2::new(keep.max.x, keep.min.y),
        Vec2::new(keep.max.x, c.y),
        keep.max,
        Vec2::new(c.x, keep.max.y),
        Vec2::new(keep.min.x, keep.max.y),
        Vec2::new(keep.min.x, c.y),
    ];
    points
        .into_iter()
        .map(|p| {
            egui::Rect::from_center_size(super::geom::to_pos2(p), egui::vec2(size_pt, size_pt))
        })
        .collect()
}

/// Which part of a crop rectangle the pointer is on.
///
/// Corners are numbered clockwise from the top-left and edges clockwise from
/// the top, matching [`tools::transform::Handle`] so the two hit tests read the
/// same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CropGrip {
    Corner(usize),
    Edge(usize),
    /// Inside the kept rectangle: drag the whole crop.
    Interior,
}

/// The grips in the order [`CropOverlay::grips`] holds them, so a hit answers
/// with the same rectangle the painter drew.
pub const GRIP_ORDER: [CropGrip; 8] = [
    CropGrip::Corner(0),
    CropGrip::Edge(0),
    CropGrip::Corner(1),
    CropGrip::Edge(1),
    CropGrip::Corner(2),
    CropGrip::Edge(2),
    CropGrip::Corner(3),
    CropGrip::Edge(3),
];

/// Tie-break order when two grips are exactly equidistant. Lower wins, and a
/// corner beats the edge beside it — the same rule the transform box uses.
fn grip_rank(grip: CropGrip) -> u8 {
    match grip {
        CropGrip::Corner(_) => 0,
        CropGrip::Edge(_) => 1,
        CropGrip::Interior => 2,
    }
}

/// Where in the drawn overlay a grip lives, so a caller can highlight it.
pub fn grip_index(grip: CropGrip) -> Option<usize> {
    GRIP_ORDER.iter().position(|g| *g == grip)
}

/// What is under the pointer, in screen points.
///
/// Nearest grip centre within `grab_pt` wins, so the eight grips stay reachable
/// on a crop small enough that their targets overlap — the same nearest-centre
/// rule as [`crate::canvas::handles::hit_test`], and for the same reason. Only
/// once no grip is within reach does the interior answer.
pub fn hit_test(pos_pt: Vec2, overlay: &CropOverlay, grab_pt: f32) -> Option<CropGrip> {
    if !pos_pt.is_finite() || overlay.is_empty() {
        return None;
    }
    let grab = if grab_pt.is_finite() {
        grab_pt.clamp(MIN_GRIP_PT, MAX_GRIP_PT)
    } else {
        MIN_GRIP_PT
    };
    let mut best: Option<(f32, u8, CropGrip)> = None;
    for (rect, grip) in overlay.grips.iter().zip(GRIP_ORDER) {
        let centre = super::geom::from_pos2(rect.center());
        let d = (pos_pt - centre).length();
        if d > grab {
            continue;
        }
        let rank = grip_rank(grip);
        let better = match &best {
            None => true,
            Some((bd, br, _)) => d < *bd - 1e-4 || (d <= *bd + 1e-4 && rank < *br),
        };
        if better {
            best = Some((d, rank, grip));
        }
    }
    if let Some((_, _, grip)) = best {
        return Some(grip);
    }
    overlay
        .keep
        .contains(super::geom::to_pos2(pos_pt))
        .then_some(CropGrip::Interior)
}

/// The cursor for a region of the crop rectangle.
///
/// Direction-aware, like the transform box: the arrow points along the way the
/// grip actually travels on screen, which is worked out from where it sits
/// relative to the crop's own centre rather than from its index.
pub fn cursor_for(grip: CropGrip, overlay: &CropOverlay) -> CanvasCursor {
    match grip {
        CropGrip::Interior => CanvasCursor::Move,
        other => match grip_index(other).and_then(|i| overlay.grips.get(i)) {
            Some(rect) => handles::resize_cursor(
                super::geom::from_pos2(rect.center())
                    - super::geom::from_pos2(overlay.keep.center()),
            ),
            None => CanvasCursor::Move,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam() -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(200.0, 200.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    fn crop() -> DocRect {
        DocRect::new(Vec2::new(150.0, 150.0), Vec2::new(250.0, 230.0))
    }

    #[test]
    fn the_kept_rectangle_is_where_the_camera_puts_it() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::Thirds, 8.0);
        let (min, max) = super::super::geom::from_egui_rect(o.keep);
        assert!((min - c.screen_pt_of(&v, crop().min)).length() < 1e-3);
        assert!((max - c.screen_pt_of(&v, crop().max)).length() < 1e-3);
        assert!(!o.is_empty());
    }

    #[test]
    fn the_scrim_covers_everything_outside_the_crop_and_nothing_inside_it() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::None, 8.0);
        assert_eq!(o.scrim.len(), 4);
        let keep = o.keep;
        // Nothing overlaps the kept region.
        for band in &o.scrim {
            let overlap = band.intersect(keep);
            assert!(
                overlap.width() <= 1e-3 || overlap.height() <= 1e-3,
                "a scrim band {band:?} covers the crop"
            );
        }
        // …and no two bands overlap each other, so the darkening is uniform.
        for (i, a) in o.scrim.iter().enumerate() {
            for b in &o.scrim[i + 1..] {
                let overlap = a.intersect(*b);
                assert!(overlap.width() <= 1e-3 || overlap.height() <= 1e-3);
            }
        }
        // Together with the crop they tile the whole content area.
        let area: f32 = o.scrim.iter().map(|r| r.width() * r.height()).sum();
        let total = v.size_pt().x * v.size_pt().y;
        assert!(
            (area + keep.width() * keep.height() - total).abs() < 1.0,
            "the scrim and the crop do not tile the viewport"
        );
    }

    #[test]
    fn a_crop_flush_against_an_edge_drops_the_empty_band() {
        let v = vp();
        let c = cam();
        // A crop whose left edge is off the left of the content area.
        let flush = DocRect::new(Vec2::new(-500.0, 180.0), Vec2::new(220.0, 220.0));
        let o = build(flush, &c, &v, CropGuide::None, 8.0);
        assert_eq!(o.scrim.len(), 3, "{:?}", o.scrim);
        for band in &o.scrim {
            assert!(band.width() > 0.0 && band.height() > 0.0);
        }
    }

    #[test]
    fn a_crop_entirely_off_screen_darkens_everything() {
        let v = vp();
        let c = cam();
        let away = DocRect::new(Vec2::new(9_000.0, 9_000.0), Vec2::new(9_100.0, 9_100.0));
        let o = build(away, &c, &v, CropGuide::None, 8.0);
        assert_eq!(o.scrim.len(), 1);
        assert_eq!(o.scrim[0], v.content_rect());
    }

    #[test]
    fn the_thirds_land_on_the_thirds() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::Thirds, 8.0);
        assert_eq!(o.guides.len(), 4);
        let keep = o.keep;
        let xs: Vec<f32> = o
            .guides
            .iter()
            .filter(|[a, b]| (a.x - b.x).abs() < 1e-3)
            .map(|[a, _]| a.x)
            .collect();
        assert_eq!(xs.len(), 2);
        assert!((xs[0] - (keep.min.x + keep.width() / 3.0)).abs() < 1e-2);
        assert!((xs[1] - (keep.min.x + keep.width() * 2.0 / 3.0)).abs() < 1e-2);
        // Every line spans the crop exactly.
        for [a, b] in &o.guides {
            assert!(keep.expand(0.01).contains(super::super::geom::to_pos2(*a)));
            assert!(keep.expand(0.01).contains(super::super::geom::to_pos2(*b)));
        }
    }

    /// Each variant's *documented* lines, asserted by count **and** position,
    /// so the doc comment on [`CropGuide`] and the arithmetic in
    /// [`guide_lines`] cannot drift apart again. The Golden Ratio variant was
    /// described as thirds plus diagonals and draws neither.
    #[test]
    fn every_guide_style_produces_the_lines_it_promises() {
        let v = vp();
        let c = cam();
        let thirds = vec![1.0 / 3.0, 2.0 / 3.0];
        let golden = vec![GOLDEN, 1.0 - GOLDEN];
        let grid: Vec<f32> = (1..GRID_DIVISIONS).map(|i| i as f32 / 8.0).collect();

        for style in CropGuide::ALL {
            let o = build(crop(), &c, &v, *style, 8.0);
            let keep = o.keep;
            let (want_fractions, want_diagonals): (&[f32], usize) = match style {
                CropGuide::None => (&[], 0),
                CropGuide::Thirds => (&thirds, 0),
                CropGuide::GoldenRatio => (&golden, 0),
                CropGuide::Diagonals => (&[], 2),
                CropGuide::Grid => (&grid, 0),
            };
            assert_eq!(
                o.guides.len(),
                want_fractions.len() * 2 + want_diagonals,
                "{style:?} drew the wrong number of lines"
            );

            let mut verticals: Vec<f32> = Vec::new();
            let mut horizontals: Vec<f32> = Vec::new();
            let mut diagonals = 0usize;
            for [a, b] in &o.guides {
                if (a.x - b.x).abs() < 1e-3 {
                    verticals.push((a.x - keep.min.x) / keep.width());
                } else if (a.y - b.y).abs() < 1e-3 {
                    horizontals.push((a.y - keep.min.y) / keep.height());
                } else {
                    diagonals += 1;
                    // A diagonal joins two opposite corners of the crop.
                    let corner = |p: &Vec2| {
                        (p.x - keep.min.x).abs().min((p.x - keep.max.x).abs()) < 1e-2
                            && (p.y - keep.min.y).abs().min((p.y - keep.max.y).abs()) < 1e-2
                    };
                    assert!(corner(a) && corner(b), "{style:?}: {a:?}..{b:?}");
                }
            }
            assert_eq!(diagonals, want_diagonals, "{style:?}");
            verticals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            horizontals.sort_by(|a, b| a.partial_cmp(b).unwrap());
            for (got, want) in verticals.iter().zip(want_fractions) {
                assert!((got - want).abs() < 1e-3, "{style:?}: {got} vs {want}");
            }
            for (got, want) in horizontals.iter().zip(want_fractions) {
                assert!((got - want).abs() < 1e-3, "{style:?}: {got} vs {want}");
            }
            assert_eq!(verticals.len(), want_fractions.len(), "{style:?}");
            assert_eq!(horizontals.len(), want_fractions.len(), "{style:?}");
            assert!(!style.name().is_empty());
        }
    }

    #[test]
    fn the_golden_lines_are_not_the_thirds() {
        let keep = DocRect::new(Vec2::ZERO, Vec2::new(300.0, 300.0));
        let thirds = guide_lines(keep, CropGuide::Thirds);
        let golden = guide_lines(keep, CropGuide::GoldenRatio);
        assert_ne!(thirds, golden);
        // The first golden line sits at 0.382 of the width.
        assert!((golden[0][0].x - 300.0 * GOLDEN).abs() < 1e-2);
    }

    #[test]
    fn the_diagonals_run_corner_to_corner() {
        let keep = DocRect::new(Vec2::new(10.0, 20.0), Vec2::new(50.0, 60.0));
        let d = guide_lines(keep, CropGuide::Diagonals);
        assert_eq!(d[0], [keep.min, keep.max]);
        assert_eq!(d[1], [Vec2::new(50.0, 20.0), Vec2::new(10.0, 60.0)]);
    }

    #[test]
    fn there_are_eight_grips_at_the_corners_and_edge_midpoints() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::None, 10.0);
        assert_eq!(o.grips.len(), 8);
        let keep = o.keep;
        let centres: Vec<egui::Pos2> = o.grips.iter().map(|r| r.center()).collect();
        for corner in [
            keep.left_top(),
            keep.right_top(),
            keep.right_bottom(),
            keep.left_bottom(),
        ] {
            assert!(
                centres.iter().any(|p| (*p - corner).length() < 1e-3),
                "no grip at {corner:?}"
            );
        }
        for grip in &o.grips {
            assert!((grip.width() - 10.0).abs() < 1e-4);
        }
    }

    /// Every grip that is drawn can be grabbed by clicking it, and each one
    /// shows the cursor for the direction it actually travels.
    #[test]
    fn every_drawn_grip_is_grabbed_by_clicking_it_and_names_its_cursor() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::None, 10.0);
        assert_eq!(o.grips.len(), GRIP_ORDER.len());
        let want = [
            (CropGrip::Corner(0), CanvasCursor::ResizeNwSe),
            (CropGrip::Edge(0), CanvasCursor::ResizeVertical),
            (CropGrip::Corner(1), CanvasCursor::ResizeNeSw),
            (CropGrip::Edge(1), CanvasCursor::ResizeHorizontal),
            (CropGrip::Corner(2), CanvasCursor::ResizeNwSe),
            (CropGrip::Edge(2), CanvasCursor::ResizeVertical),
            (CropGrip::Corner(3), CanvasCursor::ResizeNeSw),
            (CropGrip::Edge(3), CanvasCursor::ResizeHorizontal),
        ];
        for (index, (grip, cursor)) in want.iter().enumerate() {
            assert_eq!(GRIP_ORDER[index], *grip);
            assert_eq!(grip_index(*grip), Some(index));
            let at = super::super::geom::from_pos2(o.grips[index].center());
            assert_eq!(
                hit_test(at, &o, 10.0),
                Some(*grip),
                "clicking the grip drawn at {at:?} did not grab {grip:?}"
            );
            assert_eq!(cursor_for(*grip, &o), *cursor, "{grip:?}");
        }
        assert_eq!(cursor_for(CropGrip::Interior, &o), CanvasCursor::Move);
    }

    #[test]
    fn the_interior_moves_the_crop_and_the_outside_grabs_nothing() {
        let v = vp();
        let c = cam();
        let o = build(crop(), &c, &v, CropGuide::None, 10.0);
        let middle = super::super::geom::from_pos2(o.keep.center());
        assert_eq!(hit_test(middle, &o, 10.0), Some(CropGrip::Interior));
        let far = super::super::geom::from_pos2(o.keep.center()) + Vec2::splat(500.0);
        assert_eq!(hit_test(far, &o, 10.0), None);
        assert_eq!(hit_test(Vec2::new(f32::NAN, 0.0), &o, 10.0), None);
        assert_eq!(
            hit_test(middle, &CropOverlay::default(), 10.0),
            None,
            "an empty crop has nothing to grab"
        );
    }

    /// The overlap case, as on the transform box: on a crop small enough that
    /// every grip target covers every other, the nearest one still wins and a
    /// dead tie goes to the corner.
    #[test]
    fn overlapping_grips_resolve_to_the_nearest_and_ties_go_to_the_corner() {
        let v = vp();
        let c = cam();
        // Ten document pixels is ten screen points at zoom 2 on a 2x display.
        let tiny = DocRect::new(Vec2::new(200.0, 200.0), Vec2::new(210.0, 210.0));
        let o = build(tiny, &c, &v, CropGuide::None, 10.0);
        let at =
            |g: CropGrip| super::super::geom::from_pos2(o.grips[grip_index(g).unwrap()].center());
        // Sanity: the targets really do overlap, so this is the hard case.
        assert!((at(CropGrip::Corner(0)) - at(CropGrip::Edge(0))).length() < 10.0);
        for grip in GRIP_ORDER {
            let probe = at(grip) + Vec2::new(0.2, 0.0);
            assert_eq!(hit_test(probe, &o, 10.0), Some(grip), "{grip:?}");
        }
        let midway = (at(CropGrip::Corner(0)) + at(CropGrip::Edge(0))) * 0.5;
        assert_eq!(hit_test(midway, &o, 10.0), Some(CropGrip::Corner(0)));
    }

    #[test]
    fn grip_size_is_clamped_so_a_bad_setting_cannot_hide_them() {
        let v = vp();
        let c = cam();
        for size in [0.0_f32, -4.0, 1e6] {
            let o = build(crop(), &c, &v, CropGuide::None, size);
            for grip in &o.grips {
                assert!(grip.width() >= 2.0 && grip.width() <= 64.0, "{size}");
            }
        }
    }

    #[test]
    fn an_empty_crop_or_a_collapsed_viewport_draws_nothing() {
        let v = vp();
        let c = cam();
        assert!(build(DocRect::ZERO, &c, &v, CropGuide::Thirds, 8.0).is_empty());
        let collapsed = Viewport::new(Vec2::splat(50.0), PanelInsets::uniform(50.0), 1.0);
        let o = build(crop(), &c, &collapsed, CropGuide::Thirds, 8.0);
        assert!(o.is_empty() && o.scrim.is_empty() && o.guides.is_empty());
    }

    #[test]
    fn a_degenerate_camera_draws_nothing_rather_than_a_nan_rectangle() {
        let v = vp();
        let dead = CanvasCamera {
            zoom: f32::INFINITY,
            ..cam()
        };
        let o = build(crop(), &dead, &v, CropGuide::Thirds, 8.0);
        assert!(o.is_empty());
    }
}
