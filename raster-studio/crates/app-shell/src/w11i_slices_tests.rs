//! W11-I: slices are saved in the `.rstudio` document and restored from it.
//! Driven through the editor: a slice set committed the way the Slice tool
//! commits one, Slice Options edited, the document saved as a package and
//! opened again in a fresh editor.

use std::path::Path;

use raster::PixelRect;
use tools::slice_select::SliceOptions;

use super::*;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

fn slice(x: i64, y: i64, width: u32, height: u32) -> tools::Slice {
    tools::Slice {
        rect: PixelRect::new(x, y, width, height),
        name: String::new(),
    }
}

#[test]
fn a_committed_slice_set_is_saved_in_the_document_and_restored_on_open() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let png = dir.path().join("poster.png");
    let rgba: Vec<u8> = (0..48 * 32).flat_map(|_| [10u8, 20, 30, 255]).collect();
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, 48, 32, &rgba).unwrap(),
    )
    .unwrap();
    ed.open_path(&png).unwrap();

    remember_committed(&mut ed, &[slice(0, 0, 24, 32), slice(24, 0, 24, 32)]);
    set_slice_options(
        &mut ed,
        0,
        SliceOptions {
            name: "hero".into(),
            url: "https://example.com".into(),
            alt: "Hero".into(),
        },
    )
    .unwrap();
    {
        let doc = &ed.active().unwrap().document;
        assert_eq!(doc.slices.len(), 2, "the set is written into the document");
        assert_eq!(doc.slices[0].name, "hero");
        assert!(doc.is_dirty(), "a new slice set is an unsaved change");
    }

    let project = dir.path().join("poster.rstudio");
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();

    // A fresh editor, as after a restart.
    let mut again = editor(dir.path());
    let id = again.open_path(&project).unwrap();
    assert_eq!(
        again.active().unwrap().document.slices,
        ed.active().unwrap().document.slices,
        "the package carries the slices"
    );
    // The open route itself put them in the store the Slice tools read.
    assert_eq!(
        again.slices.get(id),
        &[PixelRect::new(0, 0, 24, 32), PixelRect::new(24, 0, 24, 32)]
    );
    let options = again.slices.options(id);
    assert_eq!(options[0].name, "hero");
    assert_eq!(options[0].url, "https://example.com");
    assert_eq!(options[0].alt, "Hero");
    assert_eq!(options[1].name, "slice_02");
    // A second restore does not clobber the live set.
    assert_eq!(restore_saved_slices(&mut again), 0);
}

fn editor_opening(dir: &Path, file: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new().opening(file)),
    )
}

fn write_png(path: &Path, w: u32, h: u32) {
    let rgba: Vec<u8> = (0..w * h).flat_map(|_| [10u8, 20, 30, 255]).collect();
    std::fs::write(
        path,
        raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap(),
    )
    .unwrap();
}

/// W11-I round 2: File > Open's background job (the route a `.psd` takes)
/// restores the slices the file carried, with no slice action needed first:
/// they are in the store the Slice tools and the canvas overlay read.
#[test]
fn file_open_s_background_job_restores_the_slices_a_file_carried() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("poster.png");
    write_png(&png, 48, 32);
    let mut ed = editor(dir.path());
    ed.open_path(&png).unwrap();
    remember_committed(&mut ed, &[slice(0, 0, 24, 32), slice(24, 0, 24, 32)]);
    let psd = dir.path().join("poster.psd");
    ed.active_mut().unwrap().export_psd_to(&psd).unwrap();

    let mut again = editor_opening(dir.path(), &psd);
    again.dispatch(crate::action::Action::Open).unwrap();
    again.poll_imports();
    let id = again.active().expect("the job opened the file").id();
    assert_eq!(
        again.slices.get(id),
        &[PixelRect::new(0, 0, 24, 32), PixelRect::new(24, 0, 24, 32)],
        "the job's open route restored the saved slices"
    );
}

/// W11-I round 2 regression: committing a slice set in one document must
/// not wipe another open document's saved slices, nor mark it dirty.
#[test]
fn a_slice_commit_in_one_document_leaves_another_document_s_saved_slices_alone() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("a.png");
    write_png(&png, 48, 32);
    let mut ed = editor(dir.path());
    ed.open_path(&png).unwrap();
    remember_committed(&mut ed, &[slice(0, 0, 24, 32), slice(24, 0, 24, 32)]);
    let project = dir.path().join("a.rstudio");
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();

    let mut again = editor(dir.path());
    let a = again.open_path(&project).unwrap();
    assert!(!again.active().unwrap().document.is_dirty());
    let png_b = dir.path().join("b.png");
    write_png(&png_b, 40, 40);
    let b = again.open_path(&png_b).unwrap();
    // A store that holds no set for A (as before the open route restored
    // one): writing B's slices back must still leave A's record alone.
    again.slices.remember(a, Vec::new());
    remember_committed(&mut again, &[slice(0, 0, 10, 10)]);
    // Deleting B's picked slice goes through the other write-back path.
    again.slices.set_picked(b, Some(0));
    again.set_tool(tools::ToolId::SliceSelect);
    delete_picked_slice(&mut again).unwrap().unwrap();

    let doc_a = again.documents().iter().find(|d| d.id() == a).unwrap();
    assert_eq!(doc_a.document.slices.len(), 2, "A's saved slices survive");
    assert!(!doc_a.document.is_dirty(), "A has no unsaved change");
}
