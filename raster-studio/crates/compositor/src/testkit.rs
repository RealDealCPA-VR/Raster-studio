//! Document builders shared by this crate's tests.
//!
//! Compiled only under `cfg(test)`. Everything here is deliberately explicit —
//! a test fixture that quietly does something clever is a test that proves
//! something other than what it claims.

use color::ColorSpace;
use editor_core::{Document, PixelKey, TileDelta, TileEdit};
use layer_model::{
    AdjustmentKind, AdjustmentLayer, GroupBlending, Layer, LayerId, LayerKind, LayerMask, MaskId,
};
use raster::{TileCoord, TileHash, TILE_SIZE};

use crate::source::MemoryTileSource;

/// A document under construction plus the tile bytes behind it.
pub(crate) struct TestDoc {
    pub doc: Document,
    pub src: MemoryTileSource,
}

impl TestDoc {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            doc: Document::new(width, height, "test"),
            src: MemoryTileSource::new(),
        }
    }

    /// A document whose working space is linear, so an 8-bit code `v` decodes
    /// to exactly `v / 255` and reference values can be written by hand.
    pub fn linear(width: u32, height: u32) -> Self {
        let mut t = Self::new(width, height);
        t.doc.meta.color_space = ColorSpace::LinearSrgb;
        t
    }

    pub fn finish(self) -> (Document, MemoryTileSource) {
        (self.doc, self.src)
    }

    /// Add a layer at the top of the root list. Call bottom layer first.
    pub fn push(&mut self, layer: Layer) -> LayerId {
        self.doc.layers.push_root(layer).expect("push_root")
    }

    /// Add a layer at the top of `parent`'s children. Call bottom child first.
    pub fn push_child(&mut self, parent: LayerId, layer: Layer) -> LayerId {
        self.doc
            .layers
            .insert_at(layer, Some(parent), 0)
            .expect("insert_at")
    }

    pub fn push_raster(&mut self, name: &str) -> LayerId {
        self.push(Layer::raster(name))
    }

    pub fn push_group(&mut self, name: &str) -> LayerId {
        self.push(Layer::group(name))
    }

    pub fn push_adjustment(&mut self, name: &str, kind: AdjustmentKind) -> LayerId {
        self.push(Layer::with_kind(
            name,
            LayerKind::Adjustment(AdjustmentLayer { kind }),
        ))
    }

    pub fn set_group_blending(&mut self, id: LayerId, blending: GroupBlending) {
        let layer = self.doc.layers.get_mut(id).expect("layer");
        match &mut layer.kind {
            LayerKind::Group(g) => g.blending = blending,
            other => panic!("{other:?} is not a group"),
        }
    }

    /// Fill every tile covering the document with one straight-alpha sRGB
    /// colour.
    pub fn fill(&mut self, id: LayerId, rgba: [u8; 4]) {
        let (w, h) = (self.doc.width(), self.doc.height());
        for ty in 0..h.div_ceil(TILE_SIZE) {
            for tx in 0..w.div_ceil(TILE_SIZE) {
                self.paint_tile(id, TileCoord::new(tx as i32, ty as i32, 0), rgba);
            }
        }
    }

    /// Fill one tile with a solid colour.
    pub fn paint_tile(&mut self, id: LayerId, coord: TileCoord, rgba: [u8; 4]) {
        self.paint_tile_with(id, coord, |_, _| rgba);
    }

    /// Paint one tile from a function of tile-local coordinates.
    pub fn paint_tile_with(
        &mut self,
        id: LayerId,
        coord: TileCoord,
        f: impl Fn(u32, u32) -> [u8; 4],
    ) {
        let mut bytes = Vec::with_capacity(TILE_SIZE as usize * TILE_SIZE as usize * 4);
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                bytes.extend_from_slice(&f(x, y));
            }
        }
        let hash = self.src.insert_bytes(bytes);
        self.set_tile_hash(id, coord, hash);
    }

    /// Point a layer's tile at a hash without storing bytes for it.
    pub fn set_tile_hash(&mut self, id: LayerId, coord: TileCoord, hash: TileHash) {
        self.doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(coord, hash)),
        );
    }

    /// Attach a fresh, fully-enabled mask to a layer.
    pub fn attach_mask(&mut self, id: LayerId) -> MaskId {
        let mask_id = MaskId::new();
        self.doc
            .layers
            .get_mut(id)
            .expect("layer")
            .set_mask(LayerMask::new(mask_id));
        mask_id
    }

    /// Paint one mask tile with a constant coverage byte.
    pub fn paint_mask_tile(&mut self, mask: MaskId, coord: TileCoord, value: u8) {
        self.paint_mask_with(mask, coord, |_, _| value);
    }

    /// Paint one mask tile from a function of tile-local coordinates.
    pub fn paint_mask_with(&mut self, mask: MaskId, coord: TileCoord, f: impl Fn(u32, u32) -> u8) {
        let mut bytes = Vec::with_capacity(editor_core::MASK_TILE_BYTES);
        for y in 0..TILE_SIZE {
            for x in 0..TILE_SIZE {
                bytes.push(f(x, y));
            }
        }
        let hash = self.src.insert_bytes(bytes);
        self.doc.pixels.apply(
            PixelKey::Mask(mask),
            &TileDelta::single(TileEdit::set(coord, hash)),
        );
    }
}

/// A raster layer covering the whole document in one solid colour.
pub(crate) fn solid_layer(t: &mut TestDoc, name: &str, rgba: [u8; 4]) -> LayerId {
    let id = t.push_raster(name);
    t.fill(id, rgba);
    id
}
