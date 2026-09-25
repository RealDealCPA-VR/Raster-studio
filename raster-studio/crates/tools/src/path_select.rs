//! Path Select (`A`) and Direct Selection (`Shift`+`A`).
//!
//! Paths live on their shape layers as `path_svg` (see
//! `layer_model::ShapeLayer`), so "selecting a path" is selecting the layer
//! that owns it, and "moving an anchor" is rewriting that layer's path and
//! committing it as one [`Command::SetLayerKind`] step — the parametric edit
//! the layer re-rasterises from. [`crate::vector::parse`] lowers the SVG to
//! elements; both tools speak elements, not SVG text.
//!
//! There is no document-level path store to select through: Photopea's paths
//! ride their layers, and so do ours.
//!
//! # Adding, deleting and converting anchors
//!
//! Direct Selection answers the Pen's anchor edits with a modifier click on
//! the active shape layer's path, each one [`Command::SetLayerKind`] step:
//!
//! * **Shift**-click on the outline adds an anchor there, splitting the
//!   segment so the outline does not move ([`vector::anchors::insert_anchor`]).
//! * **Ctrl**-click on an anchor deletes it; a path is never cut below a line
//!   (open) or a triangle (closed).
//! * **Alt**-click on an anchor converts it: a smooth anchor becomes a corner
//!   and a corner grows a symmetric pair of handles.
//!
//! # W9-F: combining and aligning path components
//!
//! Path Select's options bar carries Photoshop's two component operations as
//! choices, applied to the shape layer the click selects, as one
//! [`Command::SetLayerKind`] step:
//!
//! * **Combine** (`combine`: Unite / Subtract / Intersect / Exclude) merges the
//!   path's components into one outline with that boolean operation, folded
//!   bottom (first drawn) to top ([`vector::fold`]) — Photoshop's "Merge Shape
//!   Components".
//! * **Align** (`align`: Left / Horizontal Centres / Right / Top / Vertical
//!   Centres / Bottom) moves each component so that edge meets the same edge
//!   of all the components' joint bounds.
//!
//! Left on their first entry (`Select Only` / `None`) a click only selects.

use glam::Vec2;

use editor_core::Command;
use layer_model::{LayerId, LayerKind};
use vector::{anchors, svg, FillRule, PathEl, Point};

use crate::error::ToolError;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};

/// How near the pointer a path's outline must pass, in document pixels, for a
/// click to land on it.
const HIT_TOLERANCE: f64 = 6.0;

/// How near the pointer an anchor must sit, in document pixels, for Direct
/// Selection to grab it.
const ANCHOR_RADIUS: f64 = 8.0;

/// W9-F: how Path Select combines the clicked path's components.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathCombine {
    /// Select only.
    #[default]
    None,
    Unite,
    /// Subtract each later component from the ones before it.
    Subtract,
    Intersect,
    Exclude,
}

impl PathCombine {
    /// The `combine` option's labels, in [`Self::from_choice`]'s order.
    pub const CHOICES: &'static [&'static str] = &[
        "Select Only",
        "Unite",
        "Subtract Front",
        "Intersect",
        "Exclude",
    ];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => PathCombine::None,
            1 => PathCombine::Unite,
            2 => PathCombine::Subtract,
            3 => PathCombine::Intersect,
            _ => PathCombine::Exclude,
        }
    }

    /// The boolean operation, `None` for select-only.
    pub fn op(self) -> Option<vector::BoolOp> {
        match self {
            PathCombine::None => None,
            PathCombine::Unite => Some(vector::BoolOp::Union),
            PathCombine::Subtract => Some(vector::BoolOp::Difference),
            PathCombine::Intersect => Some(vector::BoolOp::Intersection),
            PathCombine::Exclude => Some(vector::BoolOp::Xor),
        }
    }
}

/// W9-F: which edge Path Select aligns the clicked path's components on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathAlign {
    #[default]
    None,
    Left,
    HorizontalCenter,
    Right,
    Top,
    VerticalCenter,
    Bottom,
}

impl PathAlign {
    /// The `align` option's labels, in [`Self::from_choice`]'s order.
    pub const CHOICES: &'static [&'static str] = &[
        "None",
        "Left Edges",
        "Horizontal Centres",
        "Right Edges",
        "Top Edges",
        "Vertical Centres",
        "Bottom Edges",
    ];

    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => PathAlign::None,
            1 => PathAlign::Left,
            2 => PathAlign::HorizontalCenter,
            3 => PathAlign::Right,
            4 => PathAlign::Top,
            5 => PathAlign::VerticalCenter,
            _ => PathAlign::Bottom,
        }
    }
}

/// W9-F: a path's components — one path per `MoveTo` run.
pub fn components(path: &vector::Path) -> Vec<vector::Path> {
    let mut out: Vec<Vec<PathEl>> = Vec::new();
    for el in path.elements() {
        match el {
            PathEl::MoveTo(_) => out.push(vec![*el]),
            _ => match out.last_mut() {
                Some(run) => run.push(*el),
                None => out.push(vec![*el]),
            },
        }
    }
    out.into_iter().map(vector::Path::from_elements).collect()
}

/// W9-F: merge a path's components with `op`, first drawn at the bottom.
/// `None` when there is nothing to merge (one component) or nothing
/// survives.
pub fn combine_components(
    path: &vector::Path,
    op: vector::BoolOp,
) -> Result<Option<vector::Path>, ToolError> {
    let parts = components(path);
    if parts.len() < 2 {
        return Ok(None);
    }
    let merged = vector::fold(&parts, op, FillRule::NonZero)?;
    Ok((!merged.is_empty()).then_some(merged))
}

/// W9-F: move each component of `path` so its `edge` meets the same edge of
/// all the components' joint bounds. `None` when nothing moves.
pub fn align_components(path: &vector::Path, edge: PathAlign) -> Option<vector::Path> {
    let parts = components(path);
    if parts.len() < 2 || edge == PathAlign::None {
        return None;
    }
    let all = path.bounds();
    let mut out = vector::Path::new();
    let mut moved = false;
    for part in &parts {
        let b = part.bounds();
        let (dx, dy) = match edge {
            PathAlign::None => (0.0, 0.0),
            PathAlign::Left => (all.min.x - b.min.x, 0.0),
            PathAlign::Right => (all.max.x - b.max.x, 0.0),
            PathAlign::HorizontalCenter => ((all.min.x + all.max.x - b.min.x - b.max.x) * 0.5, 0.0),
            PathAlign::Top => (0.0, all.min.y - b.min.y),
            PathAlign::Bottom => (0.0, all.max.y - b.max.y),
            PathAlign::VerticalCenter => (0.0, (all.min.y + all.max.y - b.min.y - b.max.y) * 0.5),
        };
        moved |= dx != 0.0 || dy != 0.0;
        out.extend(&part.transform(&vector::Affine::translate(dx, dy)));
    }
    moved.then_some(out)
}

/// W9-F: the geometry of Layer > Combine Shapes. `stack` is the selected
/// shape layers bottom first, each with its layer-to-document transform; the
/// paths are taken into document space, folded bottom to top with
/// `combine`'s boolean op ([`vector::fold`], each read with the bottom
/// layer's fill rule) and brought back into the bottom layer's own space.
/// Returns that SVG path data; [`ToolError::Degenerate`] when fewer than two
/// layers are given, `combine` is select-only, a transform cannot be
/// inverted, or nothing of the shapes remains.
pub fn combine_shape_layers(
    stack: &[(&layer_model::ShapeLayer, glam::Affine2)],
    combine: PathCombine,
) -> Result<String, ToolError> {
    let (Some(op), Some((base, base_to_doc))) = (combine.op(), stack.first()) else {
        return Err(ToolError::Degenerate);
    };
    if stack.len() < 2 {
        return Err(ToolError::Degenerate);
    }
    let affine = |t: glam::Affine2| {
        let m = t.matrix2;
        vector::Affine::new([
            f64::from(m.x_axis.x),
            f64::from(m.x_axis.y),
            f64::from(m.y_axis.x),
            f64::from(m.y_axis.y),
            f64::from(t.translation.x),
            f64::from(t.translation.y),
        ])
    };
    let mut paths = Vec::with_capacity(stack.len());
    for (shape, to_doc) in stack {
        let path = svg::parse(&shape.path_svg)?;
        paths.push(path.transform(&affine(*to_doc)));
    }
    let rule = match base.fill_rule {
        layer_model::ShapeFillRule::NonZero => FillRule::NonZero,
        layer_model::ShapeFillRule::EvenOdd => FillRule::EvenOdd,
    };
    let merged = vector::fold(&paths, op, rule)?;
    let back = base_to_doc.inverse();
    if merged.is_empty() || !back.is_finite() {
        return Err(ToolError::Degenerate);
    }
    Ok(svg::to_svg(&merged.transform(&affine(back))))
}

/// W16-F: what Path Select's options-bar Arrange and Delete buttons do to the
/// selected path components (Photopea learn/vg-manipulation: "delete them by
/// pressing Delete ... reorder paths with the Up and Down button"). A
/// component's place in the path is its stacking order: the first drawn is
/// at the bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentOp {
    BringToFront,
    BringForward,
    SendBackward,
    SendToBack,
    Delete,
}

impl ComponentOp {
    /// Every operation, in the options bar's order.
    pub const ALL: [ComponentOp; 5] = [
        ComponentOp::BringToFront,
        ComponentOp::BringForward,
        ComponentOp::SendBackward,
        ComponentOp::SendToBack,
        ComponentOp::Delete,
    ];
}

thread_local! {
    /// W16-F: an Arrange / Delete button press, parked by the options bar
    /// ([`request_component_op`]) for the live Path Select tool to perform
    /// at the confirm the same press raises (`Intent::ConfirmTool`, the
    /// road Enter takes into [`Tool::commit`]).
    static PENDING_COMPONENT_OP: std::cell::Cell<Option<ComponentOp>> =
        const { std::cell::Cell::new(None) };
}

/// W16-F: park `op` for the Path Select tool's next confirm.
pub fn request_component_op(op: ComponentOp) {
    PENDING_COMPONENT_OP.with(|slot| slot.set(Some(op)));
}

/// W16-F: the parked Arrange / Delete operation, if any (not taken).
pub fn pending_component_op() -> Option<ComponentOp> {
    PENDING_COMPONENT_OP.with(|slot| slot.get())
}

fn take_component_op() -> Option<ComponentOp> {
    PENDING_COMPONENT_OP.with(|slot| slot.take())
}

/// W16-F: apply `op` to the components `picked` (indices into
/// [`components`]) of `path`. Returns the new path and where the picked
/// components now sit; `None` when nothing changes, an index is out of
/// range, or a Delete would leave the path empty (the layer keeps at least
/// one component; Layer > Delete removes a layer).
pub fn apply_component_op(
    path: &vector::Path,
    picked: &[usize],
    op: ComponentOp,
) -> Option<(vector::Path, Vec<usize>)> {
    let parts = components(path);
    let n = parts.len();
    if picked.is_empty() || picked.iter().any(|i| *i >= n) {
        return None;
    }
    let is_picked = |i: usize| picked.contains(&i);
    let mut order: Vec<usize> = (0..n).collect();
    match op {
        ComponentOp::Delete => {
            order.retain(|i| !is_picked(*i));
            if order.is_empty() {
                return None;
            }
        }
        ComponentOp::BringToFront => {
            order.sort_by_key(|i| is_picked(*i));
        }
        ComponentOp::SendToBack => {
            order.sort_by_key(|i| !is_picked(*i));
        }
        ComponentOp::BringForward => {
            for k in (0..n.saturating_sub(1)).rev() {
                if is_picked(order[k]) && !is_picked(order[k + 1]) {
                    order.swap(k, k + 1);
                }
            }
        }
        ComponentOp::SendBackward => {
            for k in 1..n {
                if is_picked(order[k]) && !is_picked(order[k - 1]) {
                    order.swap(k, k - 1);
                }
            }
        }
    }
    if order == (0..n).collect::<Vec<_>>() {
        return None;
    }
    let mut out = vector::Path::new();
    for i in &order {
        out.extend(&parts[*i]);
    }
    let now: Vec<usize> = order
        .iter()
        .enumerate()
        .filter(|(_, i)| is_picked(**i))
        .map(|(k, _)| k)
        .collect();
    Some((out, now))
}

/// W16-F: `path` with the components `picked` moved by `(dx, dy)`.
pub fn translate_components(
    path: &vector::Path,
    picked: &[usize],
    dx: f64,
    dy: f64,
) -> vector::Path {
    let mut out = vector::Path::new();
    for (i, part) in components(path).iter().enumerate() {
        if picked.contains(&i) {
            out.extend(&part.transform(&vector::Affine::translate(dx, dy)));
        } else {
            out.extend(part);
        }
    }
    out
}

/// Path Select: click a path component to select it (Photopea: "Click on the
/// path to select it, or hold Shift to select multiple paths").
///
/// W16-F: the selection is per COMPONENT of a shape layer's path (one
/// `MoveTo` run), not the whole layer. Layers are tested top-most first (the
/// order the shell fills [`ToolContext::shape_paths`] in) and components top
/// (last drawn) first; a hit is a click within [`HIT_TOLERANCE`] of the
/// component's outline or inside its fill. The click also selects the layer
/// that owns it. A drag moves the selected components as ONE
/// [`Command::SetLayerKind`] step; the arrow keys nudge them (`nudge_x` /
/// `nudge_y`, then [`Tool::commit`]); the options bar's Arrange and Delete
/// buttons reorder or delete them ([`ComponentOp`]). W9-F: with a `combine`
/// or `align` option set, the click merges or aligns the whole path's
/// components instead.
#[derive(Default)]
pub struct PathSelectTool {
    pub combine: PathCombine,
    pub align: PathAlign,
    /// W16-F: the selected components: the layer, and indices into
    /// [`components`] of its path.
    selected: Option<(LayerId, Vec<usize>)>,
    /// W16-F: the running drag: where it was pressed and where it is now.
    drag: Option<(Vec2, Vec2)>,
    /// W16-F: an arrow nudge waiting for [`Tool::commit`].
    pending_nudge: Vec2,
}

impl PathSelectTool {
    /// W16-F: the topmost path component under `p`: its layer and its index.
    pub fn component_under(&self, ctx: &ToolContext<'_>, p: Vec2) -> Option<(LayerId, usize)> {
        let point = Point::new(p.x as f64, p.y as f64);
        for (id, shape) in &ctx.shape_paths {
            let Ok(path) = svg::parse(&shape.path_svg) else {
                continue;
            };
            for (i, part) in components(&path).iter().enumerate().rev() {
                if vector::hit_stroke(part, point, HIT_TOLERANCE)
                    || vector::hit::contains(part, point, FillRule::NonZero)
                {
                    return Some((*id, i));
                }
            }
        }
        None
    }

    /// W16-F: the selected components, as `(layer, indices)`.
    pub fn selected_components(&self) -> Option<(LayerId, &[usize])> {
        self.selected
            .as_ref()
            .map(|(layer, picked)| (*layer, picked.as_slice()))
    }

    /// W16-F: rewrite the selected components' layer with `edit` (given the
    /// parsed path and the picked indices, answering the new path and the new
    /// indices), as ONE `SetLayerKind`. A selection the path no longer has
    /// (an undo removed a component) is dropped rather than applied.
    fn edit_selected(
        &mut self,
        ctx: &mut ToolContext<'_>,
        edit: impl FnOnce(&vector::Path, &[usize]) -> Option<(vector::Path, Vec<usize>)>,
    ) -> bool {
        let Some((layer, picked)) = self.selected.clone() else {
            return false;
        };
        let Some((_, shape)) = ctx.shape_paths.iter().find(|(l, _)| *l == layer) else {
            self.selected = None;
            return false;
        };
        let Ok(path) = svg::parse(&shape.path_svg) else {
            return false;
        };
        if picked.iter().any(|i| *i >= components(&path).len()) {
            self.selected = None;
            return false;
        }
        let Some((next, now)) = edit(&path, &picked) else {
            return false;
        };
        let mut shape = shape.clone();
        shape.path_svg = svg::to_svg(&next);
        self.selected = (!now.is_empty()).then_some((layer, now));
        ctx.emit(Command::SetLayerKind {
            layer_id: layer,
            kind: Box::new(LayerKind::Shape(shape)),
        });
        true
    }

    /// W16-F: move the selected components by `d` document pixels.
    fn move_selected(&mut self, ctx: &mut ToolContext<'_>, d: Vec2) -> bool {
        if !d.is_finite() || d == Vec2::ZERO {
            return false;
        }
        self.edit_selected(ctx, |path, picked| {
            Some((
                translate_components(path, picked, d.x as f64, d.y as f64),
                picked.to_vec(),
            ))
        })
    }
}

impl Tool for PathSelectTool {
    fn id(&self) -> ToolId {
        ToolId::PathSelect
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        // A button pressed with nothing selected must not fire later.
        take_component_op();
        let Some((id, part)) = self.component_under(ctx, event.pos) else {
            if !event.modifiers.shift {
                self.selected = None;
            }
            return Ok(());
        };
        ctx.emit_request(crate::tool::ToolRequest::SelectLayer(id));
        if self.combine.op().is_some() || self.align != PathAlign::None {
            // W9-F: the whole path's components merge or align; the indices
            // change under them, so nothing stays selected.
            self.selected = None;
            return self.apply_component_ops(ctx, id);
        }
        match &mut self.selected {
            Some((layer, picked)) if *layer == id && event.modifiers.shift => {
                if let Some(at) = picked.iter().position(|i| *i == part) {
                    picked.remove(at);
                    if picked.is_empty() {
                        self.selected = None;
                    }
                    // Shift-clicking a selected component deselects it; no drag.
                    return Ok(());
                }
                picked.push(part);
            }
            Some((layer, picked)) if *layer == id && picked.contains(&part) => {}
            _ => self.selected = Some((id, vec![part])),
        }
        if event.pos.is_finite() {
            self.drag = Some((event.pos, event.pos));
        }
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some((_, now)) = &mut self.drag {
            if event.pos.is_finite() {
                *now = event.pos;
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some((start, now)) = self.drag.take() else {
            return Ok(());
        };
        let end = if event.pos.is_finite() {
            event.pos
        } else {
            now
        };
        self.move_selected(ctx, end - start);
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.drag = None;
        self.pending_nudge = Vec2::ZERO;
    }

    fn is_active(&self) -> bool {
        self.drag.is_some()
    }

    /// W16-F: an arrow nudge ([`Self::pending_nudge`]) or a parked Arrange /
    /// Delete ([`request_component_op`]) waits on selected components.
    fn has_pending_commit(&self) -> bool {
        self.selected.is_some()
            && (self.pending_nudge != Vec2::ZERO || pending_component_op().is_some())
    }

    /// W16-F: perform the waiting nudge, then the parked Arrange / Delete,
    /// on the selected components — each ONE `SetLayerKind` step.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let nudge = std::mem::take(&mut self.pending_nudge);
        self.move_selected(ctx, nudge);
        if let Some(op) = take_component_op() {
            self.edit_selected(ctx, |path, picked| apply_component_op(path, picked, op));
        }
        Ok(())
    }

    /// W9-F: `combine` and `align`, the two options the registry declares;
    /// W16-F: and `nudge_x` / `nudge_y`, an arrow key's nudge (not options:
    /// the shell sends them, then [`Tool::commit`] applies them).
    fn set_setting(
        &mut self,
        key: &str,
        setting: crate::tool::ToolSetting,
    ) -> Result<(), ToolError> {
        match (key, setting) {
            ("combine", crate::tool::ToolSetting::Choice(i)) => {
                self.combine = PathCombine::from_choice(i);
                Ok(())
            }
            ("align", crate::tool::ToolSetting::Choice(i)) => {
                self.align = PathAlign::from_choice(i);
                Ok(())
            }
            (NUDGE_X, crate::tool::ToolSetting::Float(v)) => {
                self.pending_nudge.x += crate::error::finite("nudge", v)?;
                Ok(())
            }
            (NUDGE_Y, crate::tool::ToolSetting::Float(v)) => {
                self.pending_nudge.y += crate::error::finite("nudge", v)?;
                Ok(())
            }
            ("combine" | "align" | NUDGE_X | NUDGE_Y, _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

/// W16-F: the keys an arrow key's nudge reaches Path Select and Direct
/// Selection under (document pixels), applied by [`Tool::commit`]. The same
/// spelling as Free Transform's ([`crate::transform::keys::NUDGE_X`]).
pub const NUDGE_X: &str = crate::transform::keys::NUDGE_X;
pub const NUDGE_Y: &str = crate::transform::keys::NUDGE_Y;

impl PathSelectTool {
    /// W9-F: merge then align the clicked layer's path components, as one
    /// `SetLayerKind` step; nothing when neither option changes the path.
    fn apply_component_ops(&self, ctx: &mut ToolContext<'_>, id: LayerId) -> Result<(), ToolError> {
        let Some((_, shape)) = ctx.shape_paths.iter().find(|(l, _)| *l == id) else {
            return Ok(());
        };
        let Ok(mut path) = svg::parse(&shape.path_svg) else {
            return Ok(());
        };
        let mut changed = false;
        if let Some(op) = self.combine.op() {
            if let Some(merged) = combine_components(&path, op)? {
                path = merged;
                changed = true;
            }
        }
        if let Some(aligned) = align_components(&path, self.align) {
            path = aligned;
            changed = true;
        }
        if changed {
            let mut next = shape.clone();
            next.path_svg = svg::to_svg(&path);
            ctx.emit(Command::SetLayerKind {
                layer_id: id,
                kind: Box::new(LayerKind::Shape(next)),
            });
        }
        Ok(())
    }
}

/// How long after a press a second press on the same knot or handle counts
/// as a double-click.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(500);

/// W16-F: what a Direct Selection press grabbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grab {
    /// A knot: the drag moves every selected knot.
    Knot(anchors::AnchorRef),
    /// A knot's incoming handle.
    HandleIn(anchors::AnchorRef),
    /// A knot's outgoing handle.
    HandleOut(anchors::AnchorRef),
}

/// W16-F: a Direct Selection drag: the layer, its knots at the press, what
/// was grabbed, where the press was and where the pointer is now.
struct KnotDrag {
    layer: LayerId,
    base: Vec<anchors::AnchorPath>,
    grab: Grab,
    start: Point,
    now: Point,
}

/// Direct Selection: select knots of the active shape layer's path and drag
/// them or their handles (Photopea learn/vg-manipulation).
///
/// W16-F: a click on a knot selects it; Shift-click adds or removes a knot
/// (knots of different components may be selected together); a drag moves
/// every selected knot, and the arrow keys nudge them (`nudge_x` / `nudge_y`,
/// then [`Tool::commit`]). A drag on a handle moves that handle.
/// Double-clicking a handle collapses it; double-clicking a knot converts it
/// ([`anchors::convert_anchor`]: a smooth knot's handles collapse, a corner
/// grows a pair back). Each release, nudge or double-click is ONE
/// [`Command::SetLayerKind`] step. Shift-click on the outline away from a
/// knot adds a knot there; Ctrl-click deletes a knot, Alt-click converts one.
#[derive(Default)]
pub struct DirectSelectionTool {
    /// The selected knots of the active shape layer's path.
    selected: Option<(LayerId, Vec<anchors::AnchorRef>)>,
    drag: Option<KnotDrag>,
    /// The last press that has not moved: what it grabbed and when, for the
    /// double-click.
    last_press: Option<(Grab, std::time::Instant)>,
    /// An arrow nudge waiting for [`Tool::commit`].
    pending_nudge: Vec2,
}

impl DirectSelectionTool {
    /// The active shape layer's parsed knots, if it has a path.
    fn active_knots(&self, ctx: &ToolContext<'_>) -> Option<(LayerId, Vec<anchors::AnchorPath>)> {
        let active = ctx.active_layer?;
        let shape = ctx.shape_paths.iter().find(|(id, _)| *id == active)?;
        let path = svg::parse(&shape.1.path_svg).ok()?;
        Some((active, anchors::from_path(&path)))
    }

    /// W16-F: the selected knots, as `(layer, knots)`.
    pub fn selected_knots(&self) -> Option<(LayerId, &[anchors::AnchorRef])> {
        self.selected
            .as_ref()
            .map(|(layer, knots)| (*layer, knots.as_slice()))
    }
}

/// W16-F: the knot or handle nearest `p` within [`ANCHOR_RADIUS`]; a knot
/// wins a tie with a handle over it.
fn grab_at(subpaths: &[anchors::AnchorPath], p: Point) -> Option<Grab> {
    let mut best: Option<(Grab, f64)> = None;
    let mut consider = |grab: Grab, at: Point| {
        let d = at.distance(p);
        if d <= ANCHOR_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((grab, d));
        }
    };
    for (s, sp) in subpaths.iter().enumerate() {
        for (i, a) in sp.anchors.iter().enumerate() {
            let r = anchors::AnchorRef {
                subpath: s,
                index: i,
            };
            consider(Grab::Knot(r), a.pos);
        }
    }
    for (s, sp) in subpaths.iter().enumerate() {
        for (i, a) in sp.anchors.iter().enumerate() {
            let r = anchors::AnchorRef {
                subpath: s,
                index: i,
            };
            if a.handle_in != Point::ZERO {
                consider(Grab::HandleIn(r), a.pos + a.handle_in);
            }
            if a.handle_out != Point::ZERO {
                consider(Grab::HandleOut(r), a.pos + a.handle_out);
            }
        }
    }
    best.map(|(g, _)| g)
}

fn knot_mut(
    subpaths: &mut [anchors::AnchorPath],
    r: anchors::AnchorRef,
) -> Option<&mut anchors::Anchor> {
    subpaths.get_mut(r.subpath)?.anchors.get_mut(r.index)
}

/// W16-F: emit `subpaths` as the new path of shape layer `layer`, ONE
/// `SetLayerKind`; nothing when the path would not change.
fn emit_knots(ctx: &mut ToolContext<'_>, layer: LayerId, subpaths: &[anchors::AnchorPath]) -> bool {
    let Some((_, shape)) = ctx.shape_paths.iter().find(|(id, _)| *id == layer) else {
        return false;
    };
    let svg = svg::to_svg(&anchors::to_path(subpaths));
    if svg == shape.path_svg {
        return false;
    }
    let mut shape = shape.clone();
    shape.path_svg = svg;
    ctx.emit(Command::SetLayerKind {
        layer_id: layer,
        kind: Box::new(LayerKind::Shape(shape)),
    });
    true
}

/// W16-F: `base` with the knots `knots` moved by `d` (their handles ride
/// along: they are offsets from the knot).
fn moved_knots(
    base: &[anchors::AnchorPath],
    knots: &[anchors::AnchorRef],
    d: Point,
) -> Vec<anchors::AnchorPath> {
    let mut out = base.to_vec();
    for r in knots {
        if let Some(a) = knot_mut(&mut out, *r) {
            a.pos += d;
        }
    }
    out
}

/// Apply one anchor edit to the active shape layer's path and emit it as one
/// [`Command::SetLayerKind`]. Nothing is emitted when there is no active shape
/// layer or `edit` declines (no anchor there, or the path is at its minimum).
fn edit_anchors(
    ctx: &mut ToolContext<'_>,
    edit: impl FnOnce(&mut [anchors::AnchorPath]) -> bool,
) -> bool {
    let Some(active) = ctx.active_layer else {
        return false;
    };
    let Some((_, shape)) = ctx.shape_paths.iter().find(|(id, _)| *id == active) else {
        return false;
    };
    let Ok(path) = svg::parse(&shape.path_svg) else {
        return false;
    };
    let mut subpaths = anchors::from_path(&path);
    if !edit(&mut subpaths) {
        return false;
    }
    let mut new_shape = shape.clone();
    new_shape.path_svg = svg::to_svg(&anchors::to_path(&subpaths));
    if new_shape.path_svg == shape.path_svg {
        return false;
    }
    ctx.emit(Command::SetLayerKind {
        layer_id: active,
        kind: Box::new(LayerKind::Shape(new_shape)),
    });
    true
}

impl Tool for DirectSelectionTool {
    fn id(&self) -> ToolId {
        ToolId::DirectSelection
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let p = Point::new(event.pos.x as f64, event.pos.y as f64);
        if !p.is_finite() {
            return Ok(());
        }
        let m = event.modifiers;
        if m.ctrl || m.alt {
            self.last_press = None;
            edit_anchors(ctx, |sps| {
                if let Some(at) = anchors::anchor_near(sps, p, ANCHOR_RADIUS) {
                    if m.ctrl {
                        anchors::delete_anchor(sps, at)
                    } else {
                        anchors::convert_anchor(sps, at)
                    }
                } else {
                    false
                }
            });
            // The knot indices may have shifted under a delete.
            self.selected = None;
            return Ok(());
        }
        let Some((layer, subpaths)) = self.active_knots(ctx) else {
            return Ok(());
        };
        let grab = grab_at(&subpaths, p);
        // W16-F: a second press on the same knot or handle within the
        // double-click time, with no drag in between.
        let now = std::time::Instant::now();
        let double = grab.is_some()
            && self
                .last_press
                .is_some_and(|(g, at)| Some(g) == grab && now.duration_since(at) <= DOUBLE_CLICK);
        if double {
            self.last_press = None;
            let mut next = subpaths.clone();
            let changed = match grab {
                Some(Grab::Knot(r)) => anchors::convert_anchor(&mut next, r),
                Some(Grab::HandleIn(r)) => knot_mut(&mut next, r)
                    .map(|a| a.handle_in = Point::ZERO)
                    .is_some(),
                Some(Grab::HandleOut(r)) => knot_mut(&mut next, r)
                    .map(|a| a.handle_out = Point::ZERO)
                    .is_some(),
                None => false,
            };
            if changed {
                emit_knots(ctx, layer, &next);
            }
            return Ok(());
        }
        self.last_press = grab.map(|g| (g, now));
        let same_layer = |sel: &Option<(LayerId, Vec<anchors::AnchorRef>)>| {
            sel.as_ref().is_some_and(|(l, _)| *l == layer)
        };
        match grab {
            None if m.shift => {
                // Shift on the outline away from a knot adds a knot there.
                self.last_press = None;
                edit_anchors(ctx, |sps| {
                    anchors::insert_anchor(sps, p, HIT_TOLERANCE).is_some()
                });
                self.selected = None;
                return Ok(());
            }
            None => {
                self.selected = None;
                return Ok(());
            }
            Some(Grab::Knot(r)) if m.shift => {
                // Shift-click adds a knot to the selection, or takes it out;
                // it never counts toward a double-click.
                self.last_press = None;
                if same_layer(&self.selected) {
                    let knots = &mut self.selected.as_mut().expect("same layer").1;
                    if let Some(at) = knots.iter().position(|k| *k == r) {
                        knots.remove(at);
                        if knots.is_empty() {
                            self.selected = None;
                        }
                        return Ok(());
                    }
                    knots.push(r);
                } else {
                    self.selected = Some((layer, vec![r]));
                }
            }
            Some(Grab::Knot(r)) => {
                let kept = same_layer(&self.selected)
                    && self
                        .selected
                        .as_ref()
                        .is_some_and(|(_, knots)| knots.contains(&r));
                if !kept {
                    self.selected = Some((layer, vec![r]));
                }
            }
            Some(Grab::HandleIn(_) | Grab::HandleOut(_)) => {}
        }
        self.drag = Some(KnotDrag {
            layer,
            base: subpaths,
            grab: grab.expect("a grab was matched"),
            start: p,
            now: p,
        });
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let p = Point::new(event.pos.x as f64, event.pos.y as f64);
        if let Some(drag) = &mut self.drag {
            if p.is_finite() {
                drag.now = p;
            }
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(mut drag) = self.drag.take() else {
            return Ok(());
        };
        let p = Point::new(event.pos.x as f64, event.pos.y as f64);
        if p.is_finite() {
            drag.now = p;
        }
        let d = drag.now - drag.start;
        if d == Point::ZERO {
            return Ok(()); // a click on a knot without a drag moved nothing
        }
        self.last_press = None;
        let next = match drag.grab {
            Grab::Knot(_) => {
                let knots = self
                    .selected
                    .as_ref()
                    .filter(|(l, _)| *l == drag.layer)
                    .map(|(_, k)| k.clone())
                    .unwrap_or_default();
                moved_knots(&drag.base, &knots, d)
            }
            Grab::HandleIn(r) => {
                let mut next = drag.base.clone();
                if let Some(a) = knot_mut(&mut next, r) {
                    a.handle_in += d;
                }
                next
            }
            Grab::HandleOut(r) => {
                let mut next = drag.base.clone();
                if let Some(a) = knot_mut(&mut next, r) {
                    a.handle_out += d;
                }
                next
            }
        };
        emit_knots(ctx, drag.layer, &next);
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.drag = None;
        self.last_press = None;
        self.pending_nudge = Vec2::ZERO;
    }

    fn is_active(&self) -> bool {
        self.drag.is_some()
    }

    /// W16-F: an arrow nudge waits on the selected knots.
    fn has_pending_commit(&self) -> bool {
        self.selected.is_some() && self.pending_nudge != Vec2::ZERO
    }

    /// W16-F: move the selected knots by the waiting nudge, ONE step.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let d = std::mem::take(&mut self.pending_nudge);
        if d == Vec2::ZERO || !d.is_finite() {
            return Ok(());
        }
        let Some((layer, knots)) = self.selected.clone() else {
            return Ok(());
        };
        let Some((active, subpaths)) = self.active_knots(ctx) else {
            return Ok(());
        };
        if active != layer {
            self.selected = None;
            return Ok(());
        }
        let next = moved_knots(&subpaths, &knots, Point::new(d.x as f64, d.y as f64));
        emit_knots(ctx, layer, &next);
        Ok(())
    }

    /// W16-F: `nudge_x` / `nudge_y`, an arrow key's nudge of the selected
    /// knots (the shell sends them, then [`Tool::commit`] applies them).
    fn set_setting(
        &mut self,
        key: &str,
        setting: crate::tool::ToolSetting,
    ) -> Result<(), ToolError> {
        match (key, setting) {
            (NUDGE_X, crate::tool::ToolSetting::Float(v)) => {
                self.pending_nudge.x += crate::error::finite("nudge", v)?;
                Ok(())
            }
            (NUDGE_Y, crate::tool::ToolSetting::Float(v)) => {
                self.pending_nudge.y += crate::error::finite("nudge", v)?;
                Ok(())
            }
            (NUDGE_X | NUDGE_Y, _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

/// Which anchor edit an [`AnchorTool`] performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorEdit {
    /// Click on the outline: split the segment there, the outline unmoved.
    Add,
    /// Click on an anchor: remove it (never below a line or a triangle).
    Delete,
    /// Click on an anchor: smooth becomes corner, corner grows handles.
    Convert,
}

impl AnchorEdit {
    fn tool_id(self) -> ToolId {
        match self {
            AnchorEdit::Add => ToolId::AddAnchor,
            AnchorEdit::Delete => ToolId::DeleteAnchor,
            AnchorEdit::Convert => ToolId::ConvertAnchor,
        }
    }
}

/// Add Anchor Point, Delete Anchor Point and Convert Point — the Pen slot's
/// three path-editing tools. A plain click does what Direct Selection's
/// Shift/Ctrl/Alt-click does, on the active shape layer's path, as ONE
/// [`Command::SetLayerKind`] step; a click that finds nothing to edit emits
/// nothing.
pub struct AnchorTool {
    edit: AnchorEdit,
}

impl AnchorTool {
    pub fn new(edit: AnchorEdit) -> Self {
        Self { edit }
    }

    pub fn edit(&self) -> AnchorEdit {
        self.edit
    }
}

impl Tool for AnchorTool {
    fn id(&self) -> ToolId {
        self.edit.tool_id()
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let p = Point::new(event.pos.x as f64, event.pos.y as f64);
        if !p.is_finite() {
            return Ok(());
        }
        let edit = self.edit;
        edit_anchors(ctx, |sps| match edit {
            AnchorEdit::Add => anchors::insert_anchor(sps, p, HIT_TOLERANCE).is_some(),
            AnchorEdit::Delete => anchors::anchor_near(sps, p, ANCHOR_RADIUS)
                .is_some_and(|at| anchors::delete_anchor(sps, at)),
            AnchorEdit::Convert => anchors::anchor_near(sps, p, ANCHOR_RADIUS)
                .is_some_and(|at| anchors::convert_anchor(sps, at)),
        });
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
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {}

    fn is_active(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::Modifiers;
    use layer_model::ShapeLayer;
    use raster::PixelRect;

    /// W9-F: Path Select's Unite merges the clicked layer's two overlapping
    /// rectangles into one outline covering their union; Align Left moves
    /// both components onto the joint left edge.
    #[test]
    fn path_select_combine_and_align_edit_the_clicked_path() {
        let two = "M0 0 L10 0 L10 10 L0 10 Z M5 5 L15 5 L15 15 L5 15 Z";
        let id = LayerId::new();
        let run = |key: &str, choice: usize| {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
            ctx.shape_paths = vec![(id, ShapeLayer::from_svg(two))];
            let mut tool = PathSelectTool::default();
            tool.set_setting(key, crate::tool::ToolSetting::Choice(choice))
                .unwrap();
            tool.on_pointer_down(&mut ctx, PointerEvent::at(2.0, 2.0))
                .unwrap();
            let cmds = ctx.drain();
            let Some(Command::SetLayerKind { layer_id, kind }) = cmds.first() else {
                panic!("{key}: no edit: {cmds:?}");
            };
            assert_eq!(*layer_id, id);
            let LayerKind::Shape(s) = &**kind else {
                panic!("not a shape");
            };
            svg::parse(&s.path_svg).unwrap()
        };
        let united = run("combine", 1);
        assert_eq!(components(&united).len(), 1, "one outline");
        let area = vector::fill(&united, &vector::FillOptions::default())
            .unwrap()
            .area();
        assert_eq!(area, 175.0, "the union of two overlapping 10x10 squares");

        let aligned = run("align", 1);
        let parts = components(&aligned);
        assert_eq!(parts.len(), 2);
        assert!(
            parts.iter().all(|p| p.bounds().min.x == 0.0),
            "both on x = 0"
        );

        // Select Only emits no edit, just the selection.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        ctx.shape_paths = vec![(id, ShapeLayer::from_svg(two))];
        PathSelectTool::default()
            .on_pointer_down(&mut ctx, PointerEvent::at(2.0, 2.0))
            .unwrap();
        assert!(ctx.commands().is_empty());
        assert_eq!(ctx.requests().len(), 1);
    }

    fn square_ctx(tiles: &mut MemoryTiles) -> ToolContext<'_> {
        let mut ctx = ToolContext::new(tiles, PixelRect::new(0, 0, 256, 256));
        let square = vector::shapes::rect(vector::Bounds::from_xywh(10.0, 10.0, 100.0, 100.0));
        let id = LayerId::new();
        ctx.active_layer = Some(id);
        ctx.shape_paths = vec![(id, ShapeLayer::from_svg(svg::to_svg(&square)))];
        ctx
    }

    fn click(
        tool: &mut DirectSelectionTool,
        ctx: &mut ToolContext<'_>,
        x: f32,
        y: f32,
        m: Modifiers,
    ) {
        tool.on_pointer_down(ctx, PointerEvent::at(x, y).with_modifiers(m))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y).with_modifiers(m))
            .unwrap();
    }

    fn edited_path(ctx: &mut ToolContext<'_>) -> vector::Path {
        let cmds = ctx.drain();
        let [Command::SetLayerKind { kind, .. }] = &cmds[..] else {
            panic!("expected one SetLayerKind: {cmds:?}");
        };
        let LayerKind::Shape(shape) = kind.as_ref() else {
            panic!("{kind:?}");
        };
        svg::parse(&shape.path_svg).unwrap()
    }

    const CTRL: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: true,
    };

    #[test]
    fn shift_click_on_the_outline_adds_an_anchor() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = square_ctx(&mut tiles);
        let mut tool = DirectSelectionTool::default();
        click(&mut tool, &mut ctx, 60.0, 11.0, Modifiers::shift());
        let pts = anchors::anchor_points(&edited_path(&mut ctx));
        assert_eq!(pts.len(), 5, "{pts:?}");
        assert!(pts.iter().any(|p| p.distance(Point::new(60.0, 10.0)) < 0.5));
    }

    #[test]
    fn ctrl_click_on_an_anchor_deletes_it() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = square_ctx(&mut tiles);
        let mut tool = DirectSelectionTool::default();
        click(&mut tool, &mut ctx, 110.0, 110.0, CTRL);
        let pts = anchors::anchor_points(&edited_path(&mut ctx));
        assert_eq!(pts.len(), 3, "{pts:?}");
        assert!(pts
            .iter()
            .all(|p| p.distance(Point::new(110.0, 110.0)) > 1.0));
    }

    #[test]
    fn alt_click_on_a_corner_makes_it_smooth() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = square_ctx(&mut tiles);
        let mut tool = DirectSelectionTool::default();
        click(&mut tool, &mut ctx, 110.0, 10.0, Modifiers::alt());
        let path = edited_path(&mut ctx);
        assert!(path
            .elements()
            .iter()
            .any(|e| matches!(e, PathEl::CurveTo(..))));
        assert!(!tool.is_active(), "a modifier click left a drag open");
    }

    fn plain_click(tool: &mut AnchorTool, ctx: &mut ToolContext<'_>, x: f32, y: f32) {
        tool.on_pointer_down(ctx, PointerEvent::at(x, y)).unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y)).unwrap();
    }

    #[test]
    fn the_anchor_tools_edit_with_a_plain_click() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = square_ctx(&mut tiles);
        let mut add = AnchorTool::new(AnchorEdit::Add);
        assert_eq!(add.id(), ToolId::AddAnchor);
        plain_click(&mut add, &mut ctx, 60.0, 11.0);
        assert_eq!(anchors::anchor_points(&edited_path(&mut ctx)).len(), 5);

        let mut delete = AnchorTool::new(AnchorEdit::Delete);
        assert_eq!(delete.id(), ToolId::DeleteAnchor);
        plain_click(&mut delete, &mut ctx, 110.0, 110.0);
        assert_eq!(anchors::anchor_points(&edited_path(&mut ctx)).len(), 3);

        let mut convert = AnchorTool::new(AnchorEdit::Convert);
        assert_eq!(convert.id(), ToolId::ConvertAnchor);
        plain_click(&mut convert, &mut ctx, 110.0, 10.0);
        assert!(edited_path(&mut ctx)
            .elements()
            .iter()
            .any(|e| matches!(e, PathEl::CurveTo(..))));

        // Away from the path, none of them edits anything.
        for t in [&mut add, &mut delete, &mut convert] {
            plain_click(t, &mut ctx, 60.0, 60.0);
        }
        assert!(ctx.commands().is_empty(), "{:?}", ctx.commands());
    }

    #[test]
    fn a_modifier_click_away_from_the_path_emits_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = square_ctx(&mut tiles);
        let mut tool = DirectSelectionTool::default();
        click(&mut tool, &mut ctx, 60.0, 60.0, CTRL);
        click(&mut tool, &mut ctx, 60.0, 60.0, Modifiers::shift());
        click(&mut tool, &mut ctx, 60.0, 60.0, Modifiers::alt());
        assert!(ctx.commands().is_empty(), "{:?}", ctx.commands());
    }
}

/// W16-F: Path Select works per path component, Direct Selection keeps a
/// multi-knot selection, and a double-click collapses handles — each through
/// the tools' own pointer route.
#[cfg(test)]
mod w16f_tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::{Modifiers, ToolSetting};
    use layer_model::ShapeLayer;
    use raster::PixelRect;

    /// Two squares: component 0 at 10..20, component 1 at 40..50.
    const TWO: &str = "M10 10 L20 10 L20 20 L10 20 Z M40 40 L50 40 L50 50 L40 50 Z";

    fn ctx_with<'a>(tiles: &'a mut MemoryTiles, svg_path: &str) -> (ToolContext<'a>, LayerId) {
        let mut ctx = ToolContext::new(tiles, PixelRect::new(0, 0, 128, 128));
        let id = LayerId::new();
        ctx.active_layer = Some(id);
        ctx.shape_paths = vec![(id, ShapeLayer::from_svg(svg_path))];
        (ctx, id)
    }

    fn gesture(tool: &mut dyn Tool, ctx: &mut ToolContext<'_>, pts: &[(f32, f32)], m: Modifiers) {
        let (x, y) = pts[0];
        tool.on_pointer_down(ctx, PointerEvent::at(x, y).with_modifiers(m))
            .unwrap();
        for (x, y) in &pts[1..] {
            tool.on_pointer_move(ctx, PointerEvent::at(*x, *y).with_modifiers(m))
                .unwrap();
        }
        let (x, y) = *pts.last().unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y).with_modifiers(m))
            .unwrap();
    }

    /// The one `SetLayerKind` drained from `ctx`, its path parsed; the
    /// context's shape is updated to it, as the shell's apply would.
    fn landed(ctx: &mut ToolContext<'_>) -> vector::Path {
        let cmds = ctx.drain();
        let [Command::SetLayerKind { kind, .. }] = &cmds[..] else {
            panic!("expected one SetLayerKind: {cmds:?}");
        };
        let LayerKind::Shape(shape) = kind.as_ref() else {
            panic!("{kind:?}");
        };
        ctx.shape_paths[0].1 = shape.clone();
        svg::parse(&shape.path_svg).unwrap()
    }

    fn min_of(part: &vector::Path) -> (f64, f64) {
        let b = part.bounds();
        (b.min.x, b.min.y)
    }

    #[test]
    fn path_select_picks_one_component_and_a_drag_moves_only_it() {
        let mut tiles = MemoryTiles::new();
        let (mut ctx, id) = ctx_with(&mut tiles, TWO);
        let mut tool = PathSelectTool::default();
        gesture(
            &mut tool,
            &mut ctx,
            &[(45.0, 45.0), (50.0, 47.0)],
            Modifiers::NONE,
        );
        assert_eq!(tool.selected_components(), Some((id, &[1usize][..])));
        let parts = components(&landed(&mut ctx));
        assert_eq!(parts.len(), 2);
        assert_eq!(min_of(&parts[0]), (10.0, 10.0), "component 0 stays");
        assert_eq!(min_of(&parts[1]), (45.0, 42.0), "component 1 moved");
    }

    #[test]
    fn shift_click_selects_two_components_and_a_nudge_moves_both() {
        let mut tiles = MemoryTiles::new();
        let (mut ctx, id) = ctx_with(&mut tiles, TWO);
        let mut tool = PathSelectTool::default();
        gesture(&mut tool, &mut ctx, &[(15.0, 15.0)], Modifiers::NONE);
        gesture(&mut tool, &mut ctx, &[(45.0, 45.0)], Modifiers::shift());
        assert_eq!(tool.selected_components(), Some((id, &[0usize, 1][..])));
        assert!(ctx.commands().is_empty(), "clicks alone edit nothing");
        tool.set_setting(NUDGE_X, ToolSetting::Float(10.0)).unwrap();
        assert!(tool.has_pending_commit());
        Tool::commit(&mut tool, &mut ctx).unwrap();
        let parts = components(&landed(&mut ctx));
        assert_eq!(min_of(&parts[0]), (20.0, 10.0));
        assert_eq!(min_of(&parts[1]), (50.0, 40.0));
        assert!(!tool.has_pending_commit(), "the nudge was spent");
    }

    #[test]
    fn the_arrange_and_delete_ops_reorder_and_remove_the_selected_component() {
        let mut tiles = MemoryTiles::new();
        let (mut ctx, id) = ctx_with(&mut tiles, TWO);
        let mut tool = PathSelectTool::default();
        gesture(&mut tool, &mut ctx, &[(45.0, 45.0)], Modifiers::NONE);
        request_component_op(ComponentOp::SendToBack);
        assert!(tool.has_pending_commit());
        Tool::commit(&mut tool, &mut ctx).unwrap();
        let parts = components(&landed(&mut ctx));
        assert_eq!(min_of(&parts[0]), (40.0, 40.0), "sent to the back");
        assert_eq!(min_of(&parts[1]), (10.0, 10.0));
        assert_eq!(
            tool.selected_components(),
            Some((id, &[0usize][..])),
            "the selection follows the component"
        );
        request_component_op(ComponentOp::BringForward);
        Tool::commit(&mut tool, &mut ctx).unwrap();
        let parts = components(&landed(&mut ctx));
        assert_eq!(min_of(&parts[1]), (40.0, 40.0), "one step forward");
        request_component_op(ComponentOp::Delete);
        Tool::commit(&mut tool, &mut ctx).unwrap();
        let parts = components(&landed(&mut ctx));
        assert_eq!(parts.len(), 1);
        assert_eq!(min_of(&parts[0]), (10.0, 10.0), "the other one survives");
        assert!(tool.selected_components().is_none());
        assert!(pending_component_op().is_none(), "the op was spent");
    }

    #[test]
    fn deleting_the_last_component_is_refused() {
        let one = vector::Path::from_elements(
            components(&svg::parse(TWO).unwrap())[0].elements().to_vec(),
        );
        assert!(apply_component_op(&one, &[0], ComponentOp::Delete).is_none());
    }

    /// A square 10..50 with its knots at the corners.
    const SQUARE: &str = "M10 10 L50 10 L50 50 L10 50 Z";

    #[test]
    fn shift_click_selects_two_knots_and_a_drag_moves_both() {
        let mut tiles = MemoryTiles::new();
        let (mut ctx, _) = ctx_with(&mut tiles, SQUARE);
        let mut tool = DirectSelectionTool::default();
        gesture(&mut tool, &mut ctx, &[(10.0, 10.0)], Modifiers::NONE);
        gesture(&mut tool, &mut ctx, &[(50.0, 10.0)], Modifiers::shift());
        assert_eq!(tool.selected_knots().map(|(_, k)| k.len()), Some(2));
        assert!(ctx.commands().is_empty(), "selecting edits nothing");
        // Dragging either selected knot moves both.
        gesture(
            &mut tool,
            &mut ctx,
            &[(50.0, 10.0), (50.0, 20.0)],
            Modifiers::NONE,
        );
        let pts = anchors::anchor_points(&landed(&mut ctx));
        assert_eq!(
            pts,
            vec![
                Point::new(10.0, 20.0),
                Point::new(50.0, 20.0),
                Point::new(50.0, 50.0),
                Point::new(10.0, 50.0),
            ]
        );
        // And an arrow nudge moves them both again.
        tool.set_setting(NUDGE_Y, ToolSetting::Float(1.0)).unwrap();
        Tool::commit(&mut tool, &mut ctx).unwrap();
        let pts = anchors::anchor_points(&landed(&mut ctx));
        assert_eq!(pts[0], Point::new(10.0, 21.0));
        assert_eq!(pts[1], Point::new(50.0, 21.0));
        assert_eq!(pts[2], Point::new(50.0, 50.0));
    }

    #[test]
    fn double_clicking_a_smooth_knot_collapses_its_handles_and_again_restores_them() {
        // The knot at (30, 10) is smooth: handles (-10, 0) and (10, 0).
        let arc = "M10 30 C10 20 20 10 30 10 C40 10 50 20 50 30 Z";
        let mut tiles = MemoryTiles::new();
        let (mut ctx, _) = ctx_with(&mut tiles, arc);
        let mut tool = DirectSelectionTool::default();
        let knot = |path: &vector::Path| anchors::from_path(path)[0].anchors[1];
        assert!(knot(&svg::parse(arc).unwrap()).is_smooth());
        gesture(&mut tool, &mut ctx, &[(30.0, 10.0)], Modifiers::NONE);
        assert!(ctx.commands().is_empty(), "one click only selects");
        gesture(&mut tool, &mut ctx, &[(30.0, 10.0)], Modifiers::NONE);
        let collapsed = knot(&landed(&mut ctx));
        assert_eq!(collapsed.pos, Point::new(30.0, 10.0));
        assert_eq!(collapsed.handle_in, Point::ZERO);
        assert_eq!(collapsed.handle_out, Point::ZERO);
        // Double-clicking the collapsed knot gives it a pair back.
        gesture(&mut tool, &mut ctx, &[(30.0, 10.0)], Modifiers::NONE);
        gesture(&mut tool, &mut ctx, &[(30.0, 10.0)], Modifiers::NONE);
        assert!(knot(&landed(&mut ctx)).is_smooth());
    }

    #[test]
    fn double_clicking_a_handle_collapses_only_that_handle() {
        let arc = "M10 30 C10 20 20 10 30 10 C40 10 50 20 50 30 Z";
        let mut tiles = MemoryTiles::new();
        let (mut ctx, _) = ctx_with(&mut tiles, arc);
        let mut tool = DirectSelectionTool::default();
        // The knot's outgoing handle ends at (40, 10).
        gesture(&mut tool, &mut ctx, &[(40.0, 10.0)], Modifiers::NONE);
        gesture(&mut tool, &mut ctx, &[(40.0, 10.0)], Modifiers::NONE);
        let a = anchors::from_path(&landed(&mut ctx))[0].anchors[1];
        assert_eq!(a.handle_out, Point::ZERO);
        assert_eq!(
            a.handle_in,
            Point::new(-10.0, 0.0),
            "the other handle stays"
        );
    }
}
