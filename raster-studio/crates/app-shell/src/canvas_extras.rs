//! View ▸ Extras, drawn over the shell's own composite (W3-A).
//!
//! # Why this module exists
//!
//! The `ui` crate has a complete canvas host — `ui::CanvasHost` paints the
//! rulers, the grid, the guides, the smart guides and the layer edges in
//! [`ui::canvas::paint`] — and this application never draws it: the image is
//! a wgpu composite behind egui, and `CanvasHost::central_panel` is never
//! called. Until this module the View menu's Extras were therefore checkmarks
//! and nothing else: `Rulers`, `Guides`, `Grid`, `Pixel Grid`, `Layer Edges`
//! and `Precise Cursor` toggled a bit the chrome read back to draw the tick
//! and no painter ever consulted.
//!
//! [`CanvasExtras::paint`] is the one entry the chrome calls in its
//! live-geometry pass, right after the composite and before the transform
//! overlay. It uses the **same painters** the `ui` host would (nothing about
//! a ruler is re-invented here), against the **same camera** the surface was
//! rendered with, clipped to the rectangle the docks left. Each overlay obeys
//! its [`ViewFlag`]; every colour, width and gap comes from
//! [`CanvasStyle`], which comes from `design`.
//!
//! # What each flag does here
//!
//! * **Rulers** — the top and left gutters of the content rectangle, in the
//!   workspace's ruler unit at the current zoom, with the pointer marked on
//!   both. Their thickness is the `ui` style's token; the gutters are drawn
//!   *over* the image rather than reserved from the viewport, because the
//!   renderer composites across the whole window and reserving a strip here
//!   would put every zoom command's arithmetic out by that strip (see
//!   `Chrome::sync_canvas_host`).
//! * **Guides** — the document's guides, from the view the chrome seeds each
//!   frame from `Document::guides`, shown whenever the flag is on (the flag,
//!   not the document's persisted `visible`, is the switch — as in the `ui`
//!   host). Unlocked guides drag: a press in a ruler
//!   gutter pulls a new one out, a press on a guide moves it, and a drop back
//!   into a gutter deletes it — the `ui` crate's own [`GuideGesture`], driven
//!   from egui's pointer. The edit lands on the view's guides and the chrome's
//!   existing `sync_guides` converges the document as one `SetGuides` step.
//!   The gutters and guide bands are egui areas so a press there is consumed
//!   by egui and never reaches the active tool as a stroke.
//! * **Smart Guides** — while something is being moved, the layer-edge,
//!   layer-centre and canvas-centre candidates the moving box actually landed
//!   on, from the same candidate list the tool snapped against
//!   ([`tool_input::snap_candidates_kinded`]). The moving box is a live
//!   transform session's corners, or — for a plain Move-tool drag, which
//!   publishes no session — the active layer's tight ink bounds shifted by
//!   the drag, snapped with the same [`tools::snap_delta`] the Move tool
//!   commits with ([`move_drag_corners`]).
//! * **Grid** and **Pixel Grid** — the workspace's [`ui::canvas::GridSettings`]
//!   with the two flags written in; the pixel grid appears from
//!   [`ui::canvas::grid::PIXEL_GRID_MIN_ZOOM`] (800%) upward, the `ui`
//!   crate's threshold.
//! * **Layer Edges** — the active layer's tight ink bounds in document space,
//!   projected corner by corner so a turned view tilts the box with the image.
//! * **Precise Cursor** — a crosshair at the pointer while it is over the
//!   canvas and not over a panel or a menu.
//!
//! # The pointer over the canvas (W4-C)
//!
//! The `ui` host's `CanvasView::show` is where the brush ring, the per-tool
//! cursor and the right-click menu live, and the application never calls it.
//! So they are here too, gated on the pointer being over the bare canvas —
//! not a panel, a ruler gutter (an egui area while unlocked guides can be
//! pulled from it, and left out by its rectangle while Rulers is on
//! otherwise), a guide band, a menu — and no modal up:
//!
//! * **The cursor** — [`ui::canvas::cursor_for_tool_id`] for the tool that
//!   is acting (the hand while Space is held), with View ▸ Precise Cursor as
//!   the precise toggle: a grab hand for the Hand tool (closed mid-drag, and
//!   for a middle-button pan under any tool), the zoom glass (out with Alt),
//!   the move arrows, the I-beam for Type. Set with `ctx.set_cursor_icon`,
//!   which egui-winit hands the platform.
//! * **The brush ring** — for every tool whose cursor is the brush ring, the
//!   [`brush_cursor::build`] outline at the brush's size times the zoom, and
//!   the hardness ring inside it; the system pointer is hidden under it.
//! * **The canvas context menu** — a right-click opens
//!   [`ui::context_menu`]'s canvas menu at the pointer. The shell does not
//!   route the right button to the tool (`shell::pointer_button`), so this is
//!   the one thing it does on the canvas; the rows post intents the chrome
//!   harvests through `Chrome::route` the same frame.
//!
//! **Selection Edges** is not painted here: the marching ants are GPU
//! segments `Shell::redraw` gets from `Chrome::selection_ants`, which returns
//! nothing while the flag is off.
//!
//! # Layering
//!
//! Everything here is painted on an [`egui::Order::Background`] layer: above
//! the wgpu composite (which is behind egui altogether), below the modal
//! dialog's scrim ([`egui::Order::PanelResizeLine`]) and below every window
//! ([`egui::Order::Middle`]). An `Order::Middle` layer that is not an area is
//! drawn after *every* ordered middle area, which put the grid and the layer
//! outline across an open dialog and undimmed by its scrim. The guide
//! gesture's grab areas are not raised while a modal is up either, so a press
//! the scrim should swallow cannot pull a guide.

use design::Space;
use glam::Vec2;
use ui::canvas::geom::{from_pos2, to_pos2};
use ui::canvas::{
    brush_cursor, cursor_for_tool_id, paint, rulers, Axis, CanvasCursor, CanvasStyle,
    CursorOverride, DocRect, GuideDrag, GuideGesture, GuideGrab, Guides, RulerSpec, SnapHit,
};
use ui::ViewFlag;

use crate::doc::OpenDocument;
use crate::editor::Editor;
use crate::tool_input::{self, SnapPolicy};

/// The egui layer every overlay here is painted on.
const LAYER_ID: &str = "raster-canvas-extras";
/// The layer the canvas overlays are painted on: the extras here, then the
/// live tool session's handles (`Chrome::paint_live_tool_geometry`), in that
/// order. [`egui::Order::Background`] — see the module's *Layering*.
pub(crate) fn overlay_layer() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Background, egui::Id::new(LAYER_ID))
}
/// The egui areas that make a ruler gutter a place egui owns the pointer.
const GUTTER_AREA: &str = "raster-canvas-extras-gutter";
/// The egui areas that make an unlocked guide's grab band egui's.
const GUIDE_AREA: &str = "raster-canvas-extras-guide";

/// The persistent half of the overlays: a guide drag outlives the frame it
/// started on.
#[derive(Debug, Default)]
pub struct CanvasExtras {
    /// The guide being dragged, if one is.
    guide_drag: Option<GuideDrag>,
    /// The guide set as the drag in progress has left it.
    ///
    /// `Chrome::sync_workspace` reseeds the view's guides from the document
    /// every frame, and `Chrome::sync_guides` holds its `SetGuides` while a
    /// drag is live (one drag is one undo step, not one per frame), so the
    /// document still has the pre-drag guides until the drop. This is the
    /// in-flight set, put back over the reseed each frame of the drag.
    dragging: Option<Guides>,
    /// What the last frame painted.
    last: ExtrasReport,
    /// The primary button went down on the bare canvas — not on a panel, an
    /// egui area (a ruler gutter, a guide band) or under a modal — and is
    /// still held: the press the shell hands the active tool as a gesture.
    canvas_press: bool,
    /// Layer Edges' tight ink bounds, keyed by the document, the layer and
    /// the layer's content fingerprint. The bounds are an alpha scan of the
    /// whole layer — far too slow to redo every frame on a large image — and
    /// only change when the layer does.
    edge_cache: Option<(EdgeKey, Option<raster::PixelRect>)>,
    /// W8-A: where a pen hovering in range (or in contact) is, in window
    /// physical pixels, as the shell's pen route last saw it. The brush ring
    /// follows it; `None` (the mouse moved, the contact was a finger, focus
    /// was lost) hands the ring back to egui's pointer.
    pen_hover: Option<Vec2>,
}

/// What [`CanvasExtras::edge_cache`] is valid for.
type EdgeKey = (crate::doc::DocumentId, layer_model::LayerId, u64);

/// What one frame of [`CanvasExtras::paint`] drew, for the status bar and for
/// tests that want the count beside the shapes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExtrasReport {
    /// The two ruler gutters were painted.
    pub rulers: bool,
    /// The document grid was painted (asked for and not suppressed).
    pub grid: bool,
    /// The grid was asked for but is too dense at this zoom to draw.
    pub grid_suppressed: bool,
    /// How many guides were painted.
    pub guides: usize,
    /// How many smart-guide lines were painted.
    pub smart_guides: usize,
    /// How many layer outlines were painted.
    pub layer_edges: usize,
    /// W10-J: how many committed slices View > Show > Slices painted.
    pub slices: usize,
    /// W10-B: how many note pins were painted (the document's notes, while
    /// View > Extras is on).
    pub notes: usize,
    /// The precise-cursor crosshair was painted.
    pub precise_cursor: bool,
    /// A guide drag is in progress.
    pub dragging_guide: bool,
    /// The cursor installed for the pointer over the canvas, or `None` when
    /// the pointer is not the canvas's (a panel, a menu, a modal, outside).
    pub cursor: Option<CanvasCursor>,
    /// The brush ring was painted at the pointer.
    pub brush_ring: bool,
    /// A right-click on the canvas opened the canvas context menu.
    pub context_menu: bool,
}

impl CanvasExtras {
    /// What the last call to [`CanvasExtras::paint`] drew.
    pub fn last_report(&self) -> ExtrasReport {
        self.last
    }

    /// W8-A: the pen's window position (physical pixels) for the brush ring,
    /// or `None` to follow egui's pointer. See [`Self::pen_hover`].
    pub fn set_pen_hover(&mut self, at: Option<Vec2>) {
        self.pen_hover = at.filter(|p| p.is_finite());
    }

    /// Whether a guide is being dragged right now. While it is, the chrome
    /// holds the document's `SetGuides` so the whole drag lands as one step.
    pub fn is_dragging_guide(&self) -> bool {
        self.guide_drag.is_some()
    }

    /// Paint every enabled extra over the canvas for this frame.
    ///
    /// Called by the chrome after the docks are laid out — `ctx.available_rect()`
    /// is then the rectangle the docks left, which is where the gutters go and
    /// what everything is clipped to — and before the transform overlay, so
    /// the handles sit on top of the grid as they do in the `ui` host.
    ///
    /// `modal_open` is whether a modal dialog is up: the overlays still paint
    /// (under its scrim, see the module's *Layering*), but nothing here takes
    /// the pointer — no guide grab, no smart guides for a drag the scrim
    /// swallowed, no precise cursor over the dialog.
    pub fn paint(
        &mut self,
        ctx: &egui::Context,
        workspace: &mut ui::Workspace,
        editor: &Editor,
        modal_open: bool,
    ) -> ExtrasReport {
        let mut report = ExtrasReport::default();
        let Some(doc) = editor.active() else {
            self.guide_drag = None;
            self.dragging = None;
            self.canvas_press = false;
            workspace.set_grid_suppressed(false);
            self.last = report;
            return report;
        };
        let flags = workspace.view_flags;
        let content = ctx.available_rect();
        if !(content.width() > 0.0 && content.height() > 0.0) {
            self.last = report;
            return report;
        }
        // The camera the surface was rendered with — rotation included, as
        // the ants and the transform handles read it — measured against the
        // canvas area it draws into (`viewport_origin` / `viewport_size`, the
        // rectangle between the docks; see `Chrome::canvas_area_px`). In egui
        // points, so the painter and the pointer agree on a scaled display.
        //
        // The camera's area is in physical pixels and its zoom is physical
        // pixels per document pixel; `ui::canvas::Viewport` takes points and
        // converts through the scale, so `CanvasArea::viewport` divides the
        // area by it and the zoom is left as it is.
        let camera = crate::interaction_geometry::canvas_camera_of(&doc.camera);
        let ppp = ctx.pixels_per_point();
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        let viewport =
            crate::interaction_geometry::CanvasArea::of_camera(&doc.camera).viewport(ppp);
        if viewport.is_degenerate() {
            self.last = report;
            return report;
        }
        let style = CanvasStyle::from_context(ctx);
        let doc_size = Vec2::new(doc.document.width() as f32, doc.document.height() as f32);
        let canvas = DocRect::of_canvas(doc_size);
        // Background, not Middle: see the module's *Layering*.
        let painter = ctx.layer_painter(overlay_layer());
        let painter = painter.with_clip_rect(content);
        let pointer = ctx.input(|i| i.pointer.latest_pos()).map(from_pos2);
        let over_chrome = ctx.is_pointer_over_area();
        // Whether the held primary button went down on the bare canvas. Read
        // on the press frame, before this frame's grab areas exist, so the
        // answer is what the shell's router saw (`consumed` is egui's
        // over-an-area test on the same pointer).
        let (pressed, down, origin) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_down(),
                i.pointer.press_origin(),
            )
        });
        if pressed {
            self.canvas_press =
                !modal_open && !over_chrome && origin.is_some_and(|o| content.contains(o));
        } else if !down {
            self.canvas_press = false;
        }

        // ---- grid, pixel grid ----
        let mut grid = workspace.canvas.view.grid;
        // W10-J: `shows`, not `get`: View > Extras (Ctrl+H) off hides every
        // extra at once while each keeps its own tick.
        grid.visible = flags.shows(ViewFlag::Grid);
        grid.pixel_grid = flags.shows(ViewFlag::PixelGrid);
        if grid.visible || grid.pixel_grid {
            report.grid_suppressed =
                paint::grid(&painter, &camera, &viewport, &grid, canvas, &style);
            report.grid = grid.visible && !report.grid_suppressed;
        }
        workspace.set_grid_suppressed(report.grid_suppressed);

        // ---- guides ----
        let rulers_on = flags.get(ViewFlag::Rulers);
        let gesture = GuideGesture {
            camera,
            viewport,
            outer: content,
            ruler_thickness_pt: style.ruler_thickness_pt,
            rulers_visible: rulers_on,
            // The `ui` host's own band, so a press this close to a guide is
            // the guide's here exactly when it would be there.
            grab_pt: ui::canvas::GUIDE_GRAB_PT,
        };
        if flags.shows(ViewFlag::Guides) {
            // View > Guides *is* the visibility switch here, as it is in the
            // `ui` host (`Workspace::sync_canvas_view` writes the flag over
            // `guides.visible`). The document's own `visible` is persisted
            // state that `editor_core::Guides::default()` leaves false on
            // every opened image, so honouring it would hide every guide the
            // user drags out. It is forced on for the gesture and the
            // painter, then put back, so the `SetGuides` the chrome converges
            // carries the document's value untouched and toggling a view
            // never edits the document.
            // A live transform, crop or path session has handles with a
            // higher claim than a guide under them — the `ui` host's
            // `may_grab` rule — so the guide grab bands stand down while one
            // is up. The ruler gutters stay live: no handle sits in them.
            let sessions = &workspace.canvas.sessions;
            let yield_bands =
                sessions.transform.is_some() || sessions.crop.is_some() || sessions.path.is_some();
            let guides = &mut workspace.canvas.view.guides;
            let document_visible = guides.visible;
            guides.visible = true;
            if modal_open {
                // The scrim owns the pointer: no grab areas above it, and a
                // drag a dialog interrupted is dropped, not committed.
                self.guide_drag = None;
                self.dragging = None;
            } else {
                self.drive_guides(ctx, &gesture, guides, yield_bands);
            }
            paint::guides(&painter, &camera, &viewport, guides, &style);
            report.guides = guides.len();
            guides.visible = document_visible;
            if let Some(dragging) = &mut self.dragging {
                dragging.visible = document_visible;
            }
        } else {
            self.guide_drag = None;
            self.dragging = None;
        }
        report.dragging_guide = self.guide_drag.is_some();

        // ---- smart guides ----
        let slices = tool_input::committed_slices(editor);
        if flags.shows(ViewFlag::SmartGuides) && !modal_open {
            let policy = SnapPolicy::from_view_flags(flags);
            let moving = match workspace.canvas.sessions.transform.as_ref() {
                Some((state, _)) => Some(state.corners),
                None => self.move_drag(ctx, editor, doc, &slices, policy),
            };
            if let Some(corners) = moving {
                let hits = smart_guide_hits(doc, &slices, &corners, policy);
                paint::smart_guides(&painter, &camera, &viewport, &hits, &style);
                report.smart_guides = hits.len();
            }
        }

        // ---- slices (W10-J: View > Show > Slices) ----
        if flags.shows(ViewFlag::Slices) && !slices.is_empty() {
            // W10-A: each slice labelled by its name, as the export names it.
            let labels: Vec<String> = editor
                .slices
                .options(doc.id())
                .iter()
                .map(|o| tools::slice_select::slice_label(&o.name))
                .collect();
            report.slices = paint_slices(
                ctx,
                &painter,
                (&camera, &viewport),
                &slices,
                &labels,
                &style,
            );
        }

        // ---- note pins (W10-B) ----
        // A note is annotation over the image, never in it: drawn here, on
        // the overlay, and read by no compositor or exporter. View > Extras
        // (Ctrl+H) hides the pins with every other extra.
        if flags.get(ViewFlag::Extras) && !doc.document.extras.notes.is_empty() {
            report.notes = paint_notes(
                ctx,
                &painter,
                &camera,
                &viewport,
                &doc.document.extras.notes,
                &style,
            );
        }

        // ---- layer edges ----
        if flags.shows(ViewFlag::LayerEdges) {
            let edges: Vec<DocRect> = doc
                .document
                .active_layer()
                .and_then(|id| self.layer_bounds(doc, id))
                .map(DocRect::of_pixel_rect)
                .filter(|r| !r.is_empty())
                .into_iter()
                .collect();
            paint::layer_edges(&painter, &camera, &viewport, &edges, &style);
            report.layer_edges = edges.len();
        }

        // ---- rulers ----
        if rulers_on {
            let spec = RulerSpec {
                unit: workspace.canvas.unit.into(),
                dpi: ruler_dpi(workspace.canvas.resolution_ppi),
                doc_extent: doc_size,
                ..RulerSpec::default()
            };
            paint::rulers(&painter, &camera, &viewport, content, &spec, &style);
            if let Some(p) = pointer {
                paint::ruler_pointer_mark(&painter, content, style.ruler_thickness_pt, p, &style);
            }
            report.rulers = true;
        }

        // ---- precise cursor ----
        if flags.get(ViewFlag::PreciseCursor) && !over_chrome && !modal_open {
            if let Some(p) = pointer.filter(|p| content.contains(to_pos2(*p))) {
                report.precise_cursor =
                    paint::precise_cursor(&painter, p, Space::Medium.pt(), &style);
            }
        }

        // ---- the tool's cursor, the brush ring, the context menu (W4-C) ----
        // The ruler gutters are egui areas only while unlocked guides can be
        // pulled from them (`drive_guides`); with Guides off or locked they
        // are painted but own no area, so they are left out by rectangle.
        let gutters = if rulers_on {
            rulers::gutters(content, style.ruler_thickness_pt)
        } else {
            [egui::Rect::NOTHING; 2]
        };
        // W8-A: a pen's own position when the shell's pen route has one, so
        // the ring follows a hovering pen whether or not egui-winit's
        // touch-to-mouse emulation moved egui's pointer to it (it does for a
        // hover `Moved`, and drops the pointer at every lift).
        let ring_pointer = self.pen_hover.map(|p| p / ppp).or(pointer);
        let on_canvas = ring_pointer.filter(|p| {
            let at = to_pos2(*p);
            !over_chrome
                && !modal_open
                && content.contains(at)
                && !gutters.iter().any(|g| g.contains(at))
        });
        if let Some(p) = on_canvas {
            let cursor = self.tool_cursor(ctx, editor, flags.get(ViewFlag::PreciseCursor));
            ctx.set_cursor_icon(cursor.to_egui());
            report.cursor = Some(cursor);
            if cursor == CanvasCursor::BrushOutline {
                let brush = editor.brush_for(editor.effective_tool());
                let at = camera.doc_of_screen_pt(&viewport, p);
                // A mouse has no pressure: the ring is the full-size dab.
                let ring = brush_cursor::build(&brush, 1.0, at, &camera, &viewport);
                paint::brush(&painter, &ring, &style);
                let inner = brush_cursor::hardness_ring(&ring, brush.hardness);
                if !inner.is_empty() {
                    let points: Vec<egui::Pos2> = inner.into_iter().map(to_pos2).collect();
                    // The outer ring's two strokes: a base for contrast
                    // under the over-colour hairline.
                    painter.add(egui::Shape::closed_line(
                        points.clone(),
                        style.thick(style.brush_ring_base),
                    ));
                    painter.add(egui::Shape::closed_line(
                        points,
                        style.hairline(style.brush_ring_over),
                    ));
                }
                report.brush_ring = !ring.outline.is_empty();
            }
            let right_click = ctx.input(|i| {
                if i.pointer.button_clicked(egui::PointerButton::Secondary) {
                    i.pointer.interact_pos()
                } else {
                    None
                }
            });
            if let Some(pos) = right_click {
                ui::context_menu::open(workspace, ui::context_menu::ContextTarget::Canvas, pos);
                report.context_menu = true;
            }
        }

        self.last = report;
        report
    }

    /// The cursor the acting tool shows over the canvas this frame.
    ///
    /// The tool's own ([`cursor_for_tool_id`], `precise` swapping the
    /// pictorial ones for a crosshair), with the gestures that override it: a
    /// held middle button is a pan under any tool, the Hand closes while its
    /// press on the canvas is held, and Alt turns the zoom glass round.
    fn tool_cursor(&self, ctx: &egui::Context, editor: &Editor, precise: bool) -> CanvasCursor {
        let tool = editor.effective_tool();
        let (primary, middle, alt) = ctx.input(|i| {
            (
                i.pointer.primary_down(),
                i.pointer.middle_down(),
                i.modifiers.alt,
            )
        });
        let base = cursor_for_tool_id(tool, precise);
        if middle {
            return ui::canvas::cursor::resolve(base, CursorOverride::Hand { dragging: true });
        }
        match tool {
            tools::ToolId::Hand => ui::canvas::cursor::resolve(
                base,
                CursorOverride::Hand {
                    dragging: primary && self.canvas_press,
                },
            ),
            tools::ToolId::Zoom if alt => CanvasCursor::ZoomOut,
            _ => base,
        }
    }

    /// The box a plain Move-tool drag is carrying this frame, in document
    /// space, or `None` when no such drag is running.
    ///
    /// The Move tool publishes no session (its box is only shown with Show
    /// Transform Controls, and then unmoved), so the drag is read where the
    /// shell's router reads it: the primary press that landed on the bare
    /// canvas ([`Self::canvas_press`]) and the pointer now, mapped through
    /// the router's own camera ([`tool_input::canvas_camera_of`], in physical
    /// pixels as the shell's cursor is). A held Space turns the drag into a
    /// pan, as it does in the router.
    fn move_drag(
        &mut self,
        ctx: &egui::Context,
        editor: &Editor,
        doc: &OpenDocument,
        slices: &[raster::PixelRect],
        policy: SnapPolicy,
    ) -> Option<[Vec2; 4]> {
        if !self.canvas_press || self.guide_drag.is_some() || editor.tool() != tools::ToolId::Move {
            return None;
        }
        let (origin, now, space) = ctx.input(|i| {
            (
                i.pointer.press_origin(),
                i.pointer.latest_pos(),
                i.key_down(egui::Key::Space),
            )
        });
        if space {
            return None;
        }
        let (origin, now) = (origin?, now?);
        let layer = doc.document.active_layer()?;
        let base = self.layer_bounds(doc, layer)?;
        let ppp = ctx.pixels_per_point();
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        let camera = tool_input::canvas_camera_of(&doc.camera);
        let viewport = tool_input::canvas_viewport(&doc.camera);
        let to_doc = |p: egui::Pos2| {
            crate::interaction_geometry::screen_to_document(&camera, &viewport, from_pos2(p) * ppp)
        };
        move_drag_corners(doc, slices, base, to_doc(origin), to_doc(now), policy)
    }

    /// The active layer's tight bounds in document space, from the cache when
    /// the layer has not changed since they were measured.
    fn layer_bounds(
        &mut self,
        doc: &OpenDocument,
        id: layer_model::LayerId,
    ) -> Option<raster::PixelRect> {
        let fingerprint = crate::doc::LayerThumbCache::layer_fingerprint(doc, id, 0)?;
        let key = (doc.id(), id, fingerprint);
        if let Some((cached, bounds)) = &self.edge_cache {
            if *cached == key {
                return *bounds;
            }
        }
        let bounds = tool_input::tight_document_bounds(&doc.document, &doc.tiles, id);
        self.edge_cache = Some((key, bounds));
        bounds
    }

    /// The guide gesture: claim the ruler gutters and the unlocked guides'
    /// grab bands as egui areas, and drive [`GuideGesture`] from the pointer.
    ///
    /// The areas are what keep a press in a gutter from reaching the active
    /// tool: the shell hands the tool router only pointer events egui did not
    /// consume, and egui consumes a press over one of its areas. Without them
    /// pulling a guide out of the ruler would also paint a stroke.
    fn drive_guides(
        &mut self,
        ctx: &egui::Context,
        gesture: &GuideGesture,
        guides: &mut Guides,
        yield_bands: bool,
    ) {
        // A drag in progress: the view was reseeded from the (not yet
        // updated) document this frame, so put the in-flight set back.
        if self.guide_drag.is_some() {
            if let Some(dragging) = &self.dragging {
                let visible = guides.visible;
                *guides = dragging.clone();
                guides.visible = visible;
            }
        }
        let thickness = if gesture.rulers_visible {
            gesture.ruler_thickness_pt
        } else {
            0.0
        };
        let mut regions: Vec<(egui::Id, egui::Rect)> = Vec::new();
        if !guides.locked && thickness > 0.0 {
            for (i, gutter) in rulers::gutters(gesture.outer, thickness)
                .into_iter()
                .enumerate()
            {
                regions.push((egui::Id::new((GUTTER_AREA, i)), gutter));
            }
        }
        if guides.visible && !guides.locked && !yield_bands {
            let image = egui::Rect::from_min_max(
                egui::pos2(
                    gesture.outer.min.x + thickness,
                    gesture.outer.min.y + thickness,
                ),
                gesture.outer.max,
            );
            for (i, guide) in guides.iter().enumerate() {
                if guide.locked {
                    continue;
                }
                let Some(at) = guide.screen_pt(&gesture.camera, &gesture.viewport) else {
                    continue;
                };
                let band = match guide.axis {
                    Axis::X => egui::Rect::from_min_max(
                        egui::pos2(at - gesture.grab_pt, image.min.y),
                        egui::pos2(at + gesture.grab_pt, image.max.y),
                    ),
                    Axis::Y => egui::Rect::from_min_max(
                        egui::pos2(image.min.x, at - gesture.grab_pt),
                        egui::pos2(image.max.x, at + gesture.grab_pt),
                    ),
                }
                .intersect(image);
                if band.is_positive() {
                    regions.push((egui::Id::new((GUIDE_AREA, i)), band));
                }
            }
        }

        let mut grabbed: Option<Vec2> = None;
        for (id, rect) in regions {
            let response = egui::Area::new(id)
                .order(egui::Order::Middle)
                .fixed_pos(rect.min)
                .interactable(true)
                .show(ctx, |ui| ui.allocate_rect(rect, egui::Sense::drag()))
                .inner;
            if response.drag_started() {
                // Where the press landed, not where the pointer is once egui
                // has decided it is a drag: a quick pull out of a thin ruler
                // gutter is already past it by then.
                let origin = ctx.input(|i| i.pointer.press_origin());
                if let Some(pos) = origin.or_else(|| response.interact_pointer_pos()) {
                    grabbed = Some(from_pos2(pos));
                }
            }
        }

        if self.guide_drag.is_none() {
            if let Some(pos) = grabbed {
                if let GuideGrab::Start(drag) = gesture.begin(guides, pos) {
                    self.guide_drag = Some(drag);
                }
            }
        }
        if let Some(drag) = self.guide_drag {
            let (down, pos) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos()));
            let pos = pos.map(from_pos2).unwrap_or(Vec2::NAN);
            if down {
                self.guide_drag = gesture.drag(drag, guides, pos);
                ctx.request_repaint();
            } else {
                let _ = gesture.finish(drag, guides, pos);
                self.guide_drag = None;
            }
        }
        self.dragging = self.guide_drag.map(|_| guides.clone());
    }
}

/// The resolution the rulers measure inches against: the workspace's when it
/// has one, otherwise the `ui` default.
fn ruler_dpi(resolution_ppi: f32) -> f32 {
    if resolution_ppi.is_finite() && resolution_ppi > 0.0 {
        resolution_ppi
    } else {
        RulerSpec::default().dpi
    }
}

/// Where a Move drag from `start` to `now` (document points) carries a layer
/// whose tight ink is `base`: the box's four corners, clockwise from the
/// top-left, after the snap the Move tool applies — [`tools::snap_delta`]
/// against the same [`tool_input::snap_candidates_for`] list, threshold and
/// selection the router hands the tool, so the box drawn is the box the
/// release commits. `None` for a non-finite pointer.
pub(crate) fn move_drag_corners(
    doc: &OpenDocument,
    slices: &[raster::PixelRect],
    base: raster::PixelRect,
    start: Vec2,
    now: Vec2,
    policy: SnapPolicy,
) -> Option<[Vec2; 4]> {
    let delta = now - start;
    if !delta.is_finite() {
        return None;
    }
    let selected = doc.document.layer_selection();
    let (candidates, threshold) = tool_input::snap_candidates_for(
        &doc.document,
        &doc.tiles,
        doc.camera.zoom,
        &selected,
        slices,
        policy,
    );
    let d = tools::snap_delta(base, delta, &candidates, threshold);
    let min = Vec2::new(base.x as f32, base.y as f32) + d;
    let max = min + Vec2::new(base.width as f32, base.height as f32);
    Some([min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)])
}

/// W10-J: View > Show > Slices: each committed slice's outline, projected
/// corner by corner so a turned view tilts it with the image, labelled with
/// `labels` (W10-A: the slice's own name, as Slice Select and the export
/// name it; its place in the set only when it has no label). Returns how
/// many were drawn.
fn paint_slices(
    ctx: &egui::Context,
    painter: &egui::Painter,
    (camera, viewport): (&ui::canvas::CanvasCamera, &ui::canvas::Viewport),
    slices: &[raster::PixelRect],
    labels: &[String],
    style: &CanvasStyle,
) -> usize {
    let tokens = design::current_theme(ctx).tokens();
    let font = design::egui_theme::font_id(tokens, design::TypeRole::Caption);
    let stroke = style.hairline(style.guide);
    let pad = Space::XSmall.pt();
    let mut drawn = 0;
    for (i, r) in slices.iter().enumerate() {
        let rect = DocRect::of_pixel_rect(*r);
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
        let label_at = quad[0] + egui::vec2(pad, pad);
        painter.add(egui::Shape::closed_line(quad, stroke));
        painter.text(
            label_at,
            egui::Align2::LEFT_TOP,
            labels
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("{:02}", i + 1)),
            font.clone(),
            style.guide,
        );
        drawn += 1;
    }
    drawn
}

/// W10-B: a pin at each note's document position — a filled disc in the
/// handle colours, numbered in the Notes panel's order — projected through
/// the camera so it rides the image through pan, zoom and a turned view.
/// Returns how many were drawn.
fn paint_notes(
    ctx: &egui::Context,
    painter: &egui::Painter,
    camera: &ui::canvas::CanvasCamera,
    viewport: &ui::canvas::Viewport,
    notes: &[layer_model::Note],
    style: &CanvasStyle,
) -> usize {
    let tokens = design::current_theme(ctx).tokens();
    let font = design::egui_theme::font_id(tokens, design::TypeRole::Caption);
    let radius = Space::Small.pt() * 0.5;
    let pad = Space::XSmall.pt();
    let mut drawn = 0;
    for (i, note) in notes.iter().enumerate() {
        let at = to_pos2(camera.screen_pt_of(viewport, Vec2::new(note.x, note.y)));
        if at.any_nan() {
            continue;
        }
        painter.circle(
            at,
            radius,
            style.handle_selected,
            style.hairline(style.handle_stroke),
        );
        painter.text(
            at + egui::vec2(radius + pad, -radius - pad),
            egui::Align2::LEFT_BOTTOM,
            format!("{}", i + 1),
            font.clone(),
            style.handle_selected,
        );
        drawn += 1;
    }
    drawn
}

/// The smart guides a moving box has caught on.
///
/// The session's destination corners are compared, per axis, with the
/// candidates the tool snapped against — the same list, from
/// [`tool_input::snap_candidates_kinded`] — and a candidate within half a
/// screen pixel of the moving box's edge or centre is a hit. Only the kinds
/// [`ui::canvas::SnapKind::shows_smart_guide`] says draw a line are kept: a
/// canvas edge is already the document border, and grid lines are already on
/// screen.
fn smart_guide_hits(
    doc: &OpenDocument,
    slices: &[raster::PixelRect],
    corners: &[Vec2; 4],
    policy: SnapPolicy,
) -> Vec<SnapHit> {
    if !policy.enabled {
        return Vec::new();
    }
    let selected = doc.document.layer_selection();
    let (candidates, _) = tool_input::snap_candidates_kinded(
        &doc.document,
        &doc.tiles,
        doc.camera.zoom,
        &selected,
        slices,
        policy,
    );
    let tolerance = 0.5 / doc.camera.zoom.max(0.05);
    let extent = |axis: Axis| -> [f32; 3] {
        let values = corners.map(|c| axis.of(c));
        let lo = values.iter().copied().fold(f32::INFINITY, f32::min);
        let hi = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        [lo, hi, (lo + hi) * 0.5]
    };
    let x = extent(Axis::X);
    let y = extent(Axis::Y);
    candidates
        .into_iter()
        .filter(|c| c.kind.shows_smart_guide())
        .filter(|c| {
            let moving = match c.axis {
                Axis::X => x,
                Axis::Y => y,
            };
            moving
                .iter()
                .any(|v| v.is_finite() && (v - c.doc).abs() <= tolerance)
        })
        .map(|candidate| SnapHit {
            candidate,
            distance_pt: 0.0,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    //! W4-C through the real chrome: `Chrome::ui` on a headless egui frame,
    //! with the pointer where a user's would be, reading back the painted
    //! shapes and the platform output egui-winit would hand the OS.
    use crate::chrome::{install_theme, Chrome};
    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use tools::ToolId;

    /// The window is 1400x900 points; an 8x8 document centred at zoom 4
    /// puts document (4, 4) at the window's centre, well clear of the docks.
    const CENTRE: egui::Pos2 = egui::pos2(700.0, 450.0);
    const ZOOM: f32 = 4.0;

    struct Rig {
        _dir: tempfile::TempDir,
        editor: Editor,
        chrome: Chrome,
        ctx: egui::Context,
    }

    fn rig(tool: ToolId) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.png");
        std::fs::write(
            &p,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        let mut editor = Editor::with_state(
            AppPaths::rooted(dir.path().join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        editor.open_path(&p).unwrap();
        {
            let doc = editor.active_mut().unwrap();
            doc.set_viewport(glam::Vec2::new(1400.0, 900.0));
            doc.camera.zoom = ZOOM;
            doc.camera.center = glam::Vec2::new(4.0, 4.0);
        }
        editor.set_tool(tool);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut rig = Rig {
            _dir: dir,
            editor,
            chrome: Chrome::new(),
            ctx,
        };
        // Let the layout settle before the pointer arrives.
        for _ in 0..3 {
            rig.step(Vec::new());
        }
        rig
    }

    impl Rig {
        /// One frame with `events`; the chrome's commands, actions and menu
        /// picks are applied as the shell applies them.
        fn step(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
            let mut out = None;
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let editor = &mut self.editor;
            let chrome = &mut self.chrome;
            let full = self.ctx.run(input, |ctx| {
                out = Some(chrome.ui(ctx, editor));
            });
            let out = out.expect("a frame ran");
            for command in out.commands {
                self.editor.apply_command(command);
            }
            for action in out.actions {
                let _ = self.editor.dispatch(action);
            }
            // A menu row's pick, performed as `Shell` performs it.
            for action in out.menu {
                let _ = crate::menu_bridge::perform(action, &mut self.editor);
            }
            full
        }

        fn hover(&mut self, at: egui::Pos2) -> egui::FullOutput {
            let _ = self.step(vec![egui::Event::PointerMoved(at)]);
            self.step(vec![egui::Event::PointerMoved(at)])
        }

        fn click(&mut self, at: egui::Pos2, button: egui::PointerButton) -> egui::FullOutput {
            let press = |pressed| egui::Event::PointerButton {
                pos: at,
                button,
                pressed,
                modifiers: egui::Modifiers::default(),
            };
            let _ = self.step(vec![egui::Event::PointerMoved(at)]);
            self.step(vec![press(true), press(false)])
        }
    }

    /// The mean radius about `centre` of every closed ring painted in the
    /// brush ring's over-colour.
    fn rings(full: &egui::FullOutput, ctx: &egui::Context, centre: egui::Pos2) -> Vec<f32> {
        let over = ui::canvas::CanvasStyle::from_context(ctx).brush_ring_over;
        full.shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Path(p)
                    if p.closed
                        && p.points.len() == ui::canvas::brush_cursor::OUTLINE_SEGMENTS
                        && p.stroke.color == egui::epaint::ColorMode::Solid(over) =>
                {
                    let sum: f32 = p.points.iter().map(|q| (*q - centre).length()).sum();
                    Some(sum / p.points.len() as f32)
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_brush_tool_paints_a_ring_of_size_times_zoom_at_the_pointer() {
        let mut rig = rig(ToolId::Brush);
        let brush = tools::BrushSettings {
            size: 20.0,
            hardness: 0.5,
            size_pressure: false,
            ..rig.editor.brush_for(ToolId::Brush)
        };
        rig.editor.set_brush(brush);
        let full = rig.hover(CENTRE);
        let radii = rings(&full, &rig.ctx, CENTRE);
        let expected = 20.0 * ZOOM / 2.0;
        assert!(
            radii.iter().any(|r| (r - expected).abs() < 0.5),
            "a ring of radius {expected} at the pointer: {radii:?}"
        );
        assert!(
            radii.iter().any(|r| (r - expected * 0.5).abs() < 0.5),
            "the hardness ring at half the radius: {radii:?}"
        );
        let report = rig.chrome.extras_report();
        assert!(report.brush_ring);
        assert_eq!(report.cursor, Some(ui::canvas::CanvasCursor::BrushOutline));
        // The ring *is* the cursor: the system pointer is hidden under it.
        assert_eq!(full.platform_output.cursor_icon, egui::CursorIcon::None);

        // Off the canvas, over the left tool palette: no ring at all.
        let off = egui::pos2(4.0, 450.0);
        let full = rig.hover(off);
        assert!(rings(&full, &rig.ctx, off).is_empty());
        assert!(!rig.chrome.extras_report().brush_ring);
    }

    /// The first point along `line` (x or y from 0 up) where the extras put
    /// the tool's cursor, or `None` if none does within 300pt.
    fn first_on_canvas(rig: &mut Rig, along_x: bool) -> Option<f32> {
        (0..300).map(|i| i as f32).find(|&v| {
            let at = if along_x {
                egui::pos2(v, CENTRE.y)
            } else {
                egui::pos2(CENTRE.x, v)
            };
            let _ = rig.hover(at);
            rig.chrome.extras_report().cursor.is_some()
        })
    }

    #[test]
    fn the_ruler_gutters_are_not_canvas_with_guides_off() {
        // Rulers off: where the bare canvas starts on each edge.
        let mut rig = rig(ToolId::Brush);
        for (flag, on) in [(ui::ViewFlag::Rulers, false), (ui::ViewFlag::Guides, false)] {
            rig.chrome.emit(ui::Intent::SetViewFlag { flag, on });
        }
        let _ = rig.step(Vec::new());
        let left = first_on_canvas(&mut rig, true).expect("the canvas starts on the left");
        let top = first_on_canvas(&mut rig, false).expect("the canvas starts at the top");

        // Rulers on, Guides off: the gutters own no egui area, and still are
        // not canvas — no ring, no tool cursor, no canvas menu on a right-click.
        rig.chrome.emit(ui::Intent::SetViewFlag {
            flag: ui::ViewFlag::Rulers,
            on: true,
        });
        let _ = rig.step(Vec::new());
        let _ = rig.step(Vec::new());
        let t = ui::canvas::CanvasStyle::from_context(&rig.ctx).ruler_thickness_pt;
        assert!(t > 1.0, "a ruler of some thickness: {t}");
        for at in [
            egui::pos2(left + t / 2.0, CENTRE.y),
            egui::pos2(CENTRE.x, top + t / 2.0),
        ] {
            let full = rig.hover(at);
            let report = rig.chrome.extras_report();
            assert!(report.rulers, "the rulers are painted");
            assert_eq!(
                report.cursor, None,
                "no tool cursor over the gutter at {at:?}"
            );
            assert!(
                !report.brush_ring,
                "no brush ring over the gutter at {at:?}"
            );
            assert!(rings(&full, &rig.ctx, at).is_empty());
            let _ = rig.click(at, egui::PointerButton::Secondary);
            assert!(
                !rig.chrome.extras_report().context_menu,
                "a right-click on the gutter at {at:?} is not the canvas menu"
            );
        }
        // Just past the gutters it is canvas again (the scan is in whole
        // points, the edges need not be).
        for (edge, along_x) in [(left, true), (top, false)] {
            let inside = first_on_canvas(&mut rig, along_x).expect("canvas past the gutter");
            assert!(
                (inside - (edge + t)).abs() <= 1.0,
                "canvas resumes at {inside}, the gutter ends at {}",
                edge + t
            );
        }
    }

    #[test]
    fn each_tool_sets_its_own_cursor_over_the_canvas() {
        for (tool, icon) in [
            (ToolId::Hand, egui::CursorIcon::Grab),
            (ToolId::Zoom, egui::CursorIcon::ZoomIn),
            (ToolId::Move, egui::CursorIcon::Move),
            (ToolId::Type, egui::CursorIcon::Text),
        ] {
            let mut rig = rig(tool);
            let full = rig.hover(CENTRE);
            assert_eq!(full.platform_output.cursor_icon, icon, "{tool:?}");
        }
    }

    #[test]
    fn a_right_click_on_the_canvas_opens_its_menu_and_a_row_performs() {
        let mut rig = rig(ToolId::Brush);
        assert!(matches!(
            rig.editor.active().unwrap().document.selection,
            editor_core::Selection::None
        ));
        let _ = rig.click(CENTRE, egui::PointerButton::Secondary);
        assert!(rig.chrome.extras_report().context_menu);
        // The drawer shows it; one quiet frame lays it out.
        let _ = rig.step(Vec::new());
        let actions: Vec<ui::menu::MenuAction> =
            ui::context_menu::canvas_items(&ui::MenuContext::default())
                .into_iter()
                .map(|i| i.action)
                .collect();
        for i in 0..actions.len() {
            assert!(
                rig.ctx
                    .read_response(ui::context_menu::ids::context_item(i))
                    .is_some(),
                "row {i} of the canvas menu was drawn"
            );
        }
        let select_all = actions
            .iter()
            .position(|a| *a == ui::menu::MenuAction::SelectAll)
            .expect("the canvas menu offers Select All");
        let row = rig
            .ctx
            .read_response(ui::context_menu::ids::context_item(select_all))
            .unwrap()
            .rect
            .center();
        let _ = rig.click(row, egui::PointerButton::Primary);
        let _ = rig.step(Vec::new());
        assert!(
            !matches!(
                rig.editor.active().unwrap().document.selection,
                editor_core::Selection::None
            ),
            "choosing the row selected the canvas"
        );
        // `read_response` answers from the frame before: one more frame so
        // the one it reads is a frame the menu was not drawn in.
        let _ = rig.step(Vec::new());
        assert!(
            rig.ctx
                .read_response(ui::context_menu::ids::context_item(0))
                .is_none(),
            "the menu closed after the choice"
        );
    }

    /// W10-J: the menu row Ctrl+H reaches, resolved and emitted as a click
    /// is.
    fn pick(rig: &mut Rig, action: ui::menu::MenuAction) {
        let context = crate::menu_bridge::context(&mut rig.editor, rig.chrome.workspace());
        let intent = crate::menu_bridge::resolve_intent(action, &context, &rig.editor)
            .unwrap_or_else(|e| panic!("{action:?} is greyed: {e}"));
        rig.chrome.emit(intent);
        let _ = rig.step(Vec::new());
    }

    /// W10-B: the document's notes are pinned on the canvas — one disc per
    /// note at its document position through the camera (two notes six
    /// pixels apart at 400% land 24 points apart) — and View > Extras takes
    /// them down with every other extra.
    #[test]
    fn notes_are_pinned_on_the_canvas_at_their_document_positions() {
        use ui::menu::MenuAction;
        use ui::ViewFlag;
        let mut rig = rig(ToolId::Move);
        for (x, y) in [(1.0, 1.0), (7.0, 7.0)] {
            let doc = &rig.editor.active().unwrap().document;
            let (command, _) = editor_core::extras::add_note(doc, x, y, "", "Check");
            rig.editor.apply_command(command);
        }
        let _ = rig.step(Vec::new());
        let full = rig.step(Vec::new());
        assert_eq!(rig.chrome.extras_report().notes, 2);
        let pin = ui::canvas::CanvasStyle::from_context(&rig.ctx).handle_selected;
        let centres: Vec<egui::Pos2> = full
            .shapes
            .iter()
            .filter_map(|c| match &c.shape {
                egui::Shape::Circle(circle) if circle.fill == pin => Some(circle.center),
                _ => None,
            })
            .collect();
        assert_eq!(centres.len(), 2, "one pin per note: {centres:?}");
        let d = centres[1] - centres[0];
        assert!(
            (d.x - 6.0 * ZOOM).abs() < 0.01 && (d.y - 6.0 * ZOOM).abs() < 0.01,
            "the pins are not where the notes are: {centres:?}"
        );

        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Extras));
        let _ = rig.step(Vec::new());
        assert_eq!(rig.chrome.extras_report().notes, 0, "Extras off hides them");
    }

    /// W10-J: View > Show > Slices paints the committed slices, and View >
    /// Extras (Ctrl+H) takes the grid, the guides, the layer edges and the
    /// slices down at once — each keeping its own tick, so a second Ctrl+H
    /// brings back exactly what was showing.
    #[test]
    fn extras_hides_every_overlay_at_once_and_slices_show_the_committed_set() {
        use ui::menu::MenuAction;
        use ui::ViewFlag;
        let mut rig = rig(ToolId::Brush);
        let id = rig.editor.active().unwrap().id();
        rig.editor
            .slices
            .remember(id, vec![raster::PixelRect::new(1, 1, 3, 3)]);
        rig.editor.active_mut().unwrap().document.guides.list = vec![editor_core::Guide {
            axis: editor_core::GuideAxis::Vertical,
            doc: 2.0,
            locked: false,
        }];
        rig.chrome.emit(ui::Intent::SetViewFlag {
            flag: ViewFlag::Grid,
            on: true,
        });
        let _ = rig.step(Vec::new());
        let _ = rig.step(Vec::new());
        let shown = rig.chrome.extras_report();
        assert_eq!(shown.slices, 1, "the committed slice is painted: {shown:?}");
        assert_eq!(shown.guides, 1, "{shown:?}");
        assert!(shown.grid, "{shown:?}");
        assert_eq!(shown.layer_edges, 1, "{shown:?}");

        // The chord is the menu row's.
        let chord = crate::keymap::Chord::ctrl(crate::keymap::Key::character('h'));
        assert_eq!(
            crate::keymap::Keymap::default().resolve_any(&chord),
            Some(crate::keymap::Resolved::Menu(MenuAction::ToggleView(
                ViewFlag::Extras
            )))
        );
        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Extras));
        let _ = rig.step(Vec::new());
        let hidden = rig.chrome.extras_report();
        assert_eq!(
            (
                hidden.slices,
                hidden.guides,
                hidden.grid,
                hidden.layer_edges
            ),
            (0, 0, false, 0),
            "Extras off hides them all: {hidden:?}"
        );
        let flags = rig.chrome.workspace().view_flags;
        assert!(flags.get(ViewFlag::Grid) && flags.get(ViewFlag::Guides));
        assert!(!flags.get(ViewFlag::Extras));

        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Extras));
        let _ = rig.step(Vec::new());
        let back = rig.chrome.extras_report();
        assert_eq!((back.slices, back.guides, back.grid), (1, 1, true));

        // View > Show > Slices on its own.
        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Slices));
        let _ = rig.step(Vec::new());
        assert_eq!(rig.chrome.extras_report().slices, 0, "Slices unticked");
        assert_eq!(rig.chrome.extras_report().guides, 1, "the rest stay");
    }

    /// W10-J round 2: View > Extras (Ctrl+H) also takes down the marching
    /// ants the shell strokes (`Chrome::selection_ants`, what
    /// `Shell::redraw` calls), with View > Selection Edges still ticked, and
    /// a second Ctrl+H brings them back.
    #[test]
    fn extras_off_hides_the_marching_ants_the_shell_draws() {
        use ui::menu::MenuAction;
        use ui::ViewFlag;
        let mut rig = rig(ToolId::Brush);
        rig.editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 1),
            max: glam::IVec2::new(6, 6),
        };
        let _ = rig.step(Vec::new());
        let ants = |rig: &Rig| {
            let doc = rig.editor.active().unwrap();
            let mut outline = crate::presenter::SelectionOutline::new();
            rig.chrome
                .selection_ants(&mut outline, doc, 0.0, &Default::default())
        };
        assert!(!ants(&rig).is_empty(), "precondition: the ants are drawn");

        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Extras));
        let _ = rig.step(Vec::new());
        let flags = rig.chrome.workspace().view_flags;
        assert!(flags.get(ViewFlag::SelectionEdges), "its own tick stays");
        assert!(!rig.chrome.selection_edges_visible());
        assert!(ants(&rig).is_empty(), "Extras off still drew the ants");

        pick(&mut rig, MenuAction::ToggleView(ViewFlag::Extras));
        let _ = rig.step(Vec::new());
        assert!(!ants(&rig).is_empty(), "Extras back on, the ants are back");
    }
}
