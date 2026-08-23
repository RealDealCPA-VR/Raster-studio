//! The recent-files list.
//!
//! Most-recent first, de-duplicated, and bounded. De-duplication compares the
//! *canonical* path where the file still exists, so opening `./photo.png` and
//! then `/home/me/photo.png` leaves one entry rather than two that look
//! different and mean the same file. The entry stored is the path the caller
//! gave, because that is the one the user recognises in a menu.

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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecentFiles {
    entries: Vec<PathBuf>,
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
        let path = path.into();
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
