//! Drawing the overlays.
//!
//! Every function here is thin on purpose: the geometry has already been
//! decided by the modules beside this one, and every colour and width comes out
//! of [`CanvasStyle`], which comes out of `design`. Nothing in this file writes
//! a literal colour, radius or spacing.
//!
//! The image is **not** drawn here. It is composited onto the surface by the
//! renderer before egui runs, so everything below is drawn on top of it — which
//! is why [`backdrop`] paints a hole rather than a sheet. That is not a detail:
//! an opaque fill across the content area would erase the document, and the
//! only thing that would notice is the user.
//!
//! Painting order matters and is fixed: backdrop, grid, pixel grid, guides,
//! smart guides, layer edges, then the content overlays (crop scrim first,
//! because it dims the image), then the selection ants, the transform box,
//! the path furniture,
//! the text caret, and last of all the brush ring — which must sit on top of
//! everything, because it is the cursor.

use glam::Vec2;
use tools::transform::{Handle, TransformMode, TransformState};

use super::ants::AntsGeometry;
use super::brush_cursor::BrushCursor;
use super::camera::CanvasCamera;
use super::crop::CropOverlay;
use super::geom::{to_pos2, Axis, DocRect};
use super::grid::{self, GridSettings};
use super::handles::{self, HandleLayout};
use super::paths::PathOverlay;
use super::rulers::{self, Guides, RulerMapping, RulerSpec, TickKind};
use super::snapping::SnapHit;
use super::style::CanvasStyle;
use super::text_overlay::TextOverlay;
use super::viewport::Viewport;

/// The screen-point bounding box of the projected document.
///
/// Under a rotated view the image is a quad, not a rectangle, so all four
/// corners are projected and the box drawn round them. `None` when there is no
/// document, or when the camera is degenerate enough to send a corner to
/// infinity.
pub fn document_bounds_pt(
    camera: &CanvasCamera,
    viewport: &Viewport,
    doc_size: Vec2,
) -> Option<DocRect> {
    let doc = DocRect::of_canvas(doc_size);
    if doc.is_empty() {
        return None;
    }
    let corners = [
        Vec2::new(doc.min.x, doc.min.y),
        Vec2::new(doc.max.x, doc.min.y),
        Vec2::new(doc.max.x, doc.max.y),
        Vec2::new(doc.min.x, doc.max.y),
    ]
    .map(|c| camera.screen_pt_of(viewport, c));
    if !corners.iter().all(|p| p.is_finite()) {
        return None;
    }
    DocRect::of_points(&corners)
}

/// The bands of the content area that the image does **not** cover.
///
/// The image is composited onto the surface by the renderer *before* egui runs,
/// so the backdrop must be a hole, not a sheet: an opaque rectangle over the
/// content area erases the document. egui has no even-odd fill, so the hole is
/// four rectangles — the same decomposition [`crate::canvas::crop`] uses for the
/// crop scrim, called here directly so the two cannot drift apart.
///
/// With the view rotated the bands are cut against the image's *bounding box*,
/// which leaves the four triangles between the quad and its box unfilled. That
/// is deliberate: the renderer clears the whole surface to the same
/// `BackgroundCanvas` token first, so those triangles are already the right
/// colour, and cutting them exactly would need the even-odd fill egui does not
/// have. Painting over them would risk covering the image, which is the bug
/// this function exists to prevent.
pub fn backdrop_bands(
    camera: &CanvasCamera,
    viewport: &Viewport,
    doc_size: Vec2,
) -> Vec<egui::Rect> {
    let outer = viewport.content_bounds_pt();
    if viewport.is_degenerate() {
        return Vec::new();
    }
    match document_bounds_pt(camera, viewport, doc_size) {
        // No document on screen: the whole content area is backdrop.
        None => vec![viewport.content_rect()],
        Some(image) => super::crop::scrim_bands(outer, image),
    }
}

/// Fill the area around the image, and never over it.
pub fn backdrop(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    doc_size: Vec2,
    style: &CanvasStyle,
) {
    for band in backdrop_bands(camera, viewport, doc_size) {
        painter.rect_filled(band, egui::Rounding::ZERO, style.backdrop);
    }
}

/// The document grid and, at high zoom, the pixel grid.
///
/// Returns whether the grid was asked for but is too dense at this zoom to be
/// legible, so the caller can *say* the grid is hidden rather than appear to
/// ignore the setting — see [`crate::canvas::grid::GridLines::suppressed`].
pub fn grid(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    settings: &GridSettings,
    canvas: DocRect,
    style: &CanvasStyle,
) -> bool {
    let visible = camera.visible_doc_rect(viewport);
    let stroke_line = |axis: Axis, value: f32, stroke: egui::Stroke| {
        let (a, b) = grid::line_endpoints(visible, axis, value);
        painter.line_segment(
            [
                to_pos2(camera.screen_pt_of(viewport, a)),
                to_pos2(camera.screen_pt_of(viewport, b)),
            ],
            stroke,
        );
    };

    let mut suppressed = false;
    for axis in Axis::ALL {
        let lines = grid::grid_lines(camera, viewport, settings, *axis);
        suppressed |= lines.suppressed;
        for v in &lines.minor {
            stroke_line(*axis, *v, style.hairline(style.grid_minor));
        }
        for v in &lines.major {
            stroke_line(*axis, *v, style.hairline(style.grid_major));
        }
        for v in grid::pixel_grid_lines(camera, viewport, settings, *axis, canvas) {
            stroke_line(*axis, v, style.hairline(style.pixel_grid));
        }
    }
    suppressed
}

/// The fill each ruler gutter takes, top first.
///
/// A gutter whose edge cannot read a document coordinate — the view is rotated
/// off-axis — is filled with [`CanvasStyle::ruler_disabled`] instead of the
/// ordinary panel token, so an empty ruler looks switched off rather than
/// broken. The sentence that explains it is
/// [`crate::canvas::rulers::oblique_hint`], shown on hover by
/// [`crate::canvas::CanvasView::show`].
pub fn gutter_fills(
    camera: &CanvasCamera,
    viewport: &Viewport,
    style: &CanvasStyle,
) -> [egui::Color32; 2] {
    [Axis::X, Axis::Y].map(|axis| {
        if rulers::ruler_mapping(camera, viewport, axis) == RulerMapping::Oblique {
            style.ruler_disabled
        } else {
            style.ruler_fill
        }
    })
}

/// The rulers along the top and left edges of `outer`.
///
/// `outer` is the region *including* the gutters; the image occupies `outer`
/// inset by [`CanvasStyle::ruler_thickness_pt`] on those two sides, which is
/// the viewport `camera` is measured against.
pub fn rulers(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    outer: egui::Rect,
    spec: &RulerSpec,
    style: &CanvasStyle,
) {
    let t = style.ruler_thickness_pt;
    let [top, left] = rulers::gutters(outer, t);
    for (gutter, fill) in [top, left]
        .into_iter()
        .zip(gutter_fills(camera, viewport, style))
    {
        painter.rect_filled(gutter, egui::Rounding::ZERO, fill);
    }
    painter.line_segment(
        [top.left_bottom(), top.right_bottom()],
        style.hairline(style.ruler_tick_minor),
    );
    painter.line_segment(
        [left.right_top(), left.right_bottom()],
        style.hairline(style.ruler_tick_minor),
    );

    // The label font comes from the type scale, through the design crate's own
    // mapping — the canvas never names a size or a family.
    let font = design::egui_theme::font_id(
        design::current_theme(painter.ctx()).tokens(),
        design::TypeRole::Caption,
    );

    for axis in Axis::ALL {
        if rulers::ruler_mapping(camera, viewport, *axis) == RulerMapping::Oblique {
            // Nothing is drawn rather than something wrong; the view is turned
            // off-axis and no single number describes this edge.
            continue;
        }
        for tick in rulers::ruler_ticks(camera, viewport, *axis, spec) {
            let length = match tick.kind {
                TickKind::Major => t,
                TickKind::Minor => t * 0.4,
            };
            let stroke = match tick.kind {
                TickKind::Major => style.hairline(style.ruler_tick_major),
                TickKind::Minor => style.hairline(style.ruler_tick_minor),
            };
            match axis {
                Axis::X => {
                    let x = tick.screen_pt;
                    if x < top.min.x || x > top.max.x {
                        continue;
                    }
                    painter.line_segment(
                        [egui::pos2(x, top.max.y - length), egui::pos2(x, top.max.y)],
                        stroke,
                    );
                    if let Some(label) = &tick.label {
                        painter.text(
                            egui::pos2(x + style.label_gap_pt, top.min.y),
                            egui::Align2::LEFT_TOP,
                            label,
                            font.clone(),
                            style.ruler_text,
                        );
                    }
                }
                Axis::Y => {
                    let y = tick.screen_pt;
                    if y < left.min.y || y > left.max.y {
                        continue;
                    }
                    painter.line_segment(
                        [
                            egui::pos2(left.max.x - length, y),
                            egui::pos2(left.max.x, y),
                        ],
                        stroke,
                    );
                    if let Some(label) = &tick.label {
                        painter.text(
                            egui::pos2(left.min.x, y + style.label_gap_pt),
                            egui::Align2::LEFT_TOP,
                            label,
                            font.clone(),
                            style.ruler_text,
                        );
                    }
                }
            }
        }
    }
}

/// The user's guides.
pub fn guides(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    guides: &Guides,
    style: &CanvasStyle,
) {
    if !guides.visible {
        return;
    }
    let visible = camera.visible_doc_rect(viewport);
    for guide in guides.iter() {
        let colour = if guide.locked || guides.locked {
            style.guide_locked
        } else {
            style.guide
        };
        let (a, b) = grid::line_endpoints(visible, guide.axis, guide.doc);
        painter.line_segment(
            [
                to_pos2(camera.screen_pt_of(viewport, a)),
                to_pos2(camera.screen_pt_of(viewport, b)),
            ],
            style.hairline(colour),
        );
    }
}

/// The smart guides that explain a snap.
pub fn smart_guides(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    hits: &[SnapHit],
    style: &CanvasStyle,
) {
    let visible = camera.visible_doc_rect(viewport);
    for hit in hits {
        let (a, b) = grid::line_endpoints(visible, hit.candidate.axis, hit.candidate.doc);
        painter.line_segment(
            [
                to_pos2(camera.screen_pt_of(viewport, a)),
                to_pos2(camera.screen_pt_of(viewport, b)),
            ],
            style.hairline(style.smart_guide),
        );
    }
}

/// One outline per layer bounding box — View ▸ Layer Edges.
///
/// Projected corner by corner rather than drawn as a screen-space rectangle, so
/// a rotated or flipped view tilts the box with the image instead of leaving an
/// axis-aligned rectangle floating over it.
pub fn layer_edges(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    edges: &[DocRect],
    style: &CanvasStyle,
) {
    let stroke = style.hairline(style.layer_edge);
    for rect in edges {
        if rect.is_empty() {
            continue;
        }
        let quad: Vec<egui::Pos2> = rect
            .corners()
            .iter()
            .map(|c| to_pos2(camera.screen_pt_of(viewport, *c)))
            .collect();
        if quad.iter().any(|p| p.any_nan()) {
            continue;
        }
        painter.add(egui::Shape::closed_line(quad, stroke));
    }
}

/// The marching ants: an unbroken base run, then the contrasting dashes.
pub fn ants(painter: &egui::Painter, geometry: &AntsGeometry, style: &CanvasStyle) {
    for outline in &geometry.outlines {
        if outline.len() < 2 {
            continue;
        }
        painter.add(egui::Shape::line(
            outline.iter().copied().map(to_pos2).collect(),
            style.hairline(style.ants_base),
        ));
    }
    let stroke = style.hairline(style.ants_dash);
    for [a, b] in &geometry.dashes {
        painter.line_segment([to_pos2(*a), to_pos2(*b)], stroke);
    }
}

/// A live transform session, as the painter needs it.
///
/// Bundled rather than passed as five loose parameters: the four travel
/// together everywhere, and a five-argument call site invites transposing the
/// mode and the handle.
#[derive(Debug, Clone, Copy)]
pub struct TransformPaint<'a> {
    pub state: &'a TransformState,
    pub mode: TransformMode,
    pub layout: &'a HandleLayout,
    /// The handle being dragged, drawn emphasised.
    pub active: Option<Handle>,
}

/// The transform box: its outline, then its handles.
pub fn transform(
    painter: &egui::Painter,
    camera: &CanvasCamera,
    viewport: &Viewport,
    session: &TransformPaint<'_>,
    style: &CanvasStyle,
) {
    let TransformPaint {
        state,
        mode,
        layout,
        active,
    } = *session;
    let quad = handles::screen_quad(state, camera, viewport);
    if quad.iter().all(|p| p.is_finite()) {
        painter.add(egui::Shape::closed_line(
            quad.iter().copied().map(to_pos2).collect(),
            style.hairline(style.transform_outline),
        ));
    }
    let rounding = design::egui_theme::rounding(style.handle_radius_pt);
    for h in handles::screen_handles(state, mode, camera, viewport) {
        let selected = active == Some(h.handle);
        let stroke = if selected {
            style.thick(style.handle_selected)
        } else {
            style.hairline(style.handle_stroke)
        };
        match h.handle {
            Handle::Pivot => {
                painter.circle_filled(to_pos2(h.center_pt), layout.pivot_pt, style.handle_fill);
                painter.circle_stroke(to_pos2(h.center_pt), layout.pivot_pt, stroke);
                painter.line_segment(
                    [
                        to_pos2(h.center_pt - Vec2::new(layout.pivot_pt, 0.0)),
                        to_pos2(h.center_pt + Vec2::new(layout.pivot_pt, 0.0)),
                    ],
                    stroke,
                );
                painter.line_segment(
                    [
                        to_pos2(h.center_pt - Vec2::new(0.0, layout.pivot_pt)),
                        to_pos2(h.center_pt + Vec2::new(0.0, layout.pivot_pt)),
                    ],
                    stroke,
                );
            }
            _ => {
                let rect = h.rect(layout);
                painter.rect_filled(rect, rounding, style.handle_fill);
                painter.rect_stroke(rect, rounding, stroke);
            }
        }
    }
}

/// The crop overlay: scrim, outline, composition guides, grips.
pub fn crop(painter: &egui::Painter, overlay: &CropOverlay, style: &CanvasStyle) {
    if overlay.is_empty() {
        return;
    }
    for band in &overlay.scrim {
        painter.rect_filled(*band, egui::Rounding::ZERO, style.crop_scrim);
    }
    let guide_stroke = style.hairline(style.crop_guide);
    for [a, b] in &overlay.guides {
        painter.line_segment([to_pos2(*a), to_pos2(*b)], guide_stroke);
    }
    painter.rect_stroke(
        overlay.keep,
        egui::Rounding::ZERO,
        style.hairline(style.crop_outline),
    );
    for grip in &overlay.grips {
        painter.rect_filled(*grip, egui::Rounding::ZERO, style.crop_outline);
    }
}

/// Path anchors, control handles and direction lines.
pub fn path(
    painter: &egui::Painter,
    overlay: &PathOverlay,
    anchor_pt: f32,
    control_pt: f32,
    style: &CanvasStyle,
) {
    let direction = style.hairline(style.path_direction);
    for [a, b] in &overlay.direction_lines {
        painter.line_segment([to_pos2(*a), to_pos2(*b)], direction);
    }
    for c in &overlay.controls {
        painter.circle_filled(to_pos2(*c), control_pt * 0.5, style.path_control);
        painter.circle_stroke(
            to_pos2(*c),
            control_pt * 0.5,
            style.hairline(style.path_stroke),
        );
    }
    for (p, selected) in &overlay.anchors {
        let rect = egui::Rect::from_center_size(to_pos2(*p), egui::vec2(anchor_pt, anchor_pt));
        let fill = if *selected {
            style.path_anchor_selected
        } else {
            style.path_anchor
        };
        painter.rect_filled(rect, egui::Rounding::ZERO, fill);
        painter.rect_stroke(
            rect,
            egui::Rounding::ZERO,
            style.hairline(style.path_stroke),
        );
    }
}

/// The text caret and its selection highlight.
pub fn text(painter: &egui::Painter, overlay: &TextOverlay, style: &CanvasStyle) {
    for quad in &overlay.highlight {
        painter.add(egui::Shape::convex_polygon(
            quad.iter().copied().map(to_pos2).collect(),
            style.text_highlight,
            egui::Stroke::NONE,
        ));
    }
    if overlay.caret_visible {
        if let Some([a, b]) = overlay.caret {
            painter.line_segment([to_pos2(a), to_pos2(b)], style.thick(style.caret));
        }
    }
}

/// The brush ring, drawn twice so it reads over any image content.
pub fn brush(painter: &egui::Painter, cursor: &BrushCursor, style: &CanvasStyle) {
    if !cursor.outline.is_empty() {
        let points: Vec<egui::Pos2> = cursor.outline.iter().copied().map(to_pos2).collect();
        painter.add(egui::Shape::closed_line(
            points.clone(),
            style.thick(style.brush_ring_base),
        ));
        painter.add(egui::Shape::closed_line(
            points,
            style.hairline(style.brush_ring_over),
        ));
    }
    if let Some(arms) = cursor.crosshair {
        for [a, b] in arms {
            painter.line_segment([to_pos2(a), to_pos2(b)], style.thick(style.brush_ring_base));
            painter.line_segment(
                [to_pos2(a), to_pos2(b)],
                style.hairline(style.brush_ring_over),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::ants::AntsStyle;
    use crate::canvas::brush_cursor;
    use crate::canvas::crop::CropGuide;
    use crate::canvas::paths;
    use crate::canvas::snapping::{SnapCandidate, SnapKind};
    use crate::canvas::text_overlay::{self, TextCursor, TextLayout};
    use crate::canvas::viewport::PanelInsets;
    use design::Theme;
    use glam::IVec2;
    use raster::PixelRect;
    use selection::Polyline;
    use tools::BrushSettings;

    fn viewport() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn camera() -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(200.0, 150.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    /// Every overlay has to survive being drawn, in both appearances, without
    /// panicking and without producing a non-finite shape — an infinity in a
    /// path is what makes egui's tessellator blow up at runtime.
    #[test]
    fn every_overlay_draws_in_both_themes() {
        for theme in Theme::ALL {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, *theme);
            let style = CanvasStyle::new(*theme, 2.0);
            let v = viewport();
            let cam = camera();

            let output = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let painter = ui.painter().clone();
                    backdrop(&painter, &cam, &v, Vec2::new(400.0, 300.0), &style);

                    let settings = GridSettings {
                        visible: true,
                        spacing_doc: 32.0,
                        subdivisions: 4,
                        pixel_grid: true,
                    };
                    grid(
                        &painter,
                        &cam,
                        &v,
                        &settings,
                        DocRect::of_canvas(Vec2::new(400.0, 300.0)),
                        &style,
                    );
                    rulers(
                        &painter,
                        &cam,
                        &v,
                        v.content_rect(),
                        &RulerSpec::default(),
                        &style,
                    );

                    let mut gs = Guides::new();
                    let _ = gs.add(rulers::Guide::new(Axis::X, 100.0));
                    let _ = gs.add(rulers::Guide::new(Axis::Y, 50.0).locked());
                    guides(&painter, &cam, &v, &gs, &style);
                    smart_guides(
                        &painter,
                        &cam,
                        &v,
                        &[SnapHit {
                            candidate: SnapCandidate::new(Axis::X, 120.0, SnapKind::LayerEdge),
                            distance_pt: 1.0,
                        }],
                        &style,
                    );

                    let loops = vec![Polyline {
                        points: vec![
                            IVec2::new(10, 10),
                            IVec2::new(90, 10),
                            IVec2::new(90, 70),
                            IVec2::new(10, 70),
                        ],
                        closed: true,
                    }];
                    let geometry =
                        crate::canvas::ants::build(&loops, &cam, &v, &AntsStyle::default(), 1.5);
                    ants(&painter, &geometry, &style);

                    let state = TransformState::new(PixelRect::new(20, 20, 120, 90));
                    for mode in [TransformMode::Scale, TransformMode::Warp] {
                        transform(
                            &painter,
                            &cam,
                            &v,
                            &TransformPaint {
                                state: &state,
                                mode,
                                layout: &HandleLayout::default(),
                                active: Some(Handle::Corner(1)),
                            },
                            &style,
                        );
                    }

                    let overlay = crate::canvas::crop::build(
                        DocRect::new(Vec2::new(40.0, 30.0), Vec2::new(220.0, 180.0)),
                        &cam,
                        &v,
                        CropGuide::Thirds,
                        8.0,
                    );
                    crop(&painter, &overlay, &style);

                    let curve = vector::Path::from_elements(vec![
                        vector::PathEl::MoveTo(vector::point(10.0, 10.0)),
                        vector::PathEl::CurveTo(
                            vector::point(30.0, 0.0),
                            vector::point(60.0, 0.0),
                            vector::point(80.0, 10.0),
                        ),
                    ]);
                    let topology = paths::topology(&curve);
                    let projected = paths::project(&topology, &[0, 1], &cam, &v);
                    path(&painter, &projected, 6.0, 5.0, &style);

                    let layout = TextLayout {
                        lines: vec![crate::canvas::text_overlay::LineBox {
                            byte_start: 0,
                            byte_end: 4,
                            top: 0.0,
                            bottom: 20.0,
                            x_min: 0.0,
                            x_max: 40.0,
                            rtl: false,
                            first_glyph: 0,
                            glyph_count: 1,
                        }],
                        glyphs: vec![crate::canvas::text_overlay::GlyphBox {
                            cluster_start: 0,
                            cluster_end: 4,
                            x: 0.0,
                            advance: 40.0,
                            rtl: false,
                        }],
                        em_px: 20.0,
                    };
                    let geo = text_overlay::geometry(&layout, TextCursor { anchor: 0, head: 4 });
                    let projected_text =
                        text_overlay::project(&geo, Vec2::new(30.0, 40.0), &cam, &v, 0.0);
                    text(&painter, &projected_text, &style);

                    let cursor = brush_cursor::build(
                        &BrushSettings::default(),
                        0.7,
                        Vec2::new(150.0, 120.0),
                        &cam,
                        &v,
                    );
                    brush(&painter, &cursor, &style);
                });
            });

            let shapes = ctx.tessellate(output.shapes, output.pixels_per_point);
            assert!(!shapes.is_empty(), "{theme:?}: nothing was drawn");
            for clipped in &shapes {
                assert!(
                    clipped.clip_rect.min.x.is_finite() && clipped.clip_rect.max.x.is_finite(),
                    "{theme:?}: a non-finite clip rect reached the tessellator"
                );
            }
        }
    }

    /// Drawing must be safe when the canvas has collapsed to nothing — which
    /// happens for a frame whenever a dock is dragged wider than the window.
    #[test]
    fn a_collapsed_viewport_draws_without_panicking() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, Theme::Dark);
        let style = CanvasStyle::new(Theme::Dark, 1.0);
        let v = Viewport::new(Vec2::splat(60.0), PanelInsets::uniform(60.0), 1.0);
        let cam = camera();
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter().clone();
                backdrop(&painter, &cam, &v, Vec2::splat(100.0), &style);
                grid(
                    &painter,
                    &cam,
                    &v,
                    &GridSettings {
                        visible: true,
                        ..GridSettings::default()
                    },
                    DocRect::of_canvas(Vec2::splat(100.0)),
                    &style,
                );
                rulers(
                    &painter,
                    &cam,
                    &v,
                    v.content_rect(),
                    &RulerSpec::default(),
                    &style,
                );
                guides(&painter, &cam, &v, &Guides::new(), &style);
            });
        });
        let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
    }

    /// The regression this function was rewritten for: the renderer has
    /// already composited the image onto the surface, so an opaque backdrop
    /// over the content area erases the document. Not one band may touch the
    /// inside of the projected image, at any zoom or rotation.
    #[test]
    fn the_backdrop_is_a_hole_and_never_covers_the_image() {
        let v = viewport();
        let doc_size = Vec2::new(120.0, 90.0);
        for (label, cam) in [
            ("centred", camera()),
            (
                "zoomed out so the whole image is on screen",
                CanvasCamera {
                    center: doc_size * 0.5,
                    zoom: 0.5,
                    ..CanvasCamera::default()
                },
            ),
            (
                "zoomed in past the edges",
                CanvasCamera {
                    center: doc_size * 0.5,
                    zoom: 32.0,
                    ..CanvasCamera::default()
                },
            ),
            (
                "rotated off-axis",
                CanvasCamera {
                    center: doc_size * 0.5,
                    zoom: 2.0,
                    rotation: std::f32::consts::FRAC_PI_4,
                    ..CanvasCamera::default()
                },
            ),
            (
                "flipped",
                CanvasCamera {
                    center: doc_size * 0.5,
                    zoom: 2.0,
                    flip_x: true,
                    flip_y: true,
                    ..CanvasCamera::default()
                },
            ),
        ] {
            let image = document_bounds_pt(&cam, &v, doc_size).expect(label);
            let on_screen = image.intersect(&v.content_bounds_pt());
            let bands = backdrop_bands(&cam, &v, doc_size);
            if on_screen.is_empty() {
                continue;
            }
            let inside = super::super::geom::to_egui_rect(on_screen.min, on_screen.max);
            for band in &bands {
                let overlap = band.intersect(inside);
                assert!(
                    overlap.width() <= 1e-3 || overlap.height() <= 1e-3,
                    "{label}: the backdrop band {band:?} covers the image at {inside:?}"
                );
            }
            // The centre of the image is the pixel the user is looking at.
            let middle = inside.center();
            assert!(
                !bands.iter().any(|b| b.contains(middle)),
                "{label}: the backdrop covers the middle of the document"
            );
        }
    }

    /// …and it still fills everything the image does not reach, so no stale
    /// pixel of a previous frame shows through beside it.
    #[test]
    fn the_backdrop_and_the_image_together_cover_the_content_area() {
        let v = viewport();
        let doc_size = Vec2::new(120.0, 90.0);
        let cam = CanvasCamera {
            center: doc_size * 0.5,
            zoom: 2.0,
            ..CanvasCamera::default()
        };
        let image = document_bounds_pt(&cam, &v, doc_size)
            .unwrap()
            .intersect(&v.content_bounds_pt());
        let covered: f32 = backdrop_bands(&cam, &v, doc_size)
            .iter()
            .map(|r| r.width() * r.height())
            .sum::<f32>()
            + image.width() * image.height();
        let total = v.size_pt().x * v.size_pt().y;
        assert!(
            (covered - total).abs() < 1.0,
            "{covered} of {total} points were painted"
        );
    }

    #[test]
    fn with_no_document_the_backdrop_fills_the_whole_content_area() {
        let v = viewport();
        let cam = camera();
        assert_eq!(document_bounds_pt(&cam, &v, Vec2::ZERO), None);
        assert_eq!(backdrop_bands(&cam, &v, Vec2::ZERO), vec![v.content_rect()]);
        // A camera too broken to project also falls back to the full fill
        // rather than leaving the surface unpainted.
        let dead = CanvasCamera {
            zoom: f32::INFINITY,
            ..cam
        };
        assert_eq!(
            backdrop_bands(&dead, &v, Vec2::splat(100.0)),
            vec![v.content_rect()]
        );
        // A collapsed viewport has nothing to paint at all.
        let collapsed = Viewport::new(Vec2::splat(40.0), PanelInsets::uniform(40.0), 1.0);
        assert!(backdrop_bands(&cam, &collapsed, Vec2::splat(100.0)).is_empty());
    }

    /// A ruler that cannot read anything looks switched off, not broken.
    #[test]
    fn an_oblique_view_greys_the_ruler_gutters() {
        for theme in Theme::ALL {
            let style = CanvasStyle::new(*theme, 2.0);
            let v = viewport();
            assert_eq!(
                gutter_fills(&camera(), &v, &style),
                [style.ruler_fill, style.ruler_fill],
                "{theme:?}: an upright view greyed its rulers"
            );
            let turned = CanvasCamera {
                rotation: std::f32::consts::FRAC_PI_4,
                ..camera()
            };
            assert_eq!(
                gutter_fills(&turned, &v, &style),
                [style.ruler_disabled, style.ruler_disabled],
                "{theme:?}: a 45-degree view drew ordinary rulers with no ticks"
            );
            // A quarter turn still reads, just on the other axis.
            let quarter = CanvasCamera {
                rotation: std::f32::consts::FRAC_PI_2,
                ..camera()
            };
            assert_eq!(
                gutter_fills(&quarter, &v, &style),
                [style.ruler_fill, style.ruler_fill]
            );
        }
    }

    #[test]
    fn the_grid_reports_when_it_is_too_dense_to_draw() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, Theme::Dark);
        let style = CanvasStyle::new(Theme::Dark, 1.0);
        let v = Viewport::new(Vec2::new(800.0, 600.0), PanelInsets::NONE, 1.0);
        let settings = GridSettings {
            visible: true,
            spacing_doc: 1.0,
            subdivisions: 1,
            pixel_grid: false,
        };
        let canvas = DocRect::of_canvas(Vec2::splat(400.0));
        let mut dense = false;
        let mut sparse = false;
        let out = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                let painter = ui.painter().clone();
                dense = grid(
                    &painter,
                    &CanvasCamera {
                        zoom: 1.0 / 64.0,
                        ..CanvasCamera::default()
                    },
                    &v,
                    &settings,
                    canvas,
                    &style,
                );
                sparse = grid(
                    &painter,
                    &CanvasCamera {
                        zoom: 8.0,
                        ..CanvasCamera::default()
                    },
                    &v,
                    &settings,
                    canvas,
                    &style,
                );
            });
        });
        let _ = ctx.tessellate(out.shapes, out.pixels_per_point);
        assert!(dense, "a one-pixel grid at 1/64 zoom is not drawable");
        assert!(!sparse, "the same grid at 8x reads fine");
    }

    #[test]
    fn an_oblique_view_draws_no_ruler_ticks() {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, Theme::Dark);
        let style = CanvasStyle::new(Theme::Dark, 1.0);
        let v = viewport();
        let turned = CanvasCamera {
            rotation: std::f32::consts::FRAC_PI_4,
            ..camera()
        };
        // The mapping is what the painter branches on; assert it directly so
        // this stays a statement about behaviour and not about shape counts.
        assert_eq!(
            rulers::ruler_mapping(&turned, &v, Axis::X),
            RulerMapping::Oblique
        );
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                rulers(
                    &ui.painter().clone(),
                    &turned,
                    &v,
                    v.content_rect(),
                    &RulerSpec::default(),
                    &style,
                );
            });
        });
        let _ = ctx.tessellate(output.shapes, output.pixels_per_point);
    }
}
