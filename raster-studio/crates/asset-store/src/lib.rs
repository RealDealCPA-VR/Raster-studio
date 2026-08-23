//! Content-addressed blob store for tiles and assets.
//!
//! Everything is keyed by BLAKE3 hash, so identical tiles/assets are stored
//! once (deduplication) and change-detection is a hash comparison. This backs
//! both the in-memory tile cache and the on-disk `.rstudio/tiles` directory.
//!
//! # Two layers, one type
//!
//! [`AssetStore::new`] gives a purely in-memory store. [`AssetStore::open`]
//! gives the same store with a disk backend underneath it:
//!
//! * **write-through** — [`AssetStore::put`] durably writes a new blob (staged
//!   temp file, `fsync`, rename) before it is visible in memory. A deduplicating
//!   `put` skips that write only after a `stat` confirms the file is still on
//!   disk at the right length. That is a presence-and-length check, not a
//!   content check: it catches a file that a crashed `gc` unlinked or a
//!   truncated write left short, and it deliberately does not re-read the blob
//!   to compare bytes, so `put` does not detect (or repair) corruption that
//!   preserved the length. [`AssetStore::get`] is what catches that, by
//!   re-hashing;
//! * **read-through** — [`AssetStore::get`] repopulates the memory cache from
//!   disk when the blob was evicted, verifying the content hash on the way in
//!   so a corrupted file is reported instead of returned as valid data;
//! * **bounded memory** — the in-memory cache is an LRU with a configurable
//!   byte budget ([`StoreConfig::cache_bytes`]), so a large document cannot
//!   grow the cache without limit;
//! * **persistent refcounts** — every refcount change is appended to a journal
//!   and `fsync`ed, so reopening a project does not lose the accounting.
//!
//! # Reference counting and GC
//!
//! [`AssetStore::put`] increments, [`AssetStore::release`] decrements. Dropping
//! the last reference does **not** delete anything: the blob becomes
//! *unreferenced* and stays until [`AssetStore::gc`] is called explicitly.
//! Collection is never implicit, because an undo stack routinely drops the last
//! reference to a blob it is about to need again.
//!
//! # Untrusted store directories
//!
//! A store root can arrive inside someone else's project package, so its
//! contents are untrusted input. Every path the disk layer touches is built
//! either from a fixed name this crate chose (`blobs`, `tmp`, `index`) or from
//! a hash this crate computed itself, so no component of an untrusted string is
//! ever joined onto the root. Each of them — the root included, before anything
//! is created inside it — is then checked with `symlink_metadata` before it is
//! opened, written through or unlinked; the `disk` module docs list what each
//! check defends against. Every read is size-bounded before a buffer is
//! allocated ([`StoreConfig`]).
//!
//! # Threading
//!
//! [`AssetStore`] is internally synchronised, so `&self` methods can be called
//! concurrently from many threads (`Arc<AssetStore>`). One process is assumed
//! to own a given store root; there is no cross-process file locking.

mod disk;
mod hex;

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

use disk::{io_err, Disk, INDEX_HEADER_LEN, INDEX_RECORD_LEN};

/// A content hash (BLAKE3) used as a blob key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
    pub fn of(bytes: &[u8]) -> Self {
        BlobHash(*blake3::hash(bytes).as_bytes())
    }

    /// Lowercase 64-character hex. Performs a single allocation.
    pub fn to_hex(self) -> String {
        hex::encode32(&self.0)
    }

    /// Parse 64 hex characters (either case). `None` if the input is not
    /// exactly one hash worth of hex digits.
    pub fn from_hex(s: &str) -> Option<Self> {
        hex::decode32(s).map(BlobHash)
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

/// Tunables for the memory cache and for the bounds applied to on-disk data.
///
/// The size limits exist because a store directory is user-supplied data: a
/// project package copied from someone else can claim any file size, so every
/// read is bounded before a buffer is allocated.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    /// Byte budget for blobs held in memory. Only enforced when the store is
    /// disk-backed — evicting from a memory-only store would be data loss.
    pub cache_bytes: u64,
    /// Largest blob that may be written or read back.
    pub max_blob_bytes: u64,
    /// Largest refcount journal that will be replayed at open.
    ///
    /// This is a two-sided bound: writes compact the journal early enough that
    /// it never grows past it, so a store this crate wrote can always be
    /// reopened with the same config. Adding a blob whose entry would not fit
    /// even in a fully compacted journal fails with
    /// [`StoreError::IndexTooLarge`] instead.
    pub max_index_bytes: u64,
    /// Compact the journal once it holds this many records (and at least twice
    /// as many as there are live entries). `max_index_bytes` can force a
    /// compaction sooner.
    pub compact_threshold: u64,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            cache_bytes: 256 * 1024 * 1024,
            max_blob_bytes: 256 * 1024 * 1024,
            max_index_bytes: 64 * 1024 * 1024,
            compact_threshold: 4096,
        }
    }
}

/// What one [`AssetStore::gc`] pass reclaimed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GcReport {
    /// Unreferenced blobs dropped from memory and/or deleted from disk.
    pub blobs_removed: usize,
    /// Bytes freed on disk (plus resident bytes for a memory-only store).
    pub bytes_reclaimed: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("blob {0} not found")]
    NotFound(String),
    #[error("blob {0} has no outstanding references to release")]
    NotReferenced(String),
    #[error("refcount overflow for blob {0}")]
    RefcountOverflow(String),
    #[error("blob {hash} on disk is corrupt: content hashes to {actual}")]
    Corrupt { hash: String, actual: String },
    #[error("blob {hash} is {len} bytes, over the {max} byte limit")]
    BlobTooLarge { hash: String, len: u64, max: u64 },
    #[error("refcount index is {len} bytes, over the {max} byte limit")]
    IndexTooLarge { len: u64, max: u64 },
    #[error("refcount index is corrupt: {0}")]
    IndexCorrupt(String),
    #[error("{} exists but is not a directory (a symlinked store directory is refused)", .0.display())]
    NotADirectory(PathBuf),
    #[error("{} exists but is not a regular file", .0.display())]
    NotAFile(PathBuf),
    #[error("i/o error at {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// One known blob. Bytes and refcount live in a single entry so the two cannot
/// drift apart; `data` is `None` when the blob is on disk but evicted from the
/// memory cache.
#[derive(Debug)]
struct Entry {
    data: Option<Arc<[u8]>>,
    refs: u32,
    /// LRU position; `Some` exactly when `data` is `Some`.
    tick: Option<u64>,
}

#[derive(Debug, Default)]
struct Inner {
    entries: HashMap<BlobHash, Entry>,
    /// LRU order: tick -> hash, lowest tick is least recently used.
    lru: BTreeMap<u64, BlobHash>,
    next_tick: u64,
    resident_bytes: u64,
    /// Append handle for the refcount journal (disk-backed stores only).
    index: Option<File>,
    /// Records appended since the last compaction.
    appended: u64,
}

impl Inner {
    /// Mark a resident entry as most recently used.
    fn touch(&mut self, hash: BlobHash) {
        let tick = self.next_tick;
        let old = match self.entries.get_mut(&hash) {
            Some(e) if e.data.is_some() => e.tick.replace(tick),
            _ => return,
        };
        self.next_tick += 1;
        if let Some(o) = old {
            self.lru.remove(&o);
        }
        self.lru.insert(tick, hash);
    }

    /// Make `hash` resident with `data`, replacing any previous residency.
    fn set_resident(&mut self, hash: BlobHash, data: Arc<[u8]>) {
        let len = data.len() as u64;
        let tick = self.next_tick;
        let Some(entry) = self.entries.get_mut(&hash) else {
            return;
        };
        let old_len = entry.data.as_ref().map_or(0, |d| d.len() as u64);
        let old_tick = entry.tick.replace(tick);
        entry.data = Some(data);
        self.next_tick += 1;
        self.resident_bytes = self.resident_bytes - old_len + len;
        if let Some(o) = old_tick {
            self.lru.remove(&o);
        }
        self.lru.insert(tick, hash);
    }

    /// Drop the cached bytes for `hash`, returning how many bytes were freed.
    fn evict_one(&mut self, hash: BlobHash) -> u64 {
        let Some(entry) = self.entries.get_mut(&hash) else {
            return 0;
        };
        let freed = entry.data.take().map_or(0, |d| d.len() as u64);
        let tick = entry.tick.take();
        if let Some(t) = tick {
            self.lru.remove(&t);
        }
        self.resident_bytes -= freed;
        freed
    }

    /// Forget an entry entirely (used by GC).
    fn remove_entry(&mut self, hash: BlobHash) {
        self.evict_one(hash);
        self.entries.remove(&hash);
    }

    /// Do the three structures that are mutated together still agree?
    ///
    /// * an entry has an LRU tick exactly when it holds bytes;
    /// * the LRU map lists every resident entry once, under that entry's own
    ///   tick, and lists nothing else;
    /// * `resident_bytes` is the sum of the resident entries' lengths.
    ///
    /// Used by the poisoned-lock recovery in [`AssetStore::lock`], which is
    /// only sound while this holds. Debug builds only — it is O(n).
    fn invariants_hold(&self) -> bool {
        let mut resident = 0u64;
        let mut count = 0usize;
        for (hash, entry) in &self.entries {
            if entry.tick.is_some() != entry.data.is_some() {
                return false;
            }
            if let (Some(tick), Some(data)) = (entry.tick, entry.data.as_ref()) {
                if self.lru.get(&tick) != Some(hash) {
                    return false;
                }
                resident += data.len() as u64;
                count += 1;
            }
        }
        self.lru.len() == count && self.resident_bytes == resident
    }
}

/// Content-addressed store with reference counting, an LRU memory cache and an
/// optional disk backend.
#[derive(Debug)]
pub struct AssetStore {
    inner: Mutex<Inner>,
    disk: Option<Disk>,
    config: StoreConfig,
}

impl Default for AssetStore {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetStore {
    /// A store that lives entirely in memory. Nothing is evicted, because
    /// there is nowhere to read an evicted blob back from.
    pub fn new() -> Self {
        Self::with_config(StoreConfig::default())
    }

    pub fn with_config(config: StoreConfig) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            disk: None,
            config,
        }
    }

    /// Open (creating if needed) a disk-backed store rooted at `dir`, replaying
    /// the persisted refcounts.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        Self::open_with(dir, StoreConfig::default())
    }

    pub fn open_with(dir: impl AsRef<Path>, config: StoreConfig) -> Result<Self, StoreError> {
        let disk = Disk::open(dir.as_ref())?;
        let (records, _count) = disk.load_index(config.max_index_bytes)?;
        let mut entries: HashMap<BlobHash, Entry> = HashMap::new();
        for (hash, refs) in records {
            // Later records supersede earlier ones for the same hash.
            entries.insert(
                hash,
                Entry {
                    data: None,
                    refs,
                    tick: None,
                },
            );
        }
        // Rewrite the journal from the replayed state. This bounds its growth
        // across sessions and, crucially, heals a torn tail: appending after a
        // partial record would misalign every record written from now on.
        let mut snapshot: Vec<(BlobHash, u32)> =
            entries.iter().map(|(h, e)| (*h, e.refs)).collect();
        snapshot.sort_unstable();
        let index = disk.compact_index(&snapshot)?;
        let appended = snapshot.len() as u64;
        let inner = Inner {
            entries,
            lru: BTreeMap::new(),
            next_tick: 0,
            resident_bytes: 0,
            index: Some(index),
            appended,
        };
        Ok(Self {
            inner: Mutex::new(inner),
            disk: Some(disk),
            config,
        })
    }

    /// The store root, for disk-backed stores.
    pub fn root(&self) -> Option<&Path> {
        self.disk.as_ref().map(Disk::root)
    }

    /// Take the lock, recovering from poisoning.
    ///
    /// Nothing repairs `Inner` on this path, and nothing needs to: `entries`,
    /// `lru` and `resident_bytes` are only ever mutated together inside
    /// [`Inner::touch`], [`Inner::set_resident`] and [`Inner::evict_one`], none
    /// of which contains an operation that can panic or return early between
    /// the paired mutations. A thread that panics while holding this lock
    /// therefore cannot have left a torn `Inner` behind, so continuing with the
    /// poisoned state is sound rather than merely convenient.
    ///
    /// That is a property of the current code, not a guarantee of the design,
    /// so the recovery path asserts it in debug builds: if a future edit puts a
    /// panicking operation between those mutations, this fires instead of
    /// quietly handing out a half-updated cache.
    fn lock(&self) -> MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let guard = poisoned.into_inner();
                debug_assert!(
                    guard.invariants_hold(),
                    "a panicking thread left Inner torn; the recovery path in \
                     AssetStore::lock assumes that cannot happen"
                );
                guard
            }
        }
    }

    /// Insert bytes, returning their content hash.
    ///
    /// Idempotent on content: inserting the same bytes twice stores one copy
    /// and increments the refcount. The bytes are borrowed rather than taken by
    /// value, so a dedup hit never copies the blob — the caller keeps the
    /// buffer it already had and nothing blob-sized is allocated. (A dedup hit
    /// on a disk-backed store is not free: it still builds the blob's path,
    /// which is one allocation, and `stat`s it. It just does not copy or
    /// rewrite the bytes.)
    pub fn put(&self, bytes: &[u8]) -> Result<BlobHash, StoreError> {
        let hash = BlobHash::of(bytes);
        if bytes.len() as u64 > self.config.max_blob_bytes {
            return Err(StoreError::BlobTooLarge {
                hash: hash.to_hex(),
                len: bytes.len() as u64,
                max: self.config.max_blob_bytes,
            });
        }
        let mut inner = self.lock();

        let (next_refs, is_new, was_resident) = match inner.entries.get(&hash) {
            Some(e) => (
                e.refs
                    .checked_add(1)
                    .ok_or_else(|| StoreError::RefcountOverflow(hash.to_hex()))?,
                false,
                e.data.is_some(),
            ),
            None => (1, true, false),
        };

        // Durability first: a blob is only visible in memory once its bytes and
        // its refcount are on disk, so a crash can never leave the cache
        // claiming a blob the disk does not have.
        //
        // A dedup hit only earns the right to skip the write if the file is
        // still there with the right length. It may not be: `gc` unlinks blob
        // files one at a time and rewrites the journal only after the whole
        // sweep, so a crash inside that window leaves an index entry whose file
        // is gone. Skipping the write there would make `put` return a hash for
        // bytes that can never be read back, and the entry would be stuck at
        // refs > 0 so `gc` could not even clear it. One `stat` per dedup hit is
        // cheap next to the `fsync` it guards.
        if let Some(disk) = &self.disk {
            if is_new || !disk.blob_present_with_len(hash, bytes.len() as u64)? {
                disk.write_blob(hash, bytes)?;
            }
            self.append_record(&mut inner, hash, next_refs)?;
        }

        match inner.entries.get_mut(&hash) {
            Some(e) => e.refs = next_refs,
            None => {
                inner.entries.insert(
                    hash,
                    Entry {
                        data: None,
                        refs: next_refs,
                        tick: None,
                    },
                );
            }
        }
        if was_resident {
            inner.touch(hash);
        } else {
            // We are holding the bytes anyway, so caching them now is strictly
            // cheaper than a read-through later.
            inner.set_resident(hash, Arc::from(bytes));
        }
        self.enforce_budget(&mut inner);
        Ok(hash)
    }

    /// Fetch a blob, reading through from disk (and repopulating the cache) if
    /// it is not resident.
    ///
    /// A blob read back from disk is re-hashed; if the bytes do not hash to the
    /// key they were stored under, [`StoreError::Corrupt`] is returned instead
    /// of the bytes.
    pub fn get(&self, hash: BlobHash) -> Result<Arc<[u8]>, StoreError> {
        let mut inner = self.lock();
        // Only blobs the store knows about are readable. A stray file in the
        // blob directory is garbage-collector fodder, never data.
        let Some(entry) = inner.entries.get(&hash) else {
            return Err(StoreError::NotFound(hash.to_hex()));
        };
        if let Some(data) = entry.data.clone() {
            inner.touch(hash);
            return Ok(data);
        }
        let Some(disk) = &self.disk else {
            return Err(StoreError::NotFound(hash.to_hex()));
        };
        let Some(bytes) = disk.read_blob(hash, self.config.max_blob_bytes)? else {
            return Err(StoreError::NotFound(hash.to_hex()));
        };
        let actual = BlobHash::of(&bytes);
        if actual != hash {
            return Err(StoreError::Corrupt {
                hash: hash.to_hex(),
                actual: actual.to_hex(),
            });
        }
        let data: Arc<[u8]> = Arc::from(bytes);
        inner.set_resident(hash, Arc::clone(&data));
        self.enforce_budget(&mut inner);
        Ok(data)
    }

    /// Is this blob known to the store (in memory or on disk)? Unreferenced
    /// blobs that have not been collected yet still count as present.
    pub fn contains(&self, hash: BlobHash) -> bool {
        self.lock().entries.contains_key(&hash)
    }

    /// Are this blob's bytes currently held in memory?
    pub fn is_resident(&self, hash: BlobHash) -> bool {
        self.lock()
            .entries
            .get(&hash)
            .is_some_and(|e| e.data.is_some())
    }

    /// Current refcount, or `None` if the blob is unknown.
    pub fn refcount(&self, hash: BlobHash) -> Option<u32> {
        self.lock().entries.get(&hash).map(|e| e.refs)
    }

    /// Take an extra reference to a blob that is already stored.
    pub fn retain(&self, hash: BlobHash) -> Result<u32, StoreError> {
        let mut inner = self.lock();
        let current = inner
            .entries
            .get(&hash)
            .ok_or_else(|| StoreError::NotFound(hash.to_hex()))?
            .refs;
        let next = current
            .checked_add(1)
            .ok_or_else(|| StoreError::RefcountOverflow(hash.to_hex()))?;
        if self.disk.is_some() {
            self.append_record(&mut inner, hash, next)?;
        }
        if let Some(e) = inner.entries.get_mut(&hash) {
            e.refs = next;
        }
        Ok(next)
    }

    /// Drop one reference, returning the remaining count.
    ///
    /// Releasing a hash the store never had, or one whose count is already
    /// zero, is an error: a silent no-op there would let double-releases
    /// corrupt the accounting invisibly.
    pub fn release(&self, hash: BlobHash) -> Result<u32, StoreError> {
        let mut inner = self.lock();
        let current = inner
            .entries
            .get(&hash)
            .ok_or_else(|| StoreError::NotFound(hash.to_hex()))?
            .refs;
        if current == 0 {
            return Err(StoreError::NotReferenced(hash.to_hex()));
        }
        let next = current - 1;
        if self.disk.is_some() {
            self.append_record(&mut inner, hash, next)?;
        }
        if let Some(e) = inner.entries.get_mut(&hash) {
            e.refs = next;
        }
        Ok(next)
    }

    /// Delete every unreferenced blob, from memory and from disk.
    ///
    /// Never runs implicitly. Referenced blobs, and any file in the store
    /// directory this crate did not write, are left untouched.
    pub fn gc(&self) -> Result<GcReport, StoreError> {
        let mut inner = self.lock();
        let mut report = GcReport::default();

        let dead: Vec<BlobHash> = inner
            .entries
            .iter()
            .filter(|(_, e)| e.refs == 0)
            .map(|(h, _)| *h)
            .collect();

        for hash in dead {
            let resident = inner
                .entries
                .get(&hash)
                .and_then(|e| e.data.as_ref())
                .map_or(0, |d| d.len() as u64);
            let freed = match &self.disk {
                Some(disk) => disk.delete_blob(hash)?,
                None => resident,
            };
            inner.remove_entry(hash);
            report.blobs_removed += 1;
            report.bytes_reclaimed += freed;
        }

        if let Some(disk) = &self.disk {
            // Orphans: blob files with no index entry at all, e.g. written by a
            // run that crashed before its refcount record was appended.
            for (hash, path) in disk.scan_blobs()? {
                if !inner.entries.contains_key(&hash) {
                    report.bytes_reclaimed += disk.delete_file(&path)?;
                    report.blobs_removed += 1;
                }
            }
            report.bytes_reclaimed += disk.clean_tmp()?;
            self.compact_index(&mut inner)?;
        }
        Ok(report)
    }

    /// Number of known blobs (referenced or awaiting collection).
    pub fn len(&self) -> usize {
        self.lock().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().entries.is_empty()
    }

    /// Bytes currently held in the memory cache.
    pub fn resident_bytes(&self) -> u64 {
        self.lock().resident_bytes
    }

    pub fn config(&self) -> &StoreConfig {
        &self.config
    }

    /// Evict least-recently-used blobs until the cache fits its budget. Only
    /// meaningful for disk-backed stores, where an evicted blob can be read
    /// back.
    fn enforce_budget(&self, inner: &mut Inner) {
        if self.disk.is_none() {
            return;
        }
        while inner.resident_bytes > self.config.cache_bytes {
            let Some((tick, victim)) = inner.lru.iter().next().map(|(t, h)| (*t, *h)) else {
                break;
            };
            if inner.evict_one(victim) == 0 {
                // Defensive: a stale LRU key with nothing behind it would spin
                // this loop forever. Drop it and carry on.
                inner.lru.remove(&tick);
            }
        }
    }

    /// Size of the journal that would be written if it were compacted right now
    /// to hold `live` distinct entries.
    fn compacted_len(live: u64) -> u64 {
        INDEX_HEADER_LEN as u64 + live.saturating_mul(INDEX_RECORD_LEN as u64)
    }

    /// Durably record `(hash, refcount)`.
    ///
    /// Normally this appends one record and `fsync`s it. Two bounds are applied
    /// on top, both derived from [`StoreConfig::max_index_bytes`] — the same
    /// limit [`AssetStore::open`] enforces when it reads the journal back:
    ///
    /// * if the journal has no room left to grow under that limit, the change
    ///   is folded into a freshly compacted journal instead of appended. That
    ///   is just as durable and cannot exceed the compacted size;
    /// * if even a fully compacted journal would not fit, the operation fails
    ///   with [`StoreError::IndexTooLarge`] rather than writing a store that
    ///   could never be opened again.
    fn append_record(
        &self,
        inner: &mut Inner,
        hash: BlobHash,
        refs: u32,
    ) -> Result<(), StoreError> {
        let Some(disk) = &self.disk else {
            return Ok(());
        };

        // `entries` does not yet contain a brand-new hash: the caller inserts it
        // only after this returns, so count it here.
        let live = inner.entries.len() as u64 + u64::from(!inner.entries.contains_key(&hash));
        let limit = self.config.max_index_bytes;
        let compacted = Self::compacted_len(live);
        if compacted > limit {
            return Err(StoreError::IndexTooLarge {
                len: compacted,
                max: limit,
            });
        }
        // Spend at most half the headroom between compactions, so an append is
        // amortised O(1) while the file stays strictly under `limit`.
        // `saturating_sub` only because a release build must not wrap here if
        // this ever drifts away from the guard above; it cannot underflow today.
        let ceiling = compacted + limit.saturating_sub(compacted) / 2;
        let journal = Self::compacted_len(inner.appended);
        if journal.saturating_add(INDEX_RECORD_LEN as u64) > ceiling {
            return self.compact_index_with(inner, Some((hash, refs)));
        }

        if inner.index.is_none() {
            inner.index = Some(disk.open_index_append()?);
        }
        let mut rec = [0u8; INDEX_RECORD_LEN];
        rec[0..32].copy_from_slice(&hash.0);
        rec[32..36].copy_from_slice(&refs.to_le_bytes());
        {
            let file = inner
                .index
                .as_mut()
                .expect("index handle opened immediately above");
            file.write_all(&rec)
                .map_err(|e| io_err(disk.index_path(), e))?;
            file.sync_data().map_err(|e| io_err(disk.index_path(), e))?;
        }
        inner.appended += 1;

        let resident = inner.entries.len() as u64;
        if inner.appended >= self.config.compact_threshold && inner.appended >= resident * 2 {
            // The record above is already on disk and `fsync`ed, so the caller's
            // operation has succeeded no matter what happens here. A compaction
            // failure is therefore swallowed rather than propagated: returning
            // `Err` would tell the caller nothing happened while the journal
            // already says otherwise, and the next reopen would read back a
            // refcount the caller never agreed to. The cost of swallowing is a
            // journal that stays long and an `index` handle left `None`, which
            // the next append reopens.
            let _ = self.compact_index_with(inner, Some((hash, refs)));
        }
        Ok(())
    }

    fn compact_index(&self, inner: &mut Inner) -> Result<(), StoreError> {
        self.compact_index_with(inner, None)
    }

    /// Rewrite the journal from the in-memory entries. `pending` carries a
    /// record that was just appended for a hash whose in-memory entry has not
    /// been updated yet.
    fn compact_index_with(
        &self,
        inner: &mut Inner,
        pending: Option<(BlobHash, u32)>,
    ) -> Result<(), StoreError> {
        let Some(disk) = &self.disk else {
            return Ok(());
        };
        let mut snapshot: Vec<(BlobHash, u32)> = inner
            .entries
            .iter()
            .map(|(h, e)| {
                let refs = match pending {
                    Some((ph, pr)) if ph == *h => pr,
                    _ => e.refs,
                };
                (*h, refs)
            })
            .collect();
        if let Some((ph, pr)) = pending {
            if !inner.entries.contains_key(&ph) {
                snapshot.push((ph, pr));
            }
        }
        snapshot.sort_unstable();
        // Windows cannot rename over an open file: release the handle first.
        inner.index = None;
        let file = disk.compact_index(&snapshot)?;
        inner.index = Some(file);
        inner.appended = snapshot.len() as u64;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
