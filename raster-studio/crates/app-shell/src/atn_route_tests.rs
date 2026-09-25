//! W13-E: `.atn` through the application — File > Open of an action set,
//! the Actions panel's published view, Play through the real chrome frame,
//! set editing, and Export read back by the parser.

use std::path::{Path, PathBuf};

use asset_store::resources::atn::{self, AtnAction, AtnSet, AtnStep, FillWith, Length, StepOp};
use ui::panels::actions::{self as panel, ActionSetsView, ActionsRequest, ActionsView};

use crate::action::Action;
use crate::chrome::{install_theme, Chrome};
use crate::dialogs::ScriptedDialogs;
use crate::editor::{Editor, Effect};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn png(dir: &Path) -> PathBuf {
    let rgba = vec![200u8; 16 * 16 * 4];
    let path = dir.join("doc.png");
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, 16, 16, &rgba).unwrap(),
    )
    .unwrap();
    path
}

/// Draw `n` frames of the real chrome (which runs the Actions panel's
/// per-frame sync).
fn frames(chrome: &mut Chrome, ctx: &egui::Context, ed: &mut Editor, n: usize) {
    for _ in 0..n {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                ..Default::default()
            },
            |ctx| {
                let _ = chrome.ui(ctx, ed);
            },
        );
    }
}

/// "Web / Prep": Image Size 50%, Make layer, a rectangle selection, Fill
/// red, Plastic Wrap (nothing here performs it), and an *unchecked* Canvas
/// Size 200%.
fn web_set() -> AtnSet {
    let mut unchecked = StepOp::CanvasSize {
        width: Some(Length::Percent(200.0)),
        height: Some(Length::Percent(200.0)),
        relative: false,
        horizontal: 1,
        vertical: 1,
    }
    .to_step();
    unchecked.enabled = false;
    AtnSet {
        name: "Web".into(),
        expanded: true,
        actions: vec![AtnAction {
            name: "Prep".into(),
            steps: vec![
                StepOp::ImageSize {
                    width: Some(Length::Percent(50.0)),
                    height: None,
                }
                .to_step(),
                StepOp::MakeLayer.to_step(),
                StepOp::SelectRect {
                    left: 1.0,
                    top: 2.0,
                    right: 5.0,
                    bottom: 6.0,
                }
                .to_step(),
                StepOp::Fill {
                    with: FillWith::Rgb([1.0, 0.0, 0.0]),
                    opacity: 1.0,
                }
                .to_step(),
                AtnStep::new("PlsW", "Plastic Wrap", None),
                unchecked,
            ],
            ..AtnAction::default()
        }],
    }
}

fn atn_file(dir: &Path) -> PathBuf {
    let path = dir.join("web.atn");
    std::fs::write(&path, atn::write(&web_set()).unwrap()).unwrap();
    path
}

/// File > Open of an `.atn` adds its set; the panel lists the action under
/// the set with the unmapped step marked skipped; Play (queued exactly as
/// the panel's Play button queues it, taken by the real chrome frame)
/// changes the document through the editor's routes, passes over the
/// unchecked step, and names the skipped one on the status line.
#[test]
fn an_opened_atn_plays_its_mapped_steps_on_the_document_and_reports_the_unmapped_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().opening(atn_file(dir.path())),
    );
    ed.open_path(&png(dir.path())).unwrap();
    assert_eq!(ed.dispatch(Action::Open), Ok(Effect::Panels));
    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.contains("“Web”") && status.contains("1 step(s) have no equivalent"),
        "{status}"
    );

    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frames(&mut chrome, &ctx, &mut ed, 2);
    let view = ActionsView::published(&ctx);
    let index = view
        .actions
        .iter()
        .position(|a| a.name == "Web / Prep")
        .expect("the imported action is listed under its set");
    let steps = &view.actions[index].steps;
    assert_eq!(steps.len(), 6, "{steps:?}");
    assert!(
        steps[4].starts_with("Plastic Wrap (skipped on play:"),
        "{steps:?}"
    );
    assert!(steps[5].ends_with("(unchecked)"), "{steps:?}");
    let sets = ActionSetsView::published(&ctx);
    let web = sets
        .sets
        .iter()
        .find(|s| s.name == "Web")
        .expect("set listed");
    assert!(web.actions[0].steps[4].skipped.is_some());
    assert!(!web.actions[0].steps[5].enabled);

    let layers_before = ed.active().unwrap().document.layers.len();
    panel::request(&ctx, ActionsRequest::Play(index));
    frames(&mut chrome, &ctx, &mut ed, 1);

    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.starts_with("Played 4 step(s), 1 unchecked"),
        "{status}"
    );
    assert!(
        status.contains("skipped 1") && status.contains("Plastic Wrap"),
        "{status}"
    );
    let doc = ed.active_mut().unwrap();
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (8, 8),
        "Image Size 50% ran; the unchecked Canvas Size 200% did not"
    );
    assert_eq!(
        doc.document.layers.len(),
        layers_before + 1,
        "Make layer ran"
    );
    assert_eq!(
        doc.document.selection,
        editor_core::Selection::Rect {
            min: glam::IVec2::new(1, 2),
            max: glam::IVec2::new(5, 6),
        }
    );
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    let px = |x: usize, y: usize| &rgba[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4];
    assert_eq!(
        px(2, 3),
        [255, 0, 0, 255],
        "the fill landed inside the rectangle"
    );
    assert_ne!(px(6, 6), [255, 0, 0, 255], "and only there");
    // Each played step is its own undo step, like doing it by hand.
    assert!(doc.history.undo_depth() >= 4);
}

// ---- Round 2: the set tree driven through the drawn Actions dock --------

/// A tall screen, so the whole Actions dock (flat list, set tree and its
/// footer) lies inside the window and every click lands on its control.
fn screen(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 2400.0),
        )),
        events,
        ..Default::default()
    }
}

fn frame(chrome: &mut Chrome, ctx: &egui::Context, ed: &mut Editor, events: Vec<egui::Event>) {
    let _ = ctx.run(screen(events), |ctx| {
        let _ = chrome.ui(ctx, ed);
    });
}

/// Where `id` was drawn on the last frame; panics when it was not drawn.
fn drawn(ctx: &egui::Context, id: egui::Id) -> egui::Rect {
    ctx.read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was not drawn in the Actions dock"))
        .rect
}

/// Click the control drawn under `id`: move there, press, release, as one
/// frame of real pointer events; then one more frame, in which the chrome's
/// Actions sync takes the request the click queued.
fn click(chrome: &mut Chrome, ctx: &egui::Context, ed: &mut Editor, id: egui::Id) {
    // Two settling frames: the chrome publishes the library after the docks
    // are drawn, so a change made off-panel (a recording) reaches the drawn
    // tree one frame later, and the click must aim at where it is now.
    frame(chrome, ctx, ed, Vec::new());
    frame(chrome, ctx, ed, Vec::new());
    let at = drawn(ctx, id).center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    frame(
        chrome,
        ctx,
        ed,
        vec![egui::Event::PointerMoved(at), button(true), button(false)],
    );
    frame(chrome, ctx, ed, Vec::new());
}

fn key(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

/// The real chrome with the Actions panel opened the way a user opens it:
/// Window > Actions, routed through the menu bridge and the chrome.
fn chrome_with_actions_open(ed: &mut Editor) -> (Chrome, egui::Context) {
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    frame(&mut chrome, &ctx, ed, Vec::new());
    let panel = ui::PanelId::Actions;
    assert!(!chrome.workspace().dock.is_open(panel));
    let menu_ctx = crate::menu_bridge::context(ed, chrome.workspace());
    let intent =
        crate::menu_bridge::resolve_intent(ui::menu::MenuAction::TogglePanel(panel), &menu_ctx, ed)
            .unwrap_or_else(|reason| panic!("Window > Actions is disabled: {reason}"));
    let mut out = crate::chrome::ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    // What the shell does with the chrome's output: absorb each workspace
    // intent the menu produced into the chrome's workspace.
    assert!(!out.workspace.is_empty(), "{out:?}");
    for intent in &out.workspace {
        chrome.workspace_for_test().absorb(intent);
    }
    frame(&mut chrome, &ctx, ed, Vec::new());
    frame(&mut chrome, &ctx, ed, Vec::new());
    assert!(chrome.workspace().dock.is_open(panel));
    // It opens as a tab of its group; clicking its tab brings it forward.
    if !chrome.workspace().dock.is_active(panel) {
        click(&mut chrome, &ctx, ed, ui::view::ids::panel_tab(panel));
    }
    assert!(chrome.workspace().dock.is_active(panel));
    (chrome, ctx)
}

/// Every set-tree control is a drawn widget a click reaches: Load .atn adds
/// the set (its unmapped step drawn as skipped), a step's check box unchecks
/// it, a step row + Play from step plays from there, New set / Record into /
/// Rename (typed into the drawn field) / Delete set change the library.
#[test]
fn sets_are_made_renamed_and_deleted_and_an_action_plays_from_a_step() {
    use panel::ids;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().opening(atn_file(dir.path())),
    );
    ed.open_path(&png(dir.path())).unwrap();
    let (mut chrome, ctx) = chrome_with_actions_open(&mut ed);

    // Load .atn: the dock's button, the picker answers web.atn.
    assert!(ed.action_sets().iter().all(|s| s != "Web"));
    click(&mut chrome, &ctx, &mut ed, ids::import_atn());
    assert!(
        ed.action_sets().iter().any(|s| s == "Web"),
        "{:?}",
        ed.status()
    );
    let index = ed.actions().iter().position(|a| a.set == "Web").unwrap();
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    // The tree draws the set row, and a check box per step (the sixth,
    // unchecked in the file, included).
    let web = ActionSetsView::published(&ctx)
        .sets
        .iter()
        .position(|s| s.name == "Web")
        .unwrap();
    drawn(&ctx, ids::set_row(web));
    for step in 0..6 {
        drawn(&ctx, ids::step_toggle(index, step));
    }

    // Uncheck step 3 (Fill) with its drawn check box.
    click(&mut chrome, &ctx, &mut ed, ids::step_toggle(index, 3));
    let sets = ActionSetsView::published(&ctx);
    let steps = &sets.sets[web].actions[0].steps;
    assert!(!steps[3].enabled, "{steps:?}");

    // Select step 1 (Make layer) and press Play from step: Image Size
    // (step 0) does not run.
    click(&mut chrome, &ctx, &mut ed, ids::step_row(index, 1));
    let layers_before = ed.active().unwrap().document.layers.len();
    click(&mut chrome, &ctx, &mut ed, ids::play_from_step());
    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.starts_with("Played 2 step(s), 2 unchecked"),
        "{status}"
    );
    let doc = ed.active().unwrap();
    assert_eq!(
        doc.document.width(),
        16,
        "Image Size was before the start step"
    );
    assert_eq!(
        doc.document.layers.len(),
        layers_before + 1,
        "Make layer ran"
    );

    // New set: recordings join it.
    click(&mut chrome, &ctx, &mut ed, ids::new_set());
    let names = ed.action_sets();
    assert_eq!(names.last().map(String::as_str), Some("Set"), "{names:?}");
    ed.start_recording();
    ed.apply_command(editor_core::Command::SetSelection {
        selection: editor_core::Selection::None,
    });
    ed.stop_recording();
    assert_eq!(ed.actions().last().unwrap().set, "Set");
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    let view = ActionSetsView::published(&ctx);
    let mine = view.sets.iter().position(|s| s.name == "Set").unwrap();
    assert!(view.sets[mine].recording_target);

    // Record into (on the Web set) moves the target back.
    click(&mut chrome, &ctx, &mut ed, ids::set_row(web));
    click(&mut chrome, &ctx, &mut ed, ids::record_into());
    assert!(ActionSetsView::published(&ctx).sets[web].recording_target);

    // Rename: select the new set, press Rename, type into the drawn field.
    click(&mut chrome, &ctx, &mut ed, ids::set_row(mine));
    click(&mut chrome, &ctx, &mut ed, ids::rename_set());
    drawn(&ctx, ids::rename_field());
    frame(
        &mut chrome,
        &ctx,
        &mut ed,
        vec![egui::Event::Text("Ours".into()), key(egui::Key::Enter)],
    );
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    assert_eq!(
        ed.actions().last().unwrap().set,
        "Ours",
        "{:?}",
        ed.status()
    );
    assert!(ed.action_sets().iter().any(|s| s == "Ours"));

    // Delete set (the Web set, with its action).
    let web = ed.action_sets().iter().position(|s| s == "Web").unwrap();
    click(&mut chrome, &ctx, &mut ed, ids::set_row(web));
    click(&mut chrome, &ctx, &mut ed, ids::delete_set());
    assert!(ed.action_sets().iter().all(|s| s != "Web"));
    assert!(ed.actions().iter().all(|a| a.set != "Web"));

    // The set names survive a restart through the presets file.
    let other = editor(dir.path(), ScriptedDialogs::new());
    assert!(other.presets().action_sets().iter().any(|s| s == "Ours"));
}

/// Export set as .atn, pressed in the dock, writes a set the parser reads
/// back: the imported steps as they were (the unchecked one still
/// unchecked), and a recorded rectangle selection as Photoshop's Set
/// Selection step.
#[test]
fn an_exported_set_reads_back_with_its_steps() {
    use panel::ids;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("back.atn");
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new()
            .opening(atn_file(dir.path()))
            .exporting_to(&out),
    );
    ed.open_path(&png(dir.path())).unwrap();
    let (mut chrome, ctx) = chrome_with_actions_open(&mut ed);
    click(&mut chrome, &ctx, &mut ed, ids::import_atn());
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    let web = ActionSetsView::published(&ctx)
        .sets
        .iter()
        .position(|s| s.name == "Web")
        .expect("Load .atn added the set");

    // A recording joins the Web set (Record into), then the set is exported.
    click(&mut chrome, &ctx, &mut ed, ids::set_row(web));
    click(&mut chrome, &ctx, &mut ed, ids::record_into());
    ed.start_recording();
    ed.apply_command(editor_core::Command::SetSelection {
        selection: editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(3, 4),
        },
    });
    ed.stop_recording();
    click(&mut chrome, &ctx, &mut ed, ids::export_set());
    assert!(
        ed.status()
            .unwrap_or_default()
            .starts_with("Exported “Web”"),
        "{:?}",
        ed.status()
    );

    let back = atn::parse(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(back.name, "Web");
    assert_eq!(back.actions.len(), 2);
    assert_eq!(back.actions[0].steps, web_set().actions[0].steps);
    assert_eq!(
        atn::interpret(&back.actions[1].steps[0]),
        Ok(StepOp::SelectRect {
            left: 0.0,
            top: 0.0,
            right: 3.0,
            bottom: 4.0
        })
    );
}
