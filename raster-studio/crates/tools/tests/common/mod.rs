//! Shared fixture for the tool integration tests: a real `Document`, a real
//! content-addressed tile store, and the plumbing that keeps the two in step
//! the way an application would.

use editor_core::{Command, Document, PixelKey, PixelTarget, Selection};
use layer_model::{Layer, LayerId};
use raster::{PixelRect, TILE_SIZE};
use tools::tiles::{MemoryTiles, TileAccess};
use tools::tool::{PointerEvent, Tool, ToolContext};

/// A document with one raster layer, and the tile store that backs it.
pub struct Fixture {
    pub doc: Document,
    pub tiles: MemoryTiles,
    pub layer: LayerId,
}

pub fn fixture(w: u32, h: u32) -> Fixture {
    let mut doc = Document::new(w, h, "test");
    let layer = Layer::raster("paint");
    let id = layer.id;
    Command::create_layer(layer).apply(&mut doc).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    Fixture {
        doc,
        tiles: MemoryTiles::new(),
        layer: id,
    }
}

impl Fixture {
    pub fn key(&self) -> PixelKey {
        PixelKey::Layer(self.layer)
    }

    pub fn canvas(&self) -> PixelRect {
        PixelRect::new(0, 0, self.doc.width(), self.doc.height())
    }

    /// Fill a rectangle of the layer with one straight sRGB8 colour.
    ///
    /// Written tile at a time rather than pixel at a time: a tile is 256 KiB
    /// and the per-pixel helper rewrites the whole thing on every call.
    pub fn paint_rect(&mut self, r: PixelRect, rgba: [u8; 4]) {
        let key = self.key();
        let t = TILE_SIZE as i64;
        let ts = TILE_SIZE as usize;
        for ty in r.y.div_euclid(t)..=(r.bottom() - 1).div_euclid(t) {
            for tx in r.x.div_euclid(t)..=(r.right() - 1).div_euclid(t) {
                let coord = raster::TileCoord::new(tx as i32, ty as i32, 0);
                let mut data = self
                    .tiles
                    .tile_bytes(key, coord)
                    .map(|b| b.to_vec())
                    .unwrap_or_else(|| vec![0u8; ts * ts * 4]);
                for ly in 0..ts {
                    let y = ty * t + ly as i64;
                    if y < r.y || y >= r.bottom() {
                        continue;
                    }
                    for lx in 0..ts {
                        let x = tx * t + lx as i64;
                        if x < r.x || x >= r.right() {
                            continue;
                        }
                        let i = (ly * ts + lx) * 4;
                        data[i..i + 4].copy_from_slice(&rgba);
                    }
                }
                self.tiles.put(key, coord, data);
            }
        }
        // The document has to reference the tiles the fixture just wrote, or
        // the tools would see them and the commands would not.
        let mut edits = Vec::new();
        for (coord, hash) in tile_hashes(&self.tiles, key, r) {
            edits.push(editor_core::TileEdit::set(coord, hash));
        }
        if !edits.is_empty() {
            Command::paint_tiles(PixelTarget::Layer(self.layer), edits)
                .unwrap()
                .apply(&mut self.doc)
                .unwrap();
        }
        self.tiles.sync_from(&self.doc.pixels);
    }

    pub fn pixel(&self, x: i64, y: i64) -> [u8; 4] {
        self.tiles.pixel(self.key(), x, y)
    }

    /// Apply everything a gesture queued, and refresh the store's reference
    /// mirror the way an application would.
    pub fn commit(&mut self, cmds: Vec<Command>) -> Vec<Command> {
        let mut inverses = Vec::new();
        for c in cmds {
            inverses.push(c.apply(&mut self.doc).unwrap());
        }
        self.tiles.sync_from(&self.doc.pixels);
        inverses
    }
}

pub fn tile_hashes(
    tiles: &MemoryTiles,
    key: PixelKey,
    r: PixelRect,
) -> Vec<(raster::TileCoord, raster::TileHash)> {
    let t = TILE_SIZE as i64;
    let mut out = Vec::new();
    for ty in r.y.div_euclid(t)..=(r.bottom() - 1).div_euclid(t) {
        for tx in r.x.div_euclid(t)..=(r.right() - 1).div_euclid(t) {
            let coord = raster::TileCoord::new(tx as i32, ty as i32, 0);
            if let Some(h) = tiles.tile_hash(key, coord) {
                out.push((coord, h));
            }
        }
    }
    out
}

/// Run a whole stroke and hand back what the tool queued.
pub fn stroke(
    fx: &mut Fixture,
    tool: &mut dyn Tool,
    path: &[(f32, f32, f32)],
    fg: [f32; 4],
    selection: Selection,
) -> Vec<Command> {
    let layer = fx.layer;
    let canvas = fx.canvas();
    let mut ctx = ToolContext::new(&mut fx.tiles, canvas).with_layer(layer);
    ctx.foreground = fg;
    ctx.selection = selection;
    let (x, y, p) = path[0];
    tool.on_pointer_down(&mut ctx, PointerEvent::at(x, y).with_pressure(p))
        .unwrap();
    for (x, y, p) in &path[1..path.len() - 1] {
        tool.on_pointer_move(&mut ctx, PointerEvent::at(*x, *y).with_pressure(*p))
            .unwrap();
    }
    let (x, y, p) = *path.last().unwrap();
    tool.on_pointer_up(&mut ctx, PointerEvent::at(x, y).with_pressure(p))
        .unwrap();
    ctx.drain()
}

pub const RED: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
pub const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

pub fn line(from: (f32, f32), to: (f32, f32), steps: usize) -> Vec<(f32, f32, f32)> {
    (0..=steps)
        .map(|i| {
            let t = i as f32 / steps as f32;
            (
                from.0 + (to.0 - from.0) * t,
                from.1 + (to.1 - from.1) * t,
                1.0,
            )
        })
        .collect()
}
