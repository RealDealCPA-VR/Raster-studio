//! The canvas camera: pan, zoom, view rotation and view flip.
//!
//! # Why zoom is measured in *physical* pixels
//!
//! "100%" has exactly one useful meaning in an image editor: one document pixel
//! covers one pixel of the display. If zoom were counted in logical points, a
//! 2x display would show every image at 200% while calling it 100%, and the
//! pixel grid would never line up. So [`CanvasCamera::zoom`] is physical pixels
//! per document pixel, and the point-space transform derives from it by
//! dividing out [`Viewport::pixels_per_point`].
//!
//! # The transform
//!
//! ```text
//!   doc -> screen  =  T(viewport centre) · F(flip) · S(scale) · R(rotation) · T(-centre)
//! ```
//!
//! Read right to left: translate the camera centre to the origin, rotate the
//! canvas, scale it, mirror it, then land it on the centre of the *content*
//! area — never on the centre of the window. The flip is applied last, in
//! screen space, so a flipped view is a mirror of what an unflipped one shows,
//! including the direction its rotation appears to run in.
//!
//! Rotation is positive-clockwise on screen, because the document's y axis
//! points down. That is the same convention [`tools::ViewState`] uses, so the
//! two agree when bridged through [`CanvasCamera::to_view_state`].
//!
//! # Ownership of the view
//!
//! `tools` owns three navigation tools that mutate a [`tools::ViewState`]. That
//! type predates panel insets, display scale and view flipping, so the canvas
//! is authoritative: the router in [`super::input`] applies navigation directly
//! to this camera, and [`CanvasCamera::to_view_state`] /
//! [`CanvasCamera::apply_view_state`] exist for the code paths that still have
//! to hand a `ToolContext` a view. Round-tripping through `ViewState` loses the
//! flip flags, which is asserted rather than hidden.

use glam::{Mat3, Vec2};

use super::geom::DocRect;
use super::viewport::Viewport;

/// Pan, zoom, rotation and flip of the canvas view.
///
/// A view change is deliberately **not** an [`editor_core::Command`]: it edits
/// no pixel, survives no save, and a ctrl+Z that scrolls the canvas instead of
/// undoing a stroke is the most confusing thing an editor can do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasCamera {
    /// The document point sitting at the centre of the content area.
    pub center: Vec2,
    /// Physical pixels per document pixel. `1.0` is 100%.
    pub zoom: f32,
    /// View rotation in radians, positive clockwise on screen.
    pub rotation: f32,
    /// Mirror the view left-to-right.
    pub flip_x: bool,
    /// Mirror the view top-to-bottom.
    pub flip_y: bool,
}

impl Default for CanvasCamera {
    fn default() -> Self {
        Self {
            center: Vec2::ZERO,
            zoom: 1.0,
            rotation: 0.0,
            flip_x: false,
            flip_y: false,
        }
    }
}

/// The zoom rungs the keyboard and the menu step through, ascending.
///
/// The list is the *whole* contract of [`CanvasCamera::zoom_in`] and
/// [`CanvasCamera::zoom_out`]: those two land on the next rung, so the user can
/// always get back to an exact 100% by tapping the key rather than by dragging.
pub const ZOOM_STEPS: &[f32] = &[
    1.0 / 256.0,
    1.0 / 128.0,
    1.0 / 64.0,
    1.0 / 32.0,
    1.0 / 16.0,
    1.0 / 12.0,
    1.0 / 8.0,
    1.0 / 6.0,
    1.0 / 4.0,
    1.0 / 3.0,
    1.0 / 2.0,
    2.0 / 3.0,
    1.0,
    1.5,
    2.0,
    3.0,
    4.0,
    6.0,
    8.0,
    12.0,
    16.0,
    24.0,
    32.0,
    48.0,
    64.0,
    128.0,
    256.0,
];

/// A camera expressed the way a renderer wants it: a target rectangle on the
/// physical surface, and the view that fills it.
///
/// `viewport_origin_px` is the piece that was missing before this module: a
/// renderer that only knows a surface size centres the image on the window.
/// Either honour the origin, or use [`RenderCamera::center_for_full_surface`]
/// to get the centre that makes a whole-surface renderer land the image in the
/// right place anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RenderCamera {
    /// Top-left of the region the document draws into, in physical pixels.
    pub viewport_origin_px: Vec2,
    /// Size of that region, in physical pixels.
    pub viewport_size_px: Vec2,
    /// Size of the whole surface, in physical pixels.
    pub surface_size_px: Vec2,
    /// Document point at the centre of the region.
    pub center: Vec2,
    /// Physical pixels per document pixel.
    pub zoom: f32,
    pub rotation: f32,
    pub flip_x: bool,
    pub flip_y: bool,
}

impl RenderCamera {
    /// The document point that must sit at the centre of the **whole surface**
    /// for the image to appear centred in the inset content area.
    ///
    /// This is the shim for a renderer that has no viewport offset — it lets
    /// the panels be honoured without touching the render pipeline. Prefer a
    /// real scissor rectangle when one is available, because this shim draws
    /// the image under the panels and relies on them being opaque.
    pub fn center_for_full_surface(&self) -> Vec2 {
        let target = self.viewport_origin_px + self.viewport_size_px * 0.5;
        let surface_center = self.surface_size_px * 0.5;
        let basis = basis_matrix(self.zoom, self.rotation, self.flip_x, self.flip_y);
        let Some(inverse) = invertible(basis) else {
            return self.center;
        };
        self.center - inverse.transform_vector2(target - surface_center)
    }
}

/// `F · S · R` — everything between the two translations.
fn basis_matrix(scale: f32, rotation: f32, flip_x: bool, flip_y: bool) -> Mat3 {
    let flip = Vec2::new(
        if flip_x { -1.0 } else { 1.0 },
        if flip_y { -1.0 } else { 1.0 },
    );
    Mat3::from_scale(flip) * Mat3::from_scale(Vec2::splat(scale)) * Mat3::from_angle(rotation)
}

/// `None` when the matrix cannot be inverted — which here means a zero or
/// non-finite scale. Nothing in this module produces one, because every write
/// to `zoom` goes through [`CanvasCamera::set_zoom`], but `zoom` is a public
/// field so a caller can still create the case.
fn invertible(m: Mat3) -> Option<Mat3> {
    // The threshold is far below any legal scale — the smallest the camera can
    // produce is MIN_ZOOM over MAX_SCALE, whose determinant is about 6e-8 — so
    // this rejects only a genuinely singular matrix, never a very small one.
    let det = m.determinant();
    if !det.is_finite() || det.abs() < 1e-20 {
        return None;
    }
    let inv = m.inverse();
    inv.to_cols_array()
        .iter()
        .all(|v| v.is_finite())
        .then_some(inv)
}

impl CanvasCamera {
    /// Smallest and largest zoom the camera will settle on.
    pub const MIN_ZOOM: f32 = 1.0 / 256.0;
    pub const MAX_ZOOM: f32 = 256.0;

    /// Fraction of the content area left empty around a fitted image.
    pub const FIT_MARGIN: f32 = 0.02;

    /// A camera looking at the centre of a document of `size` pixels at 100%.
    pub fn for_document(size: Vec2) -> Self {
        Self {
            center: size * 0.5,
            ..Self::default()
        }
    }

    /// Screen **points** per document pixel — the scale every overlay is drawn
    /// at, and what a hit-test threshold expressed in points converts through.
    pub fn scale_pt(&self, viewport: &Viewport) -> f32 {
        self.zoom / viewport.pixels_per_point()
    }

    /// Document space to screen **points**.
    pub fn doc_to_pt(&self, viewport: &Viewport) -> Mat3 {
        Mat3::from_translation(viewport.center_pt())
            * basis_matrix(
                self.scale_pt(viewport),
                self.rotation,
                self.flip_x,
                self.flip_y,
            )
            * Mat3::from_translation(-self.center)
    }

    /// Document space to physical **pixels** on the surface.
    pub fn doc_to_px(&self, viewport: &Viewport) -> Mat3 {
        Mat3::from_translation(viewport.center_px())
            * basis_matrix(self.zoom, self.rotation, self.flip_x, self.flip_y)
            * Mat3::from_translation(-self.center)
    }

    /// Screen points to document space, or `None` if the camera is degenerate.
    pub fn pt_to_doc(&self, viewport: &Viewport) -> Option<Mat3> {
        invertible(self.doc_to_pt(viewport))
    }

    /// Physical pixels to document space, or `None` if the camera is degenerate.
    pub fn px_to_doc(&self, viewport: &Viewport) -> Option<Mat3> {
        invertible(self.doc_to_px(viewport))
    }

    /// Where a document point lands, in screen points.
    pub fn screen_pt_of(&self, viewport: &Viewport, doc: Vec2) -> Vec2 {
        self.doc_to_pt(viewport).transform_point2(doc)
    }

    /// Where a document point lands, in physical pixels.
    pub fn screen_px_of(&self, viewport: &Viewport, doc: Vec2) -> Vec2 {
        self.doc_to_px(viewport).transform_point2(doc)
    }

    /// The document point under a screen position. Falls back to the camera
    /// centre when the camera is degenerate, so callers never see a `NaN`.
    pub fn doc_of_screen_pt(&self, viewport: &Viewport, pt: Vec2) -> Vec2 {
        match self.pt_to_doc(viewport) {
            Some(m) => m.transform_point2(pt),
            None => self.center,
        }
    }

    /// The document point under a physical-pixel position.
    pub fn doc_of_screen_px(&self, viewport: &Viewport, px: Vec2) -> Vec2 {
        match self.px_to_doc(viewport) {
            Some(m) => m.transform_point2(px),
            None => self.center,
        }
    }

    /// A screen-point *direction* in document space (no translation).
    pub fn doc_vector_of_screen_pt(&self, viewport: &Viewport, delta_pt: Vec2) -> Vec2 {
        match self.pt_to_doc(viewport) {
            Some(m) => m.transform_vector2(delta_pt),
            None => Vec2::ZERO,
        }
    }

    /// Set the zoom, clamped into the legal range. Non-finite values are
    /// ignored; this is the only safe way to write the field.
    pub fn set_zoom(&mut self, zoom: f32) {
        if !zoom.is_finite() {
            return;
        }
        self.zoom = zoom.clamp(Self::MIN_ZOOM, Self::MAX_ZOOM);
    }

    /// Set the rotation, wrapped into `(-π, π]`. Non-finite values are ignored.
    pub fn set_rotation(&mut self, radians: f32) {
        if !radians.is_finite() {
            return;
        }
        self.rotation = wrap_angle(radians);
    }

    /// Turn the view by `radians`.
    pub fn rotate_by(&mut self, radians: f32) {
        self.set_rotation(self.rotation + radians);
    }

    /// Put the canvas back upright without touching pan or zoom.
    pub fn reset_rotation(&mut self) {
        self.rotation = 0.0;
    }

    /// Mirror the view left-to-right about the centre of the content area.
    pub fn flip_horizontal(&mut self) {
        self.flip_x = !self.flip_x;
    }

    /// Mirror the view top-to-bottom about the centre of the content area.
    pub fn flip_vertical(&mut self) {
        self.flip_y = !self.flip_y;
    }

    /// Undo every mirror.
    pub fn reset_flip(&mut self) {
        self.flip_x = false;
        self.flip_y = false;
    }

    /// Pan by a screen-point delta: the document moves *with* the drag, so the
    /// point the user grabbed stays under the cursor.
    pub fn pan_screen_pt(&mut self, viewport: &Viewport, delta_pt: Vec2) {
        let d = self.doc_vector_of_screen_pt(viewport, delta_pt);
        if d.is_finite() {
            self.center -= d;
        }
    }

    /// Scale the zoom by `factor`, keeping the document point currently under
    /// `anchor_pt` exactly where it is. This is zoom-to-cursor.
    pub fn zoom_about_screen_pt(&mut self, viewport: &Viewport, anchor_pt: Vec2, factor: f32) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        self.set_zoom_about_screen_pt(viewport, anchor_pt, self.zoom * factor);
    }

    /// Move to an absolute zoom, keeping the point under `anchor_pt` fixed.
    pub fn set_zoom_about_screen_pt(&mut self, viewport: &Viewport, anchor_pt: Vec2, zoom: f32) {
        if !zoom.is_finite() || !anchor_pt.is_finite() {
            return;
        }
        let anchor_doc = self.doc_of_screen_pt(viewport, anchor_pt);
        self.set_zoom(zoom);
        // Re-derive where the anchor went and put the centre back so it did not
        // move. Computed after the clamp, so hitting MIN/MAX_ZOOM still leaves
        // the anchor stationary instead of drifting by the clamped remainder.
        let landed = self.screen_pt_of(viewport, anchor_doc);
        let drift = self.doc_vector_of_screen_pt(viewport, landed - anchor_pt);
        if drift.is_finite() {
            self.center += drift;
        }
    }

    /// The next rung of [`ZOOM_STEPS`] above the current zoom, anchored at a
    /// screen point.
    pub fn zoom_in(&mut self, viewport: &Viewport, anchor_pt: Vec2) {
        let next = ZOOM_STEPS
            .iter()
            .copied()
            .find(|z| *z > self.zoom * 1.0001)
            .unwrap_or(Self::MAX_ZOOM);
        self.set_zoom_about_screen_pt(viewport, anchor_pt, next);
    }

    /// The next rung of [`ZOOM_STEPS`] below the current zoom.
    pub fn zoom_out(&mut self, viewport: &Viewport, anchor_pt: Vec2) {
        let next = ZOOM_STEPS
            .iter()
            .rev()
            .copied()
            .find(|z| *z < self.zoom * 0.9999)
            .unwrap_or(Self::MIN_ZOOM);
        self.set_zoom_about_screen_pt(viewport, anchor_pt, next);
    }

    /// 100%: one document pixel per physical pixel, centred where it was.
    pub fn zoom_to_actual_pixels(&mut self, viewport: &Viewport) {
        self.set_zoom_about_screen_pt(viewport, viewport.center_pt(), 1.0);
        self.snap_center_to_pixel_grid(viewport);
    }

    /// Fit the whole of `rect` inside the content area, centred.
    pub fn fit_rect(&mut self, viewport: &Viewport, rect: DocRect) {
        self.frame_rect(viewport, rect, true);
    }

    /// Fill the content area with `rect`, cropping the longer dimension.
    pub fn fill_rect(&mut self, viewport: &Viewport, rect: DocRect) {
        self.frame_rect(viewport, rect, false);
    }

    /// Fit the whole document.
    pub fn fit_document(&mut self, viewport: &Viewport, doc_size: Vec2) {
        self.fit_rect(viewport, DocRect::of_canvas(doc_size));
    }

    /// Fill the content area with the document.
    pub fn fill_document(&mut self, viewport: &Viewport, doc_size: Vec2) {
        self.fill_rect(viewport, DocRect::of_canvas(doc_size));
    }

    fn frame_rect(&mut self, viewport: &Viewport, rect: DocRect, fit: bool) {
        if viewport.is_degenerate() || rect.is_empty() {
            return;
        }
        // Under rotation the rectangle needs its *rotated* bounding box to fit,
        // otherwise a 45-degree view clips the corners off.
        let (s, c) = self.rotation.sin_cos();
        let (w, h) = (rect.width(), rect.height());
        let spanned = Vec2::new((w * c).abs() + (h * s).abs(), (w * s).abs() + (h * c).abs());
        if !(spanned.x > 0.0 && spanned.y > 0.0) {
            return;
        }
        let available = viewport.size_px() * (1.0 - Self::FIT_MARGIN);
        let sx = available.x / spanned.x;
        let sy = available.y / spanned.y;
        self.set_zoom(if fit { sx.min(sy) } else { sx.max(sy) });
        self.center = rect.center();
    }

    /// Nudge the centre so the document pixel grid lands on whole physical
    /// pixels. Only meaningful on an unrotated view, so a rotated one is left
    /// alone rather than being silently shifted.
    ///
    /// This is what makes 100% *pixel-exact*: without it a content area with an
    /// odd pixel width puts the image on a half-pixel and every edge in the
    /// picture is resampled across two device pixels.
    pub fn snap_center_to_pixel_grid(&mut self, viewport: &Viewport) {
        if self.rotation != 0.0 || self.zoom <= 0.0 || !self.center.is_finite() {
            return;
        }
        let origin_px = self.screen_px_of(viewport, Vec2::ZERO);
        let drift_px = origin_px - origin_px.round();
        // A screen-pixel drift is a document offset of drift / zoom, and the
        // flip only mirrors it, so its sign cancels when applied through the
        // same basis.
        let basis = basis_matrix(self.zoom, self.rotation, self.flip_x, self.flip_y);
        let Some(inverse) = invertible(basis) else {
            return;
        };
        self.center += inverse.transform_vector2(drift_px);
    }

    /// The document rectangle currently visible, as a bounding box. Under
    /// rotation this is larger than what is literally on screen, which is what
    /// a culling caller wants.
    pub fn visible_doc_rect(&self, viewport: &Viewport) -> DocRect {
        let b = viewport.content_bounds_pt();
        let corners = [
            self.doc_of_screen_pt(viewport, b.min),
            self.doc_of_screen_pt(viewport, Vec2::new(b.max.x, b.min.y)),
            self.doc_of_screen_pt(viewport, b.max),
            self.doc_of_screen_pt(viewport, Vec2::new(b.min.x, b.max.y)),
        ];
        DocRect::of_points(&corners).unwrap_or(DocRect::ZERO)
    }

    /// Everything a renderer needs, with the content region spelled out.
    pub fn render_camera(&self, viewport: &Viewport) -> RenderCamera {
        RenderCamera {
            viewport_origin_px: viewport.origin_px(),
            viewport_size_px: viewport.size_px(),
            surface_size_px: viewport.surface_px(),
            center: self.center,
            zoom: self.zoom,
            rotation: self.rotation,
            flip_x: self.flip_x,
            flip_y: self.flip_y,
        }
    }

    /// The `tools` view of this camera, for handing to a [`tools::ToolContext`].
    ///
    /// [`tools::ViewState`] measures its viewport in the same units as its zoom
    /// and knows nothing about flipping, so the physical-pixel content size is
    /// what goes in. The flip flags are dropped — see the module docs.
    pub fn to_view_state(&self, viewport: &Viewport) -> tools::ViewState {
        tools::ViewState {
            center: self.center,
            zoom: self.zoom,
            rotation: self.rotation,
            viewport: viewport.size_px(),
        }
    }

    /// Take back a [`tools::ViewState`] a navigation tool mutated, leaving the
    /// flip flags — which that type cannot carry — untouched.
    pub fn apply_view_state(&mut self, view: &tools::ViewState) {
        self.center = view.center;
        self.set_zoom(view.zoom);
        self.set_rotation(view.rotation);
    }
}

/// Wrap an angle into `(-π, π]`.
fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let mut x = a % TAU;
    if x > PI {
        x -= TAU;
    } else if x <= -PI {
        x += TAU;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};

    fn inset_viewport(scale: f32) -> Viewport {
        Viewport::new(
            Vec2::new(1200.0, 900.0),
            PanelInsets::new(260.0, 320.0, 44.0, 28.0),
            scale,
        )
    }

    fn close(a: Vec2, b: Vec2, tol: f32) -> bool {
        (a - b).length() <= tol
    }

    /// The headline invariant: screen <-> document must round-trip at every
    /// zoom and rotation, *with panel insets and a non-1.0 display scale*.
    #[test]
    fn screen_and_document_round_trip_with_insets_and_dpi() {
        let probes = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.5, -20.25),
            Vec2::new(4096.0, 2160.0),
            Vec2::new(-777.0, 333.0),
        ];
        for scale in [1.0_f32, 1.25, 2.0, 3.0] {
            let vp = inset_viewport(scale);
            for zoom in [1.0 / 64.0, 0.5, 1.0, 2.5, 16.0, 200.0] {
                for rotation in [0.0, FRAC_PI_4, FRAC_PI_2, 2.3, -1.1, PI] {
                    for (flip_x, flip_y) in
                        [(false, false), (true, false), (false, true), (true, true)]
                    {
                        let cam = CanvasCamera {
                            center: Vec2::new(613.0, 419.0),
                            zoom,
                            rotation,
                            flip_x,
                            flip_y,
                        };
                        for doc in probes {
                            let pt = cam.screen_pt_of(&vp, doc);
                            let back = cam.doc_of_screen_pt(&vp, pt);
                            // The tolerance is a *screen* tolerance expressed
                            // in document units: a thousandth of a point is
                            // exactness, and at 1/64 zoom that is 64 times as
                            // many document pixels.
                            let tol = doc.length().max(1.0) * 1e-3 / cam.scale_pt(&vp).min(1.0);
                            assert!(
                                close(back, doc, tol),
                                "pt round trip failed: scale={scale} zoom={zoom} \
                                 rot={rotation} flip=({flip_x},{flip_y}) {doc:?} -> \
                                 {pt:?} -> {back:?}"
                            );
                            let px = cam.screen_px_of(&vp, doc);
                            let back_px = cam.doc_of_screen_px(&vp, px);
                            assert!(
                                close(back_px, doc, tol.max(1e-3 / zoom.min(1.0))),
                                "px round trip failed: {doc:?} -> {px:?} -> {back_px:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The panel-inset bug, stated directly: the camera centre lands in the
    /// middle of the free area, not in the middle of the window.
    #[test]
    fn the_camera_centre_lands_in_the_free_area_not_the_window() {
        let vp = inset_viewport(2.0);
        let cam = CanvasCamera::for_document(Vec2::new(800.0, 600.0));
        let at = cam.screen_pt_of(&vp, cam.center);
        assert!(close(at, vp.center_pt(), 1e-4), "{at:?}");
        assert!(!close(at, vp.surface_pt() * 0.5, 1.0));
    }

    #[test]
    fn points_and_pixels_differ_by_exactly_the_display_scale() {
        let vp = inset_viewport(2.0);
        let cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 3.0,
            ..CanvasCamera::default()
        };
        assert_eq!(cam.scale_pt(&vp), 1.5);
        let doc = Vec2::new(140.0, 60.0);
        let pt = cam.screen_pt_of(&vp, doc);
        let px = cam.screen_px_of(&vp, doc);
        assert!(close(px, pt * 2.0, 1e-3), "{pt:?} vs {px:?}");
    }

    /// Zoom-to-cursor: the document point under the cursor may not move.
    #[test]
    fn zooming_about_the_cursor_keeps_the_point_under_it_stationary() {
        for scale in [1.0_f32, 1.5, 2.0] {
            let vp = inset_viewport(scale);
            for rotation in [0.0, 0.7, -2.0] {
                for anchor in [
                    vp.origin_pt() + Vec2::new(5.0, 5.0),
                    vp.center_pt(),
                    vp.max_pt() - Vec2::new(3.0, 11.0),
                ] {
                    let mut cam = CanvasCamera {
                        center: Vec2::new(400.0, 250.0),
                        zoom: 1.0,
                        rotation,
                        flip_x: rotation < 0.0,
                        flip_y: false,
                    };
                    let before = cam.doc_of_screen_pt(&vp, anchor);
                    for factor in [1.25_f32, 1.25, 0.5, 8.0, 0.1] {
                        cam.zoom_about_screen_pt(&vp, anchor, factor);
                        let after = cam.doc_of_screen_pt(&vp, anchor);
                        assert!(
                            close(after, before, 0.02),
                            "anchor drifted at scale={scale} rot={rotation} \
                             factor={factor}: {before:?} -> {after:?}"
                        );
                    }
                }
            }
        }
    }

    /// Even when the zoom clamps, the anchor must not drift — the correction is
    /// computed from where the zoom actually landed.
    #[test]
    fn the_anchor_holds_even_when_the_zoom_clamps() {
        let vp = inset_viewport(2.0);
        let anchor = vp.origin_pt() + Vec2::new(17.0, 23.0);
        let mut cam = CanvasCamera {
            center: Vec2::new(50.0, 50.0),
            zoom: 200.0,
            ..CanvasCamera::default()
        };
        let before = cam.doc_of_screen_pt(&vp, anchor);
        cam.zoom_about_screen_pt(&vp, anchor, 1000.0);
        assert_eq!(cam.zoom, CanvasCamera::MAX_ZOOM);
        assert!(close(cam.doc_of_screen_pt(&vp, anchor), before, 1e-3));

        cam.zoom_about_screen_pt(&vp, anchor, 1e-9);
        assert_eq!(cam.zoom, CanvasCamera::MIN_ZOOM);
        assert!(close(cam.doc_of_screen_pt(&vp, anchor), before, 1e-2));
    }

    #[test]
    fn stepped_zoom_lands_on_the_rungs_and_stops_at_the_ends() {
        let vp = inset_viewport(1.0);
        let anchor = vp.center_pt();
        let mut cam = CanvasCamera::default();
        cam.set_zoom(1.0);
        cam.zoom_in(&vp, anchor);
        assert_eq!(cam.zoom, 1.5);
        cam.zoom_out(&vp, anchor);
        assert_eq!(cam.zoom, 1.0);
        cam.set_zoom(0.9);
        cam.zoom_in(&vp, anchor);
        assert_eq!(cam.zoom, 1.0);
        for _ in 0..100 {
            cam.zoom_in(&vp, anchor);
        }
        assert_eq!(cam.zoom, CanvasCamera::MAX_ZOOM);
        for _ in 0..100 {
            cam.zoom_out(&vp, anchor);
        }
        assert_eq!(cam.zoom, CanvasCamera::MIN_ZOOM);
    }

    #[test]
    fn zoom_steps_are_ascending_and_inside_the_legal_range() {
        for pair in ZOOM_STEPS.windows(2) {
            assert!(pair[1] > pair[0], "{pair:?} is not ascending");
        }
        assert_eq!(ZOOM_STEPS[0], CanvasCamera::MIN_ZOOM);
        assert_eq!(ZOOM_STEPS[ZOOM_STEPS.len() - 1], CanvasCamera::MAX_ZOOM);
        assert!(ZOOM_STEPS.contains(&1.0), "100% must be a rung");
    }

    #[test]
    fn fit_shows_the_whole_document_and_fill_covers_the_viewport() {
        let vp = inset_viewport(2.0);
        let doc = Vec2::new(4000.0, 1000.0);
        let mut cam = CanvasCamera::for_document(doc);

        cam.fit_document(&vp, doc);
        let visible = cam.visible_doc_rect(&vp);
        assert!(visible.min.x <= 0.0 && visible.min.y <= 0.0, "{visible:?}");
        assert!(
            visible.max.x >= doc.x && visible.max.y >= doc.y,
            "{visible:?}"
        );
        let fit_zoom = cam.zoom;

        cam.fill_document(&vp, doc);
        assert!(cam.zoom > fit_zoom, "fill must be tighter than fit");
        let filled = cam.visible_doc_rect(&vp);
        // Filling covers the short axis exactly and crops the long one.
        assert!(filled.min.y <= 0.0 && filled.max.y >= doc.y);
        assert!(filled.min.x > 0.0 || filled.max.x < doc.x);
    }

    #[test]
    fn fit_accounts_for_the_rotated_bounding_box() {
        let vp = inset_viewport(1.0);
        let doc = Vec2::new(1000.0, 1000.0);
        let mut upright = CanvasCamera::for_document(doc);
        upright.fit_document(&vp, doc);
        let mut tilted = CanvasCamera {
            rotation: FRAC_PI_4,
            ..CanvasCamera::for_document(doc)
        };
        tilted.fit_document(&vp, doc);
        assert!(
            tilted.zoom < upright.zoom,
            "a 45-degree view needs more room: {} vs {}",
            tilted.zoom,
            upright.zoom
        );
        // …and the whole document is still on screen.
        let corners = DocRect::of_canvas(doc).corners();
        let bounds = vp.content_bounds_pt().expanded(0.5);
        for c in corners {
            let p = tilted.screen_pt_of(&vp, c);
            assert!(
                bounds.contains(p),
                "corner {c:?} landed off screen at {p:?}"
            );
        }
    }

    #[test]
    fn zooming_to_a_selection_frames_it() {
        let vp = inset_viewport(2.0);
        let mut cam = CanvasCamera::for_document(Vec2::new(4000.0, 3000.0));
        let sel = DocRect::new(Vec2::new(1200.0, 900.0), Vec2::new(1400.0, 1000.0));
        cam.fit_rect(&vp, sel);
        assert!(close(cam.center, sel.center(), 1e-3));
        let visible = cam.visible_doc_rect(&vp);
        assert!(visible.min.x <= sel.min.x && visible.max.x >= sel.max.x);
        assert!(visible.min.y <= sel.min.y && visible.max.y >= sel.max.y);
    }

    #[test]
    fn framing_a_degenerate_target_or_viewport_changes_nothing() {
        let collapsed = Viewport::new(Vec2::splat(100.0), PanelInsets::uniform(100.0), 1.0);
        let mut cam = CanvasCamera::default();
        let before = cam;
        cam.fit_document(&collapsed, Vec2::new(100.0, 100.0));
        assert_eq!(cam, before);

        let vp = inset_viewport(1.0);
        cam.fit_rect(&vp, DocRect::ZERO);
        assert_eq!(cam, before);
    }

    /// Pixel-exactness at 100%: after snapping, an integer document coordinate
    /// lands on an integer physical pixel, for odd and even content sizes and
    /// at fractional display scales.
    #[test]
    fn at_one_hundred_percent_the_pixel_grids_line_up() {
        for (w, h) in [(1200.0, 900.0), (1201.0, 901.0), (999.0, 733.0)] {
            for scale in [1.0_f32, 1.25, 2.0] {
                let vp = Viewport::new(
                    Vec2::new(w, h),
                    PanelInsets::new(261.0, 317.0, 45.0, 29.0),
                    scale,
                );
                let mut cam = CanvasCamera {
                    center: Vec2::new(311.37, 208.91),
                    zoom: 1.0,
                    ..CanvasCamera::default()
                };
                cam.zoom_to_actual_pixels(&vp);
                assert_eq!(cam.zoom, 1.0);
                for doc in [Vec2::ZERO, Vec2::new(37.0, 91.0), Vec2::new(-12.0, 400.0)] {
                    let px = cam.screen_px_of(&vp, doc);
                    assert!(
                        (px.x - px.x.round()).abs() < 1e-3 && (px.y - px.y.round()).abs() < 1e-3,
                        "({w}x{h} @{scale}) doc {doc:?} landed on {px:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn pixel_snapping_leaves_a_rotated_view_alone() {
        let vp = inset_viewport(2.0);
        let mut cam = CanvasCamera {
            center: Vec2::new(10.3, 20.7),
            rotation: 0.4,
            ..CanvasCamera::default()
        };
        let before = cam;
        cam.snap_center_to_pixel_grid(&vp);
        assert_eq!(cam, before);
    }

    #[test]
    fn panning_moves_the_document_with_the_drag() {
        let vp = inset_viewport(2.0);
        let mut cam = CanvasCamera {
            center: Vec2::new(500.0, 500.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let grabbed = cam.doc_of_screen_pt(&vp, vp.center_pt());
        cam.pan_screen_pt(&vp, Vec2::new(30.0, -10.0));
        // The grabbed document point is now 30pt right and 10pt up on screen.
        let now = cam.screen_pt_of(&vp, grabbed);
        assert!(close(now, vp.center_pt() + Vec2::new(30.0, -10.0), 1e-3));
    }

    #[test]
    fn rotation_is_clockwise_on_screen_and_wraps() {
        let vp = Viewport::new(Vec2::splat(400.0), PanelInsets::NONE, 1.0);
        let cam = CanvasCamera {
            center: Vec2::ZERO,
            zoom: 1.0,
            rotation: FRAC_PI_2,
            ..CanvasCamera::default()
        };
        // +x in the document points down the screen after a quarter turn.
        let p = cam.screen_pt_of(&vp, Vec2::new(10.0, 0.0)) - vp.center_pt();
        assert!(close(p, Vec2::new(0.0, 10.0), 1e-3), "{p:?}");

        let mut c = CanvasCamera::default();
        c.set_rotation(3.0 * PI);
        assert!((c.rotation - PI).abs() < 1e-4, "{}", c.rotation);
        c.set_rotation(f32::NAN);
        assert!((c.rotation - PI).abs() < 1e-4);
        c.reset_rotation();
        assert_eq!(c.rotation, 0.0);
    }

    #[test]
    fn flipping_mirrors_the_view_about_the_content_centre() {
        let vp = inset_viewport(1.0);
        let mut cam = CanvasCamera {
            center: Vec2::new(100.0, 100.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let doc = Vec2::new(140.0, 130.0);
        let normal = cam.screen_pt_of(&vp, doc) - vp.center_pt();
        cam.flip_horizontal();
        let flipped = cam.screen_pt_of(&vp, doc) - vp.center_pt();
        assert!(close(flipped, Vec2::new(-normal.x, normal.y), 1e-3));
        cam.flip_vertical();
        let both = cam.screen_pt_of(&vp, doc) - vp.center_pt();
        assert!(close(both, -normal, 1e-3));
        cam.reset_flip();
        assert!(close(
            cam.screen_pt_of(&vp, doc) - vp.center_pt(),
            normal,
            1e-3
        ));
    }

    #[test]
    fn a_flipped_view_still_round_trips_the_cursor() {
        let vp = inset_viewport(1.5);
        let cam = CanvasCamera {
            center: Vec2::new(60.0, 40.0),
            zoom: 4.0,
            rotation: 0.9,
            flip_x: true,
            flip_y: true,
        };
        let pt = vp.origin_pt() + Vec2::new(31.0, 17.0);
        let doc = cam.doc_of_screen_pt(&vp, pt);
        assert!(close(cam.screen_pt_of(&vp, doc), pt, 1e-2));
    }

    /// The shim for a renderer that centres on the whole surface: feeding it
    /// this centre must put the image where the inset viewport says it goes.
    #[test]
    fn the_full_surface_shim_lands_the_image_in_the_inset_viewport() {
        let vp = inset_viewport(2.0);
        let cam = CanvasCamera {
            center: Vec2::new(512.0, 384.0),
            zoom: 1.7,
            rotation: 0.6,
            flip_x: true,
            flip_y: false,
        };
        let rc = cam.render_camera(&vp);
        let shimmed_center = rc.center_for_full_surface();

        // A whole-surface renderer is a camera over a viewport with no insets.
        let full = Viewport::new(vp.surface_pt(), PanelInsets::NONE, vp.pixels_per_point());
        let shimmed = CanvasCamera {
            center: shimmed_center,
            ..cam
        };
        // What the true camera puts at the content centre, the shimmed one puts
        // at the same place on the full surface.
        for doc in [cam.center, Vec2::ZERO, Vec2::new(900.0, 100.0)] {
            let want = cam.screen_px_of(&vp, doc);
            let got = shimmed.screen_px_of(&full, doc);
            assert!(close(got, want, 1e-2), "{doc:?}: {want:?} vs {got:?}");
        }
    }

    #[test]
    fn the_render_camera_reports_the_content_region_not_the_window() {
        let vp = inset_viewport(2.0);
        let rc = CanvasCamera::default().render_camera(&vp);
        assert_eq!(rc.viewport_origin_px, Vec2::new(520.0, 88.0));
        assert_eq!(rc.viewport_size_px, Vec2::new(1240.0, 1656.0));
        assert_eq!(rc.surface_size_px, Vec2::new(2400.0, 1800.0));
    }

    #[test]
    fn the_tools_view_state_bridge_agrees_on_geometry_and_drops_the_flip() {
        let vp = inset_viewport(2.0);
        let cam = CanvasCamera {
            center: Vec2::new(321.0, 123.0),
            zoom: 3.0,
            rotation: 0.5,
            flip_x: true,
            flip_y: false,
        };
        let vs = cam.to_view_state(&vp);
        assert_eq!(vs.viewport, vp.size_px());
        assert_eq!(vs.center, cam.center);
        assert_eq!(vs.zoom, cam.zoom);
        // Same geometry on the way in: a document point maps to the same offset
        // from the viewport centre under both.
        let doc = Vec2::new(400.0, 200.0);
        let ours = cam.screen_px_of(&vp, doc) - vp.center_px();
        let theirs = vs.screen_at(doc) - vs.viewport * 0.5;
        // `ViewState` has no flip, so mirror ours back before comparing.
        assert!(close(Vec2::new(-ours.x, ours.y), theirs, 1e-2));

        let mut back = CanvasCamera::default();
        back.apply_view_state(&vs);
        assert_eq!(back.center, cam.center);
        assert_eq!(back.zoom, cam.zoom);
        assert!(!back.flip_x, "the flip cannot survive the bridge");
    }

    #[test]
    fn a_degenerate_zoom_cannot_produce_a_nan_coordinate() {
        let vp = inset_viewport(1.0);
        let cam = CanvasCamera {
            zoom: 0.0,
            ..CanvasCamera::default()
        };
        assert!(cam.pt_to_doc(&vp).is_none());
        let doc = cam.doc_of_screen_pt(&vp, Vec2::new(10.0, 10.0));
        assert!(doc.is_finite());
        assert_eq!(doc, cam.center);
        assert_eq!(cam.doc_vector_of_screen_pt(&vp, Vec2::ONE), Vec2::ZERO);
    }

    #[test]
    fn set_zoom_clamps_and_refuses_nonsense() {
        let mut cam = CanvasCamera::default();
        cam.set_zoom(f32::NAN);
        assert_eq!(cam.zoom, 1.0);
        cam.set_zoom(1e9);
        assert_eq!(cam.zoom, CanvasCamera::MAX_ZOOM);
        cam.set_zoom(-4.0);
        assert_eq!(cam.zoom, CanvasCamera::MIN_ZOOM);
    }
}
