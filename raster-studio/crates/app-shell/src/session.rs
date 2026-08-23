//! Unclean-shutdown detection and journal-based recovery.
//!
//! # How a crash is noticed
//!
//! Every run owns **one marker file of its own**, `sessions/{pid}.json`. It is
//! written at start-up and deleted on a clean exit. At start-up a run reads
//! every marker in that directory: one whose process id no longer names a
//! running process belonged to a run that never reached its exit path — a
//! crash, a power cut, or a kill — and its record says which packages were open
//! and which scratch autosaves were live, so recovery has somewhere to look.
//!
//! This is deliberately *not* a lock file: a stale marker never blocks a start,
//! it only offers a restore.
//!
//! # Two instances at once
//!
//! Both halves of that matter, and both used to be wrong.
//!
//! * **One file per process.** A single shared `session.json` meant the second
//!   instance overwrote the first instance's record, so a clean exit of the
//!   second deleted the marker and a later crash of the first recovered
//!   nothing. A run now only ever writes the file named after its own pid, and
//!   [`SessionMarker::finish`] only ever removes that one.
//! * **A real liveness check.** [`process_is_running`] asks the operating
//!   system whether the recorded pid is still alive. A marker belonging to a
//!   live process is *not* a crash: it is skipped, left on disk, and never
//!   offered — which matters because declining a recovery deletes the autosave
//!   it was offering, and that autosave may be another instance's only copy of
//!   an hour of unsaved work.
//!
//! # What can be recovered
//!
//! `project-format` appends every accepted command to `commands.journal` and
//! writes a save marker on every save. So the recoverable work is exactly
//! [`JournalRecovery::since_last_save`] — the commands accepted after the last
//! save marker. Replaying the *whole* journal onto a loaded document would
//! duplicate everything the snapshot already holds, which is why that is not
//! what happens here.
//!
//! # The other half: documents that were never saved anywhere
//!
//! A document with no package has no journal either, so the paragraph above
//! reaches none of it. That work lives in *scratch autosaves*
//! ([`crate::Editor::autosave_now`]), and the marker records those packages in
//! [`SessionRecord::autosaves`] as they are written. Without that second list
//! the autosave was work nothing could ever read back — see
//! `an_unsaved_document_is_recovered_from_its_scratch_autosave`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use editor_core::{Command, Document, History};
use project_format::{CommandJournal, JOURNAL_FILE};

use crate::prefs::AppPaths;

/// What the previous run left behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Process id of the run that wrote this. Used to tell "a crash" from
    /// "another editor is running right now".
    pub pid: u32,
    /// Project packages that were open, in tab order.
    #[serde(default)]
    pub open_projects: Vec<PathBuf>,
    /// Scratch autosaves of documents that had **no** package of their own.
    ///
    /// Separate from `open_projects` because they are recovered differently:
    /// the package is opened and then detached from disk, since the location is
    /// the application's scratch directory rather than one the user chose.
    #[serde(default)]
    pub autosaves: Vec<PathBuf>,
}

/// The marker file, owned for the lifetime of a run.
#[derive(Debug)]
pub struct SessionMarker {
    path: PathBuf,
    record: SessionRecord,
}

/// `true` when `pid` names a process that is running on this machine right now.
///
/// The question the marker directory cannot answer for itself: a file left at
/// `sessions/4711.json` is a crash to recover *only* if 4711 is gone. Answered
/// by the operating system rather than guessed.
///
/// Deliberately conservative in both directions. "Running but not ours"
/// (`ERROR_ACCESS_DENIED` on Windows, `EPERM` on Unix) counts as running: the
/// process exists, and the cost of calling a live run dead is destroying its
/// autosave. Pid 0 is never a process this can ask about — on Unix `kill(0, 0)`
/// signals *our own process group* — so it is answered without asking.
pub fn process_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    platform_process_is_running(pid)
}

#[cfg(windows)]
fn platform_process_is_running(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: every argument is a plain integer, the handle is closed on every
    // path out, and `code` is a live local for the duration of the call.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            // A process owned by another user exists; we just may not look.
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let queried = GetExitCodeProcess(handle, &mut code) != 0;
        CloseHandle(handle);
        // A handle can outlive the process it names, so "the handle opened" is
        // not the answer; "it has not exited" is.
        !queried || code == STILL_ACTIVE as u32
    }
}

#[cfg(unix)]
fn platform_process_is_running(pid: u32) -> bool {
    // A pid that does not fit in `pid_t` cannot name a process, and passing a
    // negative value to `kill` would address a whole process *group*.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 performs the permission and existence checks without
    // delivering anything.
    let sent = unsafe { libc::kill(pid, 0) };
    if sent == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(any(windows, unix)))]
fn platform_process_is_running(_pid: u32) -> bool {
    // No way to ask: assume alive. That loses crash recovery on such a
    // platform, which is the safe half of the trade — the other half deletes
    // work that is still in use.
    true
}

impl SessionMarker {
    /// Claim this run's marker, reporting every unclean run found on the way.
    ///
    /// A marker whose process is still alive is a concurrent instance, not a
    /// crash: it is left exactly as it is and reported to nobody.
    pub fn begin(paths: &AppPaths) -> (SessionMarker, Vec<SessionRecord>) {
        SessionMarker::begin_with(paths, &process_is_running)
    }

    /// [`SessionMarker::begin`] with the liveness probe supplied.
    ///
    /// The seam exists so "a live instance's marker is not touched" is a test
    /// rather than a claim: a unit test cannot conjure a second running copy of
    /// the editor, but it can say what the answer is.
    pub fn begin_with(
        paths: &AppPaths,
        is_running: &dyn Fn(u32) -> bool,
    ) -> (SessionMarker, Vec<SessionRecord>) {
        let mut unclean = Vec::new();
        for path in marker_files(paths) {
            match read_record(&path) {
                Some(record) => {
                    if is_running(record.pid) {
                        // A live instance — possibly this one, if a previous
                        // marker of ours is still there. Not a crash, and not
                        // ours to delete.
                        continue;
                    }
                    unclean.push(record);
                }
                None => {
                    // Unreadable: a crash caught mid-write. There is nothing to
                    // recover from it, and leaving it re-reads it for ever.
                    tracing::warn!("discarding an unreadable session marker {}", path.display());
                }
            }
            // Handed over (or unreadable); either way this run owns the
            // clean-up. The record has been read into `unclean` and the caller
            // recovers from it immediately; anything it restores is re-recorded
            // in *this* run's marker by `Shell::sync_marker`, so the pointer to
            // the work is never without an owner for longer than that.
            if let Err(e) = std::fs::remove_file(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("cannot clear the session marker {}: {e}", path.display());
                }
            }
        }

        let pid = std::process::id();
        let marker = SessionMarker {
            path: paths.session_file_for(pid),
            record: SessionRecord {
                pid,
                open_projects: Vec::new(),
                autosaves: Vec::new(),
            },
        };
        marker.write();
        (marker, unclean)
    }

    /// Record which packages are open, so a crash can name them.
    pub fn set_open_projects(&mut self, projects: Vec<PathBuf>) {
        if self.record.open_projects != projects {
            self.record.open_projects = projects;
            self.write();
        }
    }

    /// Record which scratch autosaves are live, so a crash can offer them.
    pub fn set_autosaves(&mut self, autosaves: Vec<PathBuf>) {
        if self.record.autosaves != autosaves {
            self.record.autosaves = autosaves;
            self.write();
        }
    }

    pub fn record(&self) -> &SessionRecord {
        &self.record
    }

    fn write(&self) {
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match serde_json::to_string(&self.record) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&self.path, json) {
                    tracing::warn!("cannot write the session marker: {e}");
                }
            }
            Err(e) => tracing::warn!("cannot serialize the session marker: {e}"),
        }
    }

    /// Where this run's marker lives. Only ever this run's own file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Clean exit: remove **this run's** marker so the next start is not
    /// offered a recovery it does not need. Another instance's marker is not
    /// this run's to touch.
    pub fn finish(self) {
        if let Err(e) = std::fs::remove_file(&self.path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!("cannot clear the session marker: {e}");
            }
        }
    }
}

/// Every marker file to consider at start-up, in a stable order.
///
/// The legacy single-file marker is included so a crash of a build that
/// predates the per-process layout is still recoverable.
fn marker_files(paths: &AppPaths) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let legacy = paths.legacy_session_file();
    if legacy.is_file() {
        out.push(legacy);
    }
    if let Ok(entries) = std::fs::read_dir(paths.session_dir()) {
        let mut found: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        found.sort();
        out.extend(found);
    }
    out
}

fn read_record(path: &Path) -> Option<SessionRecord> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<SessionRecord>(&text).ok()
}

/// Work found in a package's journal that the package itself does not contain.
#[derive(Debug, Clone)]
pub struct Recoverable {
    pub project: PathBuf,
    /// Commands accepted after the last save marker.
    pub commands: Vec<Command>,
    /// The journal stopped at a record that could not be read — the tail is
    /// what a crash mid-append leaves. Reported so the UI can say the recovery
    /// is partial rather than pretending it is whole.
    pub truncated: bool,
}

impl Recoverable {
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }
}

/// What a package's journal holds beyond its last save, if anything.
///
/// `Ok(None)` means there is nothing to recover, which includes a package with
/// no journal at all.
pub fn recoverable(project: &Path) -> Result<Option<Recoverable>, project_format::ProjectError> {
    let journal = project.join(JOURNAL_FILE);
    if !journal.exists() {
        return Ok(None);
    }
    let recovery = CommandJournal::read(&journal)?;
    let commands = recovery.since_last_save().to_vec();
    if commands.is_empty() {
        return Ok(None);
    }
    Ok(Some(Recoverable {
        project: project.to_path_buf(),
        commands,
        truncated: recovery.truncated(),
    }))
}

/// Replay recovered commands onto a freshly loaded document.
///
/// They go through [`History`] rather than straight onto the document, so
/// recovered work is undoable exactly like work the user just did — a restore
/// the user did not want is one Ctrl+Z away.
///
/// A failure stops the replay rather than aborting it: the earlier commands are
/// real work, and a journal from a crashed process can end in a record that no
/// longer applies. Returns how many were applied and why it stopped.
pub fn replay(
    doc: &mut Document,
    history: &mut History,
    commands: &[Command],
) -> (usize, Option<String>) {
    let mut applied = 0;
    for cmd in commands {
        match history.apply(doc, cmd.clone()) {
            Ok(()) => applied += 1,
            Err(e) => return (applied, Some(e.to_string())),
        }
    }
    (applied, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::Command;
    use layer_model::Layer;

    fn app_paths(dir: &tempfile::TempDir) -> AppPaths {
        AppPaths::rooted(dir.path())
    }

    /// Write `record` where a run with its pid would have left it.
    fn plant(paths: &AppPaths, record: &SessionRecord) -> PathBuf {
        let path = paths.session_file_for(record.pid);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_string(record).unwrap()).unwrap();
        path
    }

    fn foreign(pid: u32) -> SessionRecord {
        SessionRecord {
            pid,
            open_projects: vec![PathBuf::from("/projects/one.rstudio")],
            autosaves: vec![PathBuf::from("/scratch/autosave-x.rstudio")],
        }
    }

    /// A pid that is neither ours nor 0, for planting foreign markers.
    fn foreign_pid() -> u32 {
        std::process::id().wrapping_add(1).max(1)
    }

    #[test]
    fn a_clean_exit_leaves_nothing_to_recover() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let (marker, previous) = SessionMarker::begin(&paths);
        assert!(previous.is_empty(), "first ever start");
        let path = marker.path().to_path_buf();
        assert!(path.exists());
        assert_eq!(path, paths.session_file_for(std::process::id()));
        marker.finish();
        assert!(!path.exists());

        let (marker, previous) = SessionMarker::begin(&paths);
        assert!(previous.is_empty(), "a clean exit is not a crash");
        marker.finish();
    }

    #[test]
    fn a_marker_left_behind_by_a_dead_process_is_an_unclean_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        // A pid that is not this process: what a crashed previous run leaves.
        let crashed = foreign(foreign_pid());
        let planted = plant(&paths, &crashed);

        let (marker, previous) = SessionMarker::begin_with(&paths, &|_| false);
        assert_eq!(previous.len(), 1, "the crash must be reported");
        assert_eq!(previous[0].open_projects, crashed.open_projects);
        assert_eq!(
            previous[0].autosaves, crashed.autosaves,
            "a never-saved document's autosave must survive into the next run"
        );
        assert_eq!(marker.record().pid, std::process::id());
        assert!(
            !planted.exists(),
            "a marker handed to recovery must not be offered again at the next start"
        );
        marker.finish();
    }

    #[test]
    fn a_live_foreign_instances_marker_is_neither_a_crash_nor_ours_to_touch() {
        // The defect: `begin` only compared the recorded pid with our own —
        // never true for a *second running copy* — so the second instance
        // announced the first's open documents as a crash, and declining that
        // offer ran `remove_dir_all` on the first instance's live autosave. It
        // then overwrote the marker, so a clean exit of the second deleted it
        // and a later crash of the first recovered nothing.
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let live = foreign(foreign_pid());
        let planted = plant(&paths, &live);
        let before = std::fs::read_to_string(&planted).unwrap();

        let (marker, previous) = SessionMarker::begin_with(&paths, &|pid| pid == live.pid);
        assert!(
            previous.is_empty(),
            "a running instance is not a crash: {previous:?}"
        );
        assert!(planted.exists(), "and its marker is not ours to delete");
        assert_eq!(
            std::fs::read_to_string(&planted).unwrap(),
            before,
            "nor ours to overwrite"
        );
        assert_ne!(
            marker.path(),
            planted,
            "this run writes a marker of its own"
        );
        marker.finish();
        assert!(planted.exists(), "and takes only its own away again");
    }

    #[test]
    fn the_liveness_probe_answers_for_a_real_process() {
        // If this ever returned a constant, the test above would pass while the
        // application still treated every live instance as a crash.
        assert!(
            process_is_running(std::process::id()),
            "this process is, in fact, running"
        );
        assert!(
            !process_is_running(0),
            "pid 0 is not a process to ask about"
        );
    }

    #[test]
    fn an_unreadable_marker_is_discarded_rather_than_re_read_for_ever() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let path = paths.session_file_for(foreign_pid());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ truncated mid-write").unwrap();

        let (marker, previous) = SessionMarker::begin_with(&paths, &|_| false);
        assert!(previous.is_empty(), "there is nothing in it to recover");
        assert!(!path.exists());
        marker.finish();
    }

    #[test]
    fn a_marker_from_the_single_file_layout_is_still_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let crashed = foreign(foreign_pid());
        paths.ensure().unwrap();
        std::fs::write(
            paths.legacy_session_file(),
            serde_json::to_string(&crashed).unwrap(),
        )
        .unwrap();

        let (marker, previous) = SessionMarker::begin_with(&paths, &|_| false);
        assert_eq!(previous.len(), 1);
        assert_eq!(previous[0].open_projects, crashed.open_projects);
        assert!(!paths.legacy_session_file().exists());
        marker.finish();
    }

    #[test]
    fn every_dead_instance_is_reported_not_just_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let mut a = foreign(foreign_pid());
        a.open_projects = vec![PathBuf::from("/a.rstudio")];
        let mut b = foreign(foreign_pid().wrapping_add(1).max(1));
        b.open_projects = vec![PathBuf::from("/b.rstudio")];
        plant(&paths, &a);
        plant(&paths, &b);

        let (marker, previous) = SessionMarker::begin_with(&paths, &|_| false);
        let mut found: Vec<PathBuf> = previous
            .iter()
            .flat_map(|r| r.open_projects.clone())
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![PathBuf::from("/a.rstudio"), PathBuf::from("/b.rstudio")],
            "one shared marker file used to lose all but the last run"
        );
        marker.finish();
    }

    #[test]
    fn scratch_autosaves_are_recorded_alongside_the_open_projects() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let (mut marker, _) = SessionMarker::begin(&paths);
        marker.set_open_projects(vec![PathBuf::from("/a.rstudio")]);
        marker.set_autosaves(vec![PathBuf::from("/scratch/autosave-7-2.rstudio")]);
        let on_disk: SessionRecord =
            serde_json::from_str(&std::fs::read_to_string(marker.path()).unwrap()).unwrap();
        assert_eq!(on_disk.open_projects.len(), 1);
        assert_eq!(
            on_disk.autosaves,
            vec![PathBuf::from("/scratch/autosave-7-2.rstudio")],
            "the marker is the only thing that can point recovery at scratch"
        );
        marker.finish();
    }

    #[test]
    fn a_marker_written_before_autosaves_existed_still_reads() {
        // The field is `#[serde(default)]` for exactly this: a crash whose
        // marker predates the upgrade must still offer its open projects.
        let old = r#"{"pid":1,"open_projects":["/a.rstudio"]}"#;
        let rec: SessionRecord = serde_json::from_str(old).unwrap();
        assert_eq!(rec.open_projects, vec![PathBuf::from("/a.rstudio")]);
        assert!(rec.autosaves.is_empty());
    }

    #[test]
    fn our_own_marker_is_not_reported_as_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let (first, _) = SessionMarker::begin(&paths);
        // Same pid, and this process is alive: the liveness probe says so, so
        // this is us rather than a previous run.
        let (second, previous) = SessionMarker::begin(&paths);
        assert!(previous.is_empty(), "{previous:?}");
        second.finish();
        drop(first);
    }

    #[test]
    fn open_projects_are_recorded_for_the_next_start() {
        let dir = tempfile::tempdir().unwrap();
        let paths = app_paths(&dir);
        let (mut marker, _) = SessionMarker::begin(&paths);
        marker.set_open_projects(vec![
            PathBuf::from("/a.rstudio"),
            PathBuf::from("/b.rstudio"),
        ]);
        let on_disk: SessionRecord =
            serde_json::from_str(&std::fs::read_to_string(marker.path()).unwrap()).unwrap();
        assert_eq!(on_disk.open_projects.len(), 2);
        marker.finish();
    }

    #[test]
    fn the_journal_suffix_after_the_last_save_is_what_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("p.rstudio");
        std::fs::create_dir_all(&project).unwrap();
        let journal = project.join(JOURNAL_FILE);

        // No journal at all: nothing to recover.
        assert!(recoverable(&project).unwrap().is_none());

        let mut doc = Document::new(64, 64, "p");
        let saved = Command::create_layer(Layer::raster("saved before"));
        saved.apply(&mut doc).unwrap();
        CommandJournal::append(&journal, &saved).unwrap();
        let bytes = rmp_serde::to_vec_named(&doc).unwrap();
        CommandJournal::mark_saved(&journal, project_format::DocumentDigest::of(&bytes)).unwrap();

        // Nothing after the save marker yet.
        assert!(recoverable(&project).unwrap().is_none());

        let lost = Command::create_layer(Layer::raster("after the save"));
        CommandJournal::append(&journal, &lost).unwrap();

        let rec = recoverable(&project).unwrap().expect("one command to redo");
        assert_eq!(rec.commands.len(), 1, "only the suffix, not the whole log");
        assert!(!rec.truncated);
        assert!(!rec.is_empty());

        let mut history = History::new();
        let (applied, error) = replay(&mut doc, &mut history, &rec.commands);
        assert_eq!((applied, error), (1, None));
        assert_eq!(doc.layers.len(), 2);
        assert!(history.can_undo(), "recovered work must be undoable");
    }

    #[test]
    fn a_replay_that_cannot_finish_keeps_what_it_managed() {
        let mut doc = Document::new(16, 16, "d");
        let good = Command::create_layer(Layer::raster("kept"));
        let bad = Command::DeleteLayer {
            layer_id: layer_model::LayerId::new(),
        };
        let mut history = History::new();
        let (applied, error) = replay(&mut doc, &mut history, &[good, bad]);
        assert_eq!(applied, 1);
        assert!(error.is_some(), "the failure must be reported");
        assert_eq!(doc.layers.len(), 1, "the good command survived");
    }
}
