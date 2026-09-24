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

/// W7-C: whole-layer reads and writes at 16 bits.
///
/// The menu edits that rewrite a whole layer (filters, adjustments, fills,
/// clears, flips and rotations) and Image Size run in a 16-bit document
/// through these instead of the RGBA8 `read_layer` / `write_layer` pair, so
/// the layer never passes through 8 bits: they read every tile at 16 bits
/// ([`raster::depth::rgba16_samples`] widens an RGBA8 tile losslessly) and
/// write RGBA16 tiles, which the apply boundary below passes through as they
/// are. An 8-bit document never reaches them.
impl OpenDocument {
    /// A layer's pixels over the canvas rectangle as straight-alpha RGBA16
    /// samples: the 16-bit twin of `menu_bridge::pixels::read_layer`, with
    /// the same clipping (tiles outside the canvas dropped, absent tiles
    /// transparent black).
    pub fn layer_rgba16(&self, layer: layer_model::LayerId) -> Vec<u16> {
        let (w, h) = (self.document.width(), self.document.height());
        match self.document.layer_tiles(layer) {
            Some(map) => tiles_rgba16(&self.tiles, map, w, h),
            None => vec![0u16; w as usize * h as usize * 4],
        }
    }

    /// The command that makes `rgba16` (canvas-sized RGBA16 samples) the
    /// layer's pixels: the 16-bit twin of `menu_bridge::pixels::write_layer`,
    /// including its clearing of tiles the new image no longer covers.
    pub fn layer_rgba16_command(
        &mut self,
        layer: layer_model::LayerId,
        rgba16: &[u16],
        label: &str,
    ) -> Result<Command, String> {
        let (w, h) = (self.document.width(), self.document.height());
        if rgba16.len() != w as usize * h as usize * 4 {
            return Err("The pixel buffer does not match the canvas".to_string());
        }
        let previous: Vec<raster::TileCoord> = self
            .document
            .layer_tiles(layer)
            .map(|m| m.iter().map(|(c, _)| c).collect())
            .unwrap_or_default();
        let mut edits = rgba16_tile_edits(&mut self.tiles, w, h, rgba16);
        let covered: std::collections::HashSet<raster::TileCoord> =
            edits.iter().map(|e| e.coord).collect();
        for coord in previous {
            if !covered.contains(&coord) {
                edits.push(TileEdit::clear(coord));
            }
        }
        let paint =
            Command::paint_tiles(PixelTarget::Layer(layer), edits).map_err(|e| e.to_string())?;
        Ok(Command::Transaction {
            label: label.to_string(),
            commands: vec![paint],
        })
    }
}

/// W7-C: Image ▸ Image Rotation on a 16-bit document. The 8-bit route
/// (`OpenDocument::rotate_canvas_90` / `rotate_canvas_arbitrary` in
/// `doc.rs`) reads each layer composited alone as RGBA8; these read the same
/// composite as RGBA16 and write RGBA16 tiles, so a quarter turn moves every
/// 16-bit code and an arbitrary turn interpolates at 16 bits.
impl OpenDocument {
    /// `layer_pixels` at 16 bits: the layer composited alone over
    /// transparent, full resolution, as straight RGBA16.
    fn layer_pixels16(
        &self,
        layer_id: layer_model::LayerId,
    ) -> Result<Vec<u16>, super::DocumentError> {
        let mut staged = self.document.clone();
        for other in staged.layers.iter_depth_first() {
            if other != layer_id {
                if let Some(l) = staged.layers.get_mut(other) {
                    l.visible = false;
                }
            }
        }
        let canvas = compositor::composite_region(
            &staged,
            &self.tiles,
            self.canvas_rect(),
            0,
            compositor::CompositeOptions::default(),
        )?;
        Ok(canvas.to_rgba16(&self.document.meta.color_space))
    }

    /// One layer of a 16-bit document re-framed onto a `new_w x new_h`
    /// canvas whose origin is the old canvas's `src_min` (Crop to Selection,
    /// Trim): every 16-bit code moves as it is.
    pub(super) fn reframed_layer16(
        &mut self,
        id: layer_model::LayerId,
        (new_w, new_h): (u32, u32),
        src_min: glam::IVec2,
    ) -> Result<Command, super::DocumentError> {
        let (old_w, old_h) = (self.document.width(), self.document.height());
        let rgba = self.layer_pixels16(id)?;
        let mut out = vec![0u16; new_w as usize * new_h as usize * 4];
        for dy in 0..new_h {
            let sy = dy as i64 + src_min.y as i64;
            if sy < 0 || sy >= old_h as i64 {
                continue;
            }
            for dx in 0..new_w {
                let sx = dx as i64 + src_min.x as i64;
                if sx < 0 || sx >= old_w as i64 {
                    continue;
                }
                let si = (sy as usize * old_w as usize + sx as usize) * 4;
                let di = (dy as usize * new_w as usize + dx as usize) * 4;
                out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
            }
        }
        let edits = rgba16_tile_edits(&mut self.tiles, new_w, new_h, &out);
        Ok(Command::paint_tiles(PixelTarget::Layer(id), edits)?)
    }

    /// One layer of a 16-bit document turned a quarter (clockwise or not):
    /// the `PaintTiles` for the swapped `(old_h, old_w)` canvas.
    pub(super) fn rotated90_layer16(
        &mut self,
        id: layer_model::LayerId,
        clockwise: bool,
    ) -> Result<Command, super::DocumentError> {
        let (old_w, old_h) = (self.document.width(), self.document.height());
        let (new_w, new_h) = (old_h, old_w);
        let rgba = self.layer_pixels16(id)?;
        let mut out = vec![0u16; new_w as usize * new_h as usize * 4];
        for y in 0..old_h {
            for x in 0..old_w {
                let (nx, ny) = if clockwise {
                    (y, old_w - 1 - x)
                } else {
                    (old_h - 1 - y, x)
                };
                let si = (y as usize * old_w as usize + x as usize) * 4;
                let di = (ny as usize * new_w as usize + nx as usize) * 4;
                out[di..di + 4].copy_from_slice(&rgba[si..si + 4]);
            }
        }
        let edits = rgba16_tile_edits(&mut self.tiles, new_w, new_h, &out);
        Ok(Command::paint_tiles(PixelTarget::Layer(id), edits)?)
    }

    /// One layer of a 16-bit document turned `(sin, cos)` about the canvas
    /// centre onto the grown `new_w x new_h` canvas: the same inverse map
    /// and premultiplied bilinear sampler as the 8-bit route, at 16 bits.
    pub(super) fn rotated_layer16(
        &mut self,
        id: layer_model::LayerId,
        (sin, cos): (f64, f64),
        (new_w, new_h): (u32, u32),
    ) -> Result<Command, super::DocumentError> {
        let (w, h) = (self.document.width(), self.document.height());
        let rgba = self.layer_pixels16(id)?;
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let (ncx, ncy) = (new_w as f64 / 2.0, new_h as f64 / 2.0);
        let mut out = vec![0u16; new_w as usize * new_h as usize * 4];
        for y in 0..new_h {
            for x in 0..new_w {
                let dx = x as f64 + 0.5 - ncx;
                let dy = y as f64 + 0.5 - ncy;
                let sx = cos * dx + sin * dy + cx;
                let sy = -sin * dx + cos * dy + cy;
                let px = sample_bilinear_premultiplied16(
                    &rgba,
                    w as usize,
                    h as usize,
                    sx - 0.5,
                    sy - 0.5,
                );
                let di = (y as usize * new_w as usize + x as usize) * 4;
                out[di..di + 4].copy_from_slice(&px);
            }
        }
        let edits = rgba16_tile_edits(&mut self.tiles, new_w, new_h, &out);
        Ok(Command::paint_tiles(PixelTarget::Layer(id), edits)?)
    }
}

/// The 16-bit twin of `doc.rs`'s `sample_bilinear_premultiplied`:
/// premultiplied on the way in, un-premultiplied on the way out, transparent
/// outside the buffer. Worked in `f64` so a weight of exactly one returns the
/// stored code.
fn sample_bilinear_premultiplied16(rgba: &[u16], w: usize, h: usize, x: f64, y: f64) -> [u16; 4] {
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let at = |xx: i64, yy: i64| -> [f64; 4] {
        if xx < 0 || yy < 0 || xx >= w as i64 || yy >= h as i64 {
            return [0.0; 4];
        }
        let i = (yy as usize * w + xx as usize) * 4;
        let a = f64::from(rgba[i + 3]) / 65_535.0;
        [
            f64::from(rgba[i]) / 65_535.0 * a,
            f64::from(rgba[i + 1]) / 65_535.0 * a,
            f64::from(rgba[i + 2]) / 65_535.0 * a,
            a,
        ]
    };
    let lerp = |a: f64, b: f64, t: f64| a + (b - a) * t;
    let (p00, p10, p01, p11) = (
        at(x0, y0),
        at(x0 + 1, y0),
        at(x0, y0 + 1),
        at(x0 + 1, y0 + 1),
    );
    let mut px = [0.0f64; 4];
    for c in 0..4 {
        px[c] = lerp(lerp(p00[c], p10[c], fx), lerp(p01[c], p11[c], fx), fy);
    }
    let a = px[3];
    if a <= 0.0 {
        return [0, 0, 0, 0];
    }
    let to16 = |v: f64| (v.clamp(0.0, 1.0) * 65_535.0).round() as u16;
    [to16(px[0] / a), to16(px[1] / a), to16(px[2] / a), to16(a)]
}

/// Image ▸ Image Size for one layer of a 16-bit document: read at 16 bits,
/// resampled in linear light, written back as RGBA16 tiles. Takes the tile
/// store and the colour space separately so the caller can keep borrowing
/// the layer's tile map out of the document while it runs.
pub(super) fn resample_layer16_edits(
    tiles: &mut MemoryTileSource,
    space: &color::ColorSpace,
    map: &editor_core::TileMap,
    (w, h): (u32, u32),
    (dw, dh): (u32, u32),
    filter: raster::ResampleFilter,
) -> Result<Vec<TileEdit>, super::DocumentError> {
    let rgba = tiles_rgba16(tiles, map, w, h);
    let image = raster::export::linear_from_rgba16(w, h, &rgba, space)?;
    let scaled = raster::export::resample(&image, dw, dh, filter)?;
    let out = raster::export::rgba16_from_linear(&scaled, space)?;
    Ok(rgba16_tile_edits(tiles, dw, dh, &out))
}

/// Every level-0 tile of `map` laid onto a `w x h` RGBA16 canvas buffer.
fn tiles_rgba16(tiles: &MemoryTileSource, map: &editor_core::TileMap, w: u32, h: u32) -> Vec<u16> {
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u16; w * h * 4];
    let ts = raster::TILE_SIZE as usize;
    for (coord, hash) in map.iter() {
        if coord.level != 0 {
            continue;
        }
        let Some(samples) = tiles.tile(hash).and_then(raster::depth::rgba16_samples) else {
            continue;
        };
        let ox = coord.x as i64 * ts as i64;
        let oy = coord.y as i64 * ts as i64;
        let x0 = ox.max(0);
        let x1 = (ox + ts as i64).min(w as i64);
        if x1 <= x0 {
            continue;
        }
        let n = (x1 - x0) as usize * 4;
        for row in 0..ts {
            let y = oy + row as i64;
            if y < 0 || y >= h as i64 {
                continue;
            }
            let s = (row * ts + (x0 - ox) as usize) * 4;
            let d = (y as usize * w + x0 as usize) * 4;
            out[d..d + n].copy_from_slice(&samples[s..s + n]);
        }
    }
    out
}

/// Store a `w x h` RGBA16 canvas buffer as RGBA16 tiles: one edit per
/// covering tile, padding zeroed (so equal edge tiles dedupe), row-major like
/// the 8-bit `raster::TileGrid::from_rgba8`.
fn rgba16_tile_edits(
    tiles: &mut MemoryTileSource,
    w: u32,
    h: u32,
    rgba16: &[u16],
) -> Vec<TileEdit> {
    let ts = raster::TILE_SIZE as usize;
    let (w, h) = (w as usize, h as usize);
    let mut edits = Vec::new();
    for ty in 0..h.div_ceil(ts) {
        for tx in 0..w.div_ceil(ts) {
            let mut tile = vec![0u16; ts * ts * 4];
            let vw = (w - tx * ts).min(ts);
            let vh = (h - ty * ts).min(ts);
            for row in 0..vh {
                let s = ((ty * ts + row) * w + tx * ts) * 4;
                tile[row * ts * 4..row * ts * 4 + vw * 4].copy_from_slice(&rgba16[s..s + vw * 4]);
            }
            let hash = tiles.insert_bytes(raster::rgba16_to_tile_bytes(&tile));
            edits.push(TileEdit::set(
                raster::TileCoord::new(tx as i32, ty as i32, 0),
                hash,
            ));
        }
    }
    edits
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
///
/// W7-C: this narrowing is what `TileAccess::bytes` answers. Free
/// Transform's resample reads through `TileAccess::native_bytes` instead
/// (`tools::patch::ColorPatch::load_native`), which the tool context answers
/// with the stored bytes, so it computes and commits at 16 bits.
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
                if bytes.tile(hash).is_some_and(|b| {
                    b.len() == raster::depth::RGBA16_TILE_BYTES
                        || raster::depth32::is_rgbaf32_tile(b)
                }) {
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
                    // W10-H: an f32 tile narrows the same way.
                    raster::rgba8_view(stored).into_owned()
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
    // W10-H: a 32-bit document lands 8/16-bit output as f32.
    if document.meta.bit_depth == 32 {
        return crate::depth32::fit_to_f32_document(document, tiles, command);
    }
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

    /// Image > Image Size on a 16-bit document resamples the picture (at 16
    /// bits since W7-C; on 8-bit-grid content the result still composites to
    /// the 8-bit twin's bytes) and keeps 16-bit tiles.
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

    // ---- W7-C: 16-bit documents are edited at 16 bits --------------------

    /// A 64 x 64 16-bit document whose one layer holds `pixel(x, y)` —
    /// genuinely 16-bit codes, stored as RGBA16 tiles. Returns the samples.
    fn deep_document(ed: &mut Editor, pixel: impl Fn(usize, usize) -> [u16; 4]) -> Vec<u16> {
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
        let samples: Vec<u16> = (0..64 * 64).flat_map(|i| pixel(i % 64, i / 64)).collect();
        let layer = doc.document.active_layer().unwrap();
        let seed = doc.layer_rgba16_command(layer, &samples, "Seed").unwrap();
        doc.apply(seed).unwrap();
        assert_eq!(doc.layer_rgba16(layer), samples, "the seed reads back");
        assert!(tile_lengths(ed).iter().all(|(_, l)| *l == RGBA16));
        samples
    }

    /// Codes that sit off the 8-bit grid almost everywhere (a multiple of 257
    /// is exactly an 8-bit code widened), so any trip through 8 bits shows.
    fn busy_ramp(x: usize, y: usize) -> [u16; 4] {
        [
            (20_000 + x * 37 + y * 5) as u16,
            (1_000 + y * 601 + x) as u16,
            (65_000 - x * 513 - y * 3) as u16,
            65_535,
        ]
    }

    fn active_layer_rgba16(ed: &Editor) -> Vec<u16> {
        let doc = ed.active().unwrap();
        doc.layer_rgba16(doc.document.active_layer().unwrap())
    }

    fn assert_within(got: &[u16], want: &[u16], codes: u16, what: &str) {
        assert_eq!(got.len(), want.len(), "{what}");
        for (i, (g, w)) in got.iter().zip(want).enumerate() {
            assert!(
                g.abs_diff(*w) <= codes,
                "{what}: sample {i} is {g}, want {w} (+/- {codes})"
            );
        }
    }

    /// The fraction of colour samples that are not an 8-bit code widened.
    fn off_the_eight_bit_grid(samples: &[u16]) -> f32 {
        let colour: Vec<u16> = samples.chunks(4).flat_map(|p| p[..3].to_vec()).collect();
        colour.iter().filter(|v| **v % 257 != 0).count() as f32 / colour.len() as f32
    }

    /// Image > Adjustments > Brightness/Contrast (brightness 0.1 on the
    /// engine's -1..=1 scale) through the menu's own adjustment route on a
    /// 16-bit document: the result is the f32 engine's answer at 16 bits,
    /// not a multiple of 257 (which is what a trip through 8 bits leaves).
    #[test]
    fn brightness_on_a_sixteen_bit_document_is_not_rounded_through_eight_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let adjustment = adjustments::Adjustment::BrightnessContrast(
            adjustments::BrightnessContrast::new(0.1, 0.0).unwrap(),
        );
        let space = ed.active().unwrap().document.meta.color_space.clone();
        let mut reference = filters::FilterBuffer::from_rgba16(64, 64, &before).unwrap();
        adjustments::PreparedAdjustment::new(&adjustment)
            .apply_premultiplied_rgba(reference.pixels_mut(), &space);
        let reference = reference.to_rgba16();

        crate::menu_bridge::run_adjustment_kind(&mut ed, &adjustment, "Brightness/Contrast")
            .unwrap();
        let after = active_layer_rgba16(&ed);
        assert_ne!(after, before, "the adjustment changed the layer");
        assert_eq!(after, reference, "the layer is the engine's 16-bit answer");
        let off = off_the_eight_bit_grid(&after);
        assert!(
            off > 0.9,
            "only {off} of the samples are off the 8-bit grid"
        );
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// The whole-layer pixel road a filter or adjustment takes in a 16-bit
    /// document (read at 16 bits, the f32 buffer, write at 16 bits) keeps
    /// every code within one for Gaussian Blur radius 0, Levels at identity
    /// and Curves at identity; the menu itself refuses an identity
    /// adjustment and a no-op filter without writing a thing.
    #[test]
    fn gaussian_zero_levels_and_curves_identity_keep_every_sixteen_bit_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let space = ed.active().unwrap().document.meta.color_space.clone();
        let read = || filters::FilterBuffer::from_rgba16(64, 64, &before).unwrap();

        let blurred = filters::blur::gaussian_blur(&read(), 0.0, filters::EdgeMode::Clamp);
        assert_within(&blurred.to_rgba16(), &before, 1, "Gaussian Blur radius 0");
        for (name, adjustment) in [
            (
                "Levels",
                adjustments::Adjustment::Levels(adjustments::Levels::IDENTITY),
            ),
            (
                "Curves",
                adjustments::Adjustment::Curves(adjustments::Curves::identity()),
            ),
        ] {
            let mut buffer = read();
            adjustments::PreparedAdjustment::new(&adjustment)
                .apply_premultiplied_rgba(buffer.pixels_mut(), &space);
            assert_within(&buffer.to_rgba16(), &before, 1, name);
        }

        // The menu route: Gaussian Blur at radius 0 changes nothing, says so,
        // and leaves the layer's 16-bit codes exactly as they were.
        let spec = ui::dialogs::filter_by_id(ui::menu::FilterId::GaussianBlur).unwrap();
        let mut params = ui::dialogs::FilterParams::defaults(spec.params);
        assert!(params.set("radius", ui::dialogs::ParamValue::Float(0.0)));
        let invocation = ui::dialogs::FilterInvocation {
            filter: spec,
            params,
        };
        let refused = crate::menu_bridge::run_filter_invocation(&mut ed, &invocation);
        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(active_layer_rgba16(&ed), before);
    }

    /// Edit > Transform > Rotate 90 CW twice, through the menu, on a square
    /// 16-bit document: every 16-bit code lands exactly where a 180-degree
    /// turn puts it.
    #[test]
    fn rotate_ninety_twice_on_a_sixteen_bit_document_moves_every_code_exactly() {
        use ui::menu::{MenuAction, TransformOp};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let rotate = MenuAction::Transform(TransformOp::Rotate90Cw);
        crate::menu_bridge::perform(rotate, &mut ed).unwrap();
        crate::menu_bridge::perform(rotate, &mut ed).unwrap();
        let after = active_layer_rgba16(&ed);
        let want: Vec<u16> = (0..64 * 64)
            .flat_map(|i| {
                let (x, y) = (63 - i % 64, 63 - i / 64);
                busy_ramp(x, y)
            })
            .collect();
        assert_eq!(after, want, "two quarter turns are a half turn, exactly");
        assert_ne!(after, before);
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Image > Image Size to half and back to full on a smooth 16-bit ramp:
    /// away from the edges (where the filter clamps) every code comes back
    /// within one; at 8 bits the same ramp would be quantised to multiples
    /// of 257, up to 128 codes away.
    #[test]
    fn image_size_half_then_double_on_a_sixteen_bit_ramp_keeps_every_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let gentle = |x: usize, y: usize| {
            [
                (20_000 + x * 3) as u16,
                (30_000 + y * 2) as u16,
                12_345,
                65_535,
            ]
        };
        let before = deep_document(&mut ed, gentle);
        for size in [32, 64] {
            let spec = ui::dialogs::ImageSizeSpec {
                width: size,
                height: size,
                resolution_ppi: 72.0,
                resample: Some(raster::ResampleFilter::Triangle),
            };
            let doc = ed.active_mut().unwrap();
            let command = doc.resample_command(&spec).unwrap();
            doc.apply(command).unwrap();
        }
        let after = active_layer_rgba16(&ed);
        assert_eq!(after.len(), before.len());
        let interior = |v: &[u16]| -> Vec<u16> {
            (0..64 * 64)
                .filter(|i| (4..60).contains(&(i % 64)) && (4..60).contains(&(i / 64)))
                .flat_map(|i| v[i * 4..i * 4 + 4].to_vec())
                .collect()
        };
        assert_within(
            &interior(&after),
            &interior(&before),
            1,
            "Image Size 0.5 then 2.0",
        );
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Image > Canvas Size with a black extension on a 16-bit document: the
    /// old pixels keep their exact 16-bit codes and the new strip is black at
    /// 16 bits (Canvas Size never resamples, it re-frames).
    #[test]
    fn canvas_size_on_a_sixteen_bit_document_keeps_every_code_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let doc = ed.active_mut().unwrap();
        let spec = ui::dialogs::CanvasSizeSpec {
            width: 128,
            height: 64,
            offset: ui::dialogs::Anchor::TopLeft.offset((64, 64), (128, 64)),
            anchor: ui::dialogs::Anchor::TopLeft,
            background: ui::dialogs::BackgroundContents::Black,
        };
        let command = doc.canvas_size_command(&spec).unwrap();
        doc.apply(command).unwrap();
        let after = active_layer_rgba16(&ed);
        for y in 0..64 {
            for x in 0..128 {
                let got = &after[(y * 128 + x) * 4..(y * 128 + x) * 4 + 4];
                let want = if x < 64 {
                    before[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4].to_vec()
                } else {
                    vec![0, 0, 0, 65_535]
                };
                assert_eq!(got, want.as_slice(), "pixel ({x}, {y})");
            }
        }
    }

    /// Edit > Fill with black at 50% on a 16-bit layer: each code is halved
    /// at 16 bits, not snapped to an 8-bit code.
    #[test]
    fn a_half_opacity_fill_on_a_sixteen_bit_document_is_computed_at_sixteen_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        ed.set_foreground([0.0, 0.0, 0.0, 1.0]);
        let spec = ui::dialogs::FillSpec {
            opacity: 0.5,
            ..Default::default()
        };
        crate::menu_bridge::fill_selection_with(&mut ed, &spec).unwrap();
        let after = active_layer_rgba16(&ed);
        let want: Vec<u16> = before
            .chunks(4)
            .flat_map(|p| {
                [
                    (f32::from(p[0]) * 0.5).round() as u16,
                    (f32::from(p[1]) * 0.5).round() as u16,
                    (f32::from(p[2]) * 0.5).round() as u16,
                    p[3],
                ]
            })
            .collect();
        assert_within(&after, &want, 1, "a 50% black fill");
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    // ---- W7-C round 2 ------------------------------------------------------

    /// The pixel at `(x, y)` of a 64-wide RGBA16 buffer.
    fn px16(v: &[u16], x: usize, y: usize) -> &[u16] {
        &v[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4]
    }

    /// Drag the Free Transform session's interior (a press off its pivot)
    /// by `by` through the
    /// canvas pointer route (100% zoom, image centred in a 400 x 300
    /// viewport) with the options bar's transform `mode`, then commit it the
    /// way Enter does.
    fn free_transform_drag(ed: &mut Editor, mode: usize, from: (f32, f32), by: (f32, f32)) {
        let viewport = Vec2::new(400.0, 300.0);
        {
            let doc = ed.active_mut().unwrap();
            doc.set_viewport(viewport);
            doc.camera.zoom = 1.0;
            doc.camera.center = Vec2::new(32.0, 32.0);
        }
        let screen = |x: f32, y: f32| viewport * 0.5 + Vec2::new(x - 32.0, y - 32.0);
        ed.set_tool(tools::ToolId::FreeTransform);
        let settings = [("mode".to_string(), tools::ToolSetting::Choice(mode))];
        let mut pointer = ToolPointer::new();
        let to = (from.0 + by.0, from.1 + by.1);
        for (phase, (x, y)) in [
            (PointerPhase::Down, from),
            (PointerPhase::Move, to),
            (PointerPhase::Up, to),
        ] {
            pointer.handle(ed, PointerInput::at(phase, screen(x, y)), false, &settings);
        }
        let outcome = pointer.commit(ed);
        assert!(outcome.had_pending, "{outcome:?}");
        assert_eq!(outcome.failed, None, "{outcome:?}");
        assert_eq!(outcome.steps, 1, "{outcome:?}");
    }

    /// Round-2 defect 1: Edit > Free Transform in Distort mode (a resample,
    /// not a layer transform) on a 16-bit layer, driven through the canvas
    /// pointer route: the layer moved by (8, 8) keeps every 16-bit code
    /// within one, off the 8-bit grid, in RGBA16 tiles.
    #[test]
    fn free_transform_distort_on_a_sixteen_bit_layer_resamples_at_sixteen_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        // Off the pivot (the centre), so the press grabs the interior.
        free_transform_drag(&mut ed, 3, (20.0, 20.0), (8.0, 8.0));
        let after = active_layer_rgba16(&ed);
        let mut moved = Vec::new();
        let mut want = Vec::new();
        for y in 8..64 {
            for x in 8..64 {
                moved.extend_from_slice(px16(&after, x, y));
                want.extend_from_slice(px16(&before, x - 8, y - 8));
            }
        }
        assert_within(&moved, &want, 1, "Free Transform (Distort) moved by 8");
        let off = off_the_eight_bit_grid(&moved);
        assert!(
            off > 0.9,
            "only {off} of the samples are off the 8-bit grid"
        );
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Round-2 defect 1, the floating road: Free Transform with a pixel
    /// selection lifts the selected pixels, moves them and lays them back,
    /// all at 16 bits; a pixel neither lifted nor covered keeps its exact
    /// stored code.
    #[test]
    fn a_floating_free_transform_on_a_sixteen_bit_layer_keeps_every_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(4, 4),
            max: glam::IVec2::new(60, 60),
        };
        // A press well away from every handle (corners, edge midpoints and
        // the pivot at the centre) grabs the interior: a move.
        free_transform_drag(&mut ed, 0, (22.0, 22.0), (4.0, 4.0));
        let after = active_layer_rgba16(&ed);
        let mut moved = Vec::new();
        let mut want = Vec::new();
        for y in 8..64 {
            for x in 8..64 {
                moved.extend_from_slice(px16(&after, x, y));
                want.extend_from_slice(px16(&before, x - 4, y - 4));
            }
        }
        assert_within(&moved, &want, 1, "the floated square");
        let off = off_the_eight_bit_grid(&moved);
        assert!(
            off > 0.9,
            "only {off} of the samples are off the 8-bit grid"
        );
        for (x, y) in [(2, 2), (62, 2), (2, 62), (1, 40)] {
            assert_eq!(px16(&after, x, y), px16(&before, x, y), "({x}, {y})");
        }
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Round-3 defect 1: an OPENED 16-bit PNG is a 16-bit document whose
    /// tiles arrive RGBA8. Free Transform (Distort) through the canvas
    /// pointer route by a sub-pixel (7.5, 5.5) must still resample at 16
    /// bits: the plane knows the document's depth rather than inferring it
    /// from tile lengths, so the half-pixel blends land off the 8-bit grid,
    /// close to the interpolated source, in RGBA16 tiles.
    #[test]
    fn an_opened_sixteen_bit_png_free_transformed_by_a_sub_pixel_stays_sixteen_bit() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64u32, 64u32);
        let grad16: Vec<u16> = (0..(w * h) as usize)
            .flat_map(|p| {
                let (x, y) = (p % w as usize, p / w as usize);
                [
                    (x * 1_000) as u16,
                    (y * 1_000 + x * 13) as u16,
                    30_000,
                    u16::MAX,
                ]
            })
            .collect();
        let source = dir.path().join("deep-ft.png");
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
        assert!(
            tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA8),
            "the opened 16-bit PNG arrives as RGBA8 tiles (the state under test)"
        );
        let before = active_layer_rgba16(&ed);

        free_transform_drag(&mut ed, 3, (20.0, 20.0), (7.5, 5.5));
        let after = active_layer_rgba16(&ed);

        // The interior, away from the edges the move uncovers.
        let mut moved = Vec::new();
        let mut want = Vec::new();
        for y in 10..60 {
            for x in 10..60 {
                // Red and green ramp; blue is flat, so it is left out.
                moved.extend_from_slice(&px16(&after, x, y)[..2]);
                // Bilinear of the (widened) source at (x - 7.5, y - 5.5).
                let (sx, sy) = (x - 8, y - 6);
                for k in 0..2 {
                    let a = f64::from(px16(&before, sx, sy)[k]);
                    let b = f64::from(px16(&before, sx + 1, sy)[k]);
                    let c = f64::from(px16(&before, sx, sy + 1)[k]);
                    let d = f64::from(px16(&before, sx + 1, sy + 1)[k]);
                    want.push(((a + b + c + d) / 4.0).round() as u16);
                }
            }
        }
        // Within two 8-bit steps of the blend, and a good share off the
        // 8-bit grid: a trip through 8 bits leaves every code a multiple of
        // 257 (0.0 off the grid, which is what the unfixed route measured).
        assert_within(&moved, &want, 514, "Free Transform (Distort) by (7.5, 5.5)");
        let off = moved.iter().filter(|v| **v % 257 != 0).count() as f32 / moved.len() as f32;
        assert!(
            off > 0.25,
            "only {off} of the samples are off the 8-bit grid"
        );
        assert!(
            tile_lengths(&ed).iter().any(|(_, l)| *l == RGBA16),
            "the transformed tiles are RGBA16"
        );
    }

    /// Round-2 defect 2: Image > Image Rotation > 90 CW twice (the Image
    /// menu's canvas rotation, not Edit > Transform) is a half turn at 16
    /// bits, and 90 CCW after it is the quarter turn clockwise from the
    /// start; every code within one.
    #[test]
    fn image_rotation_ninety_on_a_sixteen_bit_document_moves_every_code() {
        use ui::menu::{CanvasRotation, MenuAction};
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let cw = MenuAction::RotateCanvas(CanvasRotation::Deg90Cw);
        crate::menu_bridge::perform(cw, &mut ed).unwrap();
        crate::menu_bridge::perform(cw, &mut ed).unwrap();
        let half: Vec<u16> = (0..64 * 64)
            .flat_map(|i| busy_ramp(63 - i % 64, 63 - i / 64))
            .collect();
        let after = active_layer_rgba16(&ed);
        assert_within(&after, &half, 1, "Image Rotation 90 CW twice");
        assert!(off_the_eight_bit_grid(&after) > 0.9);
        let ccw = MenuAction::RotateCanvas(CanvasRotation::Deg90Ccw);
        crate::menu_bridge::perform(ccw, &mut ed).unwrap();
        // A quarter turn clockwise from the start: new (x, y) holds the
        // original (63 - y, x).
        let quarter: Vec<u16> = (0..64 * 64)
            .flat_map(|i| busy_ramp(63 - i / 64, i % 64))
            .collect();
        let after = active_layer_rgba16(&ed);
        assert_within(&after, &quarter, 1, "then 90 CCW");
        assert_ne!(after, before);
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Round-2 defect 2: Image > Image Rotation > Arbitrary (30 degrees,
    /// the call the shell makes for it) on a flat 16-bit colour off the
    /// 8-bit grid: every pixel the turned canvas fully covers keeps that
    /// exact colour, within one code.
    #[test]
    fn image_rotation_arbitrary_on_a_sixteen_bit_document_interpolates_at_sixteen_bits() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let colour = [20_001u16, 1_003, 40_007, 65_535];
        deep_document(&mut ed, |_, _| colour);
        let command = ed
            .active_mut()
            .unwrap()
            .rotate_canvas_arbitrary(30.0)
            .unwrap();
        ed.apply_command(command);
        let doc = ed.active().unwrap();
        let (w, h) = (
            doc.document.width() as usize,
            doc.document.height() as usize,
        );
        assert!(w > 64 && h > 64, "the canvas grew to {w}x{h}");
        let after = active_layer_rgba16(&ed);
        let mut interior = 0;
        for y in 0..h {
            for x in 0..w {
                let p = &after[(y * w + x) * 4..(y * w + x) * 4 + 4];
                if p[3] == 65_535 {
                    interior += 1;
                    for c in 0..3 {
                        assert!(p[c].abs_diff(colour[c]) <= 1, "({x}, {y}) {p:?}");
                    }
                }
            }
        }
        assert!(interior > 2_000, "only {interior} fully covered pixels");
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Image > Crop to Selection re-frames a 16-bit layer without rounding
    /// a code.
    #[test]
    fn crop_to_selection_on_a_sixteen_bit_document_keeps_every_code() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
            min: glam::IVec2::new(10, 20),
            max: glam::IVec2::new(42, 52),
        };
        ed.crop_to_selection().unwrap();
        let after = active_layer_rgba16(&ed);
        let want: Vec<u16> = (0..32 * 32)
            .flat_map(|i| px16(&before, 10 + i % 32, 20 + i / 32).to_vec())
            .collect();
        assert_within(&after, &want, 1, "Crop to Selection");
        assert!(off_the_eight_bit_grid(&after) > 0.9);
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Round-2 defect 3: Edit > Clear under a half-covered selection leaves
    /// each pixel its unselected share computed at 16 bits (not from an
    /// 8-bit rounding), and a fully selected pixel is cleared.
    #[test]
    fn clear_under_a_partial_selection_keeps_a_sixteen_bit_remainder() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let before = deep_document(&mut ed, busy_ramp);
        let coverage: Vec<u8> = (0..64 * 64)
            .map(|i| if i % 64 < 32 { 255 } else { 128 })
            .collect();
        ed.active_mut().unwrap().document.selection = editor_core::Selection::Mask(
            editor_core::SelectionMask::new(glam::IVec2::ZERO, 64, 64, coverage).unwrap(),
        );
        crate::menu_bridge::perform(ui::menu::MenuAction::ClearPixels, &mut ed).unwrap();
        let after = active_layer_rgba16(&ed);
        let keep = 1.0 - 128.0 / 255.0;
        let mut rest = Vec::new();
        let mut want = Vec::new();
        for y in 0..64 {
            for x in 0..64 {
                if x < 32 {
                    assert_eq!(px16(&after, x, y), [0, 0, 0, 0], "({x}, {y}) cleared");
                } else {
                    rest.extend_from_slice(px16(&after, x, y));
                    want.extend(
                        px16(&before, x, y)
                            .iter()
                            .map(|v| (f32::from(*v) * keep).round() as u16),
                    );
                }
            }
        }
        assert_within(&rest, &want, 1, "the unselected share of each pixel");
        assert!(tile_lengths(&ed).iter().all(|(_, l)| *l == RGBA16));
    }

    /// Round-2 defect 4: Levels and Curves through the menu's own
    /// adjustment route on a 16-bit layer. The menu refuses an exact
    /// identity (so that is pinned engine-side above); a small move off
    /// identity goes through it, and the layer is exactly the f32 engine's
    /// 16-bit answer, not snapped to the 8-bit grid.
    #[test]
    fn near_identity_levels_and_curves_through_the_menu_stay_at_sixteen_bits() {
        let cases = [
            (
                "Levels",
                adjustments::Adjustment::Levels(adjustments::Levels::composite(
                    adjustments::LevelsChannel::new(0.0, 1.0, 1.0)
                        .unwrap()
                        .with_output(0.0, 0.99)
                        .unwrap(),
                )),
            ),
            (
                "Curves",
                adjustments::Adjustment::Curves(adjustments::Curves::composite(
                    adjustments::Curve::new(&[[0.0, 0.0], [0.5, 0.51], [1.0, 1.0]]).unwrap(),
                )),
            ),
        ];
        for (name, adjustment) in cases {
            let dir = tempfile::tempdir().unwrap();
            let mut ed = editor(dir.path());
            let before = deep_document(&mut ed, busy_ramp);
            let space = ed.active().unwrap().document.meta.color_space.clone();
            let mut reference = filters::FilterBuffer::from_rgba16(64, 64, &before).unwrap();
            adjustments::PreparedAdjustment::new(&adjustment)
                .apply_premultiplied_rgba(reference.pixels_mut(), &space);
            let reference = reference.to_rgba16();
            crate::menu_bridge::run_adjustment_kind(&mut ed, &adjustment, name).unwrap();
            let after = active_layer_rgba16(&ed);
            assert_ne!(after, before, "{name} changed the layer");
            assert_eq!(after, reference, "{name}: the engine's 16-bit answer");
            let off = off_the_eight_bit_grid(&after);
            assert!(off > 0.9, "{name}: only {off} off the 8-bit grid");
        }
    }
}
