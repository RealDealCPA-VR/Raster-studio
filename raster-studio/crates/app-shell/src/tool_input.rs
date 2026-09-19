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
//! # Some gestures do not end at pointer-up
//!
//! Crop, Slice and Free Transform hold the gesture *after* the button comes up
//! — the crop box waits so its edges can be nudged, the slice set grows across
//! several drags, the transform quad stays live under its handles — and publish
//! only from `Tool::commit`. Nothing called it, so `grep '\.commit('` over this
//! crate returned nothing and a crop drag produced a rectangle, no command, no
//! status and no pixel. [`ToolPointer::commit`] is that call: a transform's
//! Enter confirms (see [`crate::shell::Shell::on_key`]; a text run ends on
//! Ctrl+Enter, card 031) and Escape cancels through the same
//! [`ToolPointer::cancel`] that abandons a stroke. Type and Pen hold a gesture
//! the same way — an open text run, an unfinished path — and end on the same
//! key.
//!
//! A [`tools::ToolRequest`] is not a command, so the two that arrive here are
//! performed rather than applied: a crop becomes the transaction
//! [`crop_command`] builds (a canvas resize plus one translation per root
//! layer, one undo step), and a slice set is reported.
//!
//! # What this cannot do yet
//!
//! * **Rotate View changes nothing on screen.** [`render::Camera`] is
//!   axis-aligned by construction (see its `clip_to_uv`), so the rotation the
//!   tool applies to the mirrored camera has nowhere to be written back to and
//!   is dropped. The gesture reaches the tool and the tool is correct; the
//!   renderer cannot show the result. Hand and Zoom write back in full.
//! * **A selection gesture is undoable (card 056).** The gesture's edits fold
//!   into `Command::SetSelection` entries — one gesture, one step — so undo
//!   and redo carry the selection exactly like any other edit.
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
//! * **A slice set has nowhere to go.** [`ToolPointer::commit`] performs a
//!   crop and hands the caller the slices, and the caller — the shell — has no
//!   route that exports them: slicing means writing one file per region and
//!   nothing in this build asks for a folder. The status bar says so rather
//!   than letting the gesture look like it worked. Pinned by
//!   `committing_slices_reports_them_and_says_they_cannot_be_exported`.
//! * **A crop does not straighten and does not delete.**
//!   [`tools::CropRequest::straighten`] would need every layer resampled and
//!   `delete_cropped` would need the off-canvas pixels thrown away; the crop
//!   this performs resizes the canvas and slides the layers under it, which is
//!   the whole of what [`crop_command`] claims. Both are reported in the status
//!   bar when the user asked for them.

use glam::{UVec2, Vec2};

use compositor::{MemoryTileSource, TileSource};
use editor_core::{Command, Document, PixelKey, PixelStore};
use raster::{PixelRect, TileCoord, TileHash};
use render::{Camera, MAX_ZOOM, MIN_ZOOM};
use tools::{
    registry, CropRequest, PaintTarget, Slice, TileAccess, Tool, ToolContext, ToolId, ToolRequest,
};
use ui::canvas::{
    CanvasCamera, Dispatch, InputRouter, PanelInsets, PointerInput, PointerPhase, Rejected, Route,
    Viewport,
};

use crate::doc::DocumentId;
use crate::editor::Editor;
use layer_model::text::Frame;

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

/// Card 040: the ACTIVE paintable layer's document→layer mapping (Raster,
/// Generator and SmartObject own pixels; text/shape are refused at commit).
/// `None` = identity or not paintable.
/// Card 042: a layer's tight ink extent in DOCUMENT space. Raster kinds
/// scan alpha (`alpha_bounds`, layer space) and map the rect's corners
/// through the layer's full document transform; text/shape kinds have exact
/// geometry bounds from `document_bounds` already.
pub(crate) fn tight_document_bounds(
    doc: &editor_core::Document,
    tiles: &compositor::MemoryTileSource,
    id: layer_model::LayerId,
) -> Option<PixelRect> {
    let raster = compositor::bounds::alpha_bounds(
        doc,
        tiles,
        id,
        0,
        compositor::CompositeOptions::default(),
    )
    .ok()
    .flatten();
    match raster {
        Some(ink) => {
            let m = crate::interaction_geometry::document_transform_of(doc, id, 0).ok()?;
            let corners = [
                glam::vec2(ink.x as f32, ink.y as f32),
                glam::vec2((ink.x + ink.width as i64) as f32, ink.y as f32),
                glam::vec2(ink.x as f32, (ink.y + ink.height as i64) as f32),
                glam::vec2(
                    (ink.x + ink.width as i64) as f32,
                    (ink.y + ink.height as i64) as f32,
                ),
            ];
            let mapped: Vec<glam::Vec2> = corners.iter().map(|p| m.transform_point2(*p)).collect();
            let min_x = mapped.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
            let min_y = mapped.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
            let max_x = mapped.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
            let max_y = mapped.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
            let min_i = min_x.floor() as i64;
            let min_j = min_y.floor() as i64;
            // checked_sub closes the pathological finite-saturation case.
            let width = (max_x.ceil() as i64).checked_sub(min_i).unwrap_or(0).max(0) as u32;
            let height = (max_y.ceil() as i64).checked_sub(min_j).unwrap_or(0).max(0) as u32;
            Some(PixelRect::new(min_i, min_j, width, height))
        }
        None => compositor::bounds::document_bounds(
            doc,
            tiles,
            id,
            0,
            compositor::CompositeOptions::default(),
        )
        .ok()
        .flatten(),
    }
}

/// Card 042: the snap candidates + document-space threshold for the moving
/// geometry. Canvas edges/center, then every layer NOT in the selected set
/// (nor a descendant of one) contributes its tight bounds' edges and center.
/// Candidate LAYERS are capped so layer-heavy documents stay cheap.
pub(crate) const SNAP_CANDIDATE_LAYER_CAP: usize = 64;

pub(crate) fn snap_candidates_for(
    doc: &editor_core::Document,
    tiles: &compositor::MemoryTileSource,
    zoom: f32,
    selected: &[layer_model::LayerId],
) -> (Vec<tools::SnapCandidate>, f32) {
    let threshold_doc = ui::canvas::snapping::SnapSettings::default().threshold() / zoom.max(0.05);
    let mut candidates = Vec::new();
    let canvas = glam::vec2(doc.width() as f32, doc.height() as f32);
    for (axis, values) in [
        (tools::SnapAxis::X, [0.0f32, canvas.x, canvas.x * 0.5]),
        (tools::SnapAxis::Y, [0.0f32, canvas.y, canvas.y * 0.5]),
    ] {
        for v in values {
            candidates.push(tools::SnapCandidate { axis, doc: v });
        }
    }
    let mut layers = 0usize;
    for id in doc.layers.iter_depth_first() {
        let selected_or_descendant = selected.contains(&id) || {
            let mut parent = doc.layers.parent_of(id);
            while let Some(p) = parent {
                if selected.contains(&p) {
                    break;
                }
                parent = doc.layers.parent_of(p);
            }
            // The walk breaks on a selected ancestor (parent stays Some) or
            // exhausts to the root (parent None) — is_some() is the answer.
            parent.is_some()
        };
        if selected_or_descendant {
            continue;
        }
        if layers >= SNAP_CANDIDATE_LAYER_CAP {
            break;
        }
        let Some(b) = tight_document_bounds(doc, tiles, id) else {
            continue;
        };
        layers += 1;
        for (axis, lo, hi, mid) in [
            (
                tools::SnapAxis::X,
                b.x as f32,
                (b.x + b.width as i64) as f32,
                b.x as f32 + b.width as f32 * 0.5,
            ),
            (
                tools::SnapAxis::Y,
                b.y as f32,
                (b.y + b.height as i64) as f32,
                b.y as f32 + b.height as f32 * 0.5,
            ),
        ] {
            for v in [lo, hi, mid] {
                candidates.push(tools::SnapCandidate { axis, doc: v });
            }
        }
    }
    (candidates, threshold_doc)
}

/// Card 058: pointer samples for MASK painting map through the MASK's
/// document pose — the layer transform composed with the mask's own extra
/// transform (card 043) — because that is the pose the compositor samples
/// the coverage through. A linked mask (identity extra transform) reduces to
/// the layer mapping; an unlinked, independently moved mask paints where it
/// displays, not where the layer's local grid happens to be.
/// Card 058: one mapping per paint target — the layer's document→layer
/// mapping for content, the mask pose's inverse for mask coverage.
fn sample_to_paint_target_of(
    doc: &editor_core::Document,
    layer: Option<layer_model::LayerId>,
    target: tools::PaintTarget,
) -> Option<glam::Affine2> {
    match target {
        tools::PaintTarget::Layer => sample_to_layer_of(doc, layer),
        tools::PaintTarget::Mask => sample_to_mask_of(doc, layer)
            // A mask painting target with no resolvable pose falls back to
            // the layer mapping (the identity-pose answer) rather than to
            // document space, which would paint at the raw pointer position.
            .or_else(|| sample_to_layer_of(doc, layer)),
    }
}

fn sample_to_mask_of(
    doc: &editor_core::Document,
    layer: Option<layer_model::LayerId>,
) -> Option<glam::Affine2> {
    let layer = layer?;
    let layer_transform = crate::interaction_geometry::document_transform_of(doc, layer, 0).ok()?;
    let mask = doc.layers.get(layer)?.mask.as_ref()?;
    (layer_transform * *mask.transform).inverse().into()
}

/// Card 058: the canvas rectangle expressed in the paint target's space —
/// content paints in layer space (the canvas rect itself, matching card
/// 040's semantics); a MASK paints in mask-local space, where the visible
/// canvas is the canvas rect pre-imaged through the mask pose (bounding box
/// of the transformed corners). This is the rect stroke dabs are clipped and
/// rasterized against; `ctx.canvas` is document space and would clip a
/// visible strip of a transformed mask.
fn paint_space_canvas_of(
    doc: &editor_core::Document,
    layer: Option<layer_model::LayerId>,
    target: tools::PaintTarget,
    canvas: PixelRect,
) -> PixelRect {
    match target {
        tools::PaintTarget::Layer => canvas,
        tools::PaintTarget::Mask => {
            let pose = layer
                .and_then(|l| {
                    crate::interaction_geometry::document_transform_of(doc, l, 0)
                        .ok()
                        .zip(
                            doc.layers
                                .get(l)
                                .and_then(|l| l.mask.as_ref())
                                .map(|m| *m.transform),
                        )
                })
                .map(|(t, extra)| t * extra);
            match pose {
                Some(pose) if !is_identity_pose(&pose) => {
                    let c0 =
                        pose.transform_point2(glam::Vec2::new(canvas.x as f32, canvas.y as f32));
                    let c1 = pose.transform_point2(glam::Vec2::new(
                        (canvas.x + canvas.width as i64) as f32,
                        canvas.y as f32,
                    ));
                    let c2 = pose.transform_point2(glam::Vec2::new(
                        canvas.x as f32,
                        (canvas.y + canvas.height as i64) as f32,
                    ));
                    let c3 = pose.transform_point2(glam::Vec2::new(
                        (canvas.x + canvas.width as i64) as f32,
                        (canvas.y + canvas.height as i64) as f32,
                    ));
                    let min_x = c0.x.min(c1.x).min(c2.x.min(c3.x)).floor() as i64;
                    let min_y = c0.y.min(c1.y).min(c2.y.min(c3.y)).floor() as i64;
                    let max_x = c0.x.max(c1.x).max(c2.x.max(c3.x)).ceil() as i64;
                    let max_y = c0.y.max(c1.y).max(c2.y.max(c3.y)).ceil() as i64;
                    PixelRect::new(
                        min_x,
                        min_y,
                        (max_x - min_x).max(1) as u32,
                        (max_y - min_y).max(1) as u32,
                    )
                }
                _ => canvas,
            }
        }
    }
}

/// Card 058: pose identity test (a translation-only check would still
/// resample; only the exact identity takes the 1:1 path).
fn is_identity_pose(pose: &glam::Affine2) -> bool {
    *pose == glam::Affine2::IDENTITY
}

fn sample_to_layer_of(
    doc: &editor_core::Document,
    layer: Option<layer_model::LayerId>,
) -> Option<glam::Affine2> {
    layer
        .filter(|l| {
            matches!(
                doc.layers.get(*l).map(|l| &l.kind),
                Some(layer_model::LayerKind::Raster(_))
                    | Some(layer_model::LayerKind::Generator(_))
                    | Some(layer_model::LayerKind::SmartObject(_))
            )
        })
        .and_then(|layer| {
            crate::interaction_geometry::document_transform_of(doc, layer, 0)
                .ok()
                .map(|t| t.inverse())
        })
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
/// Card 030: one overlay line for the text session — a caret bar or a
/// selection edge, tagged so the shell can colour them apart.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextOverlaySegment {
    /// Start point, in document space.
    pub a: Vec2,
    /// End point, in document space.
    pub b: Vec2,
    /// Caret bars get the brighter colour; selection edges the dimmer one.
    pub kind: TextOverlayKind,
}

/// Card 030: which overlay line a segment is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextOverlayKind {
    /// The caret bar (drawn with the emphasis colour).
    Caret,
    /// A selection rectangle edge (drawn with the dim colour).
    Selection,
    /// Card 032: the paragraph box's frame edge (drawn dimmest).
    BoxFrame,
}

impl TextOverlaySegment {
    /// Overlay emphasis colour for caret bars.
    pub const CARET: TextOverlayKind = TextOverlayKind::Caret;
    /// Overlay dim colour for selection edges.
    pub const SELECTION: TextOverlayKind = TextOverlayKind::Selection;
    /// Card 032: overlay dimmest colour for paragraph-box frame edges.
    pub const BOX_FRAME: TextOverlayKind = TextOverlayKind::BoxFrame;
}

/// Card 030: one rectangle's four edges as overlay segments (the caret rect
/// is zero-width, so its edges collapse to one visible vertical bar — the
/// other three edges are degenerate and harmless to draw).
fn push_rect_edges(out: &mut Vec<TextOverlaySegment>, r: text_engine::Rect, kind: TextOverlayKind) {
    let (x, y, w, h) = (r.x, r.y, r.width, r.height);
    for (a, b) in [
        (Vec2::new(x, y), Vec2::new(x + w, y)),
        (Vec2::new(x + w, y), Vec2::new(x + w, y + h)),
        (Vec2::new(x + w, y + h), Vec2::new(x, y + h)),
        (Vec2::new(x, y + h), Vec2::new(x, y)),
    ] {
        out.push(TextOverlaySegment { a, b, kind });
    }
}

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

/// What confirming a held gesture did — see [`ToolPointer::commit`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CommitOutcome {
    /// The live tool had something to confirm. `false` means the key belongs to
    /// whoever else wants it: there was no crop box, no slice set and no
    /// transform session.
    pub had_pending: bool,
    /// Undoable steps this commit added to the document's history.
    pub steps: usize,
    /// The keep-region a crop was applied at, in the coordinates of the canvas
    /// *before* the cut.
    pub cropped_to: Option<PixelRect>,
    /// The slice set the gesture published.
    pub slices: Vec<Slice>,
    /// Why the commit did not happen.
    pub failed: Option<String>,
}

impl CommitOutcome {
    /// `true` when the window has to be drawn again.
    pub fn needs_repaint(&self) -> bool {
        self.had_pending
    }
}

/// The one undoable command that performs `req`, or `None` when the request
/// describes no canvas at all.
///
/// A crop is two things at once: the canvas becomes the kept rectangle, and
/// every layer slides so the pixel that was at the rectangle's top-left is now
/// at the origin. Both are commands ([`Command::SetCanvasSize`] and one
/// [`Command::TransformLayer`] per **root** layer — a group's transform already
/// carries its whole subtree, so translating the children as well would move
/// them twice), and wrapping them in a [`Command::Transaction`] is what makes
/// the whole crop a single Ctrl+Z.
///
/// # What a crop still does not do
///
/// * [`CropRequest::straighten`] is **not** applied. The angle rides along in
///   the request and [`CropRequest::straightened_corners`] says exactly which
///   quad it means, but resampling that quad back into an axis-aligned document
///   is a re-render of every layer, not a translation. The caller reports it
///   rather than silently cutting the un-straightened rectangle in silence.
/// * [`CropRequest::delete_cropped`] is **not** honoured. The pixels outside
///   the new canvas stay in their layers, off-canvas — which is the
///   non-destructive behaviour, and the one that makes the undo above exact.
pub fn crop_command(document: &Document, req: &CropRequest) -> Option<Command> {
    let rect = req.rect;
    if rect.width == 0 || rect.height == 0 {
        return None;
    }
    let mut commands = vec![Command::SetCanvasSize {
        size: UVec2::new(rect.width, rect.height),
    }];
    if rect.x != 0 || rect.y != 0 {
        let delta = Vec2::new(-(rect.x as f32), -(rect.y as f32));
        for id in document.layers.root() {
            commands.push(Command::TransformLayer {
                layer_id: *id,
                matrix: tools::edit::translation_matrix(delta),
            });
        }
    }
    Some(Command::Transaction {
        label: "Crop".into(),
        commands,
    })
}

/// The pointer half of the shell: the gesture router and the live tool.
///
/// One tool instance is kept alive across the events of a gesture, because a
/// tool *is* a state machine — a stroke that rebuilt its tool between the press
/// and the release would emit nothing at all.
/// Card 055: the edit target a gesture pinned - the full triple the tools
/// route pixels with, not just the surface kind, so a mid-gesture layer
/// change redirects nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PinnedEditTarget {
    pub layer: Option<layer_model::LayerId>,
    pub mask: Option<layer_model::MaskId>,
    pub paint: tools::PaintTarget,
}

/// Card 055: THE target resolver - one function for pointer gestures and
/// off-pointer operations alike, so a brush stroke and a menu fill can never
/// disagree about where pixels land. Quick mask reroutes to the scratch
/// layer's mask (a temporary mode: the sticky edit target is untouched, so
/// exiting restores it); otherwise the sticky edit target decides, with a
/// maskless layer painting content (the read-time validation keeps the
/// stored target honest).
fn resolve_edit_target(
    doc: &editor_core::Document,
    quick_mask: bool,
    quick_mask_layer: Option<layer_model::LayerId>,
    edit_target_is_mask: bool,
    active_layer: Option<layer_model::LayerId>,
    active_mask: Option<layer_model::MaskId>,
) -> PinnedEditTarget {
    if quick_mask {
        if let Some(sid) = quick_mask_layer {
            return PinnedEditTarget {
                layer: Some(sid),
                mask: doc.layers.get(sid).and_then(|l| l.mask_id()),
                paint: PaintTarget::Mask,
            };
        }
    }
    if edit_target_is_mask && active_mask.is_some() {
        PinnedEditTarget {
            layer: active_layer,
            mask: active_mask,
            paint: PaintTarget::Mask,
        }
    } else {
        PinnedEditTarget {
            layer: active_layer,
            mask: active_mask,
            paint: PaintTarget::Layer,
        }
    }
}

#[derive(Default)]
pub struct ToolPointer {
    router: InputRouter,
    /// Card 055: the edit target (layer, mask, paint surface) the running
    /// gesture was PINNED to. Resolved through the one shared resolver at
    /// the gesture's Down sample; a target change - a thumbnail click, a
    /// layer chord - cannot redirect the middle of a stroke or its Up's
    /// commit. Overwritten at every Down (every gesture begins with one);
    /// only ever READ between a Down and its Up, because a sample outside a
    /// gesture is a hover the router declines before the pin is consulted.
    pinned_paint_target: Option<PinnedEditTarget>,
    /// The live tool and the id it was built for.
    current: Option<(ToolId, Box<dyn Tool>)>,
    /// The document the running gesture was aimed at, while one is running.
    ///
    /// Identity, not index: a closed tab renumbers the ones after it, so an
    /// index would silently re-aim the stroke at the document that slid into
    /// the slot. See the module docs.
    aimed_at: Option<DocumentId>,
    /// The document a published session's geometry belongs to (card 012):
    /// claimed with the gesture, retained while the session is visible, so a
    /// free-transform's handles survive its own pointer-up and die with the
    /// session.
    session_doc: Option<DocumentId>,
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

    /// Show the live transform session as a preview (card 013), or clear the
    /// lens when there is no session: the one route every caller shares, so a
    /// committed, cancelled and abandoned gesture all end with the preview
    /// gone and the committed document exactly as it was.
    pub fn settle_preview(&mut self, editor: &mut Editor) {
        let geometry = self.live_geometry();
        match &geometry {
            Some((
                doc_id,
                tools::SessionGeometry::Transform {
                    layer: Some(layer),
                    state,
                    ..
                },
            )) => {
                if editor.active().is_some_and(|d| d.id() == *doc_id) {
                    if let Some(delta) =
                        tools::transform::quad_affine(state.source_corners(), state.corners)
                    {
                        // Card 035: the commit lands P⁻¹·Δ·P pre-multiplied
                        // onto the layer's own transform — the preview must
                        // compose the same way, or a transformed layer's
                        // preview teleports and snaps on commit.
                        let doc = editor.active_mut().expect("checked above");
                        let own = doc
                            .document
                            .layers
                            .get(*layer)
                            .map(|l| l.transform)
                            .unwrap_or_default();
                        let parent = crate::interaction_geometry::document_transform_of(
                            &doc.document,
                            *layer,
                            0,
                        )
                        .ok()
                        .and_then(|total| {
                            doc.document
                                .layers
                                .get(*layer)
                                .map(|l| total * l.transform.inverse())
                        })
                        .unwrap_or(glam::Affine2::IDENTITY);
                        let composed = parent.inverse() * delta * parent * own;
                        doc.set_preview(*layer, composed);
                        return;
                    }
                }
                if let Some(doc) = editor.active_mut() {
                    doc.clear_preview();
                }
            }
            _ => {
                if let Some(doc) = editor.active_mut() {
                    doc.clear_preview();
                }
            }
        }
    }

    /// The live tool's overlay geometry, together with the document it
    /// belongs to (card 012). The shell publishes this into the canvas
    /// sessions every frame; `None` — no tool, no live session, or a session
    /// whose document was never claimed — clears the overlays. The same call
    /// publishes and un-publishes, so there is no second mechanism to forget.
    ///
    /// The document is the one the gesture claimed (`aimed_at`), remembered
    /// for the lifetime of the geometry: a free-transform session outlives its
    /// pointer claim (it commits on Enter, not on release), so the association
    /// outlives the claim too.
    pub fn live_geometry(&mut self) -> Option<(DocumentId, tools::SessionGeometry)> {
        let geometry = self
            .current
            .as_ref()
            .and_then(|(_, tool)| tool.live_geometry());
        match geometry {
            Some(geometry) => {
                let document = self.session_doc.or(self.aimed_at)?;
                Some((document, geometry))
            }
            None => {
                self.session_doc = None;
                None
            }
        }
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

    /// `true` when the live tool is holding a gesture Enter would confirm.
    pub fn has_pending_commit(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|(_, t)| t.has_pending_commit())
    }

    /// `true` while the live tool has a text run open, so the keyboard belongs
    /// to the canvas rather than to the shortcut table.
    pub fn is_text_editing(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|(_, t)| t.is_text_editing())
    }

    /// Run something on the live tool that is not a pointer sample — a commit,
    /// a keystroke — against a context over the active document.
    ///
    /// The same context [`ToolPointer::handle`] builds, minus the parts that
    /// only a pointer sample has (the view, the pressure). Factored out because
    /// these routes have to read the *same* selection, layer and colours a drag
    /// does: a commit that saw a different active layer from the gesture it is
    /// confirming would write to the wrong one.
    fn off_pointer(
        &mut self,
        editor: &mut Editor,
        action: impl FnOnce(&mut dyn Tool, &mut ToolContext<'_>) -> Result<(), tools::ToolError>,
    ) -> (Result<(), tools::ToolError>, Vec<Command>, Vec<ToolRequest>) {
        let Some((_, tool)) = &mut self.current else {
            return (Ok(()), Vec::new(), Vec::new());
        };
        let foreground = editor.foreground();
        let background = editor.background();
        let ramp = tools::gradient::GradientRamp::from_ui_gradient(editor.gradient_ramp())
            .unwrap_or_else(|_| tools::gradient::GradientRamp::black_to_white());
        let quick_mask = editor.quick_mask();
        let quick_mask_layer = editor.quick_mask_layer();
        // Card 055: the off-pointer route resolves the target through the
        // SAME resolver the pointer gestures use (one answer everywhere).
        let edit_target_is_mask = editor.edit_target_is_mask();
        let Some(doc) = editor.active_mut() else {
            return (Ok(()), Vec::new(), Vec::new());
        };
        let canvas = doc.canvas_rect();
        let active_layer = doc.document.active_layer();
        let active_mask = active_layer
            .and_then(|id| doc.document.layers.get(id))
            .and_then(|layer| layer.mask_id());
        let selection = doc.document.selection.clone();
        let layer_stack = doc.document.layers.iter_depth_first();
        let shape_paths: Vec<(layer_model::LayerId, layer_model::ShapeLayer)> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| {
                let layer = doc.document.layers.get(id)?;
                let layer_model::LayerKind::Shape(shape) = &layer.kind else {
                    return None;
                };
                Some((id, shape.clone()))
            })
            .collect();
        // Card 042: snap candidates for the moving geometry — canvas
        // edges/center + every layer NOT in the selected set (nor its
        // descendants), in document space. The threshold rides along in
        // doc pixels.
        let selected = doc.document.layer_selection();
        let (snap_candidates, snap_threshold_doc) =
            snap_candidates_for(&doc.document, &doc.tiles, doc.camera.zoom, &selected);
        // Card 042: the commit route gets the same tight-ink answer as the
        // gesture route — a future snap consumer here must not see None.
        let active_layer_ink_bounds =
            active_layer.and_then(|layer| tight_document_bounds(&doc.document, &doc.tiles, layer));
        // Card 043: the link chain — every layer carrying the flag.
        let linked_layers: Vec<layer_model::LayerId> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| doc.document.layers.get(*id).is_some_and(|l| l.linked))
            .collect();

        // Card 034: the bounds query borrows the tile source immutably, so
        // it runs BEFORE the mutable tool access is built. document_bounds
        // maps through the layer transform — document space, like the tool.
        let active_layer_content_bounds = active_layer.and_then(|layer| {
            compositor::bounds::document_bounds(
                &doc.document,
                &doc.tiles,
                layer,
                0,
                compositor::CompositeOptions::default(),
            )
            .ok()
            .flatten()
        });
        let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles);
        let mut ctx = ToolContext::new(&mut access, canvas);
        ctx.shape_paths = shape_paths;
        // Card 055: the ONE resolver, unconditionally — quick mask reroutes
        // to the scratch layer's mask inside it, the sticky edit target
        // decides otherwise; pointer gestures and off-pointer operations
        // cannot disagree.
        let target = resolve_edit_target(
            &doc.document,
            quick_mask,
            quick_mask_layer,
            edit_target_is_mask,
            active_layer,
            active_mask,
        );
        ctx.active_layer = target.layer;
        ctx.active_mask = target.mask;
        ctx.paint_target = target.paint;
        // Card 044: the effective edit target keeps editable geometry?
        // Computed AFTER the quick-mask reroute so the flag describes the
        // layer that will actually be edited (card 040's lesson).
        let active_layer_parametric = ctx.active_layer.is_some_and(|id| {
            doc.document
                .layers
                .get(id)
                .is_some_and(|l| l.kind.parametric())
        });
        // Card 034: the active layer's real content extent, for tools that
        // start geometry over the ink rather than the canvas.
        ctx.active_layer_content_bounds = active_layer_content_bounds;
        // Card 040: paint tools write the ACTIVE layer's pixels in LAYER
        // space — hand them the document→layer mapping (paintable kinds
        // only; text/shape kinds are refused at commit with NotPaintable
        // rather than storing invisible tiles).
        ctx.sample_to_layer =
            sample_to_paint_target_of(&doc.document, ctx.active_layer, ctx.paint_target);
        ctx.paint_space_canvas = Some(paint_space_canvas_of(
            &doc.document,
            ctx.active_layer,
            ctx.paint_target,
            canvas,
        ));
        // Card 042: the snap candidates + threshold computed pre-access.
        ctx.snap_candidates = snap_candidates;
        ctx.snap_threshold_doc = snap_threshold_doc;
        ctx.active_layer_ink_bounds = active_layer_ink_bounds;
        ctx.linked_layers = linked_layers;
        ctx.active_layer_parametric = active_layer_parametric;
        // Card 036: the panel's selection set, ancestry and locks.
        ctx.selected_layers = doc.document.layer_selection();
        ctx.layer_parents = doc
            .document
            .layers
            .iter_depth_first()
            .iter()
            .map(|id| (*id, doc.document.layers.parent_of(*id)))
            .collect();
        ctx.layer_locks = doc
            .document
            .layers
            .iter_depth_first()
            .iter()
            .map(|id| {
                (
                    *id,
                    doc.document
                        .layers
                        .get(*id)
                        .is_some_and(|l| l.locked.blocks_transform()),
                )
            })
            .collect();
        ctx.layer_parent_transforms = doc
            .document
            .layers
            .iter_depth_first()
            .iter()
            .filter_map(|id| {
                let total =
                    crate::interaction_geometry::document_transform_of(&doc.document, *id, 0)
                        .ok()?;
                let own = doc.document.layers.get(*id)?.transform;
                Some((*id, total * own.inverse()))
            })
            .collect();
        // Card 035: the parent chain (see handle's fill).
        ctx.active_layer_parent_transform = ctx.active_layer.and_then(|layer| {
            let total =
                crate::interaction_geometry::document_transform_of(&doc.document, layer, 0).ok()?;
            let own = doc.document.layers.get(layer)?.transform;
            Some(total * own.inverse())
        });
        ctx.selection = selection;
        ctx.foreground = foreground;
        ctx.background = background;
        ctx.ramp = ramp;
        ctx.layer_stack = layer_stack;
        let result = action(tool.as_mut(), &mut ctx);
        let drained = (result, ctx.drain(), ctx.drain_requests());
        drop(ctx);
        drained
    }

    /// Feed one keystroke to a tool that is editing text.
    ///
    /// This is the second half of the Type tool: the click makes the layer and
    /// this is what puts characters in it. Reports whether the keystroke was
    /// consumed — `false` means no run is open and the key belongs to the
    /// keymap, which is what stops Space from typing a space when nobody is
    /// typing.
    /// Card 028: the live session's selected text, for the shell to hand to
    /// the OS clipboard (copy/cut). Read-only — no outbox traffic.
    pub fn text_selection_text(&mut self, editor: &Editor) -> Option<String> {
        let (_, tool) = self.current.as_mut()?;
        editor.active()?;
        tool.text_selection_text()
    }

    /// Card 030: the live text session's overlay geometry, in DOCUMENT space
    /// — the caret bar and one 4-edge loop per shaped selection rectangle.
    /// The document's text layer is the draft (card 025 rides edits through
    /// `apply_text_draft`), so shaping it IS shaping what the canvas shows:
    /// the caret agrees with rendering after transform and zoom by
    /// construction. `None` without a live session.
    pub fn text_overlay_geometry(&mut self, editor: &Editor) -> Vec<TextOverlaySegment> {
        let Some((_, tool)) = self.current.as_mut() else {
            return Vec::new();
        };
        let Some(layer) = tool.text_session_layer() else {
            return Vec::new();
        };
        let Some((caret, anchor)) = tool.text_caret_anchor() else {
            return Vec::new();
        };
        let Some(doc) = editor.active() else {
            return Vec::new();
        };
        let Some(doc_layer) = doc.document.layers.get(layer) else {
            return Vec::new();
        };
        let layer_model::LayerKind::Text(text) = &doc_layer.kind else {
            return Vec::new();
        };
        let run = text_engine::TextRun::from(text);
        let mut out = Vec::new();
        let caret_rect = compositor::text_caret_rect(&run, caret.min(text.text.len()));
        push_rect_edges(&mut out, caret_rect, TextOverlaySegment::CARET);
        let (lo, hi) = if anchor <= caret {
            (anchor, caret)
        } else {
            (caret, anchor)
        };
        for rect in compositor::text_selection_rects(&run, lo, hi) {
            push_rect_edges(&mut out, rect, TextOverlaySegment::SELECTION);
        }
        // Card 032: a boxed paragraph draws its frame — the resize handles'
        // home. Auto height uses the shaped run's content height.
        if let Frame::Box { width, height } = text.frame {
            let shaped = compositor::text_content_height(&run);
            let h = height.unwrap_or(shaped.max(1.0));
            let box_rect = text_engine::Rect {
                x: 0.0,
                y: 0.0,
                width,
                height: h,
            };
            push_rect_edges(&mut out, box_rect, TextOverlaySegment::BOX_FRAME);
        }
        // Layer space → document space, so the shell maps one transform for
        // every endpoint (the same camera a click is routed against).
        for segment in &mut out {
            if let Ok(a) =
                crate::interaction_geometry::layer_to_document(&doc.document, layer, 0, segment.a)
            {
                segment.a = a;
            }
            if let Ok(b) =
                crate::interaction_geometry::layer_to_document(&doc.document, layer, 0, segment.b)
            {
                segment.b = b;
            }
        }
        out
    }

    /// Card 029: whether the live session has an IME composition under way —
    /// the shell suppresses plain-character insertion while it is.
    pub fn text_composing(&mut self, editor: &Editor) -> bool {
        let Some((_, tool)) = self.current.as_mut() else {
            return false;
        };
        if editor.active().is_none() {
            return false;
        }
        tool.text_composing()
    }

    pub fn text_edit(&mut self, editor: &mut Editor, edit: tools::TextEdit<'_>) -> CommitOutcome {
        let mut out = CommitOutcome::default();
        if !self.is_text_editing() || editor.active().is_none() {
            return out;
        }
        // Card 026: the run lives in its own document. A tab switch must not
        // let typing, confirm or cancel land on the wrong document — the
        // keystroke is consumed (the run still owns the keyboard) and the
        // status bar says where to go to finish it.
        let session_layer = self
            .current
            .as_ref()
            .and_then(|(_, tool)| tool.text_session_layer());
        if let Some(layer) = session_layer {
            let in_active = editor
                .active()
                .map(|d| d.document.layers.get(layer).is_some())
                .unwrap_or(false);
            if !in_active {
                editor.set_status(
                    "the text run belongs to another document — switch back to it to finish it",
                );
                out.had_pending = true;
                return out;
            }
        }
        out.had_pending = true;
        let (result, commands, requests) =
            self.off_pointer(editor, |tool, ctx| tool.text_edit(ctx, edit));
        if let Err(e) = result {
            out.failed = Some(e.to_string());
            editor.set_status(e.to_string());
        }
        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        for command in commands {
            editor.apply_command(command);
        }
        let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        out.steps = after.saturating_sub(before);
        // Card 025: the live draft rides the request outbox and lands on the
        // document **outside history** — the canvas renders every keystroke,
        // undo stays clean until the session confirms.
        for request in requests {
            out.steps += Self::perform_text_request(editor, request);
        }
        out
    }

    /// Card 026: the top-most unlocked text layer whose transformed ink contains
    /// `doc_pt`, with the caret byte index the shaped hit test chose.
    ///
    /// The click is converted document → layer-local through the layer's composed
    /// document transform (`interaction_geometry`, the convention T009 pinned),
    /// and the hit test runs on the layer's own run. Clicks that miss every text
    /// layer return `None` — the Type tool then creates a new layer, unchanged.
    fn text_hit_under(
        doc: &editor_core::Document,
        doc_pt: glam::Vec2,
    ) -> Option<tools::TextHitCaret> {
        for id in doc.layers.iter_depth_first() {
            let Some(layer) = doc.layers.get(id) else {
                continue;
            };
            if layer.locked.all || !layer.visible {
                continue;
            }
            let layer_model::LayerKind::Text(text) = &layer.kind else {
                continue;
            };
            let Ok(local) = crate::interaction_geometry::document_to_layer(doc, id, 0, doc_pt)
            else {
                continue;
            };
            if let Some(caret) =
                compositor::text_hit_index(&text_engine::TextRun::from(text), local.x, local.y)
            {
                return Some(tools::TextHitCaret {
                    layer: id,
                    original: text.clone(),
                    caret,
                });
            }
        }
        None
    }

    /// Card 026: double-clicking a text row enters that layer — the shell
    /// resolves the payload, caret (end of the run) and origin from the
    /// ACTIVE document, then hands them to the Type tool. No-op (and no
    /// session) when the layer is gone, is not text, or is blanket-locked.
    pub fn enter_text_session(&mut self, editor: &mut Editor, layer: layer_model::LayerId) {
        // Card 025/026: reconcile any live text session first — its typed
        // draft lands as one history entry instead of being silently dropped
        // by the tool replacement below.
        if self.is_text_editing() {
            self.text_edit(editor, tools::TextEdit::Confirm);
        }
        // Make Type the editor's effective tool BEFORE the session begins:
        // the next canvas click must route to the session's tool, not
        // silently replace it (the round-1 strand hole).
        editor.set_tool(ToolId::Type);
        let Some(doc) = editor.active() else {
            return;
        };
        let Some(l) = doc.document.layers.get(layer) else {
            return;
        };
        if l.locked.all {
            return;
        }
        let layer_model::LayerKind::Text(original) = &l.kind else {
            return;
        };
        let original = original.clone();
        let origin = glam::Vec2::new(l.transform.translation.x, l.transform.translation.y);
        let caret = original.text.len();
        self.tool(ToolId::Type)
            .enter_text_session(layer, original, caret, origin);
        // Card 031: the visible affordance for the session's lifecycle (the
        // docks see only the Document, so the Confirm/Cancel buttons they
        // would host need session-state plumbing first) — set only when the
        // session actually opened, after every guard above.
        if self.is_text_editing() {
            editor.set_status(
                "editing text — Enter for a new line, Ctrl+Enter to finish, Escape to cancel",
            );
        }
    }

    /// Perform one text-session request (card 025). Drafts land on the
    /// document history-free (0 steps); a confirm restores the original
    /// payload first and only then applies the `SetLayerKind`, so the
    /// command's inverse is the original and one undo takes the whole
    /// session back. Reports the history steps the request cost.
    /// Card 036/038: conjugate the document-space delta through EACH
    /// participant's own parent chain and commit the whole set as one
    /// transaction — one undo entry.
    fn perform_transform_layers(
        editor: &mut Editor,
        layers: &[layer_model::LayerId],
        delta: [f32; 6],
    ) -> usize {
        let Some(doc) = editor.active_mut() else {
            editor.set_status("no document to transform");
            return 0;
        };
        let mut commands = Vec::new();
        for layer in layers {
            let total =
                crate::interaction_geometry::document_transform_of(&doc.document, *layer, 0).ok();
            let own = doc.document.layers.get(*layer).map(|l| l.transform);
            if let (Some(total), Some(own)) = (total, own) {
                let parent = total * own.inverse();
                let delta_parent = glam::Affine2::from_cols_array(&delta);
                // TransformLayer pre-multiplies onto the layer's own
                // transform — the conjugated delta is the whole matrix.
                let matrix = (parent.inverse() * delta_parent * parent).to_cols_array();
                commands.push(Command::TransformLayer {
                    layer_id: *layer,
                    matrix,
                });
            }
        }
        let count = commands.len();
        if count == 0 {
            return 0;
        }
        // All-or-nothing: a refusal (a position lock that slipped through,
        // say) leaves nothing applied AND reports no history step.
        match doc.apply(Command::Transaction {
            label: format!("transform {count} layers"),
            commands,
        }) {
            Ok(_) => {
                editor.set_status(format!("moved {count} layers"));
                1
            }
            Err(e) => {
                editor.set_status(e.to_string());
                0
            }
        }
    }

    fn perform_text_request(editor: &mut Editor, request: tools::ToolRequest) -> usize {
        match request {
            tools::ToolRequest::TextDraft { layer, kind } => {
                if let Err(e) = editor.active_mut().unwrap().apply_text_draft(layer, *kind) {
                    editor.set_status(e.to_string());
                }
                0
            }
            tools::ToolRequest::TextConfirm {
                layer,
                original,
                draft,
            } => Self::perform_text_confirm(editor, layer, original, draft),
            _ => 0,
        }
    }

    /// The confirm reconciliation: restore (history-free), then commit (one
    /// entry whose inverse is the restored original). If the restore fails
    /// (e.g. the layer was locked mid-session) the DRAFT is re-applied so it
    /// stays visible and nothing strands stale — the status bar says why —
    /// and the reported steps are the history depth that actually moved.
    fn perform_text_confirm(
        editor: &mut Editor,
        layer: layer_model::LayerId,
        original: Box<layer_model::LayerKind>,
        draft: Box<layer_model::LayerKind>,
    ) -> usize {
        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        if let Err(e) = editor
            .active_mut()
            .unwrap()
            .apply_text_draft(layer, *original.clone())
        {
            // The restore could not run: keep the draft on the layer (it is
            // already there and visible) instead of attempting a commit whose
            // inverse would be the draft itself.
            editor.set_status(format!(
                "{e} — the text run is not committed; unlock the layer to commit it"
            ));
            if let Err(e) = editor.active_mut().unwrap().apply_text_draft(layer, *draft) {
                editor.set_status(e.to_string());
            }
            return editor
                .active()
                .map(|d| d.history_depth())
                .unwrap_or(0)
                .saturating_sub(before);
        }
        editor.apply_command(editor_core::Command::SetLayerKind {
            layer_id: layer,
            kind: draft,
        });
        editor
            .active()
            .map(|d| d.history_depth())
            .unwrap_or(0)
            .saturating_sub(before)
    }

    /// Confirm the gesture the live tool is holding: Enter, or the options
    /// bar's Apply.
    ///
    /// Three tools end a gesture here rather than at pointer-up — Crop, Slice
    /// and Free Transform — and until this existed none of them could finish at
    /// all: they publish only from their own `commit`, `Box<dyn Tool>` had no
    /// way to call it, and a crop drag therefore produced a rectangle on the
    /// screen and never a pixel, never a command and never a history step.
    ///
    /// The two outboxes are drained exactly as a pointer sample drains them:
    /// commands go through [`Editor::apply_command`], so Free Transform's
    /// resample is one Ctrl+Z. A [`ToolRequest`] is *not* a command, so this is
    /// where each is performed — a crop becomes the transaction
    /// [`crop_command`] builds and lands on the same history, and a slice set
    /// is reported (see [`CommitOutcome::slices`]; nothing in this build
    /// exports one yet, so it reaches the status bar and the caller and no
    /// further).
    pub fn commit(&mut self, editor: &mut Editor) -> CommitOutcome {
        let mut out = CommitOutcome::default();
        if !self.has_pending_commit() {
            return out;
        }
        if editor.active().is_none() {
            return out;
        }
        out.had_pending = true;
        let (result, commands, requests) = self.off_pointer(editor, |tool, ctx| tool.commit(ctx));

        if let Err(e) = result {
            out.failed = Some(e.to_string());
            editor.set_status(e.to_string());
        }

        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        for command in commands {
            editor.apply_command(command);
        }

        for request in requests {
            match request {
                ToolRequest::TextDraft { layer, kind } => {
                    // A draft arriving on the commit drain is performed the
                    // same history-free way (card 025); confirm's own
                    // reconciliation rides its TextConfirm request.
                    if let Err(e) = editor.active_mut().unwrap().apply_text_draft(layer, *kind) {
                        editor.set_status(e.to_string());
                    }
                }
                ToolRequest::TextConfirm {
                    layer,
                    original,
                    draft,
                } => {
                    out.steps += Self::perform_text_confirm(editor, layer, original, draft);
                }
                ToolRequest::TransformLayers { layers, delta } => {
                    // Card 035/036: one shared performer — the conjugated
                    // delta is the whole matrix (TransformLayer
                    // pre-multiplies onto the layer's own transform).
                    out.steps += Self::perform_transform_layers(editor, &layers, delta);
                }
                ToolRequest::Crop(req) => {
                    let command = editor
                        .active()
                        .and_then(|doc| crop_command(&doc.document, &req));
                    match command {
                        Some(command) => {
                            editor.apply_command(command);
                            out.cropped_to = Some(req.rect);
                            // Both halves of the request this build cannot
                            // perform are said out loud rather than left to
                            // look like they happened. See `crop_command`.
                            if req.straighten != 0.0 && req.straighten.is_finite() {
                                editor.set_status("Cropped; the straighten angle was not applied");
                            } else if req.delete_cropped {
                                editor
                                    .set_status("Cropped; the pixels outside the canvas were kept");
                            } else {
                                editor.set_status(format!(
                                    "Cropped to {} x {}",
                                    req.rect.width, req.rect.height
                                ));
                            }
                        }
                        None => {
                            let reason = "That crop region is empty".to_string();
                            out.failed = Some(reason.clone());
                            editor.set_status(reason);
                        }
                    }
                }
                ToolRequest::Slices(slices) => {
                    editor.set_status(format!(
                        "{} slice(s) defined; this build cannot export them yet",
                        slices.len()
                    ));
                    out.slices = slices;
                }
                ToolRequest::SelectLayer(id) => {
                    // Path Select clicked a shape layer's path: the layer
                    // becomes the document's selection (a field write, like
                    // every other selection change).
                    editor.set_layer_selection(vec![id], Some(id));
                }
            }
        }

        let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        out.steps = after.saturating_sub(before);
        out
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
        settings: &[(String, tools::ToolSetting)],
    ) -> PointerOutcome {
        let mut out = PointerOutcome::default();
        if over_panel && !self.router.is_gesture_active() {
            out.refused = Some(Refusal::OverPanel);
            return out;
        }
        let quick_mask = editor.quick_mask();
        let quick_mask_layer = editor.quick_mask_layer();
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
        // Card 055: the validated edit target, read once per event before the
        // document borrow. The gesture pins it (see the ctx build below).
        let edit_target_is_mask = editor.edit_target_is_mask();

        let (dispatch, viewport) = {
            let doc = editor.active_mut().expect("checked immediately above");
            let viewport = canvas_viewport(doc.camera.viewport_size);
            let mut camera = canvas_camera_of(&doc.camera);
            let dispatch = self.router.handle(input, &mut camera, &viewport, effective);
            out.view_changed = write_camera_back(&camera, &mut doc.camera);
            (dispatch, viewport)
        };
        // Card 012: a published session rides the claim's document, and stays
        // with it for the geometry's lifetime (which can outlive the claim).
        if self.router.is_gesture_active() && self.session_doc.is_none() {
            self.session_doc = Some(active_id);
        }
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
        // The pin is snapshotted before the tool borrow and written back
        // after the sample: the gesture pin belongs to the pointer, not to
        // one sample of it.
        let mut pinned_paint_target = self.pinned_paint_target;
        let brush = (routed.phase == PointerPhase::Down).then(|| editor.brush_for(id));
        let tool = self.tool(id);
        if let Some(brush) = brush {
            tool.set_brush(brush);
            // The typed options ride the same seed (card 010): what the
            // options bar holds is what the tool is, applied at the press so a
            // gesture's settings are pinned for its lifetime. A refused value
            // is surfaced, not swallowed — an options-bar control that
            // silently did nothing is the defect this seam exists to prevent.
            for (key, setting) in settings {
                // Two keys never reach `set_setting` here: the
                // brush-shared ones already travelled through `set_brush`
                // (the chrome's brush_from_options reads them out of the
                // same options map), and the UI-supplied keys (the paint
                // blend mode) name no registry option a tool could answer.
                // Both would burn the refusal channel on every
                // pointer-down.
                if crate::chrome::BRUSH_KEYS.contains(&key.as_str()) || key.starts_with("ui.") {
                    continue;
                }
                if let Err(e) = tool.set_setting(key, *setting) {
                    out.failed = Some(e.to_string());
                }
            }
        }

        let (result, commands, selection_edits, requests, picked, canvas_rect) = {
            let doc = editor.active_mut().expect("checked above");
            let canvas = doc.canvas_rect();
            let active_layer = doc.document.active_layer();
            let active_mask = active_layer
                .and_then(|id| doc.document.layers.get(id))
                .and_then(|layer| layer.mask_id());
            // Card 055: the gesture's edit target - resolved through THE one
            // shared resolver at the Down that begins the gesture, pinned for
            // every later sample of it (the Up's commit included). The
            // precomputed bounds below describe the layer that will actually
            // be edited.
            let target = if routed.phase == PointerPhase::Down {
                let t = resolve_edit_target(
                    &doc.document,
                    quick_mask,
                    quick_mask_layer,
                    edit_target_is_mask,
                    active_layer,
                    active_mask,
                );
                pinned_paint_target = Some(t);
                t
            } else {
                pinned_paint_target.unwrap_or_else(|| {
                    resolve_edit_target(
                        &doc.document,
                        quick_mask,
                        quick_mask_layer,
                        edit_target_is_mask,
                        active_layer,
                        active_mask,
                    )
                })
            };
            let selection = doc.document.selection.clone();
            // Top-most first, which is the order `LayerTree` keeps its roots in
            // — what the move tool's auto-select walks.
            let layer_stack = doc.document.layers.iter_depth_first();
            let shape_paths: Vec<(layer_model::LayerId, layer_model::ShapeLayer)> = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter_map(|id| {
                    let layer = doc.document.layers.get(id)?;
                    let layer_model::LayerKind::Shape(shape) = &layer.kind else {
                        return None;
                    };
                    Some((id, shape.clone()))
                })
                .collect();
            let view = canvas_camera_of(&doc.camera).to_view_state(&viewport);

            // Card 034: the effective edit target's bounds in DOCUMENT space
            // (document_bounds maps through the layer transform — a text
            // layer placed by click surrounds its visible text, not its
            // layer-local origin). The quick-mask reroute decides the target
            // first, so the bounds describe the layer that will be edited.
            let effective_layer = target.layer;
            let active_layer_content_bounds = effective_layer.and_then(|layer| {
                compositor::bounds::document_bounds(
                    &doc.document,
                    &doc.tiles,
                    layer,
                    0,
                    compositor::CompositeOptions::default(),
                )
                .ok()
                .flatten()
            });

            // Card 037: the bounded visible-content pick under this sample —
            // bounds queries per layer, alpha only for bounds candidates.
            // Computed before the mutable tile access is built.
            let content_hit = crate::hit_testing::visible_content_at(
                &doc.document,
                &doc.document.pixels,
                &doc.tiles,
                routed.event.pos,
                0.5,
            )
            .map(|hit| hit.layer);
            // Card 042: snap candidates (tight ink bounds, selection and
            // its descendants excluded) + the dragged layer's tight ink.
            let selected = doc.document.layer_selection();
            let (snap_candidates, snap_threshold_doc) =
                snap_candidates_for(&doc.document, &doc.tiles, doc.camera.zoom, &selected);
            let active_layer_ink_bounds = effective_layer
                .and_then(|layer| tight_document_bounds(&doc.document, &doc.tiles, layer));
            // Card 043: the link chain — every layer carrying the flag.
            let linked_layers: Vec<layer_model::LayerId> = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter(|id| doc.document.layers.get(*id).is_some_and(|l| l.linked))
                .collect();
            // Card 044: the active layer keeps editable geometry?
            let active_layer_parametric = effective_layer.is_some_and(|id| {
                doc.document
                    .layers
                    .get(id)
                    .is_some_and(|l| l.kind.parametric())
            });
            let mut access = DocumentTiles::new(&doc.document.pixels, &mut doc.tiles);
            let mut ctx = ToolContext::new(&mut access, canvas);
            ctx.shape_paths = shape_paths;
            // Card 055: the pinned target routes everything - the layer the
            // edits land on, the mask they mask with, and the surface they
            // paint. Quick mask is a temporary mode above the sticky target:
            // it reroutes while live and exits to the sticky target
            // untouched.
            ctx.active_layer = target.layer;
            ctx.active_mask = target.mask;
            ctx.paint_target = target.paint;
            ctx.active_layer_content_bounds = active_layer_content_bounds;
            ctx.sample_to_layer =
                sample_to_paint_target_of(&doc.document, ctx.active_layer, ctx.paint_target);
            ctx.paint_space_canvas = Some(paint_space_canvas_of(
                &doc.document,
                ctx.active_layer,
                ctx.paint_target,
                canvas,
            ));
            ctx.content_pick = Some(content_hit);
            ctx.snap_candidates = snap_candidates;
            ctx.snap_threshold_doc = snap_threshold_doc;
            ctx.active_layer_ink_bounds = active_layer_ink_bounds;
            ctx.linked_layers = linked_layers;
            ctx.active_layer_parametric = active_layer_parametric;

            // Card 036: the panel's selection set, ancestry and locks — the
            // transform tool's multi-layer session reads all three.
            ctx.selected_layers = doc.document.layer_selection();
            ctx.layer_parents = doc
                .document
                .layers
                .iter_depth_first()
                .iter()
                .map(|id| (*id, doc.document.layers.parent_of(*id)))
                .collect();
            ctx.layer_locks = doc
                .document
                .layers
                .iter_depth_first()
                .iter()
                .map(|id| {
                    (
                        *id,
                        doc.document
                            .layers
                            .get(*id)
                            .is_some_and(|l| l.locked.blocks_transform()),
                    )
                })
                .collect();
            ctx.layer_parent_transforms = doc
                .document
                .layers
                .iter_depth_first()
                .iter()
                .filter_map(|id| {
                    let total =
                        crate::interaction_geometry::document_transform_of(&doc.document, *id, 0)
                            .ok()?;
                    let own = doc.document.layers.get(*id)?.transform;
                    Some((*id, total * own.inverse()))
                })
                .collect();
            // Card 035: the parent chain for the transform delta's
            // conjugation (document_transform_of includes the layer's own
            // transform; the parent is it minus the own).
            ctx.active_layer_parent_transform = ctx.active_layer.and_then(|layer| {
                let total =
                    crate::interaction_geometry::document_transform_of(&doc.document, layer, 0)
                        .ok()?;
                let own = doc.document.layers.get(layer)?.transform;
                Some(total * own.inverse())
            });
            ctx.selection = selection;
            ctx.foreground = foreground;
            ctx.background = background;
            ctx.view = view;
            ctx.layer_stack = layer_stack;

            // Card 026: a fresh Type press-release may ENTER an existing text
            // layer instead of creating one. The shell resolves the hit — it
            // owns the shaping stack: the top-most unlocked text layer whose
            // transformed ink contains the click, and the caret the shaped
            // hit test chose.
            if routed.phase == PointerPhase::Up
                && tool.id() == ToolId::Type
                && !tool.is_text_editing()
            {
                ctx.text_hit = Self::text_hit_under(&doc.document, routed.event.pos);
            }
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
        // (the snapshot above is written back right after the block)
        self.pinned_paint_target = pinned_paint_target;

        if let Err(e) = result {
            out.failed = Some(e.to_string());
            editor.set_status(e.to_string());
        }

        // The step count is measured across BOTH apply paths below — the
        // selection fold and the pixel commands — so a selection gesture
        // reports the history step it landed (card 056).
        let before = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        if !selection_edits.is_empty() {
            // Card 056: selection changes ride HISTORY as Command::SetSelection
            // (whose inverse is the previous selection), with one gesture's
            // edits coalesced into ONE undoable step — undo one marquee drag
            // and redo it without any image pixel moving. The fold runs
            // sequentially so a gesture's second edit composes onto its first
            // (add on top of replace, exactly as the direct assignment did).
            // Today every selection tool emits at most one edit per gesture
            // (all at Up), so the transaction branch is future-proofing —
            // the coalescing contract is what a multi-edit tool would ride.
            let doc = editor.active_mut().expect("checked above");
            let mut current = doc.document.selection.clone();
            let mut selection_commands: Vec<Command> = Vec::new();
            let mut failure: Option<String> = None;
            for edit in &selection_edits {
                match edit.apply(canvas_rect, &current) {
                    Ok(next) if next != current => {
                        selection_commands.push(Command::SetSelection {
                            selection: next.clone(),
                        });
                        current = next;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        failure = Some(e.to_string());
                        break;
                    }
                }
            }
            match failure {
                Some(reason) => {
                    out.failed = Some(reason.clone());
                    editor.set_status(reason);
                }
                None if !selection_commands.is_empty() => {
                    let command = if selection_commands.len() == 1 {
                        selection_commands.into_iter().next().expect("non-empty")
                    } else {
                        Command::Transaction {
                            label: "Select".to_string(),
                            commands: selection_commands,
                        }
                    };
                    editor.apply_command(command);
                    // The selection is part of the saved document; the
                    // command route marks the dirty state through the editor.
                    out.selection_changed = true;
                }
                None => {}
            }
        }

        // Through the editor, so a gesture is undone by exactly the Ctrl+Z that
        // undoes a panel edit. The count is what history really took, not what
        // the tool offered: a command History refuses is not a step.
        for command in commands {
            editor.apply_command(command);
        }
        let after = editor.active().map(|d| d.history_depth()).unwrap_or(0);
        out.steps = after.saturating_sub(before);

        if let Some(rgba) = picked {
            editor.set_foreground(rgba);
            out.picked = Some(rgba);
        }

        if !requests.is_empty() {
            // Crop and slice publish only from `Tool::commit`, which is
            // [`ToolPointer::commit`]'s path, not this one — a request that
            // arrived here and was dropped in silence would be a gesture that
            // looked like it worked, so it is said rather than swallowed.
            // Path Select is the exception: its whole job is a click, so its
            // layer selection is performed right here.
            let mut deferred = 0;
            for request in requests {
                match request {
                    ToolRequest::SelectLayer(id) => {
                        editor.set_layer_selection(vec![id], Some(id));
                    }
                    ToolRequest::TextDraft { layer, kind } => {
                        // The live text draft renders from the pointer route
                        // too (IME updates can arrive between keystrokes) —
                        // but only into the document the run lives in.
                        let in_active = editor
                            .active()
                            .map(|d| d.document.layers.get(layer).is_some())
                            .unwrap_or(false);
                        if !in_active {
                            continue;
                        }
                        if let Err(e) = editor.active_mut().unwrap().apply_text_draft(layer, *kind)
                        {
                            editor.set_status(e.to_string());
                        }
                    }
                    ToolRequest::TextConfirm {
                        layer,
                        original,
                        draft,
                    } => {
                        out.steps += Self::perform_text_confirm(editor, layer, original, draft);
                    }
                    ToolRequest::TransformLayers { layers, delta } => {
                        // Card 038: the Move tool commits its set move at
                        // pointer-up — perform it now, exactly as the
                        // commit drain would, and COUNT the history step so
                        // changed_document/repaint see it.
                        out.steps += Self::perform_transform_layers(editor, &layers, delta);
                    }
                    ToolRequest::Crop(_) | ToolRequest::Slices(_) => deferred += 1,
                }
            }
            if deferred > 0 {
                tracing::warn!(
                    "{} tool request(s) arrived from a pointer sample rather than a commit",
                    deferred
                );
                editor.set_status("Press Enter to apply the crop or the slices");
            }
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
            out.push(pointer.handle(editor, sample(phase, screen(*x, *y)), false, &[]));
        }
        let (x, y) = *points.last().unwrap();
        out.push(pointer.handle(editor, sample(PointerPhase::Up, screen(x, y)), false, &[]));
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

    /// Card 010: the typed settings channel. The Auto-Select boolean rides
    /// `ToolPointer::handle`'s settings into the running Move tool, so a press
    /// claims the layer whose ink is under the pointer — not the active layer
    /// the panel has selected.
    #[test]
    fn auto_select_forwarded_through_the_pointer_route_picks_the_layer_under_the_pointer() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Move);

        // A second layer ON TOP with dark ink in one corner only. Created
        // through the real command route; the panel selection stays on the
        // opened canvas layer beneath it.
        let corner = TileCoord::new(0, 0, 0);
        let ink_rect = 4u32..12;
        let mut bytes = Vec::with_capacity(256 * 256 * 4);
        for y in 0..256u32 {
            for x in 0..256u32 {
                bytes.extend_from_slice(&if ink_rect.contains(&x) && ink_rect.contains(&y) {
                    [10, 10, 10, 255]
                } else {
                    [0, 0, 0, 0]
                });
            }
        }
        let overlay_id = {
            let doc = editor.active_mut().unwrap();
            let layer = layer_model::Layer::raster("Corner ink");
            let id = layer.id;
            doc.apply(Command::create_layer(layer)).unwrap();
            let hash = doc.tiles.insert_bytes(bytes);
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(id),
                    vec![editor_core::TileEdit::set(corner, hash)],
                )
                .unwrap(),
            )
            .unwrap();
            id
        };
        // The active layer is the opened canvas beneath, as the panel would
        // have it — NOT the corner-ink layer the pointer will be over.
        let canvas_layer = {
            let doc = editor.active_mut().unwrap();
            let id = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .find(|id| *id != overlay_id)
                .unwrap();
            doc.document.set_layer_selection(vec![id]).unwrap();
            doc.document.set_active_layer(Some(id)).unwrap();
            id
        };
        assert_ne!(canvas_layer, overlay_id);

        // Down over the corner ink with Auto-Select forwarded as a typed
        // setting: the press claims the corner layer.
        let mut pointer = ToolPointer::new();
        let settings = [("auto_select".to_string(), tools::ToolSetting::Bool(true))];
        let before = composite(&mut editor);
        let before_at = |x: u32, y: u32| {
            let i = ((y * 64 + x) * 4) as usize;
            [before[i], before[i + 1], before[i + 2], before[i + 3]]
        };
        // The corner ink is where the pointer will press.
        assert_eq!(before_at(8, 8), [10, 10, 10, 255]);
        let down = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(8.0, 8.0)),
            false,
            &settings,
        );
        assert!(down.reached_tool, "{down:?}");
        assert!(
            down.failed.is_none(),
            "the setting is accepted: {:?}",
            down.failed
        );
        for at in [(10.0, 10.0), (14.0, 14.0)] {
            pointer.handle(
                &mut editor,
                sample(PointerPhase::Move, screen(at.0, at.1)),
                false,
                &settings,
            );
        }
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(18.0, 18.0)),
            false,
            &settings,
        );
        assert!(up.failed.is_none(), "{up:?}");

        // The corner layer moved; the canvas beneath did not. If the press had
        // claimed the active layer instead, the whole white canvas would have
        // shifted and every pixel would differ.
        let after = composite(&mut editor);
        let alpha_at = |x: u32, y: u32| {
            let i = ((y * 64 + x) * 4) as usize;
            [after[i], after[i + 1], after[i + 2], after[i + 3]]
        };
        assert_ne!(
            alpha_at(8, 8),
            before_at(8, 8),
            "the corner ink moved away from its old spot"
        );
        assert_eq!(
            alpha_at(40, 40),
            before_at(40, 40),
            "the canvas layer did not move: auto-select claimed the ink under the pointer"
        );
    }

    /// Card 012: a live transform session publishes its geometry — and a
    /// committed or cancelled session un-publishes it — through the same call.
    #[test]
    fn a_live_transform_session_publishes_geometry_until_it_ends() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::FreeTransform);
        let doc_id = editor.active().unwrap().id();
        let mut pointer = ToolPointer::new();

        // A press on the canvas's top-left corner starts the session over the
        // canvas bounds and grabs that corner's handle.
        let down = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(0.0, 0.0)),
            false,
            &[],
        );
        assert!(down.reached_tool, "{down:?}");
        let (published_doc, geometry) = pointer
            .live_geometry()
            .expect("a live transform session publishes its geometry");
        assert_eq!(published_doc, doc_id, "the geometry names its document");
        let tools::SessionGeometry::Transform {
            state,
            mode,
            active: _,
            layer: _,
        } = &geometry;

        assert_eq!(*mode, tools::transform::TransformMode::Scale);
        assert_eq!(
            state.source,
            PixelRect::new(0, 0, 64, 64),
            "the session starts over the canvas"
        );

        // The geometry follows the gesture: a drag moves the grabbed corner.
        // Card 041: corner scaling preserves aspect by default, so the drag
        // is diagonal (a horizontal-only drag cannot move the corner).
        let before = state.corners;
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(8.0, 8.0)),
            false,
            &[],
        );
        let (_, geometry) = pointer.live_geometry().expect("still live");
        let tools::SessionGeometry::Transform {
            state: after,
            active,
            ..
        } = geometry;
        assert_ne!(after.corners, before, "the drag moved the published state");
        assert!(active.is_some(), "the grabbed handle is published");

        // Escape: the session ends, and the same route un-publishes.
        assert!(pointer.cancel(&mut editor));
        assert!(
            pointer.live_geometry().is_none(),
            "cancel clears the geometry"
        );
    }

    /// Card 013, end to end: while a transform session is live, the layer's
    /// pixels move **before release** (the preview through the one
    /// compositor); cancelling restores the exact baseline and leaves no
    /// history; committing lands the settled preview at committed quality and
    /// drops the lens — no double transform, one undo entry.
    #[test]
    fn the_live_transform_preview_moves_pixels_before_release_and_resolves_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::FreeTransform);

        // Ink in the canvas layer's middle, so a corner drag moves visible
        // pixels: draw a dark square, through the real command route.
        {
            let doc = editor.active_mut().unwrap();
            let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
            for y in 24..40u32 {
                for x in 24..40u32 {
                    let i = ((y * 256 + x) * 4) as usize;
                    bytes[i..i + 4].copy_from_slice(&[10, 10, 10, 255]);
                }
            }
            let hash = doc.tiles.insert_bytes(bytes);
            let layer = doc.document.active_layer().unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(layer),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
                )
                .unwrap(),
            )
            .unwrap();
        }
        let depth_after_setup = editor.active().unwrap().history_depth();
        let mut pointer = ToolPointer::new();

        // Baseline.
        let baseline = composite(&mut editor);
        let px = |buf: &[u8], x: f32, y: f32| {
            let (x, y) = (x as usize, y as usize);
            let i = (y * 64 + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };

        // Gesture: grab inside the ink (an interior drag translates the quad)
        // and drag it by (+12, +12).
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 24.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 36.0)),
            false,
            &[],
        );
        // The shell settles the preview after every sample; the test calls the
        // same route.
        pointer.settle_preview(&mut editor);

        // Still held — and the pixels have already moved: the preview renders
        // through the one compositor while the handles move.
        let previewed = composite(&mut editor);
        let at = |buf: &[u8], x: f32, y: f32| px(buf, x, y);
        assert_eq!(
            at(&previewed, 42.0, 42.0),
            [10, 10, 10, 255],
            "the ink moved before release: the preview renders the drag"
        );
        assert_eq!(
            at(&previewed, 26.0, 26.0),
            [0, 0, 0, 0],
            "the ink's old spot is empty: the preview moved it"
        );
        // The history is untouched while the handles move.
        assert_eq!(
            editor.active().unwrap().history_depth(),
            depth_after_setup,
            "preview writes no history"
        );

        // Cancel: the baseline comes back exactly, no entry, lens gone.
        assert!(pointer.cancel(&mut editor));
        pointer.settle_preview(&mut editor);
        assert_eq!(
            composite(&mut editor),
            baseline,
            "cancel restores the exact baseline"
        );
        assert_eq!(editor.active().unwrap().history_depth(), depth_after_setup);

        // Commit: the settled preview's pixels become the committed ones —
        // and the preview is dropped, so nothing is transformed twice.
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 24.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 36.0)),
            false,
            &[],
        );
        pointer.settle_preview(&mut editor);
        let previewed = composite(&mut editor);
        assert_eq!(
            at(&previewed, 42.0, 42.0),
            [10, 10, 10, 255],
            "the settled preview moved the ink"
        );
        let committed = pointer.commit(&mut editor);
        pointer.settle_preview(&mut editor);
        assert!(committed.had_pending, "{committed:?}");
        assert_eq!(
            editor.active().unwrap().history_depth(),
            depth_after_setup + 1,
            "one undo entry"
        );
        let after = composite(&mut editor);
        let worst = previewed
            .iter()
            .zip(&after)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        assert!(
            worst <= 3,
            "commit matches the settled preview (worst channel diff {worst})"
        );
        assert_ne!(after, baseline, "and the move persisted");
    }

    // ---- Card 055: the edit target routes pixel edits ------------------

    /// Attaches a fresh reveal-all raster mask to the active layer through
    /// the real command route.
    fn attach_mask(editor: &mut Editor) {
        let command = Command::SetLayerProperties {
            layer_id: editor
                .active()
                .unwrap()
                .document
                .active_layer()
                .expect("a layer"),
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(layer_model::LayerMask::new(
                    layer_model::MaskId::new(),
                )),
                ..Default::default()
            },
        };
        editor.apply_command(command);
    }

    fn layer_tile_hashes(editor: &Editor) -> Vec<raster::TileHash> {
        let doc = editor.active().unwrap();
        let layer = doc.document.active_layer().unwrap();
        doc.document
            .layer_tiles(layer)
            .map(|m| m.iter().map(|(_, h)| h).collect())
            .unwrap_or_default()
    }

    fn mask_tile_count(editor: &Editor) -> usize {
        let doc = editor.active().unwrap();
        let layer = doc.document.active_layer().unwrap();
        doc.document.mask_tiles(layer).map(|m| m.len()).unwrap_or(0)
    }

    /// Brush on the MASK-targeted layer changes coverage tiles only — the
    /// layer's pixels are untouched. Switching back paints content.
    #[test]
    fn brush_on_the_selected_mask_paints_coverage_only_and_back_paints_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        attach_mask(&mut editor);
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        let mut pointer = ToolPointer::new();

        let layer_before = layer_tile_hashes(&editor);
        let mask_tiles_before = mask_tile_count(&editor);
        stroke(&mut pointer, &mut editor, &[(32.0, 32.0), (44.0, 32.0)]);
        assert_eq!(
            layer_tile_hashes(&editor),
            layer_before,
            "a mask-targeted brush changed no layer pixels"
        );
        assert!(
            mask_tile_count(&editor) > mask_tiles_before,
            "the mask gained coverage (a fresh mask starts with no tiles)"
        );
        // The composite changed where the mask reveals: a covered brush
        // stroke over an opaque mask paints ink through the mask.
        // (mask tile present = revealed area painted with the stroke ink)

        // Switch back to content: the same gesture now paints pixels. A
        // fresh pointer: each stroke is its own gesture, exactly as a user's
        // separate drag is.
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Content);
        let mask_hashes_after_mask_stroke = mask_tile_count(&editor);
        let layer_hashes_before = layer_tile_hashes(&editor);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(40.0, 48.0), (52.0, 48.0)]);
        assert_ne!(
            layer_tile_hashes(&editor),
            layer_hashes_before,
            "a content-targeted brush paints pixels"
        );
        assert_eq!(
            mask_tile_count(&editor),
            mask_hashes_after_mask_stroke,
            "the content brush changed no coverage"
        );
    }

    /// Card 058: pointer geometry maps through the MASK's document pose
    /// (layer transform ∘ mask.transform), not the layer's local grid —
    /// an unlinked, independently transformed mask is painted where it
    /// displays, exactly where the compositor samples it.
    #[test]
    fn a_stroke_on_a_transformed_mask_lands_through_the_mask_pose() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        attach_mask(&mut editor);
        // Give the mask its own +8px transform in LAYER space (an unlinked
        // mask the user dragged): the compositor samples the coverage through
        // layer_transform ∘ mask.transform, so coverage painted at the
        // pointer must land 8px to the LEFT in the mask's local store.
        let layer_id = editor.active().unwrap().document.active_layer().unwrap();
        let mut mask = layer_model::LayerMask::new(layer_model::MaskId::new());
        mask.transform = Box::new(glam::Affine2::from_translation(glam::Vec2::new(8.0, 0.0)));
        editor.apply_command(Command::SetLayerProperties {
            layer_id,
            patch: editor_core::LayerPatch {
                mask: editor_core::Patch::Set(mask),
                ..Default::default()
            },
        });
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        // The target switch swapped the wells to the mask pair (white fg),
        // which is exactly the colour this test strokes: on a fresh (all
        // hidden) mask, white REVEALS — so every touched coverage pixel
        // must rise above 0, and the probe can tell the pose-mapped point
        // from the layer-local mirror.
        //
        // A 2px hard brush keeps the reach arithmetic exact: dab radius 1 in
        // MASK-LOCAL pixels (the brush paints in target space).
        let mut brush = *editor.brush();
        brush.size = 2.0;
        brush.hardness = 1.0;
        editor.set_brush(brush);

        let layer_before = layer_tile_hashes(&editor);

        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(24.0, 16.0), (25.0, 16.0)]);

        assert_eq!(
            layer_tile_hashes(&editor),
            layer_before,
            "a mask-targeted brush changed no layer pixels"
        );
        // Probe the STORE bytes (mask-local) directly — the ground truth the
        // compositor samples. With radius 1 the pose-mapped stroke covers
        // store x in [15, 18] on row 16; a layer-space shortcut would paint
        // [23, 26] instead.
        use compositor::TileSource as _;
        let store = |ed: &Editor, x: usize, y: usize| -> u8 {
            let doc = ed.active().unwrap();
            let map = doc.document.mask_tiles(layer_id).unwrap();
            let hash = map
                .get(raster::TileCoord::new(0, 0, 0))
                .expect("the stroke stored coverage");
            doc.tiles.tile(hash).unwrap()[y * 256 + x]
        };
        assert!(
            store(&editor, 16, 16) > 0,
            "the stroke reveals at mask-local (16,16) — the pointer's doc point (24,16) mapped through the pose inverse"
        );
        assert_eq!(
            store(&editor, 24, 16), 0,
            "the pointer's raw document position is untouched in the store — no layer-space shortcut"
        );
        assert_eq!(
            store(&editor, 33, 16), 0,
            "mask-local (33,16) is beyond the pose-mapped reach — the stroke did not smear through the layer grid"
        );
    }

    /// Card 058: a mask stroke is CONSTRAINED by the active selection —
    /// outside the marquee the coverage does not move at all (no tiles are
    /// even created), inside it the paint lands.
    #[test]
    fn a_mask_stroke_is_constrained_by_the_active_selection() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        attach_mask(&mut editor);
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        // White reveals; the wells are already swapped to the mask pair.
        let mut brush = *editor.brush();
        brush.size = 2.0;
        brush.hardness = 1.0;
        editor.set_brush(brush);

        // A small selection in the canvas's upper left; the stroke targets a
        // point well OUTSIDE it.
        editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(10, 10),
        };

        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(32.0, 16.0), (33.0, 16.0)]);
        assert_eq!(
            mask_tile_count(&editor),
            0,
            "a stroke outside the selection creates no coverage at all"
        );

        // Inside the selection the same stroke reveals.
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(5.0, 5.0), (6.0, 5.0)]);
        assert!(
            mask_tile_count(&editor) > 0,
            "a stroke inside the selection reveals coverage"
        );
        let doc = editor.active().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let coverage = crate::menu_bridge::read_mask_coverage(doc, layer, 48, 32);
        assert!(coverage[5 * 48 + 5] > 0);
        assert_eq!(
            coverage[16 * 48 + 32],
            0,
            "the outside point stayed hidden — the selection constrained the stroke"
        );
    }

    /// Card 061: the boundary-refine brush rides the real route — the mask
    /// target (card 055's resolver), one undo step, and only the painted band
    /// moves.
    /// Card 061 (review round 4): the FULL forward boundary, chrome to
    /// tool. The options bar holds a brush key (size), the UI-supplied
    /// blend mode, and a real tool option (strength); the chrome derives
    /// the forward set and the shell's conversion feeds a real press. Two
    /// invariants: nothing refused (no spurious status-bar error from keys
    /// the tool cannot answer — the blend mode must never reach
    /// `set_setting`), and the tool still receives what it does implement
    /// (the strength the op answers).
    #[test]
    fn a_touched_blend_mode_presses_clean_through_the_chrome_boundary() {
        use tools::ToolId;
        use ui::OptionValue;

        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::RefineBoundary);
        attach_mask(&mut editor);
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);

        // What a user's options bar holds after touching three controls:
        // the brush size (a brush key), the paint blend mode (a UI-supplied
        // key with no registry answer), and the refine strength (real).
        let mut chrome = crate::chrome::Chrome::new();
        chrome.set_tool_option(ToolId::RefineBoundary, "size", OptionValue::Float(8.0));
        chrome.set_tool_option(
            ToolId::RefineBoundary,
            ui::tool_options::BLEND_MODE_KEY,
            OptionValue::Choice(1),
        );
        // Non-default: setting the schema default is a deliberate no-op in
        // ToolOptions::set, and this test needs the strength actually held.
        chrome.set_tool_option(ToolId::RefineBoundary, "strength", OptionValue::Float(0.7));
        // The chrome's forward set already excludes the ui-supplied key.
        let held = chrome.tool_options(ToolId::RefineBoundary);
        assert!(
            !held.iter().any(|(k, _)| k.starts_with("ui.")),
            "ui-supplied keys never forward: {held:?}"
        );
        // The shell's boundary conversion (shell.rs), verbatim.
        let settings: Vec<(String, tools::ToolSetting)> = held
            .into_iter()
            .map(|(key, value)| {
                let setting = match value {
                    ui::OptionValue::Float(v) => tools::ToolSetting::Float(v),
                    ui::OptionValue::Int(v) => tools::ToolSetting::Int(v),
                    ui::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
                    ui::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
                    ui::OptionValue::Color(v) => tools::ToolSetting::Color(v),
                };
                (key, setting)
            })
            .collect();

        // A real press over the mask. Solid coverage is a correct no-op for
        // the op itself — this test pins the refusal channel, not geometry.
        let mut pointer = ToolPointer::new();
        let out = pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(16.0, 16.0)),
            false,
            &settings,
        );
        assert!(
            out.failed.is_none(),
            "a touched blend mode must not spuriously refuse: {:?}",
            out.failed
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(17.0, 16.0)),
            false,
            &settings,
        );
    }

    #[test]
    fn the_boundary_refine_brush_regrades_the_mask_band_over_the_real_route() {
        use tools::ToolId;
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::RefineBoundary);
        attach_mask(&mut editor);
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        // A STAIR-STEP baseline (the refine's food: flat runs are fixed
        // points, so a solid mask would be a correct no-op) — painted
        // directly (Reveal All is a CREATION op and would refuse).
        let layer = editor.active().unwrap().document.active_layer().unwrap();
        {
            let doc = editor.active_mut().unwrap();
            let ts = raster::TILE_SIZE as usize;
            let mut tile = vec![0u8; ts * ts];
            for y in 0..ts {
                for x in 0..ts {
                    tile[y * ts + x] = ((x / 6) % 4) as u8 * 85;
                }
            }
            let hash = doc.tiles.insert_bytes(tile);
            editor_core::Command::PaintTiles {
                target: editor_core::pixels::PixelTarget::Mask(layer),
                delta: editor_core::pixels::TileDelta::new(std::iter::once(
                    editor_core::pixels::TileEdit::set(raster::TileCoord::new(0, 0, 0), hash),
                ))
                .unwrap(),
            }
            .apply(&mut doc.document)
            .unwrap();
        }

        let before = {
            use compositor::TileSource as _;
            let doc = editor.active().unwrap();
            doc.document
                .mask_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(c, h)| (c, doc.tiles.tile(h).map(|b| b.to_vec())))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let layer_before = layer_tile_hashes(&editor);
        // The direct coverage paint above is itself a history entry; the
        // refine stroke adds exactly one MORE.
        let depth_before = editor.active().unwrap().history.undo_depth();

        // Stroke a band across the middle of the mask.
        editor.set_brush(tools::BrushSettings {
            size: 8.0,
            hardness: 1.0,
            ..*editor.brush()
        });
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(12.0, 16.0), (36.0, 16.0)]);

        // ONE undo step; the layer's pixels untouched; the band's coverage
        // regraded (the solid 255 plateau under the stroke stays 255 — a
        // fixed point — so the byte-level discriminator is the history +
        // the unchanged layer; a NON-solid fixture is covered tools-level).
        assert_eq!(
            editor.active().unwrap().history.undo_depth(),
            depth_before + 1,
            "one refine stroke is one undo step"
        );
        assert_eq!(
            layer_tile_hashes(&editor),
            layer_before,
            "the refine brush changed no layer pixels"
        );
        let after = {
            use compositor::TileSource as _;
            let doc = editor.active().unwrap();
            doc.document
                .mask_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(c, h)| (c, doc.tiles.tile(h).map(|b| b.to_vec())))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(after.len(), before.len(), "the same tiles are still there");
        // The band's coverage MOVED: the stair under the stroke was regraded
        // into a ramp (compare the tile bytes at the stroked row).
        let moved = after.iter().zip(before.iter()).any(|((ca, ha), (cb, hb))| {
            ca == cb
                && match (ha.as_deref(), hb.as_deref()) {
                    (Some(a), Some(b)) => a != b,
                    _ => false,
                }
        });
        assert!(moved, "the refine stroke regraded the band's coverage");
        // Undo restores the pre-stroke coverage exactly.
        editor.active_mut().unwrap().undo().unwrap();
        let restored = {
            use compositor::TileSource as _;
            let doc = editor.active().unwrap();
            doc.document
                .mask_tiles(layer)
                .map(|m| {
                    m.iter()
                        .map(|(c, h)| (c, doc.tiles.tile(h).map(|b| b.to_vec())))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        assert_eq!(restored, before, "undo restores the baseline coverage");
    }

    /// Card 055: the gesture PINS its target — a target change landing
    /// between samples cannot redirect the middle of a stroke.
    #[test]
    fn a_target_change_between_samples_cannot_redirect_the_stroke_in_progress() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        attach_mask(&mut editor);
        // Content target at gesture start.
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Content);
        let layer_before = layer_tile_hashes(&editor);
        let mask_before = mask_tile_count(&editor);
        let mut pointer = ToolPointer::new();

        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(24.0, 24.0)),
            false,
            &[],
        );
        // The target flips MID-GESTURE (a thumbnail click landing between
        // samples).
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(32.0, 24.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(40.0, 24.0)),
            false,
            &[],
        );

        assert_ne!(
            layer_tile_hashes(&editor),
            layer_before,
            "the stroke stayed on the target it was pinned to (content)"
        );
        assert_eq!(
            mask_tile_count(&editor),
            mask_before,
            "the mid-gesture flip did not redirect coverage"
        );
    }

    /// A mask target whose mask has been removed resolves to content — the
    /// read-time validation keeps painting possible instead of silently
    /// dropping edits.
    #[test]
    fn a_mask_target_without_a_mask_falls_back_to_content() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        editor.set_edit_target_kind(crate::edit_target::EditTargetKind::Mask);
        // Card 058: the switch swapped the wells to the mask pair (white),
        // which is a no-op ink on the white canvas — so the test picks a
        // colour AFTER the switch, as a user would.
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        // No mask was ever attached: the sticky kind names a mask that is
        // not there.
        let layer_before = layer_tile_hashes(&editor);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(32.0, 32.0), (44.0, 32.0)]);
        assert_ne!(
            layer_tile_hashes(&editor),
            layer_before,
            "the fallback paints the layer's pixels"
        );
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
                &[],
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
                    &[],
                );
            }
            pointer.handle(
                &mut editor,
                sample(PointerPhase::Up, screen(44.0, 32.0)),
                false,
                &[],
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
        // Card 056: the selection rides history now — one gesture, one
        // undoable step — and it is unsaved work like any other edit.
        assert_eq!(doc.history_depth(), 1);
        assert!(
            outcomes.iter().any(|o| o.steps == 1),
            "the selection gesture reports its history step: {outcomes:?}"
        );
        assert!(doc.is_dirty());
        // No image pixel changed: the composite is byte-identical.
        // (verified in the composability test below)

        // Undo restores NO selection; redo restores the rectangle.
        assert!(ed_undo(&mut editor));
        assert_eq!(
            editor.active().unwrap().document.selection,
            Selection::None,
            "undo removes the selection"
        );
        assert!(editor.active_mut().unwrap().redo().unwrap());
        let (min, max) = editor
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("the redo restores the rectangle");
        assert_eq!((min.x, min.y, max.x, max.y), (10, 12, 40, 44));
    }

    /// A redo/undo helper: `Editor::redo` is the action's route.
    fn ed_undo(editor: &mut Editor) -> bool {
        editor.active_mut().unwrap().undo().unwrap()
    }

    /// Card 056: add/subtract compose through history, one gesture is one
    /// step, and not one image pixel moves.
    #[test]
    fn a_selection_gesture_composes_add_and_subtract_through_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::RectMarquee);
        let composite_before = composite(&mut editor);
        let mut pointer = ToolPointer::new();

        // Replace: the base rectangle.
        stroke(&mut pointer, &mut editor, &[(10.0, 10.0), (30.0, 30.0)]);
        assert_eq!(editor.active().unwrap().history_depth(), 1);
        // Add (shift): a second rectangle unioned onto the first — one more
        // gesture, one more step.
        let mut add_points = [sample(PointerPhase::Down, screen(40.0, 40.0))];
        add_points[0].modifiers.shift = true;
        pointer.handle(&mut editor, add_points[0], false, &[]);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(50.0, 50.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(50.0, 50.0)),
            false,
            &[],
        );
        assert_eq!(editor.active().unwrap().history_depth(), 2);
        let (bmin, bmax) = editor
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("both");
        assert_eq!((bmin.x, bmin.y), (10, 10), "the add covers the first rect");
        assert_eq!((bmax.x, bmax.y), (50, 50), "the add covers the second rect");

        // Subtract (alt): carve the middle out of the union.
        let mut sub_points = [sample(PointerPhase::Down, screen(20.0, 20.0))];
        sub_points[0].modifiers.alt = true;
        pointer.handle(&mut editor, sub_points[0], false, &[]);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(45.0, 45.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(45.0, 45.0)),
            false,
            &[],
        );
        assert_eq!(editor.active().unwrap().history_depth(), 3);

        // Not one image pixel moved through all three gestures.
        assert_eq!(
            composite(&mut editor),
            composite_before,
            "selection edits paint nothing"
        );

        // Undo walks the gestures back one at a time, and not one image
        // pixel moved through any of it.
        assert_eq!(
            composite(&mut editor),
            composite_before,
            "the subtract painted nothing"
        );
        ed_undo(&mut editor);
        let (smin, smax) = editor
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("union");
        assert_eq!(
            (smin.x, smin.y, smax.x, smax.y),
            (10, 10, 50, 50),
            "undo removes exactly the subtract"
        );
        ed_undo(&mut editor);
        let (fmin, fmax) = editor
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("first");
        assert_eq!(
            (fmin.x, fmin.y, fmax.x, fmax.y),
            (10, 10, 30, 30),
            "undo removes exactly the add"
        );
        ed_undo(&mut editor);
        assert_eq!(editor.active().unwrap().document.selection, Selection::None);
        // Redo replays all three.
        for _ in 0..3 {
            assert!(editor.active_mut().unwrap().redo().unwrap());
        }
        let (rmin, rmax) = editor
            .active()
            .unwrap()
            .document
            .selection
            .bounds()
            .expect("redone");
        assert_eq!(
            (rmin.x, rmin.y, rmax.x, rmax.y),
            (10, 10, 50, 50),
            "redo replays the composed selection"
        );
        assert_eq!(
            composite(&mut editor),
            composite_before,
            "redo still paints nothing"
        );
    }

    /// Card 056: a committed selection survives save/reopen, and a gesture
    /// that never commits (abandoned mid-drag) replays nothing.
    #[test]
    fn a_committed_selection_survives_save_and_an_abandoned_gesture_leaves_no_trace() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::RectMarquee);
        let mut pointer = ToolPointer::new();

        // A committed gesture...
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0), (24.0, 24.0)]);
        let committed = editor.active().unwrap().document.selection.clone();
        let package = dir.path().join("selection.rstudio");
        editor
            .active_mut()
            .unwrap()
            .save_to(&package, "test")
            .expect("the project saves");

        // ...an ABANDONED one: down, move, Escape — cancel emits nothing.
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(40.0, 40.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(50.0, 50.0)),
            false,
            &[],
        );
        pointer.cancel(&mut editor);
        assert_eq!(
            editor.active().unwrap().document.selection,
            committed,
            "the abandoned gesture changed nothing"
        );

        // Reopen: the committed selection is exactly what was saved.
        let mut reopened = Editor::with_state(
            AppPaths::rooted(dir.path().join("config2")),
            crate::prefs::Preferences::default(),
            crate::recent::RecentFiles::new(),
            Box::new(crate::dialogs::ScriptedDialogs::new()),
        );
        reopened.open_path(&package).expect("the project reopens");
        assert_eq!(
            reopened.active().unwrap().document.selection,
            committed,
            "the committed selection survives persistence"
        );
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
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(40.0, 40.0)),
            false,
            &[],
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
            &[],
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
            &[],
        );
        assert!(down.reached_tool);
        let moved = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 32.0)),
            true,
            &[],
        );
        assert!(moved.reached_tool, "the drag died over a panel");
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(44.0, 32.0)),
            true,
            &[],
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
            &[],
        );
        let moved = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(44.0, 32.0)).with_button(PointerButton::Middle),
            false,
            &[],
        );
        assert_eq!(moved.route, Some(Route::Pan));
        assert!(!moved.reached_tool);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(44.0, 32.0)).with_button(PointerButton::Middle),
            false,
            &[],
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
            &[],
        );
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(32.0, 32.0)),
            false,
            &[],
        );
        assert!(up.view_changed, "a zoom click did not move the view");
        assert!(editor.active().unwrap().camera.zoom > zoom);
        assert_eq!(editor.active().unwrap().history_depth(), 0);

        // ...and alt-clicking goes back out.
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(32.0, 32.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(32.0, 32.0)).with_modifiers(Modifiers::alt()),
            false,
            &[],
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
            &[],
        );
        assert_eq!(pointer.live_tool(), Some(ToolId::Brush));

        editor.set_tool(ToolId::RectMarquee);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(30.0, 30.0)),
            false,
            &[],
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
            &[],
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
            &[],
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
            &[],
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
            &[],
        );
        let drifted = pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(40.0, 40.0)),
            false,
            &[],
        );
        let up = pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(40.0, 40.0)),
            false,
            &[],
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
            &[],
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
            &[],
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
            &[],
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
                &[],
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
            &[],
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

    /// The seven shape tools run, create a layer, **and** put it on the canvas.
    ///
    /// This test used to assert the opposite, and said so: `compositor` had no
    /// rasteriser for `LayerKind::Shape`, so a shape gesture cost an undo step
    /// and a layer row and left the composited pixels byte-identical. It now
    /// has one, so the assertion is inverted and the "draws nothing" bullets
    /// that stood in this module's docs and in `lib.rs` are gone with it.
    #[test]
    fn a_shape_gesture_creates_a_layer_the_compositor_draws() {
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

            // The pixels moved, and they moved inside the dragged box.
            // Asserted first, because it is the claim the whole gesture is for.
            let after = composite(&mut editor);
            let reached = changed_pixels(&before, &after);
            assert!(
                !reached.is_empty(),
                "{tool:?} put nothing on the canvas: the shape rasteriser is \
                 not reached from a canvas gesture"
            );
            for (x, y) in &reached {
                assert!(
                    (9..=41).contains(x) && (9..=41).contains(y),
                    "{tool:?} painted ({x}, {y}), outside the dragged box"
                );
            }

            // ...and it is a real undoable step with a real layer row.
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

            // ...and undo takes the pixels back with the layer.
            assert!(editor.active_mut().unwrap().undo().unwrap());
            assert_eq!(
                composite(&mut editor),
                before,
                "{tool:?}: undo did not restore the canvas"
            );
        }
    }

    // ------------------------------------------------ the commit route ----

    /// The headline of the commit route: a crop drag followed by Enter really
    /// cuts the canvas, moves the pixels under the new origin, and is one
    /// Ctrl+Z.
    ///
    /// Before this, `grep '\.commit(' crates/app-shell/src` returned nothing:
    /// the crop tool published its [`tools::CropRequest`] from a method the
    /// shell never called, so a drag left a rectangle on screen and produced no
    /// command, no status and no pixel.
    #[test]
    fn a_crop_drag_then_enter_cuts_the_canvas_and_one_undo_puts_it_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let mut pointer = ToolPointer::new();

        // A single black pixel at (45, 35), so "the layers slid under the new
        // origin" is checkable rather than asserted about a uniform white
        // canvas. The Pencil is one hard aliased pixel.
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
        stroke(&mut pointer, &mut editor, &[(45.0, 35.0)]);
        let full = composite(&mut editor);
        let at = |buf: &[u8], x: usize, y: usize, w: usize| {
            let i = (y * w + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert!(
            at(&full, 45, 35, W as usize)[0] < 40,
            "the fixture's mark is not where the test thinks it is"
        );
        let steps_before = editor.active().unwrap().history_depth();

        // Drag a keep-region of (40, 20)..(60, 40).
        editor.set_tool(ToolId::Crop);
        stroke(&mut pointer, &mut editor, &[(40.0, 20.0), (60.0, 40.0)]);
        // The drag alone changes nothing: the box waits for Enter.
        assert_eq!(editor.active().unwrap().document.width(), W);
        assert_eq!(
            editor.active().unwrap().history_depth(),
            steps_before,
            "the drag committed something on its own"
        );
        assert!(pointer.has_pending_commit(), "the crop box was not held");

        let outcome = pointer.commit(&mut editor);
        assert!(outcome.had_pending);
        assert_eq!(
            outcome.cropped_to.map(|r| (r.x, r.y, r.width, r.height)),
            Some((40, 20, 20, 20))
        );
        assert_eq!(outcome.steps, 1, "a crop is one undoable step: {outcome:?}");
        assert_eq!(outcome.failed, None);

        let doc = editor.active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (20, 20));
        assert_eq!(doc.history_depth(), steps_before + 1);
        assert!(editor.status().is_some_and(|s| s.contains("Cropped")));

        // The mark moved with the canvas: (45, 35) is (5, 15) now.
        let cropped = editor
            .active_mut()
            .unwrap()
            .composite(PixelRect::new(0, 0, 20, 20))
            .unwrap();
        assert!(
            at(&cropped, 5, 15, 20)[0] < 40,
            "the layers did not slide under the new origin: {:?}",
            at(&cropped, 5, 15, 20)
        );

        // ...and one undo takes the whole crop back, canvas and pixels.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        let doc = editor.active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (W, H));
        assert_eq!(
            composite(&mut editor),
            full,
            "undo did not restore the crop"
        );
    }

    /// Escape after a crop drag leaves the document exactly as it was, and the
    /// Enter that follows has nothing to confirm.
    #[test]
    fn escape_after_a_crop_drag_leaves_the_canvas_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Crop);
        let mut pointer = ToolPointer::new();
        let before = composite(&mut editor);

        stroke(&mut pointer, &mut editor, &[(10.0, 10.0), (50.0, 40.0)]);
        assert!(pointer.has_pending_commit());
        assert!(pointer.cancel(&mut editor), "there was a box to abandon");
        assert!(!pointer.has_pending_commit(), "the box survived Escape");

        let outcome = pointer.commit(&mut editor);
        assert!(!outcome.had_pending, "Enter re-applied a cancelled crop");
        assert_eq!(outcome.steps, 0);
        let doc = editor.active().unwrap();
        assert_eq!((doc.document.width(), doc.document.height()), (W, H));
        assert_eq!(doc.history_depth(), 0);
        assert_eq!(composite(&mut editor), before);
    }

    /// Enter with nothing held is not the commit route's business: it must
    /// report that it did nothing so the shell can hand the key on.
    #[test]
    fn enter_with_no_held_gesture_does_nothing_at_all() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        assert_eq!(pointer.commit(&mut editor), CommitOutcome::default());
        // ...and after a stroke, which ends at pointer-up rather than here.
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (30.0, 30.0)]);
        assert!(!pointer.has_pending_commit());
        assert!(!pointer.commit(&mut editor).had_pending);
        assert_eq!(editor.active().unwrap().history_depth(), 1);
    }

    /// A free-transform session ends on Enter with one resample and one step.
    #[test]
    fn committing_a_free_transform_paints_once_and_is_undoable() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        // Something to transform: a black mark off-centre.
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (28.0, 28.0)]);
        let painted = composite(&mut editor);

        editor.set_tool(ToolId::FreeTransform);
        // A press starts the session over the canvas; the drag moves a handle.
        stroke(&mut pointer, &mut editor, &[(0.0, 0.0), (10.0, 10.0)]);
        assert!(
            pointer.has_pending_commit(),
            "the transform session did not stay live after the release"
        );

        let outcome = pointer.commit(&mut editor);
        assert!(outcome.had_pending);
        assert_eq!(outcome.failed, None, "{outcome:?}");
        assert_eq!(outcome.steps, 1, "{outcome:?}");
        assert_ne!(composite(&mut editor), painted, "the transform did nothing");
        assert!(!pointer.has_pending_commit(), "the session did not end");

        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), painted);
    }

    /// Scaling a selection through the gizmo rewrites the selection mask as
    /// one undoable SetSelection step, and undo puts the original mask back.
    #[test]
    fn scaling_a_selection_changes_its_mask_and_undo_restores_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        // A rectangle selection to transform.
        editor.set_tool(ToolId::RectMarquee);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(16.0, 16.0), (48.0, 48.0)]);
        let before = editor.active().unwrap().document.selection.clone();
        let coverage = |sel: &editor_core::Selection| -> f32 {
            match &sel {
                editor_core::Selection::Mask(m) => {
                    m.coverage().iter().map(|&v| v as u32).sum::<u32>() as f32
                }
                _ => 0.0,
            }
        };
        assert!(coverage(&before) > 0.0, "the marquee produced a mask");

        // The gizmo wearing its Selection target: the menu route sets the
        // choice, the press begins over the selection's bounds, and a corner
        // drag scales the mask.
        let mut pointer = ToolPointer::new();
        editor.set_tool(ToolId::FreeTransform);
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(16.0, 16.0)),
            false,
            &[("target".to_string(), tools::ToolSetting::Choice(1))],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(8.0, 8.0)),
            false,
            &[("target".to_string(), tools::ToolSetting::Choice(1))],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(8.0, 8.0)),
            false,
            &[("target".to_string(), tools::ToolSetting::Choice(1))],
        );

        let outcome = pointer.commit(&mut editor);
        assert!(outcome.had_pending, "{outcome:?}");
        assert_eq!(outcome.failed, None, "{outcome:?}");
        assert_eq!(outcome.steps, 1, "one SetSelection step: {outcome:?}");

        let after = editor.active().unwrap().document.selection.clone();
        assert_ne!(after, before, "the mask moved");
        assert!(
            editor.active_mut().unwrap().undo().unwrap(),
            "the transform is undoable"
        );
        let restored = editor.active().unwrap().document.selection.clone();
        assert_eq!(restored, before, "undo restored the original mask");
        assert!((coverage(&restored) - coverage(&before)).abs() < 1e-3);
    }

    /// Per-channel editing (P2.7): with a colour component as the edit target,
    /// only that component's bytes move, and undo restores everything.
    #[test]
    fn a_colour_mode_conversion_rewrites_every_layer_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.9, 0.2, 0.3, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0), (56.0, 56.0)]);
        let rgb = composite(&mut editor);

        // RGB -> Grayscale: every pixel collapses to its luma.
        editor
            .set_color_mode(ui::menu::ColorMode::Grayscale)
            .unwrap();
        let gray = composite(&mut editor);
        for px in gray.chunks(4) {
            assert_eq!(px[0], px[1], "r == g in grayscale");
            assert_eq!(px[1], px[2], "g == b in grayscale");
        }
        let luma = |px: [u8; 4]| -> u8 {
            (0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32).round() as u8
        };
        let rgb_px: Vec<[u8; 4]> = rgb.chunks(4).map(|p| [p[0], p[1], p[2], p[3]]).collect();
        assert!(
            rgb_px.iter().any(|p| luma(*p) != p[0]),
            "the stroke carried colour worth converting"
        );
        for (src, dst) in rgb_px.iter().zip(gray.chunks(4)) {
            assert_eq!(dst[0], luma(*src), "grayscale byte is the luma");
        }
        assert_eq!(
            editor.active().unwrap().document.meta.color_mode,
            1,
            "the document now reads as grayscale"
        );

        // One undo returns the colours.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), rgb, "one undo restores RGB");

        // Grayscale -> RGB is also one step, and one undo lands back on gray.
        editor
            .set_color_mode(ui::menu::ColorMode::Grayscale)
            .unwrap();
        editor.set_color_mode(ui::menu::ColorMode::Rgb).unwrap();
        assert_eq!(
            editor.active().unwrap().document.meta.color_mode,
            0,
            "back to RGB"
        );
        assert!(editor.active_mut().unwrap().undo().unwrap());
        for px in composite(&mut editor).chunks(4) {
            assert_eq!(px[0], px[1], "one undo from RGB lands back on gray");
            assert_eq!(px[1], px[2], "g == b");
        }
    }

    #[test]
    fn recording_three_edits_replays_onto_a_second_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        // A second identically-structured document: same 64x64 white base.
        let png2 = dir.path().join("second.png");
        std::fs::write(
            &png2,
            raster::encode(
                raster::ExportFormat::Png,
                W,
                H,
                &[255u8; (W * H * 4) as usize],
            )
            .unwrap(),
        )
        .unwrap();
        editor.open_path(&png2).unwrap();
        // The same camera the first document was given: 100%, centred —
        // `screen()` routes against it.
        let doc = editor.active_mut().unwrap();
        doc.set_viewport(VIEWPORT);
        doc.camera.zoom = 1.0;
        doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
        // Record on the FIRST document; replay onto the second.
        editor.activate(0).unwrap();

        // Record three edits on the active document (B, the newest).
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.9, 0.2, 0.3, 1.0]);
        editor.start_recording();
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0), (56.0, 56.0)]);
        editor.set_tool(ToolId::Eraser);
        stroke(&mut pointer, &mut editor, &[(40.0, 8.0), (44.0, 12.0)]);
        editor.set_paint_channel(None);
        crate::menu_bridge::fill_selection_with(
            &mut editor,
            &ui::dialogs::FillSpec {
                contents: ui::dialogs::FillContents::Foreground,
                ..Default::default()
            },
        )
        .unwrap();
        let recording = editor.stop_recording().unwrap();
        assert_eq!(recording.len(), 3, "three edits captured");
        let reference = composite(&mut editor);

        // Replay on the OTHER document: activate tab 1 and replay.
        editor.activate(1).unwrap();
        let before = composite(&mut editor);
        assert_ne!(before, reference, "the documents start different");
        let applied = editor.replay(&recording);
        assert_eq!(applied, 3, "every captured edit replayed");
        assert_eq!(
            composite(&mut editor),
            reference,
            "the replay reproduces the recording's composite byte for byte"
        );
    }

    #[test]
    fn path_select_and_direct_selection_work_a_shape_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        // A shape layer carrying a triangle path.
        let shape = layer_model::Layer::with_kind(
            "Triangle",
            layer_model::LayerKind::Shape(layer_model::ShapeLayer::from_svg("M8 8 L40 8 L40 40 Z")),
        );
        let shape_id = shape.id;
        editor.apply_command(editor_core::Command::create_layer(shape));

        // Path Select: clicking the path selects the layer that owns it.
        editor.set_tool(ToolId::PathSelect);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(24.0, 8.0)]);
        let doc = editor.active().unwrap();
        assert_eq!(
            doc.document.layer_selection(),
            vec![shape_id],
            "the click on the path selected its layer"
        );

        // A click off the path leaves the selection alone: the tool only
        // speaks when a path is hit (clearing on a miss is Photopea parity
        // this build has not taken on).
        stroke(&mut pointer, &mut editor, &[(80.0, 80.0)]);
        let doc = editor.active().unwrap();
        assert_eq!(
            doc.document.layer_selection(),
            vec![shape_id],
            "a miss does not disturb the selection"
        );

        // Direct Selection: dragging the first anchor moves it as ONE undo
        // step that rewrites the layer's path.
        editor.set_tool(ToolId::DirectSelection);
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0), (20.0, 20.0)]);
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Shape(shape) = &doc.document.layers.get(shape_id).unwrap().kind
        else {
            panic!("the layer is still a shape");
        };
        let path = vector::svg::parse(&shape.path_svg).unwrap();
        assert_eq!(
            path.elements().first(),
            Some(&vector::PathEl::MoveTo(vector::Point::new(20.0, 20.0))),
            "the dragged anchor moved"
        );
        assert!(
            !shape.path_svg.contains("8 8"),
            "the old anchor position is gone: {}",
            shape.path_svg
        );

        // One undo returns the old path.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Shape(shape) = &doc.document.layers.get(shape_id).unwrap().kind
        else {
            panic!("the layer is still a shape after undo");
        };
        assert_eq!(shape.path_svg, "M8 8 L40 8 L40 40 Z", "undo restored");
    }

    #[test]
    fn quick_mask_painting_becomes_the_selection_on_leaving() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([1.0, 1.0, 1.0, 1.0]);
        editor.toggle_quick_mask().unwrap();
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (28.0, 28.0)]);
        editor.toggle_quick_mask().unwrap();

        // The painted coverage IS the selection, and the scratch layer is
        // gone: the document is back to one layer, holding a mask selection
        // that covers the stroke and nothing else.
        let doc = editor.active().unwrap();
        assert_eq!(doc.document.layers.len(), 1, "scratch layer removed");
        match &doc.document.selection {
            editor_core::Selection::Mask(mask) => {
                assert_eq!(mask.width(), doc.document.width());
                assert_eq!(mask.height(), doc.document.height());
                let w = doc.document.width() as usize;
                let cov = mask.coverage();
                assert!(cov[20 * w + 20] > 0, "the stroked pixel is selected");
                assert!(cov[24 * w + 24] > 0, "mid-stroke is selected");
                assert!(cov[60 * w + 60] == 0, "an unpainted pixel is not");
                let painted = cov.iter().filter(|b| **b > 0).count();
                assert!(painted > 2, "more than the probe pixels carry coverage");
                assert!(painted < w * w / 2, "coverage stays near the stroke");
            }
            other => panic!("expected a mask selection, got {other:?}"),
        }
    }

    #[test]
    fn the_eraser_through_the_red_channel_clears_only_red() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.8, 0.1, 0.2, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (28.0, 28.0)]);
        let before = composite(&mut editor);

        // Aim the edit at the red component and erase part of the mark.
        editor.set_paint_channel(Some(0));
        editor.set_tool(ToolId::Eraser);
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (24.0, 24.0)]);
        let after = composite(&mut editor);

        // Every pixel: red moved or stayed; green and blue are byte-exact.
        let red_moved = before
            .chunks(4)
            .zip(after.chunks(4))
            .any(|(b, a)| b[0] != a[0]);
        assert!(red_moved, "the eraser moved red");
        for (b, a) in before.chunks(4).zip(after.chunks(4)) {
            assert_eq!(b[1], a[1], "green untouched: {b:?} -> {a:?}");
            assert_eq!(b[2], a[2], "blue untouched: {b:?} -> {a:?}");
            assert_eq!(b[3], a[3], "alpha untouched: {b:?} -> {a:?}");
        }

        // Whole undo: every byte comes back.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), before, "undo restored all channels");
    }

    #[test]
    fn gaussian_blur_through_the_red_channel_blurs_only_red() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.9, 0.2, 0.3, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0), (56.0, 56.0)]);
        let before = composite(&mut editor);

        editor.set_paint_channel(Some(0));
        let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();
        let invocation = ui::dialogs::FilterInvocation {
            filter: spec,
            params: ui::dialogs::FilterParams::defaults(spec.params),
        };
        crate::menu_bridge::run_filter_invocation(&mut editor, &invocation).unwrap();
        let after = composite(&mut editor);

        // Red blurred: it moved, and only it. The other channels are isolated
        // at the TILE level (mask_delta keeps their prior bytes byte-exact);
        // the composite can still show them off by one, because the
        // premultiplied store round-trips through straight on read and the
        // masked red bytes shift the values the rounding sees. A
        // whole-channel shift would be a defect; a one-level rounding wobble
        // is the storage's own quantisation.
        assert!(
            before
                .chunks(4)
                .zip(after.chunks(4))
                .any(|(b, a)| b[0] != a[0]),
            "the blur moved red"
        );
        for (b, a) in before.chunks(4).zip(after.chunks(4)) {
            assert!(
                (b[1] as i32 - a[1] as i32).abs() <= 1,
                "green wobbled more than rounding: {b:?} -> {a:?}"
            );
            assert!(
                (b[2] as i32 - a[2] as i32).abs() <= 1,
                "blue wobbled more than rounding: {b:?} -> {a:?}"
            );
            assert_eq!(b[3], a[3], "alpha untouched: {b:?} -> {a:?}");
        }

        // Whole undo: the blurred red comes back exactly.
        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), before, "undo restored all channels");
    }

    /// A warp control-point drag bends the layer and is one undo step: the
    /// mesh gizmo was already in tools::transform, so this drives the menu
    /// route (mode = Warp) through a mesh point and commits.
    #[test]
    fn a_warp_control_point_drag_bends_the_layer_as_one_undo_step() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (28.0, 28.0)]);
        let painted = composite(&mut editor);

        editor.set_tool(ToolId::FreeTransform);
        let mut warp = ToolPointer::new();
        // The session opens over the canvas; in Warp mode the handles are the
        // 4x4 mesh, laid on the source rect: point (1,1) sits at (16,16).
        warp.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(16.0, 16.0)),
            false,
            &[("mode".to_string(), tools::ToolSetting::Choice(5))],
        );
        warp.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(26.0, 26.0)),
            false,
            &[("mode".to_string(), tools::ToolSetting::Choice(5))],
        );
        warp.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(26.0, 26.0)),
            false,
            &[("mode".to_string(), tools::ToolSetting::Choice(5))],
        );
        assert!(warp.has_pending_commit(), "the warp session stayed live");

        let outcome = warp.commit(&mut editor);
        assert!(outcome.had_pending, "{outcome:?}");
        assert_eq!(outcome.failed, None, "{outcome:?}");
        assert_eq!(outcome.steps, 1, "one undoable step: {outcome:?}");
        assert_ne!(composite(&mut editor), painted, "the mesh bent the layer");

        assert!(editor.active_mut().unwrap().undo().unwrap());
        assert_eq!(composite(&mut editor), painted, "undo restored the pixels");
    }

    /// A drag that collapses the quad has no inverse: the commit refuses, says
    /// so, and the history gains nothing.
    #[test]
    fn a_singular_transform_is_refused_not_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(20.0, 20.0), (28.0, 28.0)]);
        let depth_before = editor.active().unwrap().history_depth();

        editor.set_tool(ToolId::FreeTransform);
        // The session begins over the whole canvas, corners at its four
        // corners. Drag each one onto the canvas centre, collapsing the quad
        // to a point: four coincident corners have no inverse.
        stroke(&mut pointer, &mut editor, &[(0.0, 0.0), (32.0, 32.0)]);
        stroke(&mut pointer, &mut editor, &[(64.0, 0.0), (32.0, 32.0)]);
        stroke(&mut pointer, &mut editor, &[(64.0, 64.0), (32.0, 32.0)]);
        stroke(&mut pointer, &mut editor, &[(0.0, 64.0), (32.0, 32.0)]);

        let outcome = pointer.commit(&mut editor);
        assert!(outcome.had_pending, "the session was live: {outcome:?}");
        assert!(
            outcome.failed.is_some(),
            "a collapsed quad must be refused: {outcome:?}"
        );
        assert_eq!(
            editor.active().unwrap().history_depth(),
            depth_before,
            "a refused transform leaves no step behind"
        );
    }

    /// Slices reach the caller and the status bar, and go no further — this
    /// build cannot export them. An honest gap, said out loud.
    #[test]
    fn committing_slices_reports_them_and_says_they_cannot_be_exported() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Slice);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(4.0, 4.0), (20.0, 20.0)]);
        stroke(&mut pointer, &mut editor, &[(30.0, 30.0), (50.0, 50.0)]);
        assert!(pointer.has_pending_commit());

        let outcome = pointer.commit(&mut editor);
        assert_eq!(outcome.slices.len(), 2, "{outcome:?}");
        assert_eq!(
            outcome.slices[0].rect.width, 16,
            "the slice is not the dragged rectangle"
        );
        assert_eq!(outcome.steps, 0, "a slice set is not a document edit");
        assert!(editor
            .status()
            .is_some_and(|s| s.contains("2 slice(s)") && s.contains("cannot export")));
        // Committing twice does not publish the same slices again.
        assert!(!pointer.commit(&mut editor).had_pending);
    }

    // --------------------------------------------- creating with a click ----

    /// A Type-tool click creates exactly one text layer, at the document point
    /// that was clicked.
    #[test]
    fn a_type_click_creates_exactly_one_text_layer_at_the_clicked_point() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        let layers_before = editor.active().unwrap().document.layers.len();

        stroke(&mut pointer, &mut editor, &[(20.0, 30.0)]);

        let doc = editor.active().unwrap();
        assert_eq!(
            doc.document.layers.len(),
            layers_before + 1,
            "a click made {} layers",
            doc.document.layers.len() - layers_before
        );
        assert_eq!(doc.history_depth(), 1, "the layer is not undoable");
        let text: Vec<_> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .filter(|l| matches!(l.kind, layer_model::LayerKind::Text(_)))
            .collect();
        assert_eq!(text.len(), 1, "expected exactly one text layer");
        assert_eq!(
            text[0].transform.translation,
            Vec2::new(20.0, 30.0),
            "the text layer is not where the click landed"
        );
        assert!(text[0].visible);
        // ...and the tool is holding the run open for typing.
        assert!(pointer.is_text_editing());
    }

    /// The other half of the Type tool: a keystroke reaches the layer's run —
    /// live, through the history-free draft route (card 025), with the whole
    /// run landing as one history entry on confirm.
    #[test]
    fn typing_after_a_type_click_rewrites_the_layers_run() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        let depth_after_create = editor.active().unwrap().history_depth();

        for ch in ["H", "i"] {
            let out = pointer.text_edit(&mut editor, tools::TextEdit::Insert(ch));
            assert!(out.had_pending, "the keystroke reached nobody: {out:?}");
            assert_eq!(out.steps, 0, "keystrokes render outside history: {out:?}");
            assert_eq!(
                editor.active().unwrap().history_depth(),
                depth_after_create,
                "typing costs no history"
            );
        }
        let run = |editor: &Editor| {
            let doc = editor.active().unwrap();
            doc.document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter_map(|id| doc.document.layers.get(id))
                .find_map(|l| match &l.kind {
                    layer_model::LayerKind::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(run(&editor), "Hi", "the draft is on the canvas");
        pointer.text_edit(&mut editor, tools::TextEdit::Backspace);
        assert_eq!(run(&editor), "H");

        // Confirm ends the run, lands the whole typed draft as ONE history
        // entry, and the keyboard goes back to the shortcut table.
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert!(out.had_pending);
        assert_eq!(out.steps, 1, "confirm is one undoable entry: {out:?}");
        assert!(!pointer.is_text_editing());
        assert_eq!(
            editor.active().unwrap().history_depth(),
            depth_after_create + 1
        );
        assert!(
            !pointer
                .text_edit(&mut editor, tools::TextEdit::Insert("x"))
                .had_pending,
            "a keystroke was consumed after the run ended"
        );
        assert_eq!(run(&editor), "H");
    }

    /// Card 025's cancel semantics: Escape on a click-created run removes the
    /// layer — no stray layer survives — and an entered layer would be
    /// restored without a history entry (exercised with the create case here;
    /// the restore path is the same history-free draft route).
    #[test]
    fn cancelling_a_type_run_leaves_no_stray_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        let depth_after_create = editor.active().unwrap().history_depth();
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("d"));
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("r"));
        assert_eq!(
            editor.active().unwrap().history_depth(),
            depth_after_create,
            "the draft never touched history"
        );

        let out = pointer.text_edit(&mut editor, tools::TextEdit::Cancel);
        assert!(out.had_pending);
        assert!(!pointer.is_text_editing());
        let doc = editor.active().unwrap();
        let text_layers = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .filter(|l| matches!(l.kind, layer_model::LayerKind::Text(_)))
            .count();
        assert_eq!(text_layers, 0, "no stray layer survives the cancel");
        assert_eq!(
            doc.history_depth(),
            depth_after_create + 1,
            "the cancellation is the one delete step"
        );
    }

    /// Card 025: one undo after a confirmed run restores the pre-session
    /// payload byte-for-byte — the confirm entry's inverse is the ORIGINAL,
    /// not the already-rendered draft.
    #[test]
    fn undo_after_a_confirmed_run_takes_the_whole_session_back() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        let depth_after_create = editor.active().unwrap().history_depth();
        for ch in ["H", "i"] {
            pointer.text_edit(&mut editor, tools::TextEdit::Insert(ch));
        }
        let confirmed = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert_eq!(confirmed.steps, 1, "confirm is one entry: {confirmed:?}");

        editor.active_mut().unwrap().undo().unwrap();
        let run = |editor: &Editor| {
            let doc = editor.active().unwrap();
            doc.document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter_map(|id| doc.document.layers.get(id))
                .find_map(|l| match &l.kind {
                    layer_model::LayerKind::Text(t) => Some(t.text.clone()),
                    _ => None,
                })
                .unwrap()
        };
        assert_eq!(run(&editor), "", "one undo is the whole session back");
        assert_eq!(editor.active().unwrap().history_depth(), depth_after_create);
        editor.active_mut().unwrap().redo().unwrap();
        assert_eq!(run(&editor), "Hi", "redo brings the confirmed draft back");
    }

    /// Card 025, round-2 N1: a second Type click CONFIRMS the outgoing run —
    /// both layers keep their typed text and every keystroke is accounted
    /// for by exactly one history entry.
    #[test]
    fn a_second_type_click_confirms_the_first_run_rather_than_stranding_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("one"));
        let depth_after_one = editor.active().unwrap().history_depth();
        assert_eq!(
            depth_after_one, 1,
            "create only — the draft is history-free"
        );

        // The second click: the first run confirms (one entry), the second
        // layer is created (one entry), and the new run starts empty.
        stroke(&mut pointer, &mut editor, &[(60.0, 60.0)]);
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Insert("two"));
        assert!(out.had_pending);
        let doc = editor.active().unwrap();
        let texts: Vec<String> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .filter_map(|l| match &l.kind {
                layer_model::LayerKind::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts.len(), 2, "two text layers");
        assert!(
            texts.iter().any(|t| t == "one") && texts.iter().any(|t| t == "two"),
            "both runs keep their typed text: {texts:?}"
        );
        assert_eq!(
            doc.history_depth(),
            depth_after_one + 2,
            "confirm(one) + create(two): {texts:?}"
        );
    }

    /// Card 025, round-2 N3: locking the layer mid-session makes confirm
    /// refuse cleanly — the draft stays visible, history does not move, and
    /// the reported steps are the truth (zero).
    #[test]
    fn confirming_a_mid_session_locked_layer_preserves_the_draft_and_reports_zero_steps() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("draft"));

        // The user locks the layer from the Layers panel mid-session.
        let layer = editor
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                matches!(
                    editor
                        .active()
                        .unwrap()
                        .document
                        .layers
                        .get(*id)
                        .unwrap()
                        .kind,
                    layer_model::LayerKind::Text(_)
                )
            })
            .unwrap();
        editor.apply_command(Command::SetLayerProperties {
            layer_id: layer,
            patch: editor_core::LayerPatch {
                locked: Some(layer_model::LockState {
                    all: true,
                    ..layer_model::LockState::default()
                }),
                ..editor_core::LayerPatch::default()
            },
        });
        let depth_after_lock = editor.active().unwrap().history_depth();

        let out = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert_eq!(out.steps, 0, "history never moved: {out:?}");
        assert!(out.failed.is_some() || editor.status().is_some());
        assert!(!pointer.is_text_editing(), "the session still ends");
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(t) = &doc.document.layers.get(layer).unwrap().kind else {
            panic!("the layer is text");
        };
        assert_eq!(
            t.text, "draft",
            "the draft stays visible, not stranded invisibly"
        );
        assert_eq!(doc.history_depth(), depth_after_lock);
    }

    /// Card 026's check: clicking the middle of a transformed headline edits
    /// THAT layer — no new layer, caret where the shaped hit test said, and
    /// the session's confirm is one entry on the existing layer.
    #[test]
    fn a_type_click_on_a_transformed_headline_enters_that_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();

        // The headline, placed at (20, 20) through the command route.
        let headline = {
            let layer = layer_model::Layer::with_kind(
                "Headline",
                layer_model::LayerKind::Text(layer_model::TextLayer {
                    text: "THUMBNAILS".to_string(),
                    font_family: "DejaVu Sans".to_string(),
                    size_px: 32.0,
                    ..layer_model::TextLayer::default()
                }),
            );
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            editor.apply_command(Command::SetLayerProperties {
                layer_id: id,
                patch: editor_core::LayerPatch {
                    transform: Some([1.0, 0.0, 0.0, 1.0, 20.0, 20.0]),
                    ..editor_core::LayerPatch::default()
                },
            });
            id
        };
        let depth = editor.active().unwrap().history_depth();

        // The expected caret, computed through the same facade the shell
        // uses: document (30, 40) is layer-local (10, 20) — mid-line inside
        // the transformed headline.
        let run = text_engine::TextRun::from(&layer_model::TextLayer {
            text: "THUMBNAILS".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 32.0,
            ..layer_model::TextLayer::default()
        });
        let expected_caret = compositor::text_hit_index(&run, 10.0, 20.0).expect("inside hits");

        stroke(&mut pointer, &mut editor, &[(30.0, 40.0)]);
        assert!(
            pointer.is_text_editing(),
            "the click entered the headline instead of starting a create"
        );
        {
            let doc = editor.active().unwrap();
            assert_eq!(doc.history_depth(), depth, "entering commits nothing");
            let text_layers = doc
                .document
                .layers
                .iter_depth_first()
                .into_iter()
                .filter(|id| {
                    matches!(
                        doc.document.layers.get(*id).unwrap().kind,
                        layer_model::LayerKind::Text(_)
                    )
                })
                .count();
            assert_eq!(text_layers, 1, "no new layer behind the click");
        }

        // Typing lands at the hit caret on THAT layer, history-free; confirm
        // is one entry on the existing layer.
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("X"));
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert_eq!(out.steps, 1, "{out:?}");
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(t) = &doc.document.layers.get(headline).unwrap().kind
        else {
            panic!("the headline is still text");
        };
        let mut expected = "THUMBNAILS".to_string();
        expected.insert(expected_caret, 'X');
        assert_eq!(t.text, expected, "the caret is the hit test's answer");
        assert_eq!(doc.history_depth(), depth + 1);
    }

    /// Card 026's hit policy, half one: a click that misses every text
    /// layer's ink creates a new layer, exactly as before.
    #[test]
    fn a_type_click_off_the_ink_still_creates_a_new_layer() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        let layer = layer_model::Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        editor.apply_command(Command::create_layer(layer));
        let depth = editor.active().unwrap().history_depth();

        // (40, 63) is inside the 64x64 canvas but below the headline's ink
        // and its half-line-height padding (the block ends near y=57.6).
        stroke(&mut pointer, &mut editor, &[(40.0, 63.0)]);
        let doc = editor.active().unwrap();
        let text_layers = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                matches!(
                    doc.document.layers.get(*id).unwrap().kind,
                    layer_model::LayerKind::Text(_)
                )
            })
            .count();
        assert_eq!(text_layers, 2, "the miss created a layer");
        assert_eq!(doc.history_depth(), depth + 1);
        assert!(pointer.is_text_editing());
    }

    /// Card 026's hit policy, half two: a blanket-locked headline is skipped
    /// for entering — the click falls through to a new layer.
    #[test]
    fn a_locked_headline_is_skipped_for_entering() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        let layer = layer_model::Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let locked = layer.id;
        editor.apply_command(Command::create_layer(layer));
        editor.apply_command(Command::SetLayerProperties {
            layer_id: locked,
            patch: editor_core::LayerPatch {
                locked: Some(layer_model::LockState {
                    all: true,
                    ..layer_model::LockState::default()
                }),
                ..editor_core::LayerPatch::default()
            },
        });

        // The click lands inside the locked headline's ink; the layer is
        // skipped, so the click creates a fresh layer instead.
        stroke(&mut pointer, &mut editor, &[(30.0, 40.0)]);
        let doc = editor.active().unwrap();
        let text_layers = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                matches!(
                    doc.document.layers.get(*id).unwrap().kind,
                    layer_model::LayerKind::Text(_)
                )
            })
            .count();
        assert_eq!(text_layers, 2, "the locked layer was not entered");
        // Behavioral proof the session is on the NEW layer: typing + confirm
        // lands there, and the locked headline keeps its payload verbatim.
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("new"));
        pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        let doc = editor.active().unwrap();
        for id in doc.document.layers.iter_depth_first() {
            let layer_model::LayerKind::Text(t) = &doc.document.layers.get(id).unwrap().kind else {
                continue;
            };
            if id == locked {
                assert_eq!(t.text, "THUMBNAILS", "the locked layer was not edited");
            } else {
                assert_eq!(t.text, "new", "the typed draft landed on the new layer");
            }
        }
    }

    /// Card 026: a text layer inside a transformed GROUP is entered through
    /// the composed ancestor transform — the click lands on the child, and
    /// the typed draft reaches the child through the group.
    #[test]
    fn a_type_click_enters_a_text_layer_inside_a_transformed_group() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();

        // The child, then a group that takes it in, then the group's own
        // translate — the click's coordinates go through BOTH.
        let child = {
            let layer = layer_model::Layer::with_kind(
                "Grouped",
                layer_model::LayerKind::Text(layer_model::TextLayer {
                    text: "GROUPED".to_string(),
                    font_family: "DejaVu Sans".to_string(),
                    size_px: 24.0,
                    ..layer_model::TextLayer::default()
                }),
            );
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            id
        };
        let group = {
            let layer = layer_model::Layer::with_kind(
                "Group",
                layer_model::LayerKind::Group(layer_model::GroupLayer {
                    children: vec![],
                    collapsed: false,
                    blending: Default::default(),
                }),
            );
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            editor.apply_command(Command::MoveLayer {
                layer_id: child,
                parent: Some(id),
                index: 0,
            });
            editor.apply_command(Command::SetLayerProperties {
                layer_id: id,
                patch: editor_core::LayerPatch {
                    transform: Some([1.0, 0.0, 0.0, 1.0, 40.0, 0.0]),
                    ..editor_core::LayerPatch::default()
                },
            });
            assert!(
                editor.active().unwrap().document.layers.get(id).is_some(),
                "the group survived its own setup"
            );
            assert!(
                editor
                    .active()
                    .unwrap()
                    .document
                    .layers
                    .get(child)
                    .is_some(),
                "the child survived the group setup"
            );
            id
        };

        // Document (50, 15) = child-local (10, 15) — inside "GROUPED"'s ink.
        stroke(&mut pointer, &mut editor, &[(50.0, 15.0)]);
        assert!(pointer.is_text_editing(), "the grouped layer was entered");
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("X"));
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert_eq!(out.steps, 1, "{out:?}");
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(t) = &doc.document.layers.get(child).unwrap().kind else {
            panic!("the child is still text");
        };
        assert_eq!(t.text.len(), 8, "the X landed in the grouped child");
        assert!(t.text.contains('X'));
        assert!(doc.document.layers.get(group).is_some());
        // The child is still the group's child — the session did not
        // restructure the tree.
        let layer_model::LayerKind::Group(g) = &doc.document.layers.get(group).unwrap().kind else {
            panic!("the group is a group");
        };
        assert_eq!(g.children, vec![child]);
    }

    /// Card 026: the run lives in its own document — switching tabs refuses
    /// text operations against the wrong document instead of corrupting it,
    /// and switching back resumes the run.
    #[test]
    fn text_ops_refuse_when_the_run_lives_in_another_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        stroke(&mut pointer, &mut editor, &[(8.0, 8.0)]);
        let run_doc_depth = editor.active().unwrap().history_depth();

        // A second document becomes active.
        let second = dir.path().join("second.png");
        std::fs::write(
            &second,
            raster::encode(
                raster::ExportFormat::Png,
                W,
                H,
                &[240u8; (W * H * 4) as usize],
            )
            .expect("the second canvas encodes"),
        )
        .expect("the second canvas writes");
        editor.open_path(&second).expect("the second doc opens");
        assert_eq!(editor.active().unwrap().history_depth(), 0);

        // Typing is consumed and lands NOWHERE.
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Insert("x"));
        assert!(out.had_pending, "the run still owns the keyboard");
        assert_eq!(out.steps, 0);
        assert_eq!(
            editor.active().unwrap().history_depth(),
            0,
            "no draft on the wrong doc"
        );
        assert_eq!(
            editor.active().unwrap().history_depth(),
            0,
            "no draft on the wrong doc"
        );
        // The second document (opened from the PNG) carries its own imported
        // canvas layer — the run added nothing to it.
        let second_layers = editor
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .len();
        let second_texts = editor
            .active()
            .unwrap()
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter(|id| {
                matches!(
                    editor
                        .active()
                        .unwrap()
                        .document
                        .layers
                        .get(*id)
                        .unwrap()
                        .kind,
                    layer_model::LayerKind::Text(_)
                )
            })
            .count();
        assert_eq!(
            second_texts, 0,
            "no text layer was created on the wrong document"
        );
        assert!(
            second_layers <= 1,
            "the second document gained nothing from the run: {second_layers}"
        );

        // Switching back resumes the run on its own document.
        editor.activate(0).unwrap();
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("kept"));
        let doc = editor.active().unwrap();
        let texts: Vec<String> = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .filter_map(|l| match &l.kind {
                layer_model::LayerKind::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            texts,
            vec!["kept".to_string()],
            "the run resumed in its document"
        );
        assert_eq!(
            doc.history_depth(),
            run_doc_depth,
            "still history-free while typing"
        );
    }

    /// A pen click sequence builds the path those clicks describe, and Enter
    /// turns it into one shape layer.
    #[test]
    fn a_pen_click_sequence_builds_the_expected_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pen);
        let mut pointer = ToolPointer::new();

        for at in [(10.0, 10.0), (50.0, 10.0), (50.0, 40.0)] {
            stroke(&mut pointer, &mut editor, &[at]);
            // Nothing is emitted while the path is being drawn.
            assert_eq!(editor.active().unwrap().history_depth(), 0);
        }
        assert!(pointer.has_pending_commit());

        let outcome = pointer.commit(&mut editor);
        assert_eq!(outcome.steps, 1, "{outcome:?}");
        let doc = editor.active().unwrap();
        let shape = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .find_map(|l| match &l.kind {
                layer_model::LayerKind::Shape(s) => Some(s.clone()),
                _ => None,
            })
            .expect("the pen created no shape layer");

        let path = vector::parse_svg(&shape.path_svg).expect("the pen wrote unreadable SVG");
        assert_eq!(
            path.elements(),
            &[
                vector::PathEl::MoveTo(vector::point(10.0, 10.0)),
                vector::PathEl::LineTo(vector::point(50.0, 10.0)),
                vector::PathEl::LineTo(vector::point(50.0, 40.0)),
            ],
            "the pen authored a path the clicks do not describe"
        );
        // Left open, so it is stroked rather than filled.
        assert!(shape.stroke.is_some() && shape.fill.is_none());
    }

    /// Clicking back on the first anchor closes the path and publishes it with
    /// no Enter at all.
    #[test]
    fn a_pen_click_on_the_first_anchor_closes_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pen);
        editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        for at in [(10.0, 10.0), (50.0, 10.0), (50.0, 40.0), (11.0, 11.0)] {
            stroke(&mut pointer, &mut editor, &[at]);
        }
        assert_eq!(editor.active().unwrap().history_depth(), 1);
        assert!(!pointer.has_pending_commit(), "the path was not published");

        let doc = editor.active().unwrap();
        let shape = doc
            .document
            .layers
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| doc.document.layers.get(id))
            .find_map(|l| match &l.kind {
                layer_model::LayerKind::Shape(s) => Some(s.clone()),
                _ => None,
            })
            .expect("no shape layer");
        assert!(
            shape.path_svg.trim_end().ends_with('Z'),
            "the closed path did not close: {}",
            shape.path_svg
        );
        assert_eq!(
            shape.fill,
            Some([1.0, 0.0, 0.0, 1.0]),
            "a closed path is filled in the foreground colour"
        );
    }

    /// A crop of the whole canvas is still a crop, and a request that describes
    /// no canvas is refused rather than applied as a zero-sized document.
    #[test]
    fn a_crop_command_is_built_only_for_a_region_with_area() {
        let dir = tempfile::tempdir().unwrap();
        let editor = editor(dir.path());
        let document = &editor.active().unwrap().document;
        let empty = tools::CropRequest {
            rect: PixelRect::new(0, 0, 0, 10),
            straighten: 0.0,
            delete_cropped: false,
        };
        assert!(crop_command(document, &empty).is_none());

        // A crop at the origin needs no translation at all.
        let at_origin = tools::CropRequest {
            rect: PixelRect::new(0, 0, 32, 32),
            straighten: 0.0,
            delete_cropped: false,
        };
        let Some(Command::Transaction { commands, .. }) = crop_command(document, &at_origin) else {
            panic!("a crop at the origin built no transaction");
        };
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert!(matches!(commands[0], Command::SetCanvasSize { .. }));

        // ...and one away from it moves each root layer once.
        let moved = tools::CropRequest {
            rect: PixelRect::new(4, 6, 32, 32),
            straighten: 0.0,
            delete_cropped: false,
        };
        let Some(Command::Transaction { commands, .. }) = crop_command(document, &moved) else {
            panic!("no transaction");
        };
        assert_eq!(commands.len(), 1 + document.layers.root().len());
        assert!(matches!(
            commands[1],
            Command::TransformLayer {
                matrix: [1.0, 0.0, 0.0, 1.0, -4.0, -6.0],
                ..
            }
        ));
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

    /// Card 026: the double-click route begins an entered session on the
    /// EXISTING layer — caret at the end of the run, cancel restores the
    /// original (created_layer is false), and no new layer ever appears.
    #[test]
    fn the_double_click_intent_enters_the_existing_layer_at_the_run_end() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        editor.apply_command(Command::create_layer(layer));
        let depth = editor.active().unwrap().history_depth();

        let mut pointer = ToolPointer::new();
        pointer.enter_text_session(&mut editor, id);
        assert!(pointer.is_text_editing(), "the session entered the layer");
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("!"));
        let out = pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        assert_eq!(out.steps, 1, "{out:?}");
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(t) = &doc.document.layers.get(id).unwrap().kind else {
            panic!("the layer is text");
        };
        assert_eq!(t.text, "THUMBNAILS!", "the caret sat at the run's end");
        assert_eq!(doc.history_depth(), depth + 1);

        // Cancel semantics for an entered layer: the original comes back,
        // history-free — card 025's entered-layer machinery behind the new
        // route.
        editor.set_tool(ToolId::Type);
        pointer.enter_text_session(&mut editor, id);
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("XX"));
        pointer.text_edit(&mut editor, tools::TextEdit::Cancel);
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(restored) = &doc.document.layers.get(id).unwrap().kind
        else {
            panic!("the layer is text");
        };
        assert_eq!(
            restored.text, "THUMBNAILS!",
            "cancel restored the confirmed payload"
        );
        assert_eq!(doc.history_depth(), depth + 1, "the cancel added no entry");
    }

    /// Card 026's overlap policy, pinned: only TEXT layers are candidates and
    /// they are walked top-most first — a raster portrait above or below
    /// never blocks entering the text, and of two overlapping headlines the
    /// top-most wins.
    #[test]
    fn the_overlap_policy_is_top_most_text_and_rasters_never_block() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();

        // Two overlapping headlines; a raster "portrait" is created LAST, so
        // it is top-most of all and covers the same area.
        let headline = {
            let layer = layer_model::Layer::with_kind(
                "Headline",
                layer_model::LayerKind::Text(layer_model::TextLayer {
                    text: "THUMBNAILS".to_string(),
                    font_family: "DejaVu Sans".to_string(),
                    size_px: 32.0,
                    ..layer_model::TextLayer::default()
                }),
            );
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            id
        };
        let subhead = {
            let layer = layer_model::Layer::with_kind(
                "Subhead",
                layer_model::LayerKind::Text(layer_model::TextLayer {
                    text: "SUB".to_string(),
                    font_family: "DejaVu Sans".to_string(),
                    size_px: 32.0,
                    ..layer_model::TextLayer::default()
                }),
            );
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            id
        };
        let portrait = {
            let layer = layer_model::Layer::raster("Portrait");
            let id = layer.id;
            editor.apply_command(Command::create_layer(layer));
            id
        };
        let _ = portrait;

        // The click lands where BOTH headlines' ink overlaps. The top-most
        // TEXT candidate is the subhead (created after the headline), and the
        // raster above them does not block.
        stroke(&mut pointer, &mut editor, &[(10.0, 16.0)]);
        assert!(
            pointer.is_text_editing(),
            "the text under the portrait is entered"
        );
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("!"));
        let doc = editor.active().unwrap();
        let layer_model::LayerKind::Text(sub) = &doc.document.layers.get(subhead).unwrap().kind
        else {
            panic!("the subhead is text");
        };
        // The click's caret was mid-glyph (10px into a 32px "S"), so the
        // insertion sits inside the word — mid-string placement through the
        // transform is the point being proven.
        assert_eq!(sub.text, "S!UB", "the TOP-MOST text won at the hit caret");
        let layer_model::LayerKind::Text(head) = &doc.document.layers.get(headline).unwrap().kind
        else {
            panic!("the headline is text");
        };
        assert_eq!(head.text, "THUMBNAILS", "the lower text was not entered");
    }

    /// Card 026: a HIDDEN text layer is skipped like a locked one — a click
    /// on invisible ink falls through to layer creation.
    #[test]
    fn a_hidden_text_layer_is_skipped_for_entering() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Type);
        let mut pointer = ToolPointer::new();
        let layer = layer_model::Layer::with_kind(
            "Hidden",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "INVISIBLE".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let hidden = layer.id;
        editor.apply_command(Command::create_layer(layer));
        editor.apply_command(Command::SetLayerProperties {
            layer_id: hidden,
            patch: editor_core::LayerPatch {
                visible: Some(false),
                ..editor_core::LayerPatch::default()
            },
        });

        // The click lands inside the hidden layer's ink; it is skipped, so a
        // fresh layer is created and the hidden one keeps its payload.
        stroke(&mut pointer, &mut editor, &[(10.0, 16.0)]);
        pointer.text_edit(&mut editor, tools::TextEdit::Insert("new"));
        pointer.text_edit(&mut editor, tools::TextEdit::Confirm);
        let doc = editor.active().unwrap();
        for id in doc.document.layers.iter_depth_first() {
            let layer_model::LayerKind::Text(t) = &doc.document.layers.get(id).unwrap().kind else {
                continue;
            };
            if id == hidden {
                assert_eq!(t.text, "INVISIBLE", "the hidden layer was not entered");
            } else {
                assert_eq!(t.text, "new", "the click created and entered a fresh layer");
            }
        }
    }

    #[cfg(test)]
    mod text_overlay_tests {
        use super::*;

        #[test]
        fn a_caret_rect_collapses_to_a_bar_and_a_selection_rect_yields_four_edges() {
            let caret = text_engine::Rect {
                x: 12.0,
                y: 4.0,
                width: 0.0,
                height: 20.0,
            };
            let mut segments = Vec::new();
            push_rect_edges(&mut segments, caret, TextOverlaySegment::CARET);
            // All four edges exist but the horizontal ones are degenerate
            // (w=0): the two vertical edges are the same bar, overdrawn.
            let visible: Vec<_> = segments
                .iter()
                .filter(|segment| (segment.b - segment.a).length() > 0.5)
                .collect();
            assert_eq!(
                visible.len(),
                2,
                "the zero-width caret is one bar, drawn twice"
            );
            for segment in &visible {
                // Direction varies with the edge winding; the bar's extent
                // does not.
                let (lo, hi) = if segment.a.y <= segment.b.y {
                    (segment.a, segment.b)
                } else {
                    (segment.b, segment.a)
                };
                assert_eq!(lo, Vec2::new(12.0, 4.0));
                assert_eq!(hi, Vec2::new(12.0, 24.0));
                assert_eq!(segment.kind, TextOverlayKind::Caret);
            }

            let selection = text_engine::Rect {
                x: 2.0,
                y: 4.0,
                width: 10.0,
                height: 20.0,
            };
            let mut edges = Vec::new();
            push_rect_edges(&mut edges, selection, TextOverlaySegment::SELECTION);
            assert_eq!(edges.len(), 4, "a selection rectangle is four edges");
            assert!(edges
                .iter()
                .all(|segment| segment.kind == TextOverlayKind::Selection));
            // The loop is closed: every corner is touched by exactly two segments.
            for corner in [
                Vec2::new(2.0, 4.0),
                Vec2::new(12.0, 4.0),
                Vec2::new(12.0, 24.0),
                Vec2::new(2.0, 24.0),
            ] {
                let touches = edges
                    .iter()
                    .filter(|segment| segment.a == corner || segment.b == corner)
                    .count();
                assert_eq!(touches, 2, "corner {corner:?} is shared by two edges");
            }
        }

        #[test]
        fn the_publisher_needs_a_live_session_and_shapes_the_document_layer() {
            let dir = tempfile::tempdir().unwrap();
            let mut pointer = ToolPointer::default();
            let editor = editor(dir.path());
            // No session: empty geometry, no panic.
            assert!(pointer.text_overlay_geometry(&editor).is_empty());
        }
    }

    #[test]
    fn the_publisher_shapes_a_live_sessions_caret_and_selection_in_document_space() {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let layer = layer_model::Layer::with_kind(
            "Headline",
            layer_model::LayerKind::Text(layer_model::TextLayer {
                text: "THUMBNAILS".to_string(),
                font_family: "DejaVu Sans".to_string(),
                size_px: 32.0,
                ..layer_model::TextLayer::default()
            }),
        );
        let id = layer.id;
        editor.apply_command(Command::create_layer(layer));
        let mut pointer = ToolPointer::new();
        pointer.enter_text_session(&mut editor, id);
        assert!(pointer.is_text_editing());

        // The caret bar exists in document space; it is zoom-independent —
        // the SAME geometry at 1x and 2x (the shell maps the camera once per
        // endpoint, so the caret agrees with rendering at every zoom).
        let at_one = pointer.text_overlay_geometry(&editor);
        assert!(
            at_one
                .iter()
                .any(|segment| segment.kind == TextOverlayKind::Caret),
            "the caret bar is published"
        );
        editor.active_mut().unwrap().camera.zoom = 2.0;
        let at_two = pointer.text_overlay_geometry(&editor);
        assert_eq!(at_one, at_two, "document-space geometry ignores zoom");

        // Selecting a range publishes selection edges alongside the caret.
        pointer.text_edit(&mut editor, tools::TextEdit::SelectAll);
        let selected = pointer.text_overlay_geometry(&editor);
        assert!(
            selected
                .iter()
                .any(|segment| segment.kind == TextOverlayKind::Selection),
            "the selection publishes its rectangle edges"
        );
        assert!(selected
            .iter()
            .any(|segment| segment.kind == TextOverlayKind::Caret));

        // Card 032: converting to a box publishes the box's frame edges —
        // the resize handles' home — alongside everything else.
        pointer.text_edit(
            &mut editor,
            tools::TextEdit::ResizeBox {
                width: 120.0,
                height: None,
            },
        );
        let boxed = pointer.text_overlay_geometry(&editor);
        assert!(
            boxed
                .iter()
                .any(|segment| segment.kind == TextOverlayKind::BoxFrame),
            "the box frame is published"
        );
    }

    #[test]
    fn the_preview_matches_the_commit_for_an_already_transformed_layer() {
        // Card 035 round-2: with own != I, the preview must compose
        // P⁻¹·Δ·P·own so it agrees with the commit's placement — no
        // teleport-and-snap on a second gesture.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::FreeTransform);
        {
            let doc = editor.active_mut().unwrap();
            let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
            for y in 24..40u32 {
                for x in 24..40u32 {
                    let i = ((y * 256 + x) * 4) as usize;
                    bytes[i..i + 4].copy_from_slice(&[10, 10, 10, 255]);
                }
            }
            let hash = doc.tiles.insert_bytes(bytes);
            let layer = doc.document.active_layer().unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(layer),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
                )
                .unwrap(),
            )
            .unwrap();
            // Move the layer first: own != I now.
            doc.apply(Command::TransformLayer {
                layer_id: layer,
                matrix: [1.0, 0.0, 0.0, 1.0, 16.0, 8.0],
            })
            .unwrap();
        }
        let mut pointer = ToolPointer::new();

        // The ink now lives at (40,32)..(56,48). Grab deep inside it and
        // drag by (+8, +6).
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(48.0, 40.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(56.0, 46.0)),
            false,
            &[],
        );
        pointer.settle_preview(&mut editor);
        let previewed = composite(&mut editor);
        let at = |buf: &[u8], x: f32, y: f32| {
            let (x, y) = (x as usize, y as usize);
            let i = (y * 64 + x) * 4;
            [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
        };
        assert_eq!(
            at(&previewed, 52.0, 44.0),
            [10, 10, 10, 255],
            "the preview moved the transformed layer's ink"
        );
        assert_eq!(
            at(&previewed, 46.0, 38.0),
            [0, 0, 0, 0],
            "the preview did not leave the ink at its pre-drag spot"
        );

        // Commit: the layer transform lands where the preview showed it.
        pointer.commit(&mut editor);
        let committed = composite(&mut editor);
        assert_eq!(
            at(&committed, 52.0, 44.0),
            [10, 10, 10, 255],
            "the commit matches the preview"
        );
        assert_eq!(
            at(&committed, 46.0, 38.0),
            [0, 0, 0, 0],
            "the commit matches the preview (old spot empty)"
        );
    }

    #[test]
    fn painting_a_moved_layer_marks_the_displayed_pointer_location() {
        // Card 040's done-check: a moved raster layer is painted where the
        // pointer displays, and undo restores the original source hashes.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        // An ink patch on its own layer at (24..40, 24..40), then MOVE the
        // layer +32 in x: the ink now displays at (56..72).
        let layer = layer_model::Layer::raster("Ink");
        let ink_id = layer.id;
        {
            let doc = editor.active_mut().unwrap();
            let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
            for y in 24..40usize {
                for x in 24..40usize {
                    let i = (y * 256 + x) * 4;
                    bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
                }
            }
            let hash = doc.tiles.insert_bytes(bytes);
            doc.apply(Command::create_layer(layer)).unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(ink_id),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
                )
                .unwrap(),
            )
            .unwrap();
            doc.apply(Command::TransformLayer {
                layer_id: ink_id,
                matrix: [1.0, 0.0, 0.0, 1.0, 32.0, 0.0],
            })
            .unwrap();
        }
        let hashes_before = {
            let doc = editor.active().unwrap();
            doc.document.layer_tiles(ink_id).cloned()
        };

        // Paint at the DISPLAYED location (56, 32) — inside the moved ink.
        editor.set_layer_selection(vec![ink_id], Some(ink_id));
        editor.set_tool(ToolId::Brush);
        let mut pointer = ToolPointer::new();
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen(56.0, 32.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen(58.0, 32.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen(58.0, 32.0)),
            false,
            &[],
        );

        // The layer's OWN pixels gained ink at layer-local (24..40, 32) —
        // where the pointer pointed in layer space.
        let (hash, doc_tiles) = {
            let doc = editor.active().unwrap();
            (
                doc.document
                    .layer_tiles(ink_id)
                    .unwrap()
                    .get(TileCoord::new(0, 0, 0)),
                &doc.tiles,
            )
        };
        let bytes = hash.and_then(|h| doc_tiles.tile(h));
        let local = |x: usize, y: usize| {
            let i = (y * 256 + x) * 4;
            bytes.map(|b| [b[i], b[i + 1], b[i + 2], b[i + 3]])
        };

        // The displayed (57,32) maps to layer-local (25,32) through the
        // +32 move — the paint lands THERE, and layer-local (57,32) stays
        // clean (that would be the raw-document-coordinate bug this card
        // removes).
        assert_eq!(
            local(25, 32),
            Some([0, 0, 0, 255]),
            "the brush marked the DISPLAYED location in layer space"
        );
        assert_eq!(
            local(57, 32),
            Some([0, 0, 0, 0]),
            "no ink at the raw document coordinate in layer space"
        );
        // And the composite shows it at the displayed document location.
        let shown = composite(&mut editor);
        let shown_at = |x: usize, y: usize| {
            let i = (y * 64 + x) * 4;
            [shown[i], shown[i + 1], shown[i + 2], shown[i + 3]]
        };
        assert_eq!(
            shown_at(57, 32),
            [0, 0, 0, 255],
            "ink at the displayed doc spot"
        );
        // Undo restores the original source hashes.
        editor.active_mut().unwrap().undo().unwrap();
        assert_eq!(
            editor
                .active()
                .unwrap()
                .document
                .layer_tiles(ink_id)
                .cloned(),
            hashes_before,
            "undo restores the original source hashes"
        );
    }

    #[test]
    fn a_move_drag_snaps_the_moving_geometry_to_another_layers_edge() {
        // Card 042's done-check: the moving layer's edge lands on another
        // layer's edge at zoom 1, and its center lands on the canvas center
        // at zoom 2 — the snap is committed, not just displayed.
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let layer_a = layer_model::Layer::raster("A");
        let a_id = layer_a.id;
        let layer_b = layer_model::Layer::raster("B");
        let b_id = layer_b.id;
        {
            let doc = editor.active_mut().unwrap();
            let ink_a = {
                let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
                for y in 0..40usize {
                    for x in 0..40usize {
                        let i = (y * 256 + x) * 4;
                        bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
                    }
                }
                doc.tiles.insert_bytes(bytes)
            };
            let ink_b = {
                let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
                for y in 8..18usize {
                    for x in 62..72usize {
                        let i = (y * 256 + x) * 4;
                        bytes[i..i + 4].copy_from_slice(&[60, 10, 10, 255]);
                    }
                }
                doc.tiles.insert_bytes(bytes)
            };
            doc.apply(Command::create_layer(layer_a)).unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(a_id),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), ink_a)],
                )
                .unwrap(),
            )
            .unwrap();
            doc.apply(Command::create_layer(layer_b)).unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(b_id),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), ink_b)],
                )
                .unwrap(),
            )
            .unwrap();
        }
        editor.set_layer_selection(vec![a_id], Some(a_id));

        let screen_at = |doc_pt: Vec2, zoom: f32| -> Vec2 {
            let center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
            VIEWPORT * 0.5 + (doc_pt - center) * zoom
        };
        let bounds_of = |editor: &mut Editor, id: layer_model::LayerId| -> PixelRect {
            let doc = editor.active().unwrap();
            tight_document_bounds(&doc.document, &doc.tiles, id).expect("bounds")
        };

        editor.set_tool(ToolId::Move);
        let mut pointer = ToolPointer::new();
        // Zoom 1: drag A right by 21.6 — its right edge (61.6) is within
        // the 8pt threshold of B's left edge (62), so the commit is 22.
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen_at(Vec2::new(10.0, 10.0), 1.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen_at(Vec2::new(31.6, 10.0), 1.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen_at(Vec2::new(31.6, 10.0), 1.0)),
            false,
            &[],
        );
        let a = bounds_of(&mut editor, a_id);
        assert_eq!(
            a.x as f32 + a.width as f32,
            62.0,
            "A's right edge snapped onto B's left edge"
        );
        assert_eq!(a.y, 0, "the y axis did not drift");
        // B never moved: the snap adjusts the dragged layer only.
        let b = bounds_of(&mut editor, b_id);
        assert_eq!(b.x, 62, "the snap target stayed put");

        // Zoom 2 (threshold 4pt = 2 doc px): drag A (ink now 22..62) so its
        // CENTER misses the canvas center x=32 by 0.8 — the center feature
        // snaps exactly (delta -10.8 -> -10).
        editor.active_mut().unwrap().camera.zoom = 2.0;
        let mut pointer = ToolPointer::new();
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Down, screen_at(Vec2::new(40.0, 10.0), 2.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Move, screen_at(Vec2::new(29.2, 10.0), 2.0)),
            false,
            &[],
        );
        pointer.handle(
            &mut editor,
            sample(PointerPhase::Up, screen_at(Vec2::new(29.2, 10.0), 2.0)),
            false,
            &[],
        );
        let a = bounds_of(&mut editor, a_id);
        assert_eq!(
            a.x as f32 + a.width as f32 * 0.5,
            32.0,
            "A's center snapped onto the canvas center at zoom 2"
        );
    }

    #[test]
    fn tight_bounds_survive_ink_mapped_to_negative_document_coordinates() {
        // Card 042: dragging a layer off the canvas' left/top edge is an
        // ordinary state the Move tool itself creates — the mapped ink may
        // reach negative coordinates and the extent arithmetic must keep
        // the true width (no u32 saturation collapse).
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        let layer = layer_model::Layer::raster("Off");
        let off_id = layer.id;
        {
            let doc = editor.active_mut().unwrap();
            let mut bytes = vec![0u8; (256 * 256 * 4) as usize];
            for y in 0..20usize {
                for x in 0..20usize {
                    let i = (y * 256 + x) * 4;
                    bytes[i..i + 4].copy_from_slice(&[10, 60, 10, 255]);
                }
            }
            let hash = doc.tiles.insert_bytes(bytes);
            doc.apply(Command::create_layer(layer)).unwrap();
            doc.apply(
                Command::paint_tiles(
                    editor_core::PixelTarget::Layer(off_id),
                    vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
                )
                .unwrap(),
            )
            .unwrap();
            // Drag the ink to x ∈ [-40, -20), y ∈ [-30, -10): both edges
            // negative on x, only the origin negative on y.
            doc.apply(Command::TransformLayer {
                layer_id: off_id,
                matrix: [1.0, 0.0, 0.0, 1.0, -40.0, -30.0],
            })
            .unwrap();
        }
        let doc = editor.active().unwrap();
        let bounds = tight_document_bounds(&doc.document, &doc.tiles, off_id).expect("bounds");
        assert_eq!(bounds.x, -40, "the negative origin survives");
        assert_eq!(
            bounds.width, 20,
            "the width is the true extent, not a saturation artifact"
        );
        assert_eq!(bounds.y, -30, "the negative y origin survives");
        assert_eq!(bounds.height, 20, "the height is the true extent");
    }
}
