//! W5-B: the data-loss routes through the editor — a 16-bit document's save,
//! and the journal hold across a crash, a failed absorb and a restart.
//!
//! A child of `editor::actions_library` (declared there with `#[path]`), so it
//! reaches the editor's private journal-hold helpers the way the editor's own
//! tests do.

use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::{journal_hold_path, Editor};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use compositor::TileSource;

fn png(dir: &Path, name: &str) -> PathBuf {
    // Not one flat value, so a 16-bit round trip has something to keep.
    let mut rgba = Vec::with_capacity(40 * 30 * 4);
    for i in 0..40 * 30u32 {
        rgba.extend_from_slice(&[(i % 251) as u8, (i / 7 % 256) as u8, 90, 255]);
    }
    let bytes = raster::encode(raster::ExportFormat::Png, 40, 30, &rgba).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// Every raster tile of the active document, as (layer index, coord) → bytes.
fn layer_tiles(ed: &Editor) -> Vec<(usize, raster::TileCoord, Vec<u8>)> {
    let doc = ed.active().unwrap();
    let mut out = Vec::new();
    for (i, id) in doc
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .enumerate()
    {
        if let Some(map) = doc.document.layer_tiles(id) {
            for (coord, hash) in map.iter() {
                out.push((i, coord, doc.tiles.tile(hash).unwrap().to_vec()));
            }
        }
    }
    out.sort_by_key(|(i, c, _)| (*i, c.x, c.y, c.level));
    out
}

/// W5-B (P0): Image > Mode > 16 Bits, Save, reopen — and the pixels are the
/// ones that were saved. The tile cap was the RGBA8 size, so every save of a
/// 16-bit document failed and the work existed only in memory.
#[test]
fn a_16_bit_document_saves_and_reopens_with_the_same_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("deep.rstudio");
    let mut ed = editor(dir.path(), ScriptedDialogs::new().saving_to(target.clone()));
    ed.open_path(&png(dir.path(), "a.png")).unwrap();
    crate::menu_bridge::perform(
        ui::menu::MenuAction::SetBitDepth(ui::menu::ChannelDepth::Sixteen),
        &mut ed,
    )
    .unwrap();
    assert!(ed.active().unwrap().is_sixteen_bit());
    let before = layer_tiles(&ed);
    assert!(!before.is_empty());
    let deep = raster::Tile::byte_len(raster::PixelFormat::Rgba16);
    assert!(
        before.iter().all(|(_, _, b)| b.len() == deep),
        "16-bit tiles"
    );

    ed.dispatch(Action::Save).expect("a 16-bit document saves");
    assert!(
        !ed.active().unwrap().is_dirty(),
        "the save landed: {:?}",
        ed.status()
    );
    assert!(target.join(project_format::MANIFEST_FILE).is_file());
    assert_eq!(ed.active().unwrap().project_path(), Some(target.as_path()));

    let mut again = editor(dir.path(), ScriptedDialogs::new());
    again.open_path(&target).unwrap();
    assert!(again.active().unwrap().is_sixteen_bit());
    assert_eq!(layer_tiles(&again), before, "the same pixels came back");
}

/// A saved package with one command held beside it, as a save that was
/// running when the process died leaves it. Returns the package, the hold,
/// and how many layers the package itself has.
fn a_package_with_a_held_command(dir: &Path) -> (PathBuf, PathBuf, usize) {
    let target = dir.join("held.rstudio");
    let mut ed = editor(dir, ScriptedDialogs::new().saving_to(target.clone()));
    ed.open_path(&png(dir, "a.png")).unwrap();
    ed.dispatch(Action::Save).unwrap();
    let saved_layers = ed.active().unwrap().document.layers.len();
    let hold = journal_hold_path(&target);
    ed.docs[0].begin_journal_hold(hold.clone());
    ed.dispatch(Action::NewLayer).unwrap();
    assert!(hold.is_file(), "the command was held aside");
    // The process is gone before the hold is settled.
    drop(ed);
    (target, hold, saved_layers)
}

fn recover_after_a_crash(dir: &Path, target: &Path) -> (Editor, crate::editor::RecoveryReport) {
    let mut ed = editor(dir, ScriptedDialogs::new().answering_recover(true));
    let report = ed.recover(&crate::session::SessionRecord {
        pid: 0,
        open_projects: vec![target.to_path_buf()],
        autosaves: Vec::new(),
    });
    (ed, report)
}

fn siblings_containing(path: &Path, needle: &str) -> Vec<PathBuf> {
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.to_string_lossy().contains(needle))
        .collect()
}

/// W5-B: the absorb used to append the held records and then delete the side
/// file, so a crash between the two replayed the held edit twice on the next
/// open. Now the side file is renamed to a staged name first; whichever step
/// the crash stops at, the reopen has exactly one copy of the held edit.
#[test]
fn a_crash_at_any_step_of_the_absorb_reopens_with_exactly_one_copy_of_the_held_edit() {
    for step in ["before the rename", "after the rename", "after the append"] {
        let dir = tempfile::tempdir().unwrap();
        let (target, hold, saved_layers) = a_package_with_a_held_command(dir.path());
        let journal = target.join(project_format::JOURNAL_FILE);
        let held = std::fs::read(&hold).unwrap();
        let len = std::fs::metadata(&journal).map(|m| m.len()).unwrap_or(0);
        let mut staged = hold.clone().into_os_string();
        staged.push(format!(".absorbing-{len}"));
        match step {
            "before the rename" => {}
            "after the rename" => std::fs::rename(&hold, &staged).unwrap(),
            _ => {
                std::fs::rename(&hold, &staged).unwrap();
                use std::io::Write as _;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&journal)
                    .unwrap()
                    .write_all(&held)
                    .unwrap();
            }
        }

        let (ed, report) = recover_after_a_crash(dir.path(), &target);
        assert_eq!(
            report.restored,
            vec![(target.clone(), 1)],
            "{step}: {report:?}"
        );
        assert_eq!(
            ed.active().unwrap().document.layers.len(),
            saved_layers + 1,
            "{step}: the held layer, once"
        );
        assert!(
            siblings_containing(&hold, "journal-hold").is_empty(),
            "{step}"
        );
        // A second reopen replays the same single command, no more.
        drop(ed);
        let rec = crate::session::recoverable(&target).unwrap().unwrap();
        assert_eq!(rec.commands.len(), 1, "{step}");
    }
}

/// W5-B: a crash between the two renames of a save leaves the package under
/// its backup name and no manifest where it was. The absorb returned early on
/// the missing manifest, before the open that recovers the package ran, so
/// the held command was never absorbed and recovery found nothing to restore.
#[test]
fn a_hold_next_to_an_interrupted_save_is_absorbed_after_the_package_is_put_back() {
    let dir = tempfile::tempdir().unwrap();
    let (target, hold, saved_layers) = a_package_with_a_held_command(dir.path());
    let mut backup = target.clone().into_os_string();
    backup.push(".bak-1-2-3");
    std::fs::rename(&target, &backup).unwrap();
    assert!(!target.join(project_format::MANIFEST_FILE).exists());

    let (ed, report) = recover_after_a_crash(dir.path(), &target);
    assert_eq!(report.restored, vec![(target.clone(), 1)], "{report:?}");
    assert!(!hold.exists(), "absorbed");
    assert_eq!(ed.active().unwrap().document.layers.len(), saved_layers + 1);
}

/// W5-B: an absorb that fails only warned, and the next save's hold then
/// deleted the side file — the held commands gone. It is renamed aside now,
/// with its records intact.
#[test]
fn a_failed_absorb_keeps_the_held_commands_aside() {
    let dir = tempfile::tempdir().unwrap();
    let (target, hold, _) = a_package_with_a_held_command(dir.path());
    let held = std::fs::read(&hold).unwrap();
    // A journal the absorb refuses to write into.
    let journal = target.join(project_format::JOURNAL_FILE);
    let _ = std::fs::remove_file(&journal);
    std::fs::create_dir(&journal).unwrap();

    Editor::settle_journal_hold(Some(hold.clone()), Some(&target));
    assert!(!hold.exists(), "not left for the next hold to delete");
    let kept = siblings_containing(&hold, "unabsorbed");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read(&kept[0]).unwrap(), held);
}

/// W5-B: a stale side file a new hold finds is set aside, not deleted.
#[test]
fn a_new_hold_sets_a_stale_side_file_aside() {
    let dir = tempfile::tempdir().unwrap();
    let (_, hold, _) = a_package_with_a_held_command(dir.path());
    let held = std::fs::read(&hold).unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&png(dir.path(), "b.png")).unwrap();
    ed.docs[0].begin_journal_hold(hold.clone());
    assert!(!hold.exists());
    let kept = siblings_containing(&hold, "unabsorbed");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert_eq!(std::fs::read(&kept[0]).unwrap(), held);
}

/// W5-B: scratch holds (a document with no package has no journal to absorb
/// into) left by runs that are gone are swept when the editor starts; a live
/// run's hold and anything else in the directory are left alone.
#[test]
fn the_editor_sweeps_dead_runs_scratch_holds_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::rooted(dir.path().join("config"));
    let scratch = Preferences::default().scratch_dir(&paths);
    std::fs::create_dir_all(&scratch).unwrap();
    let dead = scratch.join("hold-0-1f-0-1.journal");
    let live = scratch.join(format!("hold-{:x}-1f-0-1.journal", std::process::id()));
    let other = scratch.join("autosave-0-1f-0-1.rstudio");
    let odd = scratch.join("hold-notapid-1.journal");
    for p in [&dead, &live, &other, &odd] {
        std::fs::write(p, b"x").unwrap();
    }

    let _ed = Editor::new(paths, Box::new(ScriptedDialogs::new()));
    assert!(!dead.exists(), "a dead run's hold is swept");
    assert!(live.exists(), "a live run's hold is kept");
    assert!(other.exists() && odd.exists());
}
