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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use asset_store::{AssetRecord, AssetSource, AssetStore, BlobHash};
use serde::{Deserialize, Serialize};

use crate::atomic::write_and_sync;
use crate::error::ProjectError;
use crate::{hexid, safepath};

/// Directory holding the asset index and asset blobs.
pub const ASSETS_DIR: &str = "assets";
/// Package-relative path of the asset index.
pub const ASSETS_INDEX: &str = "assets/index.json";

/// Largest single asset this reader will load.
pub const MAX_ASSET_BYTES: u64 = 512 << 20;
/// Most assets one package may list.
pub const MAX_ASSETS: u64 = 1 << 16;
/// Largest asset index this reader will parse.
pub const MAX_INDEX_BYTES: u64 = 16 << 20;
/// Most asset bytes one package may load into memory.
///
/// The per-blob and per-count caps alone do not bound anything useful: their
/// product is `MAX_ASSETS × MAX_ASSET_BYTES`, i.e. 32 TiB, and the store the
/// loader fills is the memory-only [`AssetStore::new`] variant, which never
/// evicts. This is the aggregate the reader actually stands behind — the
/// counterpart of [`crate::tiles::MAX_TILE_DATA_BYTES`].
pub const MAX_ASSET_DATA_BYTES: u64 = 2 << 30;

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
pub(crate) fn write_assets(
    root: &Path,
    inputs: &[AssetInput],
    collect: bool,
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

    for input in inputs {
        let (mime, bytes, link) = match input {
            AssetInput::Embedded { mime, bytes } => (mime.clone(), Some(bytes.clone()), None),
            AssetInput::Linked { mime, path } => {
                if collect {
                    let label = path.display().to_string();
                    let bytes = safepath::read_capped(path, &label, MAX_ASSET_BYTES)?;
                    (mime.clone(), Some(bytes), None)
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
    read_assets_capped(root, index_bytes, store, MAX_ASSET_DATA_BYTES)
}

/// [`read_assets`] with the aggregate byte budget as a parameter, so a test can
/// reach it without writing gigabytes.
pub(crate) fn read_assets_capped(
    root: &Path,
    index_bytes: Option<&[u8]>,
    store: &mut AssetStore,
    max_total: u64,
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
                    let bytes = safepath::read_capped(&path, &rel, MAX_ASSET_BYTES)?;
                    if BlobHash::of(&bytes) != hash {
                        return Err(ProjectError::CorruptBlob { path: rel });
                    }
                    let len = bytes.len() as u64;
                    resident_bytes = resident_bytes.saturating_add(len);
                    if resident_bytes > max_total {
                        return Err(ProjectError::AssetDataTooLarge { max: max_total });
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
        let err = read_assets_capped(dir.path(), index.as_deref(), &mut store, 100).unwrap_err();
        assert!(
            matches!(err, ProjectError::AssetDataTooLarge { max: 100 }),
            "{err}"
        );

        let mut store = AssetStore::new();
        let records = read_assets_capped(dir.path(), index.as_deref(), &mut store, 128).unwrap();
        assert_eq!(records.len(), 2, "under the budget it still loads");
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
