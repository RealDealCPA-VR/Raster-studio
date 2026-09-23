//! Move, crop, slice, eyedropper, red-eye, patch and magic eraser.
//!
//! The tools that are neither strokes nor selections nor shapes. They share
//! nothing but the command path, which is the point: every one of them ends a
//! gesture by producing a single [`Command`] (or, for crop and slice, a single
//! [`ToolRequest`] — see [`crate::tool::CropRequest`] for why those two cannot
//! be commands yet).

use color::{linear_srgb_luminance, linear_to_srgb, premultiply, unpremultiply};
use editor_core::{Command, PixelKey, Selection};
use filters::{blur::gaussian_blur, EdgeMode};
use glam::{IVec2, Vec2};
use layer_model::LayerId;
use raster::PixelRect;
use selection::{
    boolean::{combine, to_mask, BooleanOp},
    lasso::lasso_freehand,
    wand::{magic_wand, WandOptions},
    ImageView,
};

use crate::brush::BrushSettings;
use crate::error::{finite, ToolError};
use crate::patch::{read_rgba8, ColorPatch};
use crate::tool::{
    CropRequest, PointerEvent, Slice, Tool, ToolContext, ToolId, ToolRequest, ToolSetting,
};

fn unknown_option(key: &str) -> ToolError {
    ToolError::UnknownOption {
        key: key.to_owned(),
    }
}

fn kind_mismatch(key: &str) -> ToolError {
    ToolError::OptionKindMismatch {
        key: key.to_owned(),
    }
}

// ---------------------------------------------------------------- move ----

/// Where an aligned edge goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

/// The translation that aligns `item` to `target`.
pub fn align_offset(item: PixelRect, target: PixelRect, a: Alignment) -> Vec2 {
    let dx = match a {
        Alignment::Left => (target.x - item.x) as f32,
        Alignment::Right => (target.right() - item.right()) as f32,
        Alignment::HorizontalCenter => {
            ((target.x + target.right()) as f32 - (item.x + item.right()) as f32) * 0.5
        }
        _ => 0.0,
    };
    let dy = match a {
        Alignment::Top => (target.y - item.y) as f32,
        Alignment::Bottom => (target.bottom() - item.bottom()) as f32,
        Alignment::VerticalCenter => {
            ((target.y + target.bottom()) as f32 - (item.y + item.bottom()) as f32) * 0.5
        }
        _ => 0.0,
    };
    Vec2::new(dx, dy)
}

/// A pure translation as the six components `Command::TransformLayer` wants.
pub fn translation_matrix(d: Vec2) -> [f32; 6] {
    [1.0, 0.0, 0.0, 1.0, d.x, d.y]
}

/// Move: drag a layer, optionally picking the layer under the cursor first.
pub struct MoveTool {
    /// Grab whatever layer has an opaque pixel under the pointer instead of
    /// the one selected in the panel.
    pub auto_select: bool,
    /// Card 038: Layer/Group selection mode — when on, an auto-select pick
    /// climbs to the picked leaf's top-level ancestor group.
    pub select_groups: bool,
    /// How opaque a pixel has to be for auto-select to claim it.
    pub auto_select_threshold: f32,
    /// Whether transform controls are shown for the selected layer. The
    /// option is spec'd and forwarded (card 010); drawing the controls is
    /// cards 012/038's live-geometry work.
    pub show_transform: bool,
    /// Card 038: the selected layer's content bounds, cached on the last
    /// pointer contact so Show Transform Controls can draw its box without
    /// starting an edit session.
    display_bounds: Option<PixelRect>,
    /// Card 038: the layer the display box frames.
    display_layer: Option<LayerId>,
    /// Card 042: the dragged layer's ink at pointer-down — the rect the
    /// snap adjusts.
    base_bounds: Option<PixelRect>,
    start: Option<Vec2>,
    current: Vec2,
    layer: Option<LayerId>,
}

impl Default for MoveTool {
    fn default() -> Self {
        Self {
            auto_select: false,
            select_groups: false,
            auto_select_threshold: 0.5,
            show_transform: false,
            display_bounds: None,
            display_layer: None,
            base_bounds: None,
            start: None,
            current: Vec2::ZERO,
            layer: None,
        }
    }
}

impl MoveTool {
    /// The topmost layer in `ctx.layer_stack` with a sufficiently opaque pixel
    /// at `p`.
    pub fn layer_under(&self, ctx: &ToolContext<'_>, p: Vec2) -> Option<LayerId> {
        // Card 037: the shell's bounded visible-content pick is authoritative
        // whenever the shell ran — an inner `None` is a real "nothing visible
        // here" (a hidden or masked-out layer must not resurrect through the
        // raw sampler). The sampler below serves sessions begun without the
        // shell (direct tests).
        if let Some(pick) = ctx.content_pick {
            // Card 038: Group selection mode climbs the picked leaf to its
            // top-level ancestor group (the panel's group row); Layer mode
            // keeps the leaf.
            return match pick {
                Some(leaf) if self.select_groups => {
                    let mut top = leaf;
                    while let Some(parent) = ctx.parent_of(top) {
                        top = parent;
                    }
                    Some(top)
                }
                other => other,
            };
        }
        let pt = IVec2::new(p.x.floor() as i32, p.y.floor() as i32);
        let rect = PixelRect::new(pt.x as i64, pt.y as i64, 1, 1);
        for id in &ctx.layer_stack {
            let key = PixelKey::Layer(*id);
            if let Ok(patch) = ColorPatch::load(ctx.tiles, key, rect) {
                if patch.get(pt)[3] >= self.auto_select_threshold {
                    return Some(*id);
                }
            }
        }
        None
    }

    /// Emit the commands that align `layers` (each with its bounds) to
    /// `target`.
    pub fn align(
        ctx: &mut ToolContext<'_>,
        layers: &[(LayerId, PixelRect)],
        target: PixelRect,
        alignment: Alignment,
    ) {
        for (id, bounds) in layers {
            let d = align_offset(*bounds, target, alignment);
            if d == Vec2::ZERO {
                continue;
            }
            ctx.emit(Command::TransformLayer {
                layer_id: *id,
                matrix: translation_matrix(d),
            });
        }
    }
}

impl Tool for MoveTool {
    fn id(&self) -> ToolId {
        ToolId::Move
    }

    /// The typed forwarding seam (card 010): exactly the keys the registry
    /// declares for Move, each answering for its own kind. Anything else is
    /// refused loudly — an options-bar control that silently did nothing is
    /// the defect this seam exists to prevent.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("auto_select", ToolSetting::Bool(v)) => {
                self.auto_select = v;
                Ok(())
            }
            ("show_transform", ToolSetting::Bool(v)) => {
                self.show_transform = v;
                if !v {
                    // The box follows the option: off means no geometry.
                    self.display_bounds = None;
                    self.display_layer = None;
                }
                Ok(())
            }
            ("select_groups", ToolSetting::Bool(v)) => {
                self.select_groups = v;
                Ok(())
            }
            ("auto_select", _) | ("select_groups", _) | ("show_transform", _) => {
                Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                })
            }
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

    /// Card 038: Show Transform Controls displays the selected layer's box
    /// WITHOUT starting an edit — an identity transform state over the
    /// cached ink bounds. No session, no commands, no history.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        if !self.show_transform {
            return None;
        }
        let bounds = self.display_bounds?;
        let layer = self.display_layer?;
        let state = crate::transform::TransformState::new(bounds);
        Some(crate::tool::SessionGeometry::Transform {
            state,
            mode: crate::transform::TransformMode::Scale,
            active: None,
            layer: Some(layer),
        })
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("move start", event.pos)?;
        self.start = Some(event.pos);
        self.current = event.pos;
        self.layer = if self.auto_select {
            self.layer_under(ctx, event.pos).or(ctx.active_layer)
        } else {
            ctx.active_layer
        };
        // Card 042: the snap reference rect — the dragged layer's TIGHT ink
        // at pointer-down (tile-level bounds as the fallback).
        self.base_bounds = ctx
            .active_layer_ink_bounds
            .or(ctx.active_layer_content_bounds);
        // Card 038: a canvas click updates the highlighted layer row — the
        // auto-select pick (climbed in Group mode) becomes the selection,
        // without losing the rest of a Shift-selected set? No: auto-select
        // REPLACES the selection with the picked layer, matching the drag
        // semantics above.
        if self.auto_select {
            // The CLIMBED pick (Group mode resolves to the ancestor the
            // panel shows) — emitting the raw leaf would make the drag move
            // it twice inside a group transaction.
            if ctx.content_pick.is_some() {
                if let Some(target) = self.layer {
                    ctx.emit_request(crate::tool::ToolRequest::SelectLayer(target));
                }
            }
        }
        // Card 038: Show Transform Controls draws its box without starting
        // an edit — cache the framed layer's ink on every contact. The ink
        // bounds are known only for the ACTIVE layer, so the box frames the
        // pick only when the pick IS the active layer (otherwise no box
        // rather than a box around the wrong ink).
        if self.show_transform {
            self.display_bounds = if self.layer == ctx.active_layer {
                ctx.active_layer_content_bounds
            } else {
                None
            };
            self.display_layer = self.layer;
        }
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let (Some(start), true) = (
            self.start,
            event.pos.x.is_finite() && event.pos.y.is_finite(),
        ) {
            // Card 042: snap the moving geometry — the dragged rect's
            // edges/centers against the shell's candidates.
            let delta = event.pos - start;
            let snapped = if let Some(base) = self.base_bounds {
                crate::snap_delta(base, delta, &ctx.snap_candidates, ctx.snap_threshold_doc)
            } else {
                delta
            };
            self.current = start + snapped;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(start) = self.start.take() else {
            return Ok(());
        };
        let end = if event.modifiers.shift {
            // Constrain to the dominant axis.
            let d = event.pos - start;
            if d.x.abs() >= d.y.abs() {
                Vec2::new(event.pos.x, start.y)
            } else {
                Vec2::new(start.x, event.pos.y)
            }
        } else {
            event.pos
        };
        crate::error::finite_pt("move end", end)?;
        let layer = self.layer.take().ok_or(ToolError::NoActiveLayer)?;
        // Card 042: the COMMITTED delta is the snapped one — the same snap
        // the drag applied, recomputed at the release point (a no-Move
        // drag snaps here).
        let d = if let Some(base) = self.base_bounds.take() {
            crate::snap_delta(
                base,
                end - start,
                &ctx.snap_candidates,
                ctx.snap_threshold_doc,
            )
        } else {
            end - start
        };
        // A click that moved nothing is not an edit; emitting an identity
        // transform would put a do-nothing entry in the undo stack.
        if d.length() < 1e-4 {
            return Ok(());
        }
        // Card 038: with a multi-layer selection the WHOLE set moves — one
        // transaction (the shell conjugates per participant). The active
        // layer leads so the delta is defined even for an empty panel set.
        // Lock policy: locked participants are FILTERED here (they stay put;
        // the panel's lock badges are the user's cue), while a locked ACTIVE
        // layer re-enters below and the whole transaction refuses
        // all-or-nothing at apply.
        // Card 043: a linked participant carries the whole link chain —
        // expanded BEFORE the lock filter so locked chain members follow
        // the same stay-put rule as any locked participant.
        // Card 045: an ancestor-shadowed participant is dropped — a child
        // inside a moving group must not also move itself. The check is
        // ORDER-INDEPENDENT (the transform tool's rule): a candidate is
        // shadowed when ANY selected id sits on its ancestor chain, not
        // just the ones kept so far.
        let selected: Vec<LayerId> = ctx
            .selected_layers
            .iter()
            .copied()
            .filter(|id| {
                let mut parent = ctx.parent_of(*id);
                while let Some(p) = parent {
                    if ctx.selected_layers.contains(&p) {
                        return false;
                    }
                    parent = ctx.parent_of(p);
                }
                true
            })
            .collect();
        let mut participants: Vec<LayerId> = crate::with_link_chain(&selected, &ctx.linked_layers)
            .into_iter()
            .filter(|id| ctx.layer_lock(*id) != Some(true))
            .collect();
        // The active layer leads so the delta is defined even for an empty
        // panel set — but not when a kept ancestor already carries it (card
        // 045: the shadow rule outranks the leader rule).
        // The shadow check runs against the SURVIVING participants: a locked
        // ancestor was filtered out (card 036's stay-put rule), so its
        // unlocked child moves independently — the lock bars the group's own
        // transform, not the child's.
        let active_shadowed = participants.iter().any(|top| {
            let mut parent = ctx.parent_of(layer);
            while let Some(p) = parent {
                if p == *top {
                    break;
                }
                parent = ctx.parent_of(p);
            }
            parent.is_some()
        });
        if !participants.contains(&layer) && !active_shadowed {
            participants.insert(0, layer);
        }
        if participants.len() > 1 {
            ctx.emit_request(crate::tool::ToolRequest::TransformLayers {
                layers: participants,
                delta: [1.0, 0.0, 0.0, 1.0, d.x, d.y],
            });
            return Ok(());
        }
        // Card 045: the single commit targets the surviving participant —
        // when the shadow rule dropped the active layer in favor of its
        // ancestor, the ancestor is what the drag moves.
        let target = participants.first().copied().unwrap_or(layer);
        ctx.emit(Command::TransformLayer {
            layer_id: target,
            matrix: translation_matrix(d),
        });
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.start = None;
        self.layer = None;
        // Card 042: the snap reference dies with the gesture.
        self.base_bounds = None;
    }

    fn is_active(&self) -> bool {
        self.start.is_some()
    }
}

// ---------------------------------------------------------------- crop ----

/// Crop: drag a keep-region, constrain its shape, straighten, commit.
pub struct CropTool {
    /// Width divided by height the box is locked to, if any.
    pub aspect: Option<f32>,
    /// Rotation the crop asks for before the cut, radians clockwise.
    ///
    /// Reported, not performed: it rides along in the emitted
    /// [`CropRequest`], whose [`CropRequest::straightened_corners`] gives the
    /// quad it means. Actually resampling that quad needs the canvas-resize
    /// command `editor-core` does not have yet, which is the same reason a crop
    /// is a request rather than a command at all.
    pub straighten: f32,
    pub delete_cropped: bool,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    /// The committed box, once the drag has ended and before Enter.
    pub box_rect: Option<PixelRect>,
}

impl Default for CropTool {
    fn default() -> Self {
        Self {
            aspect: None,
            straighten: 0.0,
            delete_cropped: false,
            anchor: None,
            current: None,
            box_rect: None,
        }
    }
}

impl CropTool {
    /// Apply the aspect lock to a drag.
    fn constrained(&self, a: Vec2, b: Vec2) -> (Vec2, Vec2) {
        let Some(aspect) = self.aspect.filter(|r| r.is_finite() && *r > 0.0) else {
            return (a, b);
        };
        let d = b - a;
        // Keep whichever extent the user dragged further, and derive the other.
        let (w, h) = if (d.x.abs() / aspect) >= d.y.abs() {
            (d.x.abs(), d.x.abs() / aspect)
        } else {
            (d.y.abs() * aspect, d.y.abs())
        };
        (
            a,
            Vec2::new(
                a.x + w * if d.x < 0.0 { -1.0 } else { 1.0 },
                a.y + h * if d.y < 0.0 { -1.0 } else { 1.0 },
            ),
        )
    }

    /// The keep-region a drag describes, clipped to the canvas.
    pub fn rect_for(&self, ctx: &ToolContext<'_>, a: Vec2, b: Vec2) -> Option<PixelRect> {
        let (a, b) = self.constrained(a, b);
        if !a.x.is_finite() || !b.x.is_finite() || !a.y.is_finite() || !b.y.is_finite() {
            return None;
        }
        let x0 = (a.x.min(b.x).floor() as i64).max(ctx.canvas.x);
        let y0 = (a.y.min(b.y).floor() as i64).max(ctx.canvas.y);
        let x1 = (a.x.max(b.x).ceil() as i64).min(ctx.canvas.right());
        let y1 = (a.y.max(b.y).ceil() as i64).min(ctx.canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// Confirm the crop (Enter).
    pub fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let rect = self.box_rect.take().ok_or(ToolError::Degenerate)?;
        ctx.emit_request(ToolRequest::Crop(CropRequest {
            rect,
            straighten: self.straighten,
            delete_cropped: self.delete_cropped,
        }));
        Ok(())
    }
}

impl Tool for CropTool {
    fn id(&self) -> ToolId {
        ToolId::Crop
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("crop anchor", event.pos)?;
        self.anchor = Some(event.pos);
        self.current = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.anchor.is_some() {
            self.current = Some(event.pos);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(a) = self.anchor.take() else {
            return Ok(());
        };
        self.current = None;
        // Releasing sets the box; the crop itself waits for Enter, so the user
        // can nudge the edges first.
        self.box_rect = self.rect_for(ctx, a, event.pos);
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
        self.box_rect = None;
    }

    /// The registry's three Crop options. `aspect` is the bar's width/height
    /// ratio, where `0` means unconstrained; `straighten` and
    /// `delete_cropped` ride into the emitted [`CropRequest`].
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("aspect", ToolSetting::Float(v)) => {
                let v = finite("aspect ratio", v)?;
                self.aspect = (v > 0.0).then_some(v.min(100.0));
                Ok(())
            }
            ("straighten", ToolSetting::Float(v)) => {
                self.straighten = finite("straighten angle", v)?.clamp(-3.15, 3.15);
                Ok(())
            }
            ("delete_cropped", ToolSetting::Bool(v)) => {
                self.delete_cropped = v;
                Ok(())
            }
            ("aspect" | "straighten" | "delete_cropped", _) => Err(kind_mismatch(key)),
            _ => Err(unknown_option(key)),
        }
    }

    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        CropTool::commit(self, ctx)
    }

    fn has_pending_commit(&self) -> bool {
        self.box_rect.is_some()
    }

    fn is_active(&self) -> bool {
        self.anchor.is_some() || self.box_rect.is_some()
    }
}

// --------------------------------------------------------------- slice ----

/// Slice: drag out export regions.
///
/// # Contract: releasing the pointer publishes nothing
///
/// Each drag appends one region to the set [`SliceTool::slices`] returns, and
/// **only** [`SliceTool::commit`] puts a [`ToolRequest::Slices`] on the outbox
/// — the same shape as [`CropTool`], which draws its box on release and waits
/// for Enter. Auto-publishing on every release would leave one stale
/// `ToolRequest::Slices` per drag in the outbox, each carrying a prefix of the
/// set, and an application that concatenated them would export every slice
/// several times over.
///
/// `commit` publishes the whole set and then clears it, so committing twice
/// does not emit the same slices twice.
#[derive(Default)]
pub struct SliceTool {
    slices: Vec<Slice>,
    anchor: Option<Vec2>,
}

impl SliceTool {
    /// The slices drawn since the last [`SliceTool::commit`], for the overlay.
    pub fn slices(&self) -> &[Slice] {
        &self.slices
    }

    /// Publish the slice set and start a fresh one.
    ///
    /// Emits at most one [`ToolRequest::Slices`], and nothing at all when no
    /// slice has been drawn.
    pub fn commit(&mut self, ctx: &mut ToolContext<'_>) {
        if self.slices.is_empty() {
            return;
        }
        ctx.emit_request(ToolRequest::Slices(std::mem::take(&mut self.slices)));
    }
}

impl Tool for SliceTool {
    fn id(&self) -> ToolId {
        ToolId::Slice
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("slice anchor", event.pos)?;
        self.anchor = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    /// Records the drag as one more slice. Publishing waits for
    /// [`SliceTool::commit`] — see the type's contract.
    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(a) = self.anchor.take() else {
            return Ok(());
        };
        crate::error::finite_pt("slice corner", event.pos)?;
        let x0 = (a.x.min(event.pos.x).floor() as i64).max(ctx.canvas.x);
        let y0 = (a.y.min(event.pos.y).floor() as i64).max(ctx.canvas.y);
        let x1 = (a.x.max(event.pos.x).ceil() as i64).min(ctx.canvas.right());
        let y1 = (a.y.max(event.pos.y).ceil() as i64).min(ctx.canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        let n = self.slices.len() + 1;
        self.slices.push(Slice {
            rect: PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32),
            name: format!("slice_{n:02}"),
        });
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.slices.clear();
    }

    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        SliceTool::commit(self, ctx);
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        !self.slices.is_empty()
    }

    /// Active while a drag is in flight *or* while slices are waiting to be
    /// committed — the same rule [`CropTool`] uses for its pending box.
    fn is_active(&self) -> bool {
        self.anchor.is_some() || !self.slices.is_empty()
    }
}

// ---------------------------------------------------------- eyedropper ----

/// Eyedropper: read a colour off the canvas.
pub struct EyedropperTool {
    /// Half-width of the averaging square; `0` reads a single pixel.
    pub sample_radius: u32,
    /// Read the flattened composite rather than the active layer.
    pub sample_all_layers: bool,
    active: bool,
}

impl Default for EyedropperTool {
    fn default() -> Self {
        Self {
            sample_radius: 0,
            sample_all_layers: true,
            active: false,
        }
    }
}

impl EyedropperTool {
    pub fn new(sample_radius: u32, sample_all_layers: bool) -> Self {
        Self {
            sample_radius,
            sample_all_layers,
            active: false,
        }
    }

    /// The straight-alpha linear colour under `p`.
    ///
    /// Averaged in linear premultiplied light: averaging encoded values would
    /// bias a mixed sample toward the darker pixels, which is why a "5 by 5
    /// average" eyedropper reads too dark in every tool that gets this wrong.
    pub fn sample(&self, ctx: &ToolContext<'_>, p: Vec2) -> Result<[f32; 4], ToolError> {
        let key = if self.sample_all_layers {
            ctx.sample_key()?
        } else {
            PixelKey::Layer(ctx.active_layer.ok_or(ToolError::NoActiveLayer)?)
        };
        let c = IVec2::new(p.x.floor() as i32, p.y.floor() as i32);
        let r = self.sample_radius.min(64) as i32;
        let side = (r * 2 + 1) as u32;
        let rect = PixelRect::new((c.x - r) as i64, (c.y - r) as i64, side, side);
        let patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let mut acc = [0.0f64; 4];
        let mut n = 0.0f64;
        for y in c.y - r..=c.y + r {
            for x in c.x - r..=c.x + r {
                let px = patch.get(IVec2::new(x, y));
                for i in 0..4 {
                    acc[i] += px[i] as f64;
                }
                n += 1.0;
            }
        }
        let avg = [
            (acc[0] / n) as f32,
            (acc[1] / n) as f32,
            (acc[2] / n) as f32,
            (acc[3] / n) as f32,
        ];
        Ok(unpremultiply(avg))
    }
}

impl Tool for EyedropperTool {
    fn id(&self) -> ToolId {
        ToolId::Eyedropper
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("eyedropper point", event.pos)?;
        self.active = true;
        let c = self.sample(ctx, event.pos)?;
        ctx.set_picked(c);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.active && event.pos.x.is_finite() && event.pos.y.is_finite() {
            let c = self.sample(ctx, event.pos)?;
            ctx.set_picked(c);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.active = false;
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.active = false;
    }

    /// The registry's two Eyedropper options: `sample_radius` (the bar's
    /// Sample Size, the half-width of the averaged square) and
    /// `sample_all_layers`. Both are read by [`EyedropperTool::sample`].
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("sample_radius", ToolSetting::Int(v)) => {
                self.sample_radius = v.clamp(0, 64) as u32;
                Ok(())
            }
            ("sample_all_layers", ToolSetting::Bool(v)) => {
                self.sample_all_layers = v;
                Ok(())
            }
            ("sample_radius" | "sample_all_layers", _) => Err(kind_mismatch(key)),
            _ => Err(unknown_option(key)),
        }
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

// -------------------------------------------------------------- red eye ----

/// How red a pixel has to be, relative to its other channels, to be flash-red.
fn red_eye_strength(px: [f32; 4], threshold: f32) -> f32 {
    let s = unpremultiply(px);
    if s[3] <= 0.0 {
        return 0.0;
    }
    let other = s[1].max(s[2]).max(1e-4);
    let ratio = s[0] / other;
    ((ratio - threshold) / threshold.max(1e-3)).clamp(0.0, 1.0)
}

/// Red-eye: drag a box over an eye; the red goes grey.
pub struct RedEyeTool {
    /// How much redder than green/blue counts as flash red.
    pub threshold: f32,
    /// How dark the corrected pupil ends up, as a fraction of its luminance.
    pub darken: f32,
    anchor: Option<Vec2>,
}

impl Default for RedEyeTool {
    fn default() -> Self {
        Self {
            threshold: 1.6,
            darken: 0.5,
            anchor: None,
        }
    }
}

impl Tool for RedEyeTool {
    fn id(&self) -> ToolId {
        ToolId::RedEye
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("red-eye anchor", event.pos)?;
        self.anchor = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(a) = self.anchor.take() else {
            return Ok(());
        };
        // A mask stores coverage, not colour: there is no red channel to find a
        // pupil in.
        ctx.require_layer_target()?;
        crate::error::finite_pt("red-eye corner", event.pos)?;
        let x0 = (a.x.min(event.pos.x).floor() as i64).max(ctx.canvas.x);
        let y0 = (a.y.min(event.pos.y).floor() as i64).max(ctx.canvas.y);
        let x1 = (a.x.max(event.pos.x).ceil() as i64 + 1).min(ctx.canvas.right());
        let y1 = (a.y.max(event.pos.y).ceil() as i64 + 1).min(ctx.canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Err(ToolError::Degenerate);
        }
        let rect = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let darken = self.darken.clamp(0.0, 1.0);
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                let p = IVec2::new(x as i32, y as i32);
                let dst = patch.get(p);
                let k = red_eye_strength(dst, self.threshold) * ctx.selection.coverage_at(p);
                if k <= 0.0 {
                    continue;
                }
                let s = unpremultiply(dst);
                let g = linear_srgb_luminance([s[0], s[1], s[2]]) * (1.0 - darken);
                let fixed = premultiply([
                    s[0] + (g - s[0]) * k,
                    s[1] + (g - s[1]) * k,
                    s[2] + (g - s[2]) * k,
                    s[3],
                ]);
                patch.set(p, fixed);
            }
        }
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
    }

    /// The registry's two Red Eye options, both read by the fix on release.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("threshold", ToolSetting::Float(v)) => {
                self.threshold = finite("pupil threshold", v)?.clamp(1.0, 4.0);
                Ok(())
            }
            ("darken", ToolSetting::Float(v)) => {
                self.darken = finite("darken amount", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("threshold" | "darken", _) => Err(kind_mismatch(key)),
            _ => Err(unknown_option(key)),
        }
    }

    fn is_active(&self) -> bool {
        self.anchor.is_some()
    }
}

// ---------------------------------------------------------------- patch ----

/// Patch: lasso a region, then drag it onto clean pixels to heal it.
///
/// Two gestures in one tool, so it carries an explicit phase. The heal itself
/// is the same frequency split the healing brush uses: texture from the source,
/// colour and shading from the destination.
pub struct PatchTool {
    pub softness: f32,
    outline: Vec<Vec2>,
    mask: Option<editor_core::SelectionMask>,
    drag_from: Option<Vec2>,
    drawing: bool,
}

impl Default for PatchTool {
    fn default() -> Self {
        Self {
            softness: 4.0,
            outline: Vec::new(),
            mask: None,
            drag_from: None,
            drawing: false,
        }
    }
}

impl PatchTool {
    /// The region drawn so far, once the outline is closed.
    pub fn region(&self) -> Option<&editor_core::SelectionMask> {
        self.mask.as_ref()
    }

    fn heal(&mut self, ctx: &mut ToolContext<'_>, offset: IVec2) -> Result<(), ToolError> {
        // The heal is a frequency split over colour and shading; a coverage
        // plane has neither.
        ctx.require_layer_target()?;
        let Some(mask) = self.mask.clone() else {
            return Ok(());
        };
        let Some((min, max)) = mask.bounds() else {
            return Ok(());
        };
        let rect = PixelRect::new(
            min.x as i64,
            min.y as i64,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let src_rect = PixelRect::new(
            rect.x + offset.x as i64,
            rect.y + offset.y as i64,
            rect.width,
            rect.height,
        );
        let src = ColorPatch::load(ctx.tiles, key, src_rect)?;

        let sigma = self.softness.max(0.5);
        // The destination's low frequencies have to come from *outside* the
        // lassoed region: blurring the region into its own repair would leave a
        // ghost of whatever is being patched out. Same rule, same helper, as
        // the healing brush.
        let covered = {
            let (w, h) = (patch.width() as i32, patch.height() as i32);
            let o = patch.origin();
            let mut v = Vec::with_capacity((w as usize) * (h as usize));
            for y in 0..h {
                for x in 0..w {
                    let p = IVec2::new(o.x + x, o.y + y);
                    v.push(mask.coverage_at(p) as f32 / 255.0);
                }
            }
            v
        };
        let src_low = gaussian_blur(src.buffer(), sigma, EdgeMode::Clamp);
        let dst_low = crate::stroke::low_frequency_outside(patch.buffer(), &covered, sigma)?;
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                let p = IVec2::new(x as i32, y as i32);
                let a = (mask.coverage_at(p) as f32 / 255.0) * ctx.selection.coverage_at(p);
                if a <= 0.0 {
                    continue;
                }
                let sp = p + offset;
                let (Some(si), Some(di)) = (src.index_of(sp), patch.index_of(p)) else {
                    continue;
                };
                let sf = src.get(sp);
                let sl = src_low.pixels()[si];
                let dl = dst_low.pixels()[di];
                let dst = patch.get(p);
                let healed = [
                    (sf[0] - sl[0] + dl[0]).max(0.0),
                    (sf[1] - sl[1] + dl[1]).max(0.0),
                    (sf[2] - sl[2] + dl[2]).max(0.0),
                    dl[3].clamp(0.0, 1.0).max(sf[3]),
                ];
                patch.set(
                    p,
                    [
                        dst[0] + (healed[0] - dst[0]) * a,
                        dst[1] + (healed[1] - dst[1]) * a,
                        dst[2] + (healed[2] - dst[2]) * a,
                        dst[3] + (healed[3] - dst[3]) * a,
                    ],
                );
            }
        }
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
}

impl Tool for PatchTool {
    fn id(&self) -> ToolId {
        ToolId::Patch
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("patch point", event.pos)?;
        if self.mask.is_some() {
            self.drag_from = Some(event.pos);
        } else {
            self.outline.clear();
            self.outline.push(event.pos);
            self.drawing = true;
        }
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.drawing && event.pos.x.is_finite() && event.pos.y.is_finite() {
            self.outline.push(event.pos);
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.drawing {
            self.drawing = false;
            self.outline.push(event.pos);
            if self.outline.len() >= 3 {
                self.mask = Some(lasso_freehand(&self.outline)?);
            }
            return Ok(());
        }
        let Some(from) = self.drag_from.take() else {
            return Ok(());
        };
        let offset = IVec2::new(
            (event.pos.x - from.x).round() as i32,
            (event.pos.y - from.y).round() as i32,
        );
        if offset == IVec2::ZERO {
            return Ok(());
        }
        self.heal(ctx, offset)?;
        self.mask = None;
        self.outline.clear();
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.outline.clear();
        self.mask = None;
        self.drag_from = None;
        self.drawing = false;
    }

    /// The registry's one Patch option: `softness`, the Gaussian sigma in
    /// pixels (0.5..64) the heal's frequency split uses.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("softness", ToolSetting::Float(v)) => {
                self.softness = finite("softness", v)?.clamp(0.5, 64.0);
                Ok(())
            }
            ("softness", _) => Err(kind_mismatch(key)),
            _ => Err(unknown_option(key)),
        }
    }

    fn is_active(&self) -> bool {
        self.drawing || self.mask.is_some()
    }
}

// -------------------------------------------------------- magic eraser ----

/// Magic eraser: click to erase everything within tolerance of that pixel.
pub struct MagicEraserTool {
    pub tolerance: f32,
    pub contiguous: bool,
    pub antialias: bool,
    pub opacity: f32,
    /// Judge tolerance against the flattened composite (the context's
    /// `sample_from`) rather than the layer being erased.
    pub sample_merged: bool,
    seed: Option<IVec2>,
}

impl Default for MagicEraserTool {
    fn default() -> Self {
        Self {
            tolerance: 32.0 / 255.0,
            contiguous: true,
            antialias: true,
            opacity: 1.0,
            sample_merged: false,
            seed: None,
        }
    }
}

impl Tool for MagicEraserTool {
    fn id(&self) -> ToolId {
        ToolId::MagicEraser
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("magic eraser seed", event.pos)?;
        self.seed = Some(IVec2::new(
            event.pos.x.floor() as i32,
            event.pos.y.floor() as i32,
        ));
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(seed) = self.seed.take() else {
            return Ok(());
        };
        // Erasing to transparency is an alpha operation on colour pixels. On a
        // mask the equivalent gesture is a bucket fill with black, which the
        // paint bucket does properly through the coverage plane.
        ctx.require_layer_target()?;
        let canvas = ctx.canvas;
        if (seed.x as i64) < canvas.x
            || (seed.y as i64) < canvas.y
            || (seed.x as i64) >= canvas.right()
            || (seed.y as i64) >= canvas.bottom()
        {
            return Err(ToolError::PointOutside {
                x: seed.x,
                y: seed.y,
            });
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        // Sample All Layers judges the tolerance against the composite; the
        // erase itself always lands on the active layer.
        let read_key = if self.sample_merged {
            ctx.sample_key()?
        } else {
            key
        };
        let pixels = read_rgba8(ctx.tiles, read_key, canvas)?;
        let view = ImageView::new(
            IVec2::new(canvas.x as i32, canvas.y as i32),
            canvas.width,
            canvas.height,
            &pixels,
        )?;
        let mut mask = magic_wand(
            &view,
            seed,
            &WandOptions {
                tolerance: self.tolerance.clamp(0.0, 1.0),
                contiguous: self.contiguous,
                antialias: if self.antialias { 0.5 } else { 0.0 },
                metric: Default::default(),
                sample_alpha: true,
            },
        )?;
        if !ctx.selection.is_none() {
            let sel = to_mask(&ctx.selection, ctx.canvas_rect())?;
            mask = combine(&mask, &sel, BooleanOp::Intersect)?;
        }
        let Some((min, max)) = mask.bounds() else {
            return Ok(());
        };
        let rect = PixelRect::new(
            min.x as i64,
            min.y as i64,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let opacity = self.opacity.clamp(0.0, 1.0);
        for y in min.y..max.y {
            for x in min.x..max.x {
                let p = IVec2::new(x, y);
                let a = (mask.coverage_at(p) as f32 / 255.0) * opacity;
                if a <= 0.0 {
                    continue;
                }
                let dst = patch.get(p);
                patch.set(
                    p,
                    [
                        dst[0] * (1.0 - a),
                        dst[1] * (1.0 - a),
                        dst[2] * (1.0 - a),
                        dst[3] * (1.0 - a),
                    ],
                );
            }
        }
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.seed = None;
    }

    /// The registry's `FILL_OPTS` (shared with the paint bucket), each onto
    /// the field the flood and the erase read.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("tolerance", ToolSetting::Float(v)) => {
                self.tolerance = finite("tolerance", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("contiguous", ToolSetting::Bool(v)) => {
                self.contiguous = v;
                Ok(())
            }
            ("antialias", ToolSetting::Bool(v)) => {
                self.antialias = v;
                Ok(())
            }
            ("opacity", ToolSetting::Float(v)) => {
                self.opacity = finite("opacity", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("sample_merged", ToolSetting::Bool(v)) => {
                self.sample_merged = v;
                Ok(())
            }
            ("tolerance" | "contiguous" | "antialias" | "opacity" | "sample_merged", _) => {
                Err(kind_mismatch(key))
            }
            _ => Err(unknown_option(key)),
        }
    }

    /// `opacity` is a brush-shared key the shell delivers through the brush
    /// it hands every tool at pointer-down, never through `set_setting`.
    fn set_brush(&mut self, brush: BrushSettings) {
        if brush.opacity.is_finite() {
            self.opacity = brush.opacity.clamp(0.0, 1.0);
        }
    }

    fn is_active(&self) -> bool {
        self.seed.is_some()
    }
}

/// The encoded luminance of a straight-alpha linear colour — used by the tests
/// and by the UI's colour readouts.
pub fn encoded_luminance(rgba: [f32; 4]) -> f32 {
    linear_to_srgb(linear_srgb_luminance([rgba[0], rgba[1], rgba[2]]).clamp(0.0, 1.0))
}

/// Whether a selection covers a point at all — a small helper the UI shares.
pub fn selected(sel: &Selection, p: IVec2) -> bool {
    sel.coverage_at(p) > 0.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn align_offset_puts_edges_and_centres_where_they_belong() {
        let item = PixelRect::new(10, 10, 20, 40);
        let target = PixelRect::new(0, 0, 100, 100);
        assert_eq!(align_offset(item, target, Alignment::Left).x, -10.0);
        assert_eq!(align_offset(item, target, Alignment::Right).x, 70.0);
        assert_eq!(
            align_offset(item, target, Alignment::HorizontalCenter).x,
            30.0
        );
        assert_eq!(align_offset(item, target, Alignment::Top).y, -10.0);
        assert_eq!(align_offset(item, target, Alignment::Bottom).y, 50.0);
        assert_eq!(
            align_offset(item, target, Alignment::VerticalCenter).y,
            20.0
        );
        // An alignment on one axis never disturbs the other.
        assert_eq!(align_offset(item, target, Alignment::Left).y, 0.0);
        assert_eq!(align_offset(item, target, Alignment::Top).x, 0.0);
    }

    #[test]
    fn red_eye_strength_finds_flash_red_and_ignores_ordinary_colour() {
        let red = premultiply([0.8, 0.05, 0.05, 1.0]);
        let skin = premultiply([0.6, 0.45, 0.4, 1.0]);
        let grey = premultiply([0.5, 0.5, 0.5, 1.0]);
        assert!(red_eye_strength(red, 1.6) > 0.5);
        assert_eq!(red_eye_strength(grey, 1.6), 0.0);
        assert!(red_eye_strength(skin, 1.6) < 0.2);
        assert_eq!(red_eye_strength([0.0; 4], 1.6), 0.0);
    }

    // -- card 010: the typed forwarding seam -------------------------------

    #[test]
    fn move_tool_adopts_its_declared_options_through_set_setting() {
        let mut tool = MoveTool::default();
        assert!(!tool.auto_select);
        assert!(!tool.show_transform);

        tool.set_setting("auto_select", ToolSetting::Bool(true))
            .unwrap();
        assert!(tool.auto_select, "the Auto-Select boolean reaches the tool");
        tool.set_setting("show_transform", ToolSetting::Bool(true))
            .unwrap();
        assert!(tool.show_transform);

        // An absolute set: the same key clears it again.
        tool.set_setting("auto_select", ToolSetting::Bool(false))
            .unwrap();
        assert!(!tool.auto_select);
    }

    #[test]
    fn set_setting_refuses_unknown_keys_and_kind_mismatches_loudly() {
        let mut tool = MoveTool::default();
        assert!(
            matches!(
                tool.set_setting("no_such_option", ToolSetting::Bool(true)),
                Err(ToolError::UnknownOption { .. })
            ),
            "an unknown key is refused, not swallowed"
        );
        // Right key, wrong kind: the registry declares a Bool.
        assert!(matches!(
            tool.set_setting("auto_select", ToolSetting::Float(0.5)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
    }

    #[cfg(test)]
    mod content_pick_tests {
        use super::*;
        use crate::tiles::MemoryTiles;

        #[test]
        fn an_authoritative_empty_pick_blocks_the_raw_sampler_over_stored_ink() {
            // Card 037: the shell's bounded test said "nothing visible here" —
            // the raw tile sampler must not resurrect a stored pixel through it.
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            ctx.active_layer = Some(layer_model::LayerId::new());
            // The tri-state's inner None is authoritative even over stored ink.
            ctx.content_pick = Some(None);
            let tool = MoveTool::default();
            assert!(
                tool.layer_under(&ctx, Vec2::new(30.0, 30.0)).is_none(),
                "an authoritative empty pick wins over the raw sampler"
            );
            // No shell ran: the raw sampler answers (nothing stored → None here,
            // but the branch is taken rather than the authoritative path).
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            ctx.active_layer = Some(layer_model::LayerId::new());
            assert!(tool.layer_under(&ctx, Vec2::new(30.0, 30.0)).is_none());
        }
    }

    #[cfg(test)]
    mod move_options_tests {
        use super::*;
        use crate::tiles::MemoryTiles;

        #[test]
        fn group_selection_mode_climbs_to_the_top_level_ancestor() {
            // Card 038: Layer/Group mode — an auto-select pick of a leaf inside
            // a nested group resolves to the group row the panel shows.
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            let group = layer_model::LayerId::new();
            let child = layer_model::LayerId::new();
            ctx.layer_parents = vec![(child, Some(group)), (group, None)];
            let mut tool = MoveTool {
                auto_select: true,
                select_groups: true,
                ..MoveTool::default()
            };
            ctx.content_pick = Some(Some(child));
            assert_eq!(tool.layer_under(&ctx, Vec2::new(8.0, 8.0)), Some(group));
            // Layer mode keeps the leaf.
            tool.select_groups = false;
            assert_eq!(tool.layer_under(&ctx, Vec2::new(8.0, 8.0)), Some(child));
            // An authoritative empty pick stays empty in both modes.
            ctx.content_pick = Some(None);
            assert!(tool.layer_under(&ctx, Vec2::new(8.0, 8.0)).is_none());
        }

        #[test]
        fn show_transform_controls_publishes_a_box_without_starting_an_edit() {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 96, 96));
            let layer = layer_model::LayerId::new();
            ctx.active_layer = Some(layer);
            ctx.active_layer_content_bounds = Some(PixelRect::new(20, 24, 30, 18));
            let mut tool = MoveTool {
                show_transform: true,
                ..MoveTool::default()
            };

            // A no-motion click caches the framed layer's ink and emits nothing.
            tool.on_pointer_down(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(30.0, 30.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            tool.on_pointer_up(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(30.0, 30.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            assert!(ctx.drain().is_empty(), "a no-motion click is not an edit");
            let geometry = tool.live_geometry().expect("the box is published");
            let crate::tool::SessionGeometry::Transform {
                state,
                layer: framed,
                active,
                ..
            } = geometry;
            assert_eq!(framed, Some(layer));
            assert!(active.is_none(), "no handle is being dragged");
            assert_eq!(state.source, PixelRect::new(20, 24, 30, 18));
        }

        #[test]
        fn a_multi_selection_drag_moves_the_whole_set_in_one_transaction() {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 96, 96));
            let a = layer_model::LayerId::new();
            let b = layer_model::LayerId::new();
            ctx.active_layer = Some(a);
            ctx.selected_layers = vec![a, b];
            let mut tool = MoveTool::default();
            tool.on_pointer_down(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(10.0, 10.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            tool.on_pointer_up(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(24.0, 20.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            let requests = ctx.drain_requests();
            assert_eq!(requests.len(), 1, "the set rides one request");
            let ToolRequest::TransformLayers { layers, delta } = &requests[0] else {
                panic!("a set move: {:?}", requests[0]);
            };
            assert_eq!(layers, &[a, b]);
            assert_eq!(&delta[4..], &[14.0, 10.0]);
        }

        #[test]
        fn a_linked_participant_moves_the_whole_chain_together() {
            // Card 043: moving a linked layer drags every other linked
            // layer with it — one request, each id exactly once, leader
            // first.
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 96, 96));
            let a = layer_model::LayerId::new();
            let b = layer_model::LayerId::new();
            let c = layer_model::LayerId::new();
            ctx.active_layer = Some(a);
            ctx.selected_layers = vec![a];
            ctx.linked_layers = vec![b, a, c];
            let mut tool = MoveTool::default();
            tool.on_pointer_down(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(10.0, 10.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            tool.on_pointer_up(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(24.0, 20.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            let requests = ctx.drain_requests();
            assert_eq!(requests.len(), 1, "the chain rides one request");
            let ToolRequest::TransformLayers { layers, delta } = &requests[0] else {
                panic!("a chain move: {:?}", requests[0]);
            };
            assert_eq!(layers, &[a, b, c], "leader first, chain deduped");
            assert_eq!(&delta[4..], &[14.0, 10.0]);
        }

        #[test]
        fn an_unlinked_participant_moves_alone() {
            // Card 043: no linked participant — no expansion, the plain
            // single-layer TransformLayer commit.
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 96, 96));
            let a = layer_model::LayerId::new();
            ctx.active_layer = Some(a);
            ctx.selected_layers = vec![a];
            ctx.linked_layers = vec![layer_model::LayerId::new()];
            let mut tool = MoveTool::default();
            tool.on_pointer_down(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(10.0, 10.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            tool.on_pointer_up(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(24.0, 20.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            assert!(ctx.drain_requests().is_empty(), "no expansion");
            assert!(matches!(
                ctx.drain().last(),
                Some(Command::TransformLayer { layer_id, .. }) if *layer_id == a
            ));
        }

        #[test]
        fn a_locked_chain_member_stays_put_while_the_rest_move() {
            // Card 043: a LOCKED chain member follows the card-036 rule —
            // filtered out (stays put), not a refusal.
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 96, 96));
            let a = layer_model::LayerId::new();
            let b = layer_model::LayerId::new();
            let c = layer_model::LayerId::new();
            ctx.active_layer = Some(a);
            ctx.selected_layers = vec![a];
            ctx.linked_layers = vec![a, b, c];
            ctx.layer_locks = vec![(c, true)];
            let mut tool = MoveTool::default();
            tool.on_pointer_down(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(10.0, 10.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            tool.on_pointer_up(
                &mut ctx,
                PointerEvent {
                    pos: Vec2::new(24.0, 20.0),
                    pressure: 1.0,
                    modifiers: Default::default(),
                },
            )
            .unwrap();
            let requests = ctx.drain_requests();
            assert_eq!(requests.len(), 1, "a and b move; c is locked out");
            let ToolRequest::TransformLayers { layers, .. } = &requests[0] else {
                panic!("a chain move: {:?}", requests[0]);
            };
            assert_eq!(layers, &[a, b], "the locked chain member stays put");
        }
    }

    #[cfg(test)]
    mod snap_tests {
        use super::*;
        use crate::tool::{SnapAxis, SnapCandidate};

        #[test]
        fn the_moving_rect_snaps_to_another_layers_edge() {
            // Card 042: the dragged rect (0..40) moved by ~+22 lands its right
            // edge on the neighbour's left edge at 62 (delta 22 → snapped 22).
            let base = PixelRect::new(0, 0, 40, 40);
            let candidates = vec![
                SnapCandidate {
                    axis: SnapAxis::X,
                    doc: 62.0,
                },
                SnapCandidate {
                    axis: SnapAxis::Y,
                    doc: 0.0,
                },
            ];
            let snapped = crate::snap_delta(base, Vec2::new(21.6, 0.0), &candidates, 8.0);
            assert!(
                (snapped.x - 22.0).abs() < 1e-4,
                "the right edge (40 + delta) snaps to 62: {snapped:?}"
            );
            assert_eq!(snapped.y, 0.0, "the y axis had no candidate in range");

            // Outside the threshold: no snap, the raw delta survives.
            let raw = crate::snap_delta(base, Vec2::new(31.0, 0.0), &candidates, 8.0);
            assert!((raw.x - 31.0).abs() < 1e-4, "beyond the threshold no snap");
        }
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;

    const W: u32 = 64;
    const H: u32 = 64;

    fn paint(tiles: &mut MemoryTiles, key: PixelKey, color: impl Fn(usize, usize) -> [u8; 4]) {
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let i = (y * ts + x) * 4;
                data[i..i + 4].copy_from_slice(&color(x, y));
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
    }

    #[test]
    fn eyedropper_sample_size_averages_the_square_and_sample_all_layers_picks_the_source() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let composite = LayerId::new();
        // The layer: black with one white pixel at (31, 31). The composite:
        // white everywhere.
        paint(&mut tiles, PixelKey::Layer(layer), |x, y| {
            if (x, y) == (31, 31) {
                [255, 255, 255, 255]
            } else {
                [0, 0, 0, 255]
            }
        });
        paint(&mut tiles, PixelKey::Layer(composite), |_, _| {
            [255, 255, 255, 255]
        });
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
        ctx.sample_from = Some(PixelKey::Layer(composite));

        let mut tool = EyedropperTool::default();
        tool.set_setting("sample_all_layers", ToolSetting::Bool(false))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(31.0, 31.0))
            .unwrap();
        assert!(
            ctx.picked().unwrap()[0] > 0.99,
            "a point sample of the layer white pixel"
        );

        // Sample Size 2 = a 5x5 square: one white pixel in 25, in linear light.
        tool.set_setting("sample_radius", ToolSetting::Int(2))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(31.0, 31.0))
            .unwrap();
        let avg = ctx.picked().unwrap()[0];
        assert!((avg - 1.0 / 25.0).abs() < 1e-3, "5x5 average was {avg}");

        // Sample All Layers reads the composite instead: white.
        tool.set_setting("sample_all_layers", ToolSetting::Bool(true))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(31.0, 31.0))
            .unwrap();
        assert!(
            ctx.picked().unwrap()[0] > 0.99,
            "the composite is white everywhere"
        );

        assert!(matches!(
            tool.set_setting("sample_radius", ToolSetting::Float(2.0)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("tolerance", ToolSetting::Float(2.0)),
            Err(ToolError::UnknownOption { .. })
        ));
    }

    #[test]
    fn crop_aspect_constrains_the_box_and_straighten_delete_ride_the_request() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128));
        let mut tool = CropTool::default();
        tool.set_setting("aspect", ToolSetting::Float(2.0)).unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 0.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(40.0, 40.0))
            .unwrap();
        let rect = tool.box_rect.expect("a box after release");
        // A square drag under a 2:1 lock keeps the dragged height (the
        // extent the ratio makes dominant) and derives the width from it.
        assert_eq!(
            (rect.width, rect.height),
            (80, 40),
            "aspect 2:1 constrains the box to twice as wide as tall"
        );

        tool.set_setting("straighten", ToolSetting::Float(0.5))
            .unwrap();
        tool.set_setting("delete_cropped", ToolSetting::Bool(true))
            .unwrap();
        Tool::commit(&mut tool, &mut ctx).unwrap();
        match ctx.drain_requests().pop() {
            Some(ToolRequest::Crop(req)) => {
                assert_eq!(req.rect, rect);
                assert_eq!(req.straighten, 0.5);
                assert!(req.delete_cropped);
            }
            other => panic!("expected a crop request: {other:?}"),
        }

        // Aspect 0 is the bar's "unconstrained".
        tool.set_setting("aspect", ToolSetting::Float(0.0)).unwrap();
        assert_eq!(tool.aspect, None);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(0.0, 0.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(40.0, 40.0))
            .unwrap();
        assert_eq!(tool.box_rect.unwrap().height, 40);
        assert!(matches!(
            tool.set_setting("aspect", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("straighten", ToolSetting::Float(f32::INFINITY)),
            Err(ToolError::NotFinite { .. })
        ));
    }

    #[test]
    fn magic_eraser_tolerance_sample_merged_and_opacity_reach_the_erase() {
        let bands = |x: usize, _y: usize| -> [u8; 4] {
            let g = if (16..32).contains(&x) { 220 } else { 40 };
            [g, g, g, 255]
        };
        // Run one click at (4, 4) and report how many pixels went fully
        // transparent, plus the alpha left at the seed.
        let run = |configure: &dyn Fn(&mut MagicEraserTool)| -> (usize, u8) {
            let mut tiles = MemoryTiles::new();
            let layer = LayerId::new();
            let composite = LayerId::new();
            let key = PixelKey::Layer(layer);
            paint(&mut tiles, key, bands);
            paint(&mut tiles, PixelKey::Layer(composite), |_, _| {
                [40, 40, 40, 255]
            });
            let mut tool = MagicEraserTool {
                antialias: false,
                ..MagicEraserTool::default()
            };
            configure(&mut tool);
            let delta = {
                let mut ctx =
                    ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
                ctx.sample_from = Some(PixelKey::Layer(composite));
                tool.on_pointer_down(&mut ctx, PointerEvent::at(4.0, 4.0))
                    .unwrap();
                tool.on_pointer_up(&mut ctx, PointerEvent::at(4.0, 4.0))
                    .unwrap();
                match ctx.drain().pop() {
                    Some(Command::PaintTiles { delta, .. }) => delta,
                    other => panic!("expected a paint: {other:?}"),
                }
            };
            tiles.apply_delta(key, &delta);
            let mut cleared = 0;
            for y in 0..H as i64 {
                for x in 0..W as i64 {
                    if tiles.pixel(key, x, y)[3] == 0 {
                        cleared += 1;
                    }
                }
            }
            (cleared, tiles.pixel(key, 4, 4)[3])
        };
        let (tight, _) = run(&|t| {
            t.set_setting("tolerance", ToolSetting::Float(0.0)).unwrap();
        });
        assert_eq!(
            tight,
            16 * H as usize,
            "tolerance 0 erases the seed band only"
        );
        let (loose, _) = run(&|t| {
            t.set_setting("tolerance", ToolSetting::Float(1.0)).unwrap();
        });
        assert_eq!(loose, (W * H) as usize, "tolerance 1 erases everything");
        let (global, _) = run(&|t| {
            t.set_setting("tolerance", ToolSetting::Float(0.0)).unwrap();
            t.set_setting("contiguous", ToolSetting::Bool(false))
                .unwrap();
        });
        assert_eq!(
            global,
            48 * H as usize,
            "non-contiguous reaches the far dark band"
        );
        let (merged, _) = run(&|t| {
            t.set_setting("tolerance", ToolSetting::Float(0.0)).unwrap();
            t.set_setting("sample_merged", ToolSetting::Bool(true))
                .unwrap();
        });
        assert_eq!(
            merged,
            (W * H) as usize,
            "judged against the flat composite, everything matches"
        );
        let (_, half) = run(&|t| {
            t.set_setting("opacity", ToolSetting::Float(0.5)).unwrap();
        });
        assert!(
            (126..=129).contains(&half),
            "opacity 0.5 leaves half the alpha: {half}"
        );
        let (_, quarter) = run(&|t| {
            t.set_brush(BrushSettings {
                opacity: 0.75,
                ..BrushSettings::default()
            })
        });
        assert!(
            (62..=65).contains(&quarter),
            "brush opacity 0.75 erases three quarters: {quarter}"
        );
    }

    #[test]
    fn red_eye_and_patch_answer_their_declared_options() {
        let mut red = RedEyeTool::default();
        red.set_setting("threshold", ToolSetting::Float(2.5))
            .unwrap();
        red.set_setting("darken", ToolSetting::Float(0.9)).unwrap();
        assert_eq!((red.threshold, red.darken), (2.5, 0.9));
        assert!(matches!(
            red.set_setting("darken", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        let mut patch = PatchTool::default();
        patch
            .set_setting("softness", ToolSetting::Float(12.0))
            .unwrap();
        assert_eq!(patch.softness, 12.0);
        assert!(matches!(
            patch.set_setting("size", ToolSetting::Float(12.0)),
            Err(ToolError::UnknownOption { .. })
        ));
    }

    /// One Red Eye box over a flash-red layer; the centre pixel of the fix,
    /// sRGB 8-bit.
    fn red_eye_result(darken: f32) -> [u8; 4] {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        paint(&mut tiles, PixelKey::Layer(layer), |_, _| {
            [220, 30, 30, 255]
        });
        let delta = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
            let mut tool = RedEyeTool::default();
            tool.set_setting("darken", ToolSetting::Float(darken))
                .unwrap();
            tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(24.0, 24.0))
                .unwrap();
            match ctx.drain().pop() {
                Some(Command::PaintTiles { delta, .. }) => delta,
                other => panic!("expected the fix as one paint: {other:?}"),
            }
        };
        // The tool emits, the application applies: do the applying here.
        tiles.apply_delta(PixelKey::Layer(layer), &delta);
        let px = read_rgba8(&tiles, PixelKey::Layer(layer), PixelRect::new(16, 16, 1, 1)).unwrap();
        [px[0], px[1], px[2], px[3]]
    }

    /// W2-C: the options bar's Darken Amount changes the pixels, not just a
    /// field. Before the option was wired the bar drew a control that did
    /// nothing; a field test cannot tell the two apart.
    #[test]
    fn red_eye_darken_zero_and_one_fix_the_pupil_differently() {
        let bright = red_eye_result(0.0);
        let dark = red_eye_result(1.0);
        assert_ne!(
            bright, dark,
            "darken 0 and darken 1 produced the same pupil"
        );
        // Both took the red out: the fixed pupil is neutral...
        for px in [bright, dark] {
            let (r, g, b) = (i16::from(px[0]), i16::from(px[1]), i16::from(px[2]));
            assert!(
                (r - g).abs() <= 2 && (g - b).abs() <= 2,
                "{px:?} is still tinted"
            );
        }
        // ...and darken 1 is black where darken 0 keeps the pupil's luminance.
        assert!(dark[0] < 8, "darken 1 left {dark:?}");
        assert!(bright[0] > 40, "darken 0 left {bright:?}");
    }

    /// Lasso a diamond over a black blot, drag it onto a hard stripe texture,
    /// return the healed square, sRGB 8-bit.
    fn patch_result(softness: f32) -> Vec<u8> {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        paint(&mut tiles, PixelKey::Layer(layer), |x, y| {
            if x >= 32 {
                // 2 px stripes: texture a 0.5 px sigma keeps and a 1 px sigma
                // softens, so the two heals cannot come out the same.
                if (x / 2 + y / 2) % 2 == 0 {
                    [200, 200, 200, 255]
                } else {
                    [60, 60, 60, 255]
                }
            } else if (12..20).contains(&x) && (12..20).contains(&y) {
                [0, 0, 0, 255]
            } else {
                [128, 128, 128, 255]
            }
        });
        let delta = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
            let mut tool = PatchTool::default();
            tool.set_setting("softness", ToolSetting::Float(softness))
                .unwrap();
            // A diamond, so the corners of its bounding box stay uncovered
            // and the heal has an "outside" to take its shading from.
            tool.on_pointer_down(&mut ctx, PointerEvent::at(16.0, 6.0))
                .unwrap();
            for (x, y) in [(26.0, 16.0), (16.0, 26.0), (6.0, 16.0)] {
                tool.on_pointer_move(&mut ctx, PointerEvent::at(x, y))
                    .unwrap();
            }
            tool.on_pointer_up(&mut ctx, PointerEvent::at(16.0, 6.0))
                .unwrap();
            assert!(tool.region().is_some(), "the lasso closed");
            // Drag it 32 px right, onto the stripes.
            tool.on_pointer_down(&mut ctx, PointerEvent::at(16.0, 16.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(48.0, 16.0))
                .unwrap();
            match ctx.drain().pop() {
                Some(Command::PaintTiles { delta, .. }) => delta,
                other => panic!("expected the heal as one paint: {other:?}"),
            }
        };
        tiles.apply_delta(PixelKey::Layer(layer), &delta);
        read_rgba8(&tiles, PixelKey::Layer(layer), PixelRect::new(8, 8, 16, 16)).unwrap()
    }

    /// W2-C: Softness reaches the heal. `0.0` clamps to the 0.5 px floor the
    /// registry declares; `1.0` blurs the texture split twice as wide.
    #[test]
    fn patch_softness_zero_and_one_heal_differently() {
        let tight = patch_result(0.0);
        let soft = patch_result(1.0);
        assert_ne!(tight, soft, "softness 0 and 1 healed identically");
        // Both healed: the blot's centre is no longer black.
        for (name, px) in [("tight", &tight), ("soft", &soft)] {
            let centre = (8 * 16 + 8) * 4;
            assert!(
                px[centre] > 20,
                "{name} left the blot black: {:?}",
                &px[centre..centre + 4]
            );
        }
    }
}
