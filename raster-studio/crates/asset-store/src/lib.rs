//! Content-addressed blob store for tiles and assets.
//!
//! Everything is keyed by BLAKE3 hash, so identical tiles/assets are stored
//! once (deduplication) and change-detection is a hash comparison. This backs
//! both the in-memory tile cache and the on-disk `.rstudio/tiles` directory.
//!
//! The in-memory [`AssetStore`] here is the logical model; a disk-backed and a
//! GPU-LRU layer are built on top in later phases (see `docs/render-pipeline.md`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A content hash (BLAKE3) used as a blob key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    pub fn of(bytes: &[u8]) -> Self {
        BlobHash(*blake3::hash(bytes).as_bytes())
    }
    pub fn to_hex(self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Whether an asset's bytes are stored inside the project or referenced from an
/// external path (a "linked" asset the user can update on disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AssetSource {
    Embedded,
    Linked { path: String },
}

/// A record describing one asset (image, ICC profile, mask, AI output...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssetRecord {
    pub hash: BlobHash,
    pub mime: String,
    pub source: AssetSource,
    pub byte_len: u64,
}

/// In-memory content-addressed store with reference counting for GC.
#[derive(Default)]
pub struct AssetStore {
    blobs: HashMap<BlobHash, Vec<u8>>,
    refcount: HashMap<BlobHash, u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("blob {0} not found")]
    NotFound(String),
}

impl AssetStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert bytes, returning their content hash. Idempotent: inserting the
    /// same bytes twice stores once but increments the refcount.
    pub fn put(&mut self, bytes: Vec<u8>) -> BlobHash {
        let hash = BlobHash::of(&bytes);
        self.blobs.entry(hash).or_insert(bytes);
        *self.refcount.entry(hash).or_insert(0) += 1;
        hash
    }

    pub fn get(&self, hash: BlobHash) -> Option<&[u8]> {
        self.blobs.get(&hash).map(|v| v.as_slice())
    }

    pub fn contains(&self, hash: BlobHash) -> bool {
        self.blobs.contains_key(&hash)
    }

    /// Drop one reference; removes the blob when the count hits zero.
    pub fn release(&mut self, hash: BlobHash) {
        if let Some(c) = self.refcount.get_mut(&hash) {
            *c = c.saturating_sub(1);
            if *c == 0 {
                self.refcount.remove(&hash);
                self.blobs.remove(&hash);
            }
        }
    }

    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_same_bytes() {
        let mut s = AssetStore::new();
        let h1 = s.put(b"hello world".to_vec());
        let h2 = s.put(b"hello world".to_vec());
        assert_eq!(h1, h2);
        assert_eq!(s.len(), 1, "identical blobs stored once");
    }

    #[test]
    fn refcount_gc() {
        let mut s = AssetStore::new();
        let h = s.put(b"data".to_vec());
        let _ = s.put(b"data".to_vec()); // refcount = 2
        s.release(h);
        assert!(s.contains(h), "still referenced");
        s.release(h);
        assert!(!s.contains(h), "gc'd at zero refs");
    }

    #[test]
    fn hash_hex_len() {
        assert_eq!(BlobHash::of(b"x").to_hex().len(), 64);
    }
}
