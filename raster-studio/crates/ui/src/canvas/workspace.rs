//! The canvas as the workspace's central panel.
//!
//! [`super::CanvasView`] is deliberately shell-agnostic: it takes a `Ui` and a
//! [`super::CanvasContent`] and knows nothing about [`crate::Workspace`]. This
//! module is the one place that joins the two, so the canvas is reachable from
//! the application rather than being a library nothing calls, and so the panel
//! inset the module tree was written to fix is available to a renderer through
//! [`CanvasHost::render_camera`].
//!
//! That last step stops at this crate's edge: `ui` does not depend on `render`,
//! so handing the camera to the canvas pass is the native shell's job. What is
//! guaranteed here is that the number is right — see
//! `a_dock_moves_the_render_cameras_viewport_origin_by_its_own_width`.
//!
//! # Three things here are load-bearing
//!
//! **The overlays come from outside.** [`CanvasSessions`] is the seam a tool's
//! live transform, crop, path or text caret arrives through, and
//! [`CanvasHost::resolution_ppi`] and [`CanvasHost::unit`] are the two document
//! facts the rulers need that `editor_core::Document` does not carry. Without
//! them this function hard-coded `None` and `Default` for every one, and most
//! of what the canvas can draw was unreachable from the running application.
//!
//! **The central panel is drawn last, with no frame.** egui's central panel
//! takes whatever the other panels left, which is exactly the rectangle the
//! camera has to be measured against — so it has to be added after every dock.
//! And its *default* frame fills that rectangle with `SurfacePanel`, which
//! would paint over the image the renderer has already composited onto the
//! surface. [`egui::Frame::none`] is not a style choice; it is the difference
//! between seeing the document and not.
//!
//! **The selection outline is cached.** Tracing a boundary walks the whole
//! coverage mask. Doing that sixty times a second for a selection nobody has
//! touched would cost more than everything else the canvas does put together,
//! so it is recomputed only when the selection or the canvas size changes.

use editor_core::{Document, Selection};
use glam::{IVec2, Vec2};
use selection::Polyline;
use tools::transform::{Handle, TransformMode, TransformState};
use tools::{BrushSettings, ToolId};

use super::{
    CanvasContent, CanvasOutput, CanvasView, DocRect, PathTopology, RenderCamera, SnapHit,
    TextOverlayGeometry,
};

/// How opaque a coverage sample has to be to count as inside the selection.
/// Half: the same midpoint `selection` itself uses for a hard edge.
const OUTLINE_THRESHOLD: u8 = 128;

/// The resolution assumed for a document that has not been told one.
///
/// `editor_core::DocumentMeta` carries no resolution yet, so the shell writes
/// [`CanvasHost::resolution_ppi`] from whatever it knows (the Image Size
/// dialog's ppi, or a file's own metadata on import). 72 is the fallback every
/// physical unit is computed against until it does.
pub const DEFAULT_PPI: f32 = 72.0;

/// The overlay sessions the canvas draws on top of the image.
///
/// The canvas owns no session of its own: a transform, a crop, a path being
/// edited and a text caret all belong to the tool running them. This is the
/// seam — the application writes what is live, and the overlays appear. Before
/// it existed, [`CanvasHost::central_panel`] hard-coded `None` for every one of
/// them, so the transform handles, the crop scrim, the path anchors and the
/// caret were code the running application could not reach.
///
/// Owned rather than borrowed because it outlives a frame: the shell sets a
/// field when a session starts and clears it when the session ends.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanvasSessions {
    /// A live transform, and the mode it is in.
    pub transform: Option<(TransformState, TransformMode)>,
    /// The handle currently being dragged, drawn emphasised.
    pub active_handle: Option<Handle>,
    /// A live crop rectangle, in document pixels.
    pub crop: Option<DocRect>,
    /// A path being edited, and which of its anchors are selected.
    pub path: Option<(PathTopology, Vec<usize>)>,
    /// Text being edited: the caret geometry and the layer's origin.
    pub text: Option<(TextOverlayGeometry, Vec2)>,
    /// Bounds of the layers a gesture may snap against — everything except the
    /// one being dragged.
    pub snap_layers: Vec<DocRect>,
    /// Bounds of the layers View ▸ Layer Edges outlines — *all* of them, the
    /// one being dragged included. A layer's true bounds are the extent of its
    /// non-transparent pixels, which the compositor knows and
    /// `editor_core::Document` does not, so like [`CanvasSessions::snap_layers`]
    /// this is written by the shell rather than derived here.
    pub layer_edges: Vec<DocRect>,
    /// Smart guides the *application* computed, drawn under the canvas's own.
    pub smart_guides: Vec<SnapHit>,
}

impl CanvasSessions {
    /// Whether any overlay is live. `false` is the ordinary case: nothing but
    /// the image, the ants and the furniture.
    pub fn is_empty(&self) -> bool {
        self.transform.is_none()
            && self.crop.is_none()
            && self.path.is_none()
            && self.text.is_none()
    }

    /// Drop every session. What a tool change or an Escape does.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// The canvas, the frame it last produced, and the derived state a frame needs
/// that is too expensive to recompute every time.
#[derive(Debug, Clone)]
pub struct CanvasHost {
    /// The view: camera, guides, grid, snap, router.
    pub view: CanvasView,
    /// What the last frame produced — pointer samples for the active tool, the
    /// cursor, whether the grid is hidden.
    pub last: CanvasOutput,
    /// The overlays to draw over the image this frame.
    pub sessions: CanvasSessions,
    /// The document's resolution, in pixels per inch. Feeds the rulers, so an
    /// inch on a 300 ppi document is 300 document pixels and not 72.
    pub resolution_ppi: f32,
    /// The measurement unit the rulers read in — the application's, shared with
    /// the size dialogs.
    pub unit: crate::dialogs::units::Unit,
    /// The size of the document the last frame drew, so a zoom command that
    /// needs it — Fit on Screen, Fill — can be performed without one being
    /// handed in. Written by [`CanvasHost::central_panel`].
    doc_size: Vec2,
    /// The cached selection boundary, and what it was traced from.
    outline: Vec<Polyline>,
    outlined: Option<(Selection, u32, u32)>,
}

impl Default for CanvasHost {
    fn default() -> Self {
        Self {
            view: CanvasView::default(),
            last: CanvasOutput::default(),
            sessions: CanvasSessions::default(),
            resolution_ppi: DEFAULT_PPI,
            unit: crate::dialogs::units::Unit::default(),
            doc_size: Vec2::ZERO,
            outline: Vec::new(),
            outlined: None,
        }
    }
}

/// Logical points to the inch, the conventional reference used to say how big
/// a point is on a screen nobody has measured.
///
/// Only [`CanvasHost::zoom_to_print_size`] uses it, and only because "show this
/// at the size it will print" has no other answer without asking the display
/// how many inches across it is — which no windowing system reliably says.
pub const POINTS_PER_INCH: f32 = 96.0;

impl CanvasHost {
    /// A host looking at the middle of `doc`.
    pub fn for_document(doc: &Document) -> Self {
        Self {
            view: CanvasView::for_document(doc_size(doc)),
            ..Self::default()
        }
    }

    /// Everything a renderer needs to put the image in the right place —
    /// including [`RenderCamera::viewport_origin_px`], which is the whole point
    /// of the exercise: a renderer that honours it draws the image centred on
    /// the space the user can actually see rather than on the middle of the
    /// window. A renderer with no viewport offset uses
    /// [`RenderCamera::center_for_full_surface`] instead.
    pub fn render_camera(&self) -> RenderCamera {
        self.view.render_camera()
    }

    /// The selection boundary as the ants are drawn from, recomputing it only
    /// when the selection or the canvas size has actually changed.
    pub fn selection_outline(&mut self, doc: &Document) -> &[Polyline] {
        let key = (doc.selection.clone(), doc.width(), doc.height());
        if self.outlined.as_ref() != Some(&key) {
            let canvas = selection::Rect::new(
                IVec2::ZERO,
                IVec2::new(doc.width() as i32, doc.height() as i32),
            );
            // A selection too large or too odd to trace is drawn as no ants at
            // all, which is honest: an outline that is wrong is worse than one
            // that is missing.
            self.outline = match &doc.selection {
                Selection::None => Vec::new(),
                sel => {
                    selection::outline_selection(sel, canvas, OUTLINE_THRESHOLD).unwrap_or_default()
                }
            };
            self.outlined = Some(key);
        }
        &self.outline
    }

    /// The document the last frame drew, in pixels.
    pub fn doc_size(&self) -> Vec2 {
        self.doc_size
    }

    /// Fit the whole document in the viewport.
    pub fn zoom_to_fit(&mut self) {
        self.view.zoom_to_fit(self.doc_size);
    }

    /// Fill the viewport with the document.
    pub fn zoom_to_fill(&mut self) {
        self.view.zoom_to_fill(self.doc_size);
    }

    /// Frame the selection the last frame drew.
    ///
    /// The selection comes from the cache [`CanvasHost::selection_outline`]
    /// keeps, for the same reason `doc_size` is kept: Zoom to Selection is a
    /// [`crate::MenuAction`], and [`crate::Workspace::absorb`] is handed an
    /// intent and no document.
    ///
    /// Returns `false` when nothing is selected, so a caller can say so rather
    /// than appear to ignore the command. The menu disables the item in that
    /// case; this is the second line of defence, for a chord pressed on a
    /// document whose selection has since gone.
    pub fn zoom_to_selection(&mut self) -> bool {
        let Self { view, outlined, .. } = self;
        match outlined.as_ref() {
            Some((selection, _, _)) => view.zoom_to_selection(selection),
            None => false,
        }
    }

    /// Show the document at the size it will print: one document inch occupies
    /// one inch of screen, as far as [`POINTS_PER_INCH`] can tell.
    ///
    /// The camera's zoom is document pixels per *physical* pixel, so the
    /// display scale is part of the answer — at 2x, a printed inch is twice as
    /// many device pixels.
    pub fn zoom_to_print_size(&mut self) {
        let dpi = if self.resolution_ppi.is_finite() && self.resolution_ppi > 0.0 {
            self.resolution_ppi
        } else {
            DEFAULT_PPI
        };
        let ppp = self.view.viewport().pixels_per_point();
        self.view.camera.set_zoom(ppp * POINTS_PER_INCH / dpi);
    }

    /// Tell the rulers what they are measuring.
    ///
    /// Both halves used to be whatever [`super::RulerSpec::default`] said —
    /// 72 dpi and a one-pixel-wide document — so Inches read against the wrong
    /// resolution and Percent divided by one.
    fn sync_ruler_spec(&mut self, doc: &Document) {
        self.view.ruler_spec.unit = self.unit.into();
        self.view.ruler_spec.dpi = if self.resolution_ppi.is_finite() && self.resolution_ppi > 0.0 {
            self.resolution_ppi
        } else {
            DEFAULT_PPI
        };
        self.view.ruler_spec.doc_extent = doc_size(doc);
    }

    /// Draw the canvas into the central panel and route this frame's input.
    ///
    /// Call it **after** every panel has been added. Returns what the frame
    /// produced; the same value stays in [`CanvasHost::last`].
    ///
    /// The overlays come from [`CanvasHost::sessions`], which the application
    /// writes as its tools start and finish sessions. A caller that already has
    /// a [`CanvasContent`] of its own — borrowed from somewhere this struct
    /// cannot own it — hands it to [`CanvasHost::show`] instead.
    pub fn central_panel(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        tool: ToolId,
        brush: &BrushSettings,
    ) -> CanvasOutput {
        let time = ctx.input(|i| i.time);
        let size = doc_size(doc);
        // The first document the host is shown positions the camera. A canvas
        // left looking at (0, 0) puts the document's top-left *corner* in the
        // middle of the screen, which reads as the image being missing.
        if self.doc_size == Vec2::ZERO && size != Vec2::ZERO {
            self.view.camera.center = size * 0.5;
        }
        self.doc_size = size;
        self.selection_outline(doc);
        self.sync_ruler_spec(doc);
        // Split borrows: the content borrows the outline and the sessions while
        // the closure needs the view mutably, and all three are fields of the
        // same struct.
        let Self {
            view,
            sessions,
            outline,
            last,
            ..
        } = self;
        let content = CanvasContent {
            doc_size: size,
            active_tool: tool,
            selection_outline: outline,
            transform: sessions.transform.as_ref().map(|(s, m)| (s, *m)),
            active_handle: sessions.active_handle,
            crop: sessions.crop,
            path: sessions
                .path
                .as_ref()
                .map(|(topology, selected)| (topology, selected.as_slice())),
            text: sessions.text.as_ref().map(|(g, origin)| (g, *origin)),
            brush: Some(brush),
            snap_layers: &sessions.snap_layers,
            layer_edges: &sessions.layer_edges,
            smart_guides: &sessions.smart_guides,
            time_secs: time,
        };
        let mut out = CanvasOutput::default();
        egui::CentralPanel::default()
            // No frame: the image is already on the surface underneath, and the
            // default frame would fill straight over it.
            .frame(egui::Frame::none())
            .show(ctx, |ui| {
                out = view.show(ui, &content);
            });
        *last = out.clone();
        out
    }

    /// Draw one frame from a caller-built [`CanvasContent`].
    ///
    /// The escape hatch from [`CanvasHost::sessions`]: content borrowed from
    /// state this struct cannot own — a transform live inside a tool, a text
    /// layout owned by the text engine — reaches the canvas through here
    /// without being copied into the host every frame.
    pub fn show(&mut self, ui: &mut egui::Ui, content: &CanvasContent<'_>) -> CanvasOutput {
        let out = self.view.show(ui, content);
        self.last = out.clone();
        out
    }
}

/// A document's size as the canvas measures it.
pub fn doc_size(doc: &Document) -> Vec2 {
    Vec2::new(doc.width() as f32, doc.height() as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::Document;

    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |_| {},
        );
        ctx
    }

    #[test]
    fn a_host_starts_looking_at_the_middle_of_its_document() {
        let doc = Document::new(400, 300, "T");
        let host = CanvasHost::for_document(&doc);
        assert_eq!(host.view.camera.center, Vec2::new(200.0, 150.0));
        assert_eq!(doc_size(&doc), Vec2::new(400.0, 300.0));
    }

    #[test]
    fn the_selection_outline_is_traced_once_and_then_reused() {
        let mut doc = Document::new(64, 64, "T");
        let mut host = CanvasHost::default();
        assert!(host.selection_outline(&doc).is_empty());
        let first = host.outlined.clone();

        doc.selection = Selection::Rect {
            min: IVec2::new(8, 8),
            max: IVec2::new(40, 40),
        };
        let traced = host.selection_outline(&doc).len();
        assert!(traced > 0, "a rectangular selection has a boundary");
        assert_ne!(host.outlined, first);
        let key = host.outlined.clone();
        // Asking again with the same selection does not retrace it.
        assert_eq!(host.selection_outline(&doc).len(), traced);
        assert_eq!(host.outlined, key);

        // …and changing the canvas size does.
        let mut bigger = Document::new(128, 128, "T");
        bigger.selection = doc.selection.clone();
        host.selection_outline(&bigger);
        assert_ne!(host.outlined, key);
    }

    /// One frame through the real entry point, returning what it asked the
    /// painter for.
    fn frame(
        host: &mut CanvasHost,
        ctx: &egui::Context,
        doc: &Document,
        tool: ToolId,
    ) -> Vec<egui::epaint::ClippedShape> {
        let brush = BrushSettings::default();
        let full = ctx.run(egui::RawInput::default(), |ctx| {
            host.central_panel(ctx, doc, tool, &brush);
        });
        let shapes = full.shapes.clone();
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        shapes
    }

    /// The defect this seam was built for: every overlay but the ants, the
    /// guides, the grid and the brush ring was unreachable from the running
    /// application, because the only production caller hard-coded `None` for
    /// all of them and had no parameter to pass one.
    ///
    /// Each session is switched on in turn and has to cost the painter shapes.
    #[test]
    fn every_overlay_session_reaches_the_screen_through_the_central_panel() {
        let ctx = ctx();
        let doc = Document::new(200, 200, "T");
        let mut host = CanvasHost::for_document(&doc);
        // Two frames: the first settles the layout the camera is measured
        // against, so the baseline is taken from a stable one.
        frame(&mut host, &ctx, &doc, ToolId::Move);
        let bare = frame(&mut host, &ctx, &doc, ToolId::Move).len();

        let state = TransformState::new(raster::PixelRect::new(20, 20, 120, 100));
        let topology = super::super::paths::topology(&vector::Path::from_elements(vec![
            vector::PathEl::MoveTo(vector::point(20.0, 20.0)),
            vector::PathEl::CurveTo(
                vector::point(60.0, 10.0),
                vector::point(100.0, 10.0),
                vector::point(140.0, 40.0),
            ),
        ]));
        let text = TextOverlayGeometry {
            caret: Some(DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(11.0, 30.0))),
            highlight: vec![DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(90.0, 30.0))],
            run_bounds: Some(DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(90.0, 30.0))),
        };

        let cases: Vec<(&str, CanvasSessions)> = vec![
            (
                "transform",
                CanvasSessions {
                    transform: Some((state.clone(), TransformMode::Scale)),
                    active_handle: Some(Handle::Corner(0)),
                    ..CanvasSessions::default()
                },
            ),
            (
                "warp mesh",
                CanvasSessions {
                    transform: Some((state.clone(), TransformMode::Warp)),
                    ..CanvasSessions::default()
                },
            ),
            (
                "crop",
                CanvasSessions {
                    crop: Some(DocRect::new(Vec2::new(20.0, 20.0), Vec2::new(160.0, 140.0))),
                    ..CanvasSessions::default()
                },
            ),
            (
                "path",
                CanvasSessions {
                    path: Some((topology.clone(), vec![0])),
                    ..CanvasSessions::default()
                },
            ),
            (
                "text",
                CanvasSessions {
                    text: Some((text.clone(), Vec2::ZERO)),
                    ..CanvasSessions::default()
                },
            ),
        ];

        for (what, sessions) in cases {
            host.sessions = sessions;
            assert!(!host.sessions.is_empty(), "{what}");
            let drawn = frame(&mut host, &ctx, &doc, ToolId::Move).len();
            assert!(
                drawn > bare,
                "the {what} overlay drew nothing in the running application \
                 ({drawn} shapes vs {bare} without it)"
            );
        }

        host.sessions.clear();
        assert!(host.sessions.is_empty());
        assert_eq!(frame(&mut host, &ctx, &doc, ToolId::Move).len(), bare);
    }

    /// A transform session also makes its handles *grabbable*, not merely
    /// visible: the same frame reports the handle cursor under the corner.
    #[test]
    fn a_live_transform_sets_the_handle_cursor_through_the_host() {
        let ctx = ctx();
        let doc = Document::new(200, 200, "T");
        let mut host = CanvasHost::for_document(&doc);
        host.sessions.transform = Some((
            TransformState::new(raster::PixelRect::new(0, 0, 200, 200)),
            TransformMode::Scale,
        ));
        frame(&mut host, &ctx, &doc, ToolId::FreeTransform);

        let corner = host
            .view
            .camera
            .screen_pt_of(host.view.viewport(), Vec2::ZERO);
        let content = CanvasContent {
            doc_size: doc_size(&doc),
            active_tool: ToolId::FreeTransform,
            transform: host.sessions.transform.as_ref().map(|(s, m)| (s, *m)),
            ..CanvasContent::default()
        };
        assert_eq!(
            host.view.resolve_cursor(&content, Some(corner)),
            super::super::CanvasCursor::ResizeNwSe
        );
    }

    /// The rulers measure the document in front of them, not a placeholder.
    #[test]
    fn the_rulers_read_the_documents_own_resolution_and_size() {
        let ctx = ctx();
        let doc = Document::new(1200, 900, "T");
        let mut host = CanvasHost::for_document(&doc);
        host.resolution_ppi = 300.0;
        host.unit = crate::dialogs::units::Unit::Inches;
        frame(&mut host, &ctx, &doc, ToolId::Move);

        assert_eq!(host.view.ruler_spec.dpi, 300.0);
        assert_eq!(host.view.ruler_spec.doc_extent, Vec2::new(1200.0, 900.0));
        assert_eq!(host.view.ruler_spec.unit, super::super::Unit::Inches);

        host.view.camera.set_zoom(1.0);
        let ticks = super::super::rulers::ruler_ticks(
            &host.view.camera,
            host.view.viewport(),
            super::super::Axis::X,
            &host.view.ruler_spec,
        );
        let majors: Vec<f32> = ticks
            .iter()
            .filter(|t| t.kind == super::super::rulers::TickKind::Major)
            .map(|t| t.doc)
            .collect();
        assert!(majors.len() >= 2, "{ticks:?}");
        // One inch at 300 ppi is 300 document pixels: every tick's label is its
        // document coordinate divided by 300.
        for t in &ticks {
            assert!((t.doc - t.value * 300.0).abs() < 1e-2, "{t:?}");
        }
        // …and the labelled step divides the inch evenly (the 1-2-5 ladder
        // picks halves or fifths of it, never an arbitrary fraction).
        let step = majors[1] - majors[0];
        let per_inch = 300.0 / step;
        assert!(
            (per_inch - per_inch.round()).abs() < 1e-3,
            "a labelled step of {step} document pixels is not a division of an inch"
        );
        // …and at 72 dpi the same document would put them somewhere else.
        host.resolution_ppi = 72.0;
        frame(&mut host, &ctx, &doc, ToolId::Move);
        assert_eq!(host.view.ruler_spec.dpi, 72.0);

        // Percent divides by the document's real width.
        host.unit = crate::dialogs::units::Unit::Percent;
        frame(&mut host, &ctx, &doc, ToolId::Move);
        let ticks = super::super::rulers::ruler_ticks(
            &host.view.camera,
            host.view.viewport(),
            super::super::Axis::X,
            &host.view.ruler_spec,
        );
        for t in &ticks {
            assert!((t.doc - t.value * 12.0).abs() < 1e-2, "{t:?}");
        }
        assert!(!ticks.is_empty());
    }

    /// A nonsense resolution falls back rather than producing infinities.
    #[test]
    fn an_impossible_resolution_falls_back_to_the_default() {
        let ctx = ctx();
        let doc = Document::new(64, 64, "T");
        let mut host = CanvasHost::for_document(&doc);
        for bad in [0.0, -300.0, f32::NAN, f32::INFINITY] {
            host.resolution_ppi = bad;
            frame(&mut host, &ctx, &doc, ToolId::Move);
            assert_eq!(host.view.ruler_spec.dpi, DEFAULT_PPI, "{bad}");
        }
    }

    #[test]
    fn a_central_panel_frame_routes_a_press_to_the_tool() {
        let ctx = ctx();
        let doc = Document::new(200, 200, "T");
        let mut host = CanvasHost::for_document(&doc);
        let brush = BrushSettings::default();
        let full = ctx.run(egui::RawInput::default(), |ctx| {
            host.central_panel(ctx, &doc, ToolId::Brush, &brush);
        });
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);

        let at = host.view.viewport().center_pt();
        let full = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::PointerButton {
                    pos: egui::pos2(at.x, at.y),
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::default(),
                }],
                ..Default::default()
            },
            |ctx| {
                host.central_panel(ctx, &doc, ToolId::Brush, &brush);
            },
        );
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        assert_eq!(host.last.tool_events.len(), 1, "{:?}", host.last);
        assert_eq!(
            host.last.tool_events[0].route,
            crate::canvas::Route::Tool(ToolId::Brush)
        );
    }

    /// Layer Edges is a View toggle, so the boxes it draws have to reach the
    /// canvas through the same seam every other overlay uses.
    #[test]
    fn layer_edges_reach_the_screen_through_the_central_panel() {
        let ctx = ctx();
        let doc = Document::new(200, 200, "T");
        let mut host = CanvasHost::for_document(&doc);
        frame(&mut host, &ctx, &doc, ToolId::Move);
        let bare = frame(&mut host, &ctx, &doc, ToolId::Move).len();

        host.sessions.layer_edges = vec![
            DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(90.0, 70.0)),
            DocRect::new(Vec2::new(100.0, 80.0), Vec2::new(190.0, 190.0)),
        ];
        let drawn = frame(&mut host, &ctx, &doc, ToolId::Move).len();
        assert_eq!(
            drawn,
            bare + 2,
            "the layer edges never reached the painter ({drawn} vs {bare})"
        );

        host.view.layer_edges_visible = false;
        assert_eq!(frame(&mut host, &ctx, &doc, ToolId::Move).len(), bare);
    }

    /// Zoom to Selection has to work from a menu action, which arrives without
    /// a document — so the host frames the selection the last frame drew.
    #[test]
    fn the_host_frames_the_selection_it_last_drew() {
        let ctx = ctx();
        let mut doc = Document::new(2000, 2000, "T");
        let mut host = CanvasHost::for_document(&doc);
        frame(&mut host, &ctx, &doc, ToolId::Move);

        // Nothing selected: refused, and the camera does not move.
        let before = host.view.camera;
        assert!(!host.zoom_to_selection());
        assert_eq!(host.view.camera, before);

        doc.selection = Selection::Rect {
            min: IVec2::new(1000, 1200),
            max: IVec2::new(1100, 1260),
        };
        frame(&mut host, &ctx, &doc, ToolId::Move);
        assert!(host.zoom_to_selection());
        assert!(
            (host.view.camera.center - Vec2::new(1050.0, 1230.0)).length() < 1e-3,
            "{:?}",
            host.view.camera.center
        );

        // …and a selection cleared since the last frame is refused again.
        doc.selection = Selection::None;
        frame(&mut host, &ctx, &doc, ToolId::Move);
        let before = host.view.camera;
        assert!(!host.zoom_to_selection());
        assert_eq!(host.view.camera, before);
    }
}
