//! File ▸ Save as PSD… opens the *PSD* picker: the one whose filter list
//! leads with Photoshop and whose suggested name wears `.psd`.
//!
//! The picker's answer is scripted, and a scripted picker answers the `.psd`
//! path whatever it was asked — so a test that only looks at the written
//! file, or at the path that came back, is green with the arming removed.
//! This one records the *request* through the seam the real `rfd` dialog is
//! built from ([`app_shell::dialogs::ExportPickerRequest`]) and asserts the
//! filter order and the suggestion, through the menu row's real route:
//! `resolve_intent` (the row is enabled and routable) and `perform` (the arm
//! the pick reaches).

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use app_shell::dialogs::{CloseChoice, ExportPickerRequest, FileDialogs, PSD_EXTENSION};
use app_shell::menu_bridge::{context, perform, resolve_intent};
use app_shell::{Action, AppPaths, Editor, Preferences, RecentFiles};
use ui::menu::MenuAction;
use ui::Workspace;

/// Every export picker request, shared with the test after the double is
/// boxed into the editor.
type Requests = Rc<RefCell<Vec<ExportPickerRequest>>>;

/// A dialogs double that records each export picker as it would be shown
/// and answers it with one fixed path. Every other question cancels.
struct RecordingPicker {
    requests: Requests,
    answer: Option<PathBuf>,
}

impl FileDialogs for RecordingPicker {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        None
    }
    fn pick_place_file(&mut self) -> Option<PathBuf> {
        None
    }
    fn pick_replace_file(&mut self) -> Option<PathBuf> {
        None
    }
    fn pick_open_project(&mut self) -> Option<PathBuf> {
        None
    }
    fn pick_save_path(&mut self, _suggested: &Path) -> Option<PathBuf> {
        None
    }
    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        // The same request the native picker builds its dialog from.
        self.requests
            .borrow_mut()
            .push(ExportPickerRequest::next(suggested));
        self.answer.take()
    }
    fn pick_export_folder(&mut self) -> Option<PathBuf> {
        None
    }
    fn confirm_close(&mut self, _document: &str) -> CloseChoice {
        CloseChoice::Cancel
    }
    fn confirm_recover(&mut self, _document: &str) -> bool {
        false
    }
    fn report_error(&mut self, _title: &str, _message: &str) {}
    fn report_notice(&mut self, _title: &str, _message: &str) {}
}

fn png(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
    )
    .unwrap();
    path
}

/// An editor over one opened PNG whose export picker records its requests
/// and answers `target`.
fn editor_exporting_to(dir: &Path, target: &Path) -> (Editor, Requests) {
    let requests = Requests::default();
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(RecordingPicker {
            requests: requests.clone(),
            answer: Some(target.to_path_buf()),
        }),
    );
    ed.open_path(&png(dir, "photo.png"))
        .expect("the probe opens");
    (ed, requests)
}

#[test]
fn save_as_psd_asks_the_picker_for_a_psd_first_and_suggests_a_psd_name() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("stacked.psd");
    let (mut ed, requests) = editor_exporting_to(dir.path(), &target);

    // The row is enabled over an open document and resolves to an intent
    // the chrome can route — the menu's own gate, not a shortcut past it.
    let menu_ctx = context(&mut ed, &Workspace::new());
    resolve_intent(MenuAction::SaveAsPsd, &menu_ctx, &ed)
        .expect("File > Save as PSD... is enabled over an open document");
    // The arm the pick reaches.
    perform(MenuAction::SaveAsPsd, &mut ed).expect("the export ran");

    let seen = requests.borrow();
    assert_eq!(seen.len(), 1, "exactly one picker opened: {seen:?}");
    let request = &seen[0];
    assert!(
        request.leads_with_psd(),
        "File > Save as PSD must open the picker on the PSD filter, not on {:?}: {request:?}",
        request.filters.first()
    );
    assert_eq!(
        request.filters[0],
        ("Photoshop", &[PSD_EXTENSION] as &[&str]),
        "the first filter is Photoshop/.psd"
    );
    assert_eq!(request.title, "Save as PSD");
    assert_eq!(
        request.suggested_extension().as_deref(),
        Some(PSD_EXTENSION),
        "the suggested name wears .psd: {:?}",
        request.suggested
    );
    assert_eq!(
        request.suggested.file_stem().and_then(|s| s.to_str()),
        Some("photo"),
        "the suggestion keeps the document's own name"
    );
    drop(seen);
    assert!(target.is_file(), "the layered PSD was written");
    assert!(
        ed.status().unwrap_or("").contains("PSD"),
        "the status names the PSD: {:?}",
        ed.status()
    );
}

#[test]
fn the_plain_export_picker_does_not_lead_with_psd() {
    // The control for the test above: the same route without the arming —
    // File ▸ Export… — opens the plain picker, which leads with PNG and
    // suggests a raster name. So "leads with PSD" is Save as PSD's doing,
    // not something every export picker satisfies.
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("flat.png");
    let (mut ed, requests) = editor_exporting_to(dir.path(), &target);
    ed.dispatch(Action::Export).expect("the export ran");
    let seen = requests.borrow();
    assert_eq!(seen.len(), 1, "exactly one picker opened: {seen:?}");
    let request = &seen[0];
    assert!(!request.leads_with_psd(), "{request:?}");
    assert_eq!(request.filters[0].0, "PNG");
    assert_eq!(request.title, "Export");
    assert_ne!(
        request.suggested_extension().as_deref(),
        Some(PSD_EXTENSION)
    );
    assert_eq!(
        request.filters.last().map(|f| f.0),
        Some("Photoshop"),
        "PSD is still offered, last"
    );
}
