//! Flood fill and pattern fill.
//!
//! The flood itself is not reimplemented here: `selection`'s magic wand already
//! answers "which pixels are within tolerance of this one, optionally only the
//! ones connected to it, with an anti-aliased rim". A paint bucket *is* that
//! question plus a composite, so the tool asks it and then paints the coverage
//! it gets back. One algorithm, one set of edge cases, and a bucket whose
//! tolerance behaves identically to the wand's — which is what users expect,
//! because in every editor they are the same control.

use color::{premultiply, srgb8_to_linear};
use editor_core::{Command, Selection, SelectionMask};
use glam::IVec2;
use raster::PixelRect;
use selection::{
    boolean::{combine, to_mask, BooleanOp},
    wand::{magic_wand, WandOptions},
    ImageView,
};
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::patch::{read_rgba8, ColorPatch};
use crate::tool::{Pattern, PointerEvent, Tool, ToolContext, ToolId};

/// What a fill lays down.
#[derive(Debug, Clone, PartialEq)]
pub enum FillContent {
    /// The context's foreground colour.
    Foreground,
    /// A specific straight-alpha linear RGBA.
    Color([f32; 4]),
    /// The context's active pattern.
    Pattern,
}

/// Paint-bucket options.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FillSettings {
    /// Largest accepted per-channel difference, normalised — the familiar
    /// "tolerance 32" is `32.0 / 255.0`.
    pub tolerance: f32,
    /// Fill only the region connected to the click.
    pub contiguous: bool,
    /// Ramp coverage out over the last part of the tolerance so the fill's rim
    /// is not stair-stepped.
    pub antialias: bool,
    pub opacity: f32,
    /// Judge tolerance against the flattened composite rather than the layer.
    pub sample_merged: bool,
}

impl Default for FillSettings {
    fn default() -> Self {
        Self {
            tolerance: 32.0 / 255.0,
            contiguous: true,
            antialias: true,
            opacity: 1.0,
            sample_merged: false,
        }
    }
}

impl FillSettings {
    fn wand(&self) -> WandOptions {
        WandOptions {
            tolerance: self.tolerance.clamp(0.0, 1.0),
            contiguous: self.contiguous,
            antialias: if self.antialias { 0.5 } else { 0.0 },
            metric: Default::default(),
            // A bucket has to see the difference between "transparent" and
            // "white", or filling a hole in a layer would bleed into the
            // opaque pixels around it.
            sample_alpha: true,
        }
    }
}

/// Composite `content` through a coverage mask onto a patch.
///
/// Shared by the paint bucket, the pattern fill and Edit ▸ Fill, so a fill
/// through a feathered selection lands identically however it was invoked.
pub fn fill_masked(
    patch: &mut ColorPatch,
    mask: &SelectionMask,
    content: &FillContent,
    foreground: [f32; 4],
    pattern: Option<&Pattern>,
    opacity: f32,
) -> Result<(), ToolError> {
    let Some((min, max)) = mask.bounds() else {
        return Ok(());
    };
    let opacity = opacity.clamp(0.0, 1.0);
    if matches!(content, FillContent::Pattern) && pattern.is_none() {
        return Err(ToolError::Degenerate);
    }
    for y in min.y..max.y {
        for x in min.x..max.x {
            let p = IVec2::new(x, y);
            let cov = mask.coverage_at(p) as f32 / 255.0;
            if cov <= 0.0 || patch.index_of(p).is_none() {
                continue;
            }
            let straight = match content {
                FillContent::Foreground => foreground,
                FillContent::Color(c) => *c,
                FillContent::Pattern => {
                    let s = pattern.expect("checked above").sample(x as i64, y as i64);
                    [
                        srgb8_to_linear(s[0]),
                        srgb8_to_linear(s[1]),
                        srgb8_to_linear(s[2]),
                        s[3] as f32 / 255.0,
                    ]
                }
            };
            let a = (straight[3] * cov * opacity).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let src = premultiply([straight[0], straight[1], straight[2], a]);
            let dst = patch.get(p);
            patch.set(
                p,
                [
                    src[0] + dst[0] * (1.0 - a),
                    src[1] + dst[1] * (1.0 - a),
                    src[2] + dst[2] * (1.0 - a),
                    a + dst[3] * (1.0 - a),
                ],
            );
        }
    }
    Ok(())
}

/// The region a fill considers, and the mask it produced.
fn flood(
    ctx: &ToolContext<'_>,
    seed: IVec2,
    settings: &FillSettings,
) -> Result<SelectionMask, ToolError> {
    let canvas = ctx.canvas;
    if (seed.x as i64) < canvas.x
        || (seed.y as i64) < canvas.y
        || (seed.x as i64) >= canvas.right()
        || (seed.y as i64) >= canvas.bottom()
    {
        return Err(ToolError::PointOutside {
            x: seed.x,
            y: seed.y,
        });
    }
    let key = if settings.sample_merged {
        ctx.sample_key()?
    } else {
        ctx.pixel_key()?
    };
    let pixels = read_rgba8(ctx.tiles, key, canvas)?;
    let view = ImageView::new(
        IVec2::new(canvas.x as i32, canvas.y as i32),
        canvas.width,
        canvas.height,
        &pixels,
    )?;
    let mut mask = magic_wand(&view, seed, &settings.wand())?;
    // A fill never reaches outside the selection.
    if !ctx.selection.is_none() {
        let sel = to_mask(&ctx.selection, ctx.canvas_rect())?;
        mask = combine(&mask, &sel, BooleanOp::Intersect)?;
    }
    Ok(mask)
}

/// Paint bucket: click to flood-fill a region within a tolerance.
pub struct PaintBucketTool {
    pub settings: FillSettings,
    pub content: FillContent,
    seed: Option<IVec2>,
}

impl PaintBucketTool {
    pub fn new(settings: FillSettings, content: FillContent) -> Self {
        Self {
            settings,
            content,
            seed: None,
        }
    }
}

impl Default for PaintBucketTool {
    fn default() -> Self {
        Self::new(FillSettings::default(), FillContent::Foreground)
    }
}

impl Tool for PaintBucketTool {
    fn id(&self) -> ToolId {
        match self.content {
            FillContent::Pattern => ToolId::PatternFill,
            _ => ToolId::PaintBucket,
        }
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("fill seed", event.pos)?;
        self.seed = Some(IVec2::new(
            event.pos.x.floor() as i32,
            event.pos.y.floor() as i32,
        ));
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
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(seed) = self.seed.take() else {
            return Ok(());
        };
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let mask = flood(ctx, seed, &self.settings)?;
        let Some((min, max)) = mask.bounds() else {
            return Ok(());
        };
        let rect = PixelRect::new(
            min.x as i64,
            min.y as i64,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let fg = ctx.foreground;
        let pattern = ctx.pattern.clone();
        fill_masked(
            &mut patch,
            &mask,
            &self.content,
            fg,
            pattern.as_ref(),
            self.settings.opacity,
        )?;
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.seed = None;
    }

    fn is_active(&self) -> bool {
        self.seed.is_some()
    }
}

/// Pattern fill: fills the selection (or the whole canvas) with the active
/// pattern in one click. No flood — the region is the selection.
pub struct PatternFillTool {
    pub opacity: f32,
    armed: bool,
}

impl Default for PatternFillTool {
    fn default() -> Self {
        Self {
            opacity: 1.0,
            armed: false,
        }
    }
}

impl Tool for PatternFillTool {
    fn id(&self) -> ToolId {
        ToolId::PatternFill
    }

    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        self.armed = true;
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
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        if !std::mem::take(&mut self.armed) {
            return Ok(());
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let mask = match &ctx.selection {
            Selection::None => to_mask(&Selection::None, ctx.canvas_rect())?,
            other => to_mask(other, ctx.canvas_rect())?,
        };
        let Some((min, max)) = mask.bounds() else {
            return Ok(());
        };
        let rect = PixelRect::new(
            min.x as i64,
            min.y as i64,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let fg = ctx.foreground;
        let pattern = ctx.pattern.clone();
        fill_masked(
            &mut patch,
            &mask,
            &FillContent::Pattern,
            fg,
            pattern.as_ref(),
            self.opacity,
        )?;
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.armed = false;
    }

    fn is_active(&self) -> bool {
        self.armed
    }
}
