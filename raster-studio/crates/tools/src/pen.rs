//! The Pen tool: click to start a path, click again to extend it.
//!
//! # Why this exists
//!
//! `P` was the one letter of the brief the registry could not answer, and the
//! whole of `crates/vector` — paths, hit testing, stroking, SVG — was reachable
//! only through the seven shape tools, each of which draws one *fixed* outline
//! from a drag. There was no way for a user to author an arbitrary path at all.
//!
//! # The shape of a pen gesture
//!
//! Nothing is emitted while the path is being drawn. The first click starts a
//! subpath, each later one adds a corner, and the path becomes a layer at
//! exactly two moments: clicking back on the first anchor (which closes it) or
//! confirming with [`Tool::commit`] (Enter), which leaves it open. That is the
//! same "hold the gesture, publish on commit" shape [`crate::edit::CropTool`]
//! keeps, and for the same reason — emitting a `CreateLayer` per click would
//! cost one undo step and one layer row per anchor.
//!
//! # Corners, not curves
//!
//! A click makes a corner anchor. Dragging *out of* a click is what makes a
//! smooth anchor with two control handles in a full pen tool, and this one does
//! not do it: the drag would have to survive to the next click to decide the
//! outgoing handle, and `ui::canvas::paths` (which already computes the
//! editable topology and the direction lines) has no route from a shell to hand
//! it a live gesture. So the paths this authors are polygons —
//! [`vector::Path`] carries curves and this tool never produces one. Stated
//! rather than implied, and pinned by
//! `a_click_sequence_builds_corners_and_never_a_curve`.

use glam::Vec2;
use layer_model::{Layer, LayerKind, ShapeLayer, ShapeStroke};
use vector::{to_svg, Path, Point};

use editor_core::Command;

use crate::error::ToolError;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};

/// How near the first anchor a click has to land, in document pixels, to close
/// the path rather than add another corner.
///
/// A fixed document distance rather than a screen one: the tool is handed
/// document coordinates and has no camera, so a zoom-aware radius would be a
/// number it cannot compute. At any usable zoom six pixels is a comfortable
/// target and is far below the distance between two anchors a user meant to
/// place apart.
pub const CLOSE_RADIUS_PX: f32 = 6.0;

/// The fewest anchors a path needs before it is worth a layer: two for an open
/// path (a line), three to close (a triangle).
pub const MIN_OPEN_ANCHORS: usize = 2;
pub const MIN_CLOSED_ANCHORS: usize = 3;

/// The most anchors one path may hold, so a stuck auto-clicker cannot grow an
/// unbounded document.
pub const MAX_ANCHORS: usize = 10_000;

/// Pen: author a path one click at a time.
#[derive(Default)]
pub struct PenTool {
    anchors: Vec<Vec2>,
    closed: bool,
}

impl PenTool {
    /// The anchors placed so far, in document pixels.
    pub fn anchors(&self) -> &[Vec2] {
        &self.anchors
    }

    /// `true` once the path has been closed by clicking its first anchor.
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// The path as it stands. Empty until the first click.
    pub fn path(&self) -> Path {
        let mut path = Path::new();
        let mut points = self.anchors.iter();
        let Some(first) = points.next() else {
            return path;
        };
        path.move_to(Point::new(first.x as f64, first.y as f64));
        for p in points {
            path.line_to(Point::new(p.x as f64, p.y as f64));
        }
        if self.closed {
            path.close();
        }
        path
    }

    /// Whether the path has enough anchors to become a layer.
    fn publishable(&self) -> bool {
        let needed = if self.closed {
            MIN_CLOSED_ANCHORS
        } else {
            MIN_OPEN_ANCHORS
        };
        self.anchors.len() >= needed
    }

    /// Emit the shape layer this path describes and start a fresh path.
    ///
    /// Reports whether anything was published: a path of one anchor is a click
    /// the user has not finished, not an edit.
    fn publish(&mut self, ctx: &mut ToolContext<'_>) -> bool {
        if !self.publishable() {
            self.anchors.clear();
            self.closed = false;
            return false;
        }
        let path = self.path();
        let closed = self.closed;
        self.anchors.clear();
        self.closed = false;
        // A closed path encloses something, so it is filled in the foreground
        // colour; an open one encloses nothing, so filling it would paint a
        // silhouette the user never drew — it is stroked instead. Both use
        // `ctx.foreground`, which is the colour every other authoring tool
        // paints with.
        let mut shape = ShapeLayer::from_svg(to_svg(&path));
        if closed {
            shape.fill = Some(ctx.foreground);
        } else {
            shape.fill = None;
            shape.stroke = Some(ShapeStroke {
                color: ctx.foreground,
                ..ShapeStroke::default()
            });
        }
        ctx.emit(Command::create_layer(Layer::with_kind(
            "Path",
            LayerKind::Shape(shape),
        )));
        true
    }
}

impl Tool for PenTool {
    fn id(&self) -> ToolId {
        ToolId::Pen
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("pen anchor", event.pos)?;
        if let Some(first) = self.anchors.first().copied() {
            if self.anchors.len() >= MIN_CLOSED_ANCHORS
                && (event.pos - first).length() <= CLOSE_RADIUS_PX
            {
                // Clicking the first anchor closes the path and publishes it.
                self.closed = true;
                self.publish(ctx);
                return Ok(());
            }
            // A repeated click on the anchor just placed adds nothing: a
            // zero-length segment is not a corner, and `vector` would carry it
            // into every later flatten and hit test.
            if let Some(last) = self.anchors.last() {
                if (event.pos - *last).length() <= f32::EPSILON {
                    return Ok(());
                }
            }
        }
        if self.anchors.len() >= MAX_ANCHORS {
            return Err(ToolError::RegionTooLarge {
                tiles: self.anchors.len() as u64,
                max: MAX_ANCHORS as u64,
            });
        }
        self.anchors.push(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        // The rubber band from the last anchor to the cursor is a *preview*,
        // and nothing in this build draws one — see the module docs for why a
        // drag does not make a curve either.
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.anchors.clear();
        self.closed = false;
    }

    /// Enter finishes an open path and turns it into a shape layer.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        if !self.publishable() {
            // Fewer anchors than a path needs. Refused rather than silently
            // dropped, so Enter on a half-started path says why.
            self.anchors.clear();
            self.closed = false;
            return Err(ToolError::Degenerate);
        }
        self.publish(ctx);
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        !self.anchors.is_empty()
    }

    fn is_active(&self) -> bool {
        !self.anchors.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;
    use vector::PathEl;

    fn ctx(tiles: &mut MemoryTiles) -> ToolContext<'_> {
        ToolContext::new(tiles, PixelRect::new(0, 0, 256, 256))
    }

    fn click(tool: &mut PenTool, ctx: &mut ToolContext<'_>, x: f32, y: f32) {
        tool.on_pointer_down(ctx, PointerEvent::at(x, y)).unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y)).unwrap();
    }

    fn shape_svg(commands: &[Command]) -> String {
        let Some(Command::CreateLayer { layer }) = commands.first() else {
            panic!("no layer was created: {commands:?}");
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("the pen created a {:?} layer", layer.kind);
        };
        shape.path_svg.clone()
    }

    #[test]
    fn a_click_sequence_builds_corners_and_never_a_curve() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();

        click(&mut tool, &mut ctx, 10.0, 10.0);
        assert_eq!(tool.anchors(), &[Vec2::new(10.0, 10.0)]);
        assert!(tool.is_active());
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        assert_eq!(
            tool.anchors(),
            &[
                Vec2::new(10.0, 10.0),
                Vec2::new(60.0, 10.0),
                Vec2::new(60.0, 50.0)
            ]
        );

        let path = tool.path();
        assert_eq!(
            path.elements(),
            &[
                PathEl::MoveTo(Point::new(10.0, 10.0)),
                PathEl::LineTo(Point::new(60.0, 10.0)),
                PathEl::LineTo(Point::new(60.0, 50.0)),
            ],
            "the pen authored something other than corners"
        );
        // Nothing is emitted while the path is being drawn.
        assert!(ctx.commands().is_empty(), "{:?}", ctx.commands());
    }

    #[test]
    fn clicking_the_first_anchor_closes_the_path_and_makes_the_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 50.0);
        // Within the close radius of the first anchor, not exactly on it.
        click(&mut tool, &mut ctx, 12.0, 11.0);

        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "{commands:?}");
        let svg = shape_svg(&commands);
        assert!(svg.starts_with('M'), "{svg}");
        assert!(
            svg.trim_end().ends_with('Z'),
            "the closed path did not close: {svg}"
        );
        // ...and the tool is ready for the next path rather than stuck holding
        // the finished one.
        assert!(!tool.is_active());
        assert!(tool.anchors().is_empty());
    }

    #[test]
    fn enter_finishes_an_open_path() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 5.0, 5.0);
        click(&mut tool, &mut ctx, 40.0, 80.0);
        assert!(tool.has_pending_commit());
        Tool::commit(&mut tool, &mut ctx).unwrap();

        let commands = ctx.drain();
        let svg = shape_svg(&commands);
        assert!(
            !svg.trim_end().ends_with('Z'),
            "committing closed a path the user left open: {svg}"
        );
        assert!(!tool.is_active());
    }

    #[test]
    fn a_path_with_too_few_anchors_makes_no_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 5.0, 5.0);
        assert!(Tool::commit(&mut tool, &mut ctx).is_err());
        assert!(ctx.commands().is_empty());
        assert!(!tool.is_active(), "the refused path was not cleared");

        // ...and two anchors on the same point are one anchor.
        click(&mut tool, &mut ctx, 20.0, 20.0);
        click(&mut tool, &mut ctx, 20.0, 20.0);
        assert_eq!(tool.anchors().len(), 1);
    }

    #[test]
    fn a_click_near_the_first_anchor_before_the_third_is_a_corner_not_a_close() {
        // Two anchors cannot enclose anything, so clicking back on the start
        // has to keep drawing rather than publish a degenerate loop.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 40.0, 10.0);
        click(&mut tool, &mut ctx, 11.0, 10.0);
        assert_eq!(tool.anchors().len(), 3);
        assert!(!tool.is_closed());
        assert!(ctx.commands().is_empty());
    }

    #[test]
    fn escape_throws_the_path_away_and_emits_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        click(&mut tool, &mut ctx, 10.0, 10.0);
        click(&mut tool, &mut ctx, 60.0, 10.0);
        Tool::cancel(&mut tool, &mut ctx);
        assert!(!tool.is_active());
        assert!(tool.anchors().is_empty());
        assert!(ctx.commands().is_empty());
        assert!(tool.path().is_empty());
    }

    #[test]
    fn a_non_finite_click_is_refused() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        assert!(tool
            .on_pointer_down(&mut ctx, PointerEvent::at(0.0, f32::INFINITY))
            .is_err());
        assert!(tool.anchors().is_empty());
    }

    #[test]
    fn the_anchor_count_is_bounded() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = PenTool::default();
        // Every anchor further from the first than `CLOSE_RADIUS_PX`, so the
        // walk never closes the path and publishes it out from under the count.
        for i in 0..MAX_ANCHORS {
            tool.on_pointer_down(&mut ctx, PointerEvent::at(100.0 + i as f32 * 10.0, 100.0))
                .unwrap();
        }
        assert_eq!(tool.anchors().len(), MAX_ANCHORS);
        assert!(tool
            .on_pointer_down(&mut ctx, PointerEvent::at(999_999.0, 100.0))
            .is_err());
    }
}
