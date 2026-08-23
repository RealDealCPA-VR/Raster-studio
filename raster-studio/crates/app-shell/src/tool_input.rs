//! The wire from the pointer to the active tool.
//!
//! Everything below the window is already there: `tools` holds a state machine
//! per tool, `editor-core` holds the history, the compositor holds the pixels.
//! What was missing was this — the piece that turns a mouse drag into
//! [`tools::PointerEvent`]s, hands them to the tool the palette says is
//! selected, and puts the commands the gesture emits through
//! [`editor_core::History`]. Without it a left-drag panned the view whatever
//! tool was chosen, and `tools`, `selection`, `vector` and `filters` were
//! unreachable at runtime.
//!
//! ```text
//!   winit MouseInput / CursorMoved
//!          |  screen pixels
//!          v
//!   ui::canvas::InputRouter          who owns this gesture: tool, or camera
//!          |  document pixels
//!          v
//!   Box<dyn Tool>  <-- ToolContext   active layer, selection, colours, brush
//!          |
//!          v
//!   Editor::apply_command -> History  one gesture, one undoable step
//! ```
//!
//! # Where the canvas is
//!
//! The canvas is the **whole window**. [`crate::shell::Shell::redraw`] renders
//! [`render::Canvas`] over the entire surface and hands every document's camera
//! the surface size ([`crate::doc::OpenDocument::set_viewport`]), so the image
//! is centred on the window and the panels are an egui overlay drawn on top of
//! it. The viewport this module routes against is therefore built from
//! `camera.viewport_size` — the *same* number [`render::Camera::screen_to_image`]
//! divides by — which is what makes a click land on the pixel under the cursor
//! rather than a panel's width away from it. Panels are excluded by the
//! `over_panel` flag the shell reads from egui, not by shrinking the rectangle;
//! shrinking it would move every document coordinate.
//!
//! # Who owns a gesture
//!
//! [`ui::canvas::InputRouter`], unchanged: it decides at pointer-**down** and
//! holds that decision until pointer-up, so a drag that wanders onto a panel
//! keeps painting and a press that *starts* on a panel reaches neither the tool
//! nor the camera. The space bar is not a second mechanism — it works because
//! [`Editor::effective_tool`] already answers `Hand` while Space is held, and
//! the router maps `Hand` to a pan.
//!
//! # A gesture is pinned to the document it was aimed at
//!
//! The route is not the whole claim: *which document* is also decided at
//! pointer-down and held. [`ToolPointer`] records the active
//! [`crate::DocumentId`] when the router claims a gesture and compares it on
//! every later sample, because the active tab can change while the button is
//! held — Ctrl+Tab and Ctrl+W are bound and the keyboard is live during a drag.
//! Without the pin, `editor.active_mut()` was re-read per sample and the whole
//! stroke, including the part dragged over the old tab, was rasterised into
//! whichever document happened to be active at pointer-**up** and pushed onto
//! *its* history, leaving the document the user actually dragged on untouched.
//! A mismatch cancels the gesture (router and [`Tool::cancel`] both) and
//! refuses the sample as [`Refusal::WrongDocument`]. This is the same rule the
//! [`Refusal::NoDocument`] guard keeps — no gesture may outlive the document it
//! was aimed at — applied to a document that was replaced rather than closed.
//!
//! # The brush belongs to the tool
//!
//! At pointer-down, and only there, the tool is handed
//! [`Editor::brush_for`]`(id)` — *that tool's* brush, not one application-wide
//! set of settings. It has to be per tool because for the stamping tools the
//! settings are the tool: the Pencil and the Brush both paint through
//! `tools::StrokeOp::Paint` and differ in nothing but
//! `BrushSettings::pencil(1.0)`, so a shared brush makes them one tool and
//! makes Blur, Smudge, Dodge, Clone and four more paint at sizes and hardnesses
//! they were never given. [`Editor`] keeps the slots and seeds an untouched one
//! from [`tools::registry::make`], so `[`, `]` and the options bar move the
//! selected tool's brush and leave every other tool as the registry built it.
//!
//! # What this cannot do yet
//!
//! * **Rotate View changes nothing on screen.** [`render::Camera`] is
//!   axis-aligned by construction (see its `clip_to_uv`), so the rotation the
//!   tool applies to the mirrored camera has nowhere to be written back to and
//!   is dropped. The gesture reaches the tool and the tool is correct; the
//!   renderer cannot show the result. Hand and Zoom write back in full.
//! * **A selection gesture is not undoable.** `editor-core` models the
//!   selection as a field rather than a command, so a marquee changes
//!   `Document::selection` directly and marks the document dirty. That is the
//!   gap [`tools::SelectionEdit`]'s own documentation names, not a shortcut
//!   taken here.
//! * **A bare hover reaches no tool.** Only samples inside a claimed gesture
//!   are forwarded. Nothing in this shell draws the previews a hover would feed
//!   (the polygonal lasso's rubber band, the brush ring), and building a
//!   [`ToolContext`] per mouse-move would clone the selection mask sixty times
//!   a second for nobody.
//! * **A stroke is invisible until the button is released.**
//!   [`tools::StrokeTool::commit`] is what emits the single
//!   `Command::PaintTiles`, and it is called only from `on_pointer_up`, so every
//!   Move sample of a drag emits nothing, adds no history step and asks for no
//!   repaint. The document's [`editor_core::PixelStore`] references — the thing
//!   the compositor reads — are rewritten by that command and by nothing else,
//!   so there is no live preview to show in the meantime: the canvas is
//!   unchanged for the whole drag and the stroke appears at the release. Every
//!   stamping tool routed here (Brush, Pencil, Eraser, Clone, Blur, Smudge,
//!   Dodge and the rest) is a `StrokeTool` and behaves the same way. Pinned by
//!   `a_stroke_is_invisible_until_the_button_is_released`.
//! * **A shape gesture draws nothing on the canvas.** The seven shape tools —
//!   Rectangle, RoundedRectangle, Ellipse, Polygon, Star, Line and CustomShape —
//!   route and run correctly: a drag emits one undoable `Command::CreateLayer`
//!   holding a visible [`layer_model::LayerKind::Shape`] with the dragged path,
//!   and the layer appears in the Layers panel. It appears *nowhere else*,
//!   because the compositor has no rasteriser for it: see the
//!   `LayerKind::Text(_) | LayerKind::Shape(_) | LayerKind::SmartObject(_) => {}`
//!   arm of `compositor::composite`, "No rasterizer for these yet; they
//!   contribute nothing." So a shape gesture costs an undo step and a layer row
//!   and leaves the composited pixels byte-identical. Pinned by
//!   `a_shape_gesture_creates_a_layer_the_compositor_cannot_draw` — which fails
//!   the day that arm grows a rasteriser, and this bullet must go with it.

use glam::Vec2;

use compositor::{MemoryTileSource, TileSource};
use editor_core::{PixelKey, PixelStore};
use raster::{PixelRect, TileCoord, TileHash};
use render::{Camera, MAX_ZOOM, MIN_ZOOM};
use tools::{registry, PaintTarget, TileAccess, Tool, ToolContext, ToolId};
use ui::canvas::{
    CanvasCamera, Dispatch, InputRouter, PanelInsets, PointerInput, PointerPhase, Rejected, Route,
    Viewport,
};

use crate::doc::DocumentId;
use crate::editor::Editor;

/// A [`tools::TileAccess`] over one open document.
///
/// The two halves of a document's pixels live in different places: the
/// *references* (tile coordinate to content hash) are in
/// [`editor_core::Document::pixels`], and the *bytes* are in the
/// [`MemoryTileSource`] the compositor reads. A tool needs both, so this pairs
/// them. Reads resolve through the document, writes land in the byte store —
/// which is exactly right, because the reference change is not this type's to
/// make: it arrives later as the [`editor_core::Command`] the tool emits,
/// applied through history.
pub struct DocumentTiles<'a> {
    refs: &'a PixelStore,
    bytes: &'a mut MemoryTileSource,
}

impl<'a> DocumentTiles<'a> {
    pub fn new(refs: &'a PixelStore, bytes: &'a mut MemoryTileSource) -> Self {
        Self { refs, bytes }
    }
}

impl TileAccess for DocumentTiles<'_> {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.refs.tiles(key).and_then(|m| m.get(coord))
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        self.bytes.tile(hash)
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        // `insert_bytes` files bytes under `TileHash::of(bytes)`, which is the
        // content-addressing `TileAccess::store` requires.
        self.bytes.insert_bytes(data)
    }
}

/// The rectangle pointer coordinates are measured against: the whole surface.
///
/// See the module docs — the canvas is drawn over the entire window, so the
/// insets are empty and the scale is one, which makes a point in this viewport
/// a physical pixel of the surface, the unit `winit` reports and the unit
/// [`render::Camera`] measures in.
pub fn canvas_viewport(surface_px: Vec2) -> Viewport {
    Viewport::new(surface_px, PanelInsets::NONE, 1.0)
}

/// The router's camera, mirrored from the document's.
///
/// Rotation and flip are zero because [`render::Camera`] cannot express them.
pub fn canvas_camera_of(camera: &Camera) -> CanvasCamera {
    CanvasCamera {
        center: camera.center,
        zoom: camera.zoom,
        rotation: 0.0,
        flip_x: false,
        flip_y: false,
    }
}

/// Write a navigated mirror back onto the document's camera. Reports whether
/// anything actually moved.
///
/// The document's camera is the authority — it is what the renderer reads — so
/// this, and not the router's own `changed` flag, is what says a repaint is
/// owed. The two disagree for exactly one gesture: a Rotate View drag moves the
/// mirror and nothing else, and reporting that as a change would repaint an
/// identical frame.
pub fn write_camera_back(from: &CanvasCamera, to: &mut Camera) -> bool {
    let center = if from.center.is_finite() {
        from.center
    } else {
        to.center
    };
    // Clamped again on the way in: the router's camera allows a wider range
    // than `render` does, so an unclamped write could put the surface at a zoom
    // the renderer never accepts from any other route.
    let zoom = if from.zoom.is_finite() {
        from.zoom.clamp(MIN_ZOOM, MAX_ZOOM)
    } else {
        to.zoom
    };
    let moved = center != to.center || zoom != to.zoom;
    to.center = center;
    to.zoom = zoom;
    moved
}

/// Why a pointer sample did nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The pointer was over the chrome and no gesture was already running.
    OverPanel,
    /// No document is open, so there is nothing to aim at.
    NoDocument,
    /// The active document is no longer the one the gesture started on — the
    /// user switched or closed a tab with the button still held. The gesture is
    /// cancelled rather than redirected; see the module docs.
    WrongDocument,
    /// The router refused it — see [`ui::canvas::Rejected`].
    Router(Rejected),
}

/// What one pointer sample did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PointerOutcome {
    /// Who the gesture belongs to, when it belongs to anyone.
    pub route: Option<Route>,
    /// Why nothing happened.
    pub refused: Option<Refusal>,
    /// The sample was handed to the active tool.
    pub reached_tool: bool,
    /// Undoable steps this sample added to the document's history.
    pub steps: usize,
    /// The document's selection changed.
    pub selection_changed: bool,
    /// The document's camera moved.
    pub view_changed: bool,
    /// A colour the gesture picked, already installed as the foreground.
    pub picked: Option<[f32; 4]>,
    /// What the tool refused, if it refused.
    pub failed: Option<String>,
}

impl PointerOutcome {
    /// `true` when the document itself is different because of this sample.
    pub fn changed_document(&self) -> bool {
        self.steps > 0 || self.selection_changed
    }

    /// `true` when the window has to be drawn again.
    pub fn needs_repaint(&self) -> bool {
        self.changed_document() || self.view_changed || self.picked.is_some()
    }
}

/// The pointer half of the shell: the gesture router and the live tool.
///
/// One tool instance is kept alive across the events of a gesture, because a
/// tool *is* a state machine — a stroke that rebuilt its tool between the press
/// and the release would emit nothing at all.
#[derive(Default)]
pub struct ToolPointer {
    router: InputRouter,
    /// The live tool and the id it was built for.
    current: Option<(ToolId, Box<dyn Tool>)>,
    /// The document the running gesture was aimed at, while one is running.
    ///
    /// Identity, not index: a closed tab renumbers the ones after it, so an
    /// index would silently re-aim the stroke at the document that slid into
    /// the slot. See the module docs.
    aimed_at: Option<DocumentId>,
}

impl ToolPointer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Who owns the pointer right now, if anyone.
    pub fn active_route(&self) -> Option<Route> {
        self.router.active_route()
    }

    /// `true` while a button is down and some route owns it.
    pub fn is_gesture_active(&self) -> bool {
        self.router.is_gesture_active()
    }

    /// `true` while the tool itself has a gesture in progress.
    pub fn is_tool_active(&self) -> bool {
        self.current.as_ref().is_some_and(|(_, t)| t.is_active())
    }

    /// The id of the live tool, for tests and for the status bar.
    pub fn live_tool(&self) -> Option<ToolId> {
        self.current.as_ref().map(|(id, _)| *id)
    }

    /// The document the running gesture belongs to, while one is running.
    pub fn aimed_at(&self) -> Option<DocumentId> {
        self.aimed_at
    }

    /// The live instance of `id`, building it if the active tool changed.
    fn tool(&mut self, id: ToolId) -> &mut dyn Tool {
        if self.current.as_ref().map(|(have, _)| *have) != Some(id) {
            self.current = Some((id, registry::make(id)));
        }
        self.current
            .as_mut()
            .map(|(_, tool)| tool.as_mut())
            .expect("just built")
    }

    /// Abandon whatever is in progress: Escape, or the window losing focus.
    ///
    /// Reports whether there was anything to abandon. The camera keeps whatever
    /// it has already been panned to — a view change is not undoable, so there
    /// is nothing to roll back — and [`Tool::cancel`] is contracted to emit
    /// nothing, which is why the context it is given is never drained.
    pub fn cancel(&mut self, editor: &mut Editor) -> bool {
        let had = self.router.is_gesture_active() || self.is_tool_active();
        self.router.cancel();
        self.aimed_at = None;
        if let Some((_, tool)) = &mut self.current {
            match editor.active_mut() {
                Some(doc) => {
                    let canvas = doc.canvas_rect();
                    let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles);
                    tool.cancel(&mut ToolContext::new(&mut access, canvas));
                }
                None => {
                    // No document, so no pixels to offer: the contract says
                    // cancel reads nothing, and an empty store keeps that
                    // honest rather than reaching for a document that is gone.
                    let mut scratch = tools::MemoryTiles::new();
                    tool.cancel(&mut ToolContext::new(
                        &mut scratch,
                        PixelRect::new(0, 0, 0, 0),
                    ));
                }
            }
        }
        had
    }

    /// Abandon the gesture without touching any document.
    ///
    /// The wrong-document path's cancel: handing the tool the tiles of a
    /// document the gesture was never aimed at, to cancel against, would be the
    /// very confusion the guard exists to prevent. [`Tool::cancel`] is
    /// contracted to read nothing and emit nothing, so an empty store keeps
    /// that honest instead of reaching for a document that is no longer there.
    fn cancel_detached(&mut self) -> bool {
        let had = self.router.is_gesture_active() || self.is_tool_active();
        self.router.cancel();
        self.aimed_at = None;
        if let Some((_, tool)) = &mut self.current {
            let mut scratch = tools::MemoryTiles::new();
            tool.cancel(&mut ToolContext::new(
                &mut scratch,
                PixelRect::new(0, 0, 0, 0),
            ));
        }
        had
    }

    /// Route one pointer sample.
    ///
    /// `over_panel` is the shell's answer to "is the chrome under the cursor" —
    /// egui's, since egui draws the panels. It is consulted only while no
    /// gesture is running: once a drag is claimed it must survive the cursor
    /// crossing a panel.
    pub fn handle(
        &mut self,
        editor: &mut Editor,
        input: PointerInput,
        over_panel: bool,
    ) -> PointerOutcome {
        let mut out = PointerOutcome::default();
        if over_panel && !self.router.is_gesture_active() {
            out.refused = Some(Refusal::OverPanel);
            return out;
        }
        let Some(active_id) = editor.active().map(|doc| doc.id()) else {
            // The last tab closed under a held button, perhaps. Nothing to aim
            // at, and no gesture may outlive the document it was aimed at —
            // the tool's half of it least of all, since its next stroke would
            // otherwise begin half-finished.
            self.cancel(editor);
            out.refused = Some(Refusal::NoDocument);
            return out;
        };
        if self.aimed_at.is_some_and(|aimed| aimed != active_id) {
            // The user switched tabs — Ctrl+Tab, or Ctrl+W onto a survivor —
            // with the button still down. The rest of this drag belongs to a
            // document that is no longer in front of it, and applying it to the
            // one that is would rasterise the stroke into the wrong image and
            // push the step onto the wrong history. Same rule as the branch
            // above: a gesture does not outlive the document it was aimed at.
            self.cancel_detached();
            out.refused = Some(Refusal::WrongDocument);
            return out;
        }

        let effective = editor.effective_tool();
        let foreground = editor.foreground();
        let background = editor.background();

        let (dispatch, viewport) = {
            let doc = editor.active_mut().expect("checked immediately above");
            let viewport = canvas_viewport(doc.camera.viewport_size);
            let mut camera = canvas_camera_of(&doc.camera);
            let dispatch = self.router.handle(input, &mut camera, &viewport, effective);
            out.view_changed = write_camera_back(&camera, &mut doc.camera);
            (dispatch, viewport)
        };
        // The pin is taken from the router rather than from the phase, so it
        // says exactly as long as the claim does: set while a gesture is
        // running — a pan's as much as a stroke's, since panning the wrong
        // document is the same mistake — and cleared the moment it ends.
        self.aimed_at = self.router.is_gesture_active().then_some(active_id);

        let routed = match dispatch {
            Dispatch::Rejected(why) => {
                out.refused = Some(Refusal::Router(why));
                return out;
            }
            Dispatch::Navigated { route, .. } => {
                out.route = Some(route);
                return out;
            }
            Dispatch::ToTool(routed) => routed,
        };
        out.route = Some(routed.route);
        if !routed.in_gesture {
            // A hover. See the module docs: nothing here consumes one yet.
            return out;
        }
        out.reached_tool = true;

        // The gesture's own tool, not whatever is selected *now*. The two
        // differ the moment the user presses a tool letter — or the space bar —
        // with the button still down, and rebuilding the tool there would throw
        // away the half-finished stroke that is holding the pointer. The router
        // fixes the route at pointer-down for exactly this reason.
        let id = match routed.route {
            Route::Tool(id) => id,
            // Unreachable: only `Route::Tool` reaches a tool. Falling back to
            // the selected tool keeps that a routing decision rather than a
            // panic if the router ever widens.
            _ => effective,
        };
        // At the press, and only there: the brush the options bar and the
        // `[`/`]` keys have been moving *for this tool* is what the stroke is
        // drawn with. Read per tool, because the brush is part of what a tool
        // is — hand the Pencil the Brush's 24px soft round one and the two
        // become the same tool, since they share `StrokeOp::Paint`. An
        // untouched tool's slot answers with the settings
        // `tools::registry::make` built it holding, so this writes the Pencil
        // its own one hard aliased pixel back.
        let brush = (routed.phase == PointerPhase::Down).then(|| editor.brush_for(id));
        let tool = self.tool(id);
        if let Some(brush) = brush {
            tool.set_brush(brush);
        }

        let (result, commands, selection_edits, requests, picked, canvas_rect) = {
            let doc = editor.active_mut().expect("checked above");
            let canvas = doc.canvas_rect();
            let active_layer = doc.document.active_layer();
            let active_mask = active_layer
                .and_then(|id| doc.document.layers.get(id))
                .and_then(|layer| layer.mask_id());
            let selection = doc.document.selection.clone();
            // Top-most first, which is the order `LayerTree` keeps its roots in
            // — what the move tool's auto-select walks.
            let layer_stack = doc.document.layers.iter_depth_first();
            let view = canvas_camera_of(&doc.camera).to_view_state(&viewport);

            let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles);
            let mut ctx = ToolContext::new(&mut access, canvas);
            ctx.active_layer = active_layer;
            ctx.active_mask = active_mask;
            // Nothing in this shell selects a mask for editing yet, so a pixel
            // tool writes to the layer. Stated rather than defaulted into.
            ctx.paint_target = PaintTarget::Layer;
            ctx.selection = selection;
            ctx.foreground = foreground;
            ctx.background = background;
            ctx.view = view;
            ctx.layer_stack = layer_stack;

            let result = match routed.phase {
                PointerPhase::Down => tool.on_pointer_down(&mut ctx, routed.event),
                PointerPhase::Move => tool.on_pointer_move(&mut ctx, routed.event),
                PointerPhase::Up => tool.on_pointer_up(&mut ctx, routed.event),
            };
            // `ctx.view` is deliberately not read back: navigation belongs to
            // the router, which drove the camera before the tool ever saw this
            // sample, and the tools routed here are not the navigation ones.
            let out = (
                result,
                ctx.drain(),
                ctx.drain_selection(),
                ctx.drain_requests(),
                ctx.picked(),
                ctx.canvas_rect(),
            );
            // `ctx` holds the only mutable borrow of the document's tiles, and
            // the document is needed again the moment this block ends.
            drop(ctx);
            out
        };

        if let Err(e) = result {
            out.failed = Some(e.to_string());
            editor.set_status(e.to_string());
        }

        // Through the editor, so a gesture is undone by exactly the Ctrl+Z that
        // undoes a panel edit. The count is what history really took, not what
        // the tool offered: a command History refuses is not a step.
        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        for command in commands {
            editor.apply_command(command);
        }
        let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        out.steps = after.saturating_sub(before);

        if !selection_edits.is_empty() {
            let doc = editor.active_mut().expect("checked above");
            for edit in selection_edits {
                match edit.apply(canvas_rect, &doc.document.selection) {
                    Ok(next) => {
                        if next != doc.document.selection {
                            doc.document.selection = next;
                            // The selection is part of the saved document, so
                            // changing it is unsaved work — even though there
                            // is no command to undo it with.
                            doc.document.mark_dirty();
                            out.selection_changed = true;
                        }
                    }
                    Err(e) => {
                        let reason = e.to_string();
                        out.failed = Some(reason.clone());
                        editor.set_status(reason);
                        break;
                    }
                }
            }
        }

        if let Some(rgba) = picked {
            editor.set_foreground(rgba);
            out.picked = Some(rgba);
        }

        if !requests.is_empty() {
            // Unreachable from a pointer sample as things stand — the crop and
            // slice tools publish on an explicit commit, which nothing calls —
            // but a request that arrived and was dropped in silence would be a
            // gesture that looked like it worked. See `tools::ToolRequest`.
            tracing::warn!("{} tool request(s) have nowhere to go", requests.len());
            editor.set_status("Crop and slice cannot be applied from the canvas yet");
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use editor_core::Selection;
    use tools::Modifiers;
    use ui::canvas::{PointerButton, PointerInput};

    use crate::action::Action;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;

    const W: u32 = 64;
    const H: u32 = 64;
    /// The viewport every test routes against.
    const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

    /// An editor holding one opaque white 64x64 document, its camera at 100%
    /// with the image centred — so screen `(200, 150)` is document `(32, 32)`.
    fn editor(dir: &std::path::Path) -> Editor {
        let png = dir.join("canvas.png");
        std::fs::write(
            &png,
            raster::encode(
                raster::ExportFormat::Png,
                W,
                H,
                &[255u8; (W * H * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.open_path(&png).unwrap();
        let doc = editor.active_mut().unwrap();
        doc.set_viewport(VIEWPORT);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
        editor
    }

    fn composite(editor: &mut Editor) -> Vec<u8> {
        editor
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, W, H))
            .unwrap()
    }

    /// The same editor with a *second* 64x64 document open behind the first,
    /// both cameras set up identically, and tab 0 active.
    fn editor_with_two(dir: &std::path::Path) -> Editor {
        let mut editor = editor(dir);
        let png = dir.join("second.png");
        std::fs::write(
            &png,
            raster::encode(
                raster::ExportFormat::Png,
                W,
                H,
                &[200u8; (W * H * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        editor.open_path(&png).unwrap();
        assert_eq!(editor.documents().len(), 2);
        for doc in editor.documents_mut() {
            doc.set_viewport(VIEWPORT);
            doc.camera.zoom = 1.0;
            doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
        }
        editor.activate(0).unwrap();
        editor
    }

    /// Composite the document at `index`, whichever tab is in front.
    fn composite_at(editor: &mut Editor, index: usize) -> Vec<u8> {
        editor.documents_mut()[index]
            .composite(PixelRect::new(0, 0, W, H))
            .unwrap()
    }

    /// Screen point for a document point, at the fixture's camera.
    fn screen(doc_x: f32, doc_y: f32) -> Vec2 {
        VIEWPORT * 0.5 + Vec2::new(doc_x - W as f32 / 2.0, doc_y - H as f32 / 2.0)
    }

    fn sample(phase: PointerPhase, at: Vec2) -> PointerInput {
        PointerInput::at(phase, at)
    }

    /// Press, drag through the given document points, release.
    fn stroke(
        pointer: &mut ToolPointer,
        editor: &mut Editor,
        points: &[(f32, f32)],
    ) -> Vec<PointerOutcome> {
        let mut out = Vec::new();
        for (i, (x, y)) in points.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            out.push(pointer.handle(editor, sample(phase, screen(*x, *y)), false));
        }
        let (x, y) = *points.last().unwrap();
        out.push(pointer.handle(editor, sample(PointerPhase::Up, screen(x, y)), false));
        out
    }

    /// Which pixels differ between two composites, as document coordinates.
    fn changed_pixels(before: &[u8], after: &[u8]) -> Vec<(i64, i64)> {
        let mut out = Vec::new();
        for y in 0..H as i64 {
            for x in 0..W as i64 {
                let i = ((y * W as i64 + x) * 4) as usize;
                if before[i..i + 4] != after[i..i + 4] {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// The headline: a brush drag paints, once, only where it was dragged.
    #[test]
    fn a_brush_drag_paints_one_undoable_step_in_the_stroked_region_and_nowhere_else() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        let outcomes = stroke(
            &mut pointer,
            &mut editor,
            &[(32.0, 32.0), (38.0, 32.0), (44.0, 32.0)],
        );
        assert!(
            outcomes.iter().all(|o| o.reached_tool),
            "every sample of the gesture must reach the tool: {outcomes:?}"
        );
        assert_eq!(
            outcomes.iter().map(|o| o.steps).sum::<usize>(),
            1,
            "a stroke is one command: {outcomes:?}"
        );
        assert_eq!(editor.active().unwrap().history_depth(), 1);

        let after = composite(&mut editor);
        assert_ne!(before, after, "the brush painted nothing");
        let changed = changed_pixels(&before, &after);
        assert!(
            changed.contains(&(32, 32)) && changed.contains(&(44, 32)),
            "both ends of the stroke must be painted"
        );
        // The default brush is 24px across, so nothing outside a 13px margin of
        // the dragged segment may have moved.
        let radius = editor.brush().size / 2.0 + 1.0;
        for (x, y) in &changed {
            let dx = if *x < 32 {
                32 - *x
            } else if *x > 44 {
                *x - 44
            } else {
                0
            };
            assert!(
                (dx as f32) <= radius && ((*y - 32).abs() as f32) <= radius,
                "({x}, {y}) is outside the stroke"
            );
        }
        // ...and the corners of an untouched canvas are untouched.
        for corner in [(0, 0), (63, 0), (0, 63), (63, 63)] {
            assert!(!changed.contains(&corner), "{corner:?} changed");
        }
    }

    #[test]
    fn undo_restores_the_prior_pixels_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([0.0, 0.0, 1.0, 1.0]);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        stroke(
            &mut pointer,
            &mut editor,
            &[(20.0, 20.0), (30.0, 30.0), (40.0, 40.0)],
        );
        let painted = composite(&mut editor);
        assert_ne!(before, painted);

        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(
            composite(&mut editor),
            before,
            "undo did not restore the pixels exactly"
        );
        assert_eq!(editor.active().unwrap().history_depth(), 0);
    }

    #[test]
    fn the_same_gesture_with_the_hand_tool_moves_the_camera_and_emits_no_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Hand);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);
        let center = editor.active().unwrap().camera.center;

        let outcomes = stroke(
            &mut pointer,
            &mut editor,
            &[(32.0, 32.0), (38.0, 32.0), (44.0, 32.0)],
        );
        assert!(
            outcomes.iter().all(|o| !o.reached_tool),
            "the hand is the camera's, not a tool's: {outcomes:?}"
        );
        assert_eq!(outcomes.iter().map(|o| o.steps).sum::<usize>(), 0);
        assert_eq!(editor.active().unwrap().history_depth(), 0);
        assert_eq!(composite(&mut editor), before, "the hand painted");

        let after = editor.active().unwrap().camera.center;
        assert_ne!(after, center, "the hand did not move the camera");
        // Dragged twelve document pixels to the right, so the view centre moved
        // twelve pixels to the left.
        assert!((after.x - (center.x - 12.0)).abs() < 0.5, "{after:?}");
        assert!(outcomes.iter().any(|o| o.view_changed));
    }

    #[test]
    fn a_press_that_starts_over_a_panel_reaches_neither_the_tool_nor_the_camera() {
        for tool in [ToolId::Brush, ToolId::Hand, ToolId::RectMarquee] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor(dir.path());
            editor.set_tool(tool);
            let mut pointer = ToolPointer::new();
            let before = composite(&mut editor);
            let camera = editor.active().unwrap().camera.center;

            let down = pointer.handle(
                &mut editor,
                sample(PointerPhase::Down, screen(32.0, 32.0)),
                true,
            );
            assert_eq!(down.refused, Some(Refusal::OverPanel), "{tool:?}");
            assert!(!down.reached_tool);
            assert!(!pointer.is_gesture_active());

            // The drag that follows claimed nothing, so it moves nothing —
            // even once the cursor is over the canvas again.
            for at in [(38.0, 32.0), (44.0, 32.0)] {
                pointer.handle(
                    &mut editor,
                    sample(PointerPhase::Move, screen(at.0, at.1)),
                    false,
                );
            }
            pointer.handle(
                &mut editor,
                sample(PointerPhase::Up, screen(44.0, 32.0)),
                false,
            );

            assert_eq!(editor.active().unwrap().history_depth(), 0, "{tool:?}");
            assert_eq!(composite(&mut editor), before, "{tool:?} painted");
            assert_eq!(
                editor.active().unwrap().camera.center,
                camera,
                "{tool:?} panned"
            );
            assert_eq!(editor.active().unwrap().document.selection, Selection::None);
        }
    }

    #[test]
    fn a_marquee_gesture_changes_the_documents_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::RectMarquee);
        let mut pointer = ToolPointer::new();
        assert_eq!(editor.active().unwrap().document.selection, Selection::None);

        let outcomes = stroke(
            &mut pointer,
            &mut editor,
            &[(10.0, 12.0), (30.0, 30.0), (40.0, 44.0)],
        );
        assert!(
            outcomes.iter().any(|o| o.selection_changed),
            "no sample reported a selection change: {outcomes:?}"
        );
        let doc = editor.active().unwrap();
        assert_ne!(doc.document.selection, Selection::None);
        let (min, max) = doc.document.selection.bounds().expect("a rectangle");
        assert_eq!(
            (min.x, min.y, max.x, max.y),
            (10, 12, 40, 44),
            "the marquee did not cover the dragged rectangle"
        );
        // A marquee edits no pixel, so it is not a history step...
        assert_eq!(doc.history_depth(), 0);
        // ...but it is unsaved work.
        assert!(doc.is_dirty());
    }

    #[test]
    fn the_space_bar_overrides_the_active_tool_and_releasing_it_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        // Space down: the hand borrows the brush's gesture.
        editor.dispatch(Action::TemporaryHand).unwrap();
        assert_eq!(editor.effective_tool(), ToolId::Hand);
        let center = editor.active().unwrap().camera.center;
        let held = stroke(
            &mut pointer,
            &mut editor,
            &[(32.0, 32.0), (38.0, 32.0), (44.0, 32.0)],
        );
        assert!(held.iter().all(|o| !o.reached_tool), "{held:?}");
        assert_eq!(editor.active().unwrap().history_depth(), 0);
        assert_eq!(composite(&mut editor), before, "the held space bar painted");
        assert_ne!(editor.active().unwrap().camera.center, center);

        // Space up: the brush is back, and the same gesture paints.
        editor.release_temporary_hand();
        assert_eq!(editor.effective_tool(), ToolId::Brush);
        let released = stroke(
            &mut pointer,
            &mut editor,
            &[(32.0, 32.0), (38.0, 32.0), (44.0, 32.0)],
        );
        assert!(released.iter().all(|o| o.reached_tool), "{released:?}");
        assert_eq!(editor.active().unwrap().history_depth(), 1);
        assert_ne!(composite(&mut editor), before);
    }

    #[test]
    fn escape_abandons_a_stroke_in_progress_without_emitting_anything() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(20.0, 20.0)),
            false,
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(40.0, 40.0)),
            false,
        );
        assert!(pointer.is_tool_active(), "the stroke never started");

        assert!(pointer.cancel(&mut editor), "there was a gesture to cancel");
        assert!(!pointer.is_gesture_active());
        assert!(!pointer.is_tool_active());

        // The release that arrives afterwards belongs to nobody.
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(40.0, 40.0)),
            false,
        );
        assert_eq!(up.refused, Some(Refusal::Router(Rejected::NotOurGesture)));
        assert_eq!(editor.active().unwrap().history_depth(), 0);
        assert_eq!(composite(&mut editor), before, "a cancelled stroke painted");
        // Cancelling twice reports that there was nothing to cancel.
        assert!(!pointer.cancel(&mut editor));
    }

    #[test]
    fn the_application_brush_is_the_size_the_stroke_is_painted_at() {
        // What `[` and `]` and the options bar move. Before the tool was fed
        // the editor's brush they moved a number no gesture read.
        fn width_of_a_dab(size: f32) -> usize {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor(dir.path());
            editor.set_tool(ToolId::Brush);
            editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
            let mut brush = *editor.brush();
            brush.size = size;
            editor.set_brush(brush);
            let mut pointer = ToolPointer::new();
            let before = composite(&mut editor);
            stroke(&mut pointer, &mut editor, &[(32.0, 32.0)]);
            let after = composite(&mut editor);
            changed_pixels(&before, &after)
                .into_iter()
                .filter(|(_, y)| *y == 32)
                .count()
        }
        let small = width_of_a_dab(6.0);
        let large = width_of_a_dab(30.0);
        assert!(small > 0, "a 6px brush painted nothing");
        assert!(
            large > small * 2,
            "a 30px brush laid {large}px where a 6px brush laid {small}px"
        );
    }

    /// The distinct colours the changed pixels of a composite were painted.
    ///
    /// One shade means an aliased stroke: every pixel is fully in or fully out.
    /// A soft round brush leaves a falloff, so it leaves many.
    fn shades(after: &[u8], changed: &[(i64, i64)]) -> std::collections::BTreeSet<[u8; 4]> {
        changed
            .iter()
            .map(|(x, y)| {
                let i = ((y * W as i64 + x) * 4) as usize;
                [after[i], after[i + 1], after[i + 2], after[i + 3]]
            })
            .collect()
    }

    /// The Pencil is a pencil, not the application's brush wearing its name.
    ///
    /// Both tools paint through `StrokeOp::Paint`, so their [`BrushSettings`]
    /// are the *whole* difference between them: `BrushSettings::pencil(1.0)` is
    /// one hard aliased pixel with no size-from-pressure. Handing the tool a
    /// single application-wide brush at pointer-down made the identical drag
    /// composite to identical bytes — the same tool twice.
    ///
    /// [`BrushSettings`]: tools::BrushSettings
    #[test]
    fn the_pencil_paints_a_pencils_stroke_and_the_brush_paints_a_brushs() {
        fn drag(tool: ToolId) -> (Vec<u8>, Vec<(i64, i64)>, f32) {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor(dir.path());
            editor.set_tool(tool);
            editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
            let mut pointer = ToolPointer::new();
            let before = composite(&mut editor);
            stroke(
                &mut pointer,
                &mut editor,
                &[(16.0, 32.0), (32.0, 32.0), (48.0, 32.0)],
            );
            let after = composite(&mut editor);
            let changed = changed_pixels(&before, &after);
            (after, changed, editor.brush_for(tool).size)
        }

        let (pencil, pencil_px, pencil_size) = drag(ToolId::Pencil);
        let (brush, brush_px, brush_size) = drag(ToolId::Brush);

        assert_ne!(
            pencil, brush,
            "the Pencil and the Brush composited to the same bytes: \
             the Pencil's own settings were overwritten"
        );
        assert!(
            !pencil_px.is_empty() && !brush_px.is_empty(),
            "nothing drew"
        );
        // Narrower: a one-pixel nib against a 24px disc.
        assert!(
            pencil_px.len() * 8 < brush_px.len(),
            "the Pencil covered {} pixels and the Brush {}",
            pencil_px.len(),
            brush_px.len()
        );
        // ...and only the row it was dragged along.
        for (_, y) in &pencil_px {
            assert_eq!(*y, 32, "the Pencil painted off its own row");
        }
        // Aliased: every pixel fully in, so one shade and no falloff.
        let pencil_shades = shades(&pencil, &pencil_px);
        assert_eq!(
            pencil_shades.len(),
            1,
            "the Pencil left a soft edge: {pencil_shades:?}"
        );
        assert!(
            shades(&brush, &brush_px).len() > 1,
            "the Brush left no falloff, so it was painted aliased"
        );
        // The size slider the options bar shows is each tool's own, so
        // selecting the Pencil never writes 24 into it.
        assert_eq!(pencil_size, 1.0);
        assert_eq!(brush_size, 24.0);
    }

    /// Each tool keeps the brush the user tuned for it, and a tool never
    /// selected still starts at the registry's tuning.
    #[test]
    fn the_bracket_keys_move_the_active_tools_brush_and_leave_the_others_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut brush = *editor.brush();
        brush.size = 40.0;
        editor.set_brush(brush);

        editor.set_tool(ToolId::Pencil);
        assert_eq!(
            editor.brush().size,
            1.0,
            "the Pencil inherited the Brush's size"
        );
        assert!(editor.brush().aliased, "the Pencil is not aliased");
        assert!(!editor.brush().size_pressure);

        // A tool nothing has selected still answers with its own tuning.
        assert_eq!(editor.brush_for(ToolId::CloneStamp).size, 40.0);
        assert_eq!(editor.brush_for(ToolId::Dodge).size, 60.0);
        assert_eq!(editor.brush_for(ToolId::Blur).spacing, 0.05);

        // ...and going back picks up what the user left there.
        editor.set_tool(ToolId::Brush);
        assert_eq!(editor.brush().size, 40.0, "the Brush lost its tuned size");
        assert_eq!(editor.brush_for(ToolId::Pencil).size, 1.0);
    }

    #[test]
    fn the_foreground_colour_is_what_the_brush_paints() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        // Full-strength green, in the linear light the context carries.
        editor.set_foreground([0.0, 1.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(32.0, 32.0)]);

        let after = composite(&mut editor);
        let i = ((32 * W + 32) * 4) as usize;
        let px = &after[i..i + 4];
        assert!(
            px[1] > px[0] && px[1] > px[2],
            "the brush painted {px:?}, which is not the foreground green"
        );
    }

    #[test]
    fn the_eyedropper_takes_the_colour_it_clicked_as_the_foreground() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
        editor.set_tool(ToolId::Eyedropper);
        let mut pointer = ToolPointer::new();
        let outcomes = stroke(&mut pointer, &mut editor, &[(32.0, 32.0)]);
        assert!(
            outcomes.iter().any(|o| o.picked.is_some()),
            "nothing was picked: {outcomes:?}"
        );
        // The canvas is white, so the foreground is no longer black.
        let fg = editor.foreground();
        assert!(fg[0] > 0.9 && fg[1] > 0.9 && fg[2] > 0.9, "picked {fg:?}");
        assert_eq!(editor.active().unwrap().history_depth(), 0);
    }

    #[test]
    fn a_gesture_that_starts_on_the_canvas_keeps_painting_over_a_panel() {
        // The mirror image of the panel rule: the claim is made at the press
        // and nothing after it may take the gesture away.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();

        let down = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 32.0)),
            false,
        );
        assert!(down.reached_tool);
        let moved = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 32.0)),
            true,
        );
        assert!(moved.reached_tool, "the drag died over a panel");
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(44.0, 32.0)),
            true,
        );
        assert_eq!(up.steps, 1);
    }

    #[test]
    fn the_middle_button_pans_whatever_tool_is_selected() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        let center = editor.active().unwrap().camera.center;
        let before = composite(&mut editor);

        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 32.0)).with_button(PointerButton::Middle),
            false,
        );
        let moved = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 32.0)).with_button(PointerButton::Middle),
            false,
        );
        assert_eq!(moved.route, Some(Route::Pan));
        assert!(!moved.reached_tool);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(44.0, 32.0)).with_button(PointerButton::Middle),
            false,
        );
        assert_ne!(editor.active().unwrap().camera.center, center);
        assert_eq!(composite(&mut editor), before);
    }

    #[test]
    fn the_zoom_tool_still_zooms_and_the_document_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Zoom);
        let mut pointer = ToolPointer::new();
        let zoom = editor.active().unwrap().camera.zoom;

        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 32.0)),
            false,
        );
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(32.0, 32.0)),
            false,
        );
        assert!(up.view_changed, "a zoom click did not move the view");
        assert!(editor.active().unwrap().camera.zoom > zoom);
        assert_eq!(editor.active().unwrap().history_depth(), 0);

        // ...and alt-clicking goes back out.
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 32.0)),
            false,
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(32.0, 32.0)).with_modifiers(Modifiers::alt()),
            false,
        );
        assert!((editor.active().unwrap().camera.zoom - zoom).abs() < 1e-4);
    }

    /// A tool letter pressed with the button still down must not take the
    /// half-finished stroke away from the tool that is holding the pointer.
    #[test]
    fn a_tool_change_mid_gesture_does_not_hijack_the_stroke_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(20.0, 20.0)),
            false,
        );
        assert_eq!(pointer.live_tool(), Some(ToolId::Brush));

        editor.set_tool(ToolId::RectMarquee);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(30.0, 30.0)),
            false,
        );
        assert_eq!(
            pointer.live_tool(),
            Some(ToolId::Brush),
            "the marquee stole a stroke that was already running"
        );
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(30.0, 30.0)),
            false,
        );
        assert_eq!(up.steps, 1, "the stroke did not finish as a brush stroke");
        assert_eq!(editor.active().unwrap().document.selection, Selection::None);

        // The *next* gesture is the marquee's.
        let outcomes = stroke(&mut pointer, &mut editor, &[(10.0, 10.0), (20.0, 20.0)]);
        assert_eq!(pointer.live_tool(), Some(ToolId::RectMarquee));
        assert!(outcomes.iter().any(|o| o.selection_changed));
    }

    #[test]
    fn a_pointer_sample_with_no_document_is_refused_and_drops_the_gesture() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        let mut pointer = ToolPointer::new();
        let out = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, Vec2::new(200.0, 150.0)),
            false,
        );
        assert_eq!(out.refused, Some(Refusal::NoDocument));
        assert!(!pointer.is_gesture_active());
    }

    #[test]
    fn painting_with_no_active_layer_reports_the_refusal_rather_than_swallowing_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor
            .active_mut()
            .unwrap()
            .document
            .set_active_layer(None)
            .unwrap();
        let mut pointer = ToolPointer::new();
        let outcomes = stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (30.0, 30.0)]);
        assert!(
            outcomes.iter().any(|o| o.failed.is_some()),
            "a stroke with nowhere to go reported success: {outcomes:?}"
        );
        assert_eq!(editor.active().unwrap().history_depth(), 0);
        assert!(editor.status().is_some_and(|s| s.contains("layer")));
    }

    #[test]
    fn the_document_tile_seam_reads_the_document_and_writes_the_byte_store() {
        // The adapter both halves of a document's pixels are joined by.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let doc = editor.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let key = PixelKey::Layer(layer);
        let coord = TileCoord::new(0, 0, 0);
        let expected = doc.document.pixels.tiles(key).unwrap().get(coord);

        let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles);
        assert_eq!(access.tile_hash(key, coord), expected);
        assert!(access.tile_bytes(key, coord).is_some());
        assert_eq!(
            access.tile_hash(key, TileCoord::new(9, 9, 0)),
            None,
            "a tile the document does not reference must read as absent"
        );

        let fresh = vec![9u8; 32];
        let hash = access.store(fresh.clone());
        assert_eq!(
            hash,
            TileHash::of(&fresh),
            "the store is not content-addressed"
        );
        assert_eq!(access.bytes(hash), Some(fresh.as_slice()));
        // Stored bytes live in the source the compositor reads, so the command
        // the tool is about to emit can name them.
        assert!(doc.tiles.contains(hash));
    }

    #[test]
    fn the_viewport_is_the_whole_surface_so_a_click_lands_under_the_cursor() {
        // The claim the module doc makes, checked against the camera the
        // renderer actually uses.
        let dir = tempfile::tempdir().unwrap();
        let editor = editor(dir.path());
        let camera = editor.active().unwrap().camera.clone();
        let viewport = canvas_viewport(camera.viewport_size);
        let mirror = canvas_camera_of(&camera);
        for at in [
            Vec2::new(0.0, 0.0),
            Vec2::new(200.0, 150.0),
            Vec2::new(399.0, 299.0),
            Vec2::new(37.0, 211.0),
        ] {
            let router = mirror.doc_of_screen_pt(&viewport, at);
            let renderer = camera.screen_to_image(at);
            assert!(
                (router - renderer).length() < 1e-3,
                "at {at:?} the tool would see {router:?} and the screen shows {renderer:?}"
            );
        }
    }

    /// A gesture belongs to the document it was aimed at, and to no other.
    ///
    /// The tab strip is live while the button is held — Ctrl+Tab and Ctrl+W are
    /// both bound — and `handle` re-reads the *active* document every sample.
    /// Without the pin the whole stroke, the part dragged over tab 0 included,
    /// was rasterised into tab 1 and pushed onto tab 1's history at pointer-up,
    /// leaving the document the user actually dragged on untouched.
    #[test]
    fn a_stroke_does_not_follow_a_tab_switch_into_the_document_it_was_never_aimed_at() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_two(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        let first_before = composite_at(&mut editor, 0);
        let second_before = composite_at(&mut editor, 1);
        let first_id = editor.documents()[0].id();

        let down = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(20.0, 20.0)),
            false,
        );
        assert!(down.reached_tool, "the press never reached the brush");
        assert_eq!(
            pointer.aimed_at(),
            Some(first_id),
            "the gesture did not record the document it was aimed at"
        );

        // Ctrl+Tab, with the button still down.
        editor.activate(1).unwrap();

        // The rest of the drag, all of it over what is now tab 1.
        let moved = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(30.0, 30.0)),
            false,
        );
        let drifted = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(40.0, 40.0)),
            false,
        );
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(40.0, 40.0)),
            false,
        );

        // The headline, asserted before anything about *how* it was achieved:
        // neither document was edited. Not the one in front, which the gesture
        // was never aimed at, and not the one behind, which the gesture was
        // abandoned on rather than silently finished.
        assert_eq!(editor.documents()[0].history_depth(), 0, "tab 0 was edited");
        assert_eq!(
            editor.documents()[1].history_depth(),
            0,
            "the stroke was pushed onto the history of a document it was never \
             aimed at"
        );
        assert_eq!(
            composite_at(&mut editor, 0),
            first_before,
            "tab 0's pixels changed"
        );
        assert_eq!(
            composite_at(&mut editor, 1),
            second_before,
            "the stroke was painted into the wrong document"
        );

        // ...and this is how: the first sample after the switch catches it and
        // ends the gesture rather than redirecting it, so the rest of the drag
        // is a hover that belongs to nobody.
        assert_eq!(
            moved.refused,
            Some(Refusal::WrongDocument),
            "a sample was applied to a document the gesture was never aimed at"
        );
        assert!(!moved.reached_tool);
        assert!(!drifted.reached_tool, "the drag came back to life");
        assert_eq!(drifted.steps, 0);
        assert!(!up.reached_tool);
        assert_eq!(up.steps, 0, "the stroke committed after the tab switch");

        // The gesture is gone, not stuck: the tool is idle, the router has let
        // go, and the next press on the document now in front paints normally.
        assert!(
            !pointer.is_tool_active(),
            "the stroke was left half-finished"
        );
        assert!(!pointer.is_gesture_active());
        assert_eq!(pointer.aimed_at(), None);
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (30.0, 30.0)]);
        assert_eq!(editor.documents()[1].history_depth(), 1);
        assert_eq!(editor.documents()[0].history_depth(), 0);
    }

    /// Closing the tab a gesture is running on, with others still open, is the
    /// same mistake wearing a different hat: `active_index` is still `Some`, so
    /// only identity catches it.
    #[test]
    fn closing_the_tab_a_gesture_runs_on_does_not_hand_the_stroke_to_the_survivor() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor_with_two(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        let survivor_before = composite_at(&mut editor, 1);

        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(20.0, 20.0)),
            false,
        );
        editor.close_document(0).unwrap();
        assert!(
            editor.active_index().is_some(),
            "the surviving tab must still be active, or this tests the other guard"
        );

        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(40.0, 40.0)),
            false,
        );
        assert_eq!(
            editor.documents()[0].history_depth(),
            0,
            "the stroke was committed onto the tab that survived the close"
        );
        assert_eq!(
            composite_at(&mut editor, 0),
            survivor_before,
            "the stroke was painted into the tab that survived the close"
        );
        assert_eq!(up.refused, Some(Refusal::WrongDocument));
    }

    /// A stroke is committed once, at the release — so nothing is on the canvas
    /// while the button is held. Stated in the module docs; pinned here so it
    /// cannot quietly stop being true.
    #[test]
    fn a_stroke_is_invisible_until_the_button_is_released() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        let down = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(20.0, 20.0)),
            false,
        );
        assert!(down.reached_tool, "the press never reached the brush");
        assert_eq!(down.steps, 0);
        assert!(
            !down.needs_repaint(),
            "the press asked for a repaint that would draw the same frame"
        );
        let pressed = composite(&mut editor);
        assert!(
            changed_pixels(&before, &pressed).is_empty(),
            "the press painted {} pixels, so this limit is over and the doc \
             bullet must go",
            changed_pixels(&before, &pressed).len()
        );

        for at in [(28.0, 28.0), (36.0, 36.0)] {
            let moved = pointer.handle(
                &mut editor,
                sample(PointerPhase::Move, screen(at.0, at.1)),
                false,
            );
            let mid = composite(&mut editor);
            let live = changed_pixels(&before, &mid);
            assert!(
                live.is_empty(),
                "the drag showed a live preview of {} pixels at {at:?}, so the \
                 doc bullet must go",
                live.len()
            );
            assert!(moved.reached_tool);
            assert_eq!(moved.steps, 0, "a move sample committed a step");
            assert!(!moved.needs_repaint());
        }

        // ...and the release is where the whole stroke arrives at once.
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(36.0, 36.0)),
            false,
        );
        assert_eq!(up.steps, 1);
        assert!(up.needs_repaint());
        let released = composite(&mut editor);
        assert!(
            !changed_pixels(&before, &released).is_empty(),
            "the release painted nothing, so the stroke is lost rather than \
             merely late"
        );
    }

    /// The seven shape tools run, and draw nothing: the compositor has no
    /// rasteriser for `LayerKind::Shape`. Stated in the module docs, pinned
    /// here — the day that arm grows a rasteriser this test fails and the
    /// bullet has to come out with it.
    #[test]
    fn a_shape_gesture_creates_a_layer_the_compositor_cannot_draw() {
        for tool in [
            ToolId::Rectangle,
            ToolId::RoundedRectangle,
            ToolId::Ellipse,
            ToolId::Polygon,
            ToolId::Star,
            ToolId::Line,
            ToolId::CustomShape,
        ] {
            let dir = tempfile::tempdir().unwrap();
            let mut editor = editor(dir.path());
            editor.set_tool(tool);
            editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
            let mut pointer = ToolPointer::new();
            let before = composite(&mut editor);
            let layers_before = editor.active().unwrap().document.layers.len();

            stroke(
                &mut pointer,
                &mut editor,
                &[(10.0, 10.0), (25.0, 25.0), (40.0, 40.0)],
            );

            // Nothing reached the canvas: the compositor's
            // Text/Shape/SmartObject arm contributes nothing, so the picture is
            // byte-identical. Asserted first, because it is the claim the doc
            // bullet makes and the one that has to break when the gap closes.
            let after = composite(&mut editor);
            let reached = changed_pixels(&before, &after);
            assert!(
                reached.is_empty(),
                "{tool:?} put {} pixels on the canvas — the compositor grew a \
                 shape rasteriser, so the 'draws nothing' bullet in this \
                 module's docs and in lib.rs is now false and must be deleted",
                reached.len()
            );

            // ...and yet it ran, and cost the user an undo step and a layer
            // row: that pairing is exactly what makes this worth stating.
            assert_eq!(
                editor.active().unwrap().history_depth(),
                1,
                "{tool:?} emitted no undoable step"
            );
            let doc = editor.active().unwrap();
            assert_eq!(doc.document.layers.len(), layers_before + 1, "{tool:?}");
            let shape = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter_map(|id| doc.document.layers.get(id))
                .find(|layer| matches!(layer.kind, layer_model::LayerKind::Shape(_)))
                .unwrap_or_else(|| panic!("{tool:?} created no shape layer"));
            assert!(shape.visible, "{tool:?} created a hidden layer");
        }
    }

    #[test]
    fn a_navigated_mirror_is_clamped_on_the_way_back_and_nonsense_is_ignored() {
        let mut camera = Camera::new(Vec2::splat(64.0), VIEWPORT);
        camera.zoom = 1.0;
        let mut mirror = canvas_camera_of(&camera);
        mirror.center = Vec2::new(10.0, 20.0);
        mirror.zoom = 1000.0;
        assert!(write_camera_back(&mirror, &mut camera));
        assert_eq!(camera.center, Vec2::new(10.0, 20.0));
        assert_eq!(camera.zoom, MAX_ZOOM);

        // A rotate-view drag moves the mirror's rotation and nothing else, so
        // nothing is owed a repaint.
        let mut turned = canvas_camera_of(&camera);
        turned.rotation = 1.0;
        assert!(!write_camera_back(&turned, &mut camera));

        let mut broken = canvas_camera_of(&camera);
        broken.center = Vec2::new(f32::NAN, 0.0);
        broken.zoom = f32::INFINITY;
        let before = camera.center;
        assert!(!write_camera_back(&broken, &mut camera));
        assert_eq!(camera.center, before);
    }
}
