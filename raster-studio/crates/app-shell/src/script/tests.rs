//! W13-K: File ▸ Script driven the way a user reaches it — the File menu row
//! clicked through the chrome, the source typed into the window, the Run
//! button pressed in a headless frame of the whole chrome, the pick that
//! frame produced performed as the shell performs it — and checked on the
//! document, on the history stack and on the package journal.

use std::path::Path;

use editor_core::{Command, Selection};
use layer_model::LayerKind;
use ui::dialogs::ScriptLogKind;
use ui::menu::MenuAction;

use super::{run, ScriptEnd, ScriptLimits, SCRIPT_LABEL};
use crate::action::Action;
use crate::chrome::{install_theme, Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor_in(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

/// A 16x16 white document, the only one open.
fn editor_with_doc(dir: &Path) -> Editor {
    let mut ed = editor_in(dir);
    ed.new_document_with(
        16,
        16,
        "Doc",
        crate::import::BlankBackground::Solid {
            rgba8: [255, 255, 255, 255],
            depth: raster::BitDepth::Eight,
        },
    )
    .unwrap();
    ed
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

fn click_at(at: egui::Pos2) -> Vec<egui::Event> {
    vec![
        egui::Event::PointerMoved(at),
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        },
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        },
    ]
}

/// Perform every menu pick a frame produced, as `Shell::apply_chrome` does.
fn perform_all(ed: &mut Editor, out: &ChromeOutput) -> Vec<Result<String, String>> {
    out.menu
        .iter()
        .map(|action| crate::menu_bridge::perform(*action, ed))
        .collect()
}

/// Click File ▸ Script through the chrome, type `source`, press Run in a
/// headless frame of the whole chrome, and perform what that frame picked.
fn run_through_the_window(ed: &mut Editor, source: &str) -> Vec<Result<String, String>> {
    let mut chrome = Chrome::new();
    // The menu row, resolved against the live editor and routed as a click.
    let menu = crate::menu_bridge::context(ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(MenuAction::Script, &menu, ed)
        .unwrap_or_else(|reason| panic!("File > Script is disabled: {reason}"));
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    assert_eq!(out.menu, vec![MenuAction::Script], "the row picks Script");
    let opened = perform_all(ed, &out);
    assert!(opened[0].is_ok(), "{opened:?}");
    assert!(super::window_open(), "File > Script opened the window");
    super::with_window(|w| w.set_source(source)).unwrap();

    // Settle the whole chrome (the window is drawn by the menu bar), then
    // press Run where it was drawn.
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut last = None;
    for _ in 0..6 {
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            let out = chrome.ui(ctx, ed);
            assert!(out.menu.is_empty(), "nothing runs before Run is pressed");
        });
        last = super::with_window(|w| w.run_rect()).flatten();
    }
    let at = last.expect("the Run button was drawn").center();
    let mut out = ChromeOutput::default();
    let _ = ctx.run(raw_input(click_at(at)), |ctx| {
        out = chrome.ui(ctx, ed);
    });
    assert_eq!(
        out.menu,
        vec![MenuAction::Script],
        "Run clicks the Script row"
    );
    perform_all(ed, &out)
}

fn layer_named(ed: &Editor, name: &str) -> Option<layer_model::LayerId> {
    let open = ed.active()?;
    open.document
        .layers
        .iter_depth_first()
        .into_iter()
        .find(|id| {
            open.document
                .layers
                .get(*id)
                .is_some_and(|l| l.name == name)
        })
}

const FILL_SCRIPT: &str = r#"
var doc = app.activeDocument;
var layer = doc.artLayers.add();
doc.selection.select([[2, 2], [10, 2], [10, 8], [2, 8]]);
var red = new SolidColor();
red.rgb.hexValue = "FF0000";
doc.selection.fill(red);
layer.name = "Scripted";
alert(doc.layers.length + " layers, top is " + doc.layers[0].name);
"#;

/// The spec's case: a script that creates a layer, fills a selection and
/// renames the layer changes the document through the real route, and ONE
/// Undo takes the whole run back.
#[test]
fn a_script_run_from_the_window_edits_the_document_as_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let depth0 = ed.active().unwrap().history_depth();
    let count0 = ed.active().unwrap().document.layers.len();

    let said = run_through_the_window(&mut ed, FILL_SCRIPT);
    assert_eq!(said.len(), 1);
    assert!(said[0].is_ok(), "{said:?}");

    let open = ed.active().unwrap();
    assert_eq!(
        open.document.layers.len(),
        count0 + 1,
        "one layer was added"
    );
    let id = layer_named(&ed, "Scripted").expect("the new layer was renamed");
    let open = ed.active().unwrap();
    assert_eq!(
        open.document.layers.root()[0],
        id,
        "artLayers.add() put the layer above the active one, on top"
    );
    assert!(matches!(
        open.document.layers.get(id).unwrap().kind,
        LayerKind::Raster(_)
    ));
    assert_eq!(
        open.document.selection,
        Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(10, 8)
        }
    );
    // The fill landed inside the selection on the new layer, and nowhere else.
    let px = open.layer_pixels(id).unwrap();
    let at = |x: usize, y: usize| &px[(y * 16 + x) * 4..(y * 16 + x) * 4 + 4];
    assert_eq!(at(5, 5), &[255, 0, 0, 255], "inside the selection");
    assert_eq!(at(0, 0)[3], 0, "outside the selection");
    assert_eq!(at(12, 12)[3], 0, "outside the selection");
    // One step, named for the script.
    assert_eq!(
        open.history_depth(),
        depth0 + 1,
        "the run is one history step"
    );
    assert_eq!(open.history.undo_label(), Some(SCRIPT_LABEL));
    // The alert reached the window's log.
    let log = super::with_window(|w| w.log().to_vec()).unwrap();
    assert!(
        log.iter()
            .any(|l| l.kind == ScriptLogKind::Alert && l.text == "2 layers, top is Scripted"),
        "{log:?}"
    );

    // One Undo reverts all of it.
    ed.dispatch(Action::Undo).unwrap();
    let open = ed.active().unwrap();
    assert_eq!(open.document.layers.len(), count0);
    assert!(layer_named(&ed, "Scripted").is_none());
    let open = ed.active().unwrap();
    assert_eq!(open.document.selection, Selection::None);
    assert_eq!(open.history_depth(), depth0);
    // ...and Redo brings all of it back.
    ed.dispatch(Action::Redo).unwrap();
    assert!(layer_named(&ed, "Scripted").is_some());
}

/// An endless loop is stopped by the step budget, with a sentence, and what
/// the script did before the loop is kept as one step.
#[test]
fn an_endless_loop_is_stopped_by_the_step_budget() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let depth0 = ed.active().unwrap().history_depth();
    let limits = ScriptLimits {
        budget: 2_000_000,
        // Off, so the budget is what stops it.
        loop_iterations: u64::MAX,
        recursion: 512,
        wall: std::time::Duration::from_secs(10),
    };
    let started = std::time::Instant::now();
    let report = run(
        &mut ed,
        "app.activeDocument.artLayers.add().name = 'Before';\nvar i = 0;\nwhile (true) { i++; }",
        limits,
    );
    match &report.end {
        ScriptEnd::Stopped(why) => assert!(why.contains("step budget"), "{why}"),
        other => panic!("the loop was not stopped by the budget: {other:?}"),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "stopped well inside the wall-clock limit"
    );
    assert!(
        layer_named(&ed, "Before").is_some(),
        "the edit before the loop stays"
    );
    assert_eq!(ed.active().unwrap().history_depth(), depth0 + 1);
    assert!(report
        .lines
        .iter()
        .any(|l| l.kind == ScriptLogKind::Error && l.text.contains("step budget")));
}

/// A syntax error is reported, nothing runs, and nothing panics.
#[test]
fn a_syntax_error_is_reported_and_nothing_runs() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let depth0 = ed.active().unwrap().history_depth();
    let report = run(
        &mut ed,
        "app.activeDocument.artLayers.add();\nvar = ;",
        ScriptLimits::default(),
    );
    match &report.end {
        ScriptEnd::Syntax(why) => assert!(!why.is_empty()),
        other => panic!("expected a syntax error, got {other:?}"),
    }
    assert_eq!(ed.active().unwrap().history_depth(), depth0, "nothing ran");
    assert!(report.summary().starts_with("Script not run:"));
}

/// An uncaught exception is reported with its message; a caught DOM error
/// is the script's own business.
#[test]
fn an_uncaught_error_is_reported_and_a_caught_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let report = run(
        &mut ed,
        "try { app.activeDocument.layers.getByName('nope'); } catch (e) { alert('caught'); }\n\
         throw new Error('boom');",
        ScriptLimits::default(),
    );
    match &report.end {
        ScriptEnd::Failed(why) => assert!(why.contains("boom"), "{why}"),
        other => panic!("expected the throw, got {other:?}"),
    }
    assert!(report
        .lines
        .iter()
        .any(|l| l.kind == ScriptLogKind::Alert && l.text == "caught"));
}

/// The DOM reads what the editor holds: sizes, names, colours, blend modes,
/// transforms, text layers.
#[test]
fn the_dom_reads_and_writes_the_live_document() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let depth0 = ed.active().unwrap().history_depth();
    let report = run(
        &mut ed,
        r#"
var doc = app.activeDocument;
console.log(doc.width, doc.height, doc.name);
doc.resizeCanvas(20, 18, AnchorPosition.TOPLEFT);
console.log(doc.width, doc.height);
var fg = new SolidColor(); fg.rgb.red = 0; fg.rgb.green = 128; fg.rgb.blue = 255;
app.foregroundColor = fg;
console.log(app.foregroundColor.rgb.hexValue);
var l = doc.artLayers.add();
l.opacity = 40;
l.blendMode = BlendMode.MULTIPLY;
l.visible = false;
console.log(l.opacity, l.blendMode, l.visible);
l.kind = LayerKind.TEXT;
l.textItem.contents = "Hello";
l.textItem.size = 24;
l.textItem.position = [3, 4];
console.log(l.kind, l.textItem.contents, l.textItem.size, l.textItem.position.join(","));
doc.activeLayer.translate(2, 1);
console.log(doc.activeLayer.textItem.position.join(","));
var g = doc.layerSets.add();
console.log(g.typename, doc.layerSets.length);
doc.activeLayer = doc.layers[doc.layers.length - 1];
var mid = doc.artLayers.add();
mid.name = "Mid";
console.log(doc.layers[doc.layers.length - 2].name, doc.layers[doc.layers.length - 1].name);
var inside = g.artLayers.add();
console.log(g.layers.length, g.layers[0].name == inside.name, inside.parent.typename);
"#,
        ScriptLimits::default(),
    );
    assert_eq!(report.end, ScriptEnd::Completed, "{:?}", report.lines);
    let out: Vec<&str> = report
        .lines
        .iter()
        .filter(|l| l.kind == ScriptLogKind::Output)
        .map(|l| l.text.as_str())
        .collect();
    assert_eq!(
        out,
        vec![
            "16 16 Doc",
            "20 18",
            "0080FF",
            "40 multiply false",
            "text Hello 24 3,4",
            "5,5",
            "LayerSet 1",
            "Mid Layer 1",
            "1 true LayerSet",
        ]
    );
    let open = ed.active().unwrap();
    assert_eq!((open.document.width(), open.document.height()), (20, 18));
    assert_eq!(
        open.history_depth(),
        depth0 + 1,
        "one step for the whole run"
    );
    assert_eq!(ed.foreground()[2], 1.0);
}

/// `.jsx` through File ▸ Open's routing table opens the window with the
/// source and runs NOTHING until Run is pressed.
#[test]
fn opening_a_jsx_shows_it_in_the_window_and_does_not_run_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("make.jsx");
    std::fs::write(&path, FILL_SCRIPT).unwrap();
    let mut ed = editor_with_doc(dir.path());
    let depth0 = ed.active().unwrap().history_depth();
    assert!(Editor::is_library_file(&path));
    let opened = ed.open_any(&path).unwrap();
    assert_eq!(opened, None, "a script is not a document");
    assert_eq!(ed.documents().len(), 1);
    assert_eq!(ed.active().unwrap().history_depth(), depth0, "nothing ran");
    assert_eq!(
        super::with_window(|w| w.source().to_string()).as_deref(),
        Some(FILL_SCRIPT)
    );
    // The picker offers scripts.
    assert!(crate::dialogs::open_file_filters()[0].1.contains(&"jsx"));
}

/// A saved package's journal gets the run as ONE transaction, not its parts
/// and then the transaction again, so a crash recovery replays it once.
#[test]
fn a_saved_documents_journal_records_the_run_once() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path());
    let package = dir.path().join("doc.rstudio");
    ed.active_mut().unwrap().save_to(&package, "test").unwrap();
    let journal = package.join(project_format::JOURNAL_FILE);
    let before = project_format::CommandJournal::read_all(&journal)
        .map(|c| c.len())
        .unwrap_or(0);
    let report = run(&mut ed, FILL_SCRIPT, ScriptLimits::default());
    assert_eq!(report.end, ScriptEnd::Completed, "{:?}", report.lines);
    let after = project_format::CommandJournal::read_all(&journal).unwrap();
    let new: Vec<&Command> = after.iter().skip(before).collect();
    assert_eq!(new.len(), 1, "one record: {new:?}");
    assert!(
        matches!(new[0], Command::Transaction { label, .. } if label == SCRIPT_LABEL),
        "{:?}",
        new[0]
    );
}

/// W13X-6: what each Open and Export picker was opened with, shared with the
/// test after the double is boxed into the editor. The answers are
/// [`ScriptedDialogs`]'s (every queue empty, so each picker cancels).
#[derive(Default)]
struct Seen {
    exports: Vec<crate::dialogs::ExportPickerRequest>,
    opens: Vec<Option<String>>,
}

struct SeeingDialogs {
    inner: ScriptedDialogs,
    seen: std::rc::Rc<std::cell::RefCell<Seen>>,
}

impl crate::dialogs::FileDialogs for SeeingDialogs {
    fn pick_open_file(&mut self) -> Option<std::path::PathBuf> {
        let answer = self.inner.pick_open_file();
        let suggestion = self.inner.open_suggestions.last().cloned().flatten();
        self.seen.borrow_mut().opens.push(suggestion);
        answer
    }
    fn pick_place_file(&mut self) -> Option<std::path::PathBuf> {
        self.inner.pick_place_file()
    }
    fn pick_replace_file(&mut self) -> Option<std::path::PathBuf> {
        self.inner.pick_replace_file()
    }
    fn pick_open_project(&mut self) -> Option<std::path::PathBuf> {
        self.inner.pick_open_project()
    }
    fn pick_save_path(&mut self, suggested: &Path) -> Option<std::path::PathBuf> {
        self.inner.pick_save_path(suggested)
    }
    fn pick_export_path(&mut self, suggested: &Path) -> Option<std::path::PathBuf> {
        let answer = self.inner.pick_export_path(suggested);
        let request = self.inner.export_requests.last().cloned();
        self.seen.borrow_mut().exports.extend(request);
        answer
    }
    fn pick_export_folder(&mut self) -> Option<std::path::PathBuf> {
        self.inner.pick_export_folder()
    }
    fn confirm_close(&mut self, document: &str) -> crate::dialogs::CloseChoice {
        self.inner.confirm_close(document)
    }
    fn confirm_recover(&mut self, document: &str) -> bool {
        self.inner.confirm_recover(document)
    }
    fn report_error(&mut self, title: &str, message: &str) {
        self.inner.report_error(title, message)
    }
    fn report_notice(&mut self, title: &str, message: &str) {
        self.inner.report_notice(title, message)
    }
}

/// W13X-6: `saveAs(file)` and `app.open(file)` open their platform picker
/// with the script's file name in the file-name box (the export picker in
/// the document's own folder; only the name's last component is used), run
/// from the Script window. A suggestion reaches exactly one picker: the
/// user's next File > Open starts empty.
#[test]
fn a_scripts_file_names_are_suggested_to_the_open_and_export_pickers() {
    let dir = tempfile::tempdir().unwrap();
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Seen::default()));
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.path().join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(SeeingDialogs {
            inner: ScriptedDialogs::new(),
            seen: seen.clone(),
        }),
    );
    ed.new_document_with(
        16,
        16,
        "Doc",
        crate::import::BlankBackground::Solid {
            rgba8: [255, 255, 255, 255],
            depth: raster::BitDepth::Eight,
        },
    )
    .unwrap();
    let export_at = ed.active().unwrap().suggested_export_path();

    let said = run_through_the_window(
        &mut ed,
        r#"
var doc = app.activeDocument;
doc.saveAs(new File("C:/somewhere/else/poster.jpg"));
doc.saveAs("banner");
app.open(new File("/elsewhere/photo.png"));
"#,
    );
    assert!(said.iter().all(Result::is_ok), "{said:?}");

    let record = seen.borrow();
    let names: Vec<_> = record.exports.iter().map(|r| r.suggested.clone()).collect();
    assert_eq!(
        names,
        vec![
            export_at.with_file_name("poster.jpg"),
            export_at
                .with_file_name("banner")
                .with_extension(export_at.extension().unwrap()),
        ],
        "the export picker did not open with the script's names"
    );
    assert_eq!(record.opens, vec![Some("photo.png".to_string())]);
    drop(record);

    // The user's own File > Open and Export afterwards are not prefilled.
    let _ = ed.dispatch(Action::Open);
    let _ = ed.dispatch(Action::Export);
    let record = seen.borrow();
    assert_eq!(
        record.opens.last(),
        Some(&None),
        "Open kept a script's name"
    );
    assert_eq!(
        record.exports.last().map(|r| r.suggested.clone()),
        Some(export_at),
        "Export kept a script's name"
    );
}
