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
    /// W13-A: Alt was held at the press (Photopea's Alt+drag): the drag
    /// moves a COPY. With a pixel selection the tool floats a copy of the
    /// selected pixels and leaves the originals; without one the shell
    /// duplicates the layer(s) the tool's move names and moves the copies
    /// (`app-shell` `move_duplicate`), either way as one undo step.
    copy: bool,
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
            copy: false,
        }
    }
}

impl MoveTool {
    /// W13-A: whether the running drag was begun with Alt held, so it moves
    /// a copy rather than the original.
    pub fn is_copying(&self) -> bool {
        self.copy
    }

    /// W13-A: Alt+drag with a pixel selection: a COPY of the selected pixels
    /// of the active layer is laid down `step` (whole document pixels) away,
    /// over what was there, and the originals stay put. The marching ants
    /// travel with the copy. One transaction, labelled `Duplicate Selection`
    /// (ONE undo step). Nearest-sample, like the plain move's whole-pixel
    /// step: a translation of an untransformed layer copies bytes exactly.
    pub fn copy_selected_pixels(
        ctx: &mut ToolContext<'_>,
        step: Vec2,
    ) -> Result<Command, ToolError> {
        let selection = ctx.selection.clone();
        let (min, max) = selection.bounds().ok_or(ToolError::Degenerate)?;
        ctx.require_layer_target()?;
        if let Some(layer) = ctx.active_layer {
            if ctx.layer_lock(layer) == Some(true) {
                return Err(ToolError::LayerLocked);
            }
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
        let from_layer = to_layer.inverse();
        // The layer-space boxes of the selection and of where it lands.
        let layer_box = |offset: Vec2, grow: f32| -> Result<PixelRect, ToolError> {
            let (lo, hi) = [
                Vec2::new(min.x as f32, min.y as f32),
                Vec2::new(max.x as f32, min.y as f32),
                Vec2::new(min.x as f32, max.y as f32),
                Vec2::new(max.x as f32, max.y as f32),
            ]
            .iter()
            .map(|c| to_layer.transform_point2(*c + offset))
            .fold(
                (Vec2::splat(f32::INFINITY), Vec2::splat(f32::NEG_INFINITY)),
                |(lo, hi), c| (lo.min(c), hi.max(c)),
            );
            let (lo, hi) = ((lo - grow).floor(), (hi + grow).ceil());
            if !lo.is_finite() || !hi.is_finite() || hi.x <= lo.x || hi.y <= lo.y {
                return Err(ToolError::Degenerate);
            }
            Ok(PixelRect::new(
                lo.x as i64,
                lo.y as i64,
                (hi.x - lo.x) as u32,
                (hi.y - lo.y) as u32,
            ))
        };
        let dest = layer_box(step, 0.0)?;
        let source = layer_box(Vec2::ZERO, 1.0)?;
        let (x0, y0) = (dest.x.min(source.x), dest.y.min(source.y));
        let (x1, y1) = (
            dest.right().max(source.right()),
            dest.bottom().max(source.bottom()),
        );
        let rect = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
        let mut patch = ColorPatch::load_native(ctx.tiles, key, rect)?;
        // Every read first, from the untouched plane, then every write: a
        // copy that overlaps its source must not read its own output.
        let mut writes = Vec::new();
        for y in dest.y..dest.bottom() {
            for x in dest.x..dest.right() {
                let doc = from_layer.transform_point2(Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
                let from_doc = doc - step;
                let c = selection.coverage_at(IVec2::new(
                    from_doc.x.floor() as i32,
                    from_doc.y.floor() as i32,
                ));
                if c <= 0.0 {
                    continue;
                }
                let from = to_layer.transform_point2(from_doc);
                let px = patch.get(IVec2::new(from.x.floor() as i32, from.y.floor() as i32));
                let lifted = px.map(|v| v * c);
                if lifted.iter().all(|v| *v == 0.0) {
                    continue;
                }
                let at = IVec2::new(x as i32, y as i32);
                let under = patch.get(at);
                let keep = 1.0 - lifted[3].clamp(0.0, 1.0);
                let out: [f32; 4] = std::array::from_fn(|i| lifted[i] + under[i] * keep);
                writes.push((at, out));
            }
        }
        let mut commands = Vec::new();
        if !writes.is_empty() {
            for (at, px) in writes {
                patch.set(at, px);
            }
            let delta = patch.commit(ctx.tiles, key)?;
            if !delta.is_empty() {
                commands.push(Command::PaintTiles { target, delta });
            }
        }
        let canvas = selection::rect::Rect::from_xywh(
            ctx.canvas.x as i32,
            ctx.canvas.y as i32,
            ctx.canvas.width,
            ctx.canvas.height,
        );
        let next = selection::transform_selection(
            &selection,
            canvas,
            glam::Affine2::from_translation(step),
            selection::transform::ResampleFilter::Bilinear,
        )?;
        commands.push(Command::SetSelection { selection: next });
        Ok(Command::Transaction {
            label: "Duplicate Selection".to_string(),
            commands,
        })
    }

    /// W5-C: seed Show Transform Controls' box from the context alone, so
    /// ticking the option frames the active layer's ink straight away rather
    /// than after the next canvas click. No session, no commands.
    pub fn seed_display(&mut self, ctx: &ToolContext<'_>) {
        if !self.show_transform {
            self.display_bounds = None;
            self.display_layer = None;
            return;
        }
        self.display_layer = ctx.active_layer;
        // The TIGHT ink first: the stored extent is tile-aligned, so a 128 px
        // image would be framed as 256 px.
        self.display_bounds = ctx
            .active_layer_ink_bounds
            .or(ctx.active_layer_content_bounds);
    }

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
        // W13-A: Alt at the press makes the whole drag a copy.
        self.copy = event.modifiers.alt;
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
                ctx.active_layer_ink_bounds
                    .or(ctx.active_layer_content_bounds)
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
        // W13-A: the copy flag dies with the gesture (read once, here).
        let copy = std::mem::take(&mut self.copy);
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
        // W5-C: with a pixel selection the Move tool moves the SELECTED
        // PIXELS of the active layer (Photopea), not the whole layer: they
        // are lifted, laid down at the whole-pixel offset over what was left
        // behind, and the marching ants travel with them, as ONE undoable
        // step. A parametric layer (text, shape) keeps the layer move.
        if let Some((min, max)) = ctx.selection.bounds() {
            if ctx.paint_target == crate::tool::PaintTarget::Layer
                && !ctx.active_layer_parametric
                && Some(layer) == ctx.active_layer
            {
                let step = d.round();
                if step == Vec2::ZERO {
                    return Ok(());
                }
                // W13-A: Alt+drag floats a COPY; the originals stay.
                if copy {
                    let command = Self::copy_selected_pixels(ctx, step)?;
                    ctx.emit(command);
                    return Ok(());
                }
                let rect = PixelRect::new(
                    min.x as i64,
                    min.y as i64,
                    (max.x - min.x).max(0) as u32,
                    (max.y - min.y).max(0) as u32,
                );
                let mut state = crate::transform::TransformState::new(rect);
                for corner in state.corners.iter_mut() {
                    *corner += step;
                }
                state.pivot += step;
                let command = crate::transform::float_selection(
                    ctx,
                    &state,
                    crate::transform::TransformMode::Scale,
                    "Move Selection",
                )?;
                ctx.emit(command);
                return Ok(());
            }
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
        // W13-A: and so does an Alt copy.
        self.copy = false;
    }

    fn is_active(&self) -> bool {
        self.start.is_some()
    }
}

// ---------------------------------------------------------------- crop ----

/// W4-D: the Crop options bar's Ratio presets, in bar order, with the
/// width/height each one locks the box to. `0.0` marks the three that are not
/// a fixed number: Free (no lock), Original (the canvas's own ratio) and the
/// W x H x Resolution preset (the W and H fields' ratio).
pub const CROP_RATIO_PRESETS: [(&str, f32); 8] = [
    ("Free", 0.0),
    ("Original", 0.0),
    ("1:1", 1.0),
    ("4:3", 4.0 / 3.0),
    ("16:9", 16.0 / 9.0),
    ("3:2", 1.5),
    ("5:4", 1.25),
    ("W x H x Resolution", 0.0),
];

/// W4-D: the Ratio choice labels, derived from [`CROP_RATIO_PRESETS`].
pub const CROP_RATIO_LABELS: [&str; 8] = {
    let mut out = [""; 8];
    let mut i = 0;
    while i < 8 {
        out[i] = CROP_RATIO_PRESETS[i].0;
        i += 1;
    }
    out
};

/// W4-D: index of the Original preset in [`CROP_RATIO_PRESETS`].
pub const CROP_RATIO_ORIGINAL: usize = 1;
/// W4-D: index of the W x H x Resolution preset in [`CROP_RATIO_PRESETS`].
pub const CROP_RATIO_SIZE: usize = 7;

/// W4-D: the Overlay choice, in bar order.
pub const CROP_OVERLAYS: [(&str, crate::tool::CropGuide); 5] = [
    ("None", crate::tool::CropGuide::None),
    ("Rule of Thirds", crate::tool::CropGuide::Thirds),
    ("Grid", crate::tool::CropGuide::Grid),
    ("Diagonal", crate::tool::CropGuide::Diagonals),
    ("Golden Ratio", crate::tool::CropGuide::GoldenRatio),
];

/// W4-D: the Overlay choice labels, derived from [`CROP_OVERLAYS`].
pub const CROP_OVERLAY_LABELS: [&str; 5] = {
    let mut out = [""; 5];
    let mut i = 0;
    while i < 5 {
        out[i] = CROP_OVERLAYS[i].0;
        i += 1;
    }
    out
};

/// W4-D round 2: the options only the W x H x Resolution preset reads.
pub const CROP_SIZE_OPTION_KEYS: [&str; 4] = ["width", "height", "units", "resolution"];

/// W4-D round 2: whether the Crop options bar shows `key` under the Ratio
/// preset `ratio` — W, H, Units and Resolution only under W x H x
/// Resolution, the preset that reads them; everything else always.
pub fn crop_option_shown(key: &str, ratio: usize) -> bool {
    !CROP_SIZE_OPTION_KEYS.contains(&key) || ratio == CROP_RATIO_SIZE
}

/// W4-D: the largest edge the W x H x Resolution preset may ask for.
pub const CROP_MAX_OUTPUT_PX: f32 = 30_000.0;

/// W4-D: the straighten angle a drawn line asks for, radians clockwise in a
/// y-down document: the rotation that makes the line level (or plumb, for a
/// line nearer vertical than horizontal). `None` for a line too short to
/// have a direction.
pub fn straighten_angle(from: Vec2, to: Vec2) -> Option<f32> {
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI};
    let d = to - from;
    if !d.is_finite() || d.length() < 2.0 {
        return None;
    }
    let mut angle = d.y.atan2(d.x);
    // Direction does not matter: a line dragged right-to-left is the same line.
    if angle > FRAC_PI_2 {
        angle -= PI;
    } else if angle <= -FRAC_PI_2 {
        angle += PI;
    }
    // A near-vertical line is straightened to plumb, not laid flat.
    if angle > FRAC_PI_4 {
        angle -= FRAC_PI_2;
    } else if angle < -FRAC_PI_4 {
        angle += FRAC_PI_2;
    }
    Some(angle)
}

/// Crop: drag a keep-region, constrain its shape, straighten, commit.
pub struct CropTool {
    /// Width divided by height the box is locked to, if any.
    pub aspect: Option<f32>,
    /// Rotation the crop asks for before the cut, radians clockwise. It rides
    /// in the emitted [`CropRequest`]; the application performs it (W4-D).
    /// A line drawn in Straighten mode overrides it for the next commit.
    pub straighten: f32,
    pub delete_cropped: bool,
    /// W4-D: the Ratio preset, an index into [`CROP_RATIO_PRESETS`].
    pub ratio: usize,
    /// W4-D: the W x H x Resolution preset's width and height, in pixels or
    /// (with `output_in_inches`) inches.
    pub output_width: f32,
    pub output_height: f32,
    /// W4-D: W and H are inches, converted at `resolution`.
    pub output_in_inches: bool,
    /// W4-D: pixels per inch for the W x H x Resolution preset.
    pub resolution: f32,
    /// W4-D: the composition guide the live box is drawn with.
    pub overlay: crate::tool::CropGuide,
    /// W4-D: Straighten mode — a drag draws a line instead of a box, and the
    /// line's angle becomes the straighten angle of the next commit.
    pub straighten_line: bool,
    /// W4-D: the angle the last straighten line asked for; commit and cancel
    /// clear it.
    line_angle: Option<f32>,
    /// W4-D round 2: the straighten line as drawn, `[from, to]` in document
    /// pixels — published with the crop box while it is dragged and after
    /// release, so the gesture and its angle are on screen before Enter.
    line: Option<[Vec2; 2]>,
    anchor: Option<Vec2>,
    current: Option<Vec2>,
    /// W4-A: the canvas at pointer-down, so the live box is clipped the
    /// same way the released one is.
    canvas: Option<PixelRect>,
    /// The committed box, once the drag has ended and before Enter.
    pub box_rect: Option<PixelRect>,
    /// W13-I: the options bar's Content-Aware box ([`CROP_CONTENT_AWARE_KEY`]).
    /// On, the box may be dragged past the canvas, and the application fills
    /// the canvas the crop adds (past the old edges, or uncovered by a
    /// straighten) from the image by PatchMatch.
    pub content_aware: bool,
}

/// W13-I: the Crop tool's Content-Aware option key (a Bool).
pub const CROP_CONTENT_AWARE_KEY: &str = "content_aware";

impl Default for CropTool {
    fn default() -> Self {
        Self {
            aspect: None,
            straighten: 0.0,
            delete_cropped: false,
            ratio: 0,
            output_width: 1920.0,
            output_height: 1080.0,
            output_in_inches: false,
            resolution: 72.0,
            overlay: crate::tool::CropGuide::Thirds,
            straighten_line: false,
            line_angle: None,
            line: None,
            anchor: None,
            current: None,
            canvas: None,
            box_rect: None,
            content_aware: false,
        }
    }
}

impl CropTool {
    /// W13-I: the canvas the box is clipped to — none with Content-Aware on,
    /// where a box past the edges asks for new canvas to be filled.
    fn clip_to(&self, canvas: Option<PixelRect>) -> Option<PixelRect> {
        canvas.filter(|_| !self.content_aware)
    }

    /// W4-D: the width/height the box is locked to right now: the Ratio
    /// preset's, the canvas's own for Original, the W and H fields' for the
    /// W x H x Resolution preset, and the bare `aspect` for Free.
    pub fn effective_aspect(&self) -> Option<f32> {
        let ratio = match self.ratio {
            0 => self.aspect?,
            CROP_RATIO_ORIGINAL => {
                let c = self.canvas?;
                c.width as f32 / c.height.max(1) as f32
            }
            CROP_RATIO_SIZE => self.output_width / self.output_height,
            i => CROP_RATIO_PRESETS.get(i)?.1,
        };
        (ratio.is_finite() && ratio > 0.0).then_some(ratio)
    }

    /// W4-D: the exact pixel size the W x H x Resolution preset asks for, or
    /// `None` under every other preset.
    pub fn output_size(&self) -> Option<(u32, u32)> {
        if self.ratio != CROP_RATIO_SIZE {
            return None;
        }
        let scale = if self.output_in_inches {
            self.resolution
        } else {
            1.0
        };
        let px = |v: f32| {
            let p = (v * scale).round();
            (p.is_finite() && p >= 1.0).then(|| p.min(CROP_MAX_OUTPUT_PX) as u32)
        };
        Some((px(self.output_width)?, px(self.output_height)?))
    }

    /// Apply the aspect lock to a drag, inside `canvas`.
    ///
    /// Round 2 (W4-D): the lock used to be applied first and the canvas clip
    /// after it, one axis at a time, so a locked box that ran past an edge
    /// lost its ratio (a 16:9 drag to the far corner came out 60x56). The
    /// anchor is now brought onto the canvas, and the locked box is shrunk
    /// *uniformly* until it fits the room between the anchor and the canvas
    /// edges it is dragged towards — the ratio survives the clip.
    fn constrained(&self, a: Vec2, b: Vec2, canvas: Option<PixelRect>) -> (Vec2, Vec2) {
        let Some(aspect) = self.effective_aspect() else {
            return (a, b);
        };
        let a = match canvas {
            Some(c) => a.clamp(
                Vec2::new(c.x as f32, c.y as f32),
                Vec2::new(c.right() as f32, c.bottom() as f32),
            ),
            None => a,
        };
        let d = b - a;
        let (sx, sy) = (
            if d.x < 0.0 { -1.0 } else { 1.0 },
            if d.y < 0.0 { -1.0 } else { 1.0 },
        );
        // Keep whichever extent the user dragged further, and derive the other.
        let (mut w, mut h) = if (d.x.abs() / aspect) >= d.y.abs() {
            (d.x.abs(), d.x.abs() / aspect)
        } else {
            (d.y.abs() * aspect, d.y.abs())
        };
        if let Some(c) = canvas {
            let room_x = if sx < 0.0 {
                a.x - c.x as f32
            } else {
                c.right() as f32 - a.x
            };
            let room_y = if sy < 0.0 {
                a.y - c.y as f32
            } else {
                c.bottom() as f32 - a.y
            };
            let fit = (room_x / w).min(room_y / h).min(1.0);
            if fit.is_finite() {
                w *= fit.max(0.0);
                h *= fit.max(0.0);
            }
        }
        (a, Vec2::new(a.x + w * sx, a.y + h * sy))
    }

    /// The keep-region a drag describes, on the canvas. With a ratio lock
    /// the box keeps its ratio to the nearest whole pixel however far past
    /// the canvas the drag ran.
    pub fn rect_for(&self, ctx: &ToolContext<'_>, a: Vec2, b: Vec2) -> Option<PixelRect> {
        let (a, b) = self.constrained(a, b, self.clip_to(Some(ctx.canvas)));
        if !a.x.is_finite() || !b.x.is_finite() || !a.y.is_finite() || !b.y.is_finite() {
            return None;
        }
        let (x0, y0, x1, y1) = if self.effective_aspect().is_some() {
            // Locked: round both corners, so the whole-pixel box is off the
            // ratio by under a pixel rather than by an outward snap on each
            // side.
            (
                a.x.min(b.x).round() as i64,
                a.y.min(b.y).round() as i64,
                a.x.max(b.x).round() as i64,
                a.y.max(b.y).round() as i64,
            )
        } else {
            (
                a.x.min(b.x).floor() as i64,
                a.y.min(b.y).floor() as i64,
                a.x.max(b.x).ceil() as i64,
                a.y.max(b.y).ceil() as i64,
            )
        };
        // W13-I: with Content-Aware on the box keeps what it covers past the
        // canvas (up to the largest canvas a crop may make).
        let (x0, y0, x1, y1) = match self.clip_to(Some(ctx.canvas)) {
            Some(c) => (
                x0.max(c.x),
                y0.max(c.y),
                x1.min(c.right()),
                y1.min(c.bottom()),
            ),
            None => {
                let max = CROP_MAX_OUTPUT_PX as i64;
                (x0, y0, x1.min(x0 + max), y1.min(y0 + max))
            }
        };
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
    }

    /// W4-A: the box to draw, `[min, max]` in document pixels — the drag in
    /// progress (aspect-locked, clipped to the canvas), else the released
    /// box waiting for Enter.
    fn live_box(&self) -> Option<[Vec2; 2]> {
        // W4-D round 2: while a Straighten line is dragged with no box yet,
        // the box shown is the whole canvas — exactly what Enter will keep.
        if let (true, None, Some(c)) = (
            self.straighten_line && self.anchor.is_some(),
            self.box_rect,
            self.canvas,
        ) {
            return Some([
                Vec2::new(c.x as f32, c.y as f32),
                Vec2::new(c.right() as f32, c.bottom() as f32),
            ]);
        }
        // W4-D: a Straighten line is not a box; the released box stays up.
        if let (false, Some(a), Some(b)) = (self.straighten_line, self.anchor, self.current) {
            let (a, b) = self.constrained(a, b, self.clip_to(self.canvas));
            let (mut lo, mut hi) = (a.min(b), a.max(b));
            if let Some(c) = self.clip_to(self.canvas) {
                let (cmin, cmax) = (
                    Vec2::new(c.x as f32, c.y as f32),
                    Vec2::new(c.right() as f32, c.bottom() as f32),
                );
                lo = lo.clamp(cmin, cmax);
                hi = hi.clamp(cmin, cmax);
            }
            return (lo.is_finite() && hi.is_finite()).then_some([lo, hi]);
        }
        let r = self.box_rect?;
        Some([
            Vec2::new(r.x as f32, r.y as f32),
            Vec2::new(r.right() as f32, r.bottom() as f32),
        ])
    }

    /// W4-D round 2: the straighten line to draw — the one being dragged in
    /// Straighten mode, else the released one waiting for Enter.
    fn live_line(&self) -> Option<[Vec2; 2]> {
        if let (true, Some(a), Some(b)) = (self.straighten_line, self.anchor, self.current) {
            return (a.is_finite() && b.is_finite()).then_some([a, b]);
        }
        self.line
    }

    /// Confirm the crop (Enter).
    pub fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let rect = self.box_rect.take().ok_or(ToolError::Degenerate)?;
        self.line = None;
        ctx.emit_request(ToolRequest::Crop(CropRequest {
            rect,
            straighten: self.line_angle.take().unwrap_or(self.straighten),
            delete_cropped: self.delete_cropped,
            output_size: self.output_size(),
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
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("crop anchor", event.pos)?;
        self.anchor = Some(event.pos);
        self.current = Some(event.pos);
        self.canvas = Some(ctx.canvas);
        Ok(())
    }

    /// W4-A: the crop box while it is dragged and while it waits for Enter;
    /// `None` once committed or cancelled.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        Some(crate::tool::SessionGeometry::Crop {
            rect: self.live_box()?,
            guide: self.overlay,
            straighten: self.live_line(),
        })
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
        // W4-D: in Straighten mode the drag was a line along something that
        // should be level. Its angle is the straighten of the next commit, and
        // with no box yet the whole canvas is kept, so Enter straightens it.
        if self.straighten_line {
            if let Some(angle) = straighten_angle(a, event.pos) {
                self.line_angle = Some(angle);
                self.line = Some([a, event.pos]);
                if self.box_rect.is_none() {
                    self.box_rect = Some(ctx.canvas);
                }
            }
            return Ok(());
        }
        // Releasing sets the box; the crop itself waits for Enter, so the user
        // can nudge the edges first.
        self.box_rect = self.rect_for(ctx, a, event.pos);
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchor = None;
        self.current = None;
        self.box_rect = None;
        self.line_angle = None;
        self.line = None;
    }

    /// The registry's Crop options (W4-D): the Ratio preset, the W x H x
    /// Resolution fields, the Overlay, Straighten mode and Delete Cropped
    /// Pixels. `aspect` (width/height, `0` = unconstrained, used under the
    /// Free preset) and `straighten` (radians) are still answered for callers
    /// that set them directly.
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
            ("ratio", ToolSetting::Choice(i)) => {
                self.ratio = i.min(CROP_RATIO_PRESETS.len() - 1);
                Ok(())
            }
            ("width", ToolSetting::Float(v)) => {
                self.output_width = finite("crop width", v)?.clamp(0.001, CROP_MAX_OUTPUT_PX);
                Ok(())
            }
            ("height", ToolSetting::Float(v)) => {
                self.output_height = finite("crop height", v)?.clamp(0.001, CROP_MAX_OUTPUT_PX);
                Ok(())
            }
            ("units", ToolSetting::Choice(i)) => {
                self.output_in_inches = i == 1;
                Ok(())
            }
            ("resolution", ToolSetting::Float(v)) => {
                self.resolution = finite("crop resolution", v)?.clamp(1.0, 10_000.0);
                Ok(())
            }
            ("overlay", ToolSetting::Choice(i)) => {
                self.overlay = CROP_OVERLAYS
                    .get(i)
                    .map(|(_, guide)| *guide)
                    .unwrap_or_default();
                Ok(())
            }
            ("straighten_line", ToolSetting::Bool(v)) => {
                self.straighten_line = v;
                Ok(())
            }
            (CROP_CONTENT_AWARE_KEY, ToolSetting::Bool(v)) => {
                self.content_aware = v;
                Ok(())
            }
            (
                "aspect"
                | "straighten"
                | "delete_cropped"
                | "ratio"
                | "width"
                | "height"
                | "units"
                | "resolution"
                | "overlay"
                | "straighten_line"
                | CROP_CONTENT_AWARE_KEY,
                _,
            ) => Err(kind_mismatch(key)),
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
    /// W4-A: the pointer during a drag, for the live region.
    current: Option<Vec2>,
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
        self.current = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.anchor.is_some() && event.pos.is_finite() {
            self.current = Some(event.pos);
        }
        Ok(())
    }

    /// W4-A: every slice waiting for Enter, then the one being dragged.
    /// `None` when there are none — committed, cancelled or never drawn.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        let mut rects: Vec<[Vec2; 2]> = self
            .slices
            .iter()
            .map(|s| {
                [
                    Vec2::new(s.rect.x as f32, s.rect.y as f32),
                    Vec2::new(s.rect.right() as f32, s.rect.bottom() as f32),
                ]
            })
            .collect();
        if let (Some(a), Some(b)) = (self.anchor, self.current) {
            rects.push([a.min(b), a.max(b)]);
        }
        (!rects.is_empty()).then_some(crate::tool::SessionGeometry::Slices { rects })
    }

    /// Records the drag as one more slice. Publishing waits for
    /// [`SliceTool::commit`] — see the type's contract.
    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.current = None;
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
        self.current = None;
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
    /// W13-I: the options bar's Sample choice
    /// ([`crate::tool::SAMPLE_LAYERS_KEY`]): Current Layer, Current & Below
    /// or All Layers. The last two read the composite the shell lends at the
    /// press ([`ToolContext::composite_sampler`]), kept for the drag.
    pub sample_layers: Option<crate::tool::SampleLayers>,
    sampler: Option<std::sync::Arc<dyn crate::tool::CompositeSampler>>,
}

impl Default for EyedropperTool {
    fn default() -> Self {
        Self {
            sample_radius: 0,
            sample_all_layers: true,
            active: false,
            // The registry's default Sample choice (All Layers).
            sample_layers: Some(crate::tool::SampleLayers::All),
            sampler: None,
        }
    }
}

impl EyedropperTool {
    pub fn new(sample_radius: u32, sample_all_layers: bool) -> Self {
        Self {
            sample_radius,
            sample_all_layers,
            active: false,
            sample_layers: None,
            sampler: None,
        }
    }

    /// W13-I: the averaged colour of the lent composite of `layers` over the
    /// sample square at `p`, or `None` when no composite applies (Current
    /// Layer, no Sample choice set, or no sampler lent — a mask edit).
    fn sample_composite(
        &self,
        ctx: &ToolContext<'_>,
        p: Vec2,
    ) -> Option<Result<[f32; 4], ToolError>> {
        let layers = self.sample_layers?;
        if layers == crate::tool::SampleLayers::Current {
            return None;
        }
        let sampler = self.sampler.as_ref()?;
        // The composite is addressed in the paint target's pixels.
        let p = match ctx.sample_to_layer {
            Some(m) => m.transform_point2(p),
            None => p,
        };
        let c = IVec2::new(p.x.floor() as i32, p.y.floor() as i32);
        let r = self.sample_radius.min(64) as i32;
        let side = (r * 2 + 1) as u32;
        let rect = PixelRect::new((c.x - r) as i64, (c.y - r) as i64, side, side);
        Some(sampler.composite(layers, rect).map(|buf| {
            let mut acc = [0.0f64; 4];
            for px in buf.pixels() {
                for i in 0..4 {
                    acc[i] += f64::from(px[i]);
                }
            }
            let n = buf.pixels().len().max(1) as f64;
            unpremultiply([
                (acc[0] / n) as f32,
                (acc[1] / n) as f32,
                (acc[2] / n) as f32,
                (acc[3] / n) as f32,
            ])
        }))
    }

    /// The straight-alpha linear colour under `p`.
    ///
    /// Averaged in linear premultiplied light: averaging encoded values would
    /// bias a mixed sample toward the darker pixels, which is why a "5 by 5
    /// average" eyedropper reads too dark in every tool that gets this wrong.
    pub fn sample(&self, ctx: &ToolContext<'_>, p: Vec2) -> Result<[f32; 4], ToolError> {
        // W13-I: Current & Below / All Layers read the lent composite.
        if let Some(picked) = self.sample_composite(ctx, p) {
            return picked;
        }
        let current = self.sample_layers == Some(crate::tool::SampleLayers::Current);
        let key = if self.sample_all_layers && !current {
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
        // W13-I: the composite the shell lent this press, kept for the drag.
        self.sampler = ctx.composite_sampler.clone();
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
    /// `sample_all_layers`, plus the W13-I Sample choice. The legacy box is
    /// mapped onto the choice (on = All Layers, off = Current Layer); all are
    /// read by [`EyedropperTool::sample`].
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("sample_radius", ToolSetting::Int(v)) => {
                self.sample_radius = v.clamp(0, 64) as u32;
                Ok(())
            }
            ("sample_all_layers", ToolSetting::Bool(v)) => {
                // W13-I round 2: the legacy box is the Sample choice's two
                // ends — on is All Layers, off is Current Layer — so a
                // caller still setting it gets what it asked for.
                self.sample_all_layers = v;
                self.sample_layers = Some(if v {
                    crate::tool::SampleLayers::All
                } else {
                    crate::tool::SampleLayers::Current
                });
                Ok(())
            }
            (crate::tool::SAMPLE_LAYERS_KEY, ToolSetting::Choice(i)) => {
                self.sample_layers = Some(crate::tool::SampleLayers::from_choice(i));
                Ok(())
            }
            ("sample_radius" | "sample_all_layers" | crate::tool::SAMPLE_LAYERS_KEY, _) => {
                Err(kind_mismatch(key))
            }
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
            } = geometry
            else {
                panic!("a Move session publishes a transform");
            };
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

        /// W13-A: an Alt+drag with a pixel selection lays a COPY of the
        /// selected pixels down at the drop and keeps the originals, as one
        /// transaction; the same drag without Alt lifts them.
        #[test]
        fn an_alt_drag_with_a_selection_copies_the_pixels_and_keeps_the_originals() {
            let red = [200, 20, 20, 255];
            let run = |alt: bool| {
                let mut tiles = MemoryTiles::new();
                let layer = layer_model::LayerId::new();
                let key = PixelKey::Layer(layer);
                for y in 8..16 {
                    for x in 8..16 {
                        tiles.put_pixel(key, x, y, red);
                    }
                }
                let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
                ctx.active_layer = Some(layer);
                ctx.selection = Selection::Rect {
                    min: IVec2::new(8, 8),
                    max: IVec2::new(16, 16),
                };
                let mods = crate::tool::Modifiers {
                    alt,
                    ..Default::default()
                };
                let mut tool = MoveTool::default();
                tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0).with_modifiers(mods))
                    .unwrap();
                assert_eq!(tool.is_copying(), alt);
                tool.on_pointer_up(&mut ctx, PointerEvent::at(30.0, 10.0).with_modifiers(mods))
                    .unwrap();
                assert!(!tool.is_copying(), "the copy flag outlived the gesture");
                let commands = ctx.drain();
                drop(ctx);
                // Land the gesture's pixels the way the shell's history does.
                let mut stack: Vec<&Command> = commands.iter().collect();
                while let Some(c) = stack.pop() {
                    match c {
                        Command::Transaction { commands, .. } => stack.extend(commands),
                        Command::PaintTiles { delta, .. } => {
                            tiles.apply_delta(key, delta);
                        }
                        _ => {}
                    }
                }
                (commands, tiles.pixel(key, 10, 10), tiles.pixel(key, 30, 10))
            };
            let (commands, original, copy) = run(true);
            assert_eq!(commands.len(), 1, "one transaction");
            let Command::Transaction { label, commands } = &commands[0] else {
                panic!("not a transaction: {:?}", commands[0]);
            };
            assert_eq!(label, "Duplicate Selection");
            let moved = commands.iter().find_map(|c| match c {
                Command::SetSelection { selection } => selection.bounds(),
                _ => None,
            });
            assert_eq!(moved, Some((IVec2::new(28, 8), IVec2::new(36, 16))));
            assert_eq!(original, red, "the original pixels left");
            assert_eq!(copy, red, "no copy at the drop");
            // Without Alt the same drag lifts the pixels.
            let (_, original, copy) = run(false);
            assert_eq!(original[3], 0, "a plain drag copied: {original:?}");
            assert_eq!(copy, red);
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

    /// W13-I round 2: the legacy `sample_all_layers` box still decides the
    /// source when the shell lends a composite sampler (it always does, with
    /// the registry default All Layers): off reads the active layer, on reads
    /// the composite.
    #[test]
    fn the_legacy_sample_all_layers_box_overrides_a_lent_composite() {
        struct Blue;
        impl crate::tool::CompositeSampler for Blue {
            fn composite(
                &self,
                _layers: crate::tool::SampleLayers,
                rect: PixelRect,
            ) -> Result<filters::FilterBuffer, ToolError> {
                let n = (rect.width * rect.height) as usize;
                Ok(filters::FilterBuffer::from_rgba8(
                    rect.width,
                    rect.height,
                    &[0, 0, 255, 255].repeat(n),
                )
                .unwrap())
            }
        }
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        paint(&mut tiles, PixelKey::Layer(layer), |_, _| [255, 0, 0, 255]);
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
        ctx.composite_sampler = Some(std::sync::Arc::new(Blue));
        let mut pick = |tool: &mut EyedropperTool| {
            tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(8.0, 8.0))
                .unwrap();
            ctx.picked().unwrap()
        };
        let mut tool = EyedropperTool::default();
        assert!(pick(&mut tool)[2] > 0.99, "the default reads the composite");
        tool.set_setting("sample_all_layers", ToolSetting::Bool(false))
            .unwrap();
        let off = pick(&mut tool);
        assert!(
            off[0] > 0.99 && off[2] < 0.01,
            "Sample All Layers off reads the red layer: {off:?}"
        );
        tool.set_setting("sample_all_layers", ToolSetting::Bool(true))
            .unwrap();
        assert!(pick(&mut tool)[2] > 0.99, "on reads the composite again");
    }

    /// W13-I: with Content-Aware on (the registry's key, through
    /// `set_setting`) a box dragged past the canvas keeps what it covers
    /// there; off, it is clipped to the canvas as before.
    #[test]
    fn content_aware_lets_the_crop_box_run_past_the_canvas() {
        let released = |content_aware: bool| {
            let mut tiles = crate::tiles::MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            let mut tool = crate::registry::make(ToolId::Crop);
            assert!(crate::registry::info(ToolId::Crop)
                .unwrap()
                .options
                .iter()
                .any(|o| o.key == CROP_CONTENT_AWARE_KEY));
            tool.set_setting(CROP_CONTENT_AWARE_KEY, ToolSetting::Bool(content_aware))
                .unwrap();
            tool.on_pointer_down(&mut ctx, PointerEvent::at(4.0, 4.0))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(80.0, 60.0))
                .unwrap();
            let Some(crate::tool::SessionGeometry::Crop { rect, .. }) = tool.live_geometry() else {
                panic!("no live crop box");
            };
            tool.on_pointer_up(&mut ctx, PointerEvent::at(80.0, 60.0))
                .unwrap();
            tool.commit(&mut ctx).unwrap();
            let Some(ToolRequest::Crop(req)) = ctx.drain_requests().pop() else {
                panic!("no crop request");
            };
            (rect[1].x, req.rect)
        };
        let (live, rect) = released(true);
        assert_eq!(live, 80.0, "the live box runs past the canvas");
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (4, 4, 76, 56));
        let (live, rect) = released(false);
        assert_eq!(live, 64.0);
        assert_eq!((rect.x, rect.y, rect.width, rect.height), (4, 4, 60, 56));
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
