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
use editor_core::{Command, SelectionMask};
use glam::IVec2;
use layer_model::BlendMode;
use raster::PixelRect;
use selection::{
    boolean::{combine, to_mask, BooleanOp},
    wand::{magic_wand, WandOptions},
    ImageView,
};
use serde::{Deserialize, Serialize};

use crate::brush::BrushSettings;
use crate::error::{finite, ToolError};
use crate::patch::{mask_coverage_of, read_mask_rgba8, read_rgba8, ColorPatch, CoveragePatch};
use crate::tool::{PaintTarget, Pattern, PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

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

/// W16-C: the Paint Bucket's Fill option key (a Choice over
/// [`FillSource::CHOICES`]).
pub const FILL_SOURCE_KEY: &str = "fill_source";

/// W16-C: what the Paint Bucket's Fill drop-down offers, as Photopea's does:
/// the foreground colour, or the active pattern (the one the pattern-driven
/// tools share, [`crate::tool::ToolContext::pattern`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FillSource {
    #[default]
    Foreground,
    Pattern,
}

impl FillSource {
    /// The drop-down's rows, index for index with [`FillSource::from_choice`].
    pub const CHOICES: &'static [&'static str] = &["Foreground", "Pattern"];

    /// The row at `index`; an out-of-range index clamps to the last row, as
    /// every Choice does.
    pub fn from_choice(index: usize) -> Self {
        match index {
            0 => Self::Foreground,
            _ => Self::Pattern,
        }
    }

    /// What a fill from this source lays down.
    pub fn content(self) -> FillContent {
        match self {
            Self::Foreground => FillContent::Foreground,
            Self::Pattern => FillContent::Pattern,
        }
    }
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
    /// Adopt one options-bar value by the registry's `FILL_OPTS` key. Each
    /// key lands on the field the flood ([`FillSettings::wand`]) or the
    /// composite reads.
    pub fn set(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("tolerance", ToolSetting::Float(v)) => {
                self.tolerance = finite("tolerance", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("contiguous", ToolSetting::Bool(v)) => {
                self.contiguous = v;
                Ok(())
            }
            ("antialias", ToolSetting::Bool(v)) => {
                self.antialias = v;
                Ok(())
            }
            ("opacity", ToolSetting::Float(v)) => {
                self.opacity = finite("opacity", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("sample_merged", ToolSetting::Bool(v)) => {
                self.sample_merged = v;
                Ok(())
            }
            ("tolerance" | "contiguous" | "antialias" | "opacity" | "sample_merged", _) => {
                Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                })
            }
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

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

/// The straight-alpha linear colour a fill lays down at one document point.
///
/// `pattern` must be `Some` when `content` is [`FillContent::Pattern`]; both
/// entry points below check that once, before the loop, rather than per pixel.
fn content_at(
    content: &FillContent,
    foreground: [f32; 4],
    pattern: Option<&Pattern>,
    x: i32,
    y: i32,
) -> [f32; 4] {
    match content {
        FillContent::Foreground => foreground,
        FillContent::Color(c) => *c,
        FillContent::Pattern => {
            let s = pattern
                .expect("a pattern fill checks for its pattern before it starts")
                .sample(x as i64, y as i64);
            [
                srgb8_to_linear(s[0]),
                srgb8_to_linear(s[1]),
                srgb8_to_linear(s[2]),
                s[3] as f32 / 255.0,
            ]
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
    fill_masked_with_mode(
        patch,
        mask,
        content,
        foreground,
        pattern,
        opacity,
        BlendMode::Normal,
    )
}

/// W9-L: [`fill_masked`] through a paint blend mode — what the Paint
/// Bucket's options-bar Mode composites through. `Normal` is exactly
/// [`fill_masked`].
pub fn fill_masked_with_mode(
    patch: &mut ColorPatch,
    mask: &SelectionMask,
    content: &FillContent,
    foreground: [f32; 4],
    pattern: Option<&Pattern>,
    opacity: f32,
    mode: BlendMode,
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
            let straight = content_at(content, foreground, pattern, x, y);
            let a = (straight[3] * cov * opacity).clamp(0.0, 1.0);
            if a <= 0.0 {
                continue;
            }
            let src = premultiply([straight[0], straight[1], straight[2], a]);
            let dst = patch.get(p);
            patch.set(p, crate::gradient::composite_over(mode, src, dst));
        }
    }
    Ok(())
}

/// The same fill, onto a mask's coverage plane instead of a layer's pixels.
///
/// Filling a layer mask — a bucket to reveal a whole flat region, a pattern to
/// stencil one — is routine, so it goes through [`CoveragePatch`] rather than
/// being refused. What lands is the content's luminance
/// ([`mask_coverage_of`]): white reveals, black conceals, a mid-grey pattern
/// half-reveals. The content's alpha scales *how much* of that value is
/// applied, exactly as it does on a layer, so filling with a transparent colour
/// is still a no-op.
pub fn fill_masked_coverage(
    patch: &mut CoveragePatch,
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
            if cov <= 0.0 {
                continue;
            }
            let straight = content_at(content, foreground, pattern, x, y);
            patch.blend(p, mask_coverage_of(straight), straight[3] * cov * opacity);
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
    // What the tolerance is judged against. "Sample merged" always names a
    // colour surface, so it reads RGBA whatever is being painted; otherwise the
    // read has to match the plane the fill will write, or a bucket on a mask
    // would measure tolerance against a surface of the wrong shape — which
    // `read_rgba8` reports as "no tiles at all", i.e. a flood over the whole
    // canvas.
    let pixels = if settings.sample_merged {
        read_rgba8(ctx.tiles, ctx.sample_key()?, canvas)?
    } else {
        let key = ctx.pixel_key()?;
        match ctx.paint_target {
            PaintTarget::Layer => read_rgba8(ctx.tiles, key, canvas)?,
            PaintTarget::Mask => read_mask_rgba8(ctx.tiles, key, canvas)?,
        }
    };
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
    /// W9-L: the options bar's paint Mode ([`crate::BLEND_MODE_KEY`]); the
    /// layer fill composites through it. A mask fill has no colour to blend
    /// with and reads only the opacity.
    pub mode: BlendMode,
    seed: Option<IVec2>,
}

impl PaintBucketTool {
    pub fn new(settings: FillSettings, content: FillContent) -> Self {
        Self {
            settings,
            content,
            mode: BlendMode::Normal,
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
    /// W16-C: always the Paint Bucket, whatever it fills with — the Fill
    /// source is one of its options ([`FILL_SOURCE_KEY`]), so a bucket set to
    /// Pattern is still the tool the palette selected.
    fn id(&self) -> ToolId {
        ToolId::PaintBucket
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
        let fg = ctx.foreground;
        let pattern = ctx.pattern.clone();
        let opacity = self.settings.opacity;
        let delta = match ctx.paint_target {
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
                fill_masked_with_mode(
                    &mut patch,
                    &mask,
                    &self.content,
                    fg,
                    pattern.as_ref(),
                    opacity,
                    self.mode,
                )?;
                patch.commit(ctx.tiles, key)?
            }
            PaintTarget::Mask => {
                let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
                fill_masked_coverage(
                    &mut patch,
                    &mask,
                    &self.content,
                    fg,
                    pattern.as_ref(),
                    opacity,
                )?;
                patch.commit(ctx.tiles, key)?
            }
        };
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.seed = None;
    }

    /// The registry's `FILL_OPTS`, straight onto [`PaintBucketTool::settings`],
    /// and (W9-L) the options bar's paint Mode ([`crate::BLEND_MODE_KEY`], a
    /// Choice indexing [`BlendMode::ALL`], clamped like every Choice).
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        // W16-C: Photopea's Fill drop-down, Foreground or Pattern.
        if key == FILL_SOURCE_KEY {
            return match setting {
                ToolSetting::Choice(i) => {
                    self.content = FillSource::from_choice(i).content();
                    Ok(())
                }
                _ => Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                }),
            };
        }
        if key == crate::BLEND_MODE_KEY {
            return match setting {
                ToolSetting::Choice(i) => {
                    let last = BlendMode::ALL.len() - 1;
                    self.mode =
                        crate::blend_mode_from_choice(i.min(last)).unwrap_or(BlendMode::Normal);
                    Ok(())
                }
                _ => Err(ToolError::OptionKindMismatch {
                    key: key.to_owned(),
                }),
            };
        }
        self.settings.set(key, setting)
    }

    /// `opacity` is one of the brush-shared keys, so the shell carries it in
    /// the brush it hands every tool at pointer-down rather than through
    /// `set_setting`. Adopting it here is what makes the options bar's
    /// Opacity reach the fill in the running app.
    fn set_brush(&mut self, brush: BrushSettings) {
        if brush.opacity.is_finite() {
            self.settings.opacity = brush.opacity.clamp(0.0, 1.0);
        }
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
        // No flood: the region is the selection, and `Selection::None` means
        // the whole canvas.
        let mask = to_mask(&ctx.selection, ctx.canvas_rect())?;
        let Some((min, max)) = mask.bounds() else {
            return Ok(());
        };
        let rect = PixelRect::new(
            min.x as i64,
            min.y as i64,
            (max.x - min.x) as u32,
            (max.y - min.y) as u32,
        );
        let fg = ctx.foreground;
        let pattern = ctx.pattern.clone();
        let opacity = self.opacity;
        let delta = match ctx.paint_target {
            PaintTarget::Layer => {
                let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
                fill_masked(
                    &mut patch,
                    &mask,
                    &FillContent::Pattern,
                    fg,
                    pattern.as_ref(),
                    opacity,
                )?;
                patch.commit(ctx.tiles, key)?
            }
            PaintTarget::Mask => {
                let mut patch = CoveragePatch::load(ctx.tiles, key, rect)?;
                fill_masked_coverage(
                    &mut patch,
                    &mask,
                    &FillContent::Pattern,
                    fg,
                    pattern.as_ref(),
                    opacity,
                )?;
                patch.commit(ctx.tiles, key)?
            }
        };
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.armed = false;
    }

    /// The registry declares one option for Pattern Fill: `opacity`.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        match (key, setting) {
            ("opacity", ToolSetting::Float(v)) => {
                self.opacity = finite("opacity", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("opacity", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

    /// See [`PaintBucketTool::set_brush`]: the shell delivers `opacity`
    /// through the brush, so it is adopted from there too.
    fn set_brush(&mut self, brush: BrushSettings) {
        if brush.opacity.is_finite() {
            self.opacity = brush.opacity.clamp(0.0, 1.0);
        }
    }

    fn is_active(&self) -> bool {
        self.armed
    }
}

#[cfg(test)]
mod option_tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use editor_core::PixelKey;
    use layer_model::LayerId;

    const W: u32 = 64;
    const H: u32 = 64;

    fn paint(tiles: &mut MemoryTiles, key: PixelKey, color: impl Fn(usize) -> u8) {
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..H as usize {
            for x in 0..W as usize {
                let g = color(x);
                let i = (y * ts + x) * 4;
                data[i..i + 4].copy_from_slice(&[g, g, g, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
    }

    fn bands(x: usize) -> u8 {
        if (16..32).contains(&x) {
            220
        } else {
            40
        }
    }

    fn covered(mask: &SelectionMask) -> usize {
        mask.coverage().iter().filter(|&&v| v > 0).count()
    }

    fn partial(mask: &SelectionMask) -> usize {
        mask.coverage()
            .iter()
            .filter(|&&v| v > 0 && v < 255)
            .count()
    }

    #[test]
    fn bucket_tolerance_contiguous_sample_merged_and_antialias_reach_the_flood() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let composite = LayerId::new();
        paint(&mut tiles, PixelKey::Layer(layer), bands);
        paint(&mut tiles, PixelKey::Layer(composite), |_| 40);
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H)).with_layer(layer);
        ctx.sample_from = Some(PixelKey::Layer(composite));
        let seed = IVec2::new(4, 8);
        let mut tool = PaintBucketTool::default();
        tool.set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();

        tool.set_setting("tolerance", ToolSetting::Float(0.0))
            .unwrap();
        assert_eq!(
            covered(&flood(&ctx, seed, &tool.settings).unwrap()),
            16 * H as usize
        );
        tool.set_setting("tolerance", ToolSetting::Float(1.0))
            .unwrap();
        assert_eq!(
            covered(&flood(&ctx, seed, &tool.settings).unwrap()),
            (W * H) as usize
        );

        tool.set_setting("tolerance", ToolSetting::Float(0.0))
            .unwrap();
        tool.set_setting("contiguous", ToolSetting::Bool(false))
            .unwrap();
        assert_eq!(
            covered(&flood(&ctx, seed, &tool.settings).unwrap()),
            48 * H as usize
        );

        tool.set_setting("contiguous", ToolSetting::Bool(true))
            .unwrap();
        tool.set_setting("sample_merged", ToolSetting::Bool(true))
            .unwrap();
        assert_eq!(
            covered(&flood(&ctx, seed, &tool.settings).unwrap()),
            (W * H) as usize,
            "judged against the flat composite"
        );

        // Anti-alias: a tolerance straddling the band difference ramps the
        // rim when on and snaps it when off.
        tool.set_setting("sample_merged", ToolSetting::Bool(false))
            .unwrap();
        tool.set_setting("tolerance", ToolSetting::Float(0.75))
            .unwrap();
        tool.set_setting("antialias", ToolSetting::Bool(true))
            .unwrap();
        let aa = flood(&ctx, seed, &tool.settings).unwrap();
        tool.set_setting("antialias", ToolSetting::Bool(false))
            .unwrap();
        let hard = flood(&ctx, seed, &tool.settings).unwrap();
        assert!(partial(&aa) > 0);
        assert_eq!(partial(&hard), 0);

        assert!(matches!(
            tool.set_setting("tolerance", ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
        assert!(matches!(
            tool.set_setting("radius", ToolSetting::Float(1.0)),
            Err(ToolError::UnknownOption { .. })
        ));
    }

    #[test]
    fn opacity_reaches_the_fill_through_set_setting_and_through_the_brush() {
        // An empty layer: the flood covers the whole canvas, and the fill
        // alpha is the opacity.
        let alpha_after = |opacity_via: &dyn Fn(&mut PaintBucketTool)| {
            let mut tiles = MemoryTiles::new();
            let layer = LayerId::new();
            let key = PixelKey::Layer(layer);
            let mut tool = PaintBucketTool::default();
            opacity_via(&mut tool);
            let delta = {
                let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, W, H))
                    .with_layer(layer)
                    .with_foreground([1.0, 0.0, 0.0, 1.0]);
                tool.on_pointer_down(&mut ctx, PointerEvent::at(4.0, 4.0))
                    .unwrap();
                tool.on_pointer_up(&mut ctx, PointerEvent::at(4.0, 4.0))
                    .unwrap();
                match ctx.drain().pop() {
                    Some(Command::PaintTiles { delta, .. }) => delta,
                    other => panic!("expected a paint: {other:?}"),
                }
            };
            tiles.apply_delta(key, &delta);
            tiles.pixel(key, 10, 10)[3]
        };
        assert_eq!(alpha_after(&|_| {}), 255);
        let half = alpha_after(&|t| {
            t.set_setting("opacity", ToolSetting::Float(0.5)).unwrap();
        });
        assert!(
            (126..=129).contains(&half),
            "set_setting opacity 0.5 -> alpha {half}"
        );
        // The shell carries `opacity` in the brush it hands the tool at
        // pointer-down, so the brush route must land in the same place.
        let quarter = alpha_after(&|t| {
            t.set_brush(BrushSettings {
                opacity: 0.25,
                ..BrushSettings::default()
            })
        });
        assert!(
            (62..=65).contains(&quarter),
            "brush opacity 0.25 -> alpha {quarter}"
        );

        // Pattern Fill: the same two routes onto its one option.
        let mut pf = PatternFillTool::default();
        pf.set_setting("opacity", ToolSetting::Float(0.3)).unwrap();
        assert_eq!(pf.opacity, 0.3);
        pf.set_brush(BrushSettings {
            opacity: 0.6,
            ..BrushSettings::default()
        });
        assert_eq!(pf.opacity, 0.6);
        assert!(matches!(
            pf.set_setting("tolerance", ToolSetting::Float(0.5)),
            Err(ToolError::UnknownOption { .. })
        ));
    }
}

/// W9-L: the Paint Bucket composites its fill through the options bar's
/// Mode, and its Opacity still scales it. Driven as the shell drives it.
#[cfg(test)]
mod w9l_tests {
    use super::*;

    use crate::registry;
    use crate::tiles::MemoryTiles;
    use editor_core::PixelKey;
    use layer_model::LayerId;

    const SIDE: u32 = 64;

    /// A 64x64 opaque mid-grey layer (sRGB 128), in one tile.
    fn grey_layer(tiles: &mut MemoryTiles, key: PixelKey) {
        let ts = raster::TILE_SIZE as usize;
        let mut data = vec![0u8; ts * ts * 4];
        for y in 0..SIDE as usize {
            for x in 0..SIDE as usize {
                let i = (y * ts + x) * 4;
                data[i..i + 4].copy_from_slice(&[128, 128, 128, 255]);
            }
        }
        tiles.put(key, raster::TileCoord::new(0, 0, 0), data);
    }

    fn mode_index(mode: BlendMode) -> usize {
        BlendMode::ALL.iter().position(|m| *m == mode).unwrap()
    }

    fn white_fill_over_grey(mode: Option<BlendMode>, opacity: f32) -> [u8; 4] {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let key = PixelKey::Layer(layer);
        grey_layer(&mut tiles, key);
        let mut tool = registry::make(ToolId::PaintBucket);
        tool.set_setting("opacity", ToolSetting::Float(opacity))
            .unwrap();
        if let Some(mode) = mode {
            tool.set_setting(crate::BLEND_MODE_KEY, ToolSetting::Choice(mode_index(mode)))
                .expect("the Paint Bucket answers the Mode key");
        }
        let delta = {
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, SIDE, SIDE))
                .with_layer(layer)
                .with_foreground([1.0, 1.0, 1.0, 1.0]);
            tool.on_pointer_down(&mut ctx, PointerEvent::at(4.0, 4.0))
                .unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(4.0, 4.0))
                .unwrap();
            // An identity blend changes nothing and emits nothing.
            match ctx.drain().pop() {
                Some(Command::PaintTiles { delta, .. }) => Some(delta),
                None => None,
                other => panic!("expected a paint: {other:?}"),
            }
        };
        if let Some(delta) = delta {
            tiles.apply_delta(key, &delta);
        }
        tiles.pixel(key, 20, 20)
    }

    #[test]
    fn the_paint_bucket_composites_through_the_mode_it_is_given() {
        assert!(crate::composites_strokes(ToolId::PaintBucket));
        assert_eq!(white_fill_over_grey(None, 1.0), [255, 255, 255, 255]);
        let multiply = white_fill_over_grey(Some(BlendMode::Multiply), 1.0);
        for c in &multiply[..3] {
            assert!((i32::from(*c) - 128).abs() <= 1, "Multiply: {multiply:?}");
        }
        let diff = white_fill_over_grey(Some(BlendMode::Difference), 1.0);
        // In linear light: |1 - lin(128)| = 0.784, which encodes as 229.
        assert!(
            (i32::from(diff[0]) - 229).abs() <= 1,
            "Difference with white inverts the grey's light: {diff:?}"
        );
        // Opacity still scales the blended fill: half a Difference lands
        // between the grey and the full Difference.
        let half = white_fill_over_grey(Some(BlendMode::Difference), 0.5);
        assert!(
            half[0] > 128 && half[0] < diff[0],
            "half {half:?} vs full {diff:?}"
        );
        assert!(matches!(
            registry::make(ToolId::PaintBucket)
                .set_setting(crate::BLEND_MODE_KEY, ToolSetting::Bool(true)),
            Err(ToolError::OptionKindMismatch { .. })
        ));
    }
}
