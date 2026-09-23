//! The Pencil, with Auto Erase.
//!
//! The Pencil is the stroke engine's `StrokeOp::Paint` under a hard, aliased,
//! one-pixel brush ([`BrushSettings::pencil`]); everything about the stroke is
//! [`StrokeTool`]'s. What this wrapper adds is Photoshop's **Auto Erase**: with
//! it on, a stroke that *starts* on a pixel of the foreground colour paints the
//! background colour instead, so drawing over a pencil line takes it back out.
//! The decision is made once, at the press, from the pixel under it on the
//! layer being painted — the same moment [`StrokeTool`] reads the colour it
//! paints with.

use glam::{IVec2, Vec2};
use raster::PixelRect;

use crate::brush::BrushSettings;
use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::stroke::{StrokeOp, StrokeTool};
use crate::tool::{PaintTarget, PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

/// The options-bar key Auto Erase travels under.
pub const AUTO_ERASE_KEY: &str = "auto_erase";

/// How far (in 8-bit sRGB steps, per channel) the pixel under the press may
/// sit from the foreground and still count as "on the foreground" — enough to
/// absorb the round trip through linear light, not enough to confuse two
/// colours a user would call different.
const MATCH_TOLERANCE: i32 = 2;

/// The Pencil. See the module docs.
pub struct PencilTool {
    inner: StrokeTool,
    /// Paint the background colour when the stroke starts on the foreground.
    pub auto_erase: bool,
}

impl Default for PencilTool {
    fn default() -> Self {
        Self {
            inner: StrokeTool::new(
                ToolId::Pencil,
                BrushSettings::pencil(1.0),
                StrokeOp::Paint {
                    color: [0.0, 0.0, 0.0, 1.0],
                },
            ),
            auto_erase: false,
        }
    }
}

/// Linear straight-alpha to 8-bit sRGB, for the colour match.
fn srgb8(linear: f32) -> i32 {
    (color::linear_to_srgb(linear.clamp(0.0, 1.0)) * 255.0).round() as i32
}

/// Whether the premultiplied-linear pixel `px` is the straight-linear colour
/// `fg`, to within [`MATCH_TOLERANCE`]. A mostly transparent pixel is never
/// "the foreground": there is no colour there to have been drawn.
fn is_foreground(px: [f32; 4], fg: [f32; 4]) -> bool {
    if px[3] < 0.5 {
        return false;
    }
    (0..3).all(|c| (srgb8(px[c] / px[3]) - srgb8(fg[c])).abs() <= MATCH_TOLERANCE)
}

impl PencilTool {
    /// The pixel under `pos` (document space) on the surface being painted.
    fn pixel_under(ctx: &ToolContext<'_>, pos: Vec2) -> Option<[f32; 4]> {
        let key = ctx.pixel_key().ok()?;
        let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
        let p = to_layer.transform_point2(pos).floor();
        if !p.is_finite() {
            return None;
        }
        let pt = IVec2::new(p.x as i32, p.y as i32);
        let patch = ColorPatch::load(
            ctx.tiles,
            key,
            PixelRect::new(pt.x as i64, pt.y as i64, 1, 1),
        )
        .ok()?;
        Some(patch.get(pt))
    }
}

impl Tool for PencilTool {
    fn id(&self) -> ToolId {
        ToolId::Pencil
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let erase = self.auto_erase
            && ctx.paint_target == PaintTarget::Layer
            && Self::pixel_under(ctx, event.pos)
                .is_some_and(|px| is_foreground(px, ctx.foreground));
        if !erase {
            return self.inner.on_pointer_down(ctx, event);
        }
        // The stroke reads its colour from the context at the press; lend it
        // the background for exactly that read.
        let foreground = ctx.foreground;
        ctx.foreground = ctx.background;
        let result = self.inner.on_pointer_down(ctx, event);
        ctx.foreground = foreground;
        result
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.inner.on_pointer_move(ctx, event)
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.inner.on_pointer_up(ctx, event)
    }

    fn cancel(&mut self, ctx: &mut ToolContext<'_>) {
        self.inner.cancel(ctx);
    }

    fn is_active(&self) -> bool {
        self.inner.is_active()
    }

    /// W4-B: the pencil's stroke previews exactly as the brush's does.
    fn live_paint(
        &mut self,
        ctx: &mut ToolContext<'_>,
    ) -> Result<Option<crate::stroke::LivePaint>, ToolError> {
        self.inner.live_paint(ctx)
    }

    fn set_brush(&mut self, brush: BrushSettings) {
        self.inner.set_brush(brush);
    }

    fn brush(&self) -> Option<BrushSettings> {
        self.inner.brush()
    }

    fn set_choice(&mut self, key: &str, index: usize) {
        self.inner.set_choice(key, index);
    }

    /// `auto_erase` is the Pencil's own; every other key (the brush keys, the
    /// paint blend mode) is the stroke engine's and is answered there.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        if key == AUTO_ERASE_KEY {
            return match setting {
                ToolSetting::Bool(on) => {
                    self.auto_erase = on;
                    Ok(())
                }
                _ => Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                }),
            };
        }
        self.inner.set_setting(key, setting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use editor_core::{Command, PixelKey};
    use layer_model::LayerId;

    const BLACK: [u8; 4] = [0, 0, 0, 255];
    const WHITE: [u8; 4] = [255, 255, 255, 255];

    /// Click the pencil at `(x, y)` on a white layer with a black pixel at
    /// (4, 4), and return the colour it left at the click.
    fn click(auto_erase: bool, x: i64, y: i64) -> [u8; 4] {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let key = PixelKey::Layer(layer);
        for yy in 0..16 {
            for xx in 0..16 {
                tiles.put_pixel(key, xx, yy, WHITE);
            }
        }
        tiles.put_pixel(key, 4, 4, BLACK);
        let mut tool = PencilTool::default();
        tool.set_setting(AUTO_ERASE_KEY, ToolSetting::Bool(auto_erase))
            .unwrap();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 16, 16))
            .with_layer(layer)
            .with_foreground([0.0, 0.0, 0.0, 1.0]);
        ctx.background = [1.0, 1.0, 1.0, 1.0];
        let at = PointerEvent::at(x as f32 + 0.5, y as f32 + 0.5);
        tool.on_pointer_down(&mut ctx, at).unwrap();
        tool.on_pointer_up(&mut ctx, at).unwrap();
        assert_eq!(ctx.foreground, [0.0, 0.0, 0.0, 1.0], "foreground restored");
        let cmds = ctx.drain();
        drop(ctx);
        for c in &cmds {
            if let Command::PaintTiles { delta, .. } = c {
                tiles.apply_delta(key, delta);
            }
        }
        tiles.pixel(key, x, y)
    }

    #[test]
    fn auto_erase_paints_the_background_where_the_stroke_starts_on_the_foreground() {
        assert_eq!(
            click(true, 4, 4),
            WHITE,
            "started on black: erased to white"
        );
        assert_eq!(click(true, 9, 9), BLACK, "started on white: paints black");
        assert_eq!(click(false, 4, 4), BLACK, "off: always the foreground");
    }

    #[test]
    fn the_pencil_still_answers_the_stroke_engines_keys() {
        let mut tool = PencilTool::default();
        assert_eq!(tool.brush(), Some(BrushSettings::pencil(1.0)));
        tool.set_setting(crate::BLEND_MODE_KEY, ToolSetting::Choice(2))
            .unwrap();
        assert!(tool
            .set_setting(AUTO_ERASE_KEY, ToolSetting::Float(1.0))
            .is_err());
        assert_eq!(tool.id(), ToolId::Pencil);
    }
}
