//! The recent-files list.
//!
//! Most-recent first, de-duplicated, and bounded. De-duplication compares the
//! *canonical* path where the file still exists, so opening `./photo.png` and
//! then `/home/me/photo.png` leaves one entry rather than two that look
//! different and mean the same file. The entry stored is the path the caller
//! gave, because that is the one the user recognises in a menu — made
//! absolute first when it was relative to the working directory (W5-D): a
//! `photo.png` from a command line means nothing to the next launch, which
//! starts somewhere else, and would sit beside the absolute entry for the
//! same file whenever that file has since moved or gone.
//!
//! A `--shot` capture run [`RecentFiles::freeze`]s the list: fixtures it opens
//! must not land in the user's File ▸ Open Recent.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How many entries the list keeps. Older ones fall off the end.
pub const MAX_RECENT_FILES: usize = 12;

/// The identity two paths are compared on: the canonical form when the file is
/// still there, the path as given when it is not.
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Most-recently-opened files, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
    /// W5-D: set for a `--shot` run — [`RecentFiles::record`] and
    /// [`RecentFiles::save`] then do nothing. Never persisted.
    frozen: bool,
}

/// The on-disk shape: a bare JSON array of paths, as it always was.
impl Serialize for RecentFiles {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.entries.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RecentFiles {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(RecentFiles {
            entries: Vec::<PathBuf>::deserialize(deserializer)?,
            frozen: false,
        })
    }
}

/// W5-D: the form an entry is stored in — absolute when the caller's path was
/// relative to the working directory, as given otherwise. `std::path::absolute`
/// rather than `canonicalize`: it needs no file on disk and on Windows does not
/// add the verbatim-path prefix Windows canonical paths carry, which a menu
/// would then show.
fn stored_form(path: PathBuf) -> PathBuf {
    if path.has_root() {
        return path;
    }
    std::path::absolute(&path).unwrap_or(path)
}

impl RecentFiles {
    pub fn new() -> Self {
        Self::default()
    }

    /// The list, newest first.
    pub fn entries(&self) -> &[PathBuf] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Put `path` at the front, removing any earlier mention of the same file
    /// and dropping whatever falls past [`MAX_RECENT_FILES`].
    pub fn record(&mut self, path: impl Into<PathBuf>) {
        if self.frozen {
            return;
        }
        let path = stored_form(path.into());
        let id = identity(&path);
        self.entries.retain(|e| identity(e) != id);
        self.entries.insert(0, path);
        self.entries.truncate(MAX_RECENT_FILES);
    }

    /// Forget one entry — what the UI calls when opening it fails.
    pub fn forget(&mut self, path: &Path) {
        let id = identity(path);
        self.entries.retain(|e| identity(e) != id);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// W5-D: stop recording and saving — what a `--shot` capture run asks
    /// for, so the fixtures it opens never reach the user's recent list.
    pub fn freeze(&mut self) {
        self.frozen = true;
    }

    /// Whether [`RecentFiles::freeze`] has been called.
    pub fn is_frozen(&self) -> bool {
        self.frozen
    }

    /// Read the list, treating anything unreadable as an empty one.
    pub fn load(path: &Path) -> RecentFiles {
        let Ok(text) = std::fs::read_to_string(path) else {
            return RecentFiles::new();
        };
        match serde_json::from_str::<RecentFiles>(&text) {
            // A file written by hand (or by a future build) can still break the
            // two invariants, so they are re-established rather than trusted.
            Ok(list) => list.normalized(),
            Err(e) => {
                tracing::warn!("recent files at {} are unreadable: {e}", path.display());
                RecentFiles::new()
            }
        }
    }

    fn normalized(self) -> RecentFiles {
        let mut out = RecentFiles::new();
        // Re-record in reverse so the file's own order survives.
        for entry in self.entries.into_iter().rev() {
            out.record(entry);
        }
        out
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.frozen {
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W5-D: a path relative to the working directory is stored absolute, so
    /// the next launch (started somewhere else) can still open it, and the
    /// same file recorded both ways is one entry even when it does not exist.
    #[test]
    fn a_relative_path_is_recorded_absolute_and_deduplicates() {
        let mut r = RecentFiles::new();
        r.record("w5d-not-on-disk/photo.png");
        let absolute = std::env::current_dir()
            .unwrap()
            .join("w5d-not-on-disk")
            .join("photo.png");
        assert_eq!(r.entries(), std::slice::from_ref(&absolute));
        r.record(absolute.clone());
        assert_eq!(r.entries(), [absolute], "one entry, not two");
    }

    #[test]
    fn a_frozen_list_records_and_saves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("recent.json");
        let mut r = RecentFiles::new();
        r.freeze();
        r.record("/a/one.png");
        assert!(r.is_empty());
        r.save(&file).unwrap();
        assert!(!file.exists(), "a frozen list never writes");
    }

    #[test]
    fn the_file_is_still_a_bare_json_array() {
        let mut r = RecentFiles::new();
        r.record("/a/one.png");
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.starts_with('['), "{json}");
        let back: RecentFiles = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn recording_the_same_file_twice_leaves_one_entry_at_the_front() {
        let mut r = RecentFiles::new();
        r.record("/a/one.png");
        r.record("/a/two.png");
        r.record("/a/one.png");
        assert_eq!(
            r.entries(),
            [PathBuf::from("/a/one.png"), PathBuf::from("/a/two.png")]
        );
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn two_spellings_of_one_real_file_are_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("photo.png");
        std::fs::write(&file, b"x").unwrap();
        let indirect = dir.path().join("sub").join("..").join("photo.png");
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();

        let mut r = RecentFiles::new();
        r.record(&file);
        r.record(&indirect);
        assert_eq!(r.len(), 1, "same file, two spellings: {:?}", r.entries());
        assert_eq!(r.entries()[0], indirect, "the newest spelling is kept");
    }

    #[test]
    fn the_list_is_bounded() {
        let mut r = RecentFiles::new();
        for i in 0..(MAX_RECENT_FILES * 3) {
            r.record(format!("/a/{i}.png"));
        }
        assert_eq!(r.len(), MAX_RECENT_FILES);
        assert_eq!(
            r.entries()[0],
            PathBuf::from(format!("/a/{}.png", MAX_RECENT_FILES * 3 - 1)),
            "newest first"
        );
        // The oldest survivor is the (MAX-1)th newest.
        let oldest = &r.entries()[MAX_RECENT_FILES - 1];
        assert_eq!(
            oldest,
            &PathBuf::from(format!(
                "/a/{}.png",
                MAX_RECENT_FILES * 3 - MAX_RECENT_FILES
            ))
        );
    }

    #[test]
    fn forget_and_clear_remove_entries() {
        let mut r = RecentFiles::new();
        r.record("/a/one.png");
        r.record("/a/two.png");
        r.forget(Path::new("/a/one.png"));
        assert_eq!(r.entries(), [PathBuf::from("/a/two.png")]);
        r.clear();
        assert!(r.is_empty());
    }

    #[test]
    fn the_list_survives_disk_and_repairs_a_hand_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recent.json");
        assert!(RecentFiles::load(&path).is_empty(), "missing file");

        let mut r = RecentFiles::new();
        r.record("/a/one.png");
        r.record("/a/two.png");
        r.save(&path).unwrap();
        assert_eq!(RecentFiles::load(&path), r);

        // A file that breaks both invariants is repaired, not trusted.
        let mut oversized: Vec<String> = vec!["/a/dup.png".into(), "/a/dup.png".into()];
        oversized.extend((0..MAX_RECENT_FILES * 2).map(|i| format!("/b/{i}.png")));
        std::fs::write(&path, serde_json::to_string(&oversized).unwrap()).unwrap();
        let loaded = RecentFiles::load(&path);
        assert_eq!(loaded.len(), MAX_RECENT_FILES);
        assert_eq!(
            loaded.entries()[0],
            PathBuf::from("/a/dup.png"),
            "the file's own order is preserved"
        );

        std::fs::write(&path, "not json at all").unwrap();
        assert!(RecentFiles::load(&path).is_empty());
    }
}
