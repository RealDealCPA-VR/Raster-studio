//! The History Brush: paint pixels back from an earlier state of the document.
//!
//! The tool paints through the same engine every stroke tool uses — a
//! [`DabEmitter`] lays dabs, a [`StrokeBuffer`] rasterises them once, and the
//! finished stroke is ONE [`Command::PaintTiles`] — but its source colour is the
//! *history source*: the layer as it was in an earlier state. At full coverage
//! the painted pixel becomes the source pixel exactly (transparency included);
//! a partial dab mixes toward it in linear premultiplied light.
//!
//! # Where the source comes from
//!
//! `editor-core` holds pixels as content-addressed tile references, and the
//! bytes behind every hash a document has ever referenced stay in the tile
//! store (undo depends on it). A history state's pixels are therefore just its
//! [`PixelStore`] — a map of hashes, cheap to keep — plus the set of layers
//! and masks it had ([`HistorySource`]), and the tool reads the source through
//! [`SourceTiles`], which resolves references from that store and bytes from
//! the live one. The owner of the document's history (the application shell)
//! is who can rebuild a state: it hands the one the user picked — the
//! [`SOURCE_KEY`] option, set from the History panel's source column or the
//! options bar; `0`, the default, is the document as opened — over at the
//! press as [`ToolContext::history_source`], and a stroke keeps the source it
//! began with.
//!
//! The brush refuses rather than paints when
//! * there is no source at all — painting the current pixels over themselves
//!   would look like a tool that silently does nothing; or
//! * the source state has no layer matching the one being painted (it was
//!   added later): its "pixels" there are nothing, and restoring nothing is an
//!   eraser. Photoshop refuses with the same reason
//!   ([`ToolError::NoSourceLayer`]).

use std::collections::HashSet;
use std::sync::Arc;

use editor_core::{Command, Document, PixelKey, PixelStore};
use glam::IVec2;
use raster::{TileCoord, TileHash};

use crate::brush::{BrushSettings, DabEmitter};
use crate::error::ToolError;
use crate::patch::ColorPatch;
use crate::stroke::StrokeBuffer;
use crate::tiles::TileAccess;
use crate::tool::{PaintTarget, PointerEvent, Tool, ToolContext, ToolId, ToolSetting};

/// The option naming the history state the brush paints from: the History
/// panel row, `0` being the document as opened.
pub const SOURCE_KEY: &str = "source";

/// One state of a document the History Brush can paint from: its pixel
/// references and the pixel targets (layers and their masks) its layer tree
/// held. Hashes only — the bytes stay in the live tile store.
#[derive(Debug, Clone, Default)]
pub struct HistorySource {
    pixels: PixelStore,
    targets: HashSet<PixelKey>,
}

impl HistorySource {
    /// The state `document` is in right now.
    pub fn of(document: &Document) -> Self {
        let layers = &document.layers;
        let mut targets = HashSet::new();
        for id in layers.iter_depth_first() {
            targets.insert(PixelKey::Layer(id));
            if let Some(mask) = layers.get(id).and_then(|l| l.mask_id()) {
                targets.insert(PixelKey::Mask(mask));
            }
        }
        Self {
            pixels: document.pixels.clone(),
            targets,
        }
    }

    /// A state made of `pixels`, whose tree held exactly `targets`.
    pub fn from_parts(pixels: PixelStore, targets: impl IntoIterator<Item = PixelKey>) -> Self {
        Self {
            pixels,
            targets: targets.into_iter().collect(),
        }
    }

    /// The state's pixel references.
    pub fn pixels(&self) -> &PixelStore {
        &self.pixels
    }

    /// Whether the state had this layer or mask at all (an empty layer counts:
    /// it owns no tiles, but restoring it to empty is what the user asked).
    pub fn contains(&self, key: PixelKey) -> bool {
        self.targets.contains(&key)
    }
}

/// A [`TileAccess`] whose references come from a history state's
/// [`PixelStore`] and whose bytes come from the live store.
///
/// Writes go to the live store (they are content-addressed, so storing never
/// changes what any reference means); the adapter is only ever read through.
pub struct SourceTiles<'a> {
    pub refs: &'a PixelStore,
    pub bytes: &'a mut dyn TileAccess,
}

impl TileAccess for SourceTiles<'_> {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.refs.tiles(key).and_then(|map| map.get(coord))
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        self.bytes.bytes(hash)
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        self.bytes.store(data)
    }
}

/// The History Brush. See the module docs.
pub struct HistoryBrushTool {
    settings: BrushSettings,
    /// The [`SOURCE_KEY`] option as last written. The shell reads the same
    /// value from the options seed to rebuild that state; the tool keeps it
    /// only so the option round-trips.
    source_state: i32,
    /// The source the running stroke began with.
    stroke_source: Option<Arc<HistorySource>>,
    emitter: Option<DabEmitter>,
}

impl Default for HistoryBrushTool {
    fn default() -> Self {
        Self {
            settings: BrushSettings {
                size: 24.0,
                hardness: 0.5,
                spacing: 0.1,
                ..BrushSettings::default()
            },
            source_state: 0,
            stroke_source: None,
            emitter: None,
        }
    }
}

impl HistoryBrushTool {
    /// The history row the brush is set to paint from (`0` = as opened).
    pub fn source_state(&self) -> i32 {
        self.source_state
    }

    fn finish(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let Some(emitter) = self.emitter.take() else {
            return Ok(());
        };
        let Some(source) = self.stroke_source.take() else {
            return Err(ToolError::NotStarted);
        };
        let source = source.as_ref();
        let dabs = emitter.dabs();
        if dabs.is_empty() {
            return Ok(());
        }
        let target = ctx.pixel_target()?;
        let key = ctx.pixel_key()?;
        let clip = ctx.paint_space_canvas.unwrap_or(ctx.canvas);
        let Some(rect) = StrokeBuffer::bounds_of(dabs, clip) else {
            return Ok(());
        };
        let buf = StrokeBuffer::rasterize(dabs, rect)?;
        let from = {
            let access = SourceTiles {
                refs: source.pixels(),
                bytes: &mut *ctx.tiles,
            };
            ColorPatch::load(&access, key, rect)?
        };
        let mut patch = ColorPatch::load(ctx.tiles, key, rect)?;
        let opacity = self.settings.opacity.clamp(0.0, 1.0);
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                let p = IVec2::new(x as i32, y as i32);
                let a = buf.get(p) * opacity * ctx.selection.coverage_at(p);
                if a <= 0.0 {
                    continue;
                }
                let dst = patch.get(p);
                let src = from.get(p);
                patch.set(
                    p,
                    [
                        dst[0] + (src[0] - dst[0]) * a,
                        dst[1] + (src[1] - dst[1]) * a,
                        dst[2] + (src[2] - dst[2]) * a,
                        dst[3] + (src[3] - dst[3]) * a,
                    ],
                );
            }
        }
        let delta = patch.commit(ctx.tiles, key)?;
        if !delta.is_empty() {
            ctx.emit(Command::PaintTiles { target, delta });
        }
        Ok(())
    }
}

impl Tool for HistoryBrushTool {
    fn id(&self) -> ToolId {
        ToolId::HistoryBrush
    }

    fn on_pointer_down(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        // Colour from history has no meaning on an 8-bit coverage mask.
        if ctx.paint_target == PaintTarget::Mask {
            return Err(ToolError::UnsupportedOnMask);
        }
        let Some(source) = ctx.history_source.clone() else {
            return Err(ToolError::NoHistorySource);
        };
        if !source.contains(ctx.pixel_key()?) {
            return Err(ToolError::NoSourceLayer);
        }
        self.stroke_source = Some(source);
        let to_layer = ctx.sample_to_layer.unwrap_or(glam::Affine2::IDENTITY);
        self.emitter = Some(DabEmitter::begin(
            self.settings,
            to_layer.transform_point2(event.pos),
            event.pressure,
        )?);
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
        self.finish(ctx)
    }

    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.emitter = None;
        self.stroke_source = None;
    }

    fn is_active(&self) -> bool {
        self.emitter.is_some()
    }

    fn set_brush(&mut self, brush: BrushSettings) {
        if self.emitter.is_none() {
            self.settings = brush;
        }
    }

    fn brush(&self) -> Option<BrushSettings> {
        Some(self.settings)
    }

    /// The registry declares only brush-shared keys for this tool; they write
    /// the brush, with the never-mid-stroke rule of [`Tool::set_brush`].
    /// Anything else — the paint blend mode included, since this brush
    /// restores rather than composites a colour — is refused.
    fn set_setting(&mut self, key: &str, setting: ToolSetting) -> Result<(), ToolError> {
        let mut brush = self.settings;
        match (key, setting) {
            ("size", ToolSetting::Float(v)) => brush.size = v,
            ("hardness", ToolSetting::Float(v)) => brush.hardness = v,
            ("spacing", ToolSetting::Float(v)) => brush.spacing = v,
            ("opacity", ToolSetting::Float(v)) => brush.opacity = v,
            ("flow", ToolSetting::Float(v)) => brush.flow = v,
            (SOURCE_KEY, ToolSetting::Int(v)) => {
                self.source_state = v.max(0);
                return Ok(());
            }
            (SOURCE_KEY, _) => {
                return Err(ToolError::OptionKindMismatch {
                    key: SOURCE_KEY.to_owned(),
                })
            }
            (k, _) if crate::registry::BRUSH_OPTION_KEYS.contains(&k) => {
                return Err(ToolError::OptionKindMismatch { key: k.to_owned() })
            }
            (k, _) => return Err(ToolError::UnknownOption { key: k.to_owned() }),
        }
        self.set_brush(brush.validated()?);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use layer_model::LayerId;
    use raster::PixelRect;

    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLUE: [u8; 4] = [0, 0, 255, 255];

    fn fill(tiles: &mut MemoryTiles, key: PixelKey, rgba: [u8; 4]) {
        for y in 0..32 {
            for x in 0..32 {
                tiles.put_pixel(key, x, y, rgba);
            }
        }
    }

    /// A state holding exactly `tiles`'s current references for `key`, whose
    /// tree held that one layer.
    fn snapshot(tiles: &MemoryTiles, key: PixelKey) -> Arc<HistorySource> {
        let mut store = PixelStore::default();
        let coord = TileCoord::new(0, 0, 0);
        let hash = tiles.tile_hash(key, coord).expect("painted");
        store.apply(
            key,
            &editor_core::TileDelta::single(editor_core::TileEdit::set(coord, hash)),
        );
        Arc::new(HistorySource::from_parts(store, [key]))
    }

    fn stroke(
        tool: &mut HistoryBrushTool,
        ctx: &mut ToolContext<'_>,
        from: (f32, f32),
        to: (f32, f32),
    ) -> Result<(), ToolError> {
        tool.on_pointer_down(ctx, PointerEvent::at(from.0, from.1))?;
        tool.on_pointer_move(ctx, PointerEvent::at(to.0, to.1))?;
        tool.on_pointer_up(ctx, PointerEvent::at(to.0, to.1))
    }

    #[test]
    fn it_paints_the_source_state_back_under_the_stroke_as_one_command() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let key = PixelKey::Layer(layer);
        fill(&mut tiles, key, RED);
        let opened = snapshot(&tiles, key);
        // The layer has since been painted blue all over.
        fill(&mut tiles, key, BLUE);

        let mut tool = HistoryBrushTool::default();
        tool.set_brush(BrushSettings {
            size: 6.0,
            hardness: 1.0,
            spacing: 0.1,
            ..BrushSettings::default()
        });
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 32, 32)).with_layer(layer);
        ctx.history_source = Some(opened);
        stroke(&mut tool, &mut ctx, (8.0, 16.0), (20.0, 16.0)).unwrap();
        let cmds = ctx.drain();
        drop(ctx);
        let [Command::PaintTiles { delta, .. }] = &cmds[..] else {
            panic!("one stroke, one command: {cmds:?}");
        };
        tiles.apply_delta(key, delta);
        assert_eq!(tiles.pixel(key, 14, 16), RED, "the stroke restored red");
        assert_eq!(
            tiles.pixel(key, 14, 2),
            BLUE,
            "outside the stroke untouched"
        );
    }

    /// The stroke keeps the source it began with: a different source offered
    /// on a later sample of the same stroke changes nothing.
    #[test]
    fn a_stroke_keeps_the_source_it_began_with() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        let key = PixelKey::Layer(layer);
        fill(&mut tiles, key, RED);
        let red = snapshot(&tiles, key);
        fill(&mut tiles, key, BLUE);
        let blue = snapshot(&tiles, key);
        fill(&mut tiles, key, [0, 255, 0, 255]);

        let mut tool = HistoryBrushTool::default();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 32, 32)).with_layer(layer);
        ctx.history_source = Some(red);
        tool.on_pointer_down(&mut ctx, PointerEvent::at(16.0, 16.0))
            .unwrap();
        ctx.history_source = Some(blue);
        tool.on_pointer_up(&mut ctx, PointerEvent::at(16.0, 16.0))
            .unwrap();
        let cmds = ctx.drain();
        drop(ctx);
        let [Command::PaintTiles { delta, .. }] = &cmds[..] else {
            panic!("one dab, one command: {cmds:?}");
        };
        tiles.apply_delta(key, delta);
        assert_eq!(tiles.pixel(key, 16, 16), RED);
    }

    #[test]
    fn with_no_source_it_refuses_and_emits_nothing() {
        let mut tiles = MemoryTiles::new();
        let layer = LayerId::new();
        fill(&mut tiles, PixelKey::Layer(layer), BLUE);
        let mut tool = HistoryBrushTool::default();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 32, 32)).with_layer(layer);
        assert!(matches!(
            tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0)),
            Err(ToolError::NoHistorySource)
        ));
        assert!(!tool.is_active());
        assert!(ctx.commands().is_empty());
    }

    /// Round-2 review defect: a layer added after the source state has no
    /// pixels there, and "restoring" it erased the layer. It must refuse.
    #[test]
    fn a_layer_the_source_state_lacks_is_refused_not_erased() {
        let mut tiles = MemoryTiles::new();
        let old = LayerId::new();
        fill(&mut tiles, PixelKey::Layer(old), RED);
        let source = snapshot(&tiles, PixelKey::Layer(old));
        let added = LayerId::new();
        fill(&mut tiles, PixelKey::Layer(added), BLUE);

        let mut tool = HistoryBrushTool::default();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 32, 32)).with_layer(added);
        ctx.history_source = Some(source);
        assert!(matches!(
            stroke(&mut tool, &mut ctx, (8.0, 8.0), (20.0, 8.0)),
            Err(ToolError::NoSourceLayer)
        ));
        assert!(!tool.is_active());
        assert!(ctx.commands().is_empty(), "nothing was erased");

        // An EMPTY layer the state did hold is a real source: painting from
        // it clears, as the user asked.
        let empty = Arc::new(HistorySource::from_parts(
            PixelStore::default(),
            [PixelKey::Layer(added)],
        ));
        ctx.history_source = Some(empty);
        stroke(&mut tool, &mut ctx, (8.0, 8.0), (20.0, 8.0)).unwrap();
        assert_eq!(ctx.commands().len(), 1);
    }

    #[test]
    fn it_refuses_the_mask_and_the_blend_mode_and_keeps_its_source_option() {
        let mut tiles = MemoryTiles::new();
        let mut tool = HistoryBrushTool::default();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 32, 32));
        ctx.history_source = Some(Arc::new(HistorySource::default()));
        ctx.paint_target = PaintTarget::Mask;
        assert!(matches!(
            tool.on_pointer_down(&mut ctx, PointerEvent::at(8.0, 8.0)),
            Err(ToolError::UnsupportedOnMask)
        ));
        assert!(tool
            .set_setting(crate::BLEND_MODE_KEY, ToolSetting::Choice(1))
            .is_err());
        tool.set_setting("size", ToolSetting::Float(50.0)).unwrap();
        assert_eq!(tool.brush().unwrap().size, 50.0);
        tool.set_setting(SOURCE_KEY, ToolSetting::Int(3)).unwrap();
        assert_eq!(tool.source_state(), 3);
        assert!(tool
            .set_setting(SOURCE_KEY, ToolSetting::Float(3.0))
            .is_err());
    }
}
