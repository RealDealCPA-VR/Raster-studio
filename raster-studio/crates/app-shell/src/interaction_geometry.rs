//! Shared coordinate conversion (plan card 009).
//!
//! One place that names every space the shell works in and converts between
//! them, on top of the conversions that already exist — [`ui::CanvasCamera`]
//! for screen↔document, layer transforms for document↔layer — so no caller
//! re-derives the arithmetic and drifts.
//!
//! # The transform convention, stated from the compositor's actual code
//!
//! The compositor applies a layer's own transform to map **layer space into
//! its parent's canvas space** (`composite.rs::render_source`: the layer's
//! `level_transform` resamples the layer's content; a group's transform then
//! maps the group's composited children — who have each applied their own
//! transform — further). So:
//!
//! * transforms are **relative to the parent's canvas**, and the full mapping
//!   from a nested layer's space to the document composes ancestor transforms
//!   root-first ([`document_transform_of`]);
//! * a group never re-applies a child's delta (the child's own transform is
//!   the child→parent mapping; the group's is parent→grandparent), which is
//!   the "never apply both twice" rule the plan card names;
//! * a mask follows its layer's transform when *linked*, and stays in
//!   document space when not ([`document_to_mask`]). The compositor applies
//!   exactly this split in `render_source`: linked coverage multiplies the
//!   pre-transform content, unlinked coverage multiplies the post-transform
//!   result.
//!
//! # Rejections
//!
//! A singular layer transform (zero scale, collapsed axis) has no inverse, so
//! a document→layer conversion returns [`GeometryError::SingularTransform`]
//! instead of scattering infinities. Screen↔document conversion cannot fail:
//! the camera's own `doc_of_screen_pt` falls back to the camera centre when
//! degenerate, so callers never see a NaN.

use editor_core::Document;
use glam::Affine2;
use layer_model::{Layer, LayerId, LayerMask};
use ui::canvas::{CanvasCamera, Viewport};

/// Why a conversion refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeometryError {
    /// The transform's linear part has no inverse (zero determinant).
    SingularTransform,
}

impl std::fmt::Display for GeometryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GeometryError::SingularTransform => {
                write!(
                    f,
                    "the layer's transform is singular and cannot be inverted"
                )
            }
        }
    }
}

impl std::error::Error for GeometryError {}

/// The interaction camera for a document camera: the [`ui::CanvasCamera`] that
/// puts every screen point on the document pixel [`render::Camera`] is
/// drawing there.
///
/// This is the one mirror that carries the **view rotation**. Both cameras
/// measure it positive-clockwise on screen about the viewport centre
/// (`render::camera`'s module docs state the convention), so the angle is
/// copied straight across, and a click, a handle, a marching-ants corner or a
/// text caret converted through the result lands on the pixel the user is
/// looking at when the view is turned. A mirror that zeroes the rotation
/// puts every overlay where the *upright* view would show the pixel — a
/// quarter turn away from the picture.
///
/// The **view mirror** is carried the same way: `render::Camera::flip_x` /
/// `flip_y` and `CanvasCamera`'s flags mean the same on-screen mirror about
/// the viewport centre, applied after the rotation (`F · S · R`), so a click
/// on a flipped view lands on the mirrored pixel the renderer draws there.
/// (In this build the running app never sets the flags: View > Flip is still
/// greyed by the chrome. See `tool_input`'s module docs.)
///
/// It is the only mirror: `tool_input::canvas_camera_of`, which the pointer
/// router, the shell, the chrome and the dialog host call, delegates here.
pub fn canvas_camera_of(camera: &render::Camera) -> CanvasCamera {
    CanvasCamera {
        center: camera.center,
        zoom: camera.zoom,
        rotation: camera.rotation,
        flip_x: camera.flip_x,
        flip_y: camera.flip_y,
    }
}

/// The canvas area a document camera draws into: the rectangle between the
/// docks, strips and tool column, in **surface pixels** measured from the
/// window's top-left corner.
///
/// It is the one fact every space conversion in the shell shares with the
/// renderer: `render::Camera::viewport_origin` / `viewport_size` hold it,
/// [`render::Canvas::render_in`] draws into exactly it, and
/// [`CanvasArea::viewport`] turns it into the [`ui::canvas::Viewport`] the
/// pointer route and every overlay map through. The shell learns it from the
/// chrome's previous frame (`Chrome::canvas_area_px`) and hands it to each
/// document with `Chrome::place_canvas` ([`CanvasPlacement`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CanvasArea {
    /// Top-left of the area, in surface pixels.
    pub origin: glam::Vec2,
    /// Size of the area, in surface pixels.
    pub size: glam::Vec2,
}

impl CanvasArea {
    /// The area a camera currently draws into.
    pub fn of_camera(camera: &render::Camera) -> Self {
        Self {
            origin: camera.viewport_origin,
            size: camera.viewport_size,
        }
    }

    /// The whole surface: the fallback before any chrome has been laid out.
    pub fn whole(surface_px: glam::Vec2) -> Self {
        Self {
            origin: glam::Vec2::ZERO,
            size: surface_px,
        }
    }

    /// This area cut back to a `surface_px` surface, or `None` when nothing of
    /// it is left: an area measured in a bigger window (the frame before a
    /// resize) must not reach past the new surface's edge.
    pub fn within(&self, surface_px: glam::Vec2) -> Option<Self> {
        let origin = self.origin.clamp(glam::Vec2::ZERO, surface_px);
        let far = (self.origin + self.size).min(surface_px);
        let size = far - origin;
        (size.x >= 1.0 && size.y >= 1.0).then_some(Self { origin, size })
    }

    /// The middle of the area, in surface pixels: where the camera centre is
    /// drawn.
    pub fn center(&self) -> glam::Vec2 {
        self.origin + self.size * 0.5
    }

    /// The [`Viewport`] this area is, at `ppp` physical pixels per point.
    ///
    /// The surface handed to the viewport ends at the area's far corner and
    /// the area's top-left becomes the left/top insets, so
    /// [`Viewport::center_pt`] is the area's centre and nothing downstream has
    /// to know where the docks are. With `ppp == 1.0` (the pointer route and
    /// the shell's overlays) points *are* surface pixels; the chrome's
    /// painters pass their display scale and get points.
    pub fn viewport(&self, ppp: f32) -> Viewport {
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        let origin = self.origin / ppp;
        let far = (self.origin + self.size) / ppp;
        Viewport::new(
            far,
            ui::canvas::PanelInsets::new(origin.x, 0.0, origin.y, 0.0),
            ppp,
        )
    }
}

impl From<glam::Vec2> for CanvasArea {
    fn from(size: glam::Vec2) -> Self {
        Self::whole(size)
    }
}

impl From<&render::Camera> for CanvasArea {
    fn from(camera: &render::Camera) -> Self {
        Self::of_camera(camera)
    }
}

/// Hands documents the canvas area they are drawn in, and keeps a fresh
/// document's opening fit honest while the chrome's layout settles.
///
/// * The camera's `viewport_origin` / `viewport_size` become the area, through
///   [`crate::doc::OpenDocument::set_viewport`], so the first-frame fit (and
///   every later View > Fit / Fill / 100%) is against the canvas area and not
///   the window.
/// * The opening fit is **provisional** until a measured area arrives that is
///   the same as the one it was fitted to. Before the chrome has laid out a
///   frame the shell only knows the surface (`measured == false`), and egui's
///   first frames lay the docks out wider or narrower than they settle at; a
///   fit made against either is redone against the next area, until the area
///   holds still. A fit the user has already moved (zoomed, panned) is never
///   redone.
/// * Once settled, moving the area — a window resize, Tab hiding the panels, a
///   screen mode — never re-fits and never moves `center`: the document point
///   in the middle of the canvas area stays in the middle of it.
#[derive(Debug, Default)]
pub struct CanvasPlacement {
    provisional: std::collections::HashMap<crate::doc::DocumentId, ProvisionalFit>,
}

/// What a provisional opening fit was made against, and what it produced.
#[derive(Debug, Clone, Copy)]
struct ProvisionalFit {
    area: CanvasArea,
    zoom: f32,
    center: glam::Vec2,
}

impl CanvasPlacement {
    /// Place `doc` in `area`; see the type's docs for `measured`.
    pub fn place(&mut self, doc: &mut crate::doc::OpenDocument, area: CanvasArea, measured: bool) {
        if !(area.size.x >= 1.0 && area.size.y >= 1.0)
            || !area.size.is_finite()
            || !area.origin.is_finite()
        {
            return;
        }
        let was_pending = doc.awaiting_fit();
        doc.camera.viewport_origin = area.origin;
        doc.set_viewport(area.size);
        let id = doc.id();
        let remember = |doc: &crate::doc::OpenDocument| ProvisionalFit {
            area,
            zoom: doc.camera.zoom,
            center: doc.camera.center,
        };
        if was_pending {
            // `set_viewport` just made the opening fit.
            self.provisional.insert(id, remember(doc));
            return;
        }
        let Some(fit) = self.provisional.get(&id).copied() else {
            return;
        };
        if doc.camera.zoom != fit.zoom || doc.camera.center != fit.center {
            // The user moved the view: the fit is theirs now.
            self.provisional.remove(&id);
        } else if fit.area == area {
            if measured {
                // The layout held still: the fit is final.
                self.provisional.remove(&id);
            }
        } else {
            // The same fit `set_viewport` performs on a first placement:
            // opening never enlarges.
            doc.camera.fit();
            doc.camera.zoom = doc.camera.zoom.min(1.0);
            self.provisional.insert(id, remember(doc));
        }
    }
}

/// The document point under a screen position (through the canvas camera).
pub fn screen_to_document(
    camera: &CanvasCamera,
    viewport: &Viewport,
    pt: glam::Vec2,
) -> glam::Vec2 {
    camera.doc_of_screen_pt(viewport, pt)
}

/// Where a document point lands on screen (through the canvas camera).
pub fn document_to_screen(
    camera: &CanvasCamera,
    viewport: &Viewport,
    doc: glam::Vec2,
) -> glam::Vec2 {
    camera.screen_pt_of(viewport, doc)
}

/// A layer's full document mapping: the composition of its own transform with
/// every ancestor's, root first — what the compositor applies when it renders
/// the layer (each layer's transform maps layer space into its parent's
/// canvas). A singular ancestor or self makes the whole chain singular; the
/// error is the caller's to refuse, not a NaN to propagate.
pub fn document_transform_of(
    doc: &Document,
    layer: LayerId,
    level: u8,
) -> Result<Affine2, GeometryError> {
    // Ancestor chain, leaf first, then compose root→leaf.
    let mut chain: Vec<LayerId> = vec![layer];
    let mut parent = doc.layers.parent_of(layer);
    while let Some(p) = parent {
        chain.push(p);
        parent = doc.layers.parent_of(p);
    }
    let mut total = Affine2::IDENTITY;
    for id in chain.iter().rev() {
        let Some(l) = doc.layers.get(*id) else {
            return Ok(total);
        };
        let t = level_transform_of(l, level);
        total *= t;
    }
    if is_singular(&total) {
        return Err(GeometryError::SingularTransform);
    }
    Ok(total)
}

/// Document point → the layer's local space, through the layer's full
/// document mapping (ancestors composed, per [`document_transform_of`]).
pub fn document_to_layer(
    doc: &Document,
    layer: LayerId,
    level: u8,
    doc_pt: glam::Vec2,
) -> Result<glam::Vec2, GeometryError> {
    let total = document_transform_of(doc, layer, level)?;
    Ok(total.inverse().transform_point2(doc_pt))
}

/// Layer-local point → document space.
pub fn layer_to_document(
    doc: &Document,
    layer: LayerId,
    level: u8,
    local_pt: glam::Vec2,
) -> Result<glam::Vec2, GeometryError> {
    let total = document_transform_of(doc, layer, level)?;
    Ok(total.transform_point2(local_pt))
}

/// Which space a mask's coverage lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaskSpace {
    /// The mask moves with the layer: coverage is sampled in layer space,
    /// before the layer's transform (the compositor multiplies it into the
    /// pre-transform content).
    WithContent,
    /// The mask stays put in document space while the content moves under it:
    /// coverage is sampled in document space, after the layer's transform.
    Document,
}

/// The space the mask's coverage is read in — the compositor's own split in
/// `render_source` (`mask_linked`).
pub fn mask_space_of(mask: &LayerMask) -> MaskSpace {
    if mask.linked {
        MaskSpace::WithContent
    } else {
        MaskSpace::Document
    }
}

/// A document point → the coordinates to sample the layer's mask coverage at.
///
/// A linked mask shares the content's space (the layer transform applies); an
/// unlinked mask lives in document space, so the point passes through
/// unchanged. The layer's own singular transform only matters to the linked
/// half.
pub fn document_to_mask(
    doc: &Document,
    layer: LayerId,
    level: u8,
    mask: &LayerMask,
    doc_pt: glam::Vec2,
) -> Result<glam::Vec2, GeometryError> {
    match mask_space_of(mask) {
        MaskSpace::Document => Ok(doc_pt),
        MaskSpace::WithContent => {
            // The content's mapping, without the mask's own chain: the mask
            // rides the layer it is attached to.
            mask_content_transform(doc, layer, level).map(|t| t.inverse().transform_point2(doc_pt))
        }
    }
}

/// The content transform for mask sampling: ancestors composed, but the layer
/// mask never adds a transform of its own.
fn mask_content_transform(
    doc: &Document,
    layer: LayerId,
    level: u8,
) -> Result<Affine2, GeometryError> {
    document_transform_of(doc, layer, level)
}

/// The layer's transform conjugated to mip-level space — exactly the
/// compositor's convention (`Ctx::level_transform`): authored in level-0
/// parent pixels; at level L a uniform `2^-L` similarity conjugation; a
/// non-finite transform is the identity.
fn level_transform_of(layer: &Layer, level: u8) -> Affine2 {
    let m = layer.transform;
    if !m.to_cols_array().iter().all(|v| v.is_finite()) {
        return Affine2::IDENTITY;
    }
    if level == 0 {
        return m;
    }
    let s = 2.0f32.powi(-(level as i32));
    Affine2::from_scale(glam::Vec2::splat(s)) * m * Affine2::from_scale(glam::Vec2::splat(1.0 / s))
}

fn is_singular(t: &Affine2) -> bool {
    let det = t.x_axis.x * t.y_axis.y - t.x_axis.y * t.y_axis.x;
    !det.is_finite() || det.abs() <= f32::EPSILON
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::Vec2;
    use ui::canvas::camera::CanvasCamera;

    /// A camera centred on `center` over a viewport, unrotated, unflipped.
    fn camera(center: Vec2, zoom: f32) -> CanvasCamera {
        CanvasCamera {
            center,
            zoom,
            ..Default::default()
        }
    }

    fn viewport(w: f32, h: f32, pixels_per_point: f32) -> Viewport {
        Viewport::from_content_rect(
            glam::vec2(w, h),
            egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, h)),
            pixels_per_point,
        )
    }

    #[test]
    fn screen_document_round_trips_at_three_zooms() {
        let vp = viewport(400.0, 300.0, 1.0);
        for zoom in [1.0 / 3.0, 1.0, 2.0] {
            let cam = camera(Vec2::new(256.0, 144.0), zoom);
            let doc = Vec2::new(310.5, 77.25);
            let screen = document_to_screen(&cam, &vp, doc);
            let back = screen_to_document(&cam, &vp, screen);
            assert!(
                (back - doc).length() < 1e-3,
                "zoom {zoom}: {doc:?} -> {screen:?} -> {back:?}"
            );
        }
    }

    /// A rotated view still round-trips: screen → document → screen is the
    /// identity at 45°, and so is document → screen → document.
    #[test]
    fn screen_document_round_trips_on_a_view_turned_45_degrees() {
        let vp = viewport(400.0, 300.0, 1.0);
        let mut cam = camera(Vec2::new(256.0, 144.0), 1.5);
        cam.set_rotation(std::f32::consts::FRAC_PI_4);
        for doc in [
            Vec2::new(310.5, 77.25),
            Vec2::new(0.0, 0.0),
            Vec2::new(256.0, 144.0),
        ] {
            let screen = document_to_screen(&cam, &vp, doc);
            let back = screen_to_document(&cam, &vp, screen);
            assert!(
                (back - doc).length() < 1e-3,
                "{doc:?} -> {screen:?} -> {back:?}"
            );
        }
        for screen in [Vec2::new(10.0, 290.0), Vec2::new(200.0, 150.0)] {
            let doc = screen_to_document(&cam, &vp, screen);
            let back = document_to_screen(&cam, &vp, doc);
            assert!(
                (back - screen).length() < 1e-3,
                "{screen:?} -> {doc:?} -> {back:?}"
            );
        }
    }

    /// The interaction camera built from the document camera carries its
    /// rotation, and agrees with the renderer about where every screen pixel
    /// is: `screen_to_document` through the mirror is `render::Camera::
    /// screen_to_image` on the original, at 45° as at 0°. A mirror that
    /// dropped the angle would put a click a quarter turn from the pixel.
    #[test]
    fn the_interaction_camera_agrees_with_the_render_camera_when_turned() {
        let surface = Vec2::new(400.0, 300.0);
        let mut render_cam = render::Camera::new(Vec2::new(512.0, 288.0), surface);
        render_cam.zoom = 1.5;
        render_cam.center = Vec2::new(256.0, 144.0);
        for angle in [
            0.0,
            std::f32::consts::FRAC_PI_4,
            std::f32::consts::FRAC_PI_2,
            -2.3,
        ] {
            render_cam.set_rotation(angle);
            let mirror = canvas_camera_of(&render_cam);
            assert_eq!(
                mirror.rotation, render_cam.rotation,
                "the mirror dropped the angle"
            );
            assert_eq!(mirror.center, render_cam.center);
            assert_eq!(mirror.zoom, render_cam.zoom);
            let vp = crate::tool_input::canvas_viewport(surface);
            for screen in [
                Vec2::new(0.0, 0.0),
                Vec2::new(400.0, 300.0),
                Vec2::new(37.0, 250.0),
                Vec2::new(200.0, 150.0),
            ] {
                let via_ui = screen_to_document(&mirror, &vp, screen);
                let via_render = render_cam.screen_to_image(screen);
                assert!(
                    (via_ui - via_render).length() < 1e-2,
                    "angle {angle}: screen {screen:?} is document {via_ui:?} to the \
                     interaction camera and {via_render:?} to the renderer"
                );
            }
        }
        // And the turn is a real turn: on a quarter-turned view, the pixel
        // right of the window's centre is the document ABOVE the camera
        // centre.
        render_cam.set_rotation(std::f32::consts::FRAC_PI_2);
        let vp = crate::tool_input::canvas_viewport(surface);
        let right = screen_to_document(
            &canvas_camera_of(&render_cam),
            &vp,
            surface * 0.5 + Vec2::new(60.0, 0.0),
        );
        assert!(
            (right.x - 256.0).abs() < 1e-2 && (right.y - (144.0 - 40.0)).abs() < 1e-2,
            "screen right of centre is document {right:?}, expected (256, 104)"
        );
    }

    /// A mirrored view: the interaction camera carries both flip flags, a
    /// pointer hit lands on the mirrored pixel (the one `render::Camera`
    /// draws there), and screen -> document -> screen round-trips, with a
    /// turn in force as well.
    #[test]
    fn the_interaction_camera_honours_the_view_mirror() {
        let surface = Vec2::new(400.0, 300.0);
        let vp = crate::tool_input::canvas_viewport(surface);
        let mut render_cam = render::Camera::new(Vec2::new(512.0, 288.0), surface);
        render_cam.zoom = 2.0;
        render_cam.center = Vec2::new(256.0, 144.0);
        for (fx, fy) in [(true, false), (false, true), (true, true)] {
            for angle in [0.0, 0.6, std::f32::consts::FRAC_PI_2] {
                render_cam.flip_x = fx;
                render_cam.flip_y = fy;
                render_cam.set_rotation(angle);
                let mirror = canvas_camera_of(&render_cam);
                assert_eq!((mirror.flip_x, mirror.flip_y), (fx, fy));
                for screen in [
                    Vec2::new(0.0, 0.0),
                    Vec2::new(37.0, 250.0),
                    Vec2::new(260.0, 110.0),
                ] {
                    let via_ui = screen_to_document(&mirror, &vp, screen);
                    let via_render = render_cam.screen_to_image(screen);
                    assert!(
                        (via_ui - via_render).length() < 1e-2,
                        "flip ({fx},{fy}) angle {angle}: screen {screen:?} is {via_ui:?} \
                         to the pointer and {via_render:?} to the renderer"
                    );
                    let back = document_to_screen(&mirror, &vp, via_ui);
                    assert!((back - screen).length() < 1e-2, "{screen:?} -> {back:?}");
                }
            }
        }
        // The hit really is mirrored: upright, flip H, a pointer 60 px right
        // of the window centre is the document 30 px LEFT of the camera
        // centre.
        render_cam.set_rotation(0.0);
        render_cam.flip_x = true;
        render_cam.flip_y = false;
        let hit = screen_to_document(
            &canvas_camera_of(&render_cam),
            &vp,
            surface * 0.5 + Vec2::new(60.0, 0.0),
        );
        assert!(
            (hit - Vec2::new(226.0, 144.0)).length() < 1e-2,
            "the hit is not mirrored: {hit:?}"
        );
    }

    #[test]
    fn screen_document_round_trips_at_three_display_scales() {
        // The same document point at three Windows display scalings: the
        // camera answers in points, the viewport carries the pixels-per-point.
        for ppp in [1.0, 1.5, 2.0] {
            let vp = viewport(400.0, 300.0, ppp);
            let cam = camera(Vec2::new(256.0, 144.0), 1.0);
            let doc = Vec2::new(310.5, 77.25);
            let screen = document_to_screen(&cam, &vp, doc);
            let back = screen_to_document(&cam, &vp, screen);
            assert!(
                (back - doc).length() < 1e-3,
                "ppp {ppp}: {doc:?} -> {screen:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn a_scaled_translated_layer_round_trips_through_local_space() {
        let mut doc = editor_core::Document::new(512, 512, "geo");
        let id = doc
            .layers
            .push_root(layer_model::Layer::raster("Scaled"))
            .unwrap();
        doc.layers.get_mut(id).unwrap().transform =
            Affine2::from_translation(Vec2::new(-300.0, 40.0))
                * Affine2::from_scale(Vec2::splat(2.0));

        for level in [0u8, 1, 2] {
            let doc_pt = Vec2::new(123.4, 210.7);
            let local = document_to_layer(&doc, id, level, doc_pt).unwrap();
            let back = layer_to_document(&doc, id, level, local).unwrap();
            assert!(
                (back - doc_pt).length() < 1e-3,
                "level {level}: {doc_pt:?} -> {local:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn a_singular_transform_is_refused_not_nan() {
        let mut doc = editor_core::Document::new(64, 64, "singular");
        let id = doc
            .layers
            .push_root(layer_model::Layer::raster("L"))
            .unwrap();
        doc.layers.get_mut(id).unwrap().transform = Affine2::from_scale(Vec2::splat(0.0));
        assert_eq!(
            document_to_layer(&doc, id, 0, Vec2::new(10.0, 10.0)),
            Err(GeometryError::SingularTransform),
        );
    }

    #[test]
    fn a_nested_child_composes_its_ancestors_transforms_once() {
        let mut doc = editor_core::Document::new(2048, 2048, "nested");
        let group = doc
            .layers
            .push_root(layer_model::Layer::group("Group"))
            .unwrap();
        let child = doc
            .layers
            .insert_at(layer_model::Layer::raster("Child"), Some(group), 0)
            .unwrap();
        doc.layers.get_mut(group).unwrap().transform =
            Affine2::from_translation(Vec2::new(100.0, 0.0));
        doc.layers.get_mut(child).unwrap().transform =
            Affine2::from_translation(Vec2::new(0.0, 50.0));

        // Composed exactly once: group ∘ child, never child twice.
        let total = document_transform_of(&doc, child, 0).unwrap();
        let mapped = total.transform_point2(Vec2::ZERO);
        assert!(
            (mapped - Vec2::new(100.0, 50.0)).length() < 1e-4,
            "{mapped:?}"
        );

        // And the group itself maps by its own transform alone.
        let group_total = document_transform_of(&doc, group, 0).unwrap();
        assert!((group_total.transform_point2(Vec2::ZERO) - Vec2::new(100.0, 0.0)).length() < 1e-4);
    }

    #[test]
    fn a_linked_mask_samples_in_content_space_an_unlinked_one_in_document_space() {
        let mut doc = editor_core::Document::new(512, 512, "masks");
        let id = doc
            .layers
            .push_root(layer_model::Layer::raster("Masked"))
            .unwrap();
        doc.layers.get_mut(id).unwrap().transform = Affine2::from_translation(Vec2::new(40.0, 0.0));
        let doc_pt = Vec2::new(40.0, 10.0);

        let mut linked = layer_model::LayerMask::new(layer_model::MaskId::new());
        linked.linked = true;
        let mut unlinked = layer_model::LayerMask::new(layer_model::MaskId::new());
        unlinked.linked = false;

        assert_eq!(mask_space_of(&linked), MaskSpace::WithContent);
        assert_eq!(mask_space_of(&unlinked), MaskSpace::Document);

        // Linked: the point maps through the layer transform (40 subtracts).
        let in_mask = document_to_mask(&doc, id, 0, &linked, doc_pt).unwrap();
        assert!(
            (in_mask - Vec2::new(0.0, 10.0)).length() < 1e-4,
            "{in_mask:?}"
        );
        // Unlinked: the mask reads document space unchanged.
        let unchanged = document_to_mask(&doc, id, 0, &unlinked, doc_pt).unwrap();
        assert!((unchanged - doc_pt).length() < 1e-6, "{unchanged:?}");
    }

    fn open_doc(width: u32, height: u32) -> crate::doc::OpenDocument {
        let image = crate::import::DecodedImage {
            width,
            height,
            rgba8: vec![128; (width as usize) * (height as usize) * 4],
            color_space: color::ColorSpace::Srgb,
            icc_profile: None,
        };
        let imported = crate::import::document_from_image(&image, "t.png", 100).unwrap();
        crate::doc::OpenDocument::from_import(crate::doc::DocumentId(7), imported)
    }

    /// W5-A: a camera whose canvas area starts inside the window maps a screen
    /// point exactly as the renderer does, through the viewport the pointer
    /// and every overlay use — and the area's centre is the camera centre.
    #[test]
    fn the_canvas_area_viewport_agrees_with_the_render_camera() {
        let mut cam = render::Camera::new(Vec2::new(320.0, 180.0), Vec2::new(760.0, 780.0));
        cam.viewport_origin = Vec2::new(44.0, 70.0);
        cam.zoom = 1.25;
        cam.center = Vec2::new(150.0, 100.0);
        let vp = crate::tool_input::canvas_viewport(&cam);
        assert_eq!(vp.center_pt(), Vec2::new(44.0 + 380.0, 70.0 + 390.0));
        let mirror = canvas_camera_of(&cam);
        for angle in [0.0, 0.7] {
            cam.set_rotation(angle);
            let mirror = CanvasCamera {
                rotation: cam.rotation,
                ..mirror
            };
            for screen in [
                Vec2::new(44.0, 70.0),
                Vec2::new(424.0, 460.0),
                Vec2::new(700.0, 101.0),
            ] {
                let via_ui = screen_to_document(&mirror, &vp, screen);
                let via_render = cam.screen_to_image(screen);
                assert!(
                    (via_ui - via_render).length() < 1e-2,
                    "{screen:?}: {via_ui:?} vs {via_render:?}"
                );
            }
        }
        cam.set_rotation(0.0);
        assert!((cam.screen_to_image(Vec2::new(424.0, 460.0)) - cam.center).length() < 1e-4);
        // The chrome's point-space viewport is the same area at a display
        // scale: its centre in points times the scale is the centre in pixels.
        let area = CanvasArea::of_camera(&cam);
        let pts = area.viewport(2.0);
        assert!((pts.center_pt() * 2.0 - area.center()).length() < 1e-4);
    }

    /// W5-A: the first placement with a measured area fits the document to
    /// that area; moving the area afterwards (a resize, Tab) neither re-fits
    /// nor moves the centre.
    #[test]
    fn a_measured_area_fits_once_and_a_moved_area_keeps_the_centre() {
        let mut doc = open_doc(3628, 2041);
        let mut placement = CanvasPlacement::default();
        let area = CanvasArea {
            origin: Vec2::new(44.0, 70.0),
            size: Vec2::new(760.0, 790.0),
        };
        placement.place(&mut doc, area, true);
        assert!(!doc.awaiting_fit());
        assert!((doc.camera.zoom - 760.0 / 3628.0).abs() < 1e-6);
        assert_eq!(doc.camera.viewport_origin, area.origin);
        doc.camera.zoom = 0.5;
        doc.camera.center = Vec2::new(1000.0, 700.0);
        let wider = CanvasArea::whole(Vec2::new(1440.0, 900.0));
        placement.place(&mut doc, wider, true);
        assert_eq!(doc.camera.zoom, 0.5);
        assert_eq!(doc.camera.center, Vec2::new(1000.0, 700.0));
        assert!((doc.camera.screen_to_image(wider.center()) - doc.camera.center).length() < 1e-3);
    }

    /// W5-A: before the chrome has laid out a frame only the surface is
    /// known. The document is fitted to it provisionally, and fitted once more
    /// — to the canvas area — the first time a measured area arrives; never
    /// again after that.
    #[test]
    fn a_fit_made_against_the_surface_is_redone_once_against_the_canvas_area() {
        let mut doc = open_doc(3628, 2041);
        let mut placement = CanvasPlacement::default();
        placement.place(&mut doc, CanvasArea::whole(Vec2::new(1440.0, 900.0)), false);
        assert!((doc.camera.zoom - 1440.0 / 3628.0).abs() < 1e-6);
        let area = CanvasArea {
            origin: Vec2::new(44.0, 70.0),
            size: Vec2::new(760.0, 790.0),
        };
        placement.place(&mut doc, area, true);
        assert!(
            (doc.camera.zoom - 760.0 / 3628.0).abs() < 1e-6,
            "the provisional fit was not redone against the canvas area: {}",
            doc.camera.zoom
        );
        doc.camera.zoom = 0.9;
        placement.place(&mut doc, area, true);
        assert_eq!(
            doc.camera.zoom, 0.9,
            "a second measured placement re-fitted"
        );
    }

    /// W5-A: egui's first frames lay the docks out at a different width than
    /// they settle at. The opening fit follows the area until it holds still
    /// for one placement, and then never again: a later move (Tab, a resize)
    /// keeps the zoom.
    #[test]
    fn the_opening_fit_follows_the_layout_until_it_settles() {
        let mut doc = open_doc(3628, 2041);
        let mut placement = CanvasPlacement::default();
        let early = CanvasArea {
            origin: Vec2::new(44.0, 76.0),
            size: Vec2::new(856.0, 800.0),
        };
        let settled = CanvasArea {
            origin: Vec2::new(44.0, 76.0),
            size: Vec2::new(769.0, 800.0),
        };
        placement.place(&mut doc, early, true);
        assert!((doc.camera.zoom - 856.0 / 3628.0).abs() < 1e-6);
        placement.place(&mut doc, settled, true);
        assert!(
            (doc.camera.zoom - 769.0 / 3628.0).abs() < 1e-6,
            "the fit stayed at the unsettled layout's {}",
            doc.camera.zoom
        );
        placement.place(&mut doc, settled, true);
        let zoom = doc.camera.zoom;
        placement.place(&mut doc, CanvasArea::whole(Vec2::new(1440.0, 900.0)), true);
        assert_eq!(doc.camera.zoom, zoom, "a settled fit was redone");
    }

    /// An area measured in a bigger window is cut back to the surface.
    #[test]
    fn an_area_is_cut_back_to_the_surface() {
        let area = CanvasArea {
            origin: Vec2::new(40.0, 60.0),
            size: Vec2::new(1000.0, 800.0),
        };
        let cut = area.within(Vec2::new(800.0, 600.0)).unwrap();
        assert_eq!(cut.origin, Vec2::new(40.0, 60.0));
        assert_eq!(cut.size, Vec2::new(760.0, 540.0));
        assert!(area.within(Vec2::new(30.0, 30.0)).is_none());
    }
}
