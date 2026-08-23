//! Tests for [`crate::editor`]. A child module of it, so it can reach the
//! private fields the fixtures have to set up.

use super::*;
use crate::dialogs::ScriptedDialogs;
use crate::keymap::{Chord, Key};
use crate::recent::RecentFiles;
use crate::ToolKey;

fn write_png(dir: &Path, name: &str, w: u32, h: u32, value: u8) -> PathBuf {
    let rgba = vec![value; (w as usize) * (h as usize) * 4];
    let bytes = raster::encode(raster::ExportFormat::Png, w, h, &rgba).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn bare(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// An editor in a state where **every** action has something to do: two
/// documents open, the first one saved-then-edited with both an undo and a redo
/// available, a non-default colour, and dialogs primed to answer.
fn prepared(dir: &Path) -> Editor {
    let a = write_png(dir, "a.png", 64, 48, 90);
    let b = write_png(dir, "b.png", 32, 32, 200);
    let c = write_png(dir, "c.png", 16, 16, 10);

    let dialogs = ScriptedDialogs::new()
        .opening(c)
        // File ▸ Open Project… goes through the *folder* picker: a package is
        // a directory, so a file picker could never answer for one.
        .opening_project(dir.join("a.rstudio"))
        .saving_to(dir.join("saved-as.rstudio"))
        .exporting_to(dir.join("exported.png"))
        .answering_close(CloseChoice::Discard)
        .answering_close(CloseChoice::Discard)
        .answering_close(CloseChoice::Discard);
    let mut ed = bare(dir, dialogs);

    ed.open_path(&a).unwrap();
    ed.open_path(&b).unwrap();
    ed.activate(0).unwrap();

    // Give the active document a project on disk, then two edits and an undo,
    // so Save, Undo and Redo all have work waiting.
    let index = ed.active_index().unwrap();
    ed.docs[index]
        .save_to(&dir.join("a.rstudio"), "test")
        .unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::Undo).unwrap();
    assert!(ed.active().unwrap().history.can_undo());
    assert!(ed.active().unwrap().history.can_redo());
    assert!(ed.active().unwrap().is_dirty());

    // The undo above removed the layer the cursor named, and `Document`
    // deliberately keeps the cursor pointing at it so a redo restores the
    // selection too — which reads as "no active layer" until then. Point it at
    // a live layer so the layer actions have a target.
    let doc = &mut ed.active_mut().unwrap().document;
    let top = doc.layers.root()[0];
    doc.set_active_layer(Some(top)).unwrap();
    assert!(!doc.layers.get(top).unwrap().is_group());

    ed.set_foreground([1.0, 0.0, 0.0, 1.0]);
    ed.active_mut().unwrap().camera.zoom = 3.0;
    ed
}

/// The effect each action produces. Exhaustive with no wildcard: a new action
/// must state what it does before this compiles.
fn expected_effect(action: Action) -> Effect {
    match action {
        Action::NewDocument
        | Action::Open
        | Action::OpenProject
        | Action::CloseDocument
        | Action::NextDocument
        | Action::PreviousDocument => Effect::DocumentSet,
        Action::Save | Action::SaveAs => Effect::Saved,
        Action::Export => Effect::Exported,
        Action::ShowPreferences => Effect::Preferences,
        Action::Quit => Effect::Quit,
        Action::Undo
        | Action::Redo
        | Action::NewLayer
        | Action::DeleteLayer
        | Action::DuplicateLayer
        | Action::ToggleLayerVisibility => Effect::DocumentEdited,
        Action::ZoomIn | Action::ZoomOut | Action::ZoomFit | Action::ZoomActualPixels => {
            Effect::View
        }
        Action::TogglePanels => Effect::Panels,
        Action::SelectTool(_)
        | Action::TemporaryHand
        | Action::DecreaseBrushSize
        | Action::IncreaseBrushSize => Effect::Tool,
        Action::SwapColors | Action::ResetColors => Effect::Color,
    }
}

#[test]
fn every_action_does_something() {
    // The Wave-0 shell handled four zoom actions and logged the other seven as
    // "not wired yet". This is the test that stops that coming back: it runs
    // *every* action in a state where it applies and demands both the right
    // kind of effect and an observable state change.
    for action in Action::all() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = prepared(dir.path());

        if let Action::SelectTool(key) = action {
            // Pressing a tool letter while a tool in that group is already
            // active cycles within the group; for a one-tool group that is
            // correctly a no-op. Start outside the group so the press has
            // somewhere to go.
            let group = registry::by_shortcut(key.char());
            let outside = *ToolId::ALL
                .iter()
                .find(|t| !group.contains(t))
                .expect("no single group holds every tool");
            ed.set_tool(outside);
        }

        let before = ed.revision();
        let effect = ed
            .dispatch(action)
            .unwrap_or_else(|e| panic!("{}: {e}", action.id()));
        assert_eq!(
            effect,
            expected_effect(action),
            "{} produced the wrong kind of effect",
            action.id()
        );
        assert!(
            ed.revision() > before,
            "{} returned Ok without changing anything",
            action.id()
        );
    }
}

#[test]
fn every_action_is_reachable_from_the_keyboard() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = prepared(dir.path());
    let bound: Vec<Action> = ed
        .keymap()
        .bindings()
        .into_iter()
        .map(|b| b.action)
        .collect();
    for action in Action::all() {
        assert!(
            bound.contains(&action),
            "{} has no key binding",
            action.id()
        );
    }
    // ...and a bound chord actually performs it.
    let effect = ed
        .handle_chord(&Chord::plain(Key::Tab))
        .unwrap()
        .expect("Tab is bound");
    assert_eq!(effect, Effect::Panels);
    assert!(!ed.panels_visible());
    // An unbound chord is not an error.
    assert_eq!(ed.handle_chord(&Chord::ctrl(Key::Function(9))), Ok(None));
}

#[test]
fn with_nothing_open_the_menu_says_why_each_item_is_greyed_out() {
    let dir = tempfile::tempdir().unwrap();
    let ed = bare(dir.path(), ScriptedDialogs::new());
    let menus = ed.menus();
    assert!(!menus.is_empty());

    let mut saw_disabled = false;
    for menu in &menus {
        for item in &menu.items {
            assert!(!item.label.is_empty());
            assert_eq!(
                item.enabled,
                item.disabled_reason.is_none(),
                "{} must carry a reason exactly when it is disabled",
                item.action.id()
            );
            if let Some(reason) = &item.disabled_reason {
                assert!(
                    !reason.is_empty(),
                    "{} greys out silently",
                    item.action.id()
                );
                saw_disabled = true;
            }
        }
    }
    assert!(
        saw_disabled,
        "with no document open, some items must be off"
    );

    for action in [Action::Save, Action::Export, Action::Undo, Action::NewLayer] {
        let err = ed.can(action).unwrap_err();
        assert!(err.to_string().contains("no document"), "{action:?}: {err}");
        assert!(matches!(err, ActionError::Unavailable { .. }));
    }
}

#[test]
fn a_disabled_action_refuses_rather_than_pretending() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    let before = ed.revision();
    let err = ed.dispatch(Action::Undo).unwrap_err();
    assert_eq!(err.action(), Action::Undo);
    assert_eq!(ed.revision(), before, "a refusal changes nothing");
}

#[test]
fn opening_files_fills_tabs_and_the_recent_list() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 40, 30, 10);
    let b = write_png(dir.path(), "b.png", 20, 20, 250);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());

    let ids = ed.open_paths(&[a.clone(), b.clone()]);
    assert_eq!(ids.len(), 2);
    assert_eq!(ed.documents().len(), 2);
    assert_eq!(ed.active_index(), Some(1), "the newest opens active");
    assert_eq!(ed.active().unwrap().title(), "b.png");
    assert_eq!(ed.recent().entries(), [b.clone(), a.clone()]);

    // The image really is the document, not a texture beside it.
    let doc = ed.active().unwrap();
    assert_eq!(doc.document.layers.len(), 1);
    assert_eq!((doc.document.width(), doc.document.height()), (20, 20));
    assert!(doc.document.active_layer().is_some());

    // Re-opening moves it to the front instead of duplicating it.
    ed.open_path(&a).unwrap();
    assert_eq!(ed.recent().entries(), [a, b]);
    assert_eq!(ed.recent().len(), 2);
}

#[test]
fn a_file_that_cannot_be_opened_is_reported_and_forgotten() {
    let dir = tempfile::tempdir().unwrap();
    let junk = dir.path().join("broken.png");
    std::fs::write(&junk, b"this is not a png").unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.recent.record(&junk);

    let opened = ed.open_paths(std::slice::from_ref(&junk));
    assert!(opened.is_empty());
    assert!(ed.documents().is_empty());
    assert!(
        !ed.recent().entries().contains(&junk),
        "a path that will not open must not stay on the menu"
    );
}

#[test]
fn the_window_title_follows_the_document_and_its_dirty_state() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    assert_eq!(ed.window_title(), "Raster Studio");

    let a = write_png(dir.path(), "holiday.png", 8, 8, 5);
    ed.open_path(&a).unwrap();
    assert_eq!(ed.window_title(), "holiday.png — Raster Studio");
    assert!(!ed.has_unsaved_work());

    ed.dispatch(Action::NewLayer).unwrap();
    assert_eq!(ed.window_title(), "• holiday.png — Raster Studio");
    assert!(ed.has_unsaved_work());

    ed.active_mut()
        .unwrap()
        .save_to(&dir.path().join("h.rstudio"), "test")
        .unwrap();
    assert_eq!(ed.window_title(), "holiday.png — Raster Studio");
    assert!(!ed.has_unsaved_work());
}

#[test]
fn closing_a_dirty_document_asks_first_and_cancel_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 5);

    // Cancel: the document stays.
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    let err = ed.dispatch(Action::CloseDocument).unwrap_err();
    assert!(matches!(err, ActionError::Cancelled(Action::CloseDocument)));
    assert_eq!(ed.documents().len(), 1);

    // Discard: it goes, unsaved.
    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new().answering_close(CloseChoice::Discard),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::CloseDocument).unwrap();
    assert!(ed.documents().is_empty());
    assert_eq!(ed.active_index(), None);

    // Save: it is written where the dialog says, then closed.
    let target = dir.path().join("kept.rstudio");
    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new()
            .answering_close(CloseChoice::Save)
            .saving_to(target.clone()),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::CloseDocument).unwrap();
    assert!(ed.documents().is_empty());
    assert!(
        target.join(project_format::MANIFEST_FILE).is_file(),
        "it was saved"
    );
}

#[test]
fn a_clean_document_closes_without_a_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 5);
    // No answers queued: an empty queue cancels, so reaching the prompt at all
    // would fail this.
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::CloseDocument).unwrap();
    assert!(ed.documents().is_empty());
}

#[test]
fn quitting_with_unsaved_work_can_be_cancelled() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 5);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();

    assert!(ed.dispatch(Action::Quit).is_err());
    assert!(!ed.quit_requested(), "cancel must keep the app running");

    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new().answering_close(CloseChoice::Discard),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::Quit).unwrap();
    assert!(ed.quit_requested());
}

#[test]
fn save_writes_a_package_and_save_as_writes_a_second_one() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 24, 24, 33);
    let first = dir.path().join("first.rstudio");
    let second = dir.path().join("second.rstudio");
    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new()
            .saving_to(first.clone())
            .saving_to(second.clone()),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();

    // No path yet, so Save has to ask.
    ed.dispatch(Action::Save).unwrap();
    assert_eq!(ed.active().unwrap().project_path(), Some(first.as_path()));
    assert!(!ed.active().unwrap().is_dirty());
    // Now that it has one, Save must not ask again — and with nothing left to
    // save it reports why rather than writing.
    let err = ed.dispatch(Action::Save).unwrap_err();
    assert!(err.to_string().contains("no unsaved changes"), "{err}");

    ed.dispatch(Action::NewLayer).unwrap();
    ed.dispatch(Action::SaveAs).unwrap();
    assert_eq!(ed.active().unwrap().project_path(), Some(second.as_path()));
    assert!(first.join(project_format::MANIFEST_FILE).is_file());
    assert!(second.join(project_format::MANIFEST_FILE).is_file());
    assert!(ed.recent().entries().contains(&second));
}

#[test]
fn export_writes_the_composite_of_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 30, 20, 77);
    let out = dir.path().join("out.png");
    let mut ed = bare(dir.path(), ScriptedDialogs::new().exporting_to(out.clone()));
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::Export).unwrap();

    let decoded = raster::decode_path(&out).unwrap();
    assert_eq!((decoded.width, decoded.height), (30, 20));
    assert!(decoded.rgba8.iter().all(|&v| v.abs_diff(77) <= 1));
}

#[test]
fn duplicating_a_layer_copies_its_pixels_without_copying_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 300, 200, 44);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();

    let blobs_before = ed.active().unwrap().tiles.len();
    let source = ed.active().unwrap().document.active_layer().unwrap();
    let source_tiles = ed
        .active()
        .unwrap()
        .document
        .layer_tiles(source)
        .unwrap()
        .len();

    ed.dispatch(Action::DuplicateLayer).unwrap();
    {
        let doc = ed.active().unwrap();
        assert_eq!(doc.document.layers.len(), 2);
        let copy = doc.document.active_layer().unwrap();
        assert_ne!(copy, source);
        assert_eq!(doc.document.layers.get(copy).unwrap().name, "a.png copy");
        assert_eq!(
            doc.document.layer_tiles(copy).unwrap().len(),
            source_tiles,
            "the duplicate has the same pixels"
        );
        assert_eq!(
            doc.tiles.len(),
            blobs_before,
            "content addressing means no byte was copied"
        );
    }

    // One undo removes the whole duplicate, pixels included.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(ed.active().unwrap().document.layers.len(), 1);
}

#[test]
fn deleting_and_toggling_need_an_active_layer() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 5);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();

    ed.dispatch(Action::ToggleLayerVisibility).unwrap();
    {
        let doc = ed.active().unwrap();
        let id = doc.document.active_layer().unwrap();
        assert!(!doc.document.layers.get(id).unwrap().visible);
    }

    ed.dispatch(Action::DeleteLayer).unwrap();
    assert_eq!(ed.active().unwrap().document.layers.len(), 0);

    for action in [Action::DeleteLayer, Action::ToggleLayerVisibility] {
        let err = ed.can(action).unwrap_err();
        assert!(err.to_string().contains("select a layer"), "{err}");
    }
}

#[test]
fn the_bracket_keys_move_the_brush_and_stop_at_the_limits() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    let start = ed.brush().size;
    ed.dispatch(Action::IncreaseBrushSize).unwrap();
    assert!(ed.brush().size > start);
    ed.dispatch(Action::DecreaseBrushSize).unwrap();
    assert!(ed.brush().size < 30.0);

    // Down at the floor every press must refuse with a reason instead of
    // silently doing nothing.
    let mut brush = *ed.brush();
    brush.size = MIN_BRUSH_SIZE;
    ed.set_brush(brush);
    let err = ed.dispatch(Action::DecreaseBrushSize).unwrap_err();
    assert!(err.to_string().contains("smallest"), "{err}");

    // A small brush still steps by a whole pixel rather than rounding onto
    // itself.
    let mut brush = *ed.brush();
    brush.size = 3.0;
    ed.set_brush(brush);
    ed.dispatch(Action::DecreaseBrushSize).unwrap();
    assert_eq!(ed.brush().size, 2.0);
}

#[test]
fn the_colour_keys_swap_and_reset() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    assert_eq!(ed.foreground(), DEFAULT_FOREGROUND);
    ed.dispatch(Action::SwapColors).unwrap();
    assert_eq!(ed.foreground(), DEFAULT_BACKGROUND);
    assert_eq!(ed.background(), DEFAULT_FOREGROUND);
    ed.dispatch(Action::ResetColors).unwrap();
    assert_eq!(ed.foreground(), DEFAULT_FOREGROUND);
    assert_eq!(ed.background(), DEFAULT_BACKGROUND);
}

#[test]
fn space_borrows_the_hand_and_gives_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 5);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.set_tool(ToolId::Brush);

    ed.dispatch(Action::TemporaryHand).unwrap();
    assert_eq!(ed.effective_tool(), ToolId::Hand);
    // Key repeat must not toggle it back off.
    ed.dispatch(Action::TemporaryHand).unwrap();
    assert_eq!(ed.effective_tool(), ToolId::Hand);
    assert_eq!(ed.tool(), ToolId::Brush, "the real tool is untouched");

    ed.release_temporary_hand();
    assert_eq!(ed.effective_tool(), ToolId::Brush);
}

#[test]
fn a_tool_letter_cycles_within_its_group() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    let (key, group) = ToolKey::all()
        .into_iter()
        .map(|k| (k, registry::by_shortcut(k.char())))
        .find(|(_, g)| g.len() > 1)
        .expect("the registry has at least one cycle group");

    ed.set_tool(group[group.len() - 1]);
    ed.dispatch(Action::SelectTool(key)).unwrap();
    assert_eq!(ed.tool(), group[0], "the cycle wraps");
    ed.dispatch(Action::SelectTool(key)).unwrap();
    assert_eq!(ed.tool(), group[1]);
}

#[test]
fn tabs_step_forwards_and_backwards_and_wrap() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    let b = write_png(dir.path(), "b.png", 8, 8, 2);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_paths(&[a, b]);
    ed.activate(0).unwrap();

    ed.dispatch(Action::NextDocument).unwrap();
    assert_eq!(ed.active_index(), Some(1));
    ed.dispatch(Action::NextDocument).unwrap();
    assert_eq!(ed.active_index(), Some(0), "wraps");
    ed.dispatch(Action::PreviousDocument).unwrap();
    assert_eq!(ed.active_index(), Some(1));

    // One document: stepping is disabled and says so.
    ed.dispatch(Action::CloseDocument).unwrap();
    let err = ed.can(Action::NextDocument).unwrap_err();
    assert!(err.to_string().contains("only one document"), "{err}");
}

#[test]
fn autosave_arms_first_then_writes_only_dirty_documents() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 24, 24, 9);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();

    let t0 = Instant::now();
    assert!(ed.autosave_tick(t0).is_none(), "the first tick only arms");
    let due = ed.next_autosave().expect("armed");
    assert!(due > t0);

    assert!(ed.autosave_tick(t0).is_none(), "not due yet");
    assert!(ed.autosave_tick(due).is_none(), "due, but nothing is dirty");

    ed.dispatch(Action::NewLayer).unwrap();
    let due = ed.next_autosave().unwrap();
    let report = ed.autosave_tick(due).expect("a dirty document is written");
    assert_eq!(report.written.len(), 1);
    assert!(report.failed.is_empty());
    let (_, path) = &report.written[0];
    assert!(path.starts_with(ed.preferences().scratch_dir(ed.paths())));
    assert!(path.join(project_format::MANIFEST_FILE).is_file());
    assert!(
        ed.active().unwrap().is_dirty(),
        "an autosave into scratch is not the save the user asked for"
    );
    assert!(
        ed.active().unwrap().project_path().is_none(),
        "and it does not adopt the scratch path"
    );
}

#[test]
fn autosave_off_never_fires() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 9);
    let prefs = Preferences {
        autosave_interval_secs: 0,
        ..Preferences::default()
    };
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.path().join("config")),
        prefs,
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    let far_future = Instant::now() + Duration::from_secs(60 * 60 * 24);
    assert!(ed.autosave_tick(far_future).is_none());
    assert!(ed.next_autosave().is_none());
}

#[test]
fn autosave_of_a_saved_document_goes_back_to_its_own_package() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 24, 24, 9);
    let project = dir.path().join("a.rstudio");
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    ed.dispatch(Action::NewLayer).unwrap();

    let report = ed.autosave_now();
    assert_eq!(report.written.len(), 1);
    assert_eq!(report.written[0].1, project);
}

#[test]
fn a_crashed_session_is_offered_and_replayed() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 32, 32, 60);
    let project = dir.path().join("a.rstudio");

    // A previous run: save, then edit, then "crash" (drop without saving).
    {
        let mut ed = bare(dir.path(), ScriptedDialogs::new());
        ed.open_path(&a).unwrap();
        ed.active_mut().unwrap().save_to(&project, "test").unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        assert_eq!(ed.active().unwrap().document.layers.len(), 2);
    }

    let record = SessionRecord {
        pid: 1,
        open_projects: vec![project.clone()],
        autosaves: Vec::new(),
    };

    // Declining leaves the package as it was saved.
    let mut ed = bare(dir.path(), ScriptedDialogs::new().answering_recover(false));
    let report = ed.recover(&record);
    assert_eq!(report.declined, vec![project.clone()]);
    assert!(ed.documents().is_empty());

    // Accepting reopens it and replays the lost command.
    let mut ed = bare(dir.path(), ScriptedDialogs::new().answering_recover(true));
    let report = ed.recover(&record);
    assert_eq!(report.restored, vec![(project.clone(), 1)]);
    assert!(report.failed.is_empty());
    assert_eq!(ed.documents().len(), 1);
    assert_eq!(
        ed.active().unwrap().document.layers.len(),
        2,
        "the layer created after the save came back"
    );
    assert!(
        ed.active().unwrap().history.can_undo(),
        "restored work is undoable"
    );
}

#[test]
fn a_clean_previous_session_offers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 60);
    let project = dir.path().join("a.rstudio");
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    drop(ed);

    let mut ed = bare(dir.path(), ScriptedDialogs::new().answering_recover(true));
    let report = ed.recover(&SessionRecord {
        pid: 1,
        open_projects: vec![project],
        autosaves: Vec::new(),
    });
    assert!(report.is_empty(), "{report:?}");
    assert!(ed.documents().is_empty());
}

#[test]
fn an_unsaved_document_is_recovered_from_its_scratch_autosave() {
    // Requirement 6's other half. A document that has never been saved has no
    // package and therefore no journal, so `open_projects` reaches none of it:
    // the autosave *was* the whole safety net, and nothing could read it back.
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 32, 32, 60);
    let config = dir.path().join("config");
    let make = |dialogs: ScriptedDialogs| {
        Editor::with_state(
            AppPaths::rooted(&config),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(dialogs),
        )
    };

    // A run that crashes with an hour of unsaved work in it.
    let autosaves = {
        let mut ed = make(ScriptedDialogs::new());
        ed.open_path(&a).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        assert!(
            ed.open_project_paths().is_empty(),
            "nothing here has a package; this is the case that was lost"
        );
        let report = ed.autosave_now();
        assert_eq!(report.written.len(), 1, "{report:?}");
        let paths = ed.autosave_paths();
        assert_eq!(paths, vec![report.written[0].1.clone()]);
        paths
    };
    let record = SessionRecord {
        pid: 1,
        open_projects: Vec::new(),
        autosaves: autosaves.clone(),
    };

    // Accepting reopens it with its content.
    let mut ed = make(ScriptedDialogs::new().answering_recover(true));
    let report = ed.recover(&record);
    assert_eq!(report.restored, vec![(autosaves[0].clone(), 0)]);
    assert!(report.failed.is_empty(), "{report:?}");
    assert_eq!(ed.documents().len(), 1);
    let doc = ed.active().unwrap();
    assert_eq!(doc.title(), "a.png");
    assert_eq!(doc.document.layers.len(), 2, "the unsaved layer came back");
    assert!(
        doc.is_dirty(),
        "it is still unsaved work — the user never chose a location"
    );
    assert_eq!(
        doc.project_path(),
        None,
        "and the scratch directory is not a location it may adopt"
    );
    // The recovered document keeps writing to the file it came from rather
    // than leaving the old one behind for ever.
    assert_eq!(ed.autosave_paths(), autosaves);

    // Declining throws the offer away instead of repeating it at every start.
    let mut ed = make(ScriptedDialogs::new().answering_recover(false));
    let report = ed.recover(&record);
    assert_eq!(report.declined, autosaves);
    assert!(ed.documents().is_empty());
    assert!(!autosaves[0].exists(), "a declined autosave is cleaned up");
}

#[test]
fn two_runs_never_write_the_same_scratch_autosave() {
    // `DocumentId` restarts at 1 every launch, so `autosave-{id}.rstudio` made
    // the next run's first unsaved document overwrite the previous crash's
    // copy. Each editor stands for one run of the process here.
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 3);
    let config = dir.path().join("config");
    let mut paths = Vec::new();
    for _ in 0..2 {
        let mut ed = Editor::with_state(
            AppPaths::rooted(&config),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&a).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let report = ed.autosave_now();
        assert_eq!(report.written.len(), 1);
        // Same document id both times: this is exactly what used to collide.
        assert_eq!(report.written[0].0, ed.active().unwrap().id());
        paths.push(report.written[0].1.clone());
    }
    assert_ne!(
        paths[0], paths[1],
        "the second run would have silently overwritten the first's autosave"
    );
    assert!(paths[0].exists() && paths[1].exists(), "{paths:?}");
}

#[test]
fn a_scratch_autosave_is_cleaned_up_once_the_work_is_really_saved() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 3);
    let target = dir.path().join("chosen.rstudio");
    let mut ed = bare(dir.path(), ScriptedDialogs::new().saving_to(target.clone()));
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();

    ed.autosave_now();
    let scratch = ed.autosave_paths();
    assert_eq!(scratch.len(), 1);
    assert!(scratch[0].join(project_format::MANIFEST_FILE).is_file());

    ed.dispatch(Action::Save).unwrap();
    assert!(
        ed.autosave_paths().is_empty(),
        "the safety net goes when the work has a home"
    );
    assert!(!scratch[0].exists(), "and the scratch package is removed");
    assert!(target.join(project_format::MANIFEST_FILE).is_file());
}

#[test]
fn closing_a_document_takes_its_scratch_autosave_with_it() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 3);
    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new().answering_close(CloseChoice::Discard),
    );
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.autosave_now();
    let scratch = ed.autosave_paths();
    assert_eq!(scratch.len(), 1);

    ed.dispatch(Action::CloseDocument).unwrap();
    assert!(ed.autosave_paths().is_empty());
    assert!(!scratch[0].exists());
}

#[test]
fn a_document_that_gains_a_package_stops_autosaving_to_scratch() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 3);
    let project = dir.path().join("p.rstudio");
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    ed.autosave_now();
    let scratch = ed.autosave_paths()[0].clone();

    // Saved out of band (as `close`'s Save branch does), then autosaved again.
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    ed.dispatch(Action::NewLayer).unwrap();
    let report = ed.autosave_now();
    assert_eq!(report.written[0].1, project);
    assert!(ed.autosave_paths().is_empty());
    assert!(!scratch.exists(), "the stale duplicate is not left behind");
}

#[test]
fn activating_a_tab_that_is_not_open_names_the_tab_not_a_command() {
    // It used to report `Action::NextDocument` for every out-of-range index,
    // so a tab click's refusal would have named a command nobody issued.
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();

    let err = ed.activate(9).unwrap_err();
    assert_eq!(err.index, 9);
    assert_eq!(err.open, 1);
    let text = err.to_string();
    assert!(text.contains("tab 9"), "{text}");
    assert!(
        !text.contains(&Action::NextDocument.label()),
        "a tab click is not Next Document: {text}"
    );
    assert_eq!(ed.active_index(), Some(0), "and nothing moved");
}

#[test]
fn the_colour_actions_report_what_they_did() {
    // Both were menu items whose effect nothing on screen showed. The wells in
    // the tool strip are the main fix; this is the status line that tells a
    // user who reached them from the menu that anything happened.
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.dispatch(Action::SwapColors).unwrap();
    assert_eq!(ed.status(), Some("Foreground #FFFFFF"));
    ed.dispatch(Action::ResetColors).unwrap();
    assert_eq!(ed.status(), Some("Foreground #000000"));
    assert_eq!(color_hex([1.0, 0.5, 0.0, 1.0]), "#FF8000");
    assert_eq!(color_hex([-1.0, 2.0, 0.0, 1.0]), "#00FF00", "clamped");
}

#[test]
fn the_shortcut_editor_binds_reports_conflicts_and_resets() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    let chord = Chord::ctrl(Key::character('9'));
    ed.rebind(chord, Action::Export).unwrap();
    assert_eq!(ed.keymap().resolve(&chord), Some(Action::Export));
    assert_eq!(
        ed.preferences().keymap_overrides.len(),
        1,
        "the preferences follow the keymap immediately"
    );

    // A chord that already means something is refused and the conflict parked
    // for the prompt to render.
    let taken = Chord::ctrl(Key::character('s'));
    let err = ed.rebind(taken, Action::Export).unwrap_err();
    assert_eq!(err.actions, vec![Action::Save, Action::Export]);
    assert_eq!(ed.pending_conflict(), Some(&err));
    assert_eq!(
        ed.keymap().resolve(&taken),
        Some(Action::Save),
        "a refusal changes nothing"
    );

    // "Replace" takes it; the conflict is answered.
    ed.force_rebind(taken, Action::Export);
    assert_eq!(ed.keymap().resolve(&taken), Some(Action::Export));
    assert_eq!(ed.pending_conflict(), None);

    ed.unbind_chord(chord);
    assert_eq!(ed.keymap().resolve(&chord), None);

    ed.reset_keymap();
    assert_eq!(ed.keymap().resolve(&taken), Some(Action::Save));
    assert!(ed.preferences().keymap_overrides.is_empty());
}

#[test]
fn the_preferences_window_is_a_real_toggle() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    assert!(!ed.preferences_open());
    assert_eq!(
        ed.dispatch(Action::ShowPreferences),
        Ok(Effect::Preferences)
    );
    assert!(ed.preferences_open());

    // Closing it also answers any conflict prompt it was showing.
    let _ = ed.rebind(Chord::ctrl(Key::character('s')), Action::Export);
    assert!(ed.pending_conflict().is_some());
    ed.dispatch(Action::ShowPreferences).unwrap();
    assert!(!ed.preferences_open());
    assert!(ed.pending_conflict().is_none());
}

#[test]
fn a_runtime_rebinding_survives_the_shutdown_path() {
    // `Shell::shut_down` calls `capture_geometry`, which clones the *stored*
    // preferences, adds the window rectangle and hands them back. That used to
    // rebuild the keymap from a stale override list — reverting every shortcut
    // the user had changed — and `persist` then wrote the reverted list out.
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let chord = Chord::ctrl(Key::character('k'));
    {
        let mut ed = Editor::with_state(
            AppPaths::rooted(&config),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.keymap_mut().force_bind(chord, Action::Export);
        assert_eq!(ed.keymap().resolve(&chord), Some(Action::Export));

        // Exactly what `capture_geometry` does.
        let mut prefs = ed.preferences().clone();
        prefs.window = Some(crate::prefs::WindowGeometry::DEFAULT);
        ed.set_preferences(prefs);
        assert_eq!(
            ed.keymap().resolve(&chord),
            Some(Action::Export),
            "a geometry write must not revert the keymap"
        );
        ed.persist().unwrap();
    }

    let ed = Editor::new(AppPaths::rooted(&config), Box::new(ScriptedDialogs::new()));
    assert_eq!(ed.keymap().resolve(&chord), Some(Action::Export));
    assert!(ed.preferences().window.is_some(), "and the geometry landed");
}

#[test]
fn a_deliberate_override_list_still_replaces_the_keymap() {
    // The other side of the rule above: a preferences update that genuinely
    // carries a different override list is a real change, not a stale echo.
    let dir = tempfile::tempdir().unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    let chord = Chord::ctrl(Key::character('k'));
    ed.keymap_mut().force_bind(chord, Action::Export);

    let mut prefs = ed.preferences().clone();
    prefs.keymap_overrides = vec![crate::keymap::KeyOverride {
        chord: Chord::ctrl(Key::character('9')),
        action: Some(Action::ZoomFit),
    }];
    ed.set_preferences(prefs);
    assert_eq!(
        ed.keymap().resolve(&chord),
        Some(Action::ShowPreferences),
        "Ctrl+K is back to its default"
    );
    assert_eq!(
        ed.keymap().resolve(&Chord::ctrl(Key::character('9'))),
        Some(Action::ZoomFit)
    );
}

#[test]
fn preferences_and_the_keymap_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    {
        let mut ed = Editor::with_state(
            AppPaths::rooted(&config),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.open_path(&a).unwrap();
        let mut prefs = ed.preferences().clone();
        prefs.theme = crate::prefs::ThemeChoice::Light;
        prefs.ui_scale = 1.5;
        ed.set_preferences(prefs);
        ed.keymap_mut()
            .force_bind(Chord::ctrl(Key::character('k')), Action::Export);
        ed.persist().unwrap();
    }

    let ed = Editor::new(AppPaths::rooted(&config), Box::new(ScriptedDialogs::new()));
    assert_eq!(ed.preferences().theme, crate::prefs::ThemeChoice::Light);
    assert_eq!(ed.preferences().ui_scale, 1.5);
    assert_eq!(
        ed.keymap().resolve(&Chord::ctrl(Key::character('k'))),
        Some(Action::Export)
    );
    assert_eq!(ed.recent().entries(), [a]);
}

#[test]
fn changing_the_history_depth_reaches_open_documents() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_path(&a).unwrap();

    let mut prefs = ed.preferences().clone();
    prefs.history_depth = 3;
    ed.set_preferences(prefs);
    assert_eq!(ed.active().unwrap().history.limit(), 3);

    for _ in 0..6 {
        ed.dispatch(Action::NewLayer).unwrap();
    }
    assert_eq!(ed.active().unwrap().history.undo_depth(), 3);
}

#[test]
fn a_project_directory_is_told_apart_from_an_image() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    assert!(!Editor::is_project_path(&a));
    assert!(Editor::is_project_path(Path::new("/x/y.rstudio")));
    assert!(Editor::is_project_path(Path::new("/x/y.RSTUDIO")));

    // A directory holding a manifest counts even without the extension.
    let odd = dir.path().join("no-extension");
    std::fs::create_dir_all(&odd).unwrap();
    assert!(!Editor::is_project_path(&odd));
    std::fs::write(odd.join(project_format::MANIFEST_FILE), "{}").unwrap();
    assert!(Editor::is_project_path(&odd));
}

#[test]
fn open_project_paths_are_what_the_crash_marker_records() {
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 8, 8, 1);
    let b = write_png(dir.path(), "b.png", 8, 8, 2);
    let project = dir.path().join("a.rstudio");
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_paths(&[a, b]);
    assert!(
        ed.open_project_paths().is_empty(),
        "images are not projects"
    );
    ed.activate(0).unwrap();
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    assert_eq!(ed.open_project_paths(), vec![project]);
}

#[test]
fn open_project_reopens_a_saved_package_through_the_folder_picker() {
    // The defect: File ▸ Open used `rfd`'s *file* picker with a `.rstudio`
    // filter, and a package is a directory. That filter could never match, so
    // the application's own save format was reachable only from Open Recent,
    // a drag-and-drop or the command line.
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 24, 16, 140);
    let project = dir.path().join("piece.rstudio");

    // A run that saves some work.
    let saved_pixels = {
        let mut ed = bare(dir.path(), ScriptedDialogs::new());
        ed.open_path(&a).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        let doc = ed.active_mut().unwrap();
        doc.save_to(&project, "test").unwrap();
        let rect = doc.canvas_rect();
        doc.composite(rect).unwrap()
    };
    assert!(project.is_dir(), "a package is a directory");

    // A later run opens it from the dialog and gets exactly that back.
    let mut ed = bare(
        dir.path(),
        ScriptedDialogs::new().opening_project(project.clone()),
    );
    assert_eq!(ed.dispatch(Action::OpenProject), Ok(Effect::DocumentSet));
    assert_eq!(ed.documents().len(), 1, "it landed in a tab");
    let doc = ed.active_mut().unwrap();
    assert_eq!(doc.project_path(), Some(project.as_path()));
    assert_eq!(doc.document.layers.len(), 2);
    let rect = doc.canvas_rect();
    assert_eq!(
        doc.composite(rect).unwrap(),
        saved_pixels,
        "the composite is the one that was saved"
    );

    // Cancelling the folder picker opens nothing and is not an error.
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    assert_eq!(
        ed.dispatch(Action::OpenProject),
        Err(ActionError::Cancelled(Action::OpenProject))
    );
    assert!(ed.documents().is_empty());
}

#[test]
fn a_folder_that_is_not_a_package_is_refused_with_a_reason() {
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("just-a-folder");
    std::fs::create_dir_all(&plain).unwrap();
    let mut ed = bare(dir.path(), ScriptedDialogs::new().opening_project(&plain));
    let err = ed.dispatch(Action::OpenProject).unwrap_err();
    match err {
        ActionError::Failed { action, reason } => {
            assert_eq!(action, Action::OpenProject);
            assert!(reason.contains(project_format::MANIFEST_FILE), "{reason}");
        }
        other => panic!("expected a failure that says why, got {other:?}"),
    }
    assert!(ed.documents().is_empty());
}

#[test]
fn a_declined_recovery_only_ever_deletes_this_applications_own_scratch() {
    // The path a bad marker takes: `recover` used to `remove_dir_all` whatever
    // the record named the moment the user said "no". A marker naming another
    // run's live autosave — or anything else at all — was destroyed by a click
    // on a dialog about a different document.
    let dir = tempfile::tempdir().unwrap();
    let a = write_png(dir.path(), "a.png", 16, 16, 5);

    // Build a real package somewhere that is *not* this editor's scratch.
    let elsewhere = dir.path().join("somebody-elses").join("work.rstudio");
    {
        let mut ed = bare(dir.path(), ScriptedDialogs::new());
        ed.open_path(&a).unwrap();
        ed.dispatch(Action::NewLayer).unwrap();
        ed.active()
            .unwrap()
            .write_snapshot(&elsewhere, "test")
            .unwrap();
    }
    assert!(elsewhere.join(project_format::MANIFEST_FILE).is_file());

    let mut ed = bare(dir.path(), ScriptedDialogs::new().answering_recover(false));
    let report = ed.recover(&SessionRecord {
        pid: 1,
        open_projects: Vec::new(),
        autosaves: vec![elsewhere.clone()],
    });
    assert_eq!(report.declined, vec![elsewhere.clone()]);
    assert!(
        elsewhere.join(project_format::MANIFEST_FILE).is_file(),
        "a declined offer must not delete work outside this run's scratch"
    );

    // A real scratch autosave, by contrast, is cleaned up on decline.
    let scratch = ed.preferences().scratch_dir(ed.paths());
    let mine = scratch.join(format!(
        "autosave-x-1.{}",
        crate::dialogs::PROJECT_EXTENSION
    ));
    std::fs::create_dir_all(&scratch).unwrap();
    copy_tree(&elsewhere, &mine);
    let mut ed = bare(dir.path(), ScriptedDialogs::new().answering_recover(false));
    let report = ed.recover(&SessionRecord {
        pid: 1,
        open_projects: Vec::new(),
        autosaves: vec![mine.clone()],
    });
    assert_eq!(report.declined, vec![mine.clone()]);
    assert!(!mine.exists(), "the offer is not repeated at every start");
}

/// Copy a package directory, so a test can plant one where it needs it.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}
