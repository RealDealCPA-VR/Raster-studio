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

/// Path Select: click a path to select the shape layer that owns it.
///
/// Layers are tested top-most first (the same order the shell fills
/// [`ToolContext::shape_paths`] in), and a hit is a click within
/// [`HIT_TOLERANCE`] of the path's outline or inside its fill.
#[derive(Default)]
pub struct PathSelectTool;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use crate::tool::Modifiers;
    use layer_model::ShapeLayer;
    use raster::PixelRect;

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
