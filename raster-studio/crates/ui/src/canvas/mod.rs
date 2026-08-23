//! The canvas view: the viewport the document is drawn into, and everything
//! drawn on top of it.
//!
//! # Shape of the module
//!
//! | module | what it owns |
//! |---|---|
//! | [`geom`] | the shared vocabulary: axes, document rectangles, egui conversions |
//! | [`viewport`] | the content region — panel insets and the display scale |
//! | [`camera`] | pan, zoom, view rotation, view flip, and the two transforms |
//! | [`rulers`] | measurement units, ruler ticks, guides |
//! | [`grid`] | the document grid and the pixel grid |
//! | [`snapping`] | snap candidates, the nearest-within-threshold rule, smart guides |
//! | [`ants`] | the animated selection outline |
//! | [`handles`] | transform handles, their hit regions and their cursors |
//! | [`crop`] | the crop rectangle, scrim and composition guides |
//! | [`paths`] | anchors, control handles and direction lines |
//! | [`text_overlay`] | the editing caret and the selection highlight |
//! | [`brush_cursor`] | the ring at the true brush size and shape |
//! | [`cursor`] | the cursor vocabulary and the precise toggle |
//! | [`pointer`] | egui's per-frame events, tracked into a gesture stream |
//! | [`input`] | routing pointer samples to the tool or to the camera |
//! | [`style`] | every colour and width, resolved from `design` tokens |
//! | [`paint`] | the egui drawing, and nothing else |
//! | [`workspace`] | the canvas as [`crate::Workspace`]'s central panel |
//!
//! # The two rules
//!
//! **Geometry is exact.** Three spaces meet here — document pixels, egui
//! logical points, physical device pixels — and every conversion goes through
//! [`CanvasCamera`] against a [`Viewport`] that knows the panel insets *and*
//! the display scale. That is the fix for the bug this module was written
//! around: the camera used to be handed the whole physical surface while egui
//! reserved logical points for the docks, so the image centred on the middle of
//! the window rather than on the middle of the space the user could see.
//!
//! **The UI is a view.** Nothing here mutates the document. Pointer samples are
//! converted to document space and handed back for the caller to feed the
//! active tool, which is what produces [`editor_core::Command`]s. The canvas
//! owns only *view* state — pan, zoom, rotation, flip, guides, grid, snap — and
//! a view change is deliberately not a command, because a ctrl+Z that scrolls
//! the canvas instead of undoing a stroke is the most confusing thing an editor
//! can do.
//!
//! # One priority order
//!
//! The pointer can be over several things at once — a transform handle drawn on
//! top of a crop grip standing on a guide. [`Region`] and
//! [`CanvasView::what_is_under`] are the *single* place that order is decided,
//! and both the press dispatch in [`CanvasView::show`] and the cursor in
//! [`CanvasView::resolve_cursor`] read it. Two orders is how a canvas ends up
//! showing a scale arrow and then dragging a guide.
//!
//! # Known gaps
//!
//! * **Guides live in the view, not the document.** [`Guides`] is a field on
//!   [`CanvasView`], so guides are neither saved nor undoable. Moving them into
//!   `editor-core` needs a command for them, which does not exist yet.
//! * **Tablet pressure is supplied by the shell.** egui 0.29's input carries no
//!   pressure, so [`CanvasView::set_pen_pressure`] is the seam the native shell
//!   feeds from its own tablet stream. Without it every sample is a mouse at
//!   full pressure.
//! * **`tools`' own navigation tools are redundant here.** The router drives
//!   [`CanvasCamera`] directly, because [`tools::ViewState`] predates panel
//!   insets, display scale and view flipping.
//!   [`CanvasCamera::to_view_state`] bridges the two for code that still needs
//!   a `ToolContext`, and loses the flip flags in the process.
//! * **The renderer end of the wire is the shell's to connect.** [`workspace`]
//!   puts the canvas on [`crate::Workspace`] and
//!   [`crate::Workspace::render_camera`] hands out the target rectangle in
//!   physical pixels, insets included. `ui` does not depend on `render`, so the
//!   native shell is what feeds that camera to the canvas pass — until it does,
//!   the inset is computed correctly and read by nobody.
//! * **Snapping snaps the pointer, not the dragged object.** [`snapping`] pulls
//!   the sample on its way to a tool that places things (see
//!   [`snapping::tool_snaps`]); snapping a *layer's own edges* needs that
//!   layer's bounds and the grab offset, which live in the tool. A tool that
//!   wants edge snapping calls [`CanvasView::snap`] with its own rectangle.
//! * **Crop and path gestures stop at the hit test.** [`crop::hit_test`] and
//!   [`paths::hit_test`] say what is under the pointer and set the cursor, and
//!   [`CanvasOutput`] reports it; *dragging* a crop grip or an anchor is the
//!   owning tool's job, and this module deliberately does not do it, because a
//!   crop is a document edit and the canvas edits nothing.

pub mod ants;
pub mod brush_cursor;
pub mod camera;
pub mod crop;
pub mod cursor;
pub mod geom;
pub mod grid;
pub mod handles;
pub mod input;
pub mod paint;
pub mod paths;
pub mod pointer;
pub mod rulers;
pub mod snapping;
pub mod style;
pub mod text_overlay;
pub mod viewport;
pub mod workspace;

use glam::Vec2;
use selection::Polyline;
use tools::transform::{Handle, TransformMode, TransformState};
use tools::{BrushSettings, ToolId};

pub use ants::{ants_phase, AntsGeometry, AntsStyle};
pub use brush_cursor::BrushCursor;
pub use camera::{CanvasCamera, RenderCamera, ZOOM_STEPS};
pub use crop::{CropGrip, CropGuide, CropOverlay};
pub use cursor::{cursor_for_tool_id, CanvasCursor, CursorOverride};
pub use geom::{Axis, DocRect};
pub use grid::GridSettings;
pub use handles::HandleLayout;
pub use input::{
    Dispatch, InputRouter, PointerButton, PointerInput, PointerPhase, Rejected, Route,
    RoutedPointer, WheelAction,
};
pub use paths::{PathHit, PathOverlay, PathTopology};
pub use pointer::{button_from_egui, modifiers_from_egui, FrameSamples, PointerTracker};
pub use rulers::{Guide, GuideDrag, GuideDrop, GuideGesture, GuideGrab, Guides, RulerSpec, Unit};
pub use snapping::{SnapCandidate, SnapHit, SnapKind, SnapResult, SnapSettings, SnapSources};
pub use style::CanvasStyle;
pub use text_overlay::{TextCursor, TextLayout, TextOverlayGeometry};
pub use viewport::{PanelInsets, Viewport};
pub use workspace::CanvasHost;

/// What the canvas should draw on top of the composited image this frame, and
/// the document facts it needs to lay itself out.
///
/// Borrowed rather than owned: the caller already has all of it, and copying a
/// selection outline every frame would be the most expensive thing the canvas
/// does.
pub struct CanvasContent<'a> {
    /// The document's size in pixels; the canvas rectangle is `0..size`.
    pub doc_size: Vec2,
    /// The tool that gets pointer events not claimed by navigation.
    pub active_tool: ToolId,
    /// The selection boundary, from [`selection::outline_selection`].
    pub selection_outline: &'a [Polyline],
    /// A live transform session.
    pub transform: Option<(&'a TransformState, TransformMode)>,
    /// The handle currently being dragged, drawn emphasised.
    pub active_handle: Option<Handle>,
    /// A live crop.
    pub crop: Option<DocRect>,
    /// A path being edited, and which of its anchors are selected.
    pub path: Option<(&'a PathTopology, &'a [usize])>,
    /// Text being edited: the caret geometry and the layer's origin.
    pub text: Option<(&'a TextOverlayGeometry, Vec2)>,
    /// The brush whose ring is the cursor, when a brush-like tool is active.
    pub brush: Option<&'a BrushSettings>,
    /// Bounds of the layers a gesture may snap against — everything except the
    /// one being dragged, which would otherwise snap to itself and freeze.
    pub snap_layers: &'a [DocRect],
    /// Smart guides to draw, from a snap the *caller* performed. The canvas
    /// draws its own on top of these; see [`CanvasView::smart_guides`].
    pub smart_guides: &'a [snapping::SnapHit],
    /// Seconds since the app started, for the ants and the caret.
    pub time_secs: f64,
}

impl Default for CanvasContent<'_> {
    fn default() -> Self {
        Self {
            doc_size: Vec2::ZERO,
            active_tool: ToolId::Move,
            selection_outline: &[],
            transform: None,
            active_handle: None,
            crop: None,
            path: None,
            text: None,
            brush: None,
            snap_layers: &[],
            smart_guides: &[],
            time_secs: 0.0,
        }
    }
}

/// What one frame of the canvas produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CanvasOutput {
    /// Pointer samples for the caller to feed the active tool, in document
    /// coordinates and in the order they arrived.
    pub tool_events: Vec<RoutedPointer>,
    /// The view moved, so a repaint is needed.
    pub view_changed: bool,
    /// The cursor the canvas asked for.
    pub cursor: CanvasCursor,
    /// The document position under the pointer, when it is over the canvas.
    pub pointer_doc: Option<Vec2>,
    /// The grid is switched on but too dense at this zoom to be legible, so
    /// nothing was drawn. The shell should *say* the grid is hidden — a user
    /// who turns the grid on and sees nothing has been told the setting was
    /// ignored, which it was not.
    pub grid_suppressed: bool,
    /// The crop grip under the pointer, when a crop is live.
    pub crop_grip: Option<CropGrip>,
    /// The path anchor or control handle under the pointer, when a path is
    /// being edited.
    pub path_hit: Option<PathHit>,
    /// A guide is being dragged, so the shell can show its position.
    pub guide_drag: Option<GuideDrag>,
}

/// The canvas: view state, the input router, and one `show` per frame.
#[derive(Debug, Clone)]
pub struct CanvasView {
    pub camera: CanvasCamera,
    pub rulers_visible: bool,
    pub ruler_spec: RulerSpec,
    pub guides: Guides,
    pub grid: GridSettings,
    pub snap: SnapSettings,
    pub ants: AntsStyle,
    pub handle_layout: HandleLayout,
    pub crop_guide: CropGuide,
    /// Swap the pictorial cursors for a crosshair.
    pub precise_cursor: bool,
    /// The router; public so the shell can tell it about the space bar.
    pub router: InputRouter,
    viewport: Viewport,
    /// The gutter-inclusive rectangle the canvas last occupied.
    outer_rect: egui::Rect,
    /// The gutter depth the last frame resolved, so the guide hit test and the
    /// painter agree about where the rulers are.
    ruler_thickness_pt: f32,
    /// Held button and last position, across frames — see [`pointer`].
    pointer: PointerTracker,
    /// A guide being dragged, if any.
    guide_drag: Option<GuideDrag>,
    /// What the last snapped sample caught, for the smart guides this frame.
    smart_guides: Vec<SnapHit>,
    pen_pressure: f32,
    cursor: CanvasCursor,
    pointer_doc: Option<Vec2>,
}

impl Default for CanvasView {
    fn default() -> Self {
        Self {
            camera: CanvasCamera::default(),
            rulers_visible: true,
            ruler_spec: RulerSpec::default(),
            guides: Guides::new(),
            grid: GridSettings::default(),
            snap: SnapSettings::default(),
            ants: AntsStyle::default(),
            handle_layout: HandleLayout::default(),
            crop_guide: CropGuide::default(),
            precise_cursor: false,
            router: InputRouter::new(),
            viewport: Viewport::default(),
            outer_rect: egui::Rect::NOTHING,
            ruler_thickness_pt: 0.0,
            pointer: PointerTracker::default(),
            guide_drag: None,
            smart_guides: Vec::new(),
            pen_pressure: 1.0,
            cursor: CanvasCursor::Arrow,
            pointer_doc: None,
        }
    }
}

/// `true` when egui has something of its own floating over this point — a
/// dialog, a menu, a combo popup, a tooltip.
///
/// All of those live in layers above the background, and all of them are drawn
/// *inside* the canvas rectangle, so the rectangle test alone would hand their
/// clicks to the active tool as well. The canvas itself is a background-layer
/// widget, so anything the background reports is the canvas's own.
///
/// Asked per pointer sample rather than once per frame. One frame can carry
/// positions on both sides of a dialog's edge, and
/// [`egui::Context::is_pointer_over_area`] only ever sees the last of them —
/// which is how a press *inside* a dialog followed by a drag *out* of it used
/// to reach the tool anyway.
pub fn egui_owns_point(ctx: &egui::Context, pos_pt: glam::Vec2) -> bool {
    match ctx.layer_id_at(geom::to_pos2(pos_pt)) {
        Some(layer) => layer.order != egui::Order::Background,
        None => false,
    }
}

/// What is under the pointer, in the **one** priority order the canvas obeys.
///
/// This exists because the press router and the cursor used to answer that
/// question separately and disagreed: `show` offered every press to the guides
/// first, while `resolve_cursor` ranked the transform box above them. A guide
/// sitting on a layer edge — where guides are usually put, and where transform
/// handles therefore also are — showed a resize cursor and then dragged the
/// guide, and the handle underneath could not be grabbed at all.
///
/// [`CanvasView::what_is_under`] is now the only place the order is written
/// down, and both the dispatch and the cursor read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Region {
    /// Not the canvas's: a dock, or the space outside the gutters.
    Panel,
    /// A ruler gutter, which a guide can be dragged out of.
    Gutter(Axis),
    /// The hand — the space bar, or the hand tool's own drag. It outranks
    /// everything, because a temporary pan must work over any furniture.
    Hand { dragging: bool },
    /// A transform handle: scale, rotate, pivot or warp point.
    TransformHandle(Handle),
    /// A crop grip, or the kept area itself.
    CropGrip(CropGrip),
    /// A guide. `locked` when grabbing it will refuse rather than move it.
    Guide { index: usize, locked: bool },
    /// A path anchor or one of its control handles.
    Path(PathHit),
    /// Bare image: the active tool's.
    Image,
}

impl Region {
    /// Whether a press here belongs to the guides.
    ///
    /// Only two regions are theirs, and both of them are *about* guides — which
    /// is the whole fix: a guide can no longer claim a press that landed on a
    /// transform handle or a crop grip drawn over it.
    pub fn is_guides(self) -> bool {
        matches!(self, Region::Gutter(_) | Region::Guide { .. })
    }
}

/// The size of a path anchor square, in screen points — one and a half grid
/// units, the step between the two smallest rungs.
const ANCHOR_PT: f32 = design::Space::XSmall.units() * 1.5 * design::UNIT_PT;
/// The diameter of a path control-handle disc: one grid unit, so a control
/// reads as subordinate to the anchor it belongs to.
const CONTROL_PT: f32 = design::Space::XSmall.units() * design::UNIT_PT;
/// How close, in screen points, the pointer has to be to grab a guide.
pub const GUIDE_GRAB_PT: f32 = design::Space::XSmall.units() * design::UNIT_PT;

impl CanvasView {
    /// A canvas looking at the middle of a document of `size` pixels.
    pub fn for_document(size: Vec2) -> Self {
        Self {
            camera: CanvasCamera::for_document(size),
            ..Self::default()
        }
    }

    /// The viewport the image occupies — the content region with the ruler
    /// gutters already taken off.
    pub fn viewport(&self) -> &Viewport {
        &self.viewport
    }

    /// The cursor the canvas asked for last frame.
    pub fn cursor(&self) -> CanvasCursor {
        self.cursor
    }

    /// Supply the tablet pressure for the next samples.
    ///
    /// egui 0.29 has no pressure in its input stream, so the native shell reads
    /// it from the OS and pushes it here. Left alone, every sample is a mouse.
    pub fn set_pen_pressure(&mut self, pressure: f32) {
        self.pen_pressure = if pressure.is_finite() {
            pressure.clamp(0.0, 1.0)
        } else {
            1.0
        };
    }

    /// Recompute the viewport from an egui frame's geometry.
    ///
    /// `content` is the rectangle the canvas was given, in logical points —
    /// what `ui.max_rect()` reports inside the central panel. The ruler gutters
    /// come off it here, so the camera never sees them.
    ///
    /// `style` is the one the frame is actually being drawn with, not a
    /// freshly-defaulted one: the gutter depth is a token, and reading it off
    /// the wrong theme would put the image and the rulers in different places
    /// the moment that token stopped being theme-independent.
    pub fn sync_viewport(
        &mut self,
        surface_pt: Vec2,
        content: egui::Rect,
        ppp: f32,
        style: &CanvasStyle,
    ) {
        self.outer_rect = content;
        self.ruler_thickness_pt = if self.rulers_visible {
            style.ruler_thickness_pt
        } else {
            0.0
        };
        let outer = Viewport::from_content_rect(surface_pt, content, ppp);
        let t = self.ruler_thickness_pt;
        self.viewport = if t > 0.0 {
            outer.inset_by(PanelInsets::new(t, 0.0, t, 0.0))
        } else {
            outer
        };
    }

    /// The guide gesture for the geometry this frame settled on.
    ///
    /// Built by value rather than borrowed, so a caller can hand
    /// [`CanvasView::guides`] out mutably at the same time.
    pub fn guide_gesture(&self) -> GuideGesture {
        GuideGesture {
            camera: self.camera,
            viewport: self.viewport,
            outer: self.outer_rect,
            ruler_thickness_pt: self.ruler_thickness_pt,
            rulers_visible: self.rulers_visible,
            grab_pt: GUIDE_GRAB_PT,
        }
    }

    /// The guide currently being dragged, if any.
    pub fn guide_drag(&self) -> Option<GuideDrag> {
        self.guide_drag
    }

    /// The smart guides the canvas's own snapping produced for the last sample
    /// it snapped — the lines that explain why something jumped.
    pub fn smart_guides(&self) -> &[SnapHit] {
        &self.smart_guides
    }

    /// Snap one routed sample on its way to the tool, when the snap is on and
    /// this tool is one that places things.
    ///
    /// Returns whether the position moved. The smart guides the snap produced
    /// are kept for this frame's painting.
    fn snap_routed(&mut self, routed: &mut RoutedPointer, content: &CanvasContent<'_>) -> bool {
        // A bare hover is not a placement: snapping it would drag smart guides
        // around the screen while the user is only looking.
        if !self.snap.enabled || !routed.in_gesture || !snapping::tool_snaps(content.active_tool) {
            self.smart_guides.clear();
            return false;
        }
        let before = routed.event.pos;
        let result = self.snap(before, content.doc_size, content.snap_layers);
        // The gesture is over on release, and so are the lines explaining it.
        self.smart_guides = match routed.phase {
            PointerPhase::Up => Vec::new(),
            _ => result.smart_guides().collect(),
        };
        routed.event.pos = result.point;
        result.point != before
    }

    /// The crop overlay for a document rectangle, as this frame will draw it.
    ///
    /// The painter and the hit test go through the same call, so what is
    /// grabbed is exactly what is drawn — the grips cannot end up somewhere the
    /// pointer does not find them.
    pub fn crop_overlay(&self, crop_doc: DocRect) -> CropOverlay {
        crop::build(
            crop_doc,
            &self.camera,
            &self.viewport,
            self.crop_guide,
            self.handle_layout.handle_pt,
        )
    }

    /// Fit the whole document in the viewport.
    pub fn zoom_to_fit(&mut self, doc_size: Vec2) {
        self.camera.fit_document(&self.viewport, doc_size);
    }

    /// Fill the viewport with the document.
    pub fn zoom_to_fill(&mut self, doc_size: Vec2) {
        self.camera.fill_document(&self.viewport, doc_size);
    }

    /// One document pixel per physical pixel, snapped so the grids line up.
    pub fn zoom_to_actual_pixels(&mut self) {
        self.camera.zoom_to_actual_pixels(&self.viewport);
    }

    /// Frame a document rectangle — what "zoom to selection" does.
    pub fn zoom_to_rect(&mut self, rect: DocRect) {
        self.camera.fit_rect(&self.viewport, rect);
    }

    /// Frame the current selection, or do nothing when nothing is selected.
    ///
    /// Returns whether the view moved, so a caller can report "nothing is
    /// selected" rather than appearing to ignore the command.
    pub fn zoom_to_selection(&mut self, selection: &editor_core::Selection) -> bool {
        let Some((min, max)) = selection.bounds() else {
            return false;
        };
        let rect = DocRect::new(
            Vec2::new(min.x as f32, min.y as f32),
            Vec2::new(max.x as f32, max.y as f32),
        );
        if rect.is_empty() {
            return false;
        }
        self.zoom_to_rect(rect);
        true
    }

    /// Step in one rung, about the centre of the viewport.
    pub fn zoom_in(&mut self) {
        let anchor = self.viewport.center_pt();
        self.camera.zoom_in(&self.viewport, anchor);
    }

    /// Step out one rung, about the centre of the viewport.
    pub fn zoom_out(&mut self) {
        let anchor = self.viewport.center_pt();
        self.camera.zoom_out(&self.viewport, anchor);
    }

    /// Snap the document rectangle for a document of `size`.
    pub fn canvas_rect(size: Vec2) -> DocRect {
        DocRect::of_canvas(size)
    }

    /// Snap a document point against everything switched on.
    pub fn snap(&self, point: Vec2, doc_size: Vec2, other_layers: &[DocRect]) -> SnapResult {
        let scale = snapping::scale_for(&self.camera, &self.viewport);
        if !scale.is_finite() || scale <= 0.0 {
            return SnapResult::unchanged(point);
        }
        let threshold_doc = self.snap.threshold() / scale;
        let sources = SnapSources {
            guides: Some(&self.guides),
            grid: Some(&self.grid),
            canvas: Some(DocRect::of_canvas(doc_size)),
            layers: other_layers,
        };
        let around = DocRect::from_corners(point, point).expanded(threshold_doc.max(1.0));
        let candidates = snapping::collect_candidates(&sources, &self.snap, around, threshold_doc);
        snapping::snap_point(point, &candidates, &self.snap, scale)
    }

    /// Everything a renderer needs to put the image in the right place.
    pub fn render_camera(&self) -> RenderCamera {
        self.camera.render_camera(&self.viewport)
    }

    /// Route one sample to the guides, if it is theirs.
    ///
    /// `Some(changed)` means the guides consumed it — the gesture is claimed in
    /// [`InputRouter`], so neither the active tool nor the camera will ever see
    /// this press or the drag that follows it. `None` means the sample belongs
    /// to somebody else.
    ///
    /// `may_grab` is [`Region::is_guides`] for the press position: a press only
    /// reaches the guides when nothing with a higher claim — a transform
    /// handle, a crop grip, the hand — is under the pointer. Move and release
    /// are not gated by it, because a drag already in flight runs to completion
    /// wherever the pointer goes.
    fn guide_sample(&mut self, sample: PointerInput, may_grab: bool) -> Option<bool> {
        let gesture = self.guide_gesture();
        match sample.phase {
            PointerPhase::Down => {
                if !may_grab
                    || self.router.is_gesture_active()
                    || sample.button != PointerButton::Primary
                {
                    return None;
                }
                match gesture.begin(&self.guides, sample.pos_pt) {
                    GuideGrab::None => None,
                    // A locked guide is claimed too, and then does nothing: a
                    // press on it must not fall through and start painting
                    // underneath the line the user was aiming at.
                    GuideGrab::Refused => {
                        self.router.claim(Route::Guide, sample.pos_pt);
                        Some(false)
                    }
                    GuideGrab::Start(drag) => {
                        self.guide_drag = Some(drag);
                        self.router.claim(Route::Guide, sample.pos_pt);
                        Some(false)
                    }
                }
            }
            PointerPhase::Move => {
                let drag = self.guide_drag?;
                self.guide_drag = gesture.drag(drag, &mut self.guides, sample.pos_pt);
                Some(true)
            }
            PointerPhase::Up => {
                if self.router.active_route() != Some(Route::Guide) {
                    return None;
                }
                let changed = match self.guide_drag.take() {
                    Some(drag) => {
                        gesture.finish(drag, &mut self.guides, sample.pos_pt)
                            != rulers::GuideDrop::NeverCreated
                    }
                    None => false,
                };
                self.router.release(Route::Guide);
                Some(changed)
            }
        }
    }

    /// Draw one frame and route this frame's input.
    ///
    /// The image itself is *not* drawn here: it is composited onto the surface
    /// by the renderer before egui runs. This paints the backdrop *around* it —
    /// as a hole, never as a sheet over the top — and everything above it.
    pub fn show(&mut self, ui: &mut egui::Ui, content: &CanvasContent<'_>) -> CanvasOutput {
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let surface = geom::from_egui_vec2(ctx.screen_rect().size());
        let rect = ui.max_rect();
        let style = CanvasStyle::from_context(&ctx);
        self.sync_viewport(surface, rect, ppp, &style);

        let response = ui.interact(
            rect,
            ui.id().with("raster-canvas"),
            egui::Sense::click_and_drag(),
        );

        // ---- input ----
        // The space bar is the temporary hand tool only while nothing is
        // typing. Renaming a layer to "My Layer" must not arm it, which is the
        // same guard `Workspace::handle_keys` puts on every other shortcut.
        self.router.set_space_held(
            !ctx.wants_keyboard_input() && ctx.input(|i| i.key_down(egui::Key::Space)),
        );

        let frame = ctx.input(|i| self.pointer.frame(i, self.pen_pressure));
        let mut out = CanvasOutput::default();

        for sample in frame.samples {
            // Whether a press belongs to the canvas is not a rectangle test.
            // A dialog, a menu, a combo popup or a tooltip floating over the
            // image is *inside* the canvas rectangle, and a click aimed at one
            // must not also land on the active tool as a brush dab. Asked per
            // sample, and only for a gesture that has not started yet: once a
            // gesture is claimed it runs to completion wherever the pointer
            // goes, because a drag that dies when the cursor crosses a panel is
            // worse than useless.
            if !self.router.is_gesture_active() && egui_owns_point(&ctx, sample.pos_pt) {
                continue;
            }
            // One decision, read by both the dispatch and the cursor: a guide
            // only takes the press when nothing above it in the order is under
            // the pointer.
            let region = self.what_is_under(content, sample.pos_pt);
            if let Some(changed) = self.guide_sample(sample, region.is_guides()) {
                out.view_changed |= changed;
                continue;
            }
            match self.router.handle(
                sample,
                &mut self.camera,
                &self.viewport,
                content.active_tool,
            ) {
                Dispatch::ToTool(mut routed) => {
                    // Snapping happens *here*, between the router and the
                    // tool, so the position the tool acts on is the position
                    // the smart guide is drawn through. A tool that never
                    // places anything — a brush — is left alone; see
                    // [`snapping::tool_snaps`].
                    self.snap_routed(&mut routed, content);
                    out.tool_events.push(routed);
                }
                Dispatch::Navigated { changed, .. } => out.view_changed |= changed,
                Dispatch::Rejected(_) => {}
            }
        }
        if frame.pointer_gone {
            // Anything still holding the pointer after the synthesized release
            // goes now: a gesture with nothing behind it refuses every later
            // press, and the canvas would never respond again.
            self.router.cancel();
            self.guide_drag = None;
        }
        out.guide_drag = self.guide_drag;

        // Two separate streams, because egui splits them: a plain wheel arrives
        // as a scroll delta, while ctrl+wheel and a trackpad pinch are folded
        // into `zoom_delta` and never appear in the scroll at all. Reading only
        // the scroll — as this did — meant the ctrl+zoom branch of
        // [`InputRouter::wheel`] could never fire from a real window.
        let (scroll, zoom_delta, modifiers) = ctx.input(|i| {
            (
                geom::from_egui_vec2(i.smooth_scroll_delta),
                i.zoom_delta(),
                i.modifiers,
            )
        });
        if scroll != Vec2::ZERO || zoom_delta != 1.0 {
            if let Some(pos) = response
                .hover_pos()
                .filter(|p| !egui_owns_point(&ctx, geom::from_pos2(*p)))
            {
                let pos = geom::from_pos2(pos);
                if scroll != Vec2::ZERO {
                    let action = self.router.wheel(
                        scroll,
                        pos,
                        modifiers_from_egui(modifiers),
                        &self.viewport,
                    );
                    out.view_changed |=
                        InputRouter::apply_wheel(action, &mut self.camera, &self.viewport);
                }
                if zoom_delta != 1.0 && zoom_delta.is_finite() && zoom_delta > 0.0 {
                    // Anchored at the pointer: the pixel under the cursor stays
                    // under the cursor, which is the whole contract of a
                    // zoom-to-cursor.
                    out.view_changed |= InputRouter::apply_wheel(
                        WheelAction::Zoom {
                            anchor_pt: pos,
                            factor: zoom_delta,
                        },
                        &mut self.camera,
                        &self.viewport,
                    );
                }
            }
        }

        // Everything below reads one answer to "is the pointer the canvas's?",
        // so the coordinate readout, the cursor and the tool dispatch cannot
        // disagree about whether the pointer is on the image. A gesture already
        // in flight keeps the pointer whatever is drawn over it.
        //
        // The position comes from the context, not from `response.hover_pos()`:
        // the response reports nothing while another widget holds the pointer,
        // and "nothing" is exactly the case this has to tell apart from "over
        // the image".
        let egui_has_the_pointer = !self.router.is_gesture_active()
            && ctx
                .input(|i| i.pointer.hover_pos())
                .is_some_and(|p| egui_owns_point(&ctx, geom::from_pos2(p)));
        let pointer_pt = if egui_has_the_pointer {
            None
        } else {
            response.hover_pos().map(geom::from_pos2)
        };

        self.pointer_doc = pointer_pt
            .filter(|p| self.viewport.contains_pt(*p))
            .map(|p| self.camera.doc_of_screen_pt(&self.viewport, p));
        out.pointer_doc = self.pointer_doc;

        // ---- paint ----
        let painter = ui.painter_at(rect);
        paint::backdrop(
            &painter,
            &self.camera,
            &self.viewport,
            content.doc_size,
            &style,
        );
        out.grid_suppressed = paint::grid(
            &painter,
            &self.camera,
            &self.viewport,
            &self.grid,
            DocRect::of_canvas(content.doc_size),
            &style,
        );
        paint::guides(&painter, &self.camera, &self.viewport, &self.guides, &style);
        paint::smart_guides(
            &painter,
            &self.camera,
            &self.viewport,
            content.smart_guides,
            &style,
        );
        // …and the ones this frame's own snapping produced, which is what makes
        // the Snap toggle visible rather than merely available.
        paint::smart_guides(
            &painter,
            &self.camera,
            &self.viewport,
            &self.smart_guides,
            &style,
        );

        if let Some(crop_doc) = content.crop {
            paint::crop(&painter, &self.crop_overlay(crop_doc), &style);
        }

        if !content.selection_outline.is_empty() {
            let phase = ants::ants_phase(content.time_secs, &self.ants);
            let geometry = ants::build(
                content.selection_outline,
                &self.camera,
                &self.viewport,
                &self.ants,
                phase,
            );
            paint::ants(&painter, &geometry, &style);
            ctx.request_repaint();
        }

        if let Some((state, mode)) = content.transform {
            // A transform box scrolled entirely off screen draws nothing: the
            // furniture is many shapes, and every one of them would be clipped
            // away anyway.
            let bounds = handles::overlay_bounds(
                state,
                mode,
                &self.camera,
                &self.viewport,
                &self.handle_layout,
            );
            let on_screen =
                bounds.is_some_and(|b| !b.intersect(&self.viewport.content_bounds_pt()).is_empty());
            if on_screen {
                paint::transform(
                    &painter,
                    &self.camera,
                    &self.viewport,
                    &paint::TransformPaint {
                        state,
                        mode,
                        layout: &self.handle_layout,
                        active: content.active_handle,
                    },
                    &style,
                );
            }
        }

        if let Some((topology, selected)) = content.path {
            let projected = paths::project(topology, selected, &self.camera, &self.viewport);
            paint::path(&painter, &projected, ANCHOR_PT, CONTROL_PT, &style);
        }

        if let Some((geometry, origin)) = content.text {
            let projected = text_overlay::project(
                geometry,
                origin,
                &self.camera,
                &self.viewport,
                content.time_secs,
            );
            paint::text(&painter, &projected, &style);
            ctx.request_repaint();
        }

        if self.rulers_visible {
            paint::rulers(
                &painter,
                &self.camera,
                &self.viewport,
                self.outer_rect,
                &self.ruler_spec,
                &style,
            );
            // A ruler with no numbers on it is greyed by the painter; this is
            // the other half of the promise — hovering it says why, rather than
            // leaving the user to guess whether the rulers are broken.
            if let Some(hint) = rulers::oblique_hint(&self.camera, &self.viewport) {
                for (i, gutter) in rulers::gutters(self.outer_rect, self.ruler_thickness_pt)
                    .into_iter()
                    .enumerate()
                {
                    ui.interact(
                        gutter,
                        ui.id().with(("raster-ruler-gutter", i)),
                        egui::Sense::hover(),
                    )
                    .on_hover_text(hint);
                }
            }
        }

        // ---- cursor ----
        out.crop_grip = content.crop.and_then(|c| {
            pointer_pt
                .and_then(|p| crop::hit_test(p, &self.crop_overlay(c), self.handle_layout.grab()))
        });
        out.path_hit = content.path.and_then(|(topology, selected)| {
            pointer_pt.and_then(|p| {
                paths::hit_test(
                    topology,
                    selected,
                    p,
                    &self.camera,
                    &self.viewport,
                    ANCHOR_PT,
                    CONTROL_PT,
                )
            })
        });
        self.cursor = if egui_has_the_pointer {
            // Something of egui's own is under the pointer. It has already
            // chosen a cursor for itself; installing the brush ring here would
            // hide the system pointer over a dialog's own buttons.
            CanvasCursor::Arrow
        } else {
            self.resolve_cursor(content, pointer_pt)
        };
        if self.cursor == CanvasCursor::BrushOutline {
            if let (Some(brush), Some(doc)) = (content.brush, self.pointer_doc) {
                let cursor = brush_cursor::build(
                    brush,
                    self.pen_pressure,
                    doc,
                    &self.camera,
                    &self.viewport,
                );
                paint::brush(&painter, &cursor, &style);
            }
        }
        if !egui_has_the_pointer {
            ctx.set_cursor_icon(self.cursor.to_egui());
        }
        out.cursor = self.cursor;
        out
    }

    /// What is under `pos`, in the canvas's one priority order.
    ///
    /// The order is: the panels and the gutters (which are not the image at
    /// all), then the hand, then the transform box, then the crop, then a
    /// guide, then a path anchor, then bare image. Every branch **falls
    /// through** on a miss — a live transform whose handles the pointer is
    /// nowhere near must not swallow the guide underneath it.
    ///
    /// Both [`CanvasView::show`]'s dispatch and [`CanvasView::resolve_cursor`]
    /// read this, which is what makes the cursor a promise about what the next
    /// press will do rather than an independent guess.
    pub fn what_is_under(&self, content: &CanvasContent<'_>, pos: Vec2) -> Region {
        if !self.viewport.contains_pt(pos) && !self.router.is_gesture_active() {
            // The gutters are outside the viewport, but they belong to the
            // canvas: a guide can be pulled out of one.
            return match self.guide_gesture().gutter_at(pos) {
                Some(axis) => Region::Gutter(axis),
                None => Region::Panel,
            };
        }
        let base = cursor::cursor_for_tool_id(content.active_tool, self.precise_cursor);
        if self.router.space_held() || base == CanvasCursor::OpenHand {
            // The hand tool's own drag closes the hand too, not just the space
            // bar's.
            return Region::Hand {
                dragging: self.router.active_route() == Some(Route::Pan),
            };
        }
        if let Some(handle) = content.transform.and_then(|(state, mode)| {
            handles::hit_test(
                pos,
                state,
                mode,
                &self.camera,
                &self.viewport,
                &self.handle_layout,
            )
        }) {
            return Region::TransformHandle(handle);
        }
        if let Some(grip) = content
            .crop
            .and_then(|c| crop::hit_test(pos, &self.crop_overlay(c), self.handle_layout.grab()))
        {
            return Region::CropGrip(grip);
        }
        if let Some(index) = self
            .guides
            .hit_test(&self.camera, &self.viewport, pos, GUIDE_GRAB_PT)
        {
            if let Some(g) = self.guides.get(index) {
                return Region::Guide {
                    index,
                    locked: g.locked || self.guides.locked,
                };
            }
        }
        if let Some(hit) = content.path.and_then(|(topology, selected)| {
            paths::hit_test(
                topology,
                selected,
                pos,
                &self.camera,
                &self.viewport,
                ANCHOR_PT,
                CONTROL_PT,
            )
        }) {
            return Region::Path(hit);
        }
        Region::Image
    }

    /// Which cursor this frame wants, before it is installed.
    ///
    /// Separated from [`CanvasView::show`] so the priority order is testable
    /// without a window — and it is not a second copy of that order: everything
    /// below the guide-being-dragged special case is a translation of
    /// [`CanvasView::what_is_under`] into a cursor.
    pub fn resolve_cursor(
        &self,
        content: &CanvasContent<'_>,
        pointer_pt: Option<Vec2>,
    ) -> CanvasCursor {
        let base = cursor::cursor_for_tool_id(content.active_tool, self.precise_cursor);
        let Some(pos) = pointer_pt else {
            return base;
        };
        // A guide being dragged owns the cursor wherever the pointer is —
        // including in the ruler gutter it came out of, which is outside the
        // viewport and would otherwise read as "over a panel". This is about
        // the gesture in flight, not about what is under the pointer, which is
        // why it is not part of `what_is_under`.
        if let Some(drag) = self.guide_drag {
            let axis = match drag {
                GuideDrag::New { axis } => Some(axis),
                GuideDrag::Existing { index } => self.guides.get(index).map(|g| g.axis),
            };
            if let Some(axis) = axis {
                return cursor::resolve(
                    base,
                    CursorOverride::Guide {
                        horizontal: axis == Axis::Y,
                    },
                );
            }
        }
        let over = match self.what_is_under(content, pos) {
            Region::Panel => CursorOverride::OverPanel,
            Region::Gutter(axis) => CursorOverride::Guide {
                horizontal: axis == Axis::Y,
            },
            Region::Hand { dragging } => CursorOverride::Hand { dragging },
            Region::TransformHandle(handle) => {
                // Unwrapped rather than matched: `what_is_under` only returns
                // this variant when a transform is live.
                let quad = match content.transform {
                    Some((state, _)) => handles::screen_quad(state, &self.camera, &self.viewport),
                    None => return base,
                };
                CursorOverride::Handle(handles::cursor_for(handle, &quad))
            }
            Region::CropGrip(grip) => match content.crop {
                Some(c) => CursorOverride::Handle(crop::cursor_for(grip, &self.crop_overlay(c))),
                None => return base,
            },
            // A locked guide is hit-tested so the refusal can be *shown*.
            // Falling through would leave a resize cursor promising a drag that
            // will not happen.
            Region::Guide { locked: true, .. } => CursorOverride::Refused,
            Region::Guide { index, .. } => match self.guides.get(index) {
                Some(g) => CursorOverride::Guide {
                    horizontal: g.axis == Axis::Y,
                },
                None => CursorOverride::None,
            },
            // An anchor is dragged bodily; a control is aimed.
            Region::Path(PathHit::Anchor(_)) => CursorOverride::Handle(CanvasCursor::Move),
            Region::Path(PathHit::Control(_, _)) => CursorOverride::Handle(CanvasCursor::Crosshair),
            Region::Image => CursorOverride::None,
        };
        cursor::resolve(base, over)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::Selection;
    use glam::IVec2;
    use raster::PixelRect;

    fn ctx_with_size(width: f32, height: f32) -> egui::Context {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, height),
                )),
                ..Default::default()
            },
            |_| {},
        );
        ctx
    }

    /// The width of the dock every frame helper below reserves.
    const DOCK_PT: f32 = 160.0;

    /// One frame, with a dock reserved and `events` delivered, returning both
    /// what the canvas produced and the shapes it asked for.
    fn frame_shapes(
        view: &mut CanvasView,
        ctx: &egui::Context,
        content: &CanvasContent<'_>,
        events: Vec<egui::Event>,
    ) -> (CanvasOutput, Vec<egui::epaint::ClippedShape>) {
        let mut out = CanvasOutput::default();
        let full = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                egui::SidePanel::left("dock")
                    .exact_width(DOCK_PT)
                    .show(ctx, |ui| {
                        ui.label("panel");
                    });
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show(ctx, |ui| {
                        out = view.show(ui, content);
                    });
            },
        );
        let shapes = full.shapes.clone();
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        (out, shapes)
    }

    /// One frame with `events` delivered.
    fn frame_with(
        view: &mut CanvasView,
        ctx: &egui::Context,
        content: &CanvasContent<'_>,
        events: Vec<egui::Event>,
    ) -> CanvasOutput {
        frame_shapes(view, ctx, content, events).0
    }

    /// One quiet frame with every overlay live, with docks reserved.
    fn run_frame(
        view: &mut CanvasView,
        ctx: &egui::Context,
        content: &CanvasContent<'_>,
    ) -> CanvasOutput {
        frame_with(view, ctx, content, Vec::new())
    }

    fn press_at(p: Vec2) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(p.x, p.y),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn release_at(p: Vec2) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(p.x, p.y),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn move_to(p: Vec2) -> egui::Event {
        egui::Event::PointerMoved(egui::pos2(p.x, p.y))
    }

    #[test]
    fn a_full_frame_draws_and_the_viewport_excludes_the_dock() {
        let ctx = ctx_with_size(1200.0, 800.0);
        let mut view = CanvasView::for_document(Vec2::new(400.0, 300.0));
        let state = TransformState::new(PixelRect::new(20, 20, 100, 80));
        let outline = vec![Polyline {
            points: vec![
                IVec2::new(10, 10),
                IVec2::new(80, 10),
                IVec2::new(80, 60),
                IVec2::new(10, 60),
            ],
            closed: true,
        }];
        let brush = BrushSettings::default();
        let topology = paths::topology(&vector::Path::from_elements(vec![
            vector::PathEl::MoveTo(vector::point(0.0, 0.0)),
            vector::PathEl::LineTo(vector::point(50.0, 20.0)),
        ]));
        let content = CanvasContent {
            doc_size: Vec2::new(400.0, 300.0),
            active_tool: ToolId::Brush,
            selection_outline: &outline,
            transform: Some((&state, TransformMode::Scale)),
            active_handle: Some(Handle::Corner(0)),
            crop: Some(DocRect::new(Vec2::new(10.0, 10.0), Vec2::new(200.0, 150.0))),
            path: Some((&topology, &[0])),
            brush: Some(&brush),
            time_secs: 2.5,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        // The dock is 160pt wide, so the canvas starts to the right of it and
        // the ruler gutter takes a little more.
        assert!(
            view.viewport().origin_pt().x >= 160.0,
            "{:?}",
            view.viewport()
        );
        assert!(view.viewport().size_pt().x > 0.0);
        assert!(view.viewport().size_pt().x < 1200.0 - 160.0);
    }

    #[test]
    fn hiding_the_rulers_gives_the_image_the_gutter_back() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent::default();
        run_frame(&mut view, &ctx, &content);
        let with_rulers = view.viewport().size_pt();
        view.rulers_visible = false;
        run_frame(&mut view, &ctx, &content);
        let without = view.viewport().size_pt();
        assert!(without.x > with_rulers.x);
        assert!(without.y > with_rulers.y);
    }

    #[test]
    fn zoom_commands_move_the_view_as_advertised() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::new(4000.0, 3000.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());

        view.zoom_to_fit(Vec2::new(4000.0, 3000.0));
        let fit = view.camera.zoom;
        assert!(fit < 1.0, "a 4000px image cannot fit at 100%");
        let visible = view.camera.visible_doc_rect(view.viewport());
        assert!(visible.min.x <= 0.0 && visible.max.x >= 4000.0);

        view.zoom_to_fill(Vec2::new(4000.0, 3000.0));
        assert!(view.camera.zoom > fit);

        view.zoom_to_actual_pixels();
        assert_eq!(view.camera.zoom, 1.0);

        view.zoom_in();
        assert!(view.camera.zoom > 1.0);
        view.zoom_out();
        assert_eq!(view.camera.zoom, 1.0);
    }

    #[test]
    fn zoom_to_selection_frames_it_and_reports_when_there_is_none() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::new(2000.0, 2000.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());

        assert!(!view.zoom_to_selection(&Selection::None));
        assert!(!view.zoom_to_selection(&Selection::Rect {
            min: IVec2::new(5, 5),
            max: IVec2::new(5, 5),
        }));

        let before = view.camera.center;
        assert!(view.zoom_to_selection(&Selection::Rect {
            min: IVec2::new(1000, 1200),
            max: IVec2::new(1100, 1260),
        }));
        assert_ne!(view.camera.center, before);
        assert!((view.camera.center - Vec2::new(1050.0, 1230.0)).length() < 1e-3);
    }

    #[test]
    fn the_snap_helper_uses_the_cameras_scale() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(500.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());
        view.grid.visible = false;
        let _ = view.guides.add(Guide::new(Axis::X, 100.0));

        view.camera.set_zoom(1.0);
        let scale = view.camera.scale_pt(view.viewport());
        let threshold_doc = view.snap.threshold() / scale;
        let just_inside = 100.0 + threshold_doc * 0.9;
        let just_outside = 100.0 + threshold_doc * 1.1;
        // The y coordinate is deliberately not asserted on: y = 0 sits exactly
        // on the canvas's own top edge, which is a live candidate, so this is a
        // statement about the x axis alone.
        let caught = view.snap(Vec2::new(just_inside, 200.0), Vec2::splat(500.0), &[]);
        assert_eq!(caught.point.x, 100.0);
        assert_eq!(caught.x.unwrap().candidate.kind, SnapKind::Guide);
        let missed = view.snap(Vec2::new(just_outside, 200.0), Vec2::splat(500.0), &[]);
        assert!(missed.x.is_none());
        assert_eq!(missed.point.x, just_outside);
    }

    #[test]
    fn the_render_camera_reports_the_inset_region() {
        let ctx = ctx_with_size(1200.0, 800.0);
        let mut view = CanvasView::for_document(Vec2::splat(300.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());
        let rc = view.render_camera();
        assert!(
            rc.viewport_origin_px.x > 0.0,
            "the dock was not accounted for"
        );
        assert!(rc.viewport_size_px.x < rc.surface_size_px.x);
        assert_eq!(rc.zoom, view.camera.zoom);
    }

    #[test]
    fn the_cursor_follows_the_tool_the_panels_and_the_space_bar() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());
        let inside = view.viewport().center_pt();

        let brush_content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        assert_eq!(
            view.resolve_cursor(&brush_content, Some(inside)),
            CanvasCursor::BrushOutline
        );
        // Over a panel, the plain arrow.
        assert_eq!(
            view.resolve_cursor(&brush_content, Some(Vec2::new(2.0, 2.0))),
            CanvasCursor::Arrow
        );
        // The precise toggle.
        view.precise_cursor = true;
        assert_eq!(
            view.resolve_cursor(&brush_content, Some(inside)),
            CanvasCursor::PreciseCross
        );
        view.precise_cursor = false;
        // The space bar.
        view.router.set_space_held(true);
        assert_eq!(
            view.resolve_cursor(&brush_content, Some(inside)),
            CanvasCursor::OpenHand
        );
        view.router.set_space_held(false);
        // A transform handle under the pointer.
        let state = TransformState::new(PixelRect::new(0, 0, 200, 200));
        let transform_content = CanvasContent {
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, TransformMode::Scale)),
            ..CanvasContent::default()
        };
        let corner = view.camera.screen_pt_of(view.viewport(), Vec2::ZERO);
        assert_eq!(
            view.resolve_cursor(&transform_content, Some(corner)),
            CanvasCursor::ResizeNwSe
        );
        // A guide under the pointer.
        let _ = view.guides.add(Guide::new(Axis::Y, 40.0));
        let on_guide = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(0.0, 40.0));
        assert_eq!(
            view.resolve_cursor(&brush_content, Some(on_guide)),
            CanvasCursor::ResizeVertical
        );
    }

    #[test]
    fn pen_pressure_is_clamped_at_the_seam() {
        let mut view = CanvasView::default();
        view.set_pen_pressure(0.3);
        assert_eq!(view.pen_pressure, 0.3);
        view.set_pen_pressure(f32::NAN);
        assert_eq!(view.pen_pressure, 1.0);
        view.set_pen_pressure(-2.0);
        assert_eq!(view.pen_pressure, 0.0);
        view.set_pen_pressure(9.0);
        assert_eq!(view.pen_pressure, 1.0);
    }

    /// The whole point, end to end: a press over a dock must not reach the
    /// tool, and must not move the camera.
    #[test]
    fn a_press_on_the_dock_does_not_reach_the_tool_or_move_the_view() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());
        let before = view.camera;

        let content = CanvasContent {
            active_tool: ToolId::Hand,
            ..CanvasContent::default()
        };
        let mut out = CanvasOutput::default();
        let full = ctx.run(
            egui::RawInput {
                events: vec![
                    egui::Event::PointerButton {
                        // Well inside the 160pt dock.
                        pos: egui::pos2(40.0, 300.0),
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: egui::Modifiers::default(),
                    },
                    egui::Event::PointerMoved(egui::pos2(90.0, 340.0)),
                ],
                ..Default::default()
            },
            |ctx| {
                egui::SidePanel::left("dock")
                    .exact_width(160.0)
                    .show(ctx, |ui| {
                        ui.label("panel");
                    });
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = view.show(ui, &content);
                });
            },
        );
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        assert!(out.tool_events.is_empty(), "{:?}", out.tool_events);
        assert!(!out.view_changed);
        assert_eq!(view.camera, before, "the dock press panned the canvas");
        assert!(out.pointer_doc.is_none());
    }

    #[test]
    fn a_press_on_the_canvas_reaches_the_tool_in_document_space() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        run_frame(&mut view, &ctx, &CanvasContent::default());
        let at = view.viewport().center_pt();

        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        let mut out = CanvasOutput::default();
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
                egui::SidePanel::left("dock")
                    .exact_width(160.0)
                    .show(ctx, |ui| {
                        ui.label("panel");
                    });
                egui::CentralPanel::default().show(ctx, |ui| {
                    out = view.show(ui, &content);
                });
            },
        );
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        assert_eq!(out.tool_events.len(), 1, "{:?}", out.tool_events);
        let e = out.tool_events[0];
        assert_eq!(e.route, Route::Tool(ToolId::Brush));
        let want = view.camera.doc_of_screen_pt(view.viewport(), at);
        assert!((e.event.pos - want).length() < 1e-3);
    }

    #[test]
    fn the_canvas_survives_a_window_too_small_to_hold_its_panels() {
        let ctx = ctx_with_size(120.0, 90.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let mut out = CanvasOutput::default();
        let full = ctx.run(egui::RawInput::default(), |ctx| {
            egui::SidePanel::left("dock")
                .exact_width(400.0)
                .show(ctx, |ui| {
                    ui.label("panel");
                });
            egui::CentralPanel::default().show(ctx, |ui| {
                out = view.show(ui, &CanvasContent::default());
            });
        });
        let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        assert!(out.tool_events.is_empty());
    }

    /// The defect that made the canvas go dead: the press is in one frame and
    /// the pointer leaves in a *later* one, which is the only way it happens.
    /// Rebuilt-per-frame held state synthesizes no release, the router's
    /// gesture never ends, and every press after that is refused for ever.
    #[test]
    fn losing_the_pointer_a_frame_later_frees_the_canvas_for_the_next_press() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        let at = view.viewport().center_pt();

        // Frame 1: the press.
        let down = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert_eq!(down.tool_events.len(), 1);
        assert!(view.router.is_gesture_active());

        // Frame 2: the pointer leaves the window.
        let gone = frame_with(&mut view, &ctx, &content, vec![egui::Event::PointerGone]);
        assert_eq!(
            gone.tool_events.len(),
            1,
            "leaving the window must end the stroke, not drop it: {:?}",
            gone.tool_events
        );
        assert_eq!(gone.tool_events[0].phase, PointerPhase::Up);
        assert!(
            !view.router.is_gesture_active(),
            "the gesture outlived the pointer"
        );

        // Frame 3: a fresh press still works.
        let again = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert_eq!(
            again.tool_events.len(),
            1,
            "the canvas stopped responding after the pointer left"
        );
        assert_eq!(again.tool_events[0].phase, PointerPhase::Down);
    }

    /// Typing a space while renaming a layer must not arm the hand tool.
    #[test]
    fn a_space_typed_into_a_text_field_does_not_arm_the_hand() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        let mut name = String::from("My");
        let mut wants_keyboard = false;

        // Two frames: the first gives the field focus, the second types into it
        // while the space bar is held.
        for frame in 0..2 {
            let events = if frame == 0 {
                Vec::new()
            } else {
                vec![egui::Event::Key {
                    key: egui::Key::Space,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                }]
            };
            let full = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::SidePanel::left("dock")
                        .exact_width(DOCK_PT)
                        .show(ctx, |ui| {
                            let response = ui.text_edit_singleline(&mut name);
                            response.request_focus();
                        });
                    egui::CentralPanel::default().show(ctx, |ui| {
                        view.show(ui, &content);
                    });
                    wants_keyboard = ctx.wants_keyboard_input();
                },
            );
            let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        }

        assert!(
            wants_keyboard,
            "the test did not manage to focus the text field, so it proves nothing"
        );
        assert!(
            !view.router.space_held(),
            "typing a space into a layer name armed the temporary hand tool"
        );
        assert_ne!(
            view.resolve_cursor(&content, Some(view.viewport().center_pt())),
            CanvasCursor::OpenHand
        );
    }

    /// A click aimed at a dialog floating over the canvas is inside the canvas
    /// rectangle. It must not also land on the tool as a brush dab.
    #[test]
    fn a_press_inside_a_window_over_the_canvas_reaches_neither_tool_nor_camera() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };

        let window_at = |view: &CanvasView| {
            let c = view.viewport().center_pt();
            egui::Rect::from_center_size(egui::pos2(c.x, c.y), egui::vec2(200.0, 140.0))
        };

        let mut out = CanvasOutput::default();
        let mut before = view.camera;
        // egui hit-tests against the layers it knew at the start of the pass,
        // and a window's area is only measured once it has been laid out, so
        // the first two frames are the window settling and the third is the
        // one that presses into it.
        const PRESS_FRAME: usize = 2;
        for frame in 0..=PRESS_FRAME {
            let rect = window_at(&view);
            let events = if frame < PRESS_FRAME {
                Vec::new()
            } else {
                before = view.camera;
                vec![
                    press_at(geom::from_pos2(rect.center())),
                    move_to(geom::from_pos2(rect.center()) + Vec2::new(20.0, 10.0)),
                ]
            };
            let full = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::SidePanel::left("dock")
                        .exact_width(DOCK_PT)
                        .show(ctx, |ui| {
                            ui.label("panel");
                        });
                    egui::CentralPanel::default().show(ctx, |ui| {
                        out = view.show(ui, &content);
                    });
                    egui::Window::new("Levels")
                        .fixed_pos(rect.min)
                        .fixed_size(rect.size())
                        .show(ctx, |ui| {
                            ui.label("a dialog over the image");
                        });
                    // The test proves nothing unless the window really is over
                    // the point that is about to be pressed.
                    if frame == PRESS_FRAME {
                        assert!(
                            egui_owns_point(ctx, geom::from_pos2(rect.center())),
                            "the dialog is not over the canvas centre"
                        );
                    }
                },
            );
            let _ = ctx.tessellate(full.shapes, full.pixels_per_point);
        }

        assert!(
            out.tool_events.is_empty(),
            "a click in the dialog also painted: {:?}",
            out.tool_events
        );
        assert_eq!(view.camera, before, "the dialog press moved the canvas");
        assert!(!view.router.is_gesture_active());
        // The readout and the cursor agree with the dispatch: all three say the
        // pointer is not on the image. Before this, `pointer_doc` came from a
        // layer-aware `hover_pos` while the dispatch used a bare rectangle, so
        // the two contradicted each other.
        assert_eq!(
            out.pointer_doc, None,
            "the coordinate readout still tracked a pointer inside a dialog"
        );
        assert_eq!(
            out.cursor,
            CanvasCursor::Arrow,
            "the canvas hid the system pointer over a dialog"
        );
    }

    /// The backdrop is a hole. The renderer has already put the image on the
    /// surface, so an opaque fill across the content area erases it.
    #[test]
    fn no_backdrop_shape_covers_the_projected_document() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let doc = Vec2::new(300.0, 200.0);
        let mut view = CanvasView::for_document(doc);
        let content = CanvasContent {
            doc_size: doc,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        view.zoom_to_fit(doc);
        let (_, shapes) = frame_shapes(&mut view, &ctx, &content, Vec::new());

        let image = paint::document_bounds_pt(&view.camera, view.viewport(), doc)
            .expect("the document did not project")
            .intersect(&view.viewport().content_bounds_pt());
        assert!(!image.is_empty(), "the image is not on screen");
        let middle = geom::to_pos2(image.center());

        // *Any* opaque rectangle over the middle of the image erases it, not
        // only one drawn in the backdrop colour.
        let mut opaque_over_image = Vec::new();
        for clipped in &shapes {
            if let egui::Shape::Rect(r) = &clipped.shape {
                if r.fill.a() == u8::MAX && r.rect.contains(middle) {
                    opaque_over_image.push(r.rect);
                }
            }
        }
        assert!(
            opaque_over_image.is_empty(),
            "the backdrop was painted over the image at {middle:?}: {opaque_over_image:?}"
        );
        // …and it is still painted somewhere, so this is not passing by drawing
        // nothing at all.
        let bands = paint::backdrop_bands(&view.camera, view.viewport(), doc);
        assert!(!bands.is_empty(), "no backdrop was drawn at all");
    }

    /// Dragging out of the top ruler makes a horizontal guide, and the tool
    /// never sees the press that started it.
    #[test]
    fn dragging_out_of_the_ruler_creates_a_guide_and_the_tool_sees_nothing() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        assert!(view.guides.is_empty());

        // A point in the top gutter: inside the canvas rectangle, above the
        // viewport the image gets.
        let gutter = Vec2::new(view.viewport().center_pt().x, view.outer_rect.min.y + 2.0);
        assert!(!view.viewport().contains_pt(gutter));

        let down = frame_with(&mut view, &ctx, &content, vec![press_at(gutter)]);
        assert!(down.tool_events.is_empty(), "{:?}", down.tool_events);
        assert_eq!(view.guide_drag(), Some(GuideDrag::New { axis: Axis::Y }));
        assert_eq!(view.router.active_route(), Some(Route::Guide));

        let drop_at = view.viewport().center_pt();
        let dragged = frame_with(&mut view, &ctx, &content, vec![move_to(drop_at)]);
        assert!(dragged.tool_events.is_empty());
        assert_eq!(view.guides.len(), 1);
        let guide = *view.guides.get(0).unwrap();
        assert_eq!(guide.axis, Axis::Y);
        let want = view.camera.doc_of_screen_pt(view.viewport(), drop_at).y;
        assert!((guide.doc - want).abs() < 1e-3, "{} vs {want}", guide.doc);

        let up = frame_with(&mut view, &ctx, &content, vec![release_at(drop_at)]);
        assert!(up.tool_events.is_empty());
        assert!(view.guide_drag().is_none());
        assert!(!view.router.is_gesture_active());
        assert_eq!(view.guides.len(), 1);

        // …and the brush works again straight afterwards, well clear of the
        // guide that was just laid down — pressing *on* it would grab it again,
        // which is the point of laying it down.
        let clear = drop_at + Vec2::new(0.0, 80.0);
        assert!(view
            .guides
            .hit_test(&view.camera, view.viewport(), clear, GUIDE_GRAB_PT)
            .is_none());
        let after = frame_with(&mut view, &ctx, &content, vec![press_at(clear)]);
        assert_eq!(after.tool_events.len(), 1);
    }

    #[test]
    fn dragging_a_guide_back_into_the_ruler_deletes_it() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent::default();
        run_frame(&mut view, &ctx, &content);
        let middle = view.viewport().center_pt();
        let doc = view.camera.doc_of_screen_pt(view.viewport(), middle);
        view.guides.add(Guide::new(Axis::Y, doc.y)).unwrap();

        frame_with(&mut view, &ctx, &content, vec![press_at(middle)]);
        assert_eq!(view.guide_drag(), Some(GuideDrag::Existing { index: 0 }));
        let gutter = Vec2::new(middle.x, view.outer_rect.min.y + 2.0);
        frame_with(&mut view, &ctx, &content, vec![move_to(gutter)]);
        frame_with(&mut view, &ctx, &content, vec![release_at(gutter)]);
        assert!(
            view.guides.is_empty(),
            "the guide survived being thrown away"
        );
    }

    /// A locked guide says no, and swallows the press rather than letting it
    /// fall through and paint under the line the user was aiming at.
    #[test]
    fn a_locked_guide_refuses_with_a_cursor_and_eats_the_press() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Brush,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        let middle = view.viewport().center_pt();
        let doc = view.camera.doc_of_screen_pt(view.viewport(), middle);
        view.guides
            .add(Guide::new(Axis::Y, doc.y).locked())
            .unwrap();

        assert_eq!(
            view.resolve_cursor(&content, Some(middle)),
            CanvasCursor::NotAllowed,
            "a locked guide promised a drag it will not do"
        );
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(middle)]);
        assert!(out.tool_events.is_empty(), "{:?}", out.tool_events);
        assert!(view.guide_drag().is_none());
        // The refusal still ends cleanly, so the next press works.
        frame_with(&mut view, &ctx, &content, vec![release_at(middle)]);
        assert!(!view.router.is_gesture_active());
        let after = frame_with(
            &mut view,
            &ctx,
            &content,
            vec![press_at(middle + Vec2::new(0.0, 60.0))],
        );
        assert_eq!(after.tool_events.len(), 1);
    }

    /// The grid setting is not silently ignored: a grid too dense to draw is
    /// reported so the UI can say the grid is hidden.
    #[test]
    fn a_grid_too_dense_to_draw_is_reported_rather_than_dropped() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(4000.0));
        view.grid = GridSettings {
            visible: true,
            spacing_doc: 1.0,
            subdivisions: 1,
            pixel_grid: false,
        };
        let content = CanvasContent {
            doc_size: Vec2::splat(4000.0),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);

        view.camera.set_zoom(1.0 / 64.0);
        let dense = run_frame(&mut view, &ctx, &content);
        assert!(
            dense.grid_suppressed,
            "a one-pixel grid at 1/64 zoom drew nothing and said nothing"
        );

        view.camera.set_zoom(8.0);
        let sparse = run_frame(&mut view, &ctx, &content);
        assert!(!sparse.grid_suppressed, "the same grid at 8x reads fine");

        // A grid that is switched off is not "suppressed" — it is off.
        view.grid.visible = false;
        view.camera.set_zoom(1.0 / 64.0);
        assert!(!run_frame(&mut view, &ctx, &content).grid_suppressed);
    }

    /// Every crop grip that is drawn is grabbable, reported, and has a cursor.
    #[test]
    fn the_crop_grips_are_reported_and_take_their_own_cursors() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(400.0));
        let crop_doc = DocRect::new(Vec2::new(80.0, 60.0), Vec2::new(320.0, 260.0));
        let content = CanvasContent {
            doc_size: Vec2::splat(400.0),
            active_tool: ToolId::Crop,
            crop: Some(crop_doc),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        let overlay = view.crop_overlay(crop_doc);
        assert_eq!(overlay.grips.len(), 8);

        for (index, grip) in crop::GRIP_ORDER.iter().enumerate() {
            let at = geom::from_pos2(overlay.grips[index].center());
            let out = frame_with(&mut view, &ctx, &content, vec![move_to(at)]);
            assert_eq!(out.crop_grip, Some(*grip), "hovering grip {index}");
            assert_eq!(
                view.resolve_cursor(&content, Some(at)),
                crop::cursor_for(*grip, &overlay),
                "grip {index} took the wrong cursor"
            );
            // The crop tool's plain crosshair is not what a grip shows.
            assert_ne!(
                view.resolve_cursor(&content, Some(at)),
                cursor_for_tool_id(ToolId::Crop, false),
                "grip {index} is drawn but is indistinguishable from bare canvas"
            );
        }
        let middle = geom::from_pos2(overlay.keep.center());
        assert_eq!(
            view.resolve_cursor(&content, Some(middle)),
            CanvasCursor::Move
        );
    }

    /// The hand tool closes its own hand while panning, not just the space
    /// bar's stand-in.
    #[test]
    fn the_hand_tool_closes_its_hand_while_it_drags() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            active_tool: ToolId::Hand,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        let at = view.viewport().center_pt();
        assert_eq!(
            view.resolve_cursor(&content, Some(at)),
            CanvasCursor::OpenHand
        );
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert_eq!(view.router.active_route(), Some(Route::Pan));
        assert_eq!(
            out.cursor,
            CanvasCursor::ClosedHand,
            "the hand tool never closes its hand"
        );
        frame_with(&mut view, &ctx, &content, vec![release_at(at)]);
        assert_eq!(
            view.resolve_cursor(&content, Some(at)),
            CanvasCursor::OpenHand
        );
    }

    /// Path anchors and their control handles are reported and cursored.
    #[test]
    fn path_anchors_and_controls_are_reported_under_the_pointer() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let topology = paths::topology(&vector::Path::from_elements(vec![
            vector::PathEl::MoveTo(vector::point(40.0, 40.0)),
            vector::PathEl::CurveTo(
                vector::point(70.0, 10.0),
                vector::point(110.0, 10.0),
                vector::point(140.0, 40.0),
            ),
        ]));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::Brush,
            path: Some((&topology, &[0])),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);

        let anchor = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(40.0, 40.0));
        let out = frame_with(&mut view, &ctx, &content, vec![move_to(anchor)]);
        assert_eq!(out.path_hit, Some(PathHit::Anchor(0)));
        assert_eq!(
            view.resolve_cursor(&content, Some(anchor)),
            CanvasCursor::Move
        );

        let control = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(70.0, 10.0));
        let out = frame_with(&mut view, &ctx, &content, vec![move_to(control)]);
        assert_eq!(
            out.path_hit,
            Some(PathHit::Control(0, paths::ControlSide::Outgoing))
        );
        assert_eq!(
            view.resolve_cursor(&content, Some(control)),
            CanvasCursor::Crosshair
        );

        let empty = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(180.0, 180.0));
        let out = frame_with(&mut view, &ctx, &content, vec![move_to(empty)]);
        assert_eq!(out.path_hit, None);
        assert_eq!(
            view.resolve_cursor(&content, Some(empty)),
            CanvasCursor::BrushOutline,
            "bare canvas still gets the tool's own cursor"
        );
    }

    /// A transform box scrolled off screen draws nothing.
    #[test]
    fn a_transform_box_far_off_screen_draws_no_furniture() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let state = TransformState::new(PixelRect::new(0, 0, 40, 40));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, TransformMode::Scale)),
            ..CanvasContent::default()
        };
        let bare = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::FreeTransform,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);

        // In view, the box costs shapes.
        let with_box = frame_shapes(&mut view, &ctx, &content, Vec::new()).1.len();
        let without = frame_shapes(&mut view, &ctx, &bare, Vec::new()).1.len();
        assert!(
            with_box > without,
            "the transform box drew nothing even in view ({with_box} vs {without})"
        );

        // Scrolled far away, it costs none: the same frame with and without a
        // live transform asks the painter for exactly the same shapes.
        view.camera.center = Vec2::splat(500_000.0);
        assert!(
            handles::overlay_bounds(
                &state,
                TransformMode::Scale,
                &view.camera,
                view.viewport(),
                &view.handle_layout
            )
            .is_some_and(|b| b.intersect(&view.viewport().content_bounds_pt()).is_empty()),
            "the box is still on screen, so this proves nothing"
        );
        let far_with = frame_shapes(&mut view, &ctx, &content, Vec::new()).1.len();
        let far_without = frame_shapes(&mut view, &ctx, &bare, Vec::new()).1.len();
        assert_eq!(
            far_with, far_without,
            "the off-screen transform box still drew its handles"
        );
    }

    /// The rulers say why they are blank instead of leaving the user guessing.
    #[test]
    fn an_oblique_view_explains_its_blank_rulers() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        assert_eq!(
            rulers::oblique_hint(&view.camera, view.viewport()),
            None,
            "an upright view has working rulers"
        );
        view.camera.set_rotation(std::f32::consts::FRAC_PI_4);
        run_frame(&mut view, &ctx, &content);
        assert_eq!(
            rulers::oblique_hint(&view.camera, view.viewport()),
            Some(rulers::OBLIQUE_HINT)
        );
        let style = CanvasStyle::from_context(&ctx);
        assert_eq!(
            paint::gutter_fills(&view.camera, view.viewport(), &style),
            [style.ruler_disabled, style.ruler_disabled]
        );
    }

    /// The contradiction this module was rejected for: a guide sitting exactly
    /// on a transform handle showed a scale cursor and then dragged the guide,
    /// and the handle underneath could not be grabbed at all.
    ///
    /// Guides get put on layer and canvas edges, which is precisely where
    /// transform handles live, so this is the common case and not a corner one.
    #[test]
    fn a_guide_lying_on_a_transform_handle_does_not_steal_the_press() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let state = TransformState::new(PixelRect::new(0, 0, 200, 200));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, TransformMode::Scale)),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        // A guide on the document's left edge — where the corner handle is.
        view.guides.add(Guide::new(Axis::X, 0.0)).unwrap();
        let at = view.camera.screen_pt_of(view.viewport(), Vec2::ZERO);
        assert!(
            view.guides
                .hit_test(&view.camera, view.viewport(), at, GUIDE_GRAB_PT)
                .is_some(),
            "the guide is not under the pointer, so this proves nothing"
        );

        // One order, and the handle is above the guide in it.
        assert_eq!(
            view.what_is_under(&content, at),
            Region::TransformHandle(Handle::Corner(0))
        );
        let cursor = view.resolve_cursor(&content, Some(at));
        assert_eq!(cursor, CanvasCursor::ResizeNwSe);

        // …and the press does what the cursor promised.
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert_eq!(
            out.tool_events.len(),
            1,
            "the guide swallowed a press aimed at the transform handle: {out:?}"
        );
        assert_eq!(out.tool_events[0].route, Route::Tool(ToolId::FreeTransform));
        assert_eq!(view.guide_drag(), None);
        assert_ne!(view.router.active_route(), Some(Route::Guide));
        assert_eq!(out.cursor, cursor, "the cursor changed under the press");
    }

    /// The converse, and the second half of the same defect: a transform handle
    /// *miss* used to return "no override" from inside its own branch, so the
    /// crop, the guides and the paths below it were never consulted for as long
    /// as a transform happened to be live.
    #[test]
    fn a_transform_miss_falls_through_to_the_guide_under_the_pointer() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        // A small box in the corner, so the guide below is nowhere near it —
        // note that the *inside* of a transform box is itself a handle
        // (`Handle::Inside` moves the whole thing), so "away from the handles"
        // means outside the box as well as off its furniture.
        let state = TransformState::new(PixelRect::new(0, 0, 80, 80));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, TransformMode::Scale)),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        view.guides.add(Guide::new(Axis::Y, 150.0)).unwrap();
        let at = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(150.0, 150.0));
        assert!(
            handles::hit_test(
                at,
                &state,
                TransformMode::Scale,
                &view.camera,
                view.viewport(),
                &view.handle_layout
            )
            .is_none(),
            "the sample point is on a handle, so this proves nothing"
        );

        assert_eq!(
            view.what_is_under(&content, at),
            Region::Guide {
                index: 0,
                locked: false
            }
        );
        assert_eq!(
            view.resolve_cursor(&content, Some(at)),
            CanvasCursor::ResizeVertical,
            "a live transform swallowed the guide underneath it"
        );
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert!(out.tool_events.is_empty(), "{:?}", out.tool_events);
        assert_eq!(view.guide_drag(), Some(GuideDrag::Existing { index: 0 }));
    }

    /// The same rule for the crop: a grip outranks a guide drawn under it, and
    /// the press follows the cursor.
    #[test]
    fn a_guide_lying_on_a_crop_grip_does_not_steal_the_press() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(400.0));
        let crop_doc = DocRect::new(Vec2::new(80.0, 60.0), Vec2::new(320.0, 260.0));
        let content = CanvasContent {
            doc_size: Vec2::splat(400.0),
            active_tool: ToolId::Crop,
            crop: Some(crop_doc),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        // A guide standing on the crop's left edge, where its grips are.
        view.guides.add(Guide::new(Axis::X, 80.0)).unwrap();
        let overlay = view.crop_overlay(crop_doc);
        let at = geom::from_pos2(overlay.grips[0].center());
        assert!(
            view.guides
                .hit_test(&view.camera, view.viewport(), at, GUIDE_GRAB_PT)
                .is_some(),
            "the guide is not under the grip, so this proves nothing"
        );

        let grip = crop::hit_test(at, &overlay, view.handle_layout.grab()).unwrap();
        assert_eq!(view.what_is_under(&content, at), Region::CropGrip(grip));
        assert_eq!(
            view.resolve_cursor(&content, Some(at)),
            crop::cursor_for(grip, &overlay)
        );
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(at)]);
        assert_eq!(view.guide_drag(), None, "the guide took the grip's press");
        assert_eq!(out.tool_events.len(), 1, "{:?}", out.tool_events);
    }

    /// Every position the cursor is asked about is resolved from the same
    /// region the dispatch uses. This is the property the two used to break.
    #[test]
    fn the_cursor_and_the_press_read_one_priority_order() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(200.0));
        let state = TransformState::new(PixelRect::new(0, 0, 200, 200));
        let content = CanvasContent {
            doc_size: Vec2::splat(200.0),
            active_tool: ToolId::FreeTransform,
            transform: Some((&state, TransformMode::Scale)),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        view.guides.add(Guide::new(Axis::X, 0.0)).unwrap();
        view.guides.add(Guide::new(Axis::Y, 100.0)).unwrap();

        for doc in [
            Vec2::ZERO,
            Vec2::new(150.0, 100.0),
            Vec2::new(100.0, 100.0),
            Vec2::new(60.0, 40.0),
            Vec2::new(200.0, 200.0),
        ] {
            let at = view.camera.screen_pt_of(view.viewport(), doc);
            let region = view.what_is_under(&content, at);
            let cursor = view.resolve_cursor(&content, Some(at));
            // A guide only claims a press where the region says it may, and
            // wherever it may not, the guides are not what the cursor shows.
            if !region.is_guides() {
                assert!(
                    !matches!(
                        cursor,
                        CanvasCursor::ResizeVertical | CanvasCursor::ResizeHorizontal
                    ) || matches!(region, Region::TransformHandle(_) | Region::CropGrip(_)),
                    "{doc:?}: cursor {cursor:?} promises a guide drag in {region:?}"
                );
            }
        }
    }

    /// Snapping is a product feature, not a library one: with the toggle on, a
    /// tool that places things receives the *snapped* position.
    #[test]
    fn a_placing_gesture_is_snapped_on_its_way_to_the_tool_and_a_brush_is_not() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(500.0));
        view.grid.visible = false;
        let move_content = CanvasContent {
            doc_size: Vec2::splat(500.0),
            active_tool: ToolId::Move,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &move_content);
        view.camera.set_zoom(1.0);
        view.guides.add(Guide::new(Axis::X, 250.0)).unwrap();

        // Six document pixels off the guide, at 100% and 1x: past the 4pt
        // radius that would *grab* the guide, inside the 8pt snap threshold.
        let near = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(256.0, 300.0));
        assert!(
            view.guides
                .hit_test(&view.camera, view.viewport(), near, GUIDE_GRAB_PT)
                .is_none(),
            "the press would grab the guide rather than snap to it"
        );
        let out = frame_with(&mut view, &ctx, &move_content, vec![press_at(near)]);
        assert_eq!(out.tool_events.len(), 1, "{out:?}");
        assert!(
            (out.tool_events[0].event.pos.x - 250.0).abs() < 1e-3,
            "the Move tool was handed {:?}, unsnapped",
            out.tool_events[0].event.pos
        );
        frame_with(&mut view, &ctx, &move_content, vec![release_at(near)]);

        // The same press with the snap switched off lands where the hand was.
        view.snap.enabled = false;
        let out = frame_with(&mut view, &ctx, &move_content, vec![press_at(near)]);
        let raw = view.camera.doc_of_screen_pt(view.viewport(), near);
        assert!((out.tool_events[0].event.pos.x - raw.x).abs() < 1e-3);
        frame_with(&mut view, &ctx, &move_content, vec![release_at(near)]);
        view.snap.enabled = true;

        // …and a brush dab is never snapped, whatever the toggle says.
        let brush = BrushSettings::default();
        let brush_content = CanvasContent {
            doc_size: Vec2::splat(500.0),
            active_tool: ToolId::Brush,
            brush: Some(&brush),
            ..CanvasContent::default()
        };
        let out = frame_with(&mut view, &ctx, &brush_content, vec![press_at(near)]);
        assert_eq!(out.tool_events.len(), 1, "{out:?}");
        assert!(
            (out.tool_events[0].event.pos.x - raw.x).abs() < 1e-3,
            "a brush dab was pulled onto a guide"
        );
    }

    /// A snap that catches a layer edge draws the line that explains it.
    #[test]
    fn a_snap_against_a_layer_edge_leaves_a_smart_guide_on_screen() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(500.0));
        view.grid.visible = false;
        let layers = [DocRect::new(
            Vec2::new(120.0, 120.0),
            Vec2::new(240.0, 240.0),
        )];
        let content = CanvasContent {
            doc_size: Vec2::splat(500.0),
            active_tool: ToolId::Move,
            snap_layers: &layers,
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);
        view.camera.set_zoom(1.0);

        // Two pixels off the layer's centre, which is a smart-guide kind.
        let near = view
            .camera
            .screen_pt_of(view.viewport(), Vec2::new(182.0, 300.0));
        let out = frame_with(&mut view, &ctx, &content, vec![press_at(near)]);
        assert_eq!(out.tool_events.len(), 1);
        assert!((out.tool_events[0].event.pos.x - 180.0).abs() < 1e-3);
        assert!(
            view.smart_guides()
                .iter()
                .any(|h| h.candidate.kind == SnapKind::LayerCenter),
            "the snap caught but drew no line explaining why"
        );
        // The release ends the gesture and takes the line with it.
        frame_with(&mut view, &ctx, &content, vec![release_at(near)]);
        assert!(view.smart_guides().is_empty());
    }

    /// A ctrl+wheel zoom keeps the document point under the cursor where it
    /// was — and it has to arrive at all, which it did not: egui folds a
    /// ctrl+wheel into `zoom_delta` and leaves the scroll delta at zero, so
    /// reading only the scroll meant the zoom branch never ran in a real
    /// window.
    #[test]
    fn a_ctrl_wheel_zooms_the_canvas_about_the_cursor() {
        let ctx = ctx_with_size(1000.0, 700.0);
        let mut view = CanvasView::for_document(Vec2::splat(400.0));
        let content = CanvasContent {
            doc_size: Vec2::splat(400.0),
            ..CanvasContent::default()
        };
        run_frame(&mut view, &ctx, &content);

        let at = view.viewport().center_pt() + Vec2::new(70.0, -40.0);
        let before_zoom = view.camera.zoom;
        let under_cursor = view.camera.doc_of_screen_pt(view.viewport(), at);

        let out = frame_with(
            &mut view,
            &ctx,
            &content,
            vec![
                move_to(at),
                egui::Event::MouseWheel {
                    unit: egui::MouseWheelUnit::Line,
                    delta: egui::vec2(0.0, 3.0),
                    modifiers: egui::Modifiers::COMMAND,
                },
            ],
        );
        assert!(
            view.camera.zoom > before_zoom,
            "a ctrl+wheel did not reach the canvas at all ({} vs {before_zoom})",
            view.camera.zoom
        );
        assert!(out.view_changed);
        let still = view.camera.doc_of_screen_pt(view.viewport(), at);
        assert!(
            (still - under_cursor).length() < 0.5,
            "the point under the cursor moved from {under_cursor:?} to {still:?}"
        );
    }

    /// The viewport is derived from the theme actually installed, not from a
    /// default one — the gutter depth is a token and could stop agreeing.
    #[test]
    fn the_viewport_uses_the_contexts_own_theme() {
        for theme in design::Theme::ALL {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, *theme);
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(900.0, 600.0),
                    )),
                    ..Default::default()
                },
                |_| {},
            );
            let mut view = CanvasView::for_document(Vec2::splat(100.0));
            run_frame(&mut view, &ctx, &CanvasContent::default());
            let want = CanvasStyle::new(*theme, ctx.pixels_per_point()).ruler_thickness_pt;
            assert_eq!(
                view.viewport().origin_pt().y - view.outer_rect.min.y,
                want,
                "{theme:?}: the gutter came off the wrong theme"
            );
        }
    }
}
