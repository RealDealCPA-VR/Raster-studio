//! Linked and embedded assets, and "collect assets" for a portable package.
//!
//! `Manifest::assets_collected` used to be hard-coded `false` — the flag said
//! "this package is not portable" whether or not it was, so no reader could act
//! on it. It now means what it says: **every asset's bytes are inside the
//! package**.
//!
//! # Layout
//!
//! ```text
//! assets/index.json                              record per asset
//! assets/<two hex digits>/<64 hex digits>.blob   content-addressed bytes
//! ```
//!
//! Blobs share the addressing scheme and the self-verification of tile blobs
//! (see [`crate::tiles`]) and share the same [`asset_store::AssetStore`] on
//! load, so an asset whose bytes happen to equal a tile's costs nothing extra —
//! literally nothing: [`read_assets`] skips any blob the store already holds,
//! and skips any blob an earlier index entry already read. An index is
//! untrusted and may name one blob ten thousand times.
//!
//! # Collecting
//!
//! A [`AssetInput::Linked`] asset names a file outside the package. With
//! [`crate::SaveOptions::collect_assets`] set, the save **reads that file and
//! embeds it**; without it, only the link is recorded.
//!
//! Reading an arbitrary path is safe here and only here: the path comes from
//! the *application* at save time (the user picked the file), not from a
//! package. The load side never opens a `Linked` path — that would be the
//! traversal bug in a new costume — it reports the link and lets the
//! application decide.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use asset_store::{AssetRecord, AssetSource, AssetStore, BlobHash, StoreConfig};
use serde::{Deserialize, Serialize};

use crate::atomic::write_and_sync;
use crate::error::ProjectError;
use crate::{hexid, safepath};

/// Directory holding the asset index and asset blobs.
pub const ASSETS_DIR: &str = "assets";
/// Package-relative path of the asset index.
pub const ASSETS_INDEX: &str = "assets/index.json";

/// Largest single asset this format stores — **on both sides**.
///
/// The saver refuses an asset over this ([`write_assets`]) and the store the
/// loader fills is configured from it ([`store_config`]), so the two numbers
/// are one number and cannot drift apart. Both reach it through one private
/// accessor, [`caps`], which is also the only thing a test may replace — one
/// knob per bound, moving both sides of it, so no test can pass while the two
/// sides disagree.
///
/// They did drift, and it cost the file: the save side had no cap on an
/// [`AssetInput::Embedded`] asset at all, while [`AssetStore::new`] defaults
/// `max_blob_bytes` to 256 MiB and [`AssetStore::put`] refuses anything larger.
/// A 300 MiB embedded asset therefore *saved successfully* and the package
/// could never be opened again — `save_project_with` returned `Ok` and the next
/// `open_project` failed with "blob … is over the … byte limit", with the
/// user's only copy inside. [`crate::tiles::write_tiles`] had the rule right all
/// along: a package with more in it than this reader will load is a package this
/// writer must not produce.
pub const MAX_ASSET_BYTES: u64 = 512 << 20;
/// Most assets one package may list.
pub const MAX_ASSETS: u64 = 1 << 16;
/// Largest asset index this format stores — **on both sides**.
///
/// [`write_assets`] refuses to emit an index over this and [`crate::open_project`]
/// refuses to parse one, from the same [`caps`] accessor, for the reason
/// [`MAX_ASSET_BYTES`] gives: a package this writer produces that this reader
/// refuses is a project the user can never open again.
///
/// This bound was one-sided too, and it is *reachable without a hostile file*:
/// an index entry costs around 160 bytes plus its link path, and both `mime` and
/// the link path come from the application, so an average link path over roughly
/// 96 characters crosses 16 MiB at this module's own [`MAX_ASSETS`]. Twenty
/// thousand linked assets with ordinary long Windows paths saved `Ok` with a
/// 21 MiB index, and every later open failed with "assets/index.json is 21208892
/// bytes, more than the 16777216 this reader will load".
pub const MAX_INDEX_BYTES: u64 = 16 << 20;
/// Most asset bytes one package may load into memory.
///
/// The per-blob and per-count caps alone do not bound anything useful: their
/// product is `MAX_ASSETS × MAX_ASSET_BYTES`, i.e. 32 TiB, and the store the
/// loader fills is the memory-only [`AssetStore::new`] variant, which never
/// evicts. This is the aggregate the reader actually stands behind — the
/// counterpart of [`crate::tiles::MAX_TILE_DATA_BYTES`].
pub const MAX_ASSET_DATA_BYTES: u64 = 2 << 30;

// A single asset at the per-asset cap has to fit inside the aggregate one, or
// the write side would accept an asset the read side could never make resident
// — the same two-sided disagreement, one level up. Checked at compile time
// because it is a property of the constants, not of any run.
const _: () = assert!(MAX_ASSET_BYTES <= MAX_ASSET_DATA_BYTES);

/// Every byte bound the asset path applies — **in both directions**.
///
/// Each field is a limit the load enforces, and each is enforced by the save as
/// well, out of this one struct: the per-asset cap by [`write_assets`] and by
/// the store [`store_config`] builds, the aggregate by [`write_assets`] and by
/// [`read_assets`], the index size by [`write_assets`] and by
/// [`crate::open_project`]. One struct rather than a number per site is the
/// whole point — a field cannot be tightened on the reading side and left loose
/// on the writing side, which is how a package that saves and never reopens gets
/// written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AssetCaps {
    /// Largest single asset: [`MAX_ASSET_BYTES`].
    pub asset: u64,
    /// Largest total of *distinct* asset bytes: [`MAX_ASSET_DATA_BYTES`].
    pub total: u64,
    /// Largest `assets/index.json`: [`MAX_INDEX_BYTES`].
    pub index: u64,
}

impl Default for AssetCaps {
    /// The constants — what every non-test call runs at.
    fn default() -> Self {
        Self {
            asset: MAX_ASSET_BYTES,
            total: MAX_ASSET_DATA_BYTES,
            index: MAX_INDEX_BYTES,
        }
    }
}

// Test seam: the caps above, replaced as a **set**, for the length of one
// closure. Both sides of the format read them through `caps()`, so a test cannot
// move one side of a bound without moving the other. That is deliberate: the
// property under test is that each bound is one number, and a seam with a knob
// per side could be used to write a test that passes while the sides disagree,
// which is the bug itself.
#[cfg(test)]
thread_local! {
    static CAP_OVERRIDE: std::cell::Cell<Option<AssetCaps>> =
        const { std::cell::Cell::new(None) };
}

/// The byte caps in force.
///
/// [`AssetCaps::default`], except under the test seam above.
pub(crate) fn caps() -> AssetCaps {
    #[cfg(test)]
    {
        if let Some(overridden) = CAP_OVERRIDE.with(|c| c.get()) {
            return overridden;
        }
    }
    AssetCaps::default()
}

/// Run `f` with the caps replaced, restoring them afterwards even on a panic, so
/// a test can drive the real save/load API at a bound it can afford to reach.
#[cfg(test)]
pub(crate) fn with_caps<T>(caps: AssetCaps, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<AssetCaps>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAP_OVERRIDE.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(CAP_OVERRIDE.with(|c| c.replace(Some(caps))));
    f()
}

/// Configuration of the store a load fills.
///
/// Not [`StoreConfig::default`]: its `max_blob_bytes` is a number this crate
/// does not choose, and a saver capped at one limit filling a store capped at
/// another is how an unopenable package gets written. This is the single place
/// the two sides meet.
pub(crate) fn store_config() -> StoreConfig {
    StoreConfig {
        max_blob_bytes: caps().asset,
        ..StoreConfig::default()
    }
}

/// The store [`crate::open_project`] loads a package's tiles and assets into.
///
/// Memory-only, so nothing is evicted — see [`MAX_ASSET_DATA_BYTES`] and
/// [`crate::tiles::MAX_TILE_DATA_BYTES`] for what bounds it.
pub(crate) fn new_store() -> AssetStore {
    AssetStore::with_config(store_config())
}

/// An asset handed to the saver.
#[derive(Debug, Clone)]
pub enum AssetInput {
    /// Bytes the application already holds.
    Embedded { mime: String, bytes: Vec<u8> },
    /// A file on disk the document refers to.
    Linked { mime: String, path: PathBuf },
}

impl AssetInput {
    pub fn embedded(mime: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self::Embedded {
            mime: mime.into(),
            bytes: bytes.into(),
        }
    }

    pub fn linked(mime: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self::Linked {
            mime: mime.into(),
            path: path.into(),
        }
    }
}

/// On-disk form of one asset record.
///
/// Hashes are hex strings rather than 32-element arrays so the index stays
/// readable, and so parsing has a place to reject a hash that is not 64 hex
/// digits *before* it could be used to build a filename.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AssetEntry {
    hash: String,
    mime: String,
    byte_len: u64,
    /// Present only for an asset whose bytes are *not* in the package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    link: Option<String>,
}

/// What a save wrote to `assets/`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AssetReport {
    pub embedded: usize,
    pub linked: usize,
    pub blobs_written: usize,
    /// `true` when no asset is left as a bare link — the "portable project"
    /// flag written into the manifest.
    pub collected: bool,
}

fn blob_rel(hash: &BlobHash) -> String {
    let hex = hexid::to_hex(&hash.0);
    format!("{ASSETS_DIR}/{}/{hex}.blob", &hex[..2])
}

/// Write the asset index and blobs. Returns the index bytes so the caller can
/// record their digest in the manifest, and `None` when there are no assets at
/// all (in which case no index is written).
///
/// **Capped on the way out as well as the way in.** Every bound the load path
/// applies to `assets/` is applied here too, out of the same [`AssetCaps`] — the
/// per-asset size, the aggregate, the asset count and the size of the index
/// itself — because a package this writer produces that this reader refuses is a
/// project the user can never open again. See [`MAX_ASSET_BYTES`] and
/// [`MAX_INDEX_BYTES`] for the two occasions that actually happened.
pub(crate) fn write_assets(
    root: &Path,
    inputs: &[AssetInput],
    collect: bool,
) -> Result<(AssetReport, Option<Vec<u8>>), ProjectError> {
    write_assets_capped(root, inputs, collect, caps())
}

/// [`write_assets`] with the byte caps as a parameter, so a test can drive the
/// round-trip property at a bound it can reach without writing half a gigabyte.
///
/// `caps` must be the caps the *load* will run at: `caps.asset` the
/// `max_blob_bytes` of the store the package will be loaded into, `caps.total`
/// the aggregate that load will allow, `caps.index` the size of index it will
/// parse.
pub(crate) fn write_assets_capped(
    root: &Path,
    inputs: &[AssetInput],
    collect: bool,
    caps: AssetCaps,
) -> Result<(AssetReport, Option<Vec<u8>>), ProjectError> {
    if inputs.len() as u64 > MAX_ASSETS {
        return Err(ProjectError::TooManyAssets {
            count: inputs.len() as u64,
            max: MAX_ASSETS,
        });
    }
    let mut report = AssetReport::default();
    if inputs.is_empty() {
        // A package with no assets carries everything it needs, so it is
        // portable by definition.
        report.collected = true;
        return Ok((report, None));
    }

    let mut entries = Vec::with_capacity(inputs.len());
    let mut written: HashSet<String> = HashSet::new();
    // Aggregate over *distinct* blobs, which is exactly what the load makes
    // resident: a duplicated asset is one blob on both sides.
    let mut total = 0u64;

    for (index, input) in inputs.iter().enumerate() {
        let (mime, bytes, link): (String, Option<Cow<'_, [u8]>>, Option<String>) = match input {
            AssetInput::Embedded { mime, bytes } => {
                // The bytes are already in memory — the application handed them
                // over — so this is a refusal rather than an allocation guard.
                // It is the check whose absence made a saved project unopenable.
                if bytes.len() as u64 > caps.asset {
                    return Err(ProjectError::AssetTooLarge {
                        asset: format!("#{index} (embedded, {mime})"),
                        size: bytes.len() as u64,
                        max: caps.asset,
                    });
                }
                (mime.clone(), Some(Cow::Borrowed(bytes.as_slice())), None)
            }
            AssetInput::Linked { mime, path } => {
                if collect {
                    let label = path.display().to_string();
                    // `read_capped` stats before it reads, so an oversized file
                    // is refused without being allocated. Its `FileTooLarge`
                    // becomes the same error the embedded branch raises: one
                    // condition, one name, whichever way the bytes arrived.
                    let bytes =
                        safepath::read_capped(path, &label, caps.asset).map_err(|e| match e {
                            ProjectError::FileTooLarge { path, size, max } => {
                                ProjectError::AssetTooLarge {
                                    asset: path,
                                    size,
                                    max,
                                }
                            }
                            other => other,
                        })?;
                    (mime.clone(), Some(Cow::Owned(bytes)), None)
                } else {
                    (mime.clone(), None, Some(path.display().to_string()))
                }
            }
        };

        match bytes {
            Some(bytes) => {
                let hash = BlobHash::of(&bytes);
                let rel = blob_rel(&hash);
                if written.insert(rel.clone()) {
                    // Before the write, not after: a package over the budget is
                    // refused rather than half-written and then refused on load.
                    total = total.saturating_add(bytes.len() as u64);
                    if total > caps.total {
                        return Err(ProjectError::AssetDataTooLarge { max: caps.total });
                    }
                    let path = safepath::safe_join(root, &rel, "asset")?;
                    if let Some(parent) = path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    write_and_sync(&path, &bytes)?;
                    report.blobs_written += 1;
                }
                report.embedded += 1;
                entries.push(AssetEntry {
                    hash: hexid::to_hex(&hash.0),
                    mime,
                    byte_len: bytes.len() as u64,
                    link: None,
                });
            }
            None => {
                report.linked += 1;
                // The hash of a link is the hash of the path text: it is the
                // only stable identity available without opening the file, and
                // it is never used to build a path.
                let hash = BlobHash::of(link.as_deref().unwrap_or_default().as_bytes());
                entries.push(AssetEntry {
                    hash: hexid::to_hex(&hash.0),
                    mime,
                    byte_len: 0,
                    link,
                });
            }
        }
    }

    report.collected = report.linked == 0;
    std::fs::create_dir_all(root.join(ASSETS_DIR))?;
    let index = serde_json::to_vec_pretty(&entries)?;
    // The last bound, and the one that has to be measured rather than predicted:
    // an index's size is a property of every entry in it — `mime` and the link
    // path are caller-supplied text of no fixed length — so it can only be
    // checked once the bytes exist. Before `write_and_sync`, not after: the
    // reader refuses an index over this size, so writing one would hand the user
    // a package that saves and never opens. See [`MAX_INDEX_BYTES`].
    if index.len() as u64 > caps.index {
        return Err(ProjectError::PackageFileTooLarge {
            path: ASSETS_INDEX.to_string(),
            size: index.len() as u64,
            max: caps.index,
        });
    }
    write_and_sync(&safepath::safe_join(root, ASSETS_INDEX, "assets")?, &index)?;
    Ok((report, Some(index)))
}

// Test seam: how many blob files `read_assets` actually opened. The dedup it
// counts is not observable from the outside — a duplicated entry produces the
// same record either way — so the only honest way to test "read once" is to
// count the reads.
#[cfg(test)]
thread_local! {
    pub(crate) static BLOB_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Read the asset index and every embedded blob into `store`.
///
/// Returns the records in index order. A `Linked` record's file is **not**
/// opened: the path came out of the package.
///
/// # Why the index is deduplicated
///
/// An index is untrusted and may name the same blob any number of times. Doing
/// the read and the BLAKE3 once per *entry* rather than once per *blob* made a
/// 1.5 MiB package (a 1 MiB blob and a 0.5 MiB index holding 4000 copies of one
/// entry) do 4 GiB of work while the store ended up holding a single blob; at
/// this module's own caps that is tens of terabytes, from a file small enough to
/// mail. A blob whose bytes are already materialized — by an earlier entry, or
/// as a tile, since tiles and assets share one content-addressed store — is
/// already the bytes that hash to the name the entry gave, so re-reading and
/// re-hashing it proves nothing.
pub(crate) fn read_assets(
    root: &Path,
    index_bytes: Option<&[u8]>,
    store: &mut AssetStore,
) -> Result<Vec<AssetRecord>, ProjectError> {
    read_assets_capped(root, index_bytes, store, caps())
}

/// [`read_assets`] with the byte caps as a parameter, so a test can reach them
/// without writing gigabytes.
pub(crate) fn read_assets_capped(
    root: &Path,
    index_bytes: Option<&[u8]>,
    store: &mut AssetStore,
    caps: AssetCaps,
) -> Result<Vec<AssetRecord>, ProjectError> {
    let Some(index_bytes) = index_bytes else {
        return Ok(Vec::new());
    };
    let entries: Vec<AssetEntry> = serde_json::from_slice(index_bytes)?;
    if entries.len() as u64 > MAX_ASSETS {
        return Err(ProjectError::TooManyAssets {
            count: entries.len() as u64,
            max: MAX_ASSETS,
        });
    }

    // Hash -> verified length, so the *second* mention of a blob costs a
    // hashmap lookup instead of a read plus a BLAKE3.
    let mut materialized: HashMap<BlobHash, u64> = HashMap::new();
    let mut resident_bytes = 0u64;
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        let Some(raw) = hexid::from_hex(&entry.hash) else {
            return Err(ProjectError::UnsafePath {
                field: "asset.hash",
                value: entry.hash,
            });
        };
        let hash = BlobHash(raw);
        match entry.link {
            Some(link) => out.push(AssetRecord {
                hash,
                mime: entry.mime,
                source: AssetSource::Linked { path: link },
                byte_len: entry.byte_len,
            }),
            None => {
                // Already materialized? By an earlier entry, or as a tile — the
                // store is shared. Its length is authoritative; `entry.byte_len`
                // is not, because it came out of the package. A store that says
                // it has the blob but cannot hand it over falls through to the
                // read rather than failing the load.
                let known = materialized.get(&hash).copied().or_else(|| {
                    if store.contains(hash) {
                        store.get(hash).ok().map(|b| b.len() as u64)
                    } else {
                        None
                    }
                });
                let byte_len = if let Some(len) = known {
                    materialized.insert(hash, len);
                    len
                } else {
                    let rel = blob_rel(&hash);
                    let path = safepath::safe_join(root, &rel, "asset")?;
                    #[cfg(test)]
                    BLOB_READS.with(|c| c.set(c.get() + 1));
                    let bytes = safepath::read_capped(&path, &rel, caps.asset)?;
                    if BlobHash::of(&bytes) != hash {
                        return Err(ProjectError::CorruptBlob { path: rel });
                    }
                    let len = bytes.len() as u64;
                    resident_bytes = resident_bytes.saturating_add(len);
                    if resident_bytes > caps.total {
                        return Err(ProjectError::AssetDataTooLarge { max: caps.total });
                    }
                    store.put(&bytes)?;
                    materialized.insert(hash, len);
                    len
                };
                out.push(AssetRecord {
                    hash,
                    mime: entry.mime,
                    source: AssetSource::Embedded,
                    byte_len,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_package_with_no_assets_is_portable() {
        let dir = tempfile::tempdir().unwrap();
        let (report, index) = write_assets(dir.path(), &[], false).unwrap();
        assert!(report.collected);
        assert!(index.is_none());
        assert!(!dir.path().join(ASSETS_INDEX).exists());
    }

    #[test]
    fn collecting_embeds_a_linked_file_and_flips_the_flag() {
        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("logo.png");
        std::fs::write(&external, b"not really a png, but bytes are bytes").unwrap();
        let pkg = dir.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();

        let inputs = vec![AssetInput::linked("image/png", &external)];

        // Without collection: a link, and the package is not portable.
        let (report, index) = write_assets(&pkg, &inputs, false).unwrap();
        assert_eq!((report.linked, report.embedded), (1, 0));
        assert!(!report.collected);
        let mut store = AssetStore::new();
        let records = read_assets(&pkg, index.as_deref(), &mut store).unwrap();
        assert!(matches!(records[0].source, AssetSource::Linked { .. }));
        assert!(store.is_empty(), "a link contributes no bytes");

        // With collection: the bytes are in the package and it is portable.
        let pkg2 = dir.path().join("pkg2");
        std::fs::create_dir(&pkg2).unwrap();
        let (report, index) = write_assets(&pkg2, &inputs, true).unwrap();
        assert_eq!((report.linked, report.embedded), (0, 1));
        assert!(report.collected);

        // ...and it survives the original file being deleted.
        std::fs::remove_file(&external).unwrap();
        let mut store = AssetStore::new();
        let records = read_assets(&pkg2, index.as_deref(), &mut store).unwrap();
        assert!(matches!(records[0].source, AssetSource::Embedded));
        assert_eq!(
            &*store.get(records[0].hash).unwrap(),
            &b"not really a png, but bytes are bytes"[..]
        );
    }

    #[test]
    fn identical_assets_share_one_blob() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![
            AssetInput::embedded("image/png", b"same".to_vec()),
            AssetInput::embedded("image/png", b"same".to_vec()),
        ];
        let (report, _) = write_assets(dir.path(), &inputs, false).unwrap();
        assert_eq!(report.embedded, 2);
        assert_eq!(report.blobs_written, 1);
    }

    #[test]
    fn an_index_naming_one_blob_a_thousand_times_reads_it_once() {
        // The forged index is the attack: 1000 entries, one blob. Reading and
        // hashing per *entry* turned a small package into unbounded work.
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"one blob, named over and over".to_vec();
        write_assets(
            dir.path(),
            &[AssetInput::embedded(
                "application/octet-stream",
                bytes.clone(),
            )],
            false,
        )
        .unwrap();

        let hex = hexid::to_hex(&BlobHash::of(&bytes).0);
        let forged: Vec<serde_json::Value> = (0..1000)
            .map(|_| {
                serde_json::json!({
                    "hash": hex,
                    "mime": "application/octet-stream",
                    // A lie, to prove the record's length comes from the blob.
                    "byte_len": 1
                })
            })
            .collect();
        let index = serde_json::to_vec(&forged).unwrap();

        let mut store = AssetStore::new();
        BLOB_READS.with(|c| c.set(0));
        let records = read_assets(dir.path(), Some(&index), &mut store).unwrap();
        assert_eq!(
            BLOB_READS.with(|c| c.get()),
            1,
            "the blob was opened once per entry"
        );
        assert_eq!(records.len(), 1000, "every entry still gets a record");
        assert_eq!(store.len(), 1, "and they are all one blob");
        assert!(
            records.iter().all(|r| r.byte_len == bytes.len() as u64),
            "the length must come from the blob, not from the index"
        );
    }

    #[test]
    fn an_asset_whose_bytes_are_already_a_tile_is_not_read_again() {
        // The module claims a duplicate costs nothing extra. Proved by deleting
        // the blob file: if the loader still opens it, this fails.
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"bytes that are both a tile and an asset".to_vec();
        let (_, index) = write_assets(
            dir.path(),
            &[AssetInput::embedded(
                "application/octet-stream",
                bytes.clone(),
            )],
            false,
        )
        .unwrap();
        std::fs::remove_file(dir.path().join(blob_rel(&BlobHash::of(&bytes)))).unwrap();

        let mut store = AssetStore::new();
        store.put(&bytes).unwrap(); // as `read_tiles` would have done
        BLOB_READS.with(|c| c.set(0));
        let records = read_assets(dir.path(), index.as_deref(), &mut store).unwrap();
        assert_eq!(BLOB_READS.with(|c| c.get()), 0);
        assert!(matches!(records[0].source, AssetSource::Embedded));
        assert_eq!(records[0].byte_len, bytes.len() as u64);
    }

    #[test]
    fn asset_volume_over_the_aggregate_budget_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![
            AssetInput::embedded("text/plain", vec![1u8; 64]),
            AssetInput::embedded("text/plain", vec![2u8; 64]),
        ];
        let (_, index) = write_assets(dir.path(), &inputs, false).unwrap();

        let mut store = AssetStore::new();
        let err = read_assets_capped(dir.path(), index.as_deref(), &mut store, total_cap(100))
            .unwrap_err();
        assert!(
            matches!(err, ProjectError::AssetDataTooLarge { max: 100 }),
            "{err}"
        );

        let mut store = AssetStore::new();
        let records =
            read_assets_capped(dir.path(), index.as_deref(), &mut store, total_cap(128)).unwrap();
        assert_eq!(records.len(), 2, "under the budget it still loads");
    }

    /// The store a load fills, at a test-sized blob cap.
    fn store_capped(max_blob_bytes: u64) -> AssetStore {
        AssetStore::with_config(StoreConfig {
            max_blob_bytes,
            ..store_config()
        })
    }

    /// The real caps with only the per-asset one lowered.
    fn asset_cap(asset: u64) -> AssetCaps {
        AssetCaps {
            asset,
            ..AssetCaps::default()
        }
    }

    /// The real caps with only the aggregate lowered.
    fn total_cap(total: u64) -> AssetCaps {
        AssetCaps {
            total,
            ..AssetCaps::default()
        }
    }

    /// The real caps with only the index bound lowered.
    fn index_cap(index: u64) -> AssetCaps {
        AssetCaps {
            index,
            ..AssetCaps::default()
        }
    }

    #[test]
    fn the_saver_and_the_loaders_store_read_one_asset_cap() {
        // The data-loss bug in one assertion. These were two different numbers:
        // the save capped an embedded asset at nothing at all and a collected
        // one at 512 MiB, while `AssetStore::new()` refuses a blob over its
        // 256 MiB default — so a package could save and never reopen.
        assert_eq!(store_config().max_blob_bytes, MAX_ASSET_BYTES);
    }

    #[test]
    fn an_asset_at_the_cap_round_trips_and_one_byte_over_is_refused_by_the_save() {
        // The property that was broken: whatever the save accepts, the load
        // opens. Driven at a cap small enough to run, through the same code the
        // real constants go through.
        const CAP: u64 = 64;
        let dir = tempfile::tempdir().unwrap();

        let at_cap = dir.path().join("at-cap");
        std::fs::create_dir(&at_cap).unwrap();
        let inputs = vec![AssetInput::embedded(
            "application/octet-stream",
            vec![7u8; CAP as usize],
        )];
        let (report, index) = write_assets_capped(&at_cap, &inputs, false, asset_cap(CAP)).unwrap();
        assert_eq!(report.blobs_written, 1);

        let mut store = store_capped(CAP);
        let records =
            read_assets_capped(&at_cap, index.as_deref(), &mut store, asset_cap(CAP)).unwrap();
        assert_eq!(
            records[0].byte_len, CAP,
            "an asset at exactly the cap has to come back"
        );

        // One byte more: refused by the SAVE, with a name, having written
        // nothing — not accepted and then unreadable forever.
        let over = dir.path().join("over-cap");
        std::fs::create_dir(&over).unwrap();
        let inputs = vec![AssetInput::embedded(
            "application/octet-stream",
            vec![7u8; CAP as usize + 1],
        )];
        let err = write_assets_capped(&over, &inputs, false, asset_cap(CAP)).unwrap_err();
        assert!(
            matches!(err, ProjectError::AssetTooLarge { size, max, .. }
                     if size == CAP + 1 && max == CAP),
            "{err}"
        );
        assert!(
            !over.join(ASSETS_INDEX).exists(),
            "the refusal must happen before anything is written"
        );
    }

    #[test]
    fn a_collected_file_over_the_cap_is_refused_by_the_save_too() {
        // The linked-and-collected route into the same store, same cap, same
        // named error — the bytes just arrive from a file instead of from the
        // application.
        const CAP: u64 = 32;
        let dir = tempfile::tempdir().unwrap();
        let external = dir.path().join("big.bin");
        std::fs::write(&external, vec![3u8; CAP as usize + 1]).unwrap();
        let pkg = dir.path().join("pkg");
        std::fs::create_dir(&pkg).unwrap();

        let inputs = vec![AssetInput::linked("application/octet-stream", &external)];
        let err = write_assets_capped(&pkg, &inputs, true, asset_cap(CAP)).unwrap_err();
        assert!(
            matches!(err, ProjectError::AssetTooLarge { size, max, .. }
                     if size == CAP + 1 && max == CAP),
            "{err}"
        );

        // Left as a link it is fine: no bytes enter the package, so no blob can
        // be too large for the store.
        let (report, _) = write_assets_capped(&pkg, &inputs, false, asset_cap(CAP)).unwrap();
        assert_eq!((report.linked, report.blobs_written), (1, 0));
    }

    #[test]
    fn asset_volume_over_the_aggregate_budget_is_refused_by_the_save() {
        // The mirror of `asset_volume_over_the_aggregate_budget_is_refused`:
        // the write side has to stop at the same total the read side does, or
        // the package is refused only once it is the user's only copy.
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![
            AssetInput::embedded("text/plain", vec![1u8; 64]),
            AssetInput::embedded("text/plain", vec![2u8; 64]),
        ];

        let over = dir.path().join("over");
        std::fs::create_dir(&over).unwrap();
        let err = write_assets_capped(&over, &inputs, false, total_cap(100)).unwrap_err();
        assert!(
            matches!(err, ProjectError::AssetDataTooLarge { max: 100 }),
            "{err}"
        );

        let under = dir.path().join("under");
        std::fs::create_dir(&under).unwrap();
        let (report, index) = write_assets_capped(&under, &inputs, false, total_cap(128)).unwrap();
        assert_eq!(report.blobs_written, 2, "at the budget it still writes");
        let mut store = store_capped(MAX_ASSET_BYTES);
        assert_eq!(
            read_assets_capped(&under, index.as_deref(), &mut store, total_cap(128))
                .unwrap()
                .len(),
            2,
            "and what the save accepted, the load opens"
        );
    }

    #[test]
    fn an_index_at_the_cap_is_written_and_one_byte_over_is_refused_by_the_save() {
        // The index was the bound with no write-side counterpart: `open_project`
        // refuses an `assets/index.json` over `MAX_INDEX_BYTES` and nothing
        // stopped a save from producing one, so a project with enough long link
        // paths in it saved `Ok` and never opened again. Driven at the size of a
        // real index rather than at 16 MiB, through the code the constant itself
        // goes through.
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![
            AssetInput::linked("image/png", "some/reasonably/long/path/one.png"),
            AssetInput::linked("image/png", "some/reasonably/long/path/two.png"),
        ];

        // What this index actually weighs.
        let measure = dir.path().join("measure");
        std::fs::create_dir(&measure).unwrap();
        let (_, index) =
            write_assets_capped(&measure, &inputs, false, AssetCaps::default()).unwrap();
        let len = index.expect("two assets produce an index").len() as u64;

        // Exactly at the cap: written, and at a size the loader will read.
        let at_cap = dir.path().join("at-cap");
        std::fs::create_dir(&at_cap).unwrap();
        let (_, index) = write_assets_capped(&at_cap, &inputs, false, index_cap(len)).unwrap();
        assert_eq!(index.unwrap().len() as u64, len);
        assert_eq!(
            std::fs::metadata(at_cap.join(ASSETS_INDEX)).unwrap().len(),
            len,
            "an index at exactly the cap has to be written"
        );

        // One byte less of headroom: refused BY THE SAVE, by name, with nothing
        // left on disk to be refused later.
        let over = dir.path().join("over");
        std::fs::create_dir(&over).unwrap();
        let err = write_assets_capped(&over, &inputs, false, index_cap(len - 1)).unwrap_err();
        assert!(
            matches!(&err, ProjectError::PackageFileTooLarge { path, size, max }
                     if path == ASSETS_INDEX && *size == len && *max == len - 1),
            "{err}"
        );
        assert!(
            !over.join(ASSETS_INDEX).exists(),
            "an index the reader would refuse must never reach the disk"
        );
    }

    #[test]
    fn a_tampered_asset_blob_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let inputs = vec![AssetInput::embedded("text/plain", b"original".to_vec())];
        let (_, index) = write_assets(dir.path(), &inputs, false).unwrap();
        let rel = blob_rel(&BlobHash::of(b"original"));
        std::fs::write(dir.path().join(&rel), b"swapped!").unwrap();

        let mut store = AssetStore::new();
        let err = read_assets(dir.path(), index.as_deref(), &mut store).unwrap_err();
        assert!(matches!(err, ProjectError::CorruptBlob { .. }), "{err}");
    }

    #[test]
    fn an_index_hash_that_is_not_hex_never_becomes_a_path() {
        let dir = tempfile::tempdir().unwrap();
        let index = serde_json::to_vec(&serde_json::json!([{
            "hash": "../../../../etc/passwd",
            "mime": "text/plain",
            "byte_len": 0
        }]))
        .unwrap();
        let mut store = AssetStore::new();
        let err = read_assets(dir.path(), Some(&index), &mut store).unwrap_err();
        assert!(
            matches!(
                err,
                ProjectError::UnsafePath {
                    field: "asset.hash",
                    ..
                }
            ),
            "{err}"
        );
    }
}
