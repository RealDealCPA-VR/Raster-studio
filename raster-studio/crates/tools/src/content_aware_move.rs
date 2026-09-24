//! W10-A: the Content-Aware Move tool (Photoshop's, in the Healing slot).
//!
//! Two gestures, like the Patch tool's. A drag that starts outside the
//! selection lassoes a region and makes it the selection (one history step,
//! the Lasso's own). A drag that starts inside the selection moves it: the
//! selected pixels are lifted from the active layer, the hole they leave is
//! synthesised by PatchMatch from the pixels around it
//! ([`filters::content_aware_fill`], the engine W7-I's content-aware fill and
//! Spot Healing use), and the lifted patch is laid in at the destination with
//! its edge blended over [`ContentAwareMoveTool::adaptation`] pixels. In
//! Extend mode the source is left alone and only the copy lands.
//!
//! The move is ONE history step: the painted tiles and the selection, which
//! follows the patch to where it was dropped, commit together as one
//! [`Command::Transaction`].
//!
//! Positions are the document's pixel grid, as the Patch tool's are: the
//! region, the hole and the destination are all read and written at the
//! document coordinates the pointer reports.

use editor_core::{Command, Selection, SelectionMask};
use filters::{blur::gaussian_blur, EdgeMode, FilterBuffer};
use glam::{IVec2, Vec2};
use raster::PixelRect;
use selection::lasso::lasso_freehand;

use crate::error::{finite, ToolError};
use crate::patch::ColorPatch;
use crate::tool::{PointerEvent, SelectionEdit, Tool, ToolContext, ToolId, ToolSetting};

/// The options-bar key of [`CamMode`].
pub const MODE_KEY: &str = "mode";

/// The options-bar key of [`ContentAwareMoveTool::adaptation`].
pub const ADAPTATION_KEY: &str = "adaptation";

/// The largest edge blend, in pixels.
pub const MAX_ADAPTATION: f32 = 16.0;

/// The seed the source fill synthesises with, so the same move over the same
/// pixels commits the same bytes.
const CAM_SEED: u64 = 0xCA3_0E0F;

/// What a moved patch leaves behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CamMode {
    /// The source is filled content-aware: the object moves.
    #[default]
    Move,
    /// The source is kept: the object is copied (Photoshop's Extend).
    Extend,
}

impl CamMode {
    /// The options bar's labels, in [`CamMode::from_choice`] order.
    pub const CHOICES: &'static [&'static str] = &["Move", "Extend"];

    /// The mode a Choice index names; past the end clamps to the last.
    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => CamMode::Move,
            _ => CamMode::Extend,
        }
    }
}

/// See the module docs.
pub struct ContentAwareMoveTool {
    /// Move or Extend.
    pub mode: CamMode,
    /// The width, in pixels, over which the moved patch's edge is blended
    /// into the destination (a Gaussian feather of the region's coverage);
    /// `0` lays it in with the selection's own edge.
    pub adaptation: f32,
    /// The lasso so far, while the first gesture runs.
    outline: Vec<Vec2>,
    drawing: bool,
    /// The selection being dragged, where the drag began, and where it is.
    drag: Option<(Selection, Vec2, Vec2)>,
}

impl Default for ContentAwareMoveTool {
    fn default() -> Self {
        Self {
            mode: CamMode::Move,
            adaptation: 2.0,
            outline: Vec::new(),
            drawing: false,
            drag: None,
        }
    }
}

/// `sel` moved by `offset` whole pixels.
fn shifted(sel: &Selection, offset: IVec2) -> Result<Selection, ToolError> {
    Ok(match sel {
        Selection::None => Selection::None,
        Selection::Rect { min, max } => Selection::Rect {
            min: *min + offset,
            max: *max + offset,
        },
        Selection::Mask(m) => Selection::Mask(SelectionMask::new(
            m.origin() + offset,
            m.width(),
            m.height(),
            m.coverage().to_vec(),
        )?),
    })
}

/// `true` when `sel` has pixels to move (an empty or absent selection has
/// none: [`Selection::None`] answers full coverage everywhere).
fn has_pixels(sel: &Selection) -> bool {
    !matches!(sel, Selection::None) && !sel.is_empty()
}

impl ContentAwareMoveTool {
    /// The move itself: see the module docs.
    fn perform(
        &self,
        ctx: &mut ToolContext<'_>,
        region: &Selection,
        offset: IVec2,
    ) -> Result<(), ToolError> {
        ctx.require_layer_target()?;
        let Some((min, max)) = region.bounds() else {
            return Ok(());
        };
        let (w, h) = ((max.x - min.x) as i64, (max.y - min.y) as i64);
        // The context: the source and the destination, grown by a margin the
        // synthesis copies from, clipped to the canvas.
        let margin = w.max(h).clamp(16, 96);
        let lo = min.min(min + offset);
        let hi = max.max(max + offset);
        let x0 = (lo.x as i64 - margin).max(ctx.canvas.x);
        let y0 = (lo.y as i64 - margin).max(ctx.canvas.y);
        let x1 = (hi.x as i64 + margin).min(ctx.canvas.right());
        let y1 = (hi.y as i64 + margin).min(ctx.canvas.bottom());
        if x1 <= x0 || y1 <= y0 {
            return Ok(());
        }
        let context = PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32);
        let (cw, ch) = (context.width as usize, context.height as usize);
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let mut patch = ColorPatch::load(ctx.tiles, key, context)?;

        let at = |p: IVec2| -> Option<usize> {
            let (x, y) = (p.x as i64 - x0, p.y as i64 - y0);
            (x >= 0 && y >= 0 && (x as usize) < cw && (y as usize) < ch)
                .then(|| y as usize * cw + x as usize)
        };
        let point =
            |i: usize| IVec2::new((x0 + (i % cw) as i64) as i32, (y0 + (i / cw) as i64) as i32);
        let original: Vec<[f32; 4]> = (0..cw * ch).map(|i| patch.get(point(i))).collect();
        let cov: Vec<f32> = (0..cw * ch)
            .map(|i| region.coverage_at(point(i)).clamp(0.0, 1.0))
            .collect();

        // The blend weight of the lifted patch: the region's coverage,
        // softened toward its edge by the adaptation feather.
        let weight: Vec<f32> = if self.adaptation > 0.0 {
            let plane = FilterBuffer::from_pixels(
                context.width,
                context.height,
                cov.iter().map(|c| [*c; 4]).collect(),
            )?;
            let soft = gaussian_blur(&plane, self.adaptation * 0.5, EdgeMode::Clamp);
            cov.iter()
                .zip(soft.pixels())
                .map(|(c, s)| (c * (2.0 * s[0]).min(1.0)).clamp(0.0, 1.0))
                .collect()
        } else {
            cov.clone()
        };

        let mut result = original.clone();
        if self.mode == CamMode::Move {
            let hole: Vec<bool> = cov.iter().map(|c| *c > 0.0).collect();
            let plane = FilterBuffer::from_pixels(context.width, context.height, original.clone())?;
            let filled = match filters::content_aware_fill(
                &plane,
                &hole,
                filters::FillOptions {
                    seed: CAM_SEED,
                    margin: None,
                },
            ) {
                Ok(filled) => filled,
                Err(filters::ContentAwareError::NoSource) => {
                    crate::stroke::low_frequency_outside(&plane, &cov, 6.0)?
                }
                Err(filters::ContentAwareError::Filter(e)) => return Err(ToolError::Filter(e)),
                Err(_) => return Err(ToolError::Degenerate),
            };
            for (i, px) in result.iter_mut().enumerate() {
                let a = cov[i];
                if a > 0.0 {
                    let f = filled.pixels()[i];
                    *px = std::array::from_fn(|k| px[k] + (f[k] - px[k]) * a);
                }
            }
        }
        // Lay the lifted patch in at the destination.
        for (i, px) in result.iter_mut().enumerate() {
            let Some(s) = at(point(i) - offset) else {
                continue;
            };
            let a = weight[s];
            if a > 0.0 {
                let m = original[s];
                *px = std::array::from_fn(|k| px[k] + (m[k] - px[k]) * a);
            }
        }
        for (i, px) in result.iter().enumerate() {
            if *px != original[i] {
                patch.set(point(i), *px);
            }
        }
        let delta = patch.commit(ctx.tiles, key)?;
        if delta.is_empty() {
            return Ok(());
        }
        ctx.emit(Command::Transaction {
            label: "Content-Aware Move".into(),
            commands: vec![
                Command::PaintTiles { target, delta },
                Command::SetSelection {
                    selection: shifted(region, offset)?,
                },
            ],
        });
        Ok(())
    }
}

impl Tool for ContentAwareMoveTool {
    fn id(&self) -> ToolId {
        ToolId::ContentAwareMove
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("content-aware move point", event.pos)?;
        let p = IVec2::new(event.pos.x.floor() as i32, event.pos.y.floor() as i32);
        if has_pixels(&ctx.selection) && ctx.selection.coverage_at(p) > 0.0 {
            self.drag = Some((ctx.selection.clone(), event.pos, event.pos));
        } else {
            self.drag = None;
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
        if !event.pos.is_finite() {
            return Ok(());
        }
        if self.drawing {
            self.outline.push(event.pos);
        } else if let Some((_, _, current)) = &mut self.drag {
            *current = event.pos;
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
            if event.pos.is_finite() {
                self.outline.push(event.pos);
            }
            let outline = std::mem::take(&mut self.outline);
            if outline.len() >= 3 {
                let mask = lasso_freehand(&outline)?;
                if !mask.is_empty() {
                    ctx.emit_selection(SelectionEdit::new(
                        Selection::Mask(mask),
                        event.modifiers.selection_op(),
                    ));
                }
            }
            return Ok(());
        }
        let Some((region, from, _)) = self.drag.take() else {
            return Ok(());
        };
        crate::error::finite_pt("content-aware move target", event.pos)?;
        let offset = IVec2::new(
            (event.pos.x - from.x).round() as i32,
            (event.pos.y - from.y).round() as i32,
        );
        if offset == IVec2::ZERO {
            return Ok(());
        }
        self.perform(ctx, &region, offset)
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.outline.clear();
        self.drawing = false;
        self.drag = None;
    }

    /// The lasso while it is drawn; the region's outline box, moved with the
    /// pointer, while it is dragged.
    fn live_geometry(&self) -> Option<crate::tool::SessionGeometry> {
        if self.drawing && self.outline.len() > 1 {
            return Some(crate::tool::SessionGeometry::Lasso {
                points: self.outline.clone(),
                closed: true,
            });
        }
        let (region, from, current) = self.drag.as_ref()?;
        let (min, max) = region.bounds()?;
        let d = (*current - *from).round();
        let (a, b) = (min.as_vec2() + d, max.as_vec2() + d);
        Some(crate::tool::SessionGeometry::Lasso {
            points: vec![a, Vec2::new(b.x, a.y), b, Vec2::new(a.x, b.y)],
            closed: true,
        })
    }

    /// `mode` (Move / Extend) and `adaptation` (the edge blend, pixels).
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            (MODE_KEY, ToolSetting::Choice(index)) => {
                self.mode = CamMode::from_choice(index);
                Ok(())
            }
            (ADAPTATION_KEY, ToolSetting::Float(v)) => {
                self.adaptation = finite("adaptation", v)?.clamp(0.0, MAX_ADAPTATION);
                Ok(())
            }
            (MODE_KEY | ADAPTATION_KEY, _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

    fn is_active(&self) -> bool {
        self.drawing || self.drag.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use editor_core::PixelKey;
    use layer_model::LayerId;

    const N: u32 = 64;

    /// A white 64x64 layer with a black 8x8 square at (10..18, 10..18).
    fn fixture() -> (MemoryTiles, LayerId) {
        let layer = LayerId::new();
        let mut tiles = MemoryTiles::new();
        for y in 0..N as i64 {
            for x in 0..N as i64 {
                let ink = (10..18).contains(&x) && (10..18).contains(&y);
                let v = if ink { 0 } else { 255 };
                tiles.put_pixel(PixelKey::Layer(layer), x, y, [v, v, v, 255]);
            }
        }
        (tiles, layer)
    }

    fn luma(tiles: &mut MemoryTiles, layer: LayerId, x: i64, y: i64) -> f32 {
        let c = tiles.pixel(PixelKey::Layer(layer), x, y);
        (c[0] as f32 + c[1] as f32 + c[2] as f32) / (3.0 * 255.0)
    }

    fn run(mode: CamMode) -> (MemoryTiles, LayerId, Vec<Command>) {
        let (mut tiles, layer) = fixture();
        let commands = {
            let mut ctx =
                ToolContext::new(&mut tiles, PixelRect::new(0, 0, N, N)).with_layer(layer);
            ctx.selection = Selection::Rect {
                min: IVec2::new(8, 8),
                max: IVec2::new(20, 20),
            };
            let mut tool = ContentAwareMoveTool {
                mode,
                ..ContentAwareMoveTool::default()
            };
            tool.on_pointer_down(&mut ctx, PointerEvent::at(14.0, 14.0))
                .unwrap();
            tool.on_pointer_move(&mut ctx, PointerEvent::at(30.0, 30.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(44.0, 34.0))
                .unwrap();
            ctx.drain()
        };
        // What the history would install: the transaction's tile delta.
        if let Some(Command::Transaction {
            commands: inner, ..
        }) = commands.first()
        {
            if let Some(Command::PaintTiles { delta, .. }) = inner.first() {
                tiles.apply_delta(PixelKey::Layer(layer), delta);
            }
        }
        (tiles, layer, commands)
    }

    #[test]
    fn move_fills_the_source_and_lands_the_patch_as_one_transaction() {
        let (mut tiles, layer, commands) = run(CamMode::Move);
        assert_eq!(commands.len(), 1, "one command: {commands:?}");
        let Command::Transaction {
            commands: inner, ..
        } = &commands[0]
        else {
            panic!("not a transaction: {:?}", commands[0]);
        };
        assert!(matches!(inner[0], Command::PaintTiles { .. }));
        assert_eq!(
            inner[1],
            Command::SetSelection {
                selection: Selection::Rect {
                    min: IVec2::new(38, 28),
                    max: IVec2::new(50, 40),
                }
            },
            "the selection follows the patch"
        );
        // The object now sits at the destination...
        assert!(luma(&mut tiles, layer, 44, 34) < 0.05);
        // ...and its old place was filled from the white around it.
        assert!(luma(&mut tiles, layer, 14, 14) > 0.9);
    }

    #[test]
    fn extend_keeps_the_source() {
        let (mut tiles, layer, _) = run(CamMode::Extend);
        assert!(luma(&mut tiles, layer, 44, 34) < 0.05, "the copy landed");
        assert!(luma(&mut tiles, layer, 14, 14) < 0.05, "the source stayed");
    }

    #[test]
    fn a_drag_outside_the_selection_lassoes_a_new_one_and_paints_nothing() {
        let (mut tiles, layer) = fixture();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, N, N)).with_layer(layer);
        let mut tool = ContentAwareMoveTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(5.0, 5.0))
            .unwrap();
        for p in [(25.0, 5.0), (25.0, 25.0), (5.0, 25.0)] {
            tool.on_pointer_move(&mut ctx, PointerEvent::at(p.0, p.1))
                .unwrap();
        }
        assert!(matches!(
            tool.live_geometry(),
            Some(crate::tool::SessionGeometry::Lasso { .. })
        ));
        tool.on_pointer_up(&mut ctx, PointerEvent::at(5.0, 5.0))
            .unwrap();
        assert!(ctx.commands().is_empty());
        assert_eq!(ctx.selection_edits().len(), 1);
        assert!(!tool.is_active());
    }

    #[test]
    fn the_options_reach_the_tool_and_refuse_the_wrong_kind() {
        let mut tool = ContentAwareMoveTool::default();
        tool.set_setting(MODE_KEY, ToolSetting::Choice(1)).unwrap();
        assert_eq!(tool.mode, CamMode::Extend);
        tool.set_setting(ADAPTATION_KEY, ToolSetting::Float(99.0))
            .unwrap();
        assert_eq!(tool.adaptation, MAX_ADAPTATION);
        assert!(tool.set_setting(MODE_KEY, ToolSetting::Float(1.0)).is_err());
        assert!(tool.set_setting("nope", ToolSetting::Bool(true)).is_err());
    }
}
