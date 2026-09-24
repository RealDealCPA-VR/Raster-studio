//! W7-F: the Mixer Brush — wet paint that picks up the colour under it.
//!
//! # The model
//!
//! The brush carries a **reservoir**: a colour and an amount of paint. Every
//! dab reads the colour already on the canvas under it (coverage-weighted, in
//! linear premultiplied light) and then:
//!
//! 1. lays down a mix of the reservoir and that canvas colour — `mix` is the
//!    canvas's share (Photopea's "Mix"), and a reservoir that has run dry
//!    lays down the canvas colour alone, i.e. smears;
//! 2. at `flow` × the dab's own coverage × `opacity`, clipped by the
//!    selection;
//! 3. then soaks up the canvas: the reservoir moves towards the canvas colour
//!    by `wet` ("Wet"), which is how a stroke dragged out of red into blue
//!    carries the red along;
//! 4. and spends paint: the amount drops by `(1 - load)` × [`LOAD_SPEND`] per
//!    dab ("Load" — a full load never runs dry).
//!
//! **Load the brush after each stroke** refills the reservoir with the
//! foreground colour when a stroke begins; **Clean the brush after each
//! stroke** empties it, so the next stroke starts with no colour of its own
//! and only mixes what it finds. With neither, the dirty reservoir carries
//! over from the last stroke, as a real brush would.
//!
//! # One step
//!
//! The dabs are collected while the button is down and simulated in order at
//! release against one [`ColorPatch`], each dab reading what the previous
//! ones left, and the patch commits as one [`Command::PaintTiles`] — one
//! history entry, one Ctrl+Z.
//!
//! # Live preview (W8-C)
//!
//! While the button is down every Move answers [`Tool::live_paint`] the way
//! the stroke tools do (W4-B): the stroke so far is simulated from the
//! reservoir the stroke began with, by the very code the release runs, into
//! a side store, and the tiles it changed go to the shell's preview lens. The
//! reservoir is put back afterwards, so the release starts from the same
//! state and commits exactly the pixels the last preview showed. The whole
//! stroke is re-simulated per answer (a wet brush's every dab depends on all
//! the dabs before it), so each answer replaces the previous one.

use std::collections::HashMap;

use editor_core::{Command, PixelKey};
use glam::IVec2;
use raster::{TileCoord, TileHash};

use crate::brush::{BrushSettings, Dab, DabEmitter};
use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::stroke::{LivePaint, LiveTile, StrokeBuffer};
use crate::tiles::TileAccess;
use crate::tool::{PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

/// How much paint a dab spends at `load = 0`.
pub const LOAD_SPEND: f32 = 0.08;

/// The Mixer Brush.
#[derive(Debug, Clone)]
pub struct MixerBrushTool {
    settings: BrushSettings,
    emitter: Option<DabEmitter>,
    /// How much of the canvas colour the reservoir soaks up per dab, `0..=1`.
    pub wet: f32,
    /// How long the reservoir lasts, `0..=1` (1 never runs dry).
    pub load: f32,
    /// The canvas's share of the colour a dab lays down, `0..=1`.
    pub mix: f32,
    /// Refill with the foreground when a stroke begins.
    pub load_after: bool,
    /// Empty the reservoir when a stroke begins (only when not refilling).
    pub clean_after: bool,
    /// The reservoir: premultiplied linear colour and paint amount.
    reservoir: Option<([f32; 4], f32)>,
    /// W8-C: how many dabs the last live preview simulated.
    previewed: usize,
}

/// W8-C: the preview's scratch store — reads fall through to the document,
/// writes stay here, so a preview never touches the document's tiles.
struct PreviewStore<'b> {
    base: &'b dyn TileAccess,
    stored: HashMap<TileHash, Vec<u8>>,
}

impl TileAccess for PreviewStore<'_> {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.base.tile_hash(key, coord)
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        match self.stored.get(&hash) {
            Some(bytes) => Some(bytes.as_slice()),
            None => self.base.bytes(hash),
        }
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        let hash = TileHash::of(&data);
        self.stored.entry(hash).or_insert(data);
        hash
    }
}

impl Default for MixerBrushTool {
    fn default() -> Self {
        Self {
            settings: BrushSettings {
                size: 30.0,
                hardness: 0.6,
                spacing: 0.1,
                ..BrushSettings::default()
            },
            emitter: None,
            wet: 0.5,
            load: 0.5,
            mix: 0.5,
            load_after: true,
            clean_after: false,
            reservoir: None,
            previewed: 0,
        }
    }
}

fn premul(c: [f32; 4]) -> [f32; 4] {
    let a = c[3].clamp(0.0, 1.0);
    [c[0] * a, c[1] * a, c[2] * a, a]
}

fn lerp4(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
        a[3] + (b[3] - a[3]) * t,
    ]
}

impl MixerBrushTool {
    /// The reservoir as it stands: premultiplied linear colour and amount.
    pub fn reservoir(&self) -> Option<([f32; 4], f32)> {
        self.reservoir
    }

    /// The reservoir a new stroke starts with.
    fn begin_reservoir(&mut self, foreground: [f32; 4]) {
        if self.load_after {
            self.reservoir = Some((premul(foreground), 1.0));
        } else if self.clean_after {
            self.reservoir = None;
        }
    }

    /// The canvas colour under a dab: the coverage-weighted mean.
    fn under(patch: &ColorPatch, dab: &Dab) -> Option<[f32; 4]> {
        let (lo, hi) = dab.bounds();
        let mut sum = [0.0f32; 4];
        let mut weight = 0.0f32;
        for y in lo.y..hi.y {
            for x in lo.x..hi.x {
                let c = dab.coverage_pixel(x, y);
                if c <= 0.0 {
                    continue;
                }
                let p = IVec2::new(x, y);
                if !patch.contains(p) {
                    continue;
                }
                let px = patch.get(p);
                for i in 0..4 {
                    sum[i] += px[i] * c;
                }
                weight += c;
            }
        }
        (weight > 0.0).then(|| sum.map(|v| v / weight))
    }

    /// Run the whole stroke against the patch, dab by dab.
    fn simulate(&mut self, ctx: &ToolContext<'_>, dabs: &[Dab], patch: &mut ColorPatch) {
        let opacity = self.settings.opacity.clamp(0.0, 1.0);
        let (wet, mix) = (self.wet.clamp(0.0, 1.0), self.mix.clamp(0.0, 1.0));
        let spend = (1.0 - self.load.clamp(0.0, 1.0)) * LOAD_SPEND;
        for dab in dabs {
            let Some(under) = Self::under(patch, dab) else {
                continue;
            };
            // What this dab lays down: reservoir and canvas mixed, with a dry
            // reservoir contributing nothing of its own.
            let laid = match self.reservoir {
                Some((color, amount)) if amount > 0.0 => {
                    lerp4(under, color, (1.0 - mix) * amount.min(1.0))
                }
                _ => under,
            };
            let (lo, hi) = dab.bounds();
            for y in lo.y..hi.y {
                for x in lo.x..hi.x {
                    let p = IVec2::new(x, y);
                    if !patch.contains(p) {
                        continue;
                    }
                    let t = dab.coverage_pixel(x, y) * dab.flow * opacity * ctx.clip_at(p);
                    if t <= 0.0 {
                        continue;
                    }
                    let now = patch.get(p);
                    patch.set(p, lerp4(now, laid, t.min(1.0)));
                }
            }
            // The brush soaks up the canvas and spends paint.
            self.reservoir = Some(match self.reservoir {
                Some((color, amount)) => (lerp4(color, under, wet), (amount - spend).max(0.0)),
                None if wet > 0.0 => (under, 0.0),
                None => continue,
            });
        }
    }

    /// W8-C: the stroke so far, simulated from the stroke's starting
    /// reservoir into a side store — the tiles the release would change, for
    /// the preview lens. The reservoir is restored, so the release replays
    /// the identical simulation.
    fn preview(&mut self, ctx: &mut ToolContext<'_>) -> Result<Option<LivePaint>, ToolError> {
        let Some(emitter) = &self.emitter else {
            return Ok(None);
        };
        let dabs = emitter.dabs().to_vec();
        if dabs.is_empty() || dabs.len() == self.previewed {
            return Ok(None);
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let clip = ctx.paint_space_canvas.unwrap_or(ctx.canvas);
        let Some(rect) = StrokeBuffer::bounds_of(&dabs, clip) else {
            return Ok(None);
        };
        let mut patch = ColorPatch::load(&*ctx.tiles, key, rect)?;
        let start = self.reservoir;
        self.simulate(ctx, &dabs, &mut patch);
        self.reservoir = start;
        let mut side = PreviewStore {
            base: &*ctx.tiles,
            stored: HashMap::new(),
        };
        let delta = patch.commit(&mut side, key)?;
        let tiles = delta
            .iter()
            .map(|edit| {
                let tile = match edit.hash {
                    None => LiveTile::Cleared,
                    Some(h) => match side.bytes(h) {
                        Some(bytes) => LiveTile::Bytes(bytes.to_vec()),
                        None => LiveTile::Committed,
                    },
                };
                (edit.coord, tile)
            })
            .collect();
        self.previewed = dabs.len();
        Ok(Some(LivePaint {
            target,
            key,
            replace: true,
            tiles,
        }))
    }

    fn finish_stroke(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        self.previewed = 0;
        let Some(emitter) = self.emitter.take() else {
            return Ok(());
        };
        let dabs = emitter.dabs().to_vec();
        if dabs.is_empty() {
            return Ok(());
        }
        ctx.require_layer_target()?;
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let clip = ctx.paint_space_canvas.unwrap_or(ctx.canvas);
        let Some(rect) = StrokeBuffer::bounds_of(&dabs, clip) else {
            return Ok(());
        };
        let mut patch = ColorPatch::load(&*ctx.tiles, key, rect)?;
        self.simulate(ctx, &dabs, &mut patch);
        let delta = patch.commit(&mut *ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
}

impl Tool for MixerBrushTool {
    fn id(&self) -> ToolId {
        ToolId::MixerBrush
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
        let pos = to_layer.transform_point2(event.pos);
        self.emitter = Some(DabEmitter::begin(self.settings, pos, event.pressure)?);
        self.previewed = 0;
        self.begin_reservoir(ctx.foreground);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(e) = &mut self.emitter {
            let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
            e.extend(to_layer.transform_point2(event.pos), event.pressure)?;
        }
        Ok(())
    }

    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        if let Some(e) = &mut self.emitter {
            let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
            e.finish(to_layer.transform_point2(event.pos), event.pressure)?;
        }
        self.finish_stroke(ctx)
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.emitter = None;
        self.previewed = 0;
    }

    fn is_active(&self) -> bool {
        self.emitter.is_some()
    }

    /// W8-C: the wet stroke so far, for the shell's preview lens.
    fn live_paint(&mut self, ctx: &mut ToolContext<'_>) -> Result<Option<LivePaint>, ToolError> {
        self.preview(ctx)
    }

    /// Size, hardness, spacing, flow and opacity travel here, as for every
    /// brush — never mid-stroke.
    fn set_brush(&mut self, brush: BrushSettings) {
        if self.emitter.is_none() {
            self.settings = brush;
        }
    }

    fn brush(&self) -> Option<BrushSettings> {
        Some(self.settings)
    }

    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let mismatch = || {
            Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            })
        };
        match (key, setting) {
            ("wet", ToolSetting::Float(v)) => {
                self.wet = crate::error::finite("wet", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("load", ToolSetting::Float(v)) => {
                self.load = crate::error::finite("load", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("mix", ToolSetting::Float(v)) => {
                self.mix = crate::error::finite("mix", v)?.clamp(0.0, 1.0);
                Ok(())
            }
            ("load_after", ToolSetting::Bool(on)) => {
                self.load_after = on;
                Ok(())
            }
            ("clean_after", ToolSetting::Bool(on)) => {
                self.clean_after = on;
                Ok(())
            }
            ("wet" | "load" | "mix" | "load_after" | "clean_after", _) => mismatch(),
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
    use editor_core::PixelKey;
    use glam::Vec2;
    use layer_model::LayerId;
    use raster::PixelRect;

    /// The dab centres of a straight stroke — for tests and scripted strokes.
    fn stroke_points(from: Vec2, to: Vec2, steps: usize) -> Vec<Vec2> {
        (0..=steps.max(1))
            .map(|i| from + (to - from) * (i as f32 / steps.max(1) as f32))
            .collect()
    }

    /// Left half red, right half blue.
    fn halves(tiles: &mut MemoryTiles, layer: LayerId) {
        for y in 0..64i64 {
            for x in 0..128i64 {
                let px = if x < 64 {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 255, 255]
                };
                tiles.put_pixel(PixelKey::Layer(layer), x, y, px);
            }
        }
    }

    fn stroke(tool: &mut MixerBrushTool, ctx: &mut ToolContext<'_>, from: Vec2, to: Vec2) {
        let pts = stroke_points(from, to, 16);
        tool.on_pointer_down(ctx, PointerEvent::at(pts[0].x, pts[0].y))
            .unwrap();
        for p in &pts[1..] {
            tool.on_pointer_move(ctx, PointerEvent::at(p.x, p.y))
                .unwrap();
        }
        let last = pts.last().unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(last.x, last.y))
            .unwrap();
    }

    #[test]
    fn a_clean_dry_brush_drags_red_into_the_blue_half_in_one_command() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        halves(&mut tiles, layer);
        let mut tool = MixerBrushTool {
            load_after: false,
            clean_after: true,
            wet: 1.0,
            ..MixerBrushTool::default()
        };
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 64)).with_layer(layer);
        stroke(
            &mut tool,
            &mut ctx,
            Vec2::new(40.0, 32.0),
            Vec2::new(90.0, 32.0),
        );
        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "one stroke is one command");
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            panic!("{commands:?}");
        };
        tiles.apply_delta(PixelKey::Layer(layer), delta);
        // Just past the border the brush has carried red into the blue.
        let px = tiles.pixel(PixelKey::Layer(layer), 70, 32);
        assert!(px[0] > 40, "no red was picked up and carried: {px:?}");
        assert!(px[2] > 40, "the blue under it vanished: {px:?}");
        // Far from the stroke the canvas is untouched.
        assert_eq!(tiles.pixel(PixelKey::Layer(layer), 70, 2), [0, 0, 255, 255]);
    }

    #[test]
    fn a_loaded_brush_lays_down_the_foreground_mixed_with_the_canvas() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        halves(&mut tiles, layer);
        let mut tool = MixerBrushTool {
            mix: 0.0,
            load: 1.0,
            wet: 0.0,
            ..MixerBrushTool::default()
        };
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 64))
            .with_layer(layer)
            .with_foreground([0.0, 1.0, 0.0, 1.0]);
        stroke(
            &mut tool,
            &mut ctx,
            Vec2::new(90.0, 32.0),
            Vec2::new(110.0, 32.0),
        );
        let commands = ctx.drain();
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            panic!("{commands:?}");
        };
        tiles.apply_delta(PixelKey::Layer(layer), delta);
        let px = tiles.pixel(PixelKey::Layer(layer), 100, 32);
        assert!(px[1] > 200, "the loaded green was not laid down: {px:?}");
    }

    /// Previewing must not change what the stroke paints: the preview runs
    /// the simulation on a copy of the reservoir and puts it back, so a
    /// stroke previewed at every Move commits exactly the pixels of the same
    /// stroke never previewed. (Without the restore each preview drained the
    /// reservoir and the release started from the drained paint.)
    #[test]
    fn previewing_a_stroke_does_not_change_what_it_commits() {
        let run = |preview: bool| {
            let mut tiles = MemoryTiles::new();
            let layer = LayerId::new();
            halves(&mut tiles, layer);
            let mut tool = MixerBrushTool {
                wet: 0.8,
                load: 0.6,
                mix: 0.5,
                ..MixerBrushTool::default()
            };
            let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 64))
                .with_layer(layer)
                .with_foreground([0.0, 1.0, 0.0, 1.0]);
            let pts = stroke_points(Vec2::new(40.0, 32.0), Vec2::new(90.0, 32.0), 16);
            tool.on_pointer_down(&mut ctx, PointerEvent::at(pts[0].x, pts[0].y))
                .unwrap();
            for p in &pts[1..] {
                tool.on_pointer_move(&mut ctx, PointerEvent::at(p.x, p.y))
                    .unwrap();
                if preview {
                    let _ = tool.live_paint(&mut ctx).unwrap();
                }
            }
            let end = pts.last().unwrap();
            tool.on_pointer_up(&mut ctx, PointerEvent::at(end.x, end.y))
                .unwrap();
            let commands = ctx.drain();
            let Command::PaintTiles { delta, .. } = &commands[0] else {
                panic!("{commands:?}");
            };
            delta
                .iter()
                .map(|e| (e.coord, tiles.bytes(e.hash.unwrap()).unwrap().to_vec()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            run(true),
            run(false),
            "previewing the stroke changed the pixels it committed"
        );
    }

    /// W8-C: every Move previews the stroke so far, the preview writes
    /// nothing, and the release commits exactly the last previewed tiles.
    #[test]
    fn the_live_preview_is_what_the_release_commits_and_writes_nothing() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        halves(&mut tiles, layer);
        let mut tool = MixerBrushTool {
            wet: 0.8,
            ..MixerBrushTool::default()
        };
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 128, 64))
            .with_layer(layer)
            .with_foreground([0.0, 1.0, 0.0, 1.0]);
        let pts = stroke_points(Vec2::new(40.0, 32.0), Vec2::new(90.0, 32.0), 16);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(pts[0].x, pts[0].y))
            .unwrap();
        let mut last = None;
        for p in &pts[1..] {
            tool.on_pointer_move(&mut ctx, PointerEvent::at(p.x, p.y))
                .unwrap();
            if let Some(live) = tool.live_paint(&mut ctx).unwrap() {
                assert!(live.replace, "a wet stroke re-answers whole");
                assert!(!live.tiles.is_empty());
                last = Some(live);
            }
            assert!(ctx.commands().is_empty(), "a preview emits nothing");
        }
        assert!(
            tool.live_paint(&mut ctx).unwrap().is_none(),
            "no new dab, no new answer"
        );
        let last = last.expect("the drag was previewed");
        let end = pts.last().unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(end.x, end.y))
            .unwrap();
        let commands = ctx.drain();
        let Command::PaintTiles { delta, .. } = &commands[0] else {
            panic!("{commands:?}");
        };
        let committed: Vec<(raster::TileCoord, Vec<u8>)> = delta
            .iter()
            .map(|e| (e.coord, tiles.bytes(e.hash.unwrap()).unwrap().to_vec()))
            .collect();
        let previewed: Vec<(raster::TileCoord, Vec<u8>)> = last
            .tiles
            .into_iter()
            .map(|(c, t)| match t {
                LiveTile::Bytes(b) => (c, b),
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(committed, previewed, "the release drifted from the preview");
    }

    #[test]
    fn options_reach_the_tool_and_refuse_the_wrong_kind() {
        let mut tool = MixerBrushTool::default();
        tool.set_setting("wet", ToolSetting::Float(0.9)).unwrap();
        tool.set_setting("clean_after", ToolSetting::Bool(true))
            .unwrap();
        assert_eq!(tool.wet, 0.9);
        assert!(tool.clean_after);
        assert!(tool.set_setting("wet", ToolSetting::Bool(true)).is_err());
        assert!(tool
            .set_setting(crate::BLEND_MODE_KEY, ToolSetting::Choice(1))
            .is_err());
    }
}
