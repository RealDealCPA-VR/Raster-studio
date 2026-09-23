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
    paint, rulers, Axis, CanvasStyle, DocRect, GuideDrag, GuideGesture, GuideGrab, Guides,
    PanelInsets, RulerSpec, SnapHit, Viewport,
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
    /// The precise-cursor crosshair was painted.
    pub precise_cursor: bool,
    /// A guide drag is in progress.
    pub dragging_guide: bool,
}

impl CanvasExtras {
    /// What the last call to [`CanvasExtras::paint`] drew.
    pub fn last_report(&self) -> ExtrasReport {
        self.last
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
        // whole window, because that is the rectangle the shell composites
        // across (`Chrome::sync_canvas_host` says why). In egui points, so
        // the painter and the pointer agree on a scaled display.
        //
        // `render::Camera::viewport_size` is in physical pixels and its zoom is
        // physical pixels per document pixel; `ui::canvas::Viewport` takes the
        // surface in points and converts through the scale, so the surface is
        // divided by it here and the zoom is left as it is.
        let camera = crate::interaction_geometry::canvas_camera_of(&doc.camera);
        let ppp = ctx.pixels_per_point();
        let ppp = if ppp.is_finite() && ppp > 0.0 {
            ppp
        } else {
            1.0
        };
        let viewport = Viewport::new(doc.camera.viewport_size / ppp, PanelInsets::NONE, ppp);
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
        grid.visible = flags.get(ViewFlag::Grid);
        grid.pixel_grid = flags.get(ViewFlag::PixelGrid);
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
        if flags.get(ViewFlag::Guides) {
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
        if flags.get(ViewFlag::SmartGuides) && !modal_open {
            let policy = SnapPolicy::from_view_flags(flags);
            let moving = match workspace.canvas.sessions.transform.as_ref() {
                Some((state, _)) => Some(state.corners),
                None => self.move_drag(ctx, editor, doc, policy),
            };
            if let Some(corners) = moving {
                let hits = smart_guide_hits(doc, &corners, policy);
                paint::smart_guides(&painter, &camera, &viewport, &hits, &style);
                report.smart_guides = hits.len();
            }
        }

        // ---- layer edges ----
        if flags.get(ViewFlag::LayerEdges) {
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

        self.last = report;
        report
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
        let viewport = tool_input::canvas_viewport(doc.camera.viewport_size);
        let to_doc = |p: egui::Pos2| {
            crate::interaction_geometry::screen_to_document(&camera, &viewport, from_pos2(p) * ppp)
        };
        move_drag_corners(doc, base, to_doc(origin), to_doc(now), policy)
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
        policy,
    );
    let d = tools::snap_delta(base, delta, &candidates, threshold);
    let min = Vec2::new(base.x as f32, base.y as f32) + d;
    let max = min + Vec2::new(base.width as f32, base.height as f32);
    Some([min, Vec2::new(max.x, min.y), max, Vec2::new(min.x, max.y)])
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
fn smart_guide_hits(doc: &OpenDocument, corners: &[Vec2; 4], policy: SnapPolicy) -> Vec<SnapHit> {
    if !policy.enabled {
        return Vec::new();
    }
    let selected = doc.document.layer_selection();
    let (candidates, _) = tool_input::snap_candidates_kinded(
        &doc.document,
        &doc.tiles,
        doc.camera.zoom,
        &selected,
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
