//! Bits per channel of an open document: Image ▸ Mode ▸ 8/16 Bits/Channel,
//! the 16-bit New Document, and the apply boundary that keeps a 16-bit
//! document's tiles 16-bit when a tool hands it 8-bit output.
//!
//! A child module of `doc` (declared there with `#[path]`) so it reaches the
//! [`OpenDocument`] internals the conversion rewrites without widening their
//! visibility.
//!
//! # What a depth is here
//!
//! `DocumentMeta::bit_depth` is the document's *working* depth, and the tiles
//! follow it: RGBA8 tiles for 8, RGBA16 tiles ([`raster::rgba16_to_tile_bytes`]
//! layout) for 16. The compositor reads either by length, so a tile a
//! command wrote at the other depth still composites correctly — the boundary
//! below only makes sure that the common writer, a tool's `PaintTiles`, lands
//! at the document's depth.

use editor_core::pixels::{PixelTarget, TileEdit};
use editor_core::{Command, Document};

use compositor::{MemoryTileSource, TileSource};

use super::OpenDocument;

impl OpenDocument {
    /// Image ▸ Mode ▸ 8/16 Bits/Channel: convert every raster layer tile to
    /// `to_bits` (8 or 16) as ONE undoable step.
    ///
    /// 8 → 16 widens each sample by bit repetition, losslessly; 16 → 8 rounds
    /// to the nearest code, with a 4×4 ordered dither on the colour channels
    /// when `dither` is set. The depth itself rides the same
    /// [`Command::Transaction`] as a [`Command::SetMetaBitDepth`], so one undo
    /// returns both the depth and the exact tiles that were there. Mask tiles
    /// are single-channel coverage and keep their 8 bits.
    pub fn convert_depth(&mut self, to_bits: u8, dither: bool) -> Result<String, String> {
        let command = self.depth_conversion(to_bits, dither)?;
        self.apply(command).map_err(|e| e.to_string())?;
        Ok(format!("Converted to {to_bits} bits per channel"))
    }

    /// The [`Command::Transaction`] [`OpenDocument::convert_depth`] applies,
    /// built but not applied — the converted bytes are already in the tile
    /// store, so the menu can hand the command to
    /// [`crate::editor::Editor::apply_command`] (the route every edit takes).
    pub fn depth_conversion(&mut self, to_bits: u8, dither: bool) -> Result<Command, String> {
        if !matches!(to_bits, 8 | 16) {
            return Err(format!("{to_bits} bits per channel is not available"));
        }
        let from = self.document.meta.bit_depth;
        if from == to_bits {
            return Err("The document is already at that depth".to_string());
        }
        let mut commands = vec![Command::SetMetaBitDepth { from, to: to_bits }];
        for layer_id in self.document.layers.iter_depth_first() {
            let Some(map) = self.document.layer_tiles(layer_id) else {
                continue;
            };
            let converted: Vec<_> = map
                .iter()
                .filter_map(|(coord, hash)| {
                    let bytes = self.tiles.tile(hash)?;
                    // A tile already at the target depth (or not a colour
                    // tile at all) is left as it is.
                    let out = if to_bits == 16 {
                        raster::widen_rgba8_tile(bytes)
                    } else {
                        raster::narrow_rgba16_tile(bytes, dither)
                    }?;
                    Some((coord, out))
                })
                .collect();
            if converted.is_empty() {
                continue;
            }
            let edits: Vec<TileEdit> = converted
                .into_iter()
                .map(|(coord, bytes)| TileEdit::set(coord, self.tiles.insert_bytes(bytes)))
                .collect();
            commands.push(
                Command::paint_tiles(PixelTarget::Layer(layer_id), edits)
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(Command::Transaction {
            label: format!("Convert to {to_bits} Bits/Channel"),
            commands,
        })
    }

    /// Record the depth a brand-new document was created at (the New
    /// Document dialog's answer). Initial state, not an undo step: the
    /// document has no history yet. A solid 16-bit background already arrives
    /// in RGBA16 tiles; a transparent one has no tiles, so this is the only
    /// place its depth is kept.
    pub fn set_initial_bit_depth(&mut self, depth: raster::BitDepth) {
        self.document.meta.bit_depth = match depth {
            raster::BitDepth::Eight => 8,
            raster::BitDepth::Sixteen => 16,
        };
    }
}

/// The read half of the tool boundary: a tool reads RGBA8 tiles, so a 16-bit
/// tile is handed to it rounded to 8 bits.
///
/// Built once per tool context by scanning the document's references for
/// 16-bit tiles (a length check per tile, no pixel work); a tile is narrowed
/// only the first time a tool actually reads it, and at most once per
/// context. The tool's 8-bit output comes back through
/// [`fit_to_document_depth`], which restores the 16-bit value of every pixel
/// the tool did not change — so read-narrow-then-write-widen is lossless
/// outside the pixels a tool really painted.
#[derive(Debug, Default)]
pub struct NarrowedReads {
    cells: std::collections::HashMap<raster::TileHash, std::cell::OnceCell<Vec<u8>>>,
}

impl NarrowedReads {
    /// Find every RGBA16 tile `refs` names in `bytes`.
    pub fn scan(refs: &editor_core::PixelStore, bytes: &MemoryTileSource) -> Self {
        let mut cells = std::collections::HashMap::new();
        for key in refs.keys() {
            let Some(map) = refs.tiles(key) else {
                continue;
            };
            for (_, hash) in map.iter() {
                if bytes
                    .tile(hash)
                    .is_some_and(|b| b.len() == raster::depth::RGBA16_TILE_BYTES)
                {
                    cells.insert(hash, std::cell::OnceCell::new());
                }
            }
        }
        Self { cells }
    }

    /// The bytes a tool sees for `hash`: the stored bytes, or their 8-bit
    /// rounding for a 16-bit tile.
    pub fn read<'a>(
        &'a self,
        hash: raster::TileHash,
        bytes: &'a MemoryTileSource,
    ) -> Option<&'a [u8]> {
        let stored = bytes.tile(hash)?;
        match self.cells.get(&hash) {
            Some(cell) => {
                let narrowed = cell.get_or_init(|| {
                    raster::narrow_rgba16_tile(stored, false).unwrap_or_else(|| stored.to_vec())
                });
                Some(narrowed.as_slice())
            }
            None => Some(stored),
        }
    }
}

/// The apply boundary: in a 16-bit document, an RGBA8 tile a layer
/// `PaintTiles` names is replaced by its 16-bit equivalent before the command
/// reaches history.
///
/// Pixels the tool left unchanged keep the exact 16-bit value that was there
/// ([`raster::widen_rgba8_over`]); changed ones are widened. A depth
/// conversion itself is passed through untouched — its 8-bit tiles are the
/// point of a 16 → 8 conversion — and so is every command in an 8-bit
/// document.
pub(super) fn fit_to_document_depth(
    document: &Document,
    tiles: &mut MemoryTileSource,
    command: Command,
) -> Command {
    if document.meta.bit_depth != 16 {
        return command;
    }
    fit(document, tiles, command)
}

fn fit(document: &Document, tiles: &mut MemoryTileSource, command: Command) -> Command {
    match command {
        Command::PaintTiles {
            target: PixelTarget::Layer(layer),
            delta,
        } => {
            let mut changed = false;
            let edits: Vec<TileEdit> = delta
                .iter()
                .map(|edit| {
                    let Some(hash) = edit.hash else {
                        return *edit;
                    };
                    let old = document
                        .layer_tiles(layer)
                        .and_then(|m| m.get(edit.coord))
                        .and_then(|h| tiles.tile(h));
                    let Some(wide) = tiles
                        .tile(hash)
                        .and_then(|new8| raster::widen_rgba8_over(new8, old))
                    else {
                        return *edit;
                    };
                    changed = true;
                    TileEdit::set(edit.coord, tiles.insert_bytes(wide))
                })
                .collect();
            let target = PixelTarget::Layer(layer);
            if !changed {
                return Command::PaintTiles { target, delta };
            }
            // Same coordinates as the delta it replaces, so it cannot fail;
            // fall back to the original rather than lose the edit if it did.
            Command::paint_tiles(target, edits).unwrap_or(Command::PaintTiles { target, delta })
        }
        Command::Transaction { label, commands }
            if !commands
                .iter()
                .any(|c| matches!(c, Command::SetMetaBitDepth { .. })) =>
        {
            Command::Transaction {
                label,
                commands: commands
                    .into_iter()
                    .map(|c| fit(document, tiles, c))
                    .collect(),
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use glam::Vec2;
    use raster::{PixelRect, TileCoord, TILE_SIZE};
    use ui::canvas::{PointerInput, PointerPhase};

    use crate::dialogs::ScriptedDialogs;
    use crate::editor::Editor;
    use crate::import::BlankBackground;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use crate::tool_input::ToolPointer;

    use super::*;

    const RGBA8: usize = raster::depth::RGBA8_TILE_BYTES;
    const RGBA16: usize = raster::depth::RGBA16_TILE_BYTES;

    fn editor(dir: &std::path::Path) -> Editor {
        Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        )
    }

    /// Every layer tile of the active document, as (coord, byte length).
    fn tile_lengths(ed: &Editor) -> Vec<(TileCoord, usize)> {
        let doc = ed.active().unwrap();
        let mut out = Vec::new();
        for id in doc.document.layers.iter_depth_first() {
            if let Some(map) = doc.document.layer_tiles(id) {
                for (c, h) in map.iter() {
                    out.push((c, doc.tiles.tile(h).unwrap().len()));
                }
            }
        }
        out
    }

    /// The bytes of every layer tile, in a stable order.
    fn tile_bytes(ed: &Editor) -> Vec<(TileCoord, Vec<u8>)> {
        let doc = ed.active().unwrap();
        let mut out = Vec::new();
        for id in doc.document.layers.iter_depth_first() {
            if let Some(map) = doc.document.layer_tiles(id) {
                for (c, h) in map.iter() {
                    out.push((c, doc.tiles.tile(h).unwrap().to_vec()));
                }
            }
        }
        out
    }

    #[test]
    fn a_sixteen_bit_new_document_is_built_from_rgba16_tiles_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            300,
            40,
            "deep",
            BlankBackground::Solid {
                rgba8: [10, 128, 255, 255],
                depth: raster::BitDepth::Sixteen,
            },
        )
        .unwrap();
        ed.active_mut()
            .unwrap()
            .set_initial_bit_depth(raster::BitDepth::Sixteen);
        let lengths = tile_lengths(&ed);
        assert_eq!(lengths.len(), 2, "300 px wide is two tiles: {lengths:?}");
        assert!(lengths.iter().all(|(_, l)| *l == RGBA16), "{lengths:?}");
        let doc = ed.active_mut().unwrap();
        assert_eq!(doc.document.meta.bit_depth, 16);
        assert!(doc.is_sixteen_bit());
        // The composite reads those tiles at their depth.
        let deep = doc.composite_rgba16(PixelRect::new(0, 0, 1, 1)).unwrap();
        assert_eq!(deep[..3], [10 * 257, 128 * 257, 65_535]);
    }

    #[test]
    fn converting_to_eight_bits_then_undoing_restores_the_sixteen_bit_tiles_byte_exact() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            64,
            64,
            "deep",
            BlankBackground::Solid {
                rgba8: [0, 0, 0, 255],
                depth: raster::BitDepth::Sixteen,
            },
        )
        .unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_initial_bit_depth(raster::BitDepth::Sixteen);
        // Put genuinely 16-bit content in: codes that are not on the 8-bit
        // grid, so a lossy round trip would show.
        let layer = doc.document.active_layer().unwrap();
        let samples: Vec<u16> = (0..TILE_SIZE as usize * TILE_SIZE as usize * 4)
            .map(|i| (i * 7919 % 65_536) as u16)
            .collect();
        let hash = doc
            .tiles
            .insert_bytes(raster::rgba16_to_tile_bytes(&samples));
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                [TileEdit::set(TileCoord::new(0, 0, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();
        let before = tile_bytes(&ed);
        assert!(before.iter().all(|(_, b)| b.len() == RGBA16));

        let doc = ed.active_mut().unwrap();
        let depth_before = doc.history_depth();
        doc.convert_depth(8, false).unwrap();
        assert_eq!(doc.history_depth(), depth_before + 1, "one step");
        assert_eq!(doc.document.meta.bit_depth, 8);
        let narrowed = tile_bytes(&ed);
        assert!(
            narrowed.iter().all(|(_, b)| b.len() == RGBA8),
            "8-bit tiles"
        );
        assert_eq!(
            narrowed[0].1[..4],
            [
                raster::depth::narrow_sample(samples[0]),
                raster::depth::narrow_sample(samples[1]),
                raster::depth::narrow_sample(samples[2]),
                raster::depth::narrow_sample(samples[3]),
            ]
        );

        let doc = ed.active_mut().unwrap();
        assert!(doc.undo().unwrap());
        assert_eq!(doc.document.meta.bit_depth, 16);
        assert_eq!(tile_bytes(&ed), before, "undo is byte-exact");
        let doc = ed.active_mut().unwrap();
        assert!(doc.redo().unwrap());
        assert_eq!(doc.document.meta.bit_depth, 8);
        assert_eq!(tile_bytes(&ed), narrowed);
    }

    #[test]
    fn eight_to_sixteen_to_eight_is_lossless_and_the_same_depth_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            64,
            64,
            "flat",
            BlankBackground::Solid {
                rgba8: [12, 34, 56, 200],
                depth: raster::BitDepth::Eight,
            },
        )
        .unwrap();
        let before = tile_bytes(&ed);
        let doc = ed.active_mut().unwrap();
        assert_eq!(
            doc.convert_depth(8, false).unwrap_err(),
            "The document is already at that depth"
        );
        doc.convert_depth(16, false).unwrap();
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
        ed.active_mut().unwrap().convert_depth(8, true).unwrap();
        assert_eq!(tile_bytes(&ed), before, "8 -> 16 -> 8 is the identity");
    }

    /// Image > Mode > 16 Bits on an 8-bit document, then a real brush drag
    /// through the pointer route, then File > Export to PNG: the stroke lands
    /// as RGBA16 tiles and the PNG carries sixteen bits per channel that
    /// match the document's own 16-bit composite exactly.
    #[test]
    fn mode_sixteen_then_a_brush_stroke_then_a_png_export_round_trips_at_sixteen_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        // 300 x 64, transparent; one 8-bit tile of content on the right so the
        // conversion has something to widen. The stroke runs through the
        // left tile, which holds no pixels yet.
        ed.new_document_with(300, 64, "paint", BlankBackground::Transparent)
            .unwrap();
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let right = doc.tiles.insert_bytes(vec![77u8; RGBA8]);
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                [TileEdit::set(TileCoord::new(1, 0, 0), right)],
            )
            .unwrap(),
        )
        .unwrap();

        // Image > Mode > 16 Bits/Channel, through the menu bridge.
        crate::menu_bridge::perform(
            ui::menu::MenuAction::SetBitDepth(ui::menu::ChannelDepth::Sixteen),
            &mut ed,
        )
        .unwrap();
        assert_eq!(ed.active().unwrap().document.meta.bit_depth, 16);
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
        assert_eq!(
            raster::tile_bytes_to_rgba16(&tile_bytes(&ed)[0].1)[..4],
            [77 * 257; 4],
            "8 -> 16 widens by bit repetition"
        );
        // Now give the right tile genuinely deep content (codes off the
        // 8-bit grid) so the stroke below has something to lose.
        let deep: Vec<u16> = (0..RGBA16 / 2)
            .map(|i| 40_000 + (i * 7919 % 20_000) as u16)
            .collect();
        {
            let doc = ed.active_mut().unwrap();
            let h = doc.tiles.insert_bytes(raster::rgba16_to_tile_bytes(&deep));
            doc.apply(
                Command::paint_tiles(
                    PixelTarget::Layer(layer),
                    [TileEdit::set(TileCoord::new(1, 0, 0), h)],
                )
                .unwrap(),
            )
            .unwrap();
        }

        // A brush drag, the pointer route the canvas uses.
        let viewport = Vec2::new(600.0, 200.0);
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(viewport);
            doc.camera.zoom = 1.0;
            doc.camera.center = Vec2::new(150.0, 32.0);
        }
        let screen = |x: f32, y: f32| viewport * 0.5 + Vec2::new(x - 150.0, y - 32.0);
        ed.set_tool(tools::ToolId::Brush);
        ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
        let mut pointer = ToolPointer::new();
        let before_steps = ed.active().unwrap().history_depth();
        // From the empty left tile into the deep right one.
        let pts = [(40.0, 32.0), (150.0, 32.0), (270.0, 32.0)];
        for (i, (x, y)) in pts.iter().enumerate() {
            let phase = if i == 0 {
                PointerPhase::Down
            } else {
                PointerPhase::Move
            };
            pointer.handle(&mut ed, PointerInput::at(phase, screen(*x, *y)), false, &[]);
        }
        pointer.handle(
            &mut ed,
            PointerInput::at(PointerPhase::Up, screen(270.0, 32.0)),
            false,
            &[],
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            before_steps + 1,
            "the stroke is one step"
        );
        let lengths = tile_lengths(&ed);
        assert!(
            lengths.contains(&(TileCoord::new(0, 0, 0), RGBA16)),
            "the stroke landed as a 16-bit tile: {lengths:?}"
        );
        assert!(lengths.iter().all(|(_, l)| *l == RGBA16), "{lengths:?}");
        // The painted-over deep tile kept every pixel the brush did not
        // touch at its exact 16-bit code; the ones it touched are red.
        let right = tile_bytes(&ed)
            .into_iter()
            .find(|(c, _)| *c == TileCoord::new(1, 0, 0))
            .map(|(_, b)| raster::tile_bytes_to_rgba16(&b))
            .unwrap();
        let ts = TILE_SIZE as usize;
        let px16 = |x: usize, y: usize| &right[(y * ts + x) * 4..(y * ts + x) * 4 + 4];
        let deep_px = |x: usize, y: usize| &deep[(y * ts + x) * 4..(y * ts + x) * 4 + 4];
        assert_eq!(px16(24, 5), deep_px(24, 5), "untouched keeps 16 bits");
        assert_eq!(px16(40, 60), deep_px(40, 60), "untouched keeps 16 bits");
        assert_eq!(px16(10, 32), [65_535, 0, 0, 65_535], "the stroke painted");

        // Export to PNG and read it back.
        let out = dir.path().join("deep.png");
        let doc = ed.active_mut().unwrap();
        let expected = doc.composite_rgba16(PixelRect::new(0, 0, 300, 64)).unwrap();
        doc.export_to(&out).unwrap();
        let back = raster::decode_surface_path(&out, raster::ImportLimits::default()).unwrap();
        let raster::SurfacePixels::Rgba16(px) = back.pixels else {
            panic!("a 16-bit document exported to PNG at {:?}", back.format());
        };
        assert_eq!(px, expected, "the PNG carries the 16-bit composite");
        // The painted pixels are red and opaque in the file too.
        let at = |x: usize, y: usize| &px[(y * 300 + x) * 4..(y * 300 + x) * 4 + 4];
        assert_eq!(at(60, 32), [65_535, 0, 0, 65_535]);
        assert_eq!(at(266, 32), [65_535, 0, 0, 65_535]);
    }

    #[test]
    fn the_boundary_keeps_untouched_deep_pixels_and_leaves_eight_bit_documents_alone() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(64, 64, "b", BlankBackground::Transparent)
            .unwrap();
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let c = TileCoord::new(0, 0, 0);
        // 8-bit document: an 8-bit write stays 8-bit.
        let h8 = doc.tiles.insert_bytes(vec![5u8; RGBA8]);
        let cmd = Command::paint_tiles(PixelTarget::Layer(layer), [TileEdit::set(c, h8)]).unwrap();
        let same = fit_to_document_depth(&doc.document, &mut doc.tiles, cmd.clone());
        assert_eq!(same, cmd);

        // 16-bit document with deep content: an 8-bit write that changes one
        // pixel keeps every other pixel's 16-bit value.
        doc.set_initial_bit_depth(raster::BitDepth::Sixteen);
        let samples: Vec<u16> = (0..RGBA16 / 2)
            .map(|i| (i * 7919 % 65_536) as u16)
            .collect();
        let deep = doc
            .tiles
            .insert_bytes(raster::rgba16_to_tile_bytes(&samples));
        doc.apply(
            Command::paint_tiles(PixelTarget::Layer(layer), [TileEdit::set(c, deep)]).unwrap(),
        )
        .unwrap();
        let mut new8 = raster::narrow_rgba16_tile(doc.tiles.tile(deep).unwrap(), false).unwrap();
        new8[..4].copy_from_slice(&[255, 0, 0, 255]);
        let h = doc.tiles.insert_bytes(new8);
        doc.apply(Command::paint_tiles(PixelTarget::Layer(layer), [TileEdit::set(c, h)]).unwrap())
            .unwrap();
        let now = doc.document.layer_tiles(layer).unwrap().get(c).unwrap();
        let bytes = raster::tile_bytes_to_rgba16(doc.tiles.tile(now).unwrap());
        assert_eq!(bytes[..4], [65_535, 0, 0, 65_535]);
        assert_eq!(bytes[4..], samples[4..], "untouched pixels keep 16 bits");
    }

    /// A 64 x 64 8-bit document whose one layer holds a two-axis ramp, so a
    /// flip or a rotation that moved the wrong bytes would show.
    fn ramp_document(ed: &mut Editor) {
        ed.new_document_with(64, 64, "ramp", BlankBackground::Transparent)
            .unwrap();
        let doc = ed.active_mut().unwrap();
        let layer = doc.document.active_layer().unwrap();
        let ts = TILE_SIZE as usize;
        let mut bytes = vec![0u8; RGBA8];
        for y in 0..ts {
            for x in 0..ts {
                let i = (y * ts + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[(x * 4) as u8, (y * 4) as u8, 50, 255]);
            }
        }
        let h = doc.tiles.insert_bytes(bytes);
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                [TileEdit::set(TileCoord::new(0, 0, 0), h)],
            )
            .unwrap(),
        )
        .unwrap();
    }

    fn composite8(ed: &mut Editor) -> Vec<u8> {
        let doc = ed.active_mut().unwrap();
        let rect = PixelRect::new(0, 0, doc.document.width(), doc.document.height());
        doc.composite(rect).unwrap()
    }

    /// Round-1 review defect 1: after Image > Mode > 16 Bits, the edits that
    /// read a whole layer (`pixels::read_layer`: Edit > Transform flips and
    /// rotations, filters, adjustments) must see the picture, not the first
    /// half of the u16 bytes. The same actions on the 8-bit twin are the
    /// reference: every one of them works at 8 bits, so the 16-bit document
    /// must composite to exactly the same bytes, and its tiles stay 16-bit.
    #[test]
    fn mode_sixteen_then_flip_rotate_filter_and_adjustment_match_the_eight_bit_document() {
        use ui::menu::{AdjustmentId, ChannelDepth, FilterId, MenuAction, TransformOp};
        let dir = tempfile::tempdir().unwrap();
        let mut eight = editor(dir.path());
        ramp_document(&mut eight);
        let mut deep = editor(dir.path());
        ramp_document(&mut deep);
        crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Sixteen), &mut deep)
            .unwrap();
        assert!(tile_lengths(&deep).iter().all(|(_, l)| *l == RGBA16));
        let start = composite8(&mut eight);
        assert_eq!(composite8(&mut deep), start, "8 -> 16 is lossless");
        // Pixel (3, 0) of the ramp.
        assert_eq!(start[12..16], [12, 0, 50, 255]);

        // The flip alone first: pixel (60, 0) of the flipped layer was (3, 0).
        let flip = MenuAction::Transform(TransformOp::FlipHorizontal);
        crate::menu_bridge::perform(flip, &mut deep).unwrap();
        let flipped = composite8(&mut deep);
        assert_eq!(
            flipped[60 * 4..61 * 4],
            [12, 0, 50, 255],
            "the flip mirrored"
        );
        crate::menu_bridge::perform(flip, &mut eight).unwrap();

        let actions = [
            MenuAction::Transform(TransformOp::Rotate180),
            MenuAction::Filter(FilterId::Blur),
            MenuAction::ApplyAdjustment(AdjustmentId::Invert),
        ];
        assert_eq!(composite8(&mut eight), flipped, "{flip:?}");
        for action in actions {
            crate::menu_bridge::perform(action, &mut eight).unwrap();
            crate::menu_bridge::perform(action, &mut deep).unwrap();
            let want = composite8(&mut eight);
            assert_eq!(
                composite8(&mut deep),
                want,
                "{action:?} on the 16-bit document"
            );
            assert!(
                tile_lengths(&deep).iter().all(|(_, l)| *l == RGBA16),
                "{action:?} left the 16-bit document's tiles 16-bit"
            );
        }
    }

    /// Round-1 review defect 2: an opened 16-bit PNG is a 16-bit document
    /// whose tiles arrive RGBA8. The first whole-layer edit's output is
    /// widened to RGBA16 by the apply boundary; the second must read those
    /// tiles correctly, so Flip Horizontal twice is the identity.
    #[test]
    fn an_opened_sixteen_bit_png_flipped_twice_is_the_identity() {
        use ui::menu::{MenuAction, TransformOp};
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (48u32, 32u32);
        let grad16: Vec<u16> = (0..(w * h) as usize)
            .flat_map(|p| {
                let x = (p % w as usize) as u32;
                let y = (p / w as usize) as u32;
                [
                    (x * 65_535 / (w - 1)) as u16,
                    (y * 65_535 / (h - 1)) as u16,
                    30_000,
                    u16::MAX,
                ]
            })
            .collect();
        let source = dir.path().join("deep.png");
        raster::encode_to_path(
            &source,
            raster::ExportFormat::Png,
            w,
            h,
            raster::EncodedPixels::Rgba16(&grad16),
            &raster::EncodeOptions::default(),
        )
        .unwrap();
        let mut ed = editor(dir.path());
        ed.open_path(&source).unwrap();
        assert_eq!(ed.active().unwrap().document.meta.bit_depth, 16);
        let before = composite8(&mut ed);

        let flip = MenuAction::Transform(TransformOp::FlipHorizontal);
        crate::menu_bridge::perform(flip, &mut ed).unwrap();
        let once = composite8(&mut ed);
        // Row 0, column 0 now holds what column 47 held.
        assert_eq!(once[..4], before[47 * 4..48 * 4], "the flip mirrored");
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
        crate::menu_bridge::perform(flip, &mut ed).unwrap();
        assert_eq!(composite8(&mut ed), before, "flip twice is the identity");
    }

    /// Round-1 review defect 4: Image > Mode > 8 Bits dithers (Photoshop's
    /// default "Use Dither"), so a flat 16-bit value halfway between two
    /// 8-bit codes becomes a pattern of both codes rather than one band; a
    /// value that is exactly an 8-bit code is left exact.
    #[test]
    fn mode_eight_from_the_menu_dithers_between_codes_and_keeps_exact_codes() {
        use ui::menu::{ChannelDepth, MenuAction};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            64,
            64,
            "deep",
            BlankBackground::Solid {
                rgba8: [0, 0, 0, 255],
                depth: raster::BitDepth::Sixteen,
            },
        )
        .unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_initial_bit_depth(raster::BitDepth::Sixteen);
        let layer = doc.document.active_layer().unwrap();
        let ts = TILE_SIZE as usize;
        // Left half: 100.5 codes (between 100 and 101); right half: exactly 7.
        let samples: Vec<u16> = (0..ts * ts)
            .flat_map(|p| {
                let v = if p % ts < ts / 2 {
                    100 * 257 + 128
                } else {
                    7 * 257
                };
                [v, v, v, u16::MAX]
            })
            .collect();
        let hash = doc
            .tiles
            .insert_bytes(raster::rgba16_to_tile_bytes(&samples));
        doc.apply(
            Command::paint_tiles(
                PixelTarget::Layer(layer),
                [TileEdit::set(TileCoord::new(0, 0, 0), hash)],
            )
            .unwrap(),
        )
        .unwrap();
        crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Eight), &mut ed).unwrap();
        let tile = &tile_bytes(&ed)[0].1;
        assert_eq!(tile.len(), RGBA8);
        let mut left = std::collections::BTreeSet::new();
        for y in 0..ts {
            for x in 0..ts {
                let i = (y * ts + x) * 4;
                if x < ts / 2 {
                    left.insert(tile[i]);
                } else {
                    assert_eq!(tile[i..i + 4], [7, 7, 7, 255], "exact codes stay");
                }
            }
        }
        assert_eq!(
            left.into_iter().collect::<Vec<_>>(),
            vec![100, 101],
            "a between-codes value is dithered over both codes"
        );
    }

    /// Image > Canvas Size with a background colour on a 16-bit document:
    /// the exposed strip of an existing RGBA16 tile is filled at its own
    /// 8-byte stride, so the original pixels are untouched.
    #[test]
    fn canvas_size_fill_on_a_sixteen_bit_document_keeps_the_old_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            64,
            64,
            "deep",
            BlankBackground::Solid {
                rgba8: [200, 100, 50, 255],
                depth: raster::BitDepth::Sixteen,
            },
        )
        .unwrap();
        let doc = ed.active_mut().unwrap();
        doc.set_initial_bit_depth(raster::BitDepth::Sixteen);
        let spec = ui::dialogs::CanvasSizeSpec {
            width: 128,
            height: 64,
            offset: ui::dialogs::Anchor::TopLeft.offset((64, 64), (128, 64)),
            anchor: ui::dialogs::Anchor::TopLeft,
            background: ui::dialogs::BackgroundContents::Black,
        };
        let command = doc.canvas_size_command(&spec).unwrap();
        doc.apply(command).unwrap();
        let px = composite8(&mut ed);
        let at = |x: usize, y: usize| px[(y * 128 + x) * 4..(y * 128 + x) * 4 + 4].to_vec();
        for y in 0..64 {
            for x in 0..128 {
                let want = if x < 64 {
                    [200, 100, 50, 255]
                } else {
                    [0, 0, 0, 255]
                };
                assert_eq!(at(x, y), want, "pixel ({x}, {y})");
            }
        }
        assert!(tile_lengths(&ed).contains(&(TileCoord::new(0, 0, 0), RGBA16)));
    }

    /// Image > Image Size on a 16-bit document resamples the picture (read
    /// at 8 bits, the same result as the 8-bit twin) and keeps 16-bit tiles.
    #[test]
    fn image_size_on_a_sixteen_bit_document_matches_the_eight_bit_document() {
        use ui::menu::{ChannelDepth, MenuAction};
        let dir = tempfile::tempdir().unwrap();
        let mut eight = editor(dir.path());
        ramp_document(&mut eight);
        let mut deep = editor(dir.path());
        ramp_document(&mut deep);
        crate::menu_bridge::perform(MenuAction::SetBitDepth(ChannelDepth::Sixteen), &mut deep)
            .unwrap();
        let spec = ui::dialogs::ImageSizeSpec {
            width: 32,
            height: 32,
            resolution_ppi: 72.0,
            resample: Some(raster::ResampleFilter::Triangle),
        };
        for ed in [&mut eight, &mut deep] {
            let doc = ed.active_mut().unwrap();
            let command = doc.resample_command(&spec).unwrap();
            doc.apply(command).unwrap();
        }
        let want = composite8(&mut eight);
        assert_eq!(composite8(&mut deep), want);
        assert!(tile_lengths(&deep).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Image > Mode > Grayscale on a 16-bit document converts its RGBA16
    /// tiles instead of skipping them.
    #[test]
    fn grayscale_on_a_sixteen_bit_document_converts_its_tiles() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.new_document_with(
            64,
            64,
            "deep",
            BlankBackground::Solid {
                rgba8: [200, 100, 50, 255],
                depth: raster::BitDepth::Sixteen,
            },
        )
        .unwrap();
        ed.active_mut()
            .unwrap()
            .set_initial_bit_depth(raster::BitDepth::Sixteen);
        ed.set_color_mode(ui::menu::ColorMode::Grayscale).unwrap();
        // 0.299 * 200 + 0.587 * 100 + 0.114 * 50 = 124.2
        assert_eq!(composite8(&mut ed)[..4], [124, 124, 124, 255]);
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
        // The stored pixels themselves are grey, not only their display.
        let tile = raster::tile_bytes_to_rgba16(&tile_bytes(&ed)[0].1);
        assert_eq!(tile[..4], [124 * 257, 124 * 257, 124 * 257, 65_535]);
    }

    /// Every layer tile of a document read back from a `.psd`, in order.
    fn psd_layer_tiles(path: &std::path::Path) -> Vec<(TileCoord, Vec<u8>)> {
        let bytes = std::fs::read(path).unwrap();
        let back = crate::import::document_from_psd(&bytes, "back", 8).unwrap();
        let doc = &back.imported.document;
        let mut out = Vec::new();
        for id in doc.layers.iter_depth_first() {
            if let Some(map) = doc.layer_tiles(id) {
                for (c, h) in map.iter() {
                    out.push((c, back.imported.tiles.tile(h).unwrap().to_vec()));
                }
            }
        }
        out
    }

    /// Round-2 review defect 1: File > Save As PSD of a 16-bit document. The
    /// PSD writer's layer reader (`import::rgba_from_tiles`) copied RGBA16
    /// tiles at a 4-byte stride, so a solid [200,100,50,255] came back as
    /// [200,200,100,100,...]. The 16-bit document must write the same layer
    /// pixels as its 8-bit twin — for a New Document's solid background and
    /// for a Mode > 16 Bits ramp.
    #[test]
    fn a_sixteen_bit_document_saves_as_psd_with_the_eight_bit_twins_layer_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let mut exported = Vec::new();
        for depth in [raster::BitDepth::Eight, raster::BitDepth::Sixteen] {
            let mut ed = editor(dir.path());
            ed.new_document_with(
                64,
                64,
                "solid",
                BlankBackground::Solid {
                    rgba8: [200, 100, 50, 255],
                    depth,
                },
            )
            .unwrap();
            ed.active_mut().unwrap().set_initial_bit_depth(depth);
            let path = dir.path().join(format!("solid-{depth:?}.psd"));
            ed.active_mut().unwrap().export_psd_to(&path).unwrap();
            exported.push(psd_layer_tiles(&path));
        }
        assert!(!exported[0].is_empty());
        assert_eq!(exported[0], exported[1], "New Document 16-bit vs 8-bit");
        let (_, tile) = &exported[1][0];
        assert_eq!(tile[..8], [200, 100, 50, 255, 200, 100, 50, 255]);

        let mut ramps = Vec::new();
        for sixteen in [false, true] {
            let mut ed = editor(dir.path());
            ramp_document(&mut ed);
            if sixteen {
                ed.active_mut().unwrap().convert_depth(16, false).unwrap();
                assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
            }
            let path = dir.path().join(format!("ramp-{sixteen}.psd"));
            ed.active_mut().unwrap().export_psd_to(&path).unwrap();
            ramps.push(psd_layer_tiles(&path));
        }
        assert!(!ramps[0].is_empty());
        assert_eq!(ramps[0], ramps[1], "Mode 16 ramp vs the 8-bit ramp");
    }
}
