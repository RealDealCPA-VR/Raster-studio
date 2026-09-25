//! W16-G: the Vector Gradient tool (Photopea's "Vector Gradient").
//!
//! It edits the geometry of the active shape layer's gradient fill on the
//! canvas instead of painting pixels. The gradient is shown as a line from
//! its start handle to its end handle; as in Photopea, dragging the start
//! handle moves the whole gradient, dragging the end handle sets its
//! direction and length, and a press away from both starts the line again
//! from the press point. The release lands one [`Command::SetLayerKind`]
//! (one undo step) with the fill's `angle_deg`, `scale` and `offset_px`
//! rewritten; its stops, style and every other field are kept.
//!
//! # Geometry
//!
//! A shape's gradient is fitted to the shape's filled bounds (the
//! compositor's `gradient_shader`): its centre is the bounds' centre plus
//! `offset_px`, its half-length `max(w, h) / 2 * scale`, its direction
//! `angle_deg` counter-clockwise from +x. A Linear gradient runs from
//! `centre - half` to `centre + half`, so its handles are those two points;
//! every other style starts at the centre and ends one half-length out.
//! [`handles_of`] and [`fill_from_handles`] are the two directions of that
//! map, exact inverses of each other.
//!
//! The handles are in the shape's own path space. The shell does not lend a
//! shape layer's transform to tools (`sample_to_layer` is only filled for
//! pixel kinds), so on a shape layer that was moved or transformed after it
//! was drawn the line is drawn where the untransformed shape would be.

use editor_core::Command;
use glam::Vec2;
use layer_model::{GradientStyle, LayerId, LayerKind, ShapeFillPaint, ShapeGradientFill};

use crate::error::ToolError;
use crate::gradient::constrain_45;
use crate::tool::{PointerEvent, SessionGeometry, Tool, ToolContext, ToolId};

/// How close (in screen pixels) a press must land to a handle to grab it.
pub const HANDLE_GRAB_SCREEN_PX: f32 = 6.0;

/// The fitted box `[x, y, w, h]` a shape's gradient is laid on: its path's
/// bounds rounded out to whole pixels, as the compositor's fill rect is.
pub fn fit_of(path_svg: &str) -> Option<[f32; 4]> {
    let path = vector::parse_svg(path_svg).ok()?;
    let b = path.bounds();
    if !(b.width() > 0.0 && b.height() > 0.0) || !b.min.is_finite() || !b.max.is_finite() {
        return None;
    }
    let (x0, y0) = (b.min.x.floor(), b.min.y.floor());
    let (x1, y1) = (b.max.x.ceil(), b.max.y.ceil());
    Some([x0 as f32, y0 as f32, (x1 - x0) as f32, (y1 - y0) as f32])
}

fn finite_or(v: f32, or: f32) -> f32 {
    if v.is_finite() {
        v
    } else {
        or
    }
}

/// The gradient's `[start, end]` handles over the box `fit`.
pub fn handles_of(g: &ShapeGradientFill, fit: [f32; 4]) -> [Vec2; 2] {
    let [x, y, w, h] = fit;
    let scale = if g.scale.is_finite() && g.scale > 0.0 {
        g.scale.clamp(0.01, 1000.0)
    } else {
        1.0
    };
    let centre = Vec2::new(
        x + w * 0.5 + finite_or(g.offset_px[0], 0.0),
        y + h * 0.5 + finite_or(g.offset_px[1], 0.0),
    );
    let half = (w.max(h) * 0.5 * scale).max(1.0);
    let a = finite_or(g.angle_deg, 0.0).to_radians();
    // Image y grows downward, so a positive angle runs upward on screen.
    let dir = Vec2::new(a.cos(), -a.sin());
    match g.style {
        GradientStyle::Linear => [centre - dir * half, centre + dir * half],
        _ => [centre, centre + dir * half],
    }
}

/// `g` with its geometry set so that its handles over `fit` are `start` and
/// `end`; `None` when the two points coincide (no direction to take).
pub fn fill_from_handles(
    g: &ShapeGradientFill,
    fit: [f32; 4],
    start: Vec2,
    end: Vec2,
) -> Option<ShapeGradientFill> {
    let [x, y, w, h] = fit;
    let d = end - start;
    if !d.is_finite() || d.length() < 1e-3 || !(w > 0.0 && h > 0.0) {
        return None;
    }
    let (centre, half) = match g.style {
        GradientStyle::Linear => ((start + end) * 0.5, d.length() * 0.5),
        _ => (start, d.length()),
    };
    let base = w.max(h) * 0.5;
    Some(ShapeGradientFill {
        angle_deg: (-d.y).atan2(d.x).to_degrees(),
        scale: (half / base).clamp(0.01, 1000.0),
        offset_px: [centre.x - (x + w * 0.5), centre.y - (y + h * 0.5)],
        ..g.clone()
    })
}

/// The active layer, when it is a shape filled with a gradient: its id, its
/// shape, the gradient and the box it is fitted to.
fn target(ctx: &ToolContext<'_>) -> Option<(LayerId, layer_model::ShapeLayer, [f32; 4])> {
    let id = ctx.active_layer?;
    let (_, shape) = ctx.shape_paths.iter().find(|(l, _)| *l == id)?;
    if !matches!(shape.fill_paint, ShapeFillPaint::Gradient(_)) || shape.fill.is_none() {
        return None;
    }
    let fit = fit_of(&shape.path_svg)?;
    Some((id, shape.clone(), fit))
}

struct Drag {
    layer: LayerId,
    shape: layer_model::ShapeLayer,
    fit: [f32; 4],
    /// 0: the start handle (moves the whole line); 1: the end handle.
    handle: usize,
    press: Vec2,
    at_press: [Vec2; 2],
    now: [Vec2; 2],
}

/// Drag a shape's gradient fill by its handles.
#[derive(Default)]
pub struct VectorGradientTool {
    drag: Option<Drag>,
    /// The handles of the last gradient edited, drawn until the next press
    /// or a cancel.
    shown: Option<[Vec2; 2]>,
}

impl VectorGradientTool {
    /// The line being shown, `[start, end]` in the shape's pixels.
    pub fn handles(&self) -> Option<[Vec2; 2]> {
        self.drag.as_ref().map(|d| d.now).or(self.shown)
    }
}

impl Tool for VectorGradientTool {
    fn id(&self) -> ToolId {
        ToolId::VectorGradient
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let at = crate::error::finite_pt("vector gradient press", event.pos)?;
        self.drag = None;
        // A layer that is not a gradient-filled shape has nothing to edit.
        let Some((layer, shape, fit)) = target(ctx) else {
            self.shown = None;
            return Ok(());
        };
        let ShapeFillPaint::Gradient(g) = &shape.fill_paint else {
            return Ok(());
        };
        let handles = handles_of(g, fit);
        let zoom = if ctx.view.zoom.is_finite() && ctx.view.zoom > 0.0 {
            ctx.view.zoom
        } else {
            1.0
        };
        let grab = HANDLE_GRAB_SCREEN_PX / zoom;
        let (handle, at_press) = if at.distance(handles[1]) <= grab {
            (1, handles)
        } else if at.distance(handles[0]) <= grab {
            (0, handles)
        } else {
            // Away from both: a new line from the press point.
            (1, [at, at])
        };
        self.drag = Some(Drag {
            layer,
            shape,
            fit,
            handle,
            press: at,
            at_press,
            now: at_press,
        });
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(drag) = self.drag.as_mut() else {
            return Ok(());
        };
        let at = crate::error::finite_pt("vector gradient drag", event.pos)?;
        let [s, e] = drag.at_press;
        drag.now = if drag.handle == 0 {
            let by = at - drag.press;
            [s + by, e + by]
        } else {
            let mut end = e + (at - drag.press);
            if event.modifiers.shift {
                end = constrain_45(s, end);
            }
            [s, end]
        };
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.on_pointer_move(ctx, event)?;
        let Some(drag) = self.drag.take() else {
            return Ok(());
        };
        self.shown = Some(drag.now);
        let ShapeFillPaint::Gradient(g) = &drag.shape.fill_paint else {
            return Ok(());
        };
        let Some(next) = fill_from_handles(g, drag.fit, drag.now[0], drag.now[1]) else {
            // A click that made no line changes nothing.
            self.shown = Some(handles_of(g, drag.fit));
            return Ok(());
        };
        if next == *g {
            return Ok(());
        }
        let shape = layer_model::ShapeLayer {
            fill_paint: ShapeFillPaint::Gradient(next),
            ..drag.shape
        };
        ctx.emit(Command::SetLayerKind {
            layer_id: drag.layer,
            kind: Box::new(LayerKind::Shape(shape)),
        });
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.drag = None;
        self.shown = None;
    }

    fn is_active(&self) -> bool {
        self.drag.is_some()
    }

    /// No options: every key is refused rather than accepted and ignored
    /// (the trait default takes any Choice, a dead control in the bar).
    fn set_setting(
        &mut self,
        key: &str,
        _setting: crate::tool::ToolSetting,
    ) -> Result<(), ToolError> {
        Err(ToolError::UnknownOption {
            key: key.to_owned(),
        })
    }

    /// The gradient line, start to end, drawn by the canvas as a measured
    /// line (the shell's overlay for a two-point line) while it is dragged
    /// and after release.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        let [start, end] = self.handles()?;
        Some(SessionGeometry::Measure { start, end })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::ShapeLayer;
    use raster::PixelRect;

    fn gradient_square() -> ShapeLayer {
        ShapeLayer {
            fill_paint: ShapeFillPaint::Gradient(ShapeGradientFill {
                angle_deg: 0.0,
                ..ShapeGradientFill::default()
            }),
            ..ShapeLayer::from_svg("M0 0 L100 0 L100 100 L0 100 Z")
        }
    }

    #[test]
    fn handles_and_geometry_are_inverse_maps() {
        let fit = [0.0, 0.0, 100.0, 100.0];
        for style in [GradientStyle::Linear, GradientStyle::Radial] {
            let g = ShapeGradientFill {
                style,
                angle_deg: 30.0,
                scale: 0.8,
                offset_px: [5.0, -7.0],
                ..ShapeGradientFill::default()
            };
            let [s, e] = handles_of(&g, fit);
            let back = fill_from_handles(&g, fit, s, e).unwrap();
            assert!((back.angle_deg - 30.0).abs() < 1e-3, "{style:?} {back:?}");
            assert!((back.scale - 0.8).abs() < 1e-4);
            assert!((back.offset_px[0] - 5.0).abs() < 1e-3);
            assert!((back.offset_px[1] + 7.0).abs() < 1e-3);
        }
        // Default linear, angle 0: left edge to right edge through the centre.
        let g = gradient_square();
        let ShapeFillPaint::Gradient(g) = &g.fill_paint else {
            unreachable!()
        };
        assert_eq!(
            handles_of(g, fit),
            [Vec2::new(0.0, 50.0), Vec2::new(100.0, 50.0)]
        );
    }

    /// One press-to-release drag, `from` to `to`, in document pixels.
    type Drag2 = ((f32, f32), (f32, f32));

    fn run(tool: &mut VectorGradientTool, drags: &[Drag2]) -> Vec<Command> {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 200, 200));
        let id = LayerId::new();
        ctx.active_layer = Some(id);
        ctx.shape_paths = vec![(id, gradient_square())];
        for (a, b) in drags {
            tool.on_pointer_down(&mut ctx, PointerEvent::at(a.0, a.1))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(b.0, b.1))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(b.0, b.1))
                .unwrap();
        }
        ctx.drain()
    }

    fn landed(commands: &[Command]) -> ShapeGradientFill {
        let Some(Command::SetLayerKind { kind, .. }) = commands.last() else {
            panic!("no edit: {commands:?}");
        };
        let LayerKind::Shape(s) = kind.as_ref() else {
            panic!()
        };
        let ShapeFillPaint::Gradient(g) = &s.fill_paint else {
            panic!()
        };
        g.clone()
    }

    #[test]
    fn dragging_the_end_handle_turns_and_stretches_the_fill() {
        let mut tool = VectorGradientTool::default();
        // The end handle sits at (100, 50); drag it down to (50, 100).
        let cmds = run(&mut tool, &[((100.0, 50.0), (50.0, 100.0))]);
        assert_eq!(cmds.len(), 1, "one undo step");
        let g = landed(&cmds);
        // Start stays at (0, 50): the line now runs down-right at -45 deg.
        assert!((g.angle_deg + 45.0).abs() < 1e-3, "{g:?}");
        let [s, e] = handles_of(&g, [0.0, 0.0, 100.0, 100.0]);
        assert!(s.distance(Vec2::new(0.0, 50.0)) < 1e-3, "{s}");
        assert!(e.distance(Vec2::new(50.0, 100.0)) < 1e-3, "{e}");
        // The line stays shown where it was dragged to.
        let [ss, se] = tool.handles().expect("shown after release");
        assert!(ss.distance(s) < 1e-3 && se.distance(e) < 1e-3);
    }

    #[test]
    fn dragging_the_start_handle_moves_the_whole_fill() {
        let mut tool = VectorGradientTool::default();
        let cmds = run(&mut tool, &[((0.0, 50.0), (10.0, 30.0))]);
        let g = landed(&cmds);
        assert!((g.angle_deg - 0.0).abs() < 1e-3);
        assert!((g.scale - 1.0).abs() < 1e-4);
        assert_eq!(g.offset_px, [10.0, -20.0]);
        // A press away from both handles draws a new line from there.
        let mut tool = VectorGradientTool::default();
        let cmds = run(&mut tool, &[((50.0, 90.0), (50.0, 10.0))]);
        let g = landed(&cmds);
        assert!((g.angle_deg - 90.0).abs() < 1e-3, "runs upward: {g:?}");
        assert!((g.scale - 0.8).abs() < 1e-4);
        // A click with no drag and a layer without a gradient emit nothing.
        let mut tool = VectorGradientTool::default();
        assert!(run(&mut tool, &[((50.0, 90.0), (50.0, 90.0))]).is_empty());
    }
}
