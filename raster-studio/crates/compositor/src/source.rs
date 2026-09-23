//! Where the compositor gets pixel bytes.
//!
//! An [`editor_core::Document`] holds *content hashes*, never bytes — that is
//! what makes it cheap to clone for history. A [`TileSource`] is the other half:
//! it turns a [`TileHash`] back into the bytes it names. The compositor asks for
//! exactly the tiles a region needs and treats a hash the source cannot resolve
//! as absent, which reads as fully transparent for a layer and as zero coverage
//! for a mask.

use std::collections::HashMap;
use std::sync::Arc;

use raster::{Tile, TileHash};

/// Resolves content hashes to tile bytes.
///
/// `Sync` is part of the contract, not an afterthought: the compositor fans
/// tiles out across a rayon pool and every worker reads the same source.
///
/// Implementations return the raw bytes for the hash, whatever storage shape
/// the caller stored under it — `TILE_SIZE² * 4` for an RGBA8 layer tile,
/// `TILE_SIZE²` for an 8-bit mask tile. The compositor checks the length it
/// needs before reading, so a source that mixes them up starves a layer rather
/// than reading out of bounds.
pub trait TileSource: Sync {
    /// Bytes stored under `hash`, or `None` when the source does not hold it.
    fn tile(&self, hash: TileHash) -> Option<&[u8]>;
}

impl<T: TileSource + ?Sized> TileSource for &T {
    fn tile(&self, hash: TileHash) -> Option<&[u8]> {
        (**self).tile(hash)
    }
}

/// A [`TileSource`] backed by an in-memory map. Keys are always
/// `TileHash::of(bytes)`, so a value can never be filed under a hash that does
/// not describe it.
///
/// Each tile is held as an immutable `Arc<[u8]>`: a tile is content-addressed,
/// so its bytes never change once filed, and a **clone of the source shares
/// them** instead of copying them. That is what makes the snapshot a save hands
/// its worker ([`Clone`] on the UI thread, once per save and autosave) cost a
/// map of pointers rather than a copy of every pixel the document has ever
/// held — which, for a large document, was a stall of the interaction thread
/// every autosave interval.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryTileSource {
    tiles: HashMap<TileHash, Arc<[u8]>>,
}

impl MemoryTileSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Store `bytes` under their own content hash and return it.
    pub fn insert_bytes(&mut self, bytes: Vec<u8>) -> TileHash {
        let hash = TileHash::of(&bytes);
        // Already filed: the bytes are the same by construction, and keeping
        // the existing allocation keeps it shared with every clone.
        self.tiles.entry(hash).or_insert_with(|| Arc::from(bytes));
        hash
    }

    /// Store a tile's pixel bytes under [`Tile::hash`].
    pub fn insert_tile(&mut self, tile: &Tile) -> TileHash {
        let hash = tile.hash();
        self.tiles
            .entry(hash)
            .or_insert_with(|| Arc::from(tile.data()));
        hash
    }

    pub fn contains(&self, hash: TileHash) -> bool {
        self.tiles.contains_key(&hash)
    }

    pub fn len(&self) -> usize {
        self.tiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

impl TileSource for MemoryTileSource {
    fn tile(&self, hash: TileHash) -> Option<&[u8]> {
        self.tiles.get(&hash).map(|b| &**b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster::PixelFormat;

    #[test]
    fn bytes_are_filed_under_their_own_hash() {
        let mut s = MemoryTileSource::new();
        assert!(s.is_empty());
        let h = s.insert_bytes(vec![7u8; 16]);
        assert_eq!(h, TileHash::of(&[7u8; 16]));
        assert_eq!(s.tile(h), Some(&[7u8; 16][..]));
        assert_eq!(s.len(), 1);
        assert!(s.contains(h));
    }

    #[test]
    fn a_tile_resolves_under_the_hash_the_tile_itself_reports() {
        let mut s = MemoryTileSource::new();
        let tile = Tile::transparent(PixelFormat::Rgba8);
        let h = s.insert_tile(&tile);
        assert_eq!(h, tile.hash());
        assert_eq!(s.tile(h).unwrap(), tile.data());
    }

    #[test]
    fn a_clone_shares_the_tile_bytes_rather_than_copying_them() {
        // W5-B: a save snapshots the document's source with `clone()` on the
        // UI thread; with owned `Vec`s that copied every pixel held.
        let mut s = MemoryTileSource::new();
        let h = s.insert_tile(&Tile::transparent(PixelFormat::Rgba16));
        let snapshot = s.clone();
        assert_eq!(snapshot, s);
        assert!(
            std::ptr::eq(s.tile(h).unwrap(), snapshot.tile(h).unwrap()),
            "the clone points at the same bytes"
        );
        // And the source stays independent: a new tile in one is not in the
        // other.
        let h2 = s.insert_bytes(vec![3u8; 8]);
        assert!(s.contains(h2) && !snapshot.contains(h2));
    }

    #[test]
    fn an_unknown_hash_is_absent_rather_than_an_error() {
        let s = MemoryTileSource::new();
        assert_eq!(s.tile(TileHash([9; 32])), None);
    }

    #[test]
    fn a_reference_is_itself_a_source() {
        fn take<S: TileSource>(s: S, h: TileHash) -> bool {
            s.tile(h).is_some()
        }
        let mut s = MemoryTileSource::new();
        let h = s.insert_bytes(vec![1, 2, 3]);
        assert!(take(&s, h));
    }
}
