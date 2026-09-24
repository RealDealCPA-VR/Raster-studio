//! W7-F: the Artboard tool — drag out an artboard.
//!
//! An artboard is a group whose bottom child is a background plate naming
//! the artboard's rect and colour ([`layer_model::artboard`]). A drag makes
//! one on release, as ONE [`Command::Transaction`]: the group, the plate, the
//! move that puts the plate inside the group, and — unless the background is
//! Transparent — the plate's pixels, the background colour over the rect. One
//! Ctrl+Z takes the whole artboard back.
//!
//! W8-C: the compositor clips an artboard's contents to its rect
//! (`compositor::composite`, `artboard_clip`), and File > Export > Artboards
//! to Files (`app_shell::artboard_export`) writes each one as its own image,
//! as Photopea does.

use editor_core::{Command, PixelKey, PixelTarget};
use glam::{IVec2, Vec2};
use layer_model::{Artboard, Layer, LayerKind, RasterLayer};
use raster::PixelRect;

use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::select::MarqueeShape;
use crate::tool::{PointerEvent, SessionGeometry, Tool, ToolContext, ToolId, ToolSetting};

/// The Background choices, in [`ArtboardBackground::from_choice`]'s order.
pub const BACKGROUND_CHOICES: &[&str] = &["White", "Black", "Transparent", "Background Colour"];

/// What an artboard is filled with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtboardBackground {
    #[default]
    White,
    Black,
    Transparent,
    /// The document's background colour at the moment of the drag.
    BackgroundColor,
}

impl ArtboardBackground {
    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => Self::White,
            1 => Self::Black,
            2 => Self::Transparent,
            _ => Self::BackgroundColor,
        }
    }

    /// Straight-alpha linear RGBA.
    fn color(self, ctx: &ToolContext<'_>) -> [f32; 4] {
        match self {
            Self::White => [1.0, 1.0, 1.0, 1.0],
            Self::Black => [0.0, 0.0, 0.0, 1.0],
            Self::Transparent => [0.0, 0.0, 0.0, 0.0],
            Self::BackgroundColor => ctx.background,
        }
    }
}

/// The smallest artboard edge a drag makes, in document pixels.
pub const MIN_ARTBOARD_PX: f32 = 2.0;

/// The Artboard tool.
#[derive(Debug, Clone, Default)]
pub struct ArtboardTool {
    from: Option<Vec2>,
    to: Vec2,
    pub background: ArtboardBackground,
}

impl ArtboardTool {
    fn rect(&self) -> Option<PixelRect> {
        let from = self.from?;
        let lo = from.min(self.to).round();
        let hi = from.max(self.to).round();
        let size = hi - lo;
        if size.x < MIN_ARTBOARD_PX || size.y < MIN_ARTBOARD_PX {
            return None;
        }
        Some(PixelRect::new(
            lo.x as i64,
            lo.y as i64,
            size.x as u32,
            size.y as u32,
        ))
    }

    /// The transaction that makes the artboard over `rect`.
    fn build(&self, ctx: &mut ToolContext<'_>, rect: PixelRect) -> Result<Command, ToolError> {
        let background = self.background.color(ctx);
        let board = Artboard {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
            background,
        };
        let group = Layer::group("Artboard");
        let plate = Layer::with_kind(
            "Artboard Background",
            LayerKind::Raster(RasterLayer {
                artboard: Some(board),
                ..RasterLayer::default()
            }),
        );
        let (group_id, plate_id) = (group.id, plate.id);
        let mut commands = vec![
            Command::create_layer(group),
            Command::create_layer(plate),
            Command::MoveLayer {
                layer_id: plate_id,
                parent: Some(group_id),
                index: 0,
            },
        ];
        if background[3] > 0.0 {
            let key = PixelKey::Layer(plate_id);
            let mut patch = ColorPatch::load(&*ctx.tiles, key, rect)?;
            let a = background[3].clamp(0.0, 1.0);
            let px = [background[0] * a, background[1] * a, background[2] * a, a];
            for y in rect.y..rect.bottom() {
                for x in rect.x..rect.right() {
                    patch.set(IVec2::new(x as i32, y as i32), px);
                }
            }
            let delta = patch.commit(&mut *ctx.tiles, key)?;
            if !delta.is_empty() {
                commands.push(Command::PaintTiles {
                    target: PixelTarget::Layer(plate_id),
                    delta,
                });
            }
        }
        Ok(Command::Transaction {
            label: "Artboard".into(),
            commands,
        })
    }
}

impl Tool for ArtboardTool {
    fn id(&self) -> ToolId {
        ToolId::Artboard
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let pos = crate::error::finite_pt("artboard corner", event.pos)?;
        self.from = Some(pos);
        self.to = pos;
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if self.from.is_some() && event.pos.is_finite() {
            self.to = event.pos;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.on_pointer_move(ctx, event)?;
        let rect = self.rect();
        self.from = None;
        let Some(rect) = rect else {
            return Ok(());
        };
        let command = self.build(ctx, rect)?;
        ctx.emit(command);
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.from = None;
    }

    fn is_active(&self) -> bool {
        self.from.is_some()
    }

    /// The rubber band while the button is down.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        let from = self.from?;
        Some(SessionGeometry::Marquee {
            shape: MarqueeShape::Rect,
            rect: [from.min(self.to), from.max(self.to)],
        })
    }

    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("background", ToolSetting::Choice(i)) => {
                self.background = ArtboardBackground::from_choice(i);
                Ok(())
            }
            ("background", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;

    #[test]
    fn a_drag_makes_one_transaction_with_a_group_and_a_filled_plate() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128));
        let mut tool = ArtboardTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 20.0))
            .unwrap();
        tool.on_pointer_move(&mut ctx, PointerEvent::at(40.0, 50.0))
            .unwrap();
        assert!(tool.live_geometry().is_some());
        tool.on_pointer_up(&mut ctx, PointerEvent::at(50.0, 60.0))
            .unwrap();
        let commands = ctx.drain();
        assert_eq!(commands.len(), 1);
        let Command::Transaction { commands, .. } = &commands[0] else {
            panic!("{commands:?}");
        };
        let Command::CreateLayer { layer: plate } = &commands[1] else {
            panic!("{commands:?}");
        };
        let LayerKind::Raster(r) = &plate.kind else {
            panic!("the plate is not raster");
        };
        let board = r.artboard.expect("the plate names its artboard");
        assert_eq!(
            (board.x, board.y, board.width, board.height),
            (10, 20, 40, 40)
        );
        assert!(matches!(commands[2], Command::MoveLayer { .. }));
        let Command::PaintTiles { delta, .. } = &commands[3] else {
            panic!("the white background was not painted");
        };
        tiles.apply_delta(PixelKey::Layer(plate.id), delta);
        assert_eq!(
            tiles.pixel(PixelKey::Layer(plate.id), 30, 40),
            [255, 255, 255, 255]
        );
        assert_eq!(tiles.pixel(PixelKey::Layer(plate.id), 5, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn a_transparent_artboard_paints_nothing_and_a_click_makes_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 128));
        let mut tool = ArtboardTool::default();
        tool.set_setting("background", ToolSetting::Choice(2))
            .unwrap();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(11.0, 10.0))
            .unwrap();
        assert!(ctx.commands().is_empty(), "a click is not an artboard");
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(30.0, 30.0))
            .unwrap();
        let Command::Transaction { commands, .. } = &ctx.commands()[0] else {
            panic!();
        };
        assert_eq!(commands.len(), 3, "no paint for a transparent board");
        assert!(tool.set_setting("background", ToolSetting::Int(1)).is_err());
    }
}
