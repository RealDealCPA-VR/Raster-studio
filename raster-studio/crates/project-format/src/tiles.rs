//! Persisting the pixels.
//!
//! # The bug this module exists to fix
//!
//! `save_project` used to create `tiles/`, `assets/` and `previews/` and leave
//! all three **empty**. An [`editor_core::Document`] holds tile *hashes*, never
//! bytes, so a package with no tile blobs is a package with no pixels: paint a
//! canvas, save, reopen, and every stroke is gone. The document round-tripped
//! perfectly, which is what made it look like it worked.
//!
//! # Layout
//!
//! ```text
//! tiles/<first two hex digits>/<64 hex digits>.tile
//! ```
//!
//! The name **is** the BLAKE3 of the file's bytes ([`raster::TileHash`]), so:
//!
//! * identical tiles are stored once, however many layers, masks, mip levels or
//!   history states reference them — a flat fill across a layer is one blob;
//! * a blob is self-verifying. Every read re-hashes and compares against the
//!   name, which is why tile blobs are *not* listed in the manifest's digest
//!   table: the check is inherent, and listing them would make the manifest
//!   grow with the pixel count.
//!
//! The two-digit shard keeps directories to a few thousand entries on
//! filesystems that get slow with a hundred thousand.
//!
//! # Bounds, in both directions
//!
//! Both the tile count and the total byte volume are capped
//! ([`MAX_PACKAGE_TILES`], [`MAX_TILE_DATA_BYTES`]), and a single blob may not
//! exceed the largest tile this format stores ([`MAX_TILE_BYTES`]). The document
//! naming those tiles came out of the package too, so the counts it implies are
//! as untrusted as the files themselves.
//!
//! All three apply on the way **out** as well, from one [`TileCaps`]: a package
//! with more in it than this reader will load is a package this writer must not
//! produce. The count always did; the two byte bounds did not, and the shape
//! that leaves — a save that returns `Ok` and a package that never opens again —
//! is the one [`crate::assets`] had to be fixed for twice.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use asset_store::AssetStore;
use editor_core::Document;
use raster::TileHash;

use crate::atomic::write_and_sync;
use crate::error::ProjectError;
use crate::{hexid, safepath};

/// Directory holding content-addressed tile blobs.
pub const TILES_DIR: &str = "tiles";

/// Largest a single tile blob may be: one `TILE_SIZE²` RGBA8 tile.
///
/// A mask tile is a quarter of this, and nothing this format writes today is
/// larger, so a blob that claims more is refused before it is read — and refused
/// before it is *written*, which matters the day one is: a
/// [`raster::PixelFormat::Rgba16`] tile is twice this and an
/// [`raster::PixelFormat::RgbaF32`] tile four times, so whoever brings those
/// through [`TileBytes`] raises this one number and both sides move with it,
/// rather than shipping saves that never reopen.
pub const MAX_TILE_BYTES: u64 = (raster::TILE_SIZE as u64) * (raster::TILE_SIZE as u64) * 4;

/// Most distinct tiles one package may reference.
pub const MAX_PACKAGE_TILES: u64 = 1 << 20;

/// Most tile bytes one package may load into memory.
///
/// Not implied by the other two: their product is 256 GiB.
pub const MAX_TILE_DATA_BYTES: u64 = 8 << 30;

/// Every bound the tile path applies — **in both directions**.
///
/// [`write_tiles`] and [`read_tiles`] each build this from the constants above,
/// so each bound is one number rather than a writer's number and a reader's
/// number that can drift apart. See [`crate::assets::AssetCaps`], which exists
/// for the same reason and was written after the drift had already cost a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileCaps {
    /// Largest single blob: [`MAX_TILE_BYTES`].
    pub tile: u64,
    /// Most distinct tiles: [`MAX_PACKAGE_TILES`].
    pub count: u64,
    /// Largest total: [`MAX_TILE_DATA_BYTES`].
    pub data: u64,
}

impl Default for TileCaps {
    /// The constants — what every non-test call runs at.
    fn default() -> Self {
        Self {
            tile: MAX_TILE_BYTES,
            count: MAX_PACKAGE_TILES,
            data: MAX_TILE_DATA_BYTES,
        }
    }
}

/// Resolves a tile content hash to its bytes.
///
/// This is the write side of [`compositor::TileSource`]: saving needs the same
/// hash → bytes lookup the compositor needs, and the caller already has one.
/// `Sync` is required because the preview renderer hands it to the compositor,
/// which reads it from a rayon pool.
pub trait TileBytes: Sync {
    /// Bytes stored under `hash`, or `None` when this source does not hold it.
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]>;
}

impl<T: TileBytes + ?Sized> TileBytes for &T {
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]> {
        (**self).tile_bytes(hash)
    }
}

// Deliberately **not** implemented for [`asset_store::AssetStore`]. Its `get`
// hands back an `Arc<[u8]>` (it is an LRU over a disk backend, so a blob may
// have to be read in before it can be returned), and there is no way to produce
// an `Option<&[u8]>` borrowed from `&self` out of that. The load side puts
// blobs *into* a store rather than reading them through this trait, and
// [`crate::LoadedProject::tile_source`] is the documented bridge back to the
// compositor.

impl TileBytes for compositor::MemoryTileSource {
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]> {
        compositor::TileSource::tile(self, hash)
    }
}

impl TileBytes for std::collections::HashMap<TileHash, Vec<u8>> {
    fn tile_bytes(&self, hash: TileHash) -> Option<&[u8]> {
        self.get(&hash).map(Vec::as_slice)
    }
}

/// A source that holds nothing.
///
/// Used by the no-tile-source convenience wrapper. Saving a document that *does*
/// reference tiles through this fails with [`ProjectError::MissingTile`] rather
/// than writing a package with no pixels in it — losing the pixels quietly is
/// the bug this module was written to remove, so the degenerate source is loud.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTiles;

impl TileBytes for NoTiles {
    fn tile_bytes(&self, _hash: TileHash) -> Option<&[u8]> {
        None
    }
}

/// Adapts any [`TileBytes`] to the compositor's [`compositor::TileSource`].
pub(crate) struct AsTileSource<'a>(pub(crate) &'a dyn TileBytes);

impl compositor::TileSource for AsTileSource<'_> {
    fn tile(&self, hash: TileHash) -> Option<&[u8]> {
        self.0.tile_bytes(hash)
    }
}

/// What a save wrote to `tiles/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileReport {
    /// Distinct blobs written.
    pub blobs_written: usize,
    /// Total bytes of tile data in the package.
    pub bytes_written: u64,
    /// References that resolved to a blob already written — the dedup win.
    pub references_deduplicated: usize,
}

/// Package-relative path of one tile blob.
fn blob_rel(hash: TileHash) -> (String, String) {
    let hex = hexid::to_hex(&hash.0);
    (hex[..2].to_string(), hex)
}

fn blob_path(root: &Path, hash: TileHash) -> PathBuf {
    let (shard, hex) = blob_rel(hash);
    root.join(TILES_DIR).join(shard).join(format!("{hex}.tile"))
}

/// Every distinct tile hash the document references, layers and masks alike —
/// or [`ProjectError::TooManyTiles`] as soon as there are more than `max` of
/// them.
///
/// The cap is enforced **while collecting**, not after. The document naming
/// these tiles came out of the package, so its reference count is chosen by
/// whoever wrote the file; checking `out.len()` afterwards would mean the
/// allocation the cap exists to bound had already happened. `count` in the
/// error is therefore the point at which collecting stopped (`max + 1`), not a
/// total — the total is exactly what is never computed.
fn referenced(doc: &Document, max: u64) -> Result<(Vec<TileHash>, usize), ProjectError> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    let mut references = 0usize;
    for key in doc.pixels.keys() {
        let Some(map) = doc.pixels.tiles(key) else {
            continue;
        };
        for (_, hash) in map.iter() {
            references += 1;
            if seen.insert(hash) {
                if seen.len() as u64 > max {
                    return Err(ProjectError::TooManyTiles {
                        count: seen.len() as u64,
                        max,
                    });
                }
                out.push(hash);
            }
        }
    }
    // Deterministic order so two saves of the same document do the same work.
    out.sort_by_key(|h| h.0);
    Ok((out, references))
}

/// Write every tile the document references into `root/tiles`.
///
/// Capped on the way out as well as the way in: a package with more in it than
/// this reader will load is a package this writer must not produce. See
/// [`TileCaps`].
pub(crate) fn write_tiles(
    root: &Path,
    doc: &Document,
    tiles: &dyn TileBytes,
) -> Result<TileReport, ProjectError> {
    write_tiles_capped(root, doc, tiles, TileCaps::default())
}

/// [`write_tiles`] with the caps as a parameter, so a test can reach them
/// without building a million-tile document or eight gigabytes of pixels.
///
/// `caps` must be the caps the *load* will run at.
pub(crate) fn write_tiles_capped(
    root: &Path,
    doc: &Document,
    tiles: &dyn TileBytes,
    caps: TileCaps,
) -> Result<TileReport, ProjectError> {
    let (hashes, references) = referenced(doc, caps.count)?;
    let mut report = TileReport {
        references_deduplicated: references.saturating_sub(hashes.len()),
        ..TileReport::default()
    };
    let mut shards: HashSet<String> = HashSet::new();
    let mut total = 0u64;
    for hash in hashes {
        let bytes = tiles
            .tile_bytes(hash)
            .ok_or_else(|| ProjectError::MissingTile {
                hash: hexid::to_hex(&hash.0),
            })?;
        // A source that files bytes under the wrong hash would produce a
        // package whose blobs fail their own verification on load. Catch it
        // here, where the caller can still be told which tile.
        if TileHash::of(bytes) != hash {
            return Err(ProjectError::CorruptBlob {
                path: format!("{TILES_DIR}/{}.tile", hexid::to_hex(&hash.0)),
            });
        }
        let (shard, hex) = blob_rel(hash);
        // The two byte bounds, before the write rather than after: the loader
        // refuses a blob over `caps.tile` and a package over `caps.data`, so
        // writing either would hand the user a project that saves and never
        // opens.
        if bytes.len() as u64 > caps.tile {
            return Err(ProjectError::PackageFileTooLarge {
                path: format!("{TILES_DIR}/{shard}/{hex}.tile"),
                size: bytes.len() as u64,
                max: caps.tile,
            });
        }
        total = total.saturating_add(bytes.len() as u64);
        if total > caps.data {
            return Err(ProjectError::TileDataTooLarge { max: caps.data });
        }
        if shards.insert(shard.clone()) {
            std::fs::create_dir_all(root.join(TILES_DIR).join(&shard))?;
        }
        write_and_sync(&blob_path(root, hash), bytes)?;
        report.blobs_written += 1;
        report.bytes_written += bytes.len() as u64;
    }
    Ok(report)
}

/// Load every tile the document references into `store`.
///
/// Each blob is re-hashed and compared against the name it is filed under, so a
/// package cannot hand back bytes that are not the pixels the document asked
/// for.
pub(crate) fn read_tiles(
    root: &Path,
    doc: &Document,
    store: &mut AssetStore,
) -> Result<usize, ProjectError> {
    read_tiles_capped(root, doc, store, TileCaps::default())
}

/// [`read_tiles`] with the caps as a parameter, so a test can reach them without
/// building a million-tile document.
pub(crate) fn read_tiles_capped(
    root: &Path,
    doc: &Document,
    store: &mut AssetStore,
    caps: TileCaps,
) -> Result<usize, ProjectError> {
    let (hashes, _) = referenced(doc, caps.count)?;
    let mut total = 0u64;
    for hash in &hashes {
        let (shard, hex) = blob_rel(*hash);
        let rel = format!("{TILES_DIR}/{shard}/{hex}.tile");
        // Built from a 64-hex-digit name we produced ourselves, but routed
        // through the same check as anything else so there is exactly one way
        // a package-relative path becomes a real one.
        let path = safepath::safe_join(root, &rel, "tile")?;
        let bytes = safepath::read_capped(&path, &rel, caps.tile)?;
        total = total.saturating_add(bytes.len() as u64);
        if total > caps.data {
            return Err(ProjectError::TileDataTooLarge { max: caps.data });
        }
        if TileHash::of(&bytes) != *hash {
            return Err(ProjectError::CorruptBlob { path: rel });
        }
        // `TileHash` and `BlobHash` are both BLAKE3 over the same bytes, so the
        // two addressing schemes are one addressing scheme: the store files it
        // under the very hash the document names.
        store.put(&bytes)?;
    }
    Ok(hashes.len())
}

/// A fully opaque RGBA8 tile of one colour — the shape of every tile blob this
/// format writes for a raster layer. Exposed for tests in sibling modules.
#[cfg(test)]
pub(crate) fn solid_tile(rgba: [u8; 4]) -> Vec<u8> {
    let px = raster::TILE_SIZE as usize * raster::TILE_SIZE as usize;
    let mut v = Vec::with_capacity(px * 4);
    for _ in 0..px {
        v.extend_from_slice(&rgba);
    }
    debug_assert_eq!(
        v.len(),
        raster::Tile::byte_len(raster::PixelFormat::Rgba8),
        "a layer tile is a full TILE_SIZE square of RGBA8"
    );
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use asset_store::BlobHash;
    use editor_core::{PixelKey, TileDelta, TileEdit};
    use raster::TileCoord;

    fn doc_with_tile(hash: TileHash) -> Document {
        let mut doc = Document::new(256, 256, "t");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        );
        doc
    }

    #[test]
    fn a_missing_tile_is_refused_rather_than_saved_as_a_blank_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc_with_tile(TileHash([3; 32]));
        let err = write_tiles(dir.path(), &doc, &NoTiles).unwrap_err();
        assert!(matches!(err, ProjectError::MissingTile { .. }), "{err}");
    }

    #[test]
    fn identical_tiles_are_stored_once() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([10, 20, 30, 255]);
        let hash = TileHash::of(&bytes);

        // Three references — two layers plus a second coordinate — one blob.
        let mut doc = Document::new(512, 512, "t");
        let a = layer_model::Layer::raster("A");
        let b = layer_model::Layer::raster("B");
        let (a_id, b_id) = (a.id, b.id);
        doc.layers.push_root(a).unwrap();
        doc.layers.push_root(b).unwrap();
        doc.pixels.apply(
            PixelKey::Layer(a_id),
            &TileDelta::new([
                TileEdit::set(TileCoord::new(0, 0, 0), hash),
                TileEdit::set(TileCoord::new(1, 0, 0), hash),
            ])
            .unwrap(),
        );
        doc.pixels.apply(
            PixelKey::Layer(b_id),
            &TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash)),
        );

        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());
        let report = write_tiles(dir.path(), &doc, &source).unwrap();
        assert_eq!(report.blobs_written, 1);
        assert_eq!(report.references_deduplicated, 2);
        assert_eq!(report.bytes_written, bytes.len() as u64);
    }

    #[test]
    fn a_blob_that_does_not_hash_to_its_name_is_refused_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([1, 2, 3, 4]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);

        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes);
        write_tiles(dir.path(), &doc, &source).unwrap();

        // Tamper: same filename, different pixels.
        let path = blob_path(dir.path(), hash);
        std::fs::write(&path, solid_tile([9, 9, 9, 9])).unwrap();

        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::CorruptBlob { .. }), "{err}");
    }

    #[test]
    fn an_oversized_blob_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([1, 2, 3, 4]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes);
        write_tiles(dir.path(), &doc, &source).unwrap();

        std::fs::write(
            blob_path(dir.path(), hash),
            vec![0u8; MAX_TILE_BYTES as usize + 1],
        )
        .unwrap();
        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::FileTooLarge { .. }), "{err}");
    }

    #[test]
    fn tiles_come_back_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([200, 100, 50, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());
        write_tiles(dir.path(), &doc, &source).unwrap();

        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
        assert_eq!(&*store.get(BlobHash(hash.0)).unwrap(), bytes.as_slice());
    }

    /// A document referencing `n` distinct tiles, none of whose blobs exist.
    fn doc_with_n_tiles(n: u8) -> Document {
        let mut doc = Document::new(4096, 4096, "many");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        for i in 0..n {
            doc.pixels.apply(
                PixelKey::Layer(id),
                &TileDelta::single(TileEdit::set(
                    TileCoord::new(i as i32, 0, 0),
                    TileHash([i; 32]),
                )),
            );
        }
        doc
    }

    #[test]
    fn the_tile_cap_stops_the_collection_rather_than_measuring_it_afterwards() {
        let doc = doc_with_n_tiles(10);
        let err = referenced(&doc, 3).unwrap_err();
        match err {
            ProjectError::TooManyTiles { count, max } => {
                assert_eq!(max, 3);
                // 4, not 10: collecting stopped one past the cap. A count of 10
                // would mean the whole set had been materialized first, which
                // is the allocation the cap exists to prevent.
                assert_eq!(count, 4, "the full set was collected before the check");
            }
            other => panic!("{other}"),
        }
        // Under the cap it still collects everything.
        assert_eq!(referenced(&doc, 10).unwrap().0.len(), 10);
    }

    #[test]
    fn a_document_over_the_tile_cap_is_refused_before_a_blob_is_opened() {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc_with_n_tiles(10);
        let mut store = AssetStore::new();
        let err = read_tiles_capped(dir.path(), &doc, &mut store, count_cap(3)).unwrap_err();
        assert!(
            matches!(err, ProjectError::TooManyTiles { max: 3, .. }),
            "{err}"
        );
        assert!(store.is_empty(), "nothing should have been read");
    }

    #[test]
    fn tile_data_over_the_byte_budget_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([4, 4, 4, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());
        write_tiles(dir.path(), &doc, &source).unwrap();

        let mut store = AssetStore::new();
        let err = read_tiles_capped(
            dir.path(),
            &doc,
            &mut store,
            data_cap(bytes.len() as u64 - 1),
        )
        .unwrap_err();
        assert!(
            matches!(err, ProjectError::TileDataTooLarge { .. }),
            "{err}"
        );
    }

    /// The real caps with only the tile-count bound lowered.
    fn count_cap(count: u64) -> TileCaps {
        TileCaps {
            count,
            ..TileCaps::default()
        }
    }

    /// The real caps with only the aggregate lowered.
    fn data_cap(data: u64) -> TileCaps {
        TileCaps {
            data,
            ..TileCaps::default()
        }
    }

    #[test]
    fn a_blob_the_reader_would_refuse_to_open_is_refused_by_the_save() {
        // `read_tiles` refuses a blob over `MAX_TILE_BYTES` before it opens it
        // and `write_tiles` used to write one anyway, which is a package that
        // saves and then fails every open — the assets defect, in the module
        // that stated the rule. Reachable the day a `Rgba16` tile (twice this
        // size) reaches `TileBytes`.
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![9u8; MAX_TILE_BYTES as usize + 1];
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());

        let err = write_tiles(dir.path(), &doc, &source).unwrap_err();
        assert!(
            matches!(&err, ProjectError::PackageFileTooLarge { size, max, .. }
                     if *size == bytes.len() as u64 && *max == MAX_TILE_BYTES),
            "{err}"
        );
        assert!(
            !blob_path(dir.path(), hash).exists(),
            "a blob the loader will not open must never be written"
        );

        // And exactly at the cap it still round-trips.
        let ok = solid_tile([1, 2, 3, 255]);
        assert_eq!(ok.len() as u64, MAX_TILE_BYTES);
        let doc = doc_with_tile(TileHash::of(&ok));
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(ok);
        write_tiles(dir.path(), &doc, &source).unwrap();
        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
    }

    #[test]
    fn tile_data_over_the_byte_budget_is_refused_by_the_save_too() {
        // The mirror of `tile_data_over_the_byte_budget_is_refused`: the
        // aggregate is a load-side bound, so the save has to stop at the same
        // total or the package is refused only once it is the user's only copy.
        // Not implied by the other two bounds — a million tiles at
        // `MAX_TILE_BYTES` is 256 GiB against an 8 GiB budget.
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([4, 4, 4, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());

        let over = dir.path().join("over");
        std::fs::create_dir(&over).unwrap();
        let err =
            write_tiles_capped(&over, &doc, &source, data_cap(bytes.len() as u64 - 1)).unwrap_err();
        assert!(
            matches!(err, ProjectError::TileDataTooLarge { max } if max == bytes.len() as u64 - 1),
            "{err}"
        );
        assert!(
            !blob_path(&over, hash).exists(),
            "a package over the budget must not be half-written"
        );

        // At the budget it writes, and what the save accepted the load opens.
        let under = dir.path().join("under");
        std::fs::create_dir(&under).unwrap();
        let caps = data_cap(bytes.len() as u64);
        write_tiles_capped(&under, &doc, &source, caps).unwrap();
        let mut store = AssetStore::new();
        assert_eq!(
            read_tiles_capped(&under, &doc, &mut store, caps).unwrap(),
            1
        );
    }

    #[test]
    fn a_missing_blob_reads_as_a_missing_file_not_as_transparency() {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc_with_tile(TileHash([7; 32]));
        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::MissingFile { .. }), "{err}");
    }
}
