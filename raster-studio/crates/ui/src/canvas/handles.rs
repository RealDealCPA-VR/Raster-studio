//! Transform handles: where they are drawn, what grabbing each one does, and
//! which cursor the pointer takes over each region.
//!
//! # Why the hit test is not `TransformState::hit_test`
//!
//! `tools` hit-tests handles in **document pixels** with a fixed radius. That
//! is right for the tool, which only ever sees document coordinates, but wrong
//! for the pointer: at 10% zoom a six-document-pixel handle is under a point
//! across and unhittable, and at 3200% it swallows the whole quad. A grab
//! target has to be a constant size *on screen*, so this module projects the
//! handles first and hit-tests in screen points.
//!
//! # Overlapping handles
//!
//! Once the quad is small — a thin selection, or a normal one zoomed out — the
//! grab discs of the corners, the edge midpoints and the pivot all overlap.
//! Rejecting the ambiguity would leave the user unable to grab anything, so the
//! rule is **nearest centre wins**, with [`handle_rank`] breaking exact ties in
//! the order a user expects: corners first, then warp mesh points, then edges,
//! then the pivot. That keeps every handle reachable no matter how small the
//! box gets, and makes the outcome deterministic rather than dependent on the
//! order the handles happened to be generated in.

use design::Space;
use glam::Vec2;
use tools::transform::{Handle, TransformMode, TransformState};

use super::camera::CanvasCamera;
use super::cursor::CanvasCursor;
use super::geom::DocRect;
use super::viewport::Viewport;

/// Sizes of the handle furniture, in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HandleLayout {
    /// Edge length of the square drawn for a corner or edge handle.
    pub handle_pt: f32,
    /// Radius of the pointer target around a handle's centre. Larger than the
    /// drawn square, because a four-point square is not a four-point target.
    pub grab_pt: f32,
    /// How far outside a corner the rotate band reaches.
    pub rotate_band_pt: f32,
    /// Radius of the pivot marker.
    pub pivot_pt: f32,
}

impl Default for HandleLayout {
    fn default() -> Self {
        // Every size is a rung of the design crate's 4pt grid: a handle is two
        // units square, its grab target three, the rotate band six, and the
        // pivot marker the one-and-a-half-unit step between the first two.
        Self {
            handle_pt: Space::Small.pt(),
            grab_pt: Space::Medium.pt(),
            rotate_band_pt: Space::XLarge.pt(),
            pivot_pt: design::tokens::grid(1.5),
        }
    }
}

impl HandleLayout {
    /// The legal range for the grab radius, in screen points: half a grid unit
    /// up to sixteen of them. Spelled through the spacing scale rather than as
    /// bare numbers, like every other measurement in this module tree.
    pub const MIN_GRAB_PT: f32 = Space::Hair.units() * design::UNIT_PT;
    pub const MAX_GRAB_PT: f32 = design::UNIT_PT * 16.0;
    /// The legal range for the rotate band. Zero is allowed — it turns rotation
    /// by dragging outside a corner off — and the ceiling is thirty-two units.
    pub const MAX_ROTATE_BAND_PT: f32 = design::UNIT_PT * 32.0;

    /// The grab radius, clamped so a preferences file cannot make every handle
    /// unhittable or make one handle swallow the window.
    pub fn grab(&self) -> f32 {
        if self.grab_pt.is_finite() {
            self.grab_pt.clamp(Self::MIN_GRAB_PT, Self::MAX_GRAB_PT)
        } else {
            Self::default().grab_pt
        }
    }

    /// The rotate band, clamped the same way.
    pub fn rotate_band(&self) -> f32 {
        if self.rotate_band_pt.is_finite() {
            self.rotate_band_pt.clamp(0.0, Self::MAX_ROTATE_BAND_PT)
        } else {
            Self::default().rotate_band_pt
        }
    }
}

/// A handle, projected.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScreenHandle {
    pub handle: Handle,
    /// Centre, in screen points.
    pub center_pt: Vec2,
}

impl ScreenHandle {
    /// The square the painter draws, in screen points.
    pub fn rect(&self, layout: &HandleLayout) -> egui::Rect {
        let half = match self.handle {
            Handle::Pivot => layout.pivot_pt,
            _ => layout.handle_pt * 0.5,
        };
        egui::Rect::from_center_size(
            super::geom::to_pos2(self.center_pt),
            egui::vec2(half * 2.0, half * 2.0),
        )
    }
}

/// Tie-break order when two handles are exactly equidistant. Lower wins.
pub fn handle_rank(handle: Handle) -> u8 {
    match handle {
        Handle::Corner(_) => 0,
        Handle::Mesh(_, _) => 1,
        Handle::Edge(_) => 2,
        Handle::Pivot => 3,
        Handle::Rotate(_) => 4,
        Handle::Inside => 5,
    }
}

/// Every handle of `state`, projected into screen points.
pub fn screen_handles(
    state: &TransformState,
    mode: TransformMode,
    camera: &CanvasCamera,
    viewport: &Viewport,
) -> Vec<ScreenHandle> {
    state
        .handles(mode)
        .into_iter()
        .map(|(handle, doc)| ScreenHandle {
            handle,
            center_pt: camera.screen_pt_of(viewport, doc),
        })
        .filter(|h| h.center_pt.is_finite())
        .collect()
}

/// The quad's four corners in screen points, clockwise from the top-left.
pub fn screen_quad(
    state: &TransformState,
    camera: &CanvasCamera,
    viewport: &Viewport,
) -> [Vec2; 4] {
    let mut out = [Vec2::ZERO; 4];
    for (i, c) in state.corners.iter().enumerate() {
        out[i] = camera.screen_pt_of(viewport, *c);
    }
    out
}

/// `true` when `p` is inside the convex-or-not quad, by the winding-free
/// crossing test — the same predicate `tools` uses, restated in screen space.
pub fn point_in_quad(p: Vec2, quad: &[Vec2; 4]) -> bool {
    let mut inside = false;
    let mut j = 3;
    for i in 0..4 {
        let (a, b) = (quad[i], quad[j]);
        let straddles = (a.y > p.y) != (b.y > p.y);
        if straddles {
            let t = (p.y - a.y) / (b.y - a.y);
            if p.x < a.x + t * (b.x - a.x) {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// What is under the pointer, in screen points.
///
/// The order is: the explicit handles (nearest centre first), then the rotate
/// band just outside each corner, then the interior. Putting the band before
/// the corners would make scaling impossible; leaving it out would make
/// rotation need a modifier.
pub fn hit_test(
    pos_pt: Vec2,
    state: &TransformState,
    mode: TransformMode,
    camera: &CanvasCamera,
    viewport: &Viewport,
    layout: &HandleLayout,
) -> Option<Handle> {
    if !pos_pt.is_finite() {
        return None;
    }
    let grab = layout.grab();
    let handles = screen_handles(state, mode, camera, viewport);

    let mut best: Option<(f32, u8, Handle)> = None;
    for h in &handles {
        let d = (pos_pt - h.center_pt).length();
        if d > grab {
            continue;
        }
        let rank = handle_rank(h.handle);
        let better = match &best {
            None => true,
            Some((bd, br, _)) => d < *bd - 1e-4 || (d <= *bd + 1e-4 && rank < *br),
        };
        if better {
            best = Some((d, rank, h.handle));
        }
    }
    if let Some((_, _, handle)) = best {
        return Some(handle);
    }

    let quad = screen_quad(state, camera, viewport);
    if mode != TransformMode::Warp {
        let band = layout.rotate_band();
        let mut nearest: Option<(f32, usize)> = None;
        for (i, corner) in quad.iter().enumerate() {
            let d = (pos_pt - *corner).length();
            if d > grab && d <= grab + band && nearest.is_none_or(|(bd, _)| d < bd) {
                nearest = Some((d, i));
            }
        }
        if let Some((_, i)) = nearest {
            return Some(Handle::Rotate(i));
        }
    }
    if point_in_quad(pos_pt, &quad) {
        return Some(Handle::Inside);
    }
    None
}

/// The cursor for a region of the transform box.
///
/// Corner and edge cursors are direction-aware: the arrow points along the
/// direction that handle actually moves *on screen*, which is not the direction
/// it moves in the document once the view is rotated or flipped.
pub fn cursor_for(handle: Handle, quad: &[Vec2; 4]) -> CanvasCursor {
    match handle {
        Handle::Corner(i) => {
            let c = quad[i.min(3)];
            resize_cursor(c - quad_center(quad))
        }
        Handle::Edge(i) => {
            let a = quad[i.min(3)];
            let b = quad[(i + 1) % 4];
            let edge = b - a;
            // The edge moves along its own normal.
            resize_cursor(Vec2::new(-edge.y, edge.x))
        }
        Handle::Mesh(_, _) => CanvasCursor::Crosshair,
        Handle::Rotate(_) => CanvasCursor::Rotate,
        Handle::Pivot => CanvasCursor::Crosshair,
        Handle::Inside => CanvasCursor::Move,
    }
}

fn quad_center(quad: &[Vec2; 4]) -> Vec2 {
    (quad[0] + quad[1] + quad[2] + quad[3]) * 0.25
}

/// The resize cursor whose axis is closest to `dir`, in screen space.
///
/// Four buckets of 45 degrees each. A zero-length direction has no axis, so it
/// falls back to the four-way move cursor rather than picking arbitrarily.
pub fn resize_cursor(dir: Vec2) -> CanvasCursor {
    if !dir.is_finite() || dir.length_squared() <= f32::EPSILON {
        return CanvasCursor::Move;
    }
    // Fold to the upper half plane: a handle and the one opposite it take the
    // same double-headed arrow.
    let d = if dir.y < 0.0 { -dir } else { dir };
    let angle = d.y.atan2(d.x); // 0..=pi
    use std::f32::consts::PI;
    let eighth = PI / 8.0;
    if angle < eighth || angle >= 7.0 * eighth {
        CanvasCursor::ResizeHorizontal
    } else if angle < 3.0 * eighth {
        // Down-right / up-left.
        CanvasCursor::ResizeNwSe
    } else if angle < 5.0 * eighth {
        CanvasCursor::ResizeVertical
    } else {
        CanvasCursor::ResizeNeSw
    }
}

/// The screen-point bounding box of the transform box and its furniture, for
/// deciding whether anything needs repainting.
pub fn overlay_bounds(
    state: &TransformState,
    mode: TransformMode,
    camera: &CanvasCamera,
    viewport: &Viewport,
    layout: &HandleLayout,
) -> Option<DocRect> {
    let mut points: Vec<Vec2> = screen_quad(state, camera, viewport).to_vec();
    points.extend(
        screen_handles(state, mode, camera, viewport)
            .iter()
            .map(|h| h.center_pt),
    );
    DocRect::of_points(&points).map(|r| r.expanded(layout.grab() + layout.rotate_band()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use raster::PixelRect;

    fn vp() -> Viewport {
        // One screen point per document pixel at zoom 2 with a 2x display.
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    /// A camera in which one document pixel is exactly one screen point (zoom
    /// 2 on a 2x display), looking at the middle of a `box_size` square.
    fn unit_cam(_v: &Viewport, box_size: f32) -> CanvasCamera {
        CanvasCamera {
            center: Vec2::splat(box_size * 0.5),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    fn state(size: i64) -> TransformState {
        TransformState::new(PixelRect::new(0, 0, size as u32, size as u32))
    }

    #[test]
    fn every_handle_projects_to_where_the_camera_puts_it() {
        let v = vp();
        let c = unit_cam(&v, 100.0);
        let s = state(100);
        let projected = screen_handles(&s, TransformMode::Scale, &c, &v);
        let doc = s.handles(TransformMode::Scale);
        assert_eq!(projected.len(), doc.len());
        for (p, (h, d)) in projected.iter().zip(doc) {
            assert_eq!(p.handle, h);
            assert!((p.center_pt - c.screen_pt_of(&v, d)).length() < 1e-3);
        }
    }

    #[test]
    fn the_scale_mode_offers_corners_edges_and_the_pivot() {
        let s = state(100);
        let v = vp();
        let c = unit_cam(&v, 100.0);
        let hs = screen_handles(&s, TransformMode::Scale, &c, &v);
        assert_eq!(hs.len(), 9);
        for i in 0..4 {
            assert!(hs.iter().any(|h| h.handle == Handle::Corner(i)));
            assert!(hs.iter().any(|h| h.handle == Handle::Edge(i)));
        }
        assert!(hs.iter().any(|h| h.handle == Handle::Pivot));
    }

    #[test]
    fn warp_mode_offers_the_mesh_instead() {
        let s = state(100);
        let v = vp();
        let c = unit_cam(&v, 100.0);
        let hs = screen_handles(&s, TransformMode::Warp, &c, &v);
        assert_eq!(hs.len(), 16);
        assert!(hs.iter().all(|h| matches!(h.handle, Handle::Mesh(_, _))));
    }

    /// Each handle is grabbed by clicking on it, at a comfortable size.
    #[test]
    fn clicking_a_handle_grabs_that_handle() {
        let v = vp();
        let s = state(400);
        let c = unit_cam(&v, 400.0);
        let layout = HandleLayout::default();
        for h in screen_handles(&s, TransformMode::Scale, &c, &v) {
            let got = hit_test(h.center_pt, &s, TransformMode::Scale, &c, &v, &layout);
            assert_eq!(got, Some(h.handle), "clicking {:?} gave {got:?}", h.handle);
        }
    }

    /// The overlap case: at a box small enough that every grab disc overlaps
    /// every other, the pointer still resolves to the *nearest* handle.
    #[test]
    fn overlapping_handles_resolve_to_the_nearest_one() {
        let v = vp();
        // A 10-document-pixel box is 10 screen points across; the grab radius
        // is 10 points, so every handle's disc covers every other handle.
        let s = state(10);
        let c = unit_cam(&v, 10.0);
        let layout = HandleLayout::default();
        let handles = screen_handles(&s, TransformMode::Scale, &c, &v);
        let at = |h: Handle| {
            handles
                .iter()
                .find(|x| x.handle == h)
                .expect("handle missing")
                .center_pt
        };
        // Sanity: the discs really do overlap, so this is the hard case.
        assert!((at(Handle::Corner(0)) - at(Handle::Edge(0))).length() < layout.grab());

        for handle in [
            Handle::Corner(0),
            Handle::Corner(1),
            Handle::Corner(2),
            Handle::Corner(3),
            Handle::Edge(0),
            Handle::Edge(1),
            Handle::Edge(2),
            Handle::Edge(3),
            Handle::Pivot,
        ] {
            // A pointer a fifth of a point off the true centre still resolves
            // to it, because nothing else can be nearer.
            let probe = at(handle) + Vec2::new(0.2, 0.0);
            let got = hit_test(probe, &s, TransformMode::Scale, &c, &v, &layout);
            assert_eq!(got, Some(handle), "{handle:?} lost to {got:?}");
        }
    }

    #[test]
    fn an_exact_tie_between_a_corner_and_an_edge_goes_to_the_corner() {
        let v = vp();
        let s = state(10);
        let c = unit_cam(&v, 10.0);
        let layout = HandleLayout::default();
        let handles = screen_handles(&s, TransformMode::Scale, &c, &v);
        let corner = handles
            .iter()
            .find(|h| h.handle == Handle::Corner(0))
            .unwrap()
            .center_pt;
        let edge = handles
            .iter()
            .find(|h| h.handle == Handle::Edge(0))
            .unwrap()
            .center_pt;
        let midway = (corner + edge) * 0.5;
        assert_eq!(
            hit_test(midway, &s, TransformMode::Scale, &c, &v, &layout),
            Some(Handle::Corner(0))
        );
    }

    /// The bug this module replaces: a document-space radius is unusable once
    /// the zoom moves. A screen-space one is not.
    #[test]
    fn the_grab_target_is_the_same_size_on_screen_at_every_zoom() {
        let v = vp();
        let s = state(200);
        let layout = HandleLayout::default();
        for zoom in [0.05_f32, 0.5, 2.0, 32.0] {
            let c = CanvasCamera {
                center: Vec2::splat(100.0),
                zoom,
                ..CanvasCamera::default()
            };
            let handles = screen_handles(&s, TransformMode::Scale, &c, &v);
            let corner = handles
                .iter()
                .find(|h| h.handle == Handle::Corner(0))
                .unwrap()
                .center_pt;
            // Just inside the grab radius, on the diagonal away from the box.
            let inside = corner + Vec2::splat(-layout.grab() * 0.5 / 1.4143);
            assert_eq!(
                hit_test(inside, &s, TransformMode::Scale, &c, &v, &layout),
                Some(Handle::Corner(0)),
                "zoom {zoom}: a point {}pt from the corner missed it",
                layout.grab() * 0.5
            );
        }
    }

    #[test]
    fn just_outside_a_corner_rotates_and_further_out_is_nothing() {
        let v = vp();
        let s = state(400);
        let c = unit_cam(&v, 400.0);
        let layout = HandleLayout::default();
        let corner = screen_handles(&s, TransformMode::Scale, &c, &v)
            .iter()
            .find(|h| h.handle == Handle::Corner(0))
            .unwrap()
            .center_pt;
        let out = Vec2::new(-1.0, -1.0).normalize();

        let band = corner + out * (layout.grab() + layout.rotate_band() * 0.5);
        assert_eq!(
            hit_test(band, &s, TransformMode::Scale, &c, &v, &layout),
            Some(Handle::Rotate(0))
        );
        let past = corner + out * (layout.grab() + layout.rotate_band() + 5.0);
        assert_eq!(
            hit_test(past, &s, TransformMode::Scale, &c, &v, &layout),
            None
        );
    }

    #[test]
    fn the_rotate_band_is_absent_in_warp_mode() {
        let v = vp();
        let s = state(400);
        let c = unit_cam(&v, 400.0);
        let layout = HandleLayout::default();
        let corner = c.screen_pt_of(&v, s.corners[0]);
        let just_outside = corner + Vec2::new(-1.0, -1.0).normalize() * (layout.grab() + 5.0);
        assert_eq!(
            hit_test(just_outside, &s, TransformMode::Warp, &c, &v, &layout),
            None
        );
    }

    #[test]
    fn the_interior_moves_the_box_and_the_outside_grabs_nothing() {
        let v = vp();
        let s = state(400);
        let c = unit_cam(&v, 400.0);
        let layout = HandleLayout::default();
        let middle = c.screen_pt_of(&v, Vec2::new(120.0, 300.0));
        assert_eq!(
            hit_test(middle, &s, TransformMode::Scale, &c, &v, &layout),
            Some(Handle::Inside)
        );
        let far = c.screen_pt_of(&v, Vec2::new(-500.0, -500.0));
        assert_eq!(
            hit_test(far, &s, TransformMode::Scale, &c, &v, &layout),
            None
        );
    }

    #[test]
    fn a_nonsense_pointer_grabs_nothing() {
        let v = vp();
        let s = state(100);
        let c = unit_cam(&v, 100.0);
        assert_eq!(
            hit_test(
                Vec2::new(f32::NAN, 0.0),
                &s,
                TransformMode::Scale,
                &c,
                &v,
                &HandleLayout::default()
            ),
            None
        );
    }

    #[test]
    fn a_hostile_layout_is_clamped_so_handles_stay_grabbable() {
        let bad = HandleLayout {
            grab_pt: f32::NAN,
            rotate_band_pt: -10.0,
            ..HandleLayout::default()
        };
        assert_eq!(bad.grab(), HandleLayout::default().grab_pt);
        assert_eq!(bad.rotate_band(), 0.0);
        let huge = HandleLayout {
            grab_pt: 1e6,
            rotate_band_pt: 1e6,
            ..HandleLayout::default()
        };
        assert_eq!(huge.grab(), 64.0);
        assert_eq!(huge.rotate_band(), 128.0);
    }

    #[test]
    fn resize_cursors_point_along_the_direction_the_handle_moves() {
        assert_eq!(
            resize_cursor(Vec2::new(1.0, 0.0)),
            CanvasCursor::ResizeHorizontal
        );
        assert_eq!(
            resize_cursor(Vec2::new(-1.0, 0.0)),
            CanvasCursor::ResizeHorizontal
        );
        assert_eq!(
            resize_cursor(Vec2::new(0.0, 1.0)),
            CanvasCursor::ResizeVertical
        );
        assert_eq!(
            resize_cursor(Vec2::new(0.0, -1.0)),
            CanvasCursor::ResizeVertical
        );
        // Down-right on a y-down screen is the north-west/south-east diagonal.
        assert_eq!(resize_cursor(Vec2::new(1.0, 1.0)), CanvasCursor::ResizeNwSe);
        assert_eq!(
            resize_cursor(Vec2::new(-1.0, -1.0)),
            CanvasCursor::ResizeNwSe
        );
        assert_eq!(
            resize_cursor(Vec2::new(1.0, -1.0)),
            CanvasCursor::ResizeNeSw
        );
        assert_eq!(
            resize_cursor(Vec2::new(-1.0, 1.0)),
            CanvasCursor::ResizeNeSw
        );
        assert_eq!(resize_cursor(Vec2::ZERO), CanvasCursor::Move);
        assert_eq!(resize_cursor(Vec2::new(f32::NAN, 1.0)), CanvasCursor::Move);
    }

    /// Rotating the *view* has to rotate the cursors with it, or the arrow
    /// points the wrong way as soon as the canvas is turned.
    #[test]
    fn the_cursor_follows_the_view_rotation() {
        let v = vp();
        let s = state(200);
        let upright = CanvasCamera {
            center: Vec2::splat(100.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let quad = screen_quad(&s, &upright, &v);
        // The top edge of an upright box resizes vertically.
        assert_eq!(
            cursor_for(Handle::Edge(0), &quad),
            CanvasCursor::ResizeVertical
        );
        let turned = CanvasCamera {
            rotation: std::f32::consts::FRAC_PI_2,
            ..upright
        };
        let quad = screen_quad(&s, &turned, &v);
        assert_eq!(
            cursor_for(Handle::Edge(0), &quad),
            CanvasCursor::ResizeHorizontal
        );
    }

    #[test]
    fn every_region_has_a_cursor() {
        let v = vp();
        let s = state(200);
        let c = unit_cam(&v, 200.0);
        let quad = screen_quad(&s, &c, &v);
        assert_eq!(cursor_for(Handle::Inside, &quad), CanvasCursor::Move);
        assert_eq!(cursor_for(Handle::Rotate(0), &quad), CanvasCursor::Rotate);
        assert_eq!(cursor_for(Handle::Pivot, &quad), CanvasCursor::Crosshair);
        assert_eq!(
            cursor_for(Handle::Mesh(1, 2), &quad),
            CanvasCursor::Crosshair
        );
        assert_eq!(
            cursor_for(Handle::Corner(0), &quad),
            CanvasCursor::ResizeNwSe
        );
        assert_eq!(
            cursor_for(Handle::Corner(1), &quad),
            CanvasCursor::ResizeNeSw
        );
    }

    #[test]
    fn the_overlay_bounds_cover_the_furniture() {
        let v = vp();
        let s = state(200);
        let c = unit_cam(&v, 200.0);
        let layout = HandleLayout::default();
        let b = overlay_bounds(&s, TransformMode::Scale, &c, &v, &layout).unwrap();
        for corner in screen_quad(&s, &c, &v) {
            assert!(b.contains(corner));
            // The rotate band around each corner is inside the bounds too.
            assert!(b
                .expanded(1.0)
                .contains(corner - Vec2::splat(layout.rotate_band())));
        }
    }

    #[test]
    fn quad_containment_matches_the_corners() {
        let quad = [
            Vec2::new(0.0, 0.0),
            Vec2::new(10.0, 0.0),
            Vec2::new(10.0, 10.0),
            Vec2::new(0.0, 10.0),
        ];
        assert!(point_in_quad(Vec2::new(5.0, 5.0), &quad));
        assert!(!point_in_quad(Vec2::new(-1.0, 5.0), &quad));
        assert!(!point_in_quad(Vec2::new(11.0, 5.0), &quad));
        assert!(!point_in_quad(Vec2::new(5.0, -1.0), &quad));
    }
}
