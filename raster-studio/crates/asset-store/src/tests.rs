use std::fs;
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;

use tempfile::TempDir;

use super::*;
use crate::disk;

// ---------------------------------------------------------------- helpers --

fn blob_file(root: &Path, hash: BlobHash) -> PathBuf {
    let hex = hash.to_hex();
    root.join("blobs")
        .join(&hex[0..2])
        .join(&hex[2..4])
        .join(&hex)
}

fn blob_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(&root.join("blobs"), &mut out);
    out.sort();
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_files(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn tmp_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    collect_files(&root.join("tmp"), &mut out);
    out
}

fn small_cache(bytes: u64) -> StoreConfig {
    StoreConfig {
        cache_bytes: bytes,
        ..StoreConfig::default()
    }
}

fn blob(tag: u8, len: usize) -> Vec<u8> {
    vec![tag; len]
}

// ------------------------------------------------------------------- hash --

#[test]
fn hex_matches_known_blake3_vector() {
    // BLAKE3 of the empty input, the published test vector.
    assert_eq!(
        BlobHash::of(b"").to_hex(),
        "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
    );
    assert_eq!(BlobHash::of(b"x").to_hex().len(), 64);
}

#[test]
fn hex_round_trips_through_from_hex() {
    let h = BlobHash::of(b"round trip");
    assert_eq!(BlobHash::from_hex(&h.to_hex()), Some(h));
    assert_eq!(BlobHash::from_hex("nope"), None);
}

#[test]
fn store_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<AssetStore>();
}

// --------------------------------------------------------- in-memory core --

#[test]
fn dedup_same_bytes() {
    let s = AssetStore::new();
    let h1 = s.put(b"hello world").unwrap();
    let h2 = s.put(b"hello world").unwrap();
    assert_eq!(h1, h2);
    assert_eq!(s.len(), 1, "identical blobs stored once");
    assert_eq!(s.refcount(h1), Some(2), "one entry holds both counts");
    assert_eq!(&*s.get(h1).unwrap(), b"hello world");
}

#[test]
fn put_borrows_its_input() {
    // `put` takes a slice, not a `Vec`, so the caller keeps the buffer it
    // already had and a dedup hit never copies the blob. It is not "free" on a
    // disk-backed store — it still builds a path and stats it — so this asserts
    // only what is true: the input survives and no second copy is made.
    let s = AssetStore::new();
    let owned = b"reused buffer".to_vec();
    let h = s.put(&owned).unwrap();
    let again = s.put(&owned).unwrap();
    assert_eq!(h, again);
    assert_eq!(owned, b"reused buffer".to_vec(), "input still usable");

    // Same on the disk-backed path, where the dedup hit must also skip the
    // write rather than storing the bytes twice.
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let h = s.put(&owned).unwrap();
    assert_eq!(s.put(&owned).unwrap(), h);
    assert_eq!(owned, b"reused buffer".to_vec(), "input still usable");
    assert_eq!(blob_files(dir.path()).len(), 1, "one copy on disk");
    assert_eq!(s.refcount(h), Some(2));
}

#[test]
fn release_of_unknown_hash_is_not_found() {
    let s = AssetStore::new();
    let ghost = BlobHash::of(b"never stored");
    let err = s.release(ghost).unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(ref h) if *h == ghost.to_hex()),
        "expected NotFound, got {err:?}"
    );
}

#[test]
fn double_release_is_reported_not_swallowed() {
    let s = AssetStore::new();
    let h = s.put(b"data").unwrap();
    assert_eq!(s.release(h).unwrap(), 0);
    let err = s.release(h).unwrap_err();
    assert!(
        matches!(err, StoreError::NotReferenced(_)),
        "expected NotReferenced, got {err:?}"
    );
    assert_eq!(s.refcount(h), Some(0), "count did not go negative");
}

#[test]
fn refcount_survives_partial_release() {
    let s = AssetStore::new();
    let h = s.put(b"data").unwrap();
    let _ = s.put(b"data").unwrap();
    assert_eq!(s.release(h).unwrap(), 1);
    assert!(s.contains(h), "still referenced");
    assert_eq!(s.release(h).unwrap(), 0);
    assert!(s.contains(h), "unreferenced blobs wait for an explicit gc");
    let report = s.gc().unwrap();
    assert_eq!(report.blobs_removed, 1);
    assert_eq!(report.bytes_reclaimed, 4);
    assert!(!s.contains(h), "gc collected it");
    assert!(s.is_empty());
}

#[test]
fn gc_keeps_referenced_blobs_in_memory_only_stores() {
    let s = AssetStore::new();
    let live = s.put(b"live").unwrap();
    let dead = s.put(b"dead").unwrap();
    s.release(dead).unwrap();
    let report = s.gc().unwrap();
    assert_eq!(report.blobs_removed, 1);
    assert!(s.contains(live));
    assert!(!s.contains(dead));
}

#[test]
fn get_unknown_is_not_found() {
    let s = AssetStore::new();
    let err = s.get(BlobHash::of(b"absent")).unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "got {err:?}");
}

#[test]
fn retain_adds_a_reference_and_rejects_unknown() {
    let s = AssetStore::new();
    let h = s.put(b"shared").unwrap();
    assert_eq!(s.retain(h).unwrap(), 2);
    assert_eq!(s.refcount(h), Some(2));
    let err = s.retain(BlobHash::of(b"nothing")).unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "got {err:?}");
}

#[test]
fn memory_only_store_never_evicts() {
    // A memory-only store has nowhere to read an evicted blob back from, so the
    // byte budget must not apply to it.
    let s = AssetStore::with_config(small_cache(1));
    let a = s.put(&blob(1, 64)).unwrap();
    let b = s.put(&blob(2, 64)).unwrap();
    assert!(s.is_resident(a) && s.is_resident(b));
    assert_eq!(&*s.get(a).unwrap(), &blob(1, 64)[..]);
    assert_eq!(s.resident_bytes(), 128);
}

// -------------------------------------------------------------- disk core --

#[test]
fn put_writes_one_sharded_file_and_leaves_no_temp_files() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let h = s.put(b"on disk").unwrap();

    let expected = blob_file(dir.path(), h);
    assert!(expected.is_file(), "blob written to its sharded path");
    assert_eq!(fs::read(&expected).unwrap(), b"on disk");
    assert_eq!(blob_files(dir.path()), vec![expected]);
    assert!(
        tmp_files(dir.path()).is_empty(),
        "staged temp files renamed away"
    );
    assert_eq!(s.root(), Some(dir.path()));
}

#[test]
fn dedup_across_a_memory_disk_round_trip() {
    let dir = TempDir::new().unwrap();
    let payload = blob(7, 4096);

    let s = AssetStore::open(dir.path()).unwrap();
    let h = s.put(&payload).unwrap();
    let h2 = s.put(&payload).unwrap();
    assert_eq!(h, h2);
    assert_eq!(s.len(), 1);
    assert_eq!(blob_files(dir.path()).len(), 1, "stored once");
    drop(s);

    // Reopened: the same content must dedup against what is already on disk,
    // without writing a second copy.
    let s = AssetStore::open(dir.path()).unwrap();
    assert!(!s.is_resident(h), "nothing is cached until it is read");
    let h3 = s.put(&payload).unwrap();
    assert_eq!(h3, h);
    assert_eq!(s.refcount(h), Some(3));
    assert_eq!(blob_files(dir.path()).len(), 1, "still one copy on disk");
    assert_eq!(&*s.get(h).unwrap(), &payload[..]);
}

#[test]
fn put_of_a_known_intact_blob_does_not_rewrite_the_file() {
    // Deduplication is the point: a second `put` of content the store already
    // has, whose file is present at the right length, must not pay for another
    // write and fsync. (The flip side, documented rather than hidden: `put`
    // checks presence and length, not content, so it does not repair silent
    // bit rot. `get` catches that by re-hashing.)
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let payload = blob(3, 512);
    let h = s.put(&payload).unwrap();
    let path = blob_file(dir.path(), h);
    assert_eq!(fs::metadata(&path).unwrap().len(), 512);

    // Same length, different bytes: indistinguishable from intact by `stat`.
    fs::write(&path, blob(4, 512)).unwrap();
    s.put(&payload).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        blob(4, 512),
        "the dedup hit skipped the write entirely"
    );
    assert_eq!(s.refcount(h), Some(2));
}

#[test]
fn put_rewrites_a_blob_whose_file_vanished_behind_the_store() {
    // Exactly the state a crash part-way through `gc` leaves: the blob file has
    // been unlinked but the journal still lists the entry. A `put` that trusted
    // the index alone would skip the write, return Ok, and hand back a hash
    // whose bytes can never be read again — and with refs back above zero, `gc`
    // would refuse to clear the wreckage.
    let dir = TempDir::new().unwrap();
    let payload = blob(3, 512);
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        let h = s.put(&payload).unwrap();
        s.release(h).unwrap();
        h
    };
    let path = blob_file(dir.path(), h);
    fs::remove_file(&path).unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    assert!(s.contains(h), "the index still lists it");
    let again = s.put(&payload).unwrap();
    assert_eq!(again, h);
    assert_eq!(s.refcount(h), Some(1));
    assert!(path.is_file(), "put stored the bytes it claimed to store");
    assert_eq!(fs::read(&path).unwrap(), payload);
    drop(s);

    // And durably: readable from a cold cache, not just from the memory copy
    // `put` happened to leave behind.
    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(&*s.get(h).unwrap(), &payload[..]);
}

#[test]
fn put_rewrites_a_blob_whose_file_was_truncated() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let payload = blob(6, 256);
    let h = s.put(&payload).unwrap();
    let path = blob_file(dir.path(), h);
    fs::write(&path, b"").unwrap();

    s.put(&payload).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        payload,
        "a wrong-length file is not a dedup hit"
    );
}

#[test]
fn refcounts_survive_reopen() {
    let dir = TempDir::new().unwrap();
    let (a, b) = {
        let s = AssetStore::open(dir.path()).unwrap();
        let a = s.put(b"alpha").unwrap();
        s.put(b"alpha").unwrap();
        s.put(b"alpha").unwrap();
        let b = s.put(b"beta").unwrap();
        s.release(a).unwrap();
        assert_eq!(s.refcount(a), Some(2));
        (a, b)
    };

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(a), Some(2), "refcount persisted across reopen");
    assert_eq!(s.refcount(b), Some(1));
    assert_eq!(s.len(), 2);
    assert_eq!(&*s.get(a).unwrap(), b"alpha");
}

#[test]
fn zero_refcount_survives_reopen_and_is_collected_later() {
    let dir = TempDir::new().unwrap();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        let h = s.put(b"soon garbage").unwrap();
        s.release(h).unwrap();
        h
    };
    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(h), Some(0), "the zero itself is persisted");
    assert!(blob_file(dir.path(), h).is_file(), "gc is never implicit");
    s.gc().unwrap();
    assert!(!blob_file(dir.path(), h).exists());
}

#[test]
fn gc_removes_exactly_the_unreferenced_blobs() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let live = s.put(b"keep me").unwrap();
    let dead = s.put(b"drop me").unwrap();
    s.release(dead).unwrap();

    // An orphan: a well-formed blob file with no index entry, as a crash
    // between the blob write and the refcount append would leave.
    let orphan_bytes = b"orphaned".to_vec();
    let orphan = BlobHash::of(&orphan_bytes);
    let orphan_path = blob_file(dir.path(), orphan);
    fs::create_dir_all(orphan_path.parent().unwrap()).unwrap();
    fs::write(&orphan_path, &orphan_bytes).unwrap();

    // Two files this crate did not write, which must never be touched: one
    // with a name that is not a hash at all, and one whose name is valid hex
    // but which sits in a shard that hash does not belong to.
    let stray = blob_file(dir.path(), live)
        .parent()
        .unwrap()
        .join("README.txt");
    fs::write(&stray, b"not ours").unwrap();
    let misplaced = blob_file(dir.path(), live)
        .parent()
        .unwrap()
        .join(BlobHash::of(b"somewhere else entirely").to_hex());
    assert!(misplaced != blob_file(dir.path(), live));
    fs::write(&misplaced, b"also not ours").unwrap();

    let report = s.gc().unwrap();
    assert_eq!(report.blobs_removed, 2, "the released blob and the orphan");
    assert_eq!(report.bytes_reclaimed, 7 + 8);

    assert!(
        blob_file(dir.path(), live).is_file(),
        "referenced blob kept"
    );
    assert!(!blob_file(dir.path(), dead).exists());
    assert!(!orphan_path.exists());
    assert!(stray.is_file(), "foreign file untouched");
    assert_eq!(fs::read(&stray).unwrap(), b"not ours");
    assert!(misplaced.is_file(), "file in the wrong shard untouched");
    assert_eq!(fs::read(&misplaced).unwrap(), b"also not ours");

    assert!(s.contains(live));
    assert!(!s.contains(dead));
    assert_eq!(&*s.get(live).unwrap(), b"keep me");
    drop(s);

    // The collection is durable: the dead entry is gone from the journal too.
    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.len(), 1);
    assert_eq!(s.refcount(live), Some(1));
    assert!(!s.contains(dead));
}

#[test]
fn gc_sweeps_stale_temp_files() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    s.put(b"anything").unwrap();
    let stale = dir.path().join("tmp").join("blob-999-999.tmp");
    fs::write(&stale, b"half written").unwrap();
    let report = s.gc().unwrap();
    assert!(!stale.exists(), "interrupted write cleaned up");
    assert_eq!(report.bytes_reclaimed, 12);
}

// ------------------------------------------------------------------- LRU ---

#[test]
fn lru_evicts_least_recently_used_and_read_through_repopulates() {
    let dir = TempDir::new().unwrap();
    // Three 64-byte blobs fit (192); a fourth (256) does not.
    let s = AssetStore::open_with(dir.path(), small_cache(200)).unwrap();

    let a = s.put(&blob(1, 64)).unwrap();
    let b = s.put(&blob(2, 64)).unwrap();
    let c = s.put(&blob(3, 64)).unwrap();
    assert_eq!(s.resident_bytes(), 192);

    // Touch A so B becomes the least recently used.
    assert_eq!(&*s.get(a).unwrap(), &blob(1, 64)[..]);

    let d = s.put(&blob(4, 64)).unwrap();
    assert_eq!(s.resident_bytes(), 192, "budget enforced");
    assert!(!s.is_resident(b), "B was the least recently used");
    assert!(s.is_resident(a) && s.is_resident(c) && s.is_resident(d));
    assert!(s.contains(b), "evicted, not forgotten");

    // Read-through repopulates from disk with the right bytes.
    assert_eq!(&*s.get(b).unwrap(), &blob(2, 64)[..]);
    assert!(s.is_resident(b));
    assert!(!s.is_resident(c), "C was next in line");
    assert_eq!(s.resident_bytes(), 192);
}

#[test]
fn every_blob_is_readable_even_when_the_cache_holds_one() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open_with(dir.path(), small_cache(64)).unwrap();
    let hashes: Vec<_> = (0..8u8).map(|i| s.put(&blob(i, 64)).unwrap()).collect();
    assert!(s.resident_bytes() <= 64);
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(&*s.get(*h).unwrap(), &blob(i as u8, 64)[..]);
    }
}

// ------------------------------------------------------------- corruption --

#[test]
fn corrupt_blob_on_disk_is_detected_by_hash_mismatch() {
    let dir = TempDir::new().unwrap();
    let good = b"the real bytes".to_vec();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&good).unwrap()
    };

    // Bit rot / tampering: same length, different content.
    let path = blob_file(dir.path(), h);
    fs::write(&path, b"the fake bytes").unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    let err = s.get(h).unwrap_err();
    match err {
        StoreError::Corrupt {
            ref hash,
            ref actual,
        } => {
            assert_eq!(*hash, h.to_hex());
            assert_eq!(*actual, BlobHash::of(b"the fake bytes").to_hex());
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert!(!s.is_resident(h), "corrupt bytes never enter the cache");
}

#[test]
fn missing_blob_file_reads_as_not_found() {
    let dir = TempDir::new().unwrap();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(b"vanishing").unwrap()
    };
    fs::remove_file(blob_file(dir.path(), h)).unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let err = s.get(h).unwrap_err();
    assert!(matches!(err, StoreError::NotFound(_)), "got {err:?}");
}

#[test]
fn oversized_put_is_rejected_before_it_is_written() {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        max_blob_bytes: 8,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    let err = s.put(&blob(9, 9)).unwrap_err();
    assert!(
        matches!(err, StoreError::BlobTooLarge { .. }),
        "got {err:?}"
    );
    assert!(s.is_empty());
    assert!(blob_files(dir.path()).is_empty(), "nothing hit the disk");
}

#[test]
fn oversized_blob_on_disk_is_rejected_before_allocating() {
    let dir = TempDir::new().unwrap();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&blob(5, 4096)).unwrap()
    };
    let cfg = StoreConfig {
        max_blob_bytes: 16,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    let err = s.get(h).unwrap_err();
    match err {
        StoreError::BlobTooLarge { len, max, .. } => {
            assert_eq!(len, 4096);
            assert_eq!(max, 16);
        }
        other => panic!("expected BlobTooLarge, got {other:?}"),
    }
}

#[test]
fn refcount_overflow_is_reported() {
    // A hand-written journal is untrusted input: it can claim any count.
    let dir = TempDir::new().unwrap();
    let payload = b"saturated".to_vec();
    let h = BlobHash::of(&payload);
    {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&payload).unwrap();
    }
    let mut buf = Vec::new();
    buf.extend_from_slice(disk::INDEX_MAGIC);
    buf.extend_from_slice(&disk::INDEX_VERSION.to_le_bytes());
    buf.extend_from_slice(&h.0);
    buf.extend_from_slice(&u32::MAX.to_le_bytes());
    fs::write(dir.path().join("index"), &buf).unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(h), Some(u32::MAX));
    let err = s.put(&payload).unwrap_err();
    assert!(
        matches!(err, StoreError::RefcountOverflow(_)),
        "got {err:?}"
    );
    assert_eq!(s.refcount(h), Some(u32::MAX), "count did not wrap to zero");
}

// ----------------------------------------------------------------- index ---

#[test]
fn torn_journal_tail_is_tolerated_and_healed() {
    let dir = TempDir::new().unwrap();
    let (a, b) = {
        let s = AssetStore::open(dir.path()).unwrap();
        let a = s.put(b"alpha").unwrap();
        let b = s.put(b"beta").unwrap();
        s.put(b"beta").unwrap();
        (a, b)
    };
    // A crash mid-append leaves a partial record behind.
    let index = dir.path().join("index");
    let mut f = fs::OpenOptions::new().append(true).open(&index).unwrap();
    f.write_all(&[0xAB; 7]).unwrap();
    drop(f);

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(a), Some(1));
    assert_eq!(s.refcount(b), Some(2));
    // The tail must have been truncated, not appended after: otherwise every
    // record written from here on is misaligned garbage.
    let c = s.put(b"gamma").unwrap();
    drop(s);

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(a), Some(1));
    assert_eq!(s.refcount(b), Some(2));
    assert_eq!(s.refcount(c), Some(1));
    assert_eq!(s.len(), 3);
}

#[test]
fn journal_with_bad_magic_is_rejected() {
    let dir = TempDir::new().unwrap();
    {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(b"alpha").unwrap();
    }
    let index = dir.path().join("index");
    let mut bytes = fs::read(&index).unwrap();
    bytes[0] = b'X';
    fs::write(&index, &bytes).unwrap();

    let err = AssetStore::open(dir.path()).unwrap_err();
    assert!(matches!(err, StoreError::IndexCorrupt(_)), "got {err:?}");
}

#[test]
fn journal_with_unknown_version_is_rejected() {
    let dir = TempDir::new().unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(disk::INDEX_MAGIC);
    bytes.extend_from_slice(&99u32.to_le_bytes());
    fs::create_dir_all(dir.path()).unwrap();
    fs::write(dir.path().join("index"), &bytes).unwrap();
    let err = AssetStore::open(dir.path()).unwrap_err();
    assert!(matches!(err, StoreError::IndexCorrupt(_)), "got {err:?}");
}

#[test]
fn oversized_journal_is_rejected_before_allocating() {
    let dir = TempDir::new().unwrap();
    {
        let s = AssetStore::open(dir.path()).unwrap();
        for i in 0..8u8 {
            s.put(&blob(i, 4)).unwrap();
        }
    }
    let cfg = StoreConfig {
        max_index_bytes: 16,
        ..StoreConfig::default()
    };
    let err = AssetStore::open_with(dir.path(), cfg).unwrap_err();
    assert!(
        matches!(err, StoreError::IndexTooLarge { .. }),
        "got {err:?}"
    );
}

#[test]
fn journal_compaction_preserves_refcounts() {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        compact_threshold: 4,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg.clone()).unwrap();
    let a = s.put(b"alpha").unwrap();
    let b = s.put(b"beta").unwrap();
    for _ in 0..20 {
        s.put(b"alpha").unwrap();
        s.release(a).unwrap();
    }
    s.put(b"beta").unwrap();
    let (ra, rb) = (s.refcount(a).unwrap(), s.refcount(b).unwrap());
    drop(s);

    let index_len = fs::metadata(dir.path().join("index")).unwrap().len();
    assert!(
        index_len <= (disk::INDEX_HEADER_LEN + 8 * disk::INDEX_RECORD_LEN) as u64,
        "journal compacted, is {index_len} bytes"
    );

    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    assert_eq!(s.refcount(a), Some(ra));
    assert_eq!(s.refcount(b), Some(rb));
    assert_eq!(&*s.get(a).unwrap(), b"alpha");
    assert_eq!(&*s.get(b).unwrap(), b"beta");
}

#[test]
fn journal_never_outgrows_the_bound_that_reopening_enforces() {
    // `open` refuses a journal larger than `max_index_bytes`, so writes have to
    // keep it under that same number. Five live blobs compact to 8 + 5*36 = 188
    // bytes, leaving 12 bytes of headroom under this cap; a growth heuristic
    // that only watches the live/appended ratio runs the journal straight past
    // 200 and the store can then never be opened again.
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        compact_threshold: 4,
        max_index_bytes: 200,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg.clone()).unwrap();
    let hashes: Vec<BlobHash> = (0..5u8).map(|i| s.put(&blob(i, 8)).unwrap()).collect();
    // Not a multiple of the blob count, so the run does not happen to end on a
    // compaction boundary.
    for i in 0..44usize {
        s.retain(hashes[i % hashes.len()]).unwrap();
    }
    let counts: Vec<u32> = hashes.iter().map(|h| s.refcount(*h).unwrap()).collect();
    assert_eq!(counts.iter().sum::<u32>(), 5 + 44);
    drop(s);

    let len = fs::metadata(dir.path().join("index")).unwrap().len();
    assert!(
        len <= cfg.max_index_bytes,
        "journal is {len} bytes, over the {} it will be read back under",
        cfg.max_index_bytes
    );

    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    for (h, want) in hashes.iter().zip(&counts) {
        assert_eq!(s.refcount(*h), Some(*want), "refcount survived compaction");
    }
    assert_eq!(s.len(), 5);
}

#[test]
fn a_blob_whose_entry_could_never_be_read_back_is_refused() {
    // 8 + 2*36 = 80 bytes fits; a third entry (116) does not. Writing it anyway
    // would produce a store that no longer opens, so `put` fails instead.
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        max_index_bytes: 100,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg.clone()).unwrap();
    let a = s.put(b"one").unwrap();
    let b = s.put(b"two").unwrap();

    let err = s.put(b"three").unwrap_err();
    match err {
        StoreError::IndexTooLarge { len, max } => {
            assert_eq!(len, 116);
            assert_eq!(max, 100);
        }
        other => panic!("expected IndexTooLarge, got {other:?}"),
    }
    assert!(
        !s.contains(BlobHash::of(b"three")),
        "refused, not half-added"
    );

    // The refused blob's file was staged before the index said no; it is an
    // orphan like any other, and gc reclaims it.
    let report = s.gc().unwrap();
    assert_eq!(report.blobs_removed, 1);
    assert_eq!(report.bytes_reclaimed, 5);
    drop(s);

    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    assert_eq!(s.refcount(a), Some(1));
    assert_eq!(s.refcount(b), Some(1));
    assert_eq!(s.len(), 2, "the store still opens");
}

#[test]
fn a_failed_compaction_leaves_memory_and_the_journal_agreeing() {
    let dir = TempDir::new().unwrap();
    let cfg = StoreConfig {
        compact_threshold: 4,
        ..StoreConfig::default()
    };
    let s = AssetStore::open_with(dir.path(), cfg.clone()).unwrap();
    let a = s.put(b"alpha").unwrap();
    let b = s.put(b"beta").unwrap();

    // Compaction stages the rewritten journal in <root>/tmp, so removing that
    // directory makes every compaction attempt fail. Appends are unaffected:
    // they go through the handle the store already holds.
    fs::remove_dir_all(dir.path().join("tmp")).unwrap();

    assert_eq!(s.retain(a).unwrap(), 2);
    assert_eq!(
        s.retain(a).unwrap(),
        3,
        "the record was already fsynced, so the operation succeeded"
    );
    assert_eq!(s.refcount(a), Some(3));
    assert_eq!(s.refcount(b), Some(1));
    drop(s);

    // The durable journal must say exactly what the caller was told. Failing
    // the write after its record is on disk would inflate this by one.
    let s = AssetStore::open_with(dir.path(), cfg).unwrap();
    assert_eq!(s.refcount(a), Some(3));
    assert_eq!(s.refcount(b), Some(1));
    assert_eq!(&*s.get(a).unwrap(), b"alpha");
}

// ------------------------------------------ non-regular files (portable) ---
//
// The symlink tests below are the sharp end of this hardening, but they need a
// privilege Windows does not grant by default. These three reach the same
// `symlink_metadata` checks with a *directory* planted where a file belongs (or
// a file where a directory belongs), which needs no privilege at all, so the
// checks have real coverage on every platform and in every run.

#[test]
fn a_directory_planted_at_a_blob_path_reads_as_absent() {
    // Anything that is not a regular file at a canonical blob path must be
    // indistinguishable from absence. Opening it instead is what makes a FIFO
    // deadlock `get` on unix, and what turns a link into a content oracle.
    let dir = TempDir::new().unwrap();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(b"the real blob").unwrap()
    };
    let path = blob_file(dir.path(), h);
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    let err = s.get(h).unwrap_err();
    assert!(
        matches!(err, StoreError::NotFound(_)),
        "expected NotFound (never an open of the planted entry), got {err:?}"
    );
    assert!(!s.is_resident(h));
}

#[test]
fn a_directory_planted_at_the_journal_path_is_refused_at_open() {
    let dir = TempDir::new().unwrap();
    drop(AssetStore::open(dir.path()).unwrap());
    let index = dir.path().join("index");
    fs::remove_file(&index).unwrap();
    fs::create_dir(&index).unwrap();

    let err = AssetStore::open(dir.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::NotAFile(_)),
        "expected NotAFile, got {err:?}"
    );
}

#[test]
fn a_file_where_a_shard_directory_belongs_is_refused_before_the_write() {
    let dir = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();
    let payload = b"needs a shard".to_vec();
    let hash = BlobHash::of(&payload);
    let hex = hash.to_hex();
    // Occupy the first-level shard with something that is not a directory.
    fs::write(dir.path().join("blobs").join(&hex[0..2]), b"squatter").unwrap();

    let err = s.put(&payload).unwrap_err();
    match err {
        StoreError::NotADirectory(ref p) => {
            assert_eq!(*p, dir.path().join("blobs").join(&hex[0..2]))
        }
        other => panic!("expected NotADirectory, got {other:?}"),
    }
    assert!(!s.contains(hash), "refused, not half-added");
    assert!(tmp_files(dir.path()).is_empty(), "nothing was staged");
}

// ------------------------------------------------------------- symlinks ---
//
// A store root can arrive inside an untrusted project package, so every one of
// these tests plants a link the package could have shipped and asserts the
// store refuses to follow it.
//
// Creating a symlink on Windows needs `SeCreateSymbolicLinkPrivilege`
// (Developer Mode or an elevated shell), which the default developer machine
// does not have. These tests are therefore `#[ignore]`d on Windows rather than
// skipped from inside the body: an early `return` would report `ok` while
// asserting nothing, so a green Windows run would look exactly like coverage of
// the symlink defences when it is the opposite. Ignored shows up as `ignored`
// in the summary, which is honest, and CI runs them for real on Linux, where
// symlink creation always succeeds. On Windows with the privilege they can be
// opted into: `cargo test -p asset-store -- --ignored`.
// Only the `#[cfg(windows)]` arms below quote this, so on unix it is dead code
// and CI builds with `-D warnings`.
#[cfg(windows)]
const WINDOWS_SYMLINK_NOTE: &str = "needs SeCreateSymbolicLinkPrivilege on Windows \
     (Developer Mode or an elevated shell); runs unignored on unix. \
     Opt in here with: cargo test -p asset-store -- --ignored";

/// Point `link` at the directory `target`. Panics loudly rather than skipping.
fn link_dir(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("unix always permits symlinks");
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_dir(target, link) {
        panic!("could not create a directory symlink ({e}). {WINDOWS_SYMLINK_NOTE}");
    }
}

/// Point `link` at the file `target`. Panics loudly rather than skipping.
fn link_file(target: &Path, link: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("unix always permits symlinks");
    #[cfg(windows)]
    if let Err(e) = std::os::windows::fs::symlink_file(target, link) {
        panic!("could not create a file symlink ({e}). {WINDOWS_SYMLINK_NOTE}");
    }
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn gc_does_not_follow_a_symlinked_shard_out_of_the_store() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();

    // A file that belongs to somebody else, named so that following a shard
    // symlink would make it look like one of ours: canonical lowercase 64-hex,
    // sitting in the second-level shard its own hash designates.
    let victim_bytes = b"a file this crate did not write".to_vec();
    let victim = BlobHash::of(&victim_bytes);
    let hex = victim.to_hex();
    let victim_dir = outside.path().join(&hex[2..4]);
    fs::create_dir_all(&victim_dir).unwrap();
    let victim_path = victim_dir.join(&hex);
    fs::write(&victim_path, &victim_bytes).unwrap();

    // An untrusted project package can ship a store directory shaped like this.
    let s = AssetStore::open(dir.path()).unwrap();
    let shard = dir.path().join("blobs").join(&hex[0..2]);
    link_dir(outside.path(), &shard);

    let report = s.gc().unwrap();
    assert_eq!(
        report,
        GcReport::default(),
        "nothing outside the root is ours to collect"
    );
    assert!(victim_path.is_file(), "file outside the store survived gc");
    assert_eq!(fs::read(&victim_path).unwrap(), victim_bytes);
    assert!(
        fs::symlink_metadata(&shard).is_ok(),
        "the link itself is left alone too"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn gc_does_not_sweep_a_symlinked_tmp_directory() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let victim = outside.path().join("blob-1-1.tmp");
    fs::write(&victim, b"somebody else's file").unwrap();

    drop(AssetStore::open(dir.path()).unwrap());
    fs::remove_dir_all(dir.path().join("tmp")).unwrap();
    link_dir(outside.path(), &dir.path().join("tmp"));

    // `open` now refuses the store outright (a linked tmp/ would send every
    // staged write out of the root), so gc cannot even be reached to sweep
    // through the link. Both properties matter, so both are asserted.
    let err = AssetStore::open(dir.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::NotADirectory(_)),
        "expected NotADirectory, got {err:?}"
    );
    assert!(victim.is_file());
    assert_eq!(fs::read(&victim).unwrap(), b"somebody else's file");

    // And the sweep itself refuses the link even when handed one directly —
    // the check lives in `clean_tmp`, not only in `open`.
    let other = TempDir::new().unwrap();
    let disk = disk::Disk::open(other.path()).unwrap();
    let linked_tmp = other.path().join("linked-tmp");
    fs::create_dir_all(&linked_tmp).unwrap();
    fs::write(linked_tmp.join("victim.tmp"), b"not ours").unwrap();
    fs::remove_dir_all(other.path().join("tmp")).unwrap();
    link_dir(&linked_tmp, &other.path().join("tmp"));
    assert_eq!(disk.clean_tmp().unwrap(), 0, "nothing swept through a link");
    assert!(linked_tmp.join("victim.tmp").is_file());
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn open_refuses_a_symlinked_store_root() {
    // The root itself is untrusted: a project package can ship a link where the
    // store directory should be. `create_dir_all` is satisfied by a symlink to a
    // directory, so without an explicit check on the root, `blobs/` and `tmp/`
    // are created at the far end of the link — outside the intended root —
    // before anything else has a chance to refuse. Both halves are asserted: the
    // refusal, and that nothing was created through the link.
    let parent = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let root = parent.path().join("store");
    link_dir(outside.path(), &root);

    let err = AssetStore::open(&root).unwrap_err();
    match err {
        StoreError::NotADirectory(ref p) => assert_eq!(*p, root),
        other => panic!("expected NotADirectory, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing was created through the linked root"
    );
    assert!(
        fs::symlink_metadata(&root)
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link itself is left alone"
    );
}

#[test]
fn open_creates_a_root_that_does_not_exist_yet() {
    // The other half of the root check: refusing a link must not turn into
    // refusing a root this crate is supposed to create.
    let parent = TempDir::new().unwrap();
    let root = parent.path().join("nested").join("store");
    let s = AssetStore::open(&root).unwrap();
    let h = s.put(b"payload").unwrap();
    assert!(root.join("blobs").is_dir() && root.join("tmp").is_dir());
    drop(s);
    let s = AssetStore::open(&root).unwrap();
    assert_eq!(&*s.get(h).unwrap(), b"payload");
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn open_refuses_a_symlinked_blobs_directory() {
    // `fs::create_dir_all` is satisfied by a symlink pointing at a directory,
    // because it tests `is_dir()`, which follows links. Adopting one would send
    // every shard `create_dir_all` and every `rename` out of the store root.
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    link_dir(outside.path(), &dir.path().join("blobs"));

    let err = AssetStore::open(dir.path()).unwrap_err();
    match err {
        StoreError::NotADirectory(ref p) => assert_eq!(*p, dir.path().join("blobs")),
        other => panic!("expected NotADirectory, got {other:?}"),
    }
    assert!(
        blob_files(outside.path()).is_empty() && fs::read_dir(outside.path()).unwrap().count() == 0,
        "nothing was written through the link"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn open_refuses_a_symlinked_tmp_directory() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(dir.path().join("blobs")).unwrap();
    link_dir(outside.path(), &dir.path().join("tmp"));

    let err = AssetStore::open(dir.path()).unwrap_err();
    match err {
        StoreError::NotADirectory(ref p) => assert_eq!(*p, dir.path().join("tmp")),
        other => panic!("expected NotADirectory, got {other:?}"),
    }
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "no temp file staged through the link"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn put_refuses_to_write_through_a_symlinked_shard_directory() {
    // One level deeper than the blobs/ link: checking only the deepest shard
    // would pass here, because `blobs/aa/bb` is a real directory at the far end
    // of the link while every write still lands outside the root.
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let s = AssetStore::open(dir.path()).unwrap();

    let payload = b"must not escape".to_vec();
    let hash = BlobHash::of(&payload);
    let hex = hash.to_hex();
    let shard = dir.path().join("blobs").join(&hex[0..2]);
    link_dir(outside.path(), &shard);

    let err = s.put(&payload).unwrap_err();
    assert!(
        matches!(err, StoreError::NotADirectory(_)),
        "expected NotADirectory, got {err:?}"
    );
    assert!(
        !s.contains(hash),
        "a blob that could not be written is not in the store"
    );
    assert_eq!(
        fs::read_dir(outside.path()).unwrap().count(),
        0,
        "nothing written outside the root"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn get_does_not_follow_a_symlink_planted_at_a_blob_path() {
    // A package can plant a link at a path this crate builds itself. Following
    // it would read an out-of-root file, and because a hash mismatch is
    // reported with the hash actually computed, `get` would become an oracle
    // confirming that file's contents. It must look exactly like absence.
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret");
    let secret_bytes = b"somebody else's private bytes".to_vec();
    fs::write(&secret, &secret_bytes).unwrap();

    let payload = b"the real blob".to_vec();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&payload).unwrap()
    };
    let path = blob_file(dir.path(), h);
    fs::remove_file(&path).unwrap();
    link_file(&secret, &path);

    let s = AssetStore::open(dir.path()).unwrap();
    let err = s.get(h).unwrap_err();
    match err {
        StoreError::NotFound(ref got) => assert_eq!(*got, h.to_hex()),
        StoreError::Corrupt { ref actual, .. } => panic!(
            "get followed the link and leaked the target's hash: {actual} \
             (== {} for the planted file)",
            BlobHash::of(&secret_bytes).to_hex()
        ),
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(!s.is_resident(h), "nothing from outside entered the cache");
    assert_eq!(
        fs::read(&secret).unwrap(),
        secret_bytes,
        "the target was not touched"
    );

    // A dedup `put` must not accept the link as the blob either: it rewrites
    // the real bytes, replacing the link rather than writing through it.
    let again = s.put(&payload).unwrap();
    assert_eq!(again, h);
    assert!(
        fs::symlink_metadata(&path).unwrap().file_type().is_file(),
        "the rename replaced the link with a real file"
    );
    assert_eq!(fs::read(&path).unwrap(), payload);
    assert_eq!(
        fs::read(&secret).unwrap(),
        secret_bytes,
        "and did not write through it"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn open_refuses_a_symlinked_journal() {
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let victim = outside.path().join("journal");
    fs::write(&victim, b"somebody else's file").unwrap();
    fs::create_dir_all(dir.path()).unwrap();
    link_file(&victim, &dir.path().join("index"));

    let err = AssetStore::open(dir.path()).unwrap_err();
    assert!(
        matches!(err, StoreError::NotAFile(_)),
        "expected NotAFile, got {err:?}"
    );
    assert_eq!(
        fs::read(&victim).unwrap(),
        b"somebody else's file",
        "the journal target was neither read into the store nor overwritten"
    );
}

#[test]
#[cfg_attr(windows, ignore = "needs SeCreateSymbolicLinkPrivilege")]
fn gc_unlinks_a_link_planted_at_a_blob_path_without_touching_its_target() {
    // `remove_file` unlinks the link, never the target — but sizing the delete
    // with `fs::metadata` would follow it and report the target's length, which
    // both corrupts the reclaim accounting and leaks the size of a file outside
    // the root. The link is at a path this crate owns, so removing it is right;
    // counting it as 2 KiB reclaimed is not.
    let dir = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let victim = outside.path().join("big");
    fs::write(&victim, blob(9, 2048)).unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    let h = s.put(b"tiny").unwrap();
    s.release(h).unwrap();
    let path = blob_file(dir.path(), h);
    fs::remove_file(&path).unwrap();
    link_file(&victim, &path);

    let report = s.gc().unwrap();
    assert_eq!(report.blobs_removed, 1);
    assert_ne!(
        report.bytes_reclaimed, 2048,
        "the target's size must not be reported as reclaimed"
    );
    assert!(
        fs::symlink_metadata(&path).is_err(),
        "the planted link inside our root is gone"
    );
    assert_eq!(
        fs::read(&victim).unwrap(),
        blob(9, 2048),
        "the file outside the root is untouched"
    );
}

/// A FIFO at a canonical blob path is the worst case of the same bug: on unix
/// `File::open` on a FIFO with no writer blocks *inside* `get`, while the store
/// mutex is held, so every other thread in the process deadlocks behind it.
#[cfg(unix)]
#[test]
fn get_does_not_open_a_fifo_planted_at_a_blob_path() {
    use std::sync::mpsc;
    use std::time::Duration;

    let dir = TempDir::new().unwrap();
    let payload = b"the real blob".to_vec();
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&payload).unwrap()
    };
    let path = blob_file(dir.path(), h);
    fs::remove_file(&path).unwrap();

    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .map(|st| st.success())
        .unwrap_or(false);
    assert!(
        made && fs::symlink_metadata(&path).unwrap().file_type().is_fifo(),
        "could not create a FIFO with mkfifo(1); this test needs it"
    );

    let s = Arc::new(AssetStore::open(dir.path()).unwrap());
    let (tx, rx) = mpsc::channel();
    let worker = {
        let s = Arc::clone(&s);
        thread::spawn(move || {
            let r = s.get(h);
            let _ = tx.send(matches!(r, Err(StoreError::NotFound(_))));
        })
    };
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(was_not_found) => {
            assert!(was_not_found, "a FIFO must read as absent, not as data");
            worker.join().unwrap();
        }
        Err(_) => panic!("get() blocked on the FIFO: the store mutex is deadlocked"),
    }

    // The store is still usable, which is the point: the mutex was never held
    // across a blocking open.
    assert!(!s.is_resident(h));
    assert_eq!(s.refcount(h), Some(1));
}

// ---------------------------------------------------- documented behaviour --

#[test]
fn a_dedup_put_confirms_length_not_content_and_get_catches_the_difference() {
    // Pins the module doc's write-through claim to what the code actually does.
    // A `put` that hit an intact-looking (right length, wrong bytes) file
    // returns Ok — it does not re-read to compare content — and `get` is what
    // reports the corruption. The doc must not claim `put` guarantees the bytes
    // can be read back, because this is the case where they cannot.
    let dir = TempDir::new().unwrap();
    let payload = blob(3, 512);
    let h = {
        let s = AssetStore::open(dir.path()).unwrap();
        s.put(&payload).unwrap()
    };
    let path = blob_file(dir.path(), h);
    fs::write(&path, blob(4, 512)).unwrap();

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(
        s.put(&payload).unwrap(),
        h,
        "a same-length corruption is invisible to put's stat"
    );
    assert_eq!(
        fs::read(&path).unwrap(),
        blob(4, 512),
        "and the corrupt file was not rewritten"
    );
    drop(s);

    // Cold cache, so the read actually goes to disk rather than to the copy
    // `put` left resident.
    let s = AssetStore::open(dir.path()).unwrap();
    let err = s.get(h).unwrap_err();
    match err {
        StoreError::Corrupt { ref actual, .. } => {
            assert_eq!(*actual, BlobHash::of(&blob(4, 512)).to_hex())
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

#[test]
fn blob_path_is_the_sharded_content_addressed_path() {
    // The hot-path version builds the hex in a stack buffer and the PathBuf in
    // one reservation; it must still produce exactly the naive join.
    let dir = TempDir::new().unwrap();
    let d = disk::Disk::open(dir.path()).unwrap();
    for i in 0..64u8 {
        let h = BlobHash::of(&blob(i, 3));
        let hex = h.to_hex();
        let want = dir
            .path()
            .join("blobs")
            .join(&hex[0..2])
            .join(&hex[2..4])
            .join(&hex);
        assert_eq!(d.blob_path(h), want);
    }
}

// ------------------------------------------------- lock poison recovery ---

#[test]
fn invariants_hold_rejects_a_torn_inner() {
    // The guard that makes `AssetStore::lock`'s poison recovery sound. If this
    // cannot see a broken cache, the debug assertion protecting that recovery
    // is decorative.
    let mut inner = Inner::default();
    let h = BlobHash::of(b"x");
    let data: Arc<[u8]> = Arc::from(&b"12345"[..]);
    inner.entries.insert(
        h,
        Entry {
            data: None,
            refs: 1,
            tick: None,
        },
    );
    inner.set_resident(h, Arc::clone(&data));
    assert!(inner.invariants_hold(), "a consistent Inner passes");

    // resident_bytes drifting away from the cached bytes.
    inner.resident_bytes += 1;
    assert!(!inner.invariants_hold(), "byte total mismatch is caught");
    inner.resident_bytes -= 1;
    assert!(inner.invariants_hold());

    // Bytes dropped without clearing the LRU tick.
    inner.entries.get_mut(&h).unwrap().data = None;
    assert!(!inner.invariants_hold(), "tick without data is caught");
    inner.entries.get_mut(&h).unwrap().data = Some(Arc::clone(&data));
    assert!(inner.invariants_hold());

    // An LRU entry with nothing behind it.
    inner.lru.insert(9999, BlobHash::of(b"ghost"));
    assert!(!inner.invariants_hold(), "stale LRU key is caught");
}

#[test]
fn a_poisoned_lock_recovers_with_a_usable_store() {
    // Poisoning must not brick the store, and the state handed back must still
    // satisfy the invariants the recovery path assumes.
    let s = Arc::new(AssetStore::new());
    let h = s.put(b"still here").unwrap();

    let s2 = Arc::clone(&s);
    let poisoner = thread::spawn(move || {
        let _guard = s2.inner.lock().unwrap();
        panic!("deliberate panic while holding the store lock");
    });
    assert!(poisoner.join().is_err(), "the thread did panic");
    assert!(s.inner.is_poisoned(), "and the mutex is poisoned");

    // Every accessor goes through `lock`, whose debug assertion checks the
    // recovered state before handing it out.
    assert_eq!(&*s.get(h).unwrap(), b"still here");
    assert_eq!(s.refcount(h), Some(1));
    assert_eq!(s.resident_bytes(), 10);
    let h2 = s.put(b"added after the panic").unwrap();
    assert_eq!(s.len(), 2);
    assert_eq!(&*s.get(h2).unwrap(), b"added after the panic");
}

// ----------------------------------------------------------- concurrency ---

#[test]
fn concurrent_puts_of_the_same_content_are_safe() {
    let dir = TempDir::new().unwrap();
    let s = Arc::new(AssetStore::open(dir.path()).unwrap());
    let payload = blob(42, 1024);

    let mut handles = Vec::new();
    for _ in 0..8 {
        let s = Arc::clone(&s);
        let payload = payload.clone();
        handles.push(thread::spawn(move || {
            let mut last = None;
            for _ in 0..25 {
                let h = s.put(&payload).unwrap();
                assert_eq!(&*s.get(h).unwrap(), &payload[..]);
                last = Some(h);
            }
            last.unwrap()
        }));
    }
    let hashes: Vec<BlobHash> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let h = hashes[0];
    assert!(hashes.iter().all(|x| *x == h));

    assert_eq!(s.len(), 1, "one entry for one content");
    assert_eq!(s.refcount(h), Some(200), "no lost updates");
    assert_eq!(blob_files(dir.path()).len(), 1, "written once");
    assert!(tmp_files(dir.path()).is_empty());
    assert_eq!(&*s.get(h).unwrap(), &payload[..]);
    drop(s);

    let s = AssetStore::open(dir.path()).unwrap();
    assert_eq!(s.refcount(h), Some(200), "and it is durable");
}

#[test]
fn concurrent_puts_of_distinct_content_all_survive() {
    let dir = TempDir::new().unwrap();
    // A cache far too small for the working set, so eviction races too.
    let s = Arc::new(AssetStore::open_with(dir.path(), small_cache(512)).unwrap());

    let mut handles = Vec::new();
    for t in 0..8u8 {
        let s = Arc::clone(&s);
        handles.push(thread::spawn(move || {
            (0..16u8)
                .map(|i| {
                    let payload = blob(t.wrapping_mul(16).wrapping_add(i), 256);
                    (s.put(&payload).unwrap(), payload)
                })
                .collect::<Vec<_>>()
        }));
    }
    let all: Vec<(BlobHash, Vec<u8>)> = handles
        .into_iter()
        .flat_map(|h| h.join().unwrap())
        .collect();

    assert!(s.resident_bytes() <= 512, "budget held under contention");
    for (h, payload) in &all {
        assert_eq!(&*s.get(*h).unwrap(), &payload[..], "read-through intact");
    }
    assert_eq!(blob_files(dir.path()).len(), s.len());
}
