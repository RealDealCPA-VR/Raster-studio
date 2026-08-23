//! Disk backend for the content-addressed store.
//!
//! Layout under the store root:
//!
//! ```text
//! <root>/blobs/<aa>/<bb>/<64-hex>   blob bytes, sharded by the first two bytes
//! <root>/index                      append-only refcount journal
//! <root>/tmp/                       staging area for atomic renames
//! ```
//!
//! Every path is derived from a hash this crate computed itself (or from a file
//! name that was validated as exactly 64 hex characters), so no component of an
//! untrusted string is ever joined onto the root.
//!
//! That is not sufficient on its own: a store directory unpacked from someone
//! else's project package can plant a *symlink* at a path this crate builds
//! itself, which would redirect a read, a write or a delete outside the root.
//! So every path is also checked with `symlink_metadata` — which does not
//! follow links — before it is opened, written through or swept:
//!
//! * [`Disk::open`] refuses a store *root* that is not a real directory before
//!   it creates anything inside it, so a linked root never has `blobs/` or
//!   `tmp/` materialised at the far end of the link;
//! * [`Disk::open`] refuses a `blobs/` or `tmp/` that is not a real directory,
//!   so no write can be staged or renamed through a link;
//! * [`Disk::read_blob`] and [`Disk::load_index`] refuse anything that is not a
//!   regular file, so `File::open` never blocks on a FIFO and never reads a
//!   device or a file outside the root;
//! * [`Disk::scan_blobs`] and [`Disk::clean_tmp`] refuse to descend through a
//!   link, so garbage collection cannot delete a file this crate did not write.
//!
//! All writes are staged in `tmp/`, `fsync`ed, then renamed into place. The
//! containing directory is `fsync`ed too on platforms where that is meaningful.
//! A `rename` replaces a symlink sitting at the destination rather than
//! following it, so a planted link is repaired by the next write.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{hex, BlobHash, StoreError};

/// Magic bytes at the head of the refcount journal.
pub(crate) const INDEX_MAGIC: &[u8; 4] = b"RSAS";
/// Journal format version.
pub(crate) const INDEX_VERSION: u32 = 1;
/// Bytes of fixed header preceding the records.
pub(crate) const INDEX_HEADER_LEN: usize = 8;
/// `[32-byte hash][u32 refcount LE]`.
pub(crate) const INDEX_RECORD_LEN: usize = 36;

/// Counter used to give every staged temp file a unique name.
static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

pub(crate) fn io_err(path: &Path, source: std::io::Error) -> StoreError {
    StoreError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// A real directory, not a symlink pointing at one.
///
/// `Path::is_dir` follows links, which would let a store directory unpacked
/// from someone else's project package aim a read, a write or the garbage
/// collector at a directory outside the root. Every step that opens, writes or
/// unlinks therefore uses `symlink_metadata` instead.
fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// A real file, not a symlink pointing at one. See [`is_real_dir`].
fn is_real_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// `symlink_metadata` for a path that must be a plain file.
///
/// `Ok(None)` means "absent, or present but not a regular file". The two are
/// deliberately indistinguishable to the caller: reporting *what* was found at
/// an attacker-planted path would leak information about it, and every caller
/// treats both the same way (a read reports `NotFound`, a write replaces it).
fn regular_file_meta(path: &Path) -> Result<Option<fs::Metadata>, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_file() => Ok(Some(m)),
        Ok(_) => Ok(None),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_err(path, e)),
    }
}

/// Open a path that has already been confirmed to be a regular file, and
/// confirm it again through the open handle.
///
/// The second check closes the window between the `stat` and the `open`: on
/// unix an `fstat` of the descriptor cannot be redirected, so if the path was
/// swapped for a link or a device in between, this notices and refuses. A FIFO
/// is not reachable here at all — it is rejected before `open`, which is what
/// keeps `open` from blocking forever while the store mutex is held.
fn open_regular_file(path: &Path) -> Result<Option<File>, StoreError> {
    let f = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_err(path, e)),
    };
    let meta = f.metadata().map_err(|e| io_err(path, e))?;
    if !meta.file_type().is_file() {
        return Ok(None);
    }
    Ok(Some(f))
}

/// Handles to the on-disk store directories.
#[derive(Debug)]
pub(crate) struct Disk {
    root: PathBuf,
    blobs: PathBuf,
    tmp: PathBuf,
    index_path: PathBuf,
}

impl Disk {
    /// Create (or adopt) the directory structure at `root`.
    ///
    /// `create_dir_all` succeeds when the path already exists as a *symlink to*
    /// a directory, because it tests `is_dir()`, which follows links. Adopting
    /// such a directory would send every staged temp file, every shard
    /// `create_dir_all` and every `rename` through the link and out of the
    /// store root. The root and both directories are therefore re-checked with
    /// `symlink_metadata` afterwards and a linked one is refused here, before
    /// anything is written.
    ///
    /// The root is checked *first*, and on its own, because `blobs/` and `tmp/`
    /// are created *through* it: creating them before the root has been proven
    /// real would materialise two directories at the far end of a linked root
    /// even though the store is about to be refused. Every later refusal in this
    /// module then rests on a root that is known not to be a link.
    pub(crate) fn open(root: &Path) -> Result<Self, StoreError> {
        let disk = Self {
            root: root.to_path_buf(),
            blobs: root.join("blobs"),
            tmp: root.join("tmp"),
            index_path: root.join("index"),
        };
        // A missing root is this crate's to create; an existing one must be a
        // real directory. `create_dir_all` is a no-op on a symlink to a
        // directory, so the check below is what distinguishes the two.
        fs::create_dir_all(&disk.root).map_err(|e| io_err(&disk.root, e))?;
        if !is_real_dir(&disk.root) {
            return Err(StoreError::NotADirectory(disk.root.clone()));
        }
        fs::create_dir_all(&disk.blobs).map_err(|e| io_err(&disk.blobs, e))?;
        fs::create_dir_all(&disk.tmp).map_err(|e| io_err(&disk.tmp, e))?;
        for dir in [&disk.blobs, &disk.tmp] {
            if !is_real_dir(dir) {
                return Err(StoreError::NotADirectory(dir.clone()));
            }
        }
        // The journal is opened and read by path; a link or a device planted
        // there is refused for the same reason.
        if fs::symlink_metadata(&disk.index_path).is_ok_and(|m| !m.file_type().is_file()) {
            return Err(StoreError::NotAFile(disk.index_path.clone()));
        }
        Ok(disk)
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn index_path(&self) -> &Path {
        &self.index_path
    }

    /// Content-addressed path: `blobs/<first byte>/<second byte>/<full hex>`.
    ///
    /// Two levels of 256-way sharding keep any single directory small even for
    /// a project with millions of tiles, and the directories are created lazily
    /// so a small project only materialises the shards it uses.
    ///
    /// This is on the dedup hot path (`put` calls it for every insert, hit or
    /// miss), so the hex goes into a stack buffer and the whole path is built
    /// in one reservation instead of the four allocations a `to_hex()` plus a
    /// `join` chain would cost.
    pub(crate) fn blob_path(&self, hash: BlobHash) -> PathBuf {
        let mut buf = [0u8; 64];
        hex::encode32_into(&hash.0, &mut buf);
        let hex = std::str::from_utf8(&buf).expect("hex digits are ASCII");
        // <blobs>/aa/bb/<64 hex> — three separators, two shard names, 64 digits.
        let mut path = PathBuf::with_capacity(self.blobs.as_os_str().len() + 3 + 2 + 2 + 64);
        path.push(&self.blobs);
        path.push(&hex[0..2]);
        path.push(&hex[2..4]);
        path.push(hex);
        path
    }

    fn tmp_path(&self, tag: &str) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        self.tmp
            .join(format!("{tag}-{}-{n}.tmp", std::process::id()))
    }

    /// Write `bytes` to `path` atomically: staged temp file, fsync, rename,
    /// then fsync the destination directory.
    fn write_atomic(&self, path: &Path, tag: &str, bytes: &[u8]) -> Result<(), StoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
            // `create_dir_all` is satisfied by a symlink pointing at a
            // directory, which would put the rename below outside the root.
            if !is_real_dir(parent) {
                return Err(StoreError::NotADirectory(parent.to_path_buf()));
            }
        }
        let tmp = self.tmp_path(tag);
        let staged = (|| -> std::io::Result<()> {
            let mut f = File::create(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()
        })();
        if let Err(e) = staged {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(&tmp, e));
        }
        if let Err(e) = fs::rename(&tmp, path) {
            let _ = fs::remove_file(&tmp);
            return Err(io_err(path, e));
        }
        if let Some(parent) = path.parent() {
            fsync_dir(parent)?;
        }
        Ok(())
    }

    /// Create `blobs/aa` and `blobs/aa/bb`, refusing either level if it already
    /// exists as anything but a real directory.
    ///
    /// Checking only the deepest level would not be enough: if `blobs/aa` is a
    /// link out of the store, `blobs/aa/bb` resolves to a genuine directory at
    /// the far end and passes the check while every write still lands outside.
    /// So each level is created and verified on the way down.
    fn ensure_shard_dirs(&self, blob_path: &Path) -> Result<(), StoreError> {
        let lvl2 = blob_path
            .parent()
            .expect("a blob path always has a second-level shard");
        let lvl1 = lvl2
            .parent()
            .expect("a blob path always has a first-level shard");
        for dir in [lvl1, lvl2] {
            match fs::create_dir(dir) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(io_err(dir, e)),
            }
            if !is_real_dir(dir) {
                return Err(StoreError::NotADirectory(dir.to_path_buf()));
            }
        }
        Ok(())
    }

    pub(crate) fn write_blob(&self, hash: BlobHash, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.blob_path(hash);
        self.ensure_shard_dirs(&path)?;
        self.write_atomic(&path, "blob", bytes)
    }

    /// Is there a plain file of exactly `len` bytes at this hash's path?
    ///
    /// Used to decide whether a deduplicating `put` may skip the write. It is
    /// a single `stat`, which is cheap next to the `fsync` it guards, and it
    /// closes the window where `gc` has unlinked a blob file but not yet
    /// rewritten the journal that still lists the entry.
    pub(crate) fn blob_present_with_len(
        &self,
        hash: BlobHash,
        len: u64,
    ) -> Result<bool, StoreError> {
        let path = self.blob_path(hash);
        Ok(regular_file_meta(&path)?.is_some_and(|m| m.len() == len))
    }

    /// Read a blob back. `Ok(None)` means the file is absent — or that
    /// something that is not a regular file is sitting at the blob's path, in
    /// which case it is treated exactly like absence. A file larger than
    /// `max_bytes` is rejected before any buffer is allocated.
    ///
    /// The non-regular case is not paranoia: a store directory unpacked from an
    /// untrusted package can plant a symlink or a FIFO at a canonical blob
    /// path. Following it would (on unix) block `File::open` forever inside a
    /// `get` that holds the store mutex, deadlocking every other thread; and
    /// since a hash mismatch is reported with the hash it actually computed,
    /// reading an out-of-root file would turn `get` into an oracle that
    /// confirms that file's contents. `symlink_metadata` refuses before `open`.
    pub(crate) fn read_blob(
        &self,
        hash: BlobHash,
        max_bytes: u64,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let path = self.blob_path(hash);
        let Some(meta) = regular_file_meta(&path)? else {
            return Ok(None);
        };
        if meta.len() > max_bytes {
            return Err(StoreError::BlobTooLarge {
                hash: hash.to_hex(),
                len: meta.len(),
                max: max_bytes,
            });
        }
        let cap = usize::try_from(meta.len()).map_err(|_| StoreError::BlobTooLarge {
            hash: hash.to_hex(),
            len: meta.len(),
            max: max_bytes,
        })?;
        let Some(mut f) = open_regular_file(&path)? else {
            return Ok(None);
        };
        let mut buf = Vec::with_capacity(cap);
        // `take` bounds the read even if the file grew between stat and open.
        Read::by_ref(&mut f)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| io_err(&path, e))?;
        if buf.len() as u64 > max_bytes {
            return Err(StoreError::BlobTooLarge {
                hash: hash.to_hex(),
                len: buf.len() as u64,
                max: max_bytes,
            });
        }
        Ok(Some(buf))
    }

    /// Delete one blob file, returning the bytes reclaimed (0 if absent).
    pub(crate) fn delete_blob(&self, hash: BlobHash) -> Result<u64, StoreError> {
        let path = self.blob_path(hash);
        self.delete_file(&path)
    }

    /// Unlink `path`, returning the bytes reclaimed.
    ///
    /// The size comes from `symlink_metadata`, so a symlink planted at a path
    /// this crate owns contributes its own (tiny) size rather than the size of
    /// whatever it points at — `remove_file` unlinks the link, never the
    /// target, and reporting the target's length would leak it. A directory is
    /// left alone entirely: this crate never writes one where a file belongs,
    /// so it is not ours to remove.
    pub(crate) fn delete_file(&self, path: &Path) -> Result<u64, StoreError> {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io_err(path, e)),
        };
        if meta.file_type().is_dir() {
            return Ok(0);
        }
        let len = if meta.file_type().is_file() {
            meta.len()
        } else {
            0
        };
        match fs::remove_file(path) {
            Ok(()) => Ok(len),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(io_err(path, e)),
        }
    }

    /// Replay the refcount journal. Returns the records in file order (later
    /// records supersede earlier ones for the same hash) plus the record count.
    ///
    /// A torn tail (a partial record from a crash mid-append) is ignored rather
    /// than treated as corruption; anything else malformed is an error.
    pub(crate) fn load_index(
        &self,
        max_bytes: u64,
    ) -> Result<(Vec<(BlobHash, u32)>, u64), StoreError> {
        // Not a regular file (absent, or a planted link/FIFO/device): no
        // journal. `Disk::open` has already refused the planted cases outright.
        let Some(meta) = regular_file_meta(&self.index_path)? else {
            return Ok((Vec::new(), 0));
        };
        if meta.len() > max_bytes {
            return Err(StoreError::IndexTooLarge {
                len: meta.len(),
                max: max_bytes,
            });
        }
        let Some(mut f) = open_regular_file(&self.index_path)? else {
            return Ok((Vec::new(), 0));
        };
        let cap = usize::try_from(meta.len()).map_err(|_| StoreError::IndexTooLarge {
            len: meta.len(),
            max: max_bytes,
        })?;
        let mut buf = Vec::with_capacity(cap);
        Read::by_ref(&mut f)
            .take(max_bytes.saturating_add(1))
            .read_to_end(&mut buf)
            .map_err(|e| io_err(&self.index_path, e))?;
        if buf.len() as u64 > max_bytes {
            return Err(StoreError::IndexTooLarge {
                len: buf.len() as u64,
                max: max_bytes,
            });
        }
        if buf.is_empty() {
            // Created but never written (crash between create and header write).
            return Ok((Vec::new(), 0));
        }
        if buf.len() < INDEX_HEADER_LEN {
            return Err(StoreError::IndexCorrupt(
                "index shorter than its header".into(),
            ));
        }
        if &buf[0..4] != INDEX_MAGIC {
            return Err(StoreError::IndexCorrupt("bad index magic".into()));
        }
        let mut ver = [0u8; 4];
        ver.copy_from_slice(&buf[4..8]);
        let ver = u32::from_le_bytes(ver);
        if ver != INDEX_VERSION {
            return Err(StoreError::IndexCorrupt(format!(
                "unsupported index version {ver}"
            )));
        }
        let body = &buf[INDEX_HEADER_LEN..];
        let full = body.len() / INDEX_RECORD_LEN;
        let mut out = Vec::with_capacity(full);
        for rec in body.chunks_exact(INDEX_RECORD_LEN) {
            let mut h = [0u8; 32];
            h.copy_from_slice(&rec[0..32]);
            let mut c = [0u8; 4];
            c.copy_from_slice(&rec[32..36]);
            out.push((BlobHash(h), u32::from_le_bytes(c)));
        }
        Ok((out, full as u64))
    }

    /// Open the journal for appending, creating it with a header if needed.
    ///
    /// A non-regular file at the journal path counts as "does not exist", which
    /// makes the atomic write below `rename` over it — replacing the planted
    /// link rather than appending through it.
    pub(crate) fn open_index_append(&self) -> Result<File, StoreError> {
        let exists = regular_file_meta(&self.index_path)?
            .is_some_and(|m| m.len() >= INDEX_HEADER_LEN as u64);
        if !exists {
            let mut header = Vec::with_capacity(INDEX_HEADER_LEN);
            header.extend_from_slice(INDEX_MAGIC);
            header.extend_from_slice(&INDEX_VERSION.to_le_bytes());
            self.write_atomic(&self.index_path, "index", &header)?;
        }
        OpenOptions::new()
            .append(true)
            .open(&self.index_path)
            .map_err(|e| io_err(&self.index_path, e))
    }

    /// Rewrite the journal with exactly `entries`, dropping superseded records.
    ///
    /// The caller must have dropped its append handle first: on Windows a file
    /// cannot be renamed over while it is open.
    pub(crate) fn compact_index(&self, entries: &[(BlobHash, u32)]) -> Result<File, StoreError> {
        let mut buf = Vec::with_capacity(INDEX_HEADER_LEN + entries.len() * INDEX_RECORD_LEN);
        buf.extend_from_slice(INDEX_MAGIC);
        buf.extend_from_slice(&INDEX_VERSION.to_le_bytes());
        for (h, c) in entries {
            buf.extend_from_slice(&h.0);
            buf.extend_from_slice(&c.to_le_bytes());
        }
        self.write_atomic(&self.index_path, "index", &buf)?;
        self.open_index_append()
    }

    /// Enumerate every blob file whose name is a canonical 64-hex hash sitting
    /// in the shard directory that hash belongs to.
    ///
    /// Files that do not match are left alone and never reported, so garbage
    /// collection can never delete something this crate did not write.
    ///
    /// Neither shard level is followed through a symlink, so a store directory
    /// unpacked from an untrusted package cannot point the collector at files
    /// outside the root.
    pub(crate) fn scan_blobs(&self) -> Result<Vec<(BlobHash, PathBuf)>, StoreError> {
        let mut out = Vec::new();
        if !is_real_dir(&self.blobs) {
            return Ok(out);
        }
        let top = match fs::read_dir(&self.blobs) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(io_err(&self.blobs, e)),
        };
        for lvl1 in top {
            let lvl1 = lvl1.map_err(|e| io_err(&self.blobs, e))?;
            let l1_name = lvl1.file_name();
            let Some(l1) = l1_name.to_str() else { continue };
            if l1.len() != 2 || !is_real_dir(&lvl1.path()) {
                continue;
            }
            let inner = match fs::read_dir(lvl1.path()) {
                Ok(d) => d,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(io_err(&lvl1.path(), e)),
            };
            for lvl2 in inner {
                let lvl2 = lvl2.map_err(|e| io_err(&lvl1.path(), e))?;
                let l2_name = lvl2.file_name();
                let Some(l2) = l2_name.to_str() else { continue };
                if l2.len() != 2 || !is_real_dir(&lvl2.path()) {
                    continue;
                }
                let files = match fs::read_dir(lvl2.path()) {
                    Ok(d) => d,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(io_err(&lvl2.path(), e)),
                };
                for f in files {
                    let f = f.map_err(|e| io_err(&lvl2.path(), e))?;
                    let name = f.file_name();
                    let Some(name) = name.to_str() else { continue };
                    let Some(bytes) = hex::decode32(name) else {
                        continue;
                    };
                    // Only accept a file that lives in the shard its own hash
                    // designates, in canonical lowercase form.
                    let hash = BlobHash(bytes);
                    let canonical = hash.to_hex();
                    if canonical != name || &canonical[0..2] != l1 || &canonical[2..4] != l2 {
                        continue;
                    }
                    if is_real_file(&f.path()) {
                        out.push((hash, f.path()));
                    }
                }
            }
        }
        Ok(out)
    }

    /// Remove staged temp files left behind by an interrupted write.
    ///
    /// Like [`Disk::scan_blobs`], this refuses to descend through a symlink: if
    /// `<root>/tmp` is a link, or an entry inside it is, nothing is deleted.
    pub(crate) fn clean_tmp(&self) -> Result<u64, StoreError> {
        let mut freed = 0;
        if !is_real_dir(&self.tmp) {
            return Ok(0);
        }
        let dir = match fs::read_dir(&self.tmp) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(io_err(&self.tmp, e)),
        };
        for entry in dir {
            let entry = entry.map_err(|e| io_err(&self.tmp, e))?;
            let path = entry.path();
            if is_real_file(&path) {
                freed += self.delete_file(&path)?;
            }
        }
        Ok(freed)
    }
}

/// `fsync` a directory so a rename into it is durable.
///
/// This is a POSIX guarantee. On Windows a directory handle cannot be opened
/// with the plain `File` API (it needs `FILE_FLAG_BACKUP_SEMANTICS`), and NTFS
/// journals the metadata of a `MoveFileEx` replace itself, so this is a no-op
/// there rather than a silent failure.
#[cfg(unix)]
fn fsync_dir(path: &Path) -> Result<(), StoreError> {
    let f = File::open(path).map_err(|e| io_err(path, e))?;
    f.sync_all().map_err(|e| io_err(path, e))
}

#[cfg(not(unix))]
fn fsync_dir(path: &Path) -> Result<(), StoreError> {
    let _ = path;
    Ok(())
}
