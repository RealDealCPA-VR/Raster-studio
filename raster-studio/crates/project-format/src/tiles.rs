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
//! tiles/<first two hex digits>/<64 hex digits>.tilez    deflate-compressed
//! tiles/<first two hex digits>/<64 hex digits>.tile     raw pixels
//! ```
//!
//! The name **is** the BLAKE3 of the tile's *pixels* ([`raster::TileHash`]) —
//! of the decompressed bytes, whichever form the blob is stored in — so:
//!
//! * identical tiles are stored once, however many layers, masks, mip levels or
//!   history states reference them — a flat fill across a layer is one blob;
//! * a blob is self-verifying. Every read decodes, re-hashes and compares
//!   against the name, which is why tile blobs are *not* listed in the
//!   manifest's digest table: the check is inherent, and listing them would
//!   make the manifest grow with the pixel count.
//!
//! The two-digit shard keeps directories to a few thousand entries on
//! filesystems that get slow with a hundred thousand.
//!
//! # What a save costs, and the three things that keep it small
//!
//! A 4000×3000 document with ten layers references on the order of two
//! thousand 256×256 tiles — half a gigabyte of pixels. Writing every one of
//! them, raw, with an `fsync` each, on every save (and every autosave, every
//! five minutes) was the shape of the finding this module was rewritten for.
//! Three changes, each measurable through [`TileReport`]:
//!
//! 1. **Reuse before re-encoding.** When the save replaces an existing package,
//!    every tile whose hash the previous package already holds is *verified*
//!    (decoded and re-hashed — a blob is never trusted by its name alone) and
//!    then hard-linked into the new package, or copied where the filesystem has
//!    no hard links. The tile source is not consulted, nothing is compressed,
//!    nothing is fsynced: a save of an unchanged document writes **zero** new
//!    tile bytes, and an edit to one layer rewrites only that layer's changed
//!    tiles ([`TileReport::blobs_reused`]).
//! 2. **Compression.** A new blob is deflate-compressed (`.tilez`), stored raw
//!    (`.tile`) only when compression would not make it smaller. Flat and
//!    near-flat regions — most of most layers — shrink by one to two orders of
//!    magnitude ([`TileReport::encoded_bytes_written`]).
//! 3. **One batch of fsyncs.** New blobs are written without a per-file
//!    `fsync`; the pass ends with one [`crate::atomic::sync_files`] over
//!    exactly the files it wrote, before the manifest is written and before
//!    the swap. The crash-safety argument is unchanged — nothing is renamed
//!    into place until every byte behind it is on disk — but the OS gets the
//!    whole pass to write back first, and a reused blob is never fsynced at all
//!    ([`TileReport::file_syncs`]). It is still one `fsync` *call* per new
//!    file; `std` offers nothing coarser, which is why avoiding the write in
//!    the first place (1) is the rule that matters most.
//!
//! # Bounds, in both directions
//!
//! Both the tile count and the total byte volume are capped
//! ([`MAX_PACKAGE_TILES`], [`MAX_TILE_DATA_BYTES`]), and a single blob may not
//! exceed the largest tile this format stores ([`MAX_TILE_BYTES`]) — the
//! *decoded* size, which is what becomes resident, and which a compressed blob
//! is inflated no further than. The document naming those tiles came out of
//! the package too, so the counts it implies are as untrusted as the files
//! themselves.
//!
//! All three apply on the way **out** as well, from one [`TileCaps`]: a package
//! with more in it than this reader will load is a package this writer must not
//! produce. The count always did; the two byte bounds did not, and the shape
//! that leaves — a save that returns `Ok` and a package that never opens again —
//! is the one [`crate::assets`] had to be fixed for twice.

use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use asset_store::AssetStore;
use editor_core::Document;
use raster::TileHash;

use crate::atomic::{sync_files, write_unsynced};
use crate::error::ProjectError;
use crate::{hexid, safepath};

/// Directory holding content-addressed tile blobs.
pub const TILES_DIR: &str = "tiles";

/// Extension of a blob stored as raw pixels — every blob a v2 package holds,
/// and the form a v3 writer falls back to when compression would not help.
pub const TILE_EXT: &str = "tile";

/// Extension of a blob stored deflate-compressed. The hash in the name is the
/// hash of the *inflated* bytes.
pub const COMPRESSED_TILE_EXT: &str = "tilez";

/// Largest a single tile blob may be: one `TILE_SIZE²` [`raster::PixelFormat::Rgba16`]
/// tile, eight bytes a pixel.
///
/// Image > Mode > 16 Bits and a 16-bit New Document put tiles of exactly this
/// size in the store (`raster::depth::widen_rgba8_tile`), and the cap used to
/// be the RGBA8 size, half of it: every save of a 16-bit document was refused
/// with [`ProjectError::PackageFileTooLarge`] and the only copy of the work
/// was the one in memory. An RGBA8 tile is half this and a mask tile an
/// eighth. A [`raster::PixelFormat::RgbaF32`] tile is twice this; whoever
/// brings those through [`TileBytes`] raises this one number and both sides
/// (write and read) move with it.
pub const MAX_TILE_BYTES: u64 = raster::Tile::byte_len(raster::PixelFormat::Rgba16) as u64;

/// Most distinct tiles one package may reference.
pub const MAX_PACKAGE_TILES: u64 = 1 << 20;

/// Most tile bytes one package may load into memory.
///
/// Not implied by the other two: their product is 512 GiB. It stays a whole
/// number of the largest tile ([`MAX_TILE_BYTES`] divides it), so a package
/// of full 16-bit tiles fills it exactly rather than stopping one short.
pub const MAX_TILE_DATA_BYTES: u64 = 8 << 30;

/// Every bound the tile path applies — **in both directions**.
///
/// [`write_tiles`] and [`read_tiles`] each build this from the constants above,
/// so each bound is one number rather than a writer's number and a reader's
/// number that can drift apart. See [`crate::assets::AssetCaps`], which exists
/// for the same reason and was written after the drift had already cost a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TileCaps {
    /// Largest single blob, decoded: [`MAX_TILE_BYTES`].
    pub tile: u64,
    /// Most distinct tiles: [`MAX_PACKAGE_TILES`].
    pub count: u64,
    /// Largest total, decoded: [`MAX_TILE_DATA_BYTES`].
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
///
/// One exception, and it is the point of blob reuse: a save *over an existing
/// package that already holds every referenced tile* succeeds through this
/// source, because no tile has to be produced — see [`write_tiles`].
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

/// How far a save's tile pass has got, readable from another thread.
///
/// The save runs on a worker; the status bar runs on the interaction thread.
/// This is the one piece of state they share: two counters the writer bumps
/// and the reader polls once a frame. Plain atomics, no lock, no channel — a
/// stale reading costs a frame of progress text, never correctness.
#[derive(Debug, Default)]
pub struct SaveProgress {
    tiles_total: AtomicU64,
    tiles_done: AtomicU64,
}

impl SaveProgress {
    /// A fresh, shareable progress record.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Distinct tiles the pass will handle. Zero until the pass starts.
    pub fn tiles_total(&self) -> u64 {
        self.tiles_total.load(Ordering::Relaxed)
    }

    /// Tiles reused or written so far.
    pub fn tiles_done(&self) -> u64 {
        self.tiles_done.load(Ordering::Relaxed)
    }

    /// The writer's side: the pass is about to handle `n` distinct tiles.
    /// Public so a test of the *reader* (a status bar) can move the numbers
    /// without a real tile pass.
    pub fn set_total(&self, n: u64) {
        self.tiles_total.store(n, Ordering::Relaxed);
        self.tiles_done.store(0, Ordering::Relaxed);
    }

    /// The writer's side: one more tile reused or written.
    pub fn bump(&self) {
        self.tiles_done.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a save did to `tiles/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TileReport {
    /// Distinct blobs newly encoded and written.
    pub blobs_written: usize,
    /// Pixel bytes behind those newly written blobs.
    pub bytes_written: u64,
    /// Bytes those newly written blobs occupy on disk (after compression).
    pub encoded_bytes_written: u64,
    /// Distinct blobs taken from the previous package — verified by hash, then
    /// hard-linked or copied — instead of being re-encoded from the source.
    pub blobs_reused: usize,
    /// Pixel bytes behind the reused blobs.
    pub bytes_reused: u64,
    /// References that resolved to a blob already in the package — the dedup
    /// win.
    pub references_deduplicated: usize,
    /// File fsyncs the pass issued. One per *newly written or copied* blob,
    /// issued as a single batch at the end of the pass; zero for a save that
    /// reused everything.
    pub file_syncs: usize,
}

/// Package-relative shard and hex name of one tile blob.
fn blob_rel(hash: TileHash) -> (String, String) {
    let hex = hexid::to_hex(&hash.0);
    (hex[..2].to_string(), hex)
}

/// Package-relative path of one tile blob in the given form.
fn blob_rel_path(hash: TileHash, ext: &str) -> String {
    let (shard, hex) = blob_rel(hash);
    format!("{TILES_DIR}/{shard}/{hex}.{ext}")
}

/// Absolute path of the *raw* form of a blob.
fn blob_path(root: &Path, hash: TileHash) -> PathBuf {
    let (shard, hex) = blob_rel(hash);
    root.join(TILES_DIR)
        .join(shard)
        .join(format!("{hex}.{TILE_EXT}"))
}

/// Absolute path of the *compressed* form of a blob.
fn compressed_blob_path(root: &Path, hash: TileHash) -> PathBuf {
    let (shard, hex) = blob_rel(hash);
    root.join(TILES_DIR)
        .join(shard)
        .join(format!("{hex}.{COMPRESSED_TILE_EXT}"))
}

/// Deflate `bytes`. Level 1: on pixel data the ratio barely moves past it
/// while the time per tile does, and a save is on the clock.
fn deflate(bytes: &[u8]) -> Result<Vec<u8>, ProjectError> {
    let mut encoder = flate2::write::DeflateEncoder::new(
        Vec::with_capacity(bytes.len() / 8),
        flate2::Compression::fast(),
    );
    encoder.write_all(bytes)?;
    Ok(encoder.finish()?)
}

/// Why a compressed blob could not be inflated into a tile.
enum InflateError {
    /// The stream is not deflate, or is truncated.
    Corrupt,
    /// It inflates past `max` bytes: not a tile this format stores.
    TooLarge,
}

/// Inflate `encoded`, refusing to produce more than `max` bytes.
///
/// The cap is applied *while* inflating (`take`), not to the result: a hostile
/// blob that inflates to gigabytes must not get to allocate them first.
fn inflate_capped(encoded: &[u8], max: u64) -> Result<Vec<u8>, InflateError> {
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(encoded)
        .take(max.saturating_add(1))
        .read_to_end(&mut out)
        .map_err(|_| InflateError::Corrupt)?;
    if out.len() as u64 > max {
        return Err(InflateError::TooLarge);
    }
    Ok(out)
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
/// `reuse_from` is the package this save is replacing, if any: a blob it
/// already holds for a referenced hash is verified and linked (or copied) into
/// `root` instead of being re-encoded from `tiles` — see the module header.
/// `progress`, when given, is kept current for a status bar on another thread.
///
/// Capped on the way out as well as the way in: a package with more in it than
/// this reader will load is a package this writer must not produce. See
/// [`TileCaps`].
pub(crate) fn write_tiles(
    root: &Path,
    doc: &Document,
    tiles: &dyn TileBytes,
    reuse_from: Option<&Path>,
    progress: Option<&SaveProgress>,
) -> Result<TileReport, ProjectError> {
    write_tiles_capped(root, doc, tiles, TileCaps::default(), reuse_from, progress)
}

/// A blob taken over from the previous package.
struct ReusedBlob {
    /// Where it now lives in the new package.
    path: PathBuf,
    /// Its pixel size — what counts against the data budget.
    decoded_len: u64,
    /// `true` when the filesystem refused a hard link and the bytes were
    /// copied instead, so the new file still needs its fsync.
    copied: bool,
}

/// Take `hash`'s blob over from the package at `previous`, if it holds one that
/// verifies. `None` means "produce it from the source": absent, unreadable,
/// oversized, or not hashing to its name — a corrupt blob in the old package is
/// *replaced*, never propagated.
fn reuse_blob(
    previous: &Path,
    root: &Path,
    hash: TileHash,
    tile_cap: u64,
) -> Result<Option<ReusedBlob>, ProjectError> {
    for (ext, compressed) in [(COMPRESSED_TILE_EXT, true), (TILE_EXT, false)] {
        let rel = blob_rel_path(hash, ext);
        // Built from a name we minted, but routed through the one guard every
        // package-relative path goes through; a previous package that fails it
        // is simply not reused.
        let Ok(src) = safepath::safe_join(previous, &rel, "tile") else {
            return Ok(None);
        };
        // `read_capped` refuses a symlink, a directory and anything over the
        // cap, and reports a missing file — all of which mean "not this one".
        let encoded = match safepath::read_capped(&src, &rel, tile_cap) {
            Ok(bytes) => bytes,
            Err(ProjectError::MissingFile { .. }) => continue,
            Err(_) => return Ok(None),
        };
        let decoded_len = if compressed {
            match inflate_capped(&encoded, tile_cap) {
                Ok(pixels) if TileHash::of(&pixels) == hash => pixels.len() as u64,
                _ => return Ok(None),
            }
        } else {
            if TileHash::of(&encoded) != hash {
                return Ok(None);
            }
            encoded.len() as u64
        };
        let dst = root.join(&rel);
        // A hard link shares the bytes with the previous package and costs no
        // data write and no fsync; both packages are content-addressed and
        // never rewrite a blob in place, so sharing is safe. A filesystem
        // without hard links (or one that refuses across a boundary) gets a
        // copy of the *encoded* bytes — still no re-encoding.
        let copied = match std::fs::hard_link(&src, &dst) {
            Ok(()) => false,
            Err(_) => {
                write_unsynced(&dst, &encoded)?;
                true
            }
        };
        return Ok(Some(ReusedBlob {
            path: dst,
            decoded_len,
            copied,
        }));
    }
    Ok(None)
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
    reuse_from: Option<&Path>,
    progress: Option<&SaveProgress>,
) -> Result<TileReport, ProjectError> {
    let (hashes, references) = referenced(doc, caps.count)?;
    if let Some(p) = progress {
        p.set_total(hashes.len() as u64);
    }
    let mut report = TileReport {
        references_deduplicated: references.saturating_sub(hashes.len()),
        ..TileReport::default()
    };
    let mut shards: HashSet<String> = HashSet::new();
    let mut total = 0u64;
    // Every file this pass creates and therefore owes an fsync. Filled during
    // the pass, flushed once at the end of it.
    let mut to_sync: Vec<PathBuf> = Vec::new();
    for hash in hashes {
        let (shard, hex) = blob_rel(hash);
        if shards.insert(shard.clone()) {
            std::fs::create_dir_all(root.join(TILES_DIR).join(&shard))?;
        }

        // 1. Reuse. The previous package's blob, verified, linked. The source
        //    is not consulted for this tile.
        if let Some(previous) = reuse_from {
            if let Some(reused) = reuse_blob(previous, root, hash, caps.tile)? {
                total = total.saturating_add(reused.decoded_len);
                if total > caps.data {
                    return Err(ProjectError::TileDataTooLarge { max: caps.data });
                }
                if reused.copied {
                    to_sync.push(reused.path);
                }
                report.blobs_reused += 1;
                report.bytes_reused += reused.decoded_len;
                if let Some(p) = progress {
                    p.bump();
                }
                continue;
            }
        }

        // 2. Produce. From the source, verified, compressed, written unsynced.
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
                path: format!("{TILES_DIR}/{hex}.{TILE_EXT}"),
            });
        }
        // The two byte bounds, before the write rather than after: the loader
        // refuses a blob over `caps.tile` and a package over `caps.data`, so
        // writing either would hand the user a project that saves and never
        // opens.
        if bytes.len() as u64 > caps.tile {
            return Err(ProjectError::PackageFileTooLarge {
                path: format!("{TILES_DIR}/{shard}/{hex}.{TILE_EXT}"),
                size: bytes.len() as u64,
                max: caps.tile,
            });
        }
        total = total.saturating_add(bytes.len() as u64);
        if total > caps.data {
            return Err(ProjectError::TileDataTooLarge { max: caps.data });
        }
        let compressed = deflate(bytes)?;
        // Compressed when that is smaller, raw otherwise: noise does not
        // deflate, and a `.tilez` larger than its pixels would only add the
        // inflate to every open.
        let (path, encoded): (PathBuf, &[u8]) = if compressed.len() < bytes.len() {
            (compressed_blob_path(root, hash), &compressed)
        } else {
            (blob_path(root, hash), bytes)
        };
        write_unsynced(&path, encoded)?;
        report.blobs_written += 1;
        report.bytes_written += bytes.len() as u64;
        report.encoded_bytes_written += encoded.len() as u64;
        to_sync.push(path);
        if let Some(p) = progress {
            p.bump();
        }
    }
    // 3. One batch of fsyncs, over exactly the files this pass created.
    report.file_syncs = sync_files(&to_sync)?;
    Ok(report)
}

/// Load every tile the document references into `store`.
///
/// Each blob is decoded, re-hashed and compared against the name it is filed
/// under, so a package cannot hand back bytes that are not the pixels the
/// document asked for. Both forms are read — `.tilez` first, then the raw
/// `.tile` a v2 package holds — so a package written before compression opens
/// unchanged.
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
        let bytes = read_blob(root, *hash, caps.tile)?;
        total = total.saturating_add(bytes.len() as u64);
        if total > caps.data {
            return Err(ProjectError::TileDataTooLarge { max: caps.data });
        }
        // `TileHash` and `BlobHash` are both BLAKE3 over the same bytes, so the
        // two addressing schemes are one addressing scheme: the store files it
        // under the very hash the document names.
        store.put(&bytes)?;
    }
    Ok(hashes.len())
}

/// The pixels of one blob, from whichever form the package holds, verified.
fn read_blob(root: &Path, hash: TileHash, tile_cap: u64) -> Result<Vec<u8>, ProjectError> {
    // Built from a 64-hex-digit name we produced ourselves, but routed through
    // the same check as anything else so there is exactly one way a
    // package-relative path becomes a real one.
    let rel_z = blob_rel_path(hash, COMPRESSED_TILE_EXT);
    let path_z = safepath::safe_join(root, &rel_z, "tile")?;
    let (pixels, rel) = match safepath::read_capped(&path_z, &rel_z, tile_cap) {
        Ok(encoded) => {
            // The on-disk cap bounds the read; the inflate is bounded
            // separately, to the same number, because deflate can expand a
            // kilobyte into a gigabyte.
            let pixels = inflate_capped(&encoded, tile_cap).map_err(|e| match e {
                InflateError::Corrupt => ProjectError::CorruptBlob {
                    path: rel_z.clone(),
                },
                InflateError::TooLarge => ProjectError::FileTooLarge {
                    path: rel_z.clone(),
                    size: tile_cap + 1,
                    max: tile_cap,
                },
            })?;
            (pixels, rel_z)
        }
        Err(ProjectError::MissingFile { .. }) => {
            let rel = blob_rel_path(hash, TILE_EXT);
            let path = safepath::safe_join(root, &rel, "tile")?;
            (safepath::read_capped(&path, &rel, tile_cap)?, rel)
        }
        Err(e) => return Err(e),
    };
    if TileHash::of(&pixels) != hash {
        return Err(ProjectError::CorruptBlob { path: rel });
    }
    Ok(pixels)
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

/// The file the writer actually produced for `hash` under `root`, whichever
/// form it chose. Exposed for tests in sibling modules.
#[cfg(test)]
pub(crate) fn stored_blob_path(root: &Path, hash: TileHash) -> Option<PathBuf> {
    [compressed_blob_path(root, hash), blob_path(root, hash)]
        .into_iter()
        .find(|p| p.is_file())
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

    /// The production entry point with nothing to reuse and nobody watching.
    fn write(
        root: &Path,
        doc: &Document,
        tiles: &dyn TileBytes,
    ) -> Result<TileReport, ProjectError> {
        write_tiles(root, doc, tiles, None, None)
    }

    /// Replace whatever the writer stored for `hash` with raw `bytes` — the
    /// tamper the verification tests need, aimed at the file the reader will
    /// actually open.
    fn tamper(root: &Path, hash: TileHash, bytes: &[u8]) {
        let stored = stored_blob_path(root, hash).expect("the blob was written");
        std::fs::remove_file(&stored).unwrap();
        std::fs::write(blob_path(root, hash), bytes).unwrap();
    }

    #[test]
    fn a_missing_tile_is_refused_rather_than_saved_as_a_blank_canvas() {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc_with_tile(TileHash([3; 32]));
        let err = write(dir.path(), &doc, &NoTiles).unwrap_err();
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
        let report = write(dir.path(), &doc, &source).unwrap();
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
        write(dir.path(), &doc, &source).unwrap();

        // Tamper: same filename, different pixels.
        tamper(dir.path(), hash, &solid_tile([9, 9, 9, 9]));

        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::CorruptBlob { .. }), "{err}");
    }

    #[test]
    fn a_compressed_blob_that_does_not_inflate_to_its_name_is_refused_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([1, 2, 3, 4]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes);
        write(dir.path(), &doc, &source).unwrap();
        let stored = stored_blob_path(dir.path(), hash).unwrap();
        assert_eq!(
            stored.extension().unwrap(),
            COMPRESSED_TILE_EXT,
            "a flat tile is stored compressed"
        );

        // A valid deflate stream of the wrong pixels, under the right name.
        std::fs::write(&stored, deflate(&solid_tile([9, 9, 9, 9])).unwrap()).unwrap();
        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::CorruptBlob { .. }), "{err}");

        // Not a deflate stream at all.
        std::fs::write(&stored, b"this is not deflate").unwrap();
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
        write(dir.path(), &doc, &source).unwrap();

        tamper(dir.path(), hash, &vec![0u8; MAX_TILE_BYTES as usize + 1]);
        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::FileTooLarge { .. }), "{err}");
    }

    #[test]
    fn a_compressed_blob_that_inflates_past_the_cap_is_refused() {
        // A few hundred bytes of deflate can name gigabytes of zeros. The cap
        // has to bind the *inflated* size, while inflating.
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([1, 2, 3, 4]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes);
        write(dir.path(), &doc, &source).unwrap();

        let bomb = deflate(&vec![0u8; MAX_TILE_BYTES as usize * 4]).unwrap();
        assert!(
            (bomb.len() as u64) < MAX_TILE_BYTES,
            "the bomb is small on disk"
        );
        std::fs::write(compressed_blob_path(dir.path(), hash), bomb).unwrap();
        let mut store = AssetStore::new();
        let err = read_tiles(dir.path(), &doc, &mut store).unwrap_err();
        assert!(
            matches!(err, ProjectError::FileTooLarge { max, .. } if max == MAX_TILE_BYTES),
            "{err}"
        );
    }

    #[test]
    fn tiles_come_back_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([200, 100, 50, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());
        let report = write(dir.path(), &doc, &source).unwrap();
        assert!(
            report.encoded_bytes_written < report.bytes_written,
            "a flat tile compresses: {report:?}"
        );

        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
        assert_eq!(&*store.get(BlobHash(hash.0)).unwrap(), bytes.as_slice());
    }

    #[test]
    fn a_raw_blob_from_a_v2_package_still_reads() {
        // The form every package written before compression holds.
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([7, 8, 9, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let (shard, _) = blob_rel(hash);
        std::fs::create_dir_all(dir.path().join(TILES_DIR).join(shard)).unwrap();
        std::fs::write(blob_path(dir.path(), hash), &bytes).unwrap();

        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
        assert_eq!(&*store.get(BlobHash(hash.0)).unwrap(), bytes.as_slice());
    }

    #[test]
    fn a_tile_that_does_not_compress_is_stored_raw() {
        let dir = tempfile::tempdir().unwrap();
        // Pseudo-random bytes: deflate cannot shrink these.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let bytes: Vec<u8> = (0..MAX_TILE_BYTES)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());
        let report = write(dir.path(), &doc, &source).unwrap();
        assert_eq!(report.encoded_bytes_written, bytes.len() as u64);
        assert_eq!(
            stored_blob_path(dir.path(), hash)
                .unwrap()
                .extension()
                .unwrap(),
            TILE_EXT
        );
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
        write(dir.path(), &doc, &source).unwrap();

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
    fn a_16_bit_tile_saves_and_reopens_byte_for_byte() {
        // W5-B (P0): Image > Mode > 16 Bits puts `Rgba16` tiles — twice the
        // RGBA8 size — in the store, and the cap was the RGBA8 size: every
        // save of a 16-bit document failed with `PackageFileTooLarge`.
        let dir = tempfile::tempdir().unwrap();
        let wide = raster::widen_rgba8_tile(&solid_tile([10, 20, 30, 255])).unwrap();
        assert_eq!(
            wide.len(),
            raster::Tile::byte_len(raster::PixelFormat::Rgba16)
        );
        let hash = TileHash::of(&wide);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(wide.clone());
        write(dir.path(), &doc, &source).expect("a 16-bit tile saves");

        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
        assert_eq!(&*store.get(BlobHash(hash.0)).unwrap(), wide.as_slice());
        // The data budget is a whole number of the largest tile.
        assert_eq!(MAX_TILE_DATA_BYTES % MAX_TILE_BYTES, 0);
    }

    #[test]
    fn a_blob_the_reader_would_refuse_to_open_is_refused_by_the_save() {
        // `read_tiles` refuses a blob over `MAX_TILE_BYTES` before it opens it
        // and `write_tiles` used to write one anyway, which is a package that
        // saves and then fails every open — the assets defect, in the module
        // that stated the rule. Reachable the day a `RgbaF32` tile (twice this
        // size) reaches `TileBytes`.
        let dir = tempfile::tempdir().unwrap();
        let bytes = vec![9u8; MAX_TILE_BYTES as usize + 1];
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());

        let err = write(dir.path(), &doc, &source).unwrap_err();
        assert!(
            matches!(&err, ProjectError::PackageFileTooLarge { size, max, .. }
                     if *size == bytes.len() as u64 && *max == MAX_TILE_BYTES),
            "{err}"
        );
        assert!(
            stored_blob_path(dir.path(), hash).is_none(),
            "a blob the loader will not open must never be written"
        );

        // And exactly at the cap it still round-trips.
        let ok = raster::widen_rgba8_tile(&solid_tile([1, 2, 3, 255])).unwrap();
        assert_eq!(ok.len() as u64, MAX_TILE_BYTES);
        let doc = doc_with_tile(TileHash::of(&ok));
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(ok);
        write(dir.path(), &doc, &source).unwrap();
        let mut store = AssetStore::new();
        assert_eq!(read_tiles(dir.path(), &doc, &mut store).unwrap(), 1);
    }

    #[test]
    fn tile_data_over_the_byte_budget_is_refused_by_the_save_too() {
        // The mirror of `tile_data_over_the_byte_budget_is_refused`: the
        // aggregate is a load-side bound, so the save has to stop at the same
        // total or the package is refused only once it is the user's only copy.
        // Not implied by the other two bounds — a million tiles at
        // `MAX_TILE_BYTES` is 512 GiB against an 8 GiB budget.
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([4, 4, 4, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());

        let over = dir.path().join("over");
        std::fs::create_dir(&over).unwrap();
        let err = write_tiles_capped(
            &over,
            &doc,
            &source,
            data_cap(bytes.len() as u64 - 1),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            matches!(err, ProjectError::TileDataTooLarge { max } if max == bytes.len() as u64 - 1),
            "{err}"
        );
        assert!(
            stored_blob_path(&over, hash).is_none(),
            "a package over the budget must not be half-written"
        );

        // At the budget it writes, and what the save accepted the load opens.
        let under = dir.path().join("under");
        std::fs::create_dir(&under).unwrap();
        let caps = data_cap(bytes.len() as u64);
        write_tiles_capped(&under, &doc, &source, caps, None, None).unwrap();
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

    #[test]
    fn a_corrupt_blob_in_the_previous_package_is_replaced_not_propagated() {
        // Reuse trusts nothing by name: a blob in the old package that no
        // longer hashes to its name is produced afresh from the source.
        let dir = tempfile::tempdir().unwrap();
        let bytes = solid_tile([5, 6, 7, 255]);
        let hash = TileHash::of(&bytes);
        let doc = doc_with_tile(hash);
        let mut source = compositor::MemoryTileSource::new();
        source.insert_bytes(bytes.clone());

        let old = dir.path().join("old");
        std::fs::create_dir(&old).unwrap();
        write(&old, &doc, &source).unwrap();
        tamper(&old, hash, &solid_tile([0, 0, 0, 0]));

        let new = dir.path().join("new");
        std::fs::create_dir(&new).unwrap();
        let report = write_tiles(&new, &doc, &source, Some(&old), None).unwrap();
        assert_eq!(report.blobs_reused, 0, "{report:?}");
        assert_eq!(report.blobs_written, 1, "{report:?}");
        let mut store = AssetStore::new();
        read_tiles(&new, &doc, &mut store).unwrap();
        assert_eq!(&*store.get(BlobHash(hash.0)).unwrap(), bytes.as_slice());
    }

    #[test]
    fn progress_counts_every_tile_reused_or_written() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = compositor::MemoryTileSource::new();
        let mut doc = Document::new(1024, 256, "p");
        let layer = layer_model::Layer::raster("L");
        let id = layer.id;
        doc.layers.push_root(layer).unwrap();
        for i in 0..4u8 {
            let bytes = solid_tile([i, i, i, 255]);
            let hash = source.insert_bytes(bytes);
            doc.pixels.apply(
                PixelKey::Layer(id),
                &TileDelta::single(TileEdit::set(TileCoord::new(i as i32, 0, 0), hash)),
            );
        }
        let progress = SaveProgress::new();
        assert_eq!(progress.tiles_total(), 0);
        let old = dir.path().join("old");
        std::fs::create_dir(&old).unwrap();
        write_tiles(&old, &doc, &source, None, Some(&progress)).unwrap();
        assert_eq!((progress.tiles_total(), progress.tiles_done()), (4, 4));

        let new = dir.path().join("new");
        std::fs::create_dir(&new).unwrap();
        let progress = SaveProgress::new();
        let report = write_tiles(&new, &doc, &NoTiles, Some(&old), Some(&progress)).unwrap();
        assert_eq!(report.blobs_reused, 4);
        assert_eq!((progress.tiles_total(), progress.tiles_done()), (4, 4));
    }
}
