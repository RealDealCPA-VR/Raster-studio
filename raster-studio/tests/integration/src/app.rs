//! Driving the application's own engine from a test.
//!
//! Everything here is either a *command builder* or a *seam bridge*. There is
//! deliberately no document, no history and no tile store of this crate's own:
//! those live in [`app_shell::doc::OpenDocument`], which is what the shipping
//! application opens, edits, composites, saves and exports through, and which
//! carries no window and no GPU handle. A test that rebuilds that state proves
//! its own rebuild works; these tests have to prove the product does.
//!
//! # The one seam that is not wired in the product yet
//!
//! [`DocTiles`] exists because `app-shell` does not yet run the `tools` crate's
//! pointer state machines: it holds a [`tools::BrushSettings`] and a selected
//! [`tools::ToolId`], but nothing in it constructs a [`tools::ToolContext`] or
//! feeds a [`tools::PointerEvent`] to a tool. So a stroke cannot be driven
//! "the way the application does", because the application does not do it yet.
//!
//! `DocTiles` is the smallest possible stand-in for the missing wiring: it
//! presents the open document's *real* byte store
//! ([`compositor::MemoryTileSource`], the one `OpenDocument` composites and
//! saves out of) through the [`tools::TileAccess`] trait, so a tool reads the
//! document's own pixels and writes new tiles straight into the document's own
//! store. The command the tool emits then goes through
//! [`OpenDocument::apply`] like every other edit. Nothing is mirrored and
//! nothing is copied; when `app-shell` grows a pointer route, it needs an
//! adapter of exactly this shape and this file can be deleted.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use app_shell::doc::{DocumentId, OpenDocument};
use color::ColorSpace;
use compositor::{Canvas, CompositeOptions, MemoryTileSource, TileSource};
use editor_core::{
    Command, LayerPatch, Patch, PixelKey, PixelStore, PixelTarget, TileEdit, TileMap,
    MASK_TILE_BYTES,
};
use layer_model::{Layer, LayerId, LayerMask, MaskId};
use raster::{PixelFormat, PixelRect, Tile, TileCoord, TileHash, TILE_SIZE};
use tools::TileAccess;

/// The version string a save records. The application passes its own; the value
/// only has to be stable so a reopened package can be compared with itself.
pub const APP_VERSION: &str = "integration-test";

/// Undo depth the tests open documents with — the application's preference
/// default is in the same range and nothing here depends on the exact number.
pub const HISTORY_DEPTH: usize = 100;

/// A fresh document identity. The application allocates these per tab; a test
/// only needs them to be distinct.
pub fn next_id() -> DocumentId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    DocumentId(NEXT.fetch_add(1, Ordering::Relaxed))
}

/// File ▸ New, through the application's own path.
pub fn blank(width: u32, height: u32, title: &str) -> OpenDocument {
    OpenDocument::blank(next_id(), width, height, title, HISTORY_DEPTH)
        .expect("a blank document of a sane size")
}

/// The same, with the working space set to linear.
///
/// In a linear document an 8-bit code `v` decodes to exactly `v / 255` and
/// re-encodes to exactly `v`, so a compositing reference can be computed on
/// paper and compared as bytes without a transfer curve in the way.
///
/// The working space is written straight onto the document because there is no
/// command for it: `editor-core` has no `SetColorSpace`, and the application
/// picks the space when the document is created rather than editing it later.
/// It is set before any edit, so nothing undoable depends on it.
pub fn linear(width: u32, height: u32, title: &str) -> OpenDocument {
    let mut doc = blank(width, height, title);
    doc.document.meta.color_space = ColorSpace::LinearSrgb;
    doc
}

/// File ▸ Open, through the application's own path.
pub fn open_image(path: &std::path::Path) -> OpenDocument {
    OpenDocument::open_image(next_id(), path, HISTORY_DEPTH).expect("the image opens")
}

/// Reopen a `.rstudio` package, through the application's own path.
pub fn open_project(path: &std::path::Path) -> OpenDocument {
    OpenDocument::open_project(next_id(), path, HISTORY_DEPTH).expect("the package opens")
}

/// Command builders and read-backs the tests share.
///
/// Every method here ends in [`OpenDocument::apply`]: the document is never
/// mutated directly, which is what keeps undo and redo uniform.
pub trait DocExt {
    /// Every level-0 tile coordinate the canvas touches.
    fn canvas_tiles(&self) -> Vec<TileCoord>;
    /// Add a layer at the document root — the *top* of the stack, so the bottom
    /// layer of a fixture is added first.
    fn add_layer(&mut self, layer: Layer) -> LayerId;
    /// Add a layer as the top-most child of `parent`, in one transaction.
    fn add_child(&mut self, parent: LayerId, layer: Layer) -> LayerId;
    /// Attach a fresh, fully-enabled raster mask to a layer.
    fn attach_mask(&mut self, layer: LayerId) -> MaskId;
    /// Change layer properties.
    fn set_props(&mut self, layer: LayerId, patch: LayerPatch);
    /// Paint whole tiles of a layer from a function of tile-local coordinates,
    /// as one command.
    fn paint_layer(
        &mut self,
        layer: LayerId,
        coords: &[TileCoord],
        f: &dyn Fn(TileCoord, u32, u32) -> [u8; 4],
    );
    /// Fill every tile the canvas covers with one straight-alpha colour.
    fn fill_layer(&mut self, layer: LayerId, rgba: [u8; 4]);
    /// Paint whole tiles of a layer's mask. A mask tile is one byte per pixel.
    fn paint_mask(
        &mut self,
        layer: LayerId,
        coords: &[TileCoord],
        f: &dyn Fn(TileCoord, u32, u32) -> u8,
    );
    /// The whole canvas, composited through the cache the application paints
    /// with, encoded the way the screen and every export see it.
    fn composite_all(&mut self) -> Vec<u8>;
    /// The same region composited with no cache at all — the independent
    /// answer a cached frame has to agree with.
    fn composite_uncached(&self, region: PixelRect) -> Canvas;
    /// The bytes behind a hash, out of the document's own store.
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]>;
    /// Present the document's byte store to a `tools` state machine.
    fn tool_tiles(&mut self) -> DocTiles<'_>;
}

impl DocExt for OpenDocument {
    fn canvas_tiles(&self) -> Vec<TileCoord> {
        let mut out = Vec::new();
        for ty in 0..self.document.height().div_ceil(TILE_SIZE) {
            for tx in 0..self.document.width().div_ceil(TILE_SIZE) {
                out.push(TileCoord::new(tx as i32, ty as i32, 0));
            }
        }
        out
    }

    fn add_layer(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.apply(Command::create_layer(layer)).expect("create");
        id
    }

    fn add_child(&mut self, parent: LayerId, layer: Layer) -> LayerId {
        let id = layer.id;
        // One transaction, because a create followed by a move is two edits the
        // user thinks of as one — and because a tree can only take a group
        // empty, so this is the shape every "put it in the group" has.
        self.apply(Command::Transaction {
            label: "Add layer to group".into(),
            commands: vec![
                Command::create_layer(layer),
                Command::MoveLayer {
                    layer_id: id,
                    parent: Some(parent),
                    index: 0,
                },
            ],
        })
        .expect("create into a group");
        id
    }

    fn attach_mask(&mut self, layer: LayerId) -> MaskId {
        let mask = MaskId::new();
        self.set_props(
            layer,
            LayerPatch {
                mask: Patch::Set(LayerMask::new(mask)),
                ..Default::default()
            },
        );
        mask
    }

    fn set_props(&mut self, layer: LayerId, patch: LayerPatch) {
        self.apply(Command::SetLayerProperties {
            layer_id: layer,
            patch,
        })
        .expect("property change");
    }

    fn paint_layer(
        &mut self,
        layer: LayerId,
        coords: &[TileCoord],
        f: &dyn Fn(TileCoord, u32, u32) -> [u8; 4],
    ) {
        let mut edits = Vec::with_capacity(coords.len());
        for &coord in coords {
            let mut bytes = Vec::with_capacity(Tile::byte_len(PixelFormat::Rgba8));
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    bytes.extend_from_slice(&f(coord, x, y));
                }
            }
            edits.push(TileEdit::set(coord, self.tiles.insert_bytes(bytes)));
        }
        let cmd = Command::paint_tiles(PixelTarget::Layer(layer), edits).expect("distinct coords");
        self.apply(cmd).expect("paint");
    }

    fn fill_layer(&mut self, layer: LayerId, rgba: [u8; 4]) {
        let coords = self.canvas_tiles();
        self.paint_layer(layer, &coords, &move |_, _, _| rgba);
    }

    fn paint_mask(
        &mut self,
        layer: LayerId,
        coords: &[TileCoord],
        f: &dyn Fn(TileCoord, u32, u32) -> u8,
    ) {
        let mut edits = Vec::with_capacity(coords.len());
        for &coord in coords {
            let mut bytes = Vec::with_capacity(MASK_TILE_BYTES);
            for y in 0..TILE_SIZE {
                for x in 0..TILE_SIZE {
                    bytes.push(f(coord, x, y));
                }
            }
            edits.push(TileEdit::set(coord, self.tiles.insert_bytes(bytes)));
        }
        // The command targets the *layer*; `editor-core` resolves it to
        // whichever mask is attached.
        let cmd = Command::paint_tiles(PixelTarget::Mask(layer), edits).expect("distinct coords");
        self.apply(cmd).expect("paint mask");
    }

    fn composite_all(&mut self) -> Vec<u8> {
        let rect = self.canvas_rect();
        self.composite(rect).expect("the canvas composites")
    }

    fn composite_uncached(&self, region: PixelRect) -> Canvas {
        compositor::composite_region(
            &self.document,
            &self.tiles,
            region,
            0,
            CompositeOptions::default(),
        )
        .expect("the region composites")
    }

    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]> {
        TileSource::tile(&self.tiles, hash)
    }

    fn tool_tiles(&mut self) -> DocTiles<'_> {
        let refs = mirror(&self.document.pixels);
        DocTiles {
            refs,
            bytes: &mut self.tiles,
        }
    }
}

/// A snapshot of a document's tile references, keyed the way a tool asks.
fn mirror(store: &PixelStore) -> HashMap<PixelKey, TileMap> {
    let mut out = HashMap::new();
    for key in store.keys() {
        if let Some(map) = store.tiles(key) {
            out.insert(key, map.clone());
        }
    }
    out
}

/// The open document's byte store, seen through the `tools` crate's trait.
///
/// The references are a *snapshot* — [`editor_core::Document`] owns those, and
/// a tool only reads them. The bytes are the document's own store, borrowed
/// mutably, so a tile a tool produces is already where the compositor and the
/// package writer will look for it: there is no second store to keep in step
/// and no copy step that could be forgotten.
///
/// Being a borrow, this cannot be alive while the document is applying a
/// command — which is the right shape anyway: a tool produces a command, and
/// the command is applied afterwards.
pub struct DocTiles<'a> {
    refs: HashMap<PixelKey, TileMap>,
    bytes: &'a mut MemoryTileSource,
}

impl TileAccess for DocTiles<'_> {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.refs.get(&key).and_then(|m| m.get(coord))
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        TileSource::tile(self.bytes, hash)
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        self.bytes.insert_bytes(data)
    }
}
