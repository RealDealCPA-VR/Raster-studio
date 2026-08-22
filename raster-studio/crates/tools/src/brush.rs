//! Brush tool: records a vector stroke (points + pressure) during a drag.
//!
//! Per the render doc we record a *vector* stroke with deterministic settings
//! (for reproducibility and compact history), while the renderer materializes
//! painted tile deltas for fast, predictable raster output. This module owns
//! the vector capture; tile rasterization lives in `render`/`raster`.

use glam::Vec2;
use serde::{Deserialize, Serialize};

use crate::tool::{PointerEvent, Tool, ToolContext, ToolId};

/// Deterministic brush parameters. Same settings + same points => same pixels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushSettings {
    pub radius_px: f32,
    pub hardness: f32,
    pub flow: f32,
    pub spacing: f32,
    /// Straight-alpha RGBA color.
    pub color: [f32; 4],
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            radius_px: 12.0,
            hardness: 0.8,
            flow: 1.0,
            spacing: 0.25,
            color: [0.0, 0.0, 0.0, 1.0],
        }
    }
}

/// A single captured point along a stroke.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StrokePoint {
    pub pos: [f32; 2],
    pub pressure: f32,
}

/// A recorded brush stroke: settings + the polyline of sampled points.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrushStroke {
    pub settings: BrushSettings,
    pub points: Vec<StrokePoint>,
}

/// The interactive brush tool. Accumulates points, respecting `spacing` so we
/// don't record thousands of near-identical samples.
pub struct BrushTool {
    settings: BrushSettings,
    active: Option<BrushStroke>,
    last_recorded: Option<Vec2>,
}

impl BrushTool {
    pub fn new(settings: BrushSettings) -> Self {
        Self {
            settings,
            active: None,
            last_recorded: None,
        }
    }

    fn min_step(&self) -> f32 {
        (self.settings.radius_px * 2.0 * self.settings.spacing).max(0.5)
    }

    fn push_point(&mut self, e: PointerEvent) {
        let step = self.min_step();
        let should = match self.last_recorded {
            None => true,
            Some(last) => last.distance(e.pos) >= step,
        };
        if should {
            if let Some(stroke) = &mut self.active {
                stroke.points.push(StrokePoint {
                    pos: [e.pos.x, e.pos.y],
                    pressure: e.pressure,
                });
                self.last_recorded = Some(e.pos);
            }
        }
    }

    /// The stroke currently being drawn, if any (for live preview).
    pub fn active_stroke(&self) -> Option<&BrushStroke> {
        self.active.as_ref()
    }
}

impl Tool for BrushTool {
    fn id(&self) -> ToolId {
        ToolId::Brush
    }

    fn on_pointer_down(&mut self, _ctx: &mut ToolContext, event: PointerEvent) {
        self.active = Some(BrushStroke {
            settings: self.settings.clone(),
            points: Vec::new(),
        });
        self.last_recorded = None;
        self.push_point(event);
    }

    fn on_pointer_move(&mut self, _ctx: &mut ToolContext, event: PointerEvent) {
        if self.active.is_some() {
            self.push_point(event);
        }
    }

    fn on_pointer_up(&mut self, _ctx: &mut ToolContext, event: PointerEvent) {
        if self.active.is_some() {
            self.push_point(event);
        }
        // On finish, the app would turn `self.active` into a PaintStroke
        // command (SetMaskTile / tile-delta) via the renderer. The command
        // variant is added when the tile-paint path lands; we simply clear.
        self.active = None;
        self.last_recorded = None;
    }

    fn cancel(&mut self, _ctx: &mut ToolContext) {
        self.active = None;
        self.last_recorded = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(x: f32, y: f32) -> PointerEvent {
        PointerEvent {
            pos: Vec2::new(x, y),
            pressure: 1.0,
            shift: false,
            alt: false,
            ctrl: false,
        }
    }

    #[test]
    fn spacing_throttles_points() {
        let mut b = BrushTool::new(BrushSettings {
            radius_px: 10.0,
            spacing: 0.5,
            ..Default::default()
        });
        let mut ctx = ToolContext::new(None);
        b.on_pointer_down(&mut ctx, ev(0.0, 0.0));
        // min_step = 10*2*0.5 = 10px. A 3px move should NOT record.
        b.on_pointer_move(&mut ctx, ev(3.0, 0.0));
        assert_eq!(b.active_stroke().unwrap().points.len(), 1);
        // A 12px move SHOULD record.
        b.on_pointer_move(&mut ctx, ev(12.0, 0.0));
        assert_eq!(b.active_stroke().unwrap().points.len(), 2);
    }

    #[test]
    fn cancel_discards_stroke() {
        let mut b = BrushTool::new(BrushSettings::default());
        let mut ctx = ToolContext::new(None);
        b.on_pointer_down(&mut ctx, ev(0.0, 0.0));
        b.cancel(&mut ctx);
        assert!(b.active_stroke().is_none());
    }
}
