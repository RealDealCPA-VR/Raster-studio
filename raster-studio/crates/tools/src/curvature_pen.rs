//! W7-F: the Curvature Pen — click points, and the path bends smoothly
//! through every one of them.
//!
//! Where the [`crate::pen::PenTool`] asks the user to drag out each handle,
//! the Curvature Pen computes them: each anchor's tangent is the direction
//! from the point before it to the point after it (a Catmull-Rom spline,
//! turned into cubic Béziers by [`smooth_anchors`]), so the curve passes
//! through every click with no corner. The two ends of an open path take
//! the direction of their one neighbour.
//!
//! The rest is the Pen's: pressing the first point (with at least three
//! placed) closes the path and publishes it, Enter publishes an open one,
//! Escape drops it, and nothing is emitted until then — so the whole path is
//! ONE history step. Mode (Path / Shape / Pixels), the paint and Combine are
//! the Pen's options and reach the same publishing code
//! ([`crate::pen::PenTool::publish_anchors`]).

use glam::Vec2;

use crate::error::ToolError;
use crate::pen::{Anchor, PenTool, CLOSE_RADIUS_PX, MAX_ANCHORS, MIN_CLOSED_ANCHORS};
use crate::tool::{PointerEvent, SessionGeometry, Tool, ToolContext, ToolId, ToolSetting};

/// The handles that make a smooth curve through `points`: a uniform
/// Catmull-Rom spline as cubic Béziers — each anchor's handles lie along
/// `(next - previous) / 6`, mirrored. A closed run wraps its neighbours; an
/// open run's end anchors use their single neighbour. Fewer than two points
/// have nothing to bend.
pub fn smooth_anchors(points: &[Vec2], closed: bool) -> Vec<Anchor> {
    let n = points.len();
    if n < 2 {
        return points.iter().map(|p| Anchor::corner(*p)).collect();
    }
    (0..n)
        .map(|i| {
            let prev = if i > 0 {
                points[i - 1]
            } else if closed {
                points[n - 1]
            } else {
                points[i]
            };
            let next = if i + 1 < n {
                points[i + 1]
            } else if closed {
                points[0]
            } else {
                points[i]
            };
            let tangent = (next - prev) / 6.0;
            Anchor {
                pos: points[i],
                handle_in: -tangent,
                handle_out: tangent,
            }
        })
        .collect()
}

/// The Curvature Pen.
#[derive(Default)]
pub struct CurvaturePenTool {
    points: Vec<Vec2>,
    /// The publisher: mode, paint and combine live here, exactly as the Pen
    /// keeps them.
    pen: PenTool,
    closing: bool,
    /// The button is down on the point just placed.
    dragging: bool,
}

impl CurvaturePenTool {
    /// The points placed so far.
    pub fn points(&self) -> &[Vec2] {
        &self.points
    }

    /// The anchors the placed points currently describe.
    pub fn anchors(&self) -> Vec<Anchor> {
        smooth_anchors(&self.points, false)
    }

    fn publish(&mut self, ctx: &mut ToolContext<'_>, closed: bool) -> Result<bool, ToolError> {
        let anchors = smooth_anchors(&self.points, closed);
        self.points.clear();
        self.closing = false;
        self.dragging = false;
        self.pen.publish_anchors(ctx, anchors, closed)
    }
}

impl Tool for CurvaturePenTool {
    fn id(&self) -> ToolId {
        ToolId::CurvaturePen
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let pos = crate::error::finite_pt("curvature pen point", event.pos)?;
        if let Some(first) = self.points.first() {
            if self.points.len() >= MIN_CLOSED_ANCHORS && (pos - *first).length() <= CLOSE_RADIUS_PX
            {
                self.closing = true;
                return Ok(());
            }
            if self
                .points
                .last()
                .is_some_and(|l| (pos - *l).length() <= f32::EPSILON)
            {
                return Ok(());
            }
        }
        if self.points.len() >= MAX_ANCHORS {
            return Err(ToolError::RegionTooLarge {
                tiles: self.points.len() as u64,
                max: MAX_ANCHORS as u64,
            });
        }
        self.points.push(pos);
        self.dragging = true;
        Ok(())
    }

    /// Dragging moves the point just placed, so the curve can be steered
    /// before the button comes up.
    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.closing || !self.dragging || !event.pos.is_finite() {
            return Ok(());
        }
        if let Some(last) = self.points.last_mut() {
            *last = event.pos;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.dragging = false;
        if self.closing {
            self.publish(ctx, true)?;
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.points.clear();
        self.closing = false;
        self.dragging = false;
    }

    /// Enter publishes the open path.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        if self.points.len() < crate::pen::MIN_OPEN_ANCHORS {
            self.points.clear();
            return Err(ToolError::Degenerate);
        }
        self.publish(ctx, false)?;
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        !self.points.is_empty()
    }

    fn is_active(&self) -> bool {
        !self.points.is_empty()
    }

    fn live_geometry(&self) -> Option<SessionGeometry> {
        if self.points.is_empty() {
            return None;
        }
        let anchors = self.anchors();
        Some(SessionGeometry::Path {
            anchors: anchors.iter().map(|a| a.pos).collect(),
            handles: anchors
                .iter()
                .map(|a| [a.pos + a.handle_in, a.pos + a.handle_out])
                .collect(),
            closing: self.closing,
        })
    }

    /// The Pen's options: mode, combine and the five paint keys.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        self.pen.set_setting(key, setting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use editor_core::Command;
    use layer_model::LayerKind;
    use raster::PixelRect;
    use vector::{svg, PathEl};

    fn click(tool: &mut CurvaturePenTool, ctx: &mut ToolContext<'_>, x: f32, y: f32) {
        tool.on_pointer_down(ctx, PointerEvent::at(x, y)).unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(x, y)).unwrap();
    }

    #[test]
    fn the_curve_passes_through_every_click_with_smooth_handles() {
        let pts = [
            Vec2::new(10.0, 50.0),
            Vec2::new(50.0, 10.0),
            Vec2::new(90.0, 50.0),
        ];
        let a = smooth_anchors(&pts, false);
        assert_eq!(a.iter().map(|a| a.pos).collect::<Vec<_>>(), pts);
        // The middle anchor is smooth: collinear, opposite handles along the
        // chord from its neighbours.
        assert_eq!(a[1].handle_out, Vec2::new(80.0 / 6.0, 0.0));
        assert_eq!(a[1].handle_in, -a[1].handle_out);
    }

    #[test]
    fn clicks_then_enter_publish_one_curved_shape_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128));
        let mut tool = CurvaturePenTool::default();
        click(&mut tool, &mut ctx, 10.0, 50.0);
        click(&mut tool, &mut ctx, 50.0, 10.0);
        click(&mut tool, &mut ctx, 90.0, 50.0);
        assert!(
            ctx.commands().is_empty(),
            "nothing emits while placing points"
        );
        assert!(tool.live_geometry().is_some());
        tool.commit(&mut ctx).unwrap();
        let commands = ctx.drain();
        assert_eq!(commands.len(), 1);
        let Command::CreateLayer { layer } = &commands[0] else {
            panic!("{commands:?}");
        };
        let LayerKind::Shape(shape) = &layer.kind else {
            panic!("not a shape");
        };
        let path = svg::parse(&shape.path_svg).unwrap();
        assert!(
            path.elements()
                .iter()
                .any(|e| matches!(e, PathEl::CurveTo(..))),
            "the path is not curved: {}",
            shape.path_svg
        );
        assert!(!tool.is_active());
    }

    #[test]
    fn pressing_the_first_point_closes_and_publishes() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128));
        let mut tool = CurvaturePenTool::default();
        for (x, y) in [(10.0, 10.0), (60.0, 10.0), (35.0, 50.0)] {
            click(&mut tool, &mut ctx, x, y);
        }
        click(&mut tool, &mut ctx, 11.0, 11.0);
        assert_eq!(ctx.commands().len(), 1);
        assert!(!tool.is_active());
    }
}
