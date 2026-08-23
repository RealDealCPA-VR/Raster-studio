//! Durable file writes and the crash-safe directory swap.
//!
//! # The window the old swap left open
//!
//! ```text
//! rename(dest -> dest.old);   //  <-- crash here
//! rename(tmp  -> dest);
//! ```
//!
//! A crash between those two lines leaves **no project at `dest`**. The user's
//! file is not corrupt, it is *gone from where they left it*, sitting under a
//! `.old` suffix that nothing on startup ever looked for. Two further bugs sat
//! on top: the rollback was `let _ = rename(old, path)`, so a failed rollback
//! was discarded silently and the caller was told only about the original
//! error; and both siblings had fixed names, so two saves running at once each
//! deleted the other's in-flight temp directory.
//!
//! What this module does instead:
//!
//! * temp and backup siblings get **unique** names ([`unique_sibling`]), so
//!   concurrent saves never collide and a save never deletes a directory it did
//!   not create;
//! * the backup is left behind for the loader to find, and [`recover`] — run at
//!   the top of every open — completes an interrupted swap by renaming it back;
//! * a rollback failure is returned as [`ProjectError::RollbackFailed`], which
//!   names both directories left on disk — the previous package and the save
//!   that was in flight — because with `dest` empty either one may be the only
//!   copy of the user's work;
//! * every failure path **that returns** removes the temp package it was
//!   handed, with exactly one deliberate exception: [`ProjectError::RollbackFailed`]
//!   names the temp instead of deleting it, because with `dest` empty it may be
//!   the only copy of the work;
//! * directories are fsynced after a rename, not only the files inside them,
//!   because a file fsync says nothing about the durability of the directory
//!   entry that points at it — but an fsync that fails *after* the rename that
//!   completed the save is reported to nobody, because the save did happen and
//!   telling the user it failed would be a lie about where their work is.
//!
//! # What a crash leaks
//!
//! A process that dies between the two renames returns nothing and therefore
//! cleans nothing up, so it leaves a `.new-` sibling behind: a complete,
//! full-size copy of the package it was writing. [`recover`] restores the
//! backup and deliberately never adopts or deletes that sibling — a `.new-`
//! directory may belong to a save that is still running in another process.
//! Nothing else reclaims it either, so **an interrupted save leaves a `.new-`
//! sibling that nothing in this crate ever removes.** It is disk usage, not
//! data loss, and it is stated here rather than implied because the alternative
//! — an age-gated sweep — would have to guess how long a legitimate save may
//! take.
//!
//! # Honest platform note
//!
//! [`sync_dir`] is a real `fsync` on Unix. On Windows, `std` cannot open a
//! directory handle (that needs `FILE_FLAG_BACKUP_SEMANTICS`), so it is a no-op
//! and durability of the rename itself rests on the filesystem. That is exactly
//! why [`recover`] exists and runs unconditionally rather than being a Unix-only
//! fallback: the recoverable state is reconstructed from what is on disk, not
//! from an assumption about ordering.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::error::ProjectError;

/// Prefix of the sibling directory a save builds into.
pub(crate) const TEMP_PREFIX: &str = "new";
/// Prefix of the sibling directory holding the previous package mid-swap.
pub(crate) const BACKUP_PREFIX: &str = "bak";

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A sibling of `path` whose name no other save will pick.
///
/// Uniqueness comes from three independent sources so that neither two threads
/// in one process (counter), two processes on one machine (pid), nor a process
/// whose pid was reused after a crash (nanosecond clock) can produce the same
/// name.
pub(crate) fn unique_sibling(path: &Path, prefix: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_default();
    name.push(format!(".{prefix}-{pid:x}-{nanos:x}-{n:x}"));
    path.with_file_name(name)
}

/// The directory a package and its siblings live in.
///
/// Not `path.parent().unwrap_or(Path::new("."))`: for a bare relative name,
/// `Path::new("P.rstudio").parent()` is `Some("")`, **not** `None`, so that
/// fallback never fires and the empty path reaches [`sync_dir`], where
/// `File::open("")` fails on every platform. In [`swap_into_place`] that turned
/// a plain relative save path into the exact data-loss window this module
/// exists to close: the first rename succeeds, the fsync errors, the second
/// rename never runs, and the user is left with no package where they saved.
pub(crate) fn parent_dir(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

/// Does `name` look like a sibling this module creates for `stem`?
fn sibling_of(name: &std::ffi::OsStr, stem: &std::ffi::OsStr, prefix: &str) -> bool {
    let (Some(name), Some(stem)) = (name.to_str(), stem.to_str()) else {
        return false;
    };
    name.starts_with(&format!("{stem}.{prefix}-"))
}

/// Write `bytes` to `path`, flush, and fsync the file.
pub(crate) fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(bytes)?;
    f.flush()?;
    f.sync_all()?;
    Ok(())
}

/// fsync a directory so a rename or creation inside it is durable.
///
/// See the module's platform note: a no-op off Unix.
pub(crate) fn sync_dir(dir: &Path) -> Result<(), ProjectError> {
    #[cfg(unix)]
    {
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
    }
    Ok(())
}

/// Recursively fsync every directory in a freshly built package, deepest first.
pub(crate) fn sync_tree(root: &Path) -> Result<(), ProjectError> {
    let mut stack = vec![root.to_path_buf()];
    let mut dirs = Vec::new();
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                stack.push(entry.path());
            }
        }
        dirs.push(d);
    }
    for d in dirs.iter().rev() {
        sync_dir(d)?;
    }
    Ok(())
}

// Test seams: make `swap_into_place` fail at a chosen point, exactly as a power
// cut or a filesystem error would. Only compiled for this crate's own tests, and
// thread-local so a test that arms one cannot disturb one running beside it.
#[cfg(test)]
thread_local! {
    pub(crate) static CRASH_BETWEEN_RENAMES: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Make `rename(dest -> backup)` fail. There is no portable way to make a
    /// directory rename fail on demand, and the branch has to be exercised:
    /// it is the one that used to strand a full-size copy of the project.
    pub(crate) static FAIL_BACKUP_RENAME: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Make `rename(tmp -> dest)` fail: the forward move, after the previous
    /// package has already been parked under the backup name. This is the seam
    /// that opens the rollback.
    pub(crate) static FAIL_FORWARD_RENAME: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
    /// Make `rename(backup -> dest)` fail: the rollback itself. Armed together
    /// with `FAIL_FORWARD_RENAME` this reaches
    /// [`ProjectError::RollbackFailed`] — the state where nothing is at the
    /// save path and the error message is the only record of where the two
    /// surviving copies are.
    pub(crate) static FAIL_ROLLBACK_RENAME: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

fn rename_dest_to_backup(dest: &Path, backup: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_BACKUP_RENAME.with(|c| c.replace(false)) {
        return Err(std::io::Error::other("simulated backup rename failure"));
    }
    std::fs::rename(dest, backup)
}

fn rename_tmp_to_dest(tmp: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_FORWARD_RENAME.with(|c| c.replace(false)) {
        return Err(std::io::Error::other("simulated forward rename failure"));
    }
    std::fs::rename(tmp, dest)
}

fn rename_backup_to_dest(backup: &Path, dest: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if FAIL_ROLLBACK_RENAME.with(|c| c.replace(false)) {
        return Err(std::io::Error::other("simulated rollback rename failure"));
    }
    std::fs::rename(backup, dest)
}

/// Move the freshly built package at `tmp` onto `dest`.
///
/// On success `dest` is the new package and no sibling is left behind.
///
/// On failure `dest` holds whatever it held before and the temp package this
/// call was handed is removed — *except* when putting the previous package back
/// also failed, in which case nothing is deleted and the returned
/// [`ProjectError::RollbackFailed`] names both directories that survive,
/// because either one may be the only copy of the user's work.
///
/// A crash inside this function is a third case and is not a return at all: see
/// the module header's "What a crash leaks".
pub(crate) fn swap_into_place(dest: &Path, tmp: &Path) -> Result<(), ProjectError> {
    let parent = parent_dir(dest);

    if !dest.exists() {
        if let Err(e) = rename_tmp_to_dest(tmp, dest) {
            // Nothing landed. The temp is ours and nothing else will ever
            // reclaim it, so it goes rather than sitting next to the user's
            // file as a full-size copy forever.
            let _ = std::fs::remove_dir_all(tmp);
            return Err(e.into());
        }
        // The save is *done*: `dest` is the new package. A failing directory
        // fsync from here on is a durability warning about a save that
        // completed, and returning it would tell the user their save failed
        // when their work is on disk.
        let _ = sync_dir(parent);
        return Ok(());
    }

    let backup = unique_sibling(dest, BACKUP_PREFIX);
    if let Err(e) = rename_dest_to_backup(dest, &backup) {
        // Nothing moved: `dest` is exactly as it was. The package we built is
        // ours and nothing else will ever reclaim it — `recover` deliberately
        // never touches a `.new-` sibling — so leaving it would strand a
        // full-size copy of the project next to the user's file.
        let _ = std::fs::remove_dir_all(tmp);
        return Err(e.into());
    }
    // The directory entry for the backup has to be durable before the window
    // opens, or a crash could leave neither name pointing anywhere.
    sync_dir(parent)?;

    #[cfg(test)]
    if CRASH_BETWEEN_RENAMES.with(|c| c.replace(false)) {
        // Return the way a dead process "returns": `dest` does not exist, the
        // backup does, the temp is still on disk. Nothing is cleaned up,
        // because a crash cleans nothing up.
        return Err(ProjectError::Io(std::io::Error::other(
            "simulated crash between renames",
        )));
    }

    match rename_tmp_to_dest(tmp, dest) {
        Ok(()) => {
            // Same reasoning as the `!dest.exists()` branch: the rename that
            // completed the save has already happened, so an fsync failure
            // after it is not a failed save.
            let _ = sync_dir(parent);
            // Only now is the backup redundant. A failure to remove it is not a
            // failed save: `recover` ignores a backup whose destination exists.
            let _ = std::fs::remove_dir_all(&backup);
            Ok(())
        }
        Err(source) => match rename_backup_to_dest(&backup, dest) {
            Ok(()) => {
                let _ = sync_dir(parent);
                let _ = std::fs::remove_dir_all(tmp);
                Err(source.into())
            }
            // Never `let _ =`. The user's project is at `backup`, the save
            // they just asked for is at `tmp`, and this error is the only
            // thing that knows either. The temp is *not* removed here: with
            // `dest` empty and the rollback broken it may be the only copy of
            // the work, so it is named rather than deleted.
            Err(rollback) => Err(ProjectError::RollbackFailed {
                source,
                rollback,
                backup,
                temp: tmp.to_path_buf(),
            }),
        },
    }
}

/// Complete a save that was interrupted between the two renames.
///
/// Runs at the top of every open. If `dest` is absent and a sibling backup of
/// it holds a manifest, the backup is renamed back into place and `true` is
/// returned. Anything else — `dest` already present, no backup, a backup with
/// no manifest — is left exactly as it is.
///
/// Deliberately conservative: it never touches a `.new-` temp, because a temp
/// that is not ours belongs to a save that is still running.
pub(crate) fn recover(dest: &Path) -> Result<bool, ProjectError> {
    if dest.exists() {
        return Ok(false);
    }
    let Some(stem) = dest.file_name() else {
        return Ok(false);
    };
    let parent = parent_dir(dest);
    let dir = match std::fs::read_dir(parent) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };

    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in dir {
        let entry = entry?;
        if !sibling_of(&entry.file_name(), stem, BACKUP_PREFIX) {
            continue;
        }
        let path = entry.path();
        if !path.join(crate::MANIFEST_FILE).is_file() {
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        // Newest wins: an older backup is from an earlier interrupted save and
        // its contents are staler than this one's.
        if best.as_ref().is_none_or(|(t, _)| mtime > *t) {
            best = Some((mtime, path));
        }
    }

    let Some((_, backup)) = best else {
        return Ok(false);
    };
    std::fs::rename(&backup, dest)?;
    sync_dir(parent)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_are_unique_so_concurrent_saves_cannot_collide() {
        let p = Path::new("/projects/P.rstudio");
        let a = unique_sibling(p, TEMP_PREFIX);
        let b = unique_sibling(p, TEMP_PREFIX);
        assert_ne!(a, b);
        assert_eq!(a.parent(), p.parent());
        assert!(a
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("P.rstudio.new-"));
    }

    #[test]
    fn unique_names_stay_unique_across_threads() {
        let p = Path::new("P.rstudio");
        let names: std::collections::HashSet<PathBuf> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    s.spawn(|| {
                        (0..64)
                            .map(|_| unique_sibling(p, TEMP_PREFIX))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap())
                .collect()
        });
        assert_eq!(names.len(), 8 * 64, "a name was handed out twice");
    }

    #[test]
    fn recover_is_a_no_op_when_the_destination_exists() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        std::fs::create_dir(&dest).unwrap();
        assert!(!recover(&dest).unwrap());
    }

    #[test]
    fn recover_ignores_a_backup_with_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        let backup = unique_sibling(&dest, BACKUP_PREFIX);
        std::fs::create_dir(&backup).unwrap();
        assert!(!recover(&dest).unwrap());
        assert!(backup.exists(), "an unrecognized sibling is left alone");
    }

    #[test]
    fn recover_renames_the_newest_backup_back_into_place() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        let backup = unique_sibling(&dest, BACKUP_PREFIX);
        std::fs::create_dir(&backup).unwrap();
        std::fs::write(backup.join(crate::MANIFEST_FILE), b"{}").unwrap();

        assert!(recover(&dest).unwrap());
        assert!(dest.join(crate::MANIFEST_FILE).is_file());
        assert!(!backup.exists());
    }

    #[test]
    fn a_bare_relative_name_has_a_usable_parent_directory() {
        // The whole bug in one assertion: `parent()` on a bare relative name is
        // `Some("")`, so `unwrap_or(".")` never fires and the empty path is
        // what reaches `sync_dir`.
        assert_eq!(Path::new("P.rstudio").parent(), Some(Path::new("")));
        assert_eq!(parent_dir(Path::new("P.rstudio")), Path::new("."));
        assert_eq!(parent_dir(Path::new("P.rstudio/")), Path::new("."));
        assert_eq!(parent_dir(Path::new("d/P.rstudio")), Path::new("d"));
        // And an absolute path is untouched.
        let abs = Path::new("/projects/P.rstudio");
        assert_eq!(parent_dir(abs), Path::new("/projects"));
    }

    #[test]
    fn overwriting_a_bare_relative_destination_works() {
        // On Unix the pre-fix code failed here *between the two renames*:
        // `sync_dir("")` is `File::open("")`, which is `NotFound`, so the
        // package was renamed away to a backup and never renamed back. On
        // Windows `sync_dir` is a no-op, so this test only pins the behaviour
        // there rather than reproducing the failure.
        static CWD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = CWD.lock().unwrap_or_else(|e| e.into_inner());

        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        let outcome = (|| -> Result<Vec<String>, ProjectError> {
            let dest = Path::new("P.rstudio");
            let mut seen = Vec::new();
            for round in 0..2 {
                let tmp = unique_sibling(dest, TEMP_PREFIX);
                std::fs::create_dir(&tmp)?;
                std::fs::write(tmp.join(crate::MANIFEST_FILE), format!("{round}"))?;
                swap_into_place(dest, &tmp)?;
                seen.push(std::fs::read_to_string(dest.join(crate::MANIFEST_FILE))?);
            }
            Ok(seen)
        })();

        // Restore before asserting: a panic here would leave the whole test
        // process in a deleted directory.
        std::env::set_current_dir(previous).unwrap();
        let seen = outcome.expect("saving twice to a bare relative name");
        assert_eq!(seen, vec!["0".to_string(), "1".to_string()]);
        assert!(dir.path().join("P.rstudio").is_dir());
    }

    #[test]
    fn a_failed_backup_rename_removes_the_temp_it_was_handed() {
        // `recover` never reclaims a `.new-` sibling, so a temp left here is a
        // full-size copy of the project that nothing will ever delete.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join(crate::MANIFEST_FILE), b"previous").unwrap();

        let tmp = unique_sibling(&dest, TEMP_PREFIX);
        std::fs::create_dir(&tmp).unwrap();
        std::fs::write(tmp.join(crate::MANIFEST_FILE), b"new").unwrap();

        FAIL_BACKUP_RENAME.with(|c| c.set(true));
        let err = swap_into_place(&dest, &tmp).unwrap_err();
        assert!(
            err.to_string().contains("simulated backup rename failure"),
            "{err}"
        );

        assert!(!tmp.exists(), "stranded {}", tmp.display());
        assert_eq!(
            std::fs::read_to_string(dest.join(crate::MANIFEST_FILE)).unwrap(),
            "previous",
            "the previous package must be exactly where it was"
        );
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "P.rstudio")
            .collect();
        assert!(strays.is_empty(), "left {strays:?} behind");
    }

    /// A `dest` holding `previous` and a `tmp` holding `new`, ready to swap.
    fn staged(dir: &Path) -> (PathBuf, PathBuf) {
        let dest = dir.join("P.rstudio");
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join(crate::MANIFEST_FILE), b"previous").unwrap();
        let tmp = unique_sibling(&dest, TEMP_PREFIX);
        std::fs::create_dir(&tmp).unwrap();
        std::fs::write(tmp.join(crate::MANIFEST_FILE), b"new").unwrap();
        (dest, tmp)
    }

    fn manifest_text(pkg: &Path) -> String {
        std::fs::read_to_string(pkg.join(crate::MANIFEST_FILE)).unwrap()
    }

    #[test]
    fn a_failed_forward_rename_puts_the_previous_package_back() {
        // The previous package has already been moved aside when the forward
        // rename fails, so `dest` is momentarily empty. This is the branch that
        // has to put it back, remove the temp, and hand the caller the *original*
        // error rather than one about the rollback.
        let dir = tempfile::tempdir().unwrap();
        let (dest, tmp) = staged(dir.path());

        FAIL_FORWARD_RENAME.with(|c| c.set(true));
        let err = swap_into_place(&dest, &tmp).unwrap_err();

        assert!(
            err.to_string().contains("simulated forward rename failure"),
            "the caller must see the original failure, not the rollback: {err}"
        );
        assert!(
            !matches!(err, ProjectError::RollbackFailed { .. }),
            "the rollback succeeded, so this is not that error: {err}"
        );
        assert_eq!(
            manifest_text(&dest),
            "previous",
            "the user's project has to be back where they left it"
        );
        assert!(!tmp.exists(), "stranded {}", tmp.display());
        let strays: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|n| n != "P.rstudio")
            .collect();
        assert!(strays.is_empty(), "left {strays:?} behind");
    }

    #[test]
    fn a_failed_rollback_names_both_surviving_copies_and_deletes_neither() {
        // The worst case this module can reach without a crash: nothing at the
        // save path, and two directories on disk either of which may be the only
        // copy of the work. The error is the only thing that knows their names,
        // so it must carry both and must not have deleted either.
        let dir = tempfile::tempdir().unwrap();
        let (dest, tmp) = staged(dir.path());

        FAIL_FORWARD_RENAME.with(|c| c.set(true));
        FAIL_ROLLBACK_RENAME.with(|c| c.set(true));
        let err = swap_into_place(&dest, &tmp).unwrap_err();

        let ProjectError::RollbackFailed {
            ref backup,
            temp: ref reported_temp,
            ..
        } = err
        else {
            panic!("expected RollbackFailed, got {err}");
        };
        assert_eq!(reported_temp, &tmp);
        assert!(!dest.exists(), "this is the state the error is describing");
        assert!(
            tmp.exists(),
            "the in-flight save may be the only copy; it is named, not deleted"
        );
        assert!(backup.exists(), "and so may the previous package");
        assert_eq!(manifest_text(&tmp), "new");
        assert_eq!(manifest_text(backup), "previous");

        let msg = err.to_string();
        assert!(
            msg.contains(&backup.display().to_string()),
            "the message must name the backup: {msg}"
        );
        assert!(
            msg.contains(&tmp.display().to_string()),
            "the message must name the temp: {msg}"
        );
        assert!(
            msg.contains("simulated forward rename failure")
                && msg.contains("simulated rollback rename failure"),
            "both failures have to be visible: {msg}"
        );
    }

    #[test]
    fn a_failed_first_rename_onto_an_empty_destination_removes_the_temp() {
        // No previous package, so there is nothing to roll back — but the temp
        // still has to go, or a failed first save leaves a full-size `.new-`
        // sibling that `recover` will never touch.
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        let tmp = unique_sibling(&dest, TEMP_PREFIX);
        std::fs::create_dir(&tmp).unwrap();
        std::fs::write(tmp.join(crate::MANIFEST_FILE), b"new").unwrap();

        FAIL_FORWARD_RENAME.with(|c| c.set(true));
        let err = swap_into_place(&dest, &tmp).unwrap_err();
        assert!(
            err.to_string().contains("simulated forward rename failure"),
            "{err}"
        );
        assert!(!dest.exists());
        assert!(!tmp.exists(), "stranded {}", tmp.display());
    }

    #[test]
    fn swap_leaves_no_sibling_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("P.rstudio");
        for round in 0..2 {
            let tmp = unique_sibling(&dest, TEMP_PREFIX);
            std::fs::create_dir(&tmp).unwrap();
            std::fs::write(tmp.join(crate::MANIFEST_FILE), format!("{round}")).unwrap();
            swap_into_place(&dest, &tmp).unwrap();
            assert_eq!(
                std::fs::read_to_string(dest.join(crate::MANIFEST_FILE)).unwrap(),
                format!("{round}")
            );
            let siblings: Vec<_> = std::fs::read_dir(dir.path())
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .filter(|n| n != "P.rstudio")
                .collect();
            assert!(siblings.is_empty(), "left {siblings:?} behind");
        }
    }
}
