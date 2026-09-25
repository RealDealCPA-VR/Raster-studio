//! W13-I: the Eyedropper's sampling ring.
//!
//! While the Eyedropper is dragged over the canvas, Photoshop and Photopea
//! show a ring around the pointer: its upper half is the colour being picked
//! now, its lower half the foreground the drag started from, so the user
//! sees the new colour against the old one before letting go.
//!
//! The chrome keeps the workspace's foreground in step with the editor's
//! every frame, and the Eyedropper writes the foreground on every sample, so
//! the upper half is simply the current foreground. The lower half is the
//! foreground as it stood on the last frame BEFORE the press (the press
//! itself may already carry the first pick). The ring is drawn only for a
//! press that began off every egui surface, i.e. on the canvas.

use design::{color32, current_theme, ColorRole, Space};
use egui::{Color32, Mesh, Pos2, Shape, Stroke, Vec2};
use tools::ToolId;

use crate::panels::color::ColorWell;
use crate::Workspace;

/// Where the ring keeps its state between frames: the foreground of the
/// last idle frame, and the one the running drag started from.
fn state_id() -> egui::Id {
    egui::Id::new("raster-w13i-eyedropper-ring")
}

/// The ring's own paint layer, above the canvas and the panels.
pub fn layer_id() -> egui::LayerId {
    egui::LayerId::new(egui::Order::Tooltip, state_id())
}

/// Segments per half ring.
const SEGMENTS: usize = 32;

/// What the options bar's frame hands the ring: the foreground and whether
/// the Eyedropper is the active tool.
fn input_id() -> egui::Id {
    state_id().with("input")
}

/// Whether the end-of-pass painter is registered on this context.
fn registered_id() -> egui::Id {
    state_id().with("registered")
}

/// Hand the ring this frame's foreground and tool. The ring itself is
/// decided and painted at the END of the pass ([`end_of_pass`]), once every
/// panel has claimed its rect, so a press is known to be on the canvas (off
/// every panel and window) rather than guessed from the last frame.
pub(crate) fn paint(w: &Workspace, ctx: &egui::Context) {
    let fg = w.color.well(ColorWell::Foreground);
    let eyedropper = w.palette.active() == ToolId::Eyedropper;
    ctx.data_mut(|d| d.insert_temp(input_id(), (fg, eyedropper)));
    let registered = ctx.data(|d| d.get_temp::<bool>(registered_id()));
    if registered != Some(true) {
        ctx.data_mut(|d| d.insert_temp(registered_id(), true));
        ctx.on_end_pass(
            "raster-w13i-eyedropper-ring",
            std::sync::Arc::new(|ctx| {
                end_of_pass(ctx);
            }),
        );
    }
}

/// Track the drag and, while an Eyedropper drag that began on the canvas is
/// running, paint the ring at the pointer. Returns the ring's outer bounds
/// when it was painted.
fn end_of_pass(ctx: &egui::Context) -> Option<egui::Rect> {
    let (fg, eyedropper): ([f32; 4], bool) = ctx.data(|d| d.get_temp(input_id()))?;
    // Nothing handed over this frame (the bar was not drawn): no ring.
    ctx.data_mut(|d| d.remove::<([f32; 4], bool)>(input_id()));
    let (down, pressed, pos) = ctx.input(|i| {
        (
            i.pointer.primary_down(),
            i.pointer.primary_pressed(),
            i.pointer.latest_pos(),
        )
    });
    let mut state: (Option<[f32; 4]>, Option<[f32; 4]>) =
        ctx.data(|d| d.get_temp(state_id())).unwrap_or_default();
    if !down {
        state = (Some(fg), None);
    } else if pressed && eyedropper && !ctx.is_pointer_over_area() {
        state.1 = Some(state.0.unwrap_or(fg));
    }
    ctx.data_mut(|d| d.insert_temp(state_id(), state));
    let old = state.1.filter(|_| down && eyedropper)?;
    let centre = pos?;

    let t = current_theme(ctx).tokens();
    let outer = t.metrics.toolbar_height * 1.5;
    let inner = outer - Space::Large.pt();
    let painter = ctx.layer_painter(layer_id());
    painter.add(Shape::mesh(half_ring(
        centre,
        inner,
        outer,
        true,
        super::rgba_to_color32(fg),
    )));
    painter.add(Shape::mesh(half_ring(
        centre,
        inner,
        outer,
        false,
        super::rgba_to_color32(old),
    )));
    let edge = Stroke::new(
        t.borders.hairline_for_scale(ctx.pixels_per_point()),
        color32(t.palette.color(ColorRole::SeparatorStrong)),
    );
    painter.circle_stroke(centre, outer, edge);
    painter.circle_stroke(centre, inner, edge);
    Some(egui::Rect::from_center_size(
        centre,
        Vec2::splat(outer * 2.0),
    ))
}

/// One half of an annulus about `centre` as a triangle strip: the upper half
/// (screen y above the centre) or the lower.
fn half_ring(centre: Pos2, inner: f32, outer: f32, upper: bool, color: Color32) -> Mesh {
    use std::f32::consts::PI;
    let mut mesh = Mesh::default();
    let start = if upper { PI } else { 0.0 };
    for i in 0..=SEGMENTS {
        let a = start + PI * i as f32 / SEGMENTS as f32;
        let dir = Vec2::new(a.cos(), a.sin());
        mesh.colored_vertex(centre + dir * inner, color);
        mesh.colored_vertex(centre + dir * outer, color);
        if i > 0 {
            let k = (i * 2) as u32;
            mesh.add_triangle(k - 2, k - 1, k);
            mesh.add_triangle(k - 1, k + 1, k);
        }
    }
    mesh
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intent::Intent;

    const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
    const BLUE: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

    struct Frame {
        ctx: egui::Context,
        w: Workspace,
    }

    impl Frame {
        fn new(tool: ToolId) -> Self {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let mut w = Workspace::new();
            w.absorb(&Intent::SelectTool(tool));
            Self { ctx, w }
        }

        /// One frame of the real options bar (which paints the ring), and
        /// the shapes it left on the ring's layer.
        fn run(&mut self, events: Vec<egui::Event>) -> Vec<Shape> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(1600.0, 900.0),
                )),
                events,
                ..Default::default()
            };
            let w = &mut self.w;
            let out = self
                .ctx
                .run(input, |ctx| super::super::tool_options(w, ctx));
            out.shapes
                .into_iter()
                .filter(|c| c.clip_rect.is_positive())
                .map(|c| c.shape)
                .filter(|s| matches!(s, Shape::Mesh(m) if m.vertices.len() == (SEGMENTS + 1) * 2))
                .collect()
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// The colour of every ring mesh whose vertices lie above (`upper`) or
    /// below the pointer.
    fn half_colors(shapes: &[Shape], at: Pos2, upper: bool) -> Vec<Color32> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Shape::Mesh(m) => Some(m),
                _ => None,
            })
            .filter(|m| {
                m.vertices.iter().all(|v| {
                    if upper {
                        v.pos.y <= at.y + 1e-3
                    } else {
                        v.pos.y >= at.y - 1e-3
                    }
                })
            })
            .map(|m| m.vertices[0].color)
            .collect()
    }

    #[test]
    fn an_eyedropper_drag_on_the_canvas_draws_new_over_old_and_release_clears_it() {
        let mut f = Frame::new(ToolId::Eyedropper);
        let at = Pos2::new(600.0, 500.0);
        f.w.color.set_well(ColorWell::Foreground, RED);
        assert!(f.run(vec![egui::Event::PointerMoved(at)]).is_empty());
        // The press lands with the first pick already in the foreground.
        f.w.color.set_well(ColorWell::Foreground, BLUE);
        let shapes = f.run(vec![button(at, true)]);
        assert_eq!(shapes.len(), 2, "one mesh per half: {shapes:?}");
        let blue = super::super::rgba_to_color32(BLUE);
        let red = super::super::rgba_to_color32(RED);
        assert_eq!(half_colors(&shapes, at, true), vec![blue], "new on top");
        assert_eq!(half_colors(&shapes, at, false), vec![red], "old below");
        // Dragging on: the ring follows the pointer and the next pick.
        let next = Pos2::new(640.0, 520.0);
        f.w.color.set_well(ColorWell::Foreground, RED);
        let shapes = f.run(vec![egui::Event::PointerMoved(next)]);
        assert_eq!(half_colors(&shapes, next, true), vec![red]);
        assert_eq!(half_colors(&shapes, next, false), vec![red]);
        // Letting go takes it away.
        assert!(f.run(vec![button(next, false)]).is_empty());
    }

    #[test]
    fn no_ring_for_another_tool_or_a_press_on_the_options_bar() {
        let mut f = Frame::new(ToolId::Brush);
        let at = Pos2::new(600.0, 500.0);
        f.run(vec![egui::Event::PointerMoved(at)]);
        assert!(
            f.run(vec![button(at, true)]).is_empty(),
            "the Brush has no ring"
        );

        let mut f = Frame::new(ToolId::Eyedropper);
        let bar = Pos2::new(600.0, 10.0);
        f.run(vec![egui::Event::PointerMoved(bar)]);
        f.run(vec![egui::Event::PointerMoved(bar)]);
        assert!(
            f.run(vec![button(bar, true)]).is_empty(),
            "a press on the options bar is not a canvas pick"
        );
    }

    /// W13-I: every new control is drawn by the real options bar — the Line's
    /// arrowheads, the Crop's Content-Aware box, the Eyedropper's Sample
    /// choice — and the Gradient's Style offers Shape Burst.
    #[test]
    fn the_options_bar_draws_the_w13i_controls() {
        let drawn = |tool: ToolId, key: &'static str| {
            let mut w = Workspace::new();
            w.palette.activate(&crate::PaletteModel::build(), tool);
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let input = || egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    Pos2::ZERO,
                    egui::vec2(8000.0, 400.0),
                )),
                ..Default::default()
            };
            for _ in 0..2 {
                let _ = ctx.run(input(), |ctx| super::super::tool_options(&mut w, ctx));
            }
            ctx.read_response(super::super::ids::tool_option(tool, key))
                .map(|r| r.rect)
                .filter(|r| r.is_positive() && r.is_finite())
        };
        for (tool, key) in [
            (ToolId::Line, "arrow_start"),
            (ToolId::Line, "arrow_end"),
            (ToolId::Line, "arrow_width"),
            (ToolId::Line, "arrow_length"),
            (ToolId::Line, "arrow_concavity"),
            (ToolId::Crop, tools::edit::CROP_CONTENT_AWARE_KEY),
            (ToolId::Eyedropper, tools::tool::SAMPLE_LAYERS_KEY),
            (ToolId::Gradient, "shape"),
        ] {
            assert!(drawn(tool, key).is_some(), "{tool:?}: no `{key}` control");
        }
        let style = tools::registry::info(ToolId::Gradient)
            .unwrap()
            .options
            .iter()
            .find(|o| o.key == "shape")
            .unwrap();
        let tools::OptionKind::Choice { choices, .. } = style.kind else {
            panic!("Style is a choice");
        };
        assert!(choices.contains(&"Shape Burst"), "{choices:?}");
        let sample = tools::registry::info(ToolId::Eyedropper)
            .unwrap()
            .options
            .iter()
            .find(|o| o.key == tools::tool::SAMPLE_LAYERS_KEY)
            .unwrap();
        let tools::OptionKind::Choice { choices, default } = sample.kind else {
            panic!("Sample is a choice");
        };
        assert_eq!(
            choices,
            ["Current Layer", "Current & Below", "All Layers"].as_slice()
        );
        assert_eq!(choices[default], "All Layers");
    }
}
