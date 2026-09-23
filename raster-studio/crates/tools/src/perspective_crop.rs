//! W7-F: the Perspective Crop tool — Photopea's second tool in the Crop slot.
//!
//! # The gesture
//!
//! A drag draws a rectangle, which becomes a four-corner quad on release.
//! Pressing near one of its corners (within [`CORNER_GRAB_PX`] screen pixels)
//! drags that corner alone, so the quad can be laid over something that is
//! photographed at an angle — a sign, a document, a facade. Pressing anywhere
//! else starts a fresh quad. Nothing is emitted until [`Tool::commit`]
//! (Enter), exactly like [`crate::edit::CropTool`].
//!
//! # What Enter does
//!
//! The quad is mapped onto an upright rectangle through a projective map
//! ([`crate::transform::Homography`]) and the active layer's pixels are
//! resampled through it, so what was a trapezoid in the photo is square in
//! the result; then the canvas becomes that rectangle. The output size is the
//! quad's average edge lengths (the `width` / `height` options override
//! either, `0` meaning "from the quad"). All of it is ONE
//! [`Command::Transaction`] — the rectified pixels
//! ([`Command::PaintTiles`]), the new canvas size
//! ([`Command::SetCanvasSize`]) and one translation per root layer
//! ([`Command::TransformLayer`]) — so a perspective crop is one Ctrl+Z.
//!
//! # Limits, named
//!
//! * Only the **active raster layer** is rectified; the other layers are
//!   cropped (moved under the new canvas) but not warped. A text, shape or
//!   smart-object layer is refused with [`ToolError::NonAffineParametric`]
//!   rather than silently rasterised.
//! * The resample is bicubic in linear premultiplied light, the same sampler
//!   Free Transform's perspective mode uses.

use editor_core::{Command, PixelKey, PixelTarget};
use glam::{Affine2, IVec2, UVec2, Vec2};
use raster::PixelRect;

use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::tool::{PointerEvent, SessionGeometry, Tool, ToolContext, ToolId, ToolSetting};
use crate::transform::Homography;

/// How near a corner a press must land, in *screen* pixels, to grab it.
pub const CORNER_GRAB_PX: f32 = 10.0;

/// A drag shorter than this (document pixels) on either axis is a click, not
/// a quad.
pub const MIN_QUAD_PX: f32 = 2.0;

/// The largest output edge the options accept.
pub const MAX_OUTPUT_PX: i32 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Drag {
    /// Rubber-banding a fresh quad from this corner.
    New { from: Vec2 },
    /// Moving one corner of the held quad.
    Corner(usize),
}

/// Perspective Crop: drag a quad, adjust its corners, Enter rectifies.
#[derive(Debug, Clone, Default)]
pub struct PerspectiveCropTool {
    /// The held quad, clockwise from the top-left, in document pixels.
    quad: Option<[Vec2; 4]>,
    drag: Option<Drag>,
    /// Output width in pixels; `0` derives it from the quad.
    pub width: u32,
    /// Output height in pixels; `0` derives it from the quad.
    pub height: u32,
}

fn rect_quad(a: Vec2, b: Vec2) -> [Vec2; 4] {
    let lo = a.min(b);
    let hi = a.max(b);
    [lo, Vec2::new(hi.x, lo.y), hi, Vec2::new(lo.x, hi.y)]
}

impl PerspectiveCropTool {
    /// The held quad, if any.
    pub fn quad(&self) -> Option<[Vec2; 4]> {
        self.quad
    }

    /// Replace the held quad (the corners the options bar or a test names).
    pub fn set_quad(&mut self, quad: [Vec2; 4]) -> Result<(), ToolError> {
        for p in quad {
            crate::error::finite_pt("perspective crop corner", p)?;
        }
        self.quad = Some(quad);
        Ok(())
    }

    /// The output size the commit will produce for `quad`: the average of
    /// each pair of opposite edges, overridden by the `width` / `height`
    /// options when they are non-zero.
    pub fn output_size(&self, quad: [Vec2; 4]) -> Option<UVec2> {
        let top = (quad[1] - quad[0]).length();
        let bottom = (quad[2] - quad[3]).length();
        let left = (quad[3] - quad[0]).length();
        let right = (quad[2] - quad[1]).length();
        let w = if self.width > 0 {
            self.width as f32
        } else {
            ((top + bottom) * 0.5).round()
        };
        let h = if self.height > 0 {
            self.height as f32
        } else {
            ((left + right) * 0.5).round()
        };
        if !w.is_finite() || !h.is_finite() || w < 1.0 || h < 1.0 {
            return None;
        }
        Some(UVec2::new(
            w.min(MAX_OUTPUT_PX as f32) as u32,
            h.min(MAX_OUTPUT_PX as f32) as u32,
        ))
    }

    fn grab_radius(ctx: &ToolContext<'_>) -> f32 {
        let zoom = if ctx.view.zoom.is_finite() && ctx.view.zoom > 0.0 {
            ctx.view.zoom
        } else {
            1.0
        };
        CORNER_GRAB_PX / zoom
    }

    /// Rectify the active layer into `rect` (document pixels) through the
    /// map `rect -> quad`, returning the tile delta.
    fn rectify(
        ctx: &mut ToolContext<'_>,
        layer: layer_model::LayerId,
        quad: [Vec2; 4],
        rect: PixelRect,
    ) -> Result<editor_core::TileDelta, ToolError> {
        let (w, h) = (rect.width as f32, rect.height as f32);
        let origin = Vec2::new(rect.x as f32, rect.y as f32);
        let local = [
            Vec2::ZERO,
            Vec2::new(w, 0.0),
            Vec2::new(w, h),
            Vec2::new(0.0, h),
        ];
        let map = Homography::from_quads(local, quad).ok_or_else(ToolError::not_invertible)?;
        // Document -> layer pixels (card 040): a moved or scaled layer is
        // rectified where it displays.
        let to_layer = ctx.sample_to_layer.unwrap_or(Affine2::IDENTITY);
        let to_doc = to_layer.inverse();
        if !to_doc.is_finite() {
            return Err(ToolError::not_invertible());
        }
        let bbox = |pts: &[Vec2]| -> Option<PixelRect> {
            let lo = pts
                .iter()
                .fold(Vec2::splat(f32::INFINITY), |a, p| a.min(*p));
            let hi = pts
                .iter()
                .fold(Vec2::splat(f32::NEG_INFINITY), |a, p| a.max(*p));
            if !lo.is_finite() || !hi.is_finite() {
                return None;
            }
            let x0 = lo.x.floor() as i64 - 2;
            let y0 = lo.y.floor() as i64 - 2;
            let x1 = hi.x.ceil() as i64 + 2;
            let y1 = hi.y.ceil() as i64 + 2;
            Some(PixelRect::new(x0, y0, (x1 - x0) as u32, (y1 - y0) as u32))
        };
        let quad_layer: Vec<Vec2> = quad.iter().map(|p| to_layer.transform_point2(*p)).collect();
        let dest_corners: Vec<Vec2> = local
            .iter()
            .map(|p| to_layer.transform_point2(*p + origin))
            .collect();
        let src_rect = bbox(&quad_layer).ok_or(ToolError::Degenerate)?;
        let dst_rect = bbox(&dest_corners).ok_or(ToolError::Degenerate)?;
        let key = PixelKey::Layer(layer);
        let source = ColorPatch::load(&*ctx.tiles, key, src_rect)?;
        let src_box = source.rect();
        let src_origin = source.origin();
        let mut out = ColorPatch::load(&*ctx.tiles, key, dst_rect)?;
        let (dx0, dy0) = (dst_rect.x, dst_rect.y);
        for ly in dy0..dst_rect.bottom() {
            for lx in dx0..dst_rect.right() {
                let q = Vec2::new(lx as f32 + 0.5, ly as f32 + 0.5);
                let p = to_doc.transform_point2(q) - origin;
                if p.x < 0.0 || p.y < 0.0 || p.x >= w || p.y >= h {
                    continue;
                }
                let Some(s_doc) = map.apply(p) else {
                    continue;
                };
                let s = to_layer.transform_point2(s_doc);
                let inside = s.x >= src_box.x as f32
                    && s.y >= src_box.y as f32
                    && s.x < src_box.right() as f32
                    && s.y < src_box.bottom() as f32;
                let px = if inside {
                    source.buffer().sample_bicubic(
                        s.x - src_origin.x as f32,
                        s.y - src_origin.y as f32,
                        filters::EdgeMode::Clamp,
                    )
                } else {
                    [0.0; 4]
                };
                out.set(IVec2::new(lx as i32, ly as i32), px);
            }
        }
        out.commit(&mut *ctx.tiles, key)
    }
}

impl Tool for PerspectiveCropTool {
    fn id(&self) -> ToolId {
        ToolId::PerspectiveCrop
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let pos = crate::error::finite_pt("perspective crop point", event.pos)?;
        if let Some(quad) = self.quad {
            let radius = Self::grab_radius(ctx);
            let nearest = quad
                .iter()
                .enumerate()
                .map(|(i, c)| (i, (*c - pos).length()))
                .filter(|(_, d)| *d <= radius)
                .min_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, _)) = nearest {
                self.drag = Some(Drag::Corner(i));
                return Ok(());
            }
        }
        self.quad = None;
        self.drag = Some(Drag::New { from: pos });
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
        match self.drag {
            Some(Drag::New { from }) => self.quad = Some(rect_quad(from, event.pos)),
            Some(Drag::Corner(i)) => {
                if let Some(q) = &mut self.quad {
                    q[i] = event.pos;
                }
            }
            None => {}
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.on_pointer_move(ctx, event)?;
        if let Some(Drag::New { from }) = self.drag.take() {
            let d = (event.pos - from).abs();
            if d.x < MIN_QUAD_PX || d.y < MIN_QUAD_PX {
                // A click is not a quad.
                self.quad = None;
            }
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.quad = None;
        self.drag = None;
    }

    /// Enter: rectify and crop, as one transaction.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let quad = self.quad.ok_or(ToolError::Degenerate)?;
        let layer = ctx.active_layer.ok_or(ToolError::NoActiveLayer)?;
        if ctx.active_layer_parametric {
            return Err(ToolError::NonAffineParametric);
        }
        let size = self.output_size(quad).ok_or(ToolError::Degenerate)?;
        let lo = quad
            .iter()
            .fold(Vec2::splat(f32::INFINITY), |a, p| a.min(*p));
        let rect = PixelRect::new(lo.x.round() as i64, lo.y.round() as i64, size.x, size.y);
        let delta = Self::rectify(ctx, layer, quad, rect)?;
        let mut commands = Vec::new();
        if !delta.is_empty() {
            commands.push(Command::PaintTiles {
                target: PixelTarget::Layer(layer),
                delta,
            });
        }
        commands.push(Command::SetCanvasSize { size });
        let to_new = Affine2::from_translation(-Vec2::new(rect.x as f32, rect.y as f32));
        if to_new != Affine2::IDENTITY {
            let matrix = to_new.to_cols_array();
            let mut roots: Vec<layer_model::LayerId> = ctx
                .layer_parents
                .iter()
                .filter(|(_, parent)| parent.is_none())
                .map(|(id, _)| *id)
                .collect();
            if roots.is_empty() {
                // A context with no ancestry map (a bare harness): the active
                // layer is the one layer there is to move.
                roots.push(layer);
            }
            for id in roots {
                commands.push(Command::TransformLayer {
                    layer_id: id,
                    matrix,
                });
            }
        }
        ctx.emit(Command::Transaction {
            label: "Perspective Crop".into(),
            commands,
        });
        self.quad = None;
        self.drag = None;
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        self.quad.is_some() && self.drag.is_none()
    }

    fn is_active(&self) -> bool {
        self.quad.is_some() || self.drag.is_some()
    }

    /// The quad, drawn closed — the same outline the lasso overlay paints.
    fn live_geometry(&self) -> Option<SessionGeometry> {
        let quad = self.quad?;
        Some(SessionGeometry::Lasso {
            points: quad.to_vec(),
            closed: true,
        })
    }

    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let slot = match key {
            "width" => &mut self.width,
            "height" => &mut self.height,
            _ => {
                return Err(ToolError::UnknownOption {
                    key: key.to_owned(),
                })
            }
        };
        match setting {
            ToolSetting::Int(v) => {
                *slot = v.clamp(0, MAX_OUTPUT_PX) as u32;
                Ok(())
            }
            _ => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;

    /// A 64x64 layer: a white field with a red square over 16..48.
    fn fixture(tiles: &mut MemoryTiles, layer: LayerId) {
        for y in 0..64 {
            for x in 0..64 {
                let red = (16..48).contains(&x) && (16..48).contains(&y);
                let px = if red {
                    [255, 0, 0, 255]
                } else {
                    [255, 255, 255, 255]
                };
                tiles.put_pixel(PixelKey::Layer(layer), x, y, px);
            }
        }
    }

    #[test]
    fn a_drag_makes_a_quad_a_corner_press_moves_one_corner_and_escape_drops_it() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        let mut tool = PerspectiveCropTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(50.0, 40.0))
            .unwrap();
        assert_eq!(
            tool.quad(),
            Some(rect_quad(Vec2::new(10.0, 10.0), Vec2::new(50.0, 40.0)))
        );
        assert!(tool.has_pending_commit());
        // Grab the top-right corner and pull it inwards.
        tool.on_pointer_down(&mut ctx, PointerEvent::at(49.0, 11.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(44.0, 12.0))
            .unwrap();
        let q = tool.quad().unwrap();
        assert_eq!(q[1], Vec2::new(44.0, 12.0));
        assert_eq!(q[0], Vec2::new(10.0, 10.0), "only the grabbed corner moved");
        assert!(ctx.commands().is_empty(), "nothing emits before Enter");
        tool.cancel(&mut ctx);
        assert!(tool.quad().is_none() && !tool.is_active());
    }

    #[test]
    fn commit_rectifies_a_trapezoid_into_an_upright_canvas_in_one_transaction() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        fixture(&mut tiles, layer);
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64)).with_layer(layer);
        ctx.layer_parents = vec![(layer, None)];
        let mut tool = PerspectiveCropTool::default();
        // A trapezoid hugging the red square: the top edge is pinched in, so
        // the rectified output widens the square's top back out.
        tool.set_quad([
            Vec2::new(20.0, 16.0),
            Vec2::new(44.0, 16.0),
            Vec2::new(48.0, 48.0),
            Vec2::new(16.0, 48.0),
        ])
        .unwrap();
        tool.commit(&mut ctx).unwrap();
        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "one history step: {commands:?}");
        let Command::Transaction { commands, .. } = &commands[0] else {
            panic!("not a transaction: {commands:?}");
        };
        assert!(matches!(commands[0], Command::PaintTiles { .. }));
        // Average edges: (24 + 32) / 2 = 28 wide, 32 tall.
        assert!(commands
            .iter()
            .any(|c| matches!(c, Command::SetCanvasSize { size } if *size == UVec2::new(28, 32))));
        assert!(commands
            .iter()
            .any(|c| matches!(c, Command::TransformLayer { layer_id, .. } if *layer_id == layer)));
        // The rectified region is red edge to edge: the pinched corners of
        // the trapezoid were stretched onto the rect's corners.
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            unreachable!()
        };
        tiles.apply_delta(PixelKey::Layer(layer), delta);
        // The output rect sits at the quad's top-left (16, 16), 28 x 32.
        for (x, y) in [(17, 17), (42, 17), (17, 46), (42, 46), (30, 30)] {
            let px = tiles.pixel(PixelKey::Layer(layer), x, y);
            assert!(
                px[0] > 230 && px[1] < 25,
                "({x},{y}) is not red after rectification: {px:?}"
            );
        }
    }

    #[test]
    fn a_click_is_not_a_quad_and_enter_without_one_is_refused() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 64, 64));
        let mut tool = PerspectiveCropTool::default();
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(10.5, 10.0))
            .unwrap();
        assert!(tool.quad().is_none());
        assert!(tool.commit(&mut ctx).is_err());
    }

    #[test]
    fn the_size_options_override_the_derived_size_and_refuse_the_wrong_kind() {
        let mut tool = PerspectiveCropTool::default();
        let quad = rect_quad(Vec2::ZERO, Vec2::new(30.0, 20.0));
        assert_eq!(tool.output_size(quad), Some(UVec2::new(30, 20)));
        tool.set_setting("width", ToolSetting::Int(100)).unwrap();
        assert_eq!(tool.output_size(quad), Some(UVec2::new(100, 20)));
        assert!(tool.set_setting("width", ToolSetting::Float(1.0)).is_err());
        assert!(tool.set_setting("nope", ToolSetting::Int(1)).is_err());
    }
}
