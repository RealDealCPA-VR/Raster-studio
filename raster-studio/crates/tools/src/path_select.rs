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

/// Path Select: click a path to select the shape layer that owns it.
///
/// Layers are tested top-most first (the same order the shell fills
/// [`ToolContext::shape_paths`] in), and a hit is a click within
/// [`HIT_TOLERANCE`] of the path's outline or inside its fill. W9-F: with a
/// `combine` or `align` option set, the click also merges or aligns that
/// path's components.
#[derive(Default)]
pub struct PathSelectTool {
    pub combine: PathCombine,
    pub align: PathAlign,
}

impl PathSelectTool {
    /// The topmost shape layer whose path sits under `p`.
    fn path_under(&self, ctx: &ToolContext<'_>, p: Vec2) -> Option<LayerId> {
        let point = Point::new(p.x as f64, p.y as f64);
        for (id, shape) in &ctx.shape_paths {
            let Ok(path) = svg::parse(&shape.path_svg) else {
                continue;
            };
            if vector::hit_stroke(&path, point, HIT_TOLERANCE)
                || vector::hit::contains(&path, point, FillRule::NonZero)
            {
                return Some(*id);
            }
        }
        None
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
        if let Some(id) = self.path_under(ctx, event.pos) {
            ctx.emit_request(crate::tool::ToolRequest::SelectLayer(id));
            self.apply_component_ops(ctx, id)?;
        }
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

    /// W9-F: `combine` and `align`, the two options the registry declares.
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
            ("combine" | "align", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

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

/// Direct Selection: drag an anchor of the active shape layer's path.
///
/// The gesture holds a working copy of the path elements from press to
/// release; every move rewrites the grabbed point in that copy, and release
/// commits the whole path as one [`Command::SetLayerKind`] — one undo step
/// returns the old path, and the layer re-rasterises from the new one.
#[derive(Default)]
pub struct DirectSelectionTool {
    /// The working path elements while an anchor is grabbed.
    elements: Option<(LayerId, Vec<PathEl>, usize, usize)>,
}

/// Which point of which element an anchor index names, and the point itself.
fn anchor_at(elements: &[PathEl], index: usize) -> Option<(usize, usize, Point)> {
    let mut seen = 0;
    for (i, el) in elements.iter().enumerate() {
        let points: &[Point] = match el {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => std::slice::from_ref(p),
            PathEl::QuadTo(c, p) => &[*c, *p],
            PathEl::CurveTo(c1, c2, p) => &[*c1, *c2, *p],
            PathEl::ClosePath => &[],
        };
        for (slot, p) in points.iter().enumerate() {
            if seen == index {
                return Some((i, slot, *p));
            }
            seen += 1;
        }
    }
    None
}

fn anchor_count(elements: &[PathEl]) -> usize {
    elements
        .iter()
        .map(|el| match el {
            PathEl::MoveTo(_) | PathEl::LineTo(_) => 1,
            PathEl::QuadTo(..) => 2,
            PathEl::CurveTo(..) => 3,
            PathEl::ClosePath => 0,
        })
        .sum()
}

/// Replace one point of one element, leaving the rest of the path alone.
fn with_anchor(mut elements: Vec<PathEl>, el: usize, slot: usize, p: Point) -> Vec<PathEl> {
    elements[el] = match (elements[el], slot) {
        (PathEl::MoveTo(_), _) | (PathEl::LineTo(_), _) => PathEl::LineTo(p),
        (PathEl::QuadTo(_, end), _) if slot == 0 => PathEl::QuadTo(p, end),
        (PathEl::QuadTo(c, _), _) => PathEl::QuadTo(c, p),
        (PathEl::CurveTo(_, c2, end), 0) => PathEl::CurveTo(p, c2, end),
        (PathEl::CurveTo(c1, _, end), 1) => PathEl::CurveTo(c1, p, end),
        (PathEl::CurveTo(c1, c2, _), _) => PathEl::CurveTo(c1, c2, p),
        (other, _) => other,
    };
    if el == 0 {
        // A moved first anchor of an open subpath is a move, not a line.
        if let PathEl::LineTo(_) = elements[0] {
            elements[0] = PathEl::MoveTo(p);
        }
    }
    elements
}

impl DirectSelectionTool {
    /// The active shape layer's parsed elements, if it has a path.
    fn active_elements(&self, ctx: &ToolContext<'_>) -> Option<(LayerId, Vec<PathEl>)> {
        let active = ctx.active_layer?;
        let shape = ctx.shape_paths.iter().find(|(id, _)| *id == active)?;
        let path = svg::parse(&shape.1.path_svg).ok()?;
        Some((active, path.elements().to_vec()))
    }
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
        let m = event.modifiers;
        if m.shift || m.ctrl || m.alt {
            if !p.is_finite() {
                return Ok(());
            }
            edit_anchors(ctx, |sps| {
                if m.shift {
                    anchors::insert_anchor(sps, p, HIT_TOLERANCE).is_some()
                } else if let Some(at) = anchors::anchor_near(sps, p, ANCHOR_RADIUS) {
                    if m.ctrl {
                        anchors::delete_anchor(sps, at)
                    } else {
                        anchors::convert_anchor(sps, at)
                    }
                } else {
                    false
                }
            });
            return Ok(());
        }
        let Some((layer, elements)) = self.active_elements(ctx) else {
            return Ok(());
        };
        let mut best: Option<(usize, f64)> = None;
        for index in 0..anchor_count(&elements) {
            let Some((_, _, a)) = anchor_at(&elements, index) else {
                continue;
            };
            let d = ((a.x - p.x).powi(2) + (a.y - p.y).powi(2)).sqrt();
            if d <= ANCHOR_RADIUS && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((index, d));
            }
        }
        let Some((index, _)) = best else {
            return Ok(());
        };
        let (el, slot, _) = anchor_at(&elements, index).expect("index just measured");
        let elements = with_anchor(elements, el, slot, p);
        self.elements = Some((layer, elements, el, slot));
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some((_, elements, el, slot)) = &mut self.elements else {
            return Ok(());
        };
        let p = Point::new(event.pos.x as f64, event.pos.y as f64);
        let snapshot = elements.clone();
        *elements = with_anchor(snapshot, *el, *slot, p);
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some((layer, elements, ..)) = self.elements.take() else {
            return Ok(());
        };
        let svg = svg::to_svg(&vector::Path::from_elements(elements));
        // Rebuild the layer's kind with only the path replaced: the fill,
        // stroke and fill rule survive, and SetLayerKind is the one undo step
        // an anchor drag costs.
        let Some((_, shape)) = ctx.shape_paths.iter().find(|(id, _)| *id == layer) else {
            return Ok(());
        };
        if shape.path_svg == svg {
            return Ok(()); // a click on an anchor without a drag moved nothing
        }
        let mut new_shape = shape.clone();
        new_shape.path_svg = svg;
        ctx.emit(Command::SetLayerKind {
            layer_id: layer,
            kind: Box::new(LayerKind::Shape(new_shape)),
        });
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.elements = None;
    }

    fn is_active(&self) -> bool {
        self.elements.is_some()
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
