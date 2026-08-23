//! The bytes behind the hashes.
//!
//! `editor-core` deliberately holds no pixels: a layer's content is a sparse
//! `TileCoord -> TileHash` map and the bytes live in a content-addressed store.
//! A tool, however, *must* read pixels — a flood fill needs to know what colour
//! it is standing on, a clone stamp needs a source, a blur needs neighbours —
//! and it must produce new bytes and learn their hash so it can name them in a
//! [`editor_core::TileDelta`].
//!
//! [`TileAccess`] is that seam, and it is intentionally three methods wide:
//! resolve a reference, fetch bytes by hash, store bytes and get a hash back.
//! An application backs it with its real tile store; [`MemoryTiles`] backs it
//! with two hash maps, which is what the tests in this crate use and what a
//! headless render or a scripted batch job can use unchanged.

use std::collections::HashMap;

use editor_core::{PixelKey, PixelStore, TileDelta, TileMap};
use raster::{PixelFormat, Tile, TileCoord, TileHash};

/// Read pixel bytes, and store new ones under their content hash.
///
/// # Contract
/// * [`TileAccess::store`] is content-addressed: storing identical bytes twice
///   yields the same hash, and that hash is [`TileHash::of`] over exactly those
///   bytes — the same rule [`raster::Tile::hash`] follows, so a hash a tool
///   produces is one the tile store recognises.
/// * An absent reference means the zero tile: fully transparent for a layer,
///   zero coverage for a mask. A tool never has to distinguish "absent" from
///   "present and empty".
pub trait TileAccess {
    /// The hash currently referenced at `coord` for `key`, if any.
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash>;

    /// The bytes behind a hash, if this store holds them.
    fn bytes(&self, hash: TileHash) -> Option<&[u8]>;

    /// Store bytes, returning their content hash.
    fn store(&mut self, data: Vec<u8>) -> TileHash;

    /// The bytes currently at `coord`, resolving the reference first.
    fn tile_bytes(&self, key: PixelKey, coord: TileCoord) -> Option<&[u8]> {
        let hash = self.tile_hash(key, coord)?;
        self.bytes(hash)
    }
}

/// An in-memory [`TileAccess`]: a mirror of the document's tile references plus
/// the byte store they point into.
///
/// The references are a *mirror*, not the authority — [`editor_core::Document`]
/// owns those. [`MemoryTiles::sync_from`] refreshes them after a command has
/// been applied, which is exactly what an application does when its own store
/// observes a document change.
#[derive(Debug, Clone, Default)]
pub struct MemoryTiles {
    refs: HashMap<PixelKey, TileMap>,
    bytes: HashMap<TileHash, Vec<u8>>,
}

impl MemoryTiles {
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the reference mirror with a document's.
    ///
    /// Bytes are never dropped here: undo restores a *hash*, and the bytes
    /// behind it have to still be there for the restore to be exact.
    pub fn sync_from(&mut self, store: &PixelStore) {
        self.refs.clear();
        for key in store.keys() {
            if let Some(map) = store.tiles(key) {
                self.refs.insert(key, map.clone());
            }
        }
    }

    /// Apply a delta to the reference mirror, for callers driving the store
    /// without a full [`editor_core::Document`].
    pub fn apply_delta(&mut self, key: PixelKey, delta: &TileDelta) -> TileDelta {
        let map = self.refs.entry(key).or_default();
        let inverse = map.apply_delta(delta);
        if map.is_empty() {
            self.refs.remove(&key);
        }
        inverse
    }

    /// Seed content directly: store `data` and point `coord` at it.
    pub fn put(&mut self, key: PixelKey, coord: TileCoord, data: Vec<u8>) -> TileHash {
        let hash = self.store(data);
        self.refs
            .entry(key)
            .or_default()
            .apply_delta(&TileDelta::single(editor_core::TileEdit::set(coord, hash)));
        hash
    }

    /// Paint one straight-alpha sRGB8 pixel into a layer tile, creating the
    /// tile if it is absent. A convenience for building fixtures.
    pub fn put_pixel(&mut self, key: PixelKey, x: i64, y: i64, rgba: [u8; 4]) {
        let t = raster::TILE_SIZE as i64;
        let coord = TileCoord::new(x.div_euclid(t) as i32, y.div_euclid(t) as i32, 0);
        let mut data = self
            .tile_bytes(key, coord)
            .map(|b| b.to_vec())
            .unwrap_or_else(|| vec![0u8; Tile::byte_len(PixelFormat::Rgba8)]);
        let lx = x.rem_euclid(t) as usize;
        let ly = y.rem_euclid(t) as usize;
        let i = (ly * raster::TILE_SIZE as usize + lx) * 4;
        data[i..i + 4].copy_from_slice(&rgba);
        self.put(key, coord, data);
    }

    /// Read one straight-alpha sRGB8 pixel out of a layer tile.
    pub fn pixel(&self, key: PixelKey, x: i64, y: i64) -> [u8; 4] {
        let t = raster::TILE_SIZE as i64;
        let coord = TileCoord::new(x.div_euclid(t) as i32, y.div_euclid(t) as i32, 0);
        match self.tile_bytes(key, coord) {
            Some(b) => {
                let lx = x.rem_euclid(t) as usize;
                let ly = y.rem_euclid(t) as usize;
                let i = (ly * raster::TILE_SIZE as usize + lx) * 4;
                [b[i], b[i + 1], b[i + 2], b[i + 3]]
            }
            None => [0, 0, 0, 0],
        }
    }

    /// How many distinct byte blobs the store holds.
    pub fn blob_count(&self) -> usize {
        self.bytes.len()
    }

    /// The reference map of one target.
    pub fn refs(&self, key: PixelKey) -> Option<&TileMap> {
        self.refs.get(&key)
    }
}

impl TileAccess for MemoryTiles {
    fn tile_hash(&self, key: PixelKey, coord: TileCoord) -> Option<TileHash> {
        self.refs.get(&key).and_then(|m| m.get(coord))
    }

    fn bytes(&self, hash: TileHash) -> Option<&[u8]> {
        self.bytes.get(&hash).map(|v| v.as_slice())
    }

    fn store(&mut self, data: Vec<u8>) -> TileHash {
        let hash = TileHash::of(&data);
        self.bytes.entry(hash).or_insert(data);
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::TileEdit;
    use layer_model::LayerId;

    fn key() -> PixelKey {
        PixelKey::Layer(LayerId::new())
    }

    #[test]
    fn storing_identical_bytes_twice_yields_one_blob_and_the_tile_stores_hash() {
        let mut tiles = MemoryTiles::new();
        let data = vec![7u8; Tile::byte_len(PixelFormat::Rgba8)];
        let a = tiles.store(data.clone());
        let b = tiles.store(data.clone());
        assert_eq!(a, b);
        assert_eq!(tiles.blob_count(), 1);
        // And it is the hash `raster` computes for the same bytes, or a tool's
        // delta would name a tile the store cannot recognise.
        assert_eq!(
            a,
            Tile::from_bytes(PixelFormat::Rgba8, data).unwrap().hash()
        );
    }

    #[test]
    fn an_absent_reference_reads_as_nothing_rather_than_panicking() {
        let tiles = MemoryTiles::new();
        let k = key();
        assert!(tiles.tile_bytes(k, TileCoord::new(9, 9, 0)).is_none());
        assert_eq!(tiles.pixel(k, 100_000, -5), [0, 0, 0, 0]);
    }

    #[test]
    fn put_pixel_and_read_back_across_a_negative_tile_boundary() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        tiles.put_pixel(k, -1, -1, [1, 2, 3, 4]);
        tiles.put_pixel(k, 0, 0, [5, 6, 7, 8]);
        assert_eq!(tiles.pixel(k, -1, -1), [1, 2, 3, 4]);
        assert_eq!(tiles.pixel(k, 0, 0), [5, 6, 7, 8]);
        assert_eq!(tiles.pixel(k, -2, -1), [0, 0, 0, 0]);
    }

    #[test]
    fn the_reference_mirror_drops_a_target_whose_last_tile_goes_away() {
        let mut tiles = MemoryTiles::new();
        let k = key();
        let c = TileCoord::new(0, 0, 0);
        tiles.put(k, c, vec![1u8; Tile::byte_len(PixelFormat::Rgba8)]);
        assert!(tiles.refs(k).is_some());
        tiles.apply_delta(k, &TileDelta::single(TileEdit::clear(c)));
        assert!(tiles.refs(k).is_none());
        // The bytes survive, because an undo restores the hash.
        assert_eq!(tiles.blob_count(), 1);
    }

    #[test]
    fn sync_from_a_document_store_replaces_the_mirror() {
        let mut doc_store = PixelStore::default();
        let k = key();
        let mut tiles = MemoryTiles::new();
        let hash = tiles.store(vec![3u8; Tile::byte_len(PixelFormat::Rgba8)]);
        doc_store.apply(
            k,
            &TileDelta::single(TileEdit::set(TileCoord::new(2, 3, 0), hash)),
        );
        tiles.sync_from(&doc_store);
        assert_eq!(tiles.tile_hash(k, TileCoord::new(2, 3, 0)), Some(hash));
        assert!(tiles.tile_bytes(k, TileCoord::new(2, 3, 0)).is_some());
    }
}
