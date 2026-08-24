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
        Action::ShowFileInfo => Effect::Preferences,
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

    // Walked straight through `Editor::can`, which is the enablement rule
    // itself. This used to walk `Editor::menus()` — a third menu model beside
    // `ui::menu` and `menu_bridge`, drawn by nothing — and asserting on a menu
    // no window renders proves only that the builder ran.
    let mut saw_disabled = false;
    for action in Action::all() {
        assert!(!action.label().is_empty(), "{} has no label", action.id());
        if let Err(refusal) = ed.can(action) {
            assert!(
                !refusal.to_string().is_empty(),
                "{} greys out silently",
                action.id()
            );
            saw_disabled = true;
        }
    }
    assert!(
        saw_disabled,
        "with no document open, some actions must be off"
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
fn painting_with_the_red_channel_isolated_writes_only_red() {
    use editor_core::pixels::{PixelTarget, TileDelta, TileEdit};
    use raster::TileCoord;

    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "one.png", 256, 256, 0);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_paths(&[png]);
    let layer = ed.active().unwrap().document.active_layer().unwrap();

    // A solid RGBA8 tile with the same byte pattern on every pixel.
    let solid = |p: [u8; 4]| {
        let n = (raster::TILE_SIZE as usize).pow(2) * 4;
        let mut v = Vec::with_capacity(n);
        for _ in 0..(n / 4) {
            v.extend_from_slice(&p);
        }
        v
    };

    // First lay down a known pattern on every channel.
    let hash_a = ed
        .active_mut()
        .unwrap()
        .tiles
        .insert_bytes(solid([11, 22, 33, 255]));
    ed.apply_command(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash_a)),
    });

    // Isolate red as the edit target, then paint a different pattern.
    ed.set_paint_channel(Some(0));
    let hash_b = ed
        .active_mut()
        .unwrap()
        .tiles
        .insert_bytes(solid([200, 40, 50, 255]));
    ed.apply_command(Command::PaintTiles {
        target: PixelTarget::Layer(layer),
        delta: TileDelta::single(TileEdit::set(TileCoord::new(0, 0, 0), hash_b)),
    });

    // Only the red channel moved; green and blue kept the prior values.
    let key = editor_core::pixels::PixelKey::Layer(layer);
    let coord = TileCoord::new(0, 0, 0);
    let stored = ed
        .active()
        .unwrap()
        .document
        .pixels
        .tile(key, coord)
        .expect("tile stored");
    let bytes = ed.active().unwrap().tiles.tile(stored).unwrap();
    assert_eq!(&bytes[0..4], &[200, 22, 33, 255], "only red changed");
    assert_eq!(&bytes[123 * 4..123 * 4 + 4], &[200, 22, 33, 255]);

    // Undo restores the whole prior tile (all channels).
    ed.active_mut().unwrap().undo().unwrap();
    let back = ed
        .active()
        .unwrap()
        .document
        .pixels
        .tile(key, coord)
        .unwrap();
    assert_eq!(
        &ed.active().unwrap().tiles.tile(back).unwrap()[0..4],
        &[11, 22, 33, 255]
    );
}

#[test]
fn export_layers_writes_a_png_per_layer_into_the_chosen_folder() {
    use editor_core::Command as Cmd;
    use layer_model::Layer as L;

    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("layers");
    std::fs::create_dir(&folder).unwrap();
    let png = write_png(dir.path(), "one.png", 8, 8, 90);
    let mut ed = bare(dir.path(), ScriptedDialogs::new().exporting_folder(&folder));
    ed.open_paths(&[png]);
    // A second, distinct layer so the export has two files and its own name.
    ed.apply_command(Cmd::create_layer(L::raster("Photo & Bright")));

    ed.export_layers().unwrap();

    let pngs: Vec<_> = std::fs::read_dir(&folder)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(pngs.len(), 2, "one PNG per layer, not one per document");
    let names: Vec<String> = pngs
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert!(
        !names.contains(&"Photo & Bright.png".to_string()),
        "a hostile layer name must be made file-safe: {names:?}"
    );
    assert!(
        names
            .iter()
            .all(|n| n.ends_with(".png") && std::path::Path::new(n).is_file()
                || n.chars().all(|c| c.is_alphanumeric() || " _-.".contains(c))),
        "name is file-safe: {names:?}"
    );
}

#[test]
fn rasterizing_a_non_raster_layer_bakes_it_to_a_raster() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, adj) = with_adjustment(dir.path());
    ed.active_mut()
        .unwrap()
        .document
        .set_active_layer(Some(adj))
        .unwrap();

    ed.rasterize_active_layer().unwrap();

    let doc = &ed.active().unwrap().document;
    assert_eq!(doc.layers.len(), 2, "one layer replaced, not duplicated");
    let ids = doc.layers.iter_depth_first();
    assert!(
        ids.iter().any(|id| {
            matches!(
                doc.layers.get(*id).map(|l| &l.kind),
                Some(layer_model::LayerKind::Raster(_))
            )
        }),
        "the baked layer became a raster layer"
    );
}

#[test]
fn a_solid_fill_layer_covers_the_canvas_in_the_foreground_colour() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "one.png", 16, 16, 0);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_paths(&[png]);
    ed.set_foreground([1.0, 0.0, 0.0, 1.0]);

    ed.new_solid_fill_layer().unwrap();

    let doc = ed.active_mut().unwrap();
    assert_eq!(doc.document.layers.len(), 2, "a fill layer was added");
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    assert_eq!(
        &rgba[0..4],
        &[255, 0, 0, 255],
        "the canvas reads as the fill"
    );
}

#[test]
fn converting_a_layer_to_a_smart_object_keeps_its_pixels() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "one.png", 32, 32, 77);
    let mut ed = bare(dir.path(), ScriptedDialogs::new());
    ed.open_paths(&[png]);
    let rect = ed.active().unwrap().canvas_rect();
    let before = ed.active_mut().unwrap().composite(rect).unwrap();

    ed.convert_to_smart_object().unwrap();

    let kind = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .iter()
        .find_map(|id| {
            ed.active()
                .unwrap()
                .document
                .layers
                .get(*id)
                .map(|l| &l.kind)
        });
    assert!(
        matches!(kind, Some(layer_model::LayerKind::SmartObject(_))),
        "the layer became a smart object"
    );
    let rect = ed.active().unwrap().canvas_rect();
    let after = ed.active_mut().unwrap().composite(rect).unwrap();
    assert_eq!(after, before, "a smart object renders what the source drew");
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

// ---------------------------------------------------------------------------
// Editing an adjustment layer's parameters
//
// Every one of these fails without `Command::SetLayerKind` and
// `Editor::apply_kind_edit`. Before them, `Intent::EditLayerKind` — the only
// channel an adjustment's parameters or a text layer's content can travel
// through — resolved to `None` in `menu_bridge::pick` and was dropped without a
// word by `Chrome::harvest`, so every slider in the Properties panel moved a
// value that was re-read from the document on the next frame and sprang back.
// ---------------------------------------------------------------------------

/// An editor with one 64x48 image open and a Brightness/Contrast adjustment
/// layer above it, at identity — the state adding an adjustment leaves you in.
fn with_adjustment(dir: &Path) -> (Editor, LayerId) {
    let image = write_png(dir, "adj.png", 64, 48, 90);
    let mut ed = bare(dir, ScriptedDialogs::new());
    ed.open_path(&image).unwrap();
    let layer = Layer::with_kind(
        "Brightness/Contrast",
        LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::BrightnessContrast {
                brightness: 0.0,
                contrast: 0.0,
            },
        }),
    );
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    assert!(
        ed.active().unwrap().document.layers.get(id).is_some(),
        "the fixture never got its adjustment layer: {:?}",
        ed.status()
    );
    (ed, id)
}

/// The brightness the document currently holds for `layer`.
fn brightness_of(ed: &Editor, layer: LayerId) -> f32 {
    match &ed
        .active()
        .unwrap()
        .document
        .layers
        .get(layer)
        .unwrap()
        .kind
    {
        LayerKind::Adjustment(a) => match a.kind {
            layer_model::AdjustmentKind::BrightnessContrast { brightness, .. } => brightness,
            ref other => panic!("not a Brightness/Contrast: {other:?}"),
        },
        other => panic!("not an adjustment layer: {other:?}"),
    }
}

/// What the Properties panel emits for one frame of a slider drag.
fn slide_brightness(ed: &mut Editor, layer: LayerId, brightness: f32, gesture: Option<u64>) {
    ed.apply_kind_edit(crate::chrome::KindEdit {
        layer,
        kind: Box::new(LayerKind::Adjustment(layer_model::AdjustmentLayer {
            kind: layer_model::AdjustmentKind::BrightnessContrast {
                brightness,
                contrast: 0.0,
            },
        })),
        gesture,
    });
}

#[test]
fn an_adjustment_slider_changes_the_document_and_what_the_canvas_shows() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    let region = ed.active().unwrap().canvas_rect();
    let before = ed.active_mut().unwrap().composite(region).unwrap();

    slide_brightness(&mut ed, layer, 0.5, None);

    assert_eq!(
        brightness_of(&ed, layer),
        0.5,
        "the value never reached the document, so the knob springs back"
    );
    let after = ed.active_mut().unwrap().composite(region).unwrap();
    assert_ne!(
        before, after,
        "the parameter moved and the composited image did not — an adjustment \
         nobody can see is the same decoration as one nobody can change"
    );

    // ...and it went through history like every other edit, so it can be taken
    // back rather than being a mutation behind undo's back.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.0);
    let undone = ed.active_mut().unwrap().composite(region).unwrap();
    assert_eq!(undone, before, "undo did not restore the pixels");
}

#[test]
fn one_drag_of_a_slider_is_one_undo_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    let depth = ed.active().unwrap().history_depth();

    // Two hundred frames of a single sweep, exactly as the panel emits them:
    // one intent per frame the pointer moved, all inside one press.
    for frame in 1..=200 {
        slide_brightness(&mut ed, layer, frame as f32 / 200.0, Some(7));
    }

    assert_eq!(brightness_of(&ed, layer), 1.0, "the sweep did not land");
    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth + 1,
        "one drag pushed {} entries, so undo would take that many presses",
        ed.active().unwrap().history_depth() - depth
    );
    // One Ctrl+Z goes back to where the sweep began, not to 199/200 of the way
    // through it — which is what makes the single entry *correct* and not
    // merely small.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.0);
    // ...and the redo stack still holds the whole sweep.
    ed.dispatch(Action::Redo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 1.0);
}

#[test]
fn a_drag_writes_one_journal_record_per_frame_while_history_gains_one() {
    // The half of the fold that is *not* folded, measured so the doc comment on
    // `apply_kind_edit` cannot drift back into claiming otherwise. The undo the
    // fold takes is `OpenDocument::undo`, which writes no journal record, while
    // `OpenDocument::apply` appends and fsyncs one every single time — so a
    // saved project collects a record per frame of the sweep even though its
    // in-memory history collects one entry for the whole sweep.
    //
    // This is a statement about disk growth, not about correctness: the last
    // assertion replays what was written and lands on the value the user
    // settled on, because `SetLayerKind` is an absolute payload.
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    let project = dir.path().join("drag.rstudio");
    ed.active_mut().unwrap().save_to(&project, "test").unwrap();
    let depth = ed.active().unwrap().history_depth();

    const FRAMES: usize = 20;
    for frame in 1..=FRAMES {
        slide_brightness(&mut ed, layer, frame as f32 / FRAMES as f32, Some(3));
    }

    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth + 1,
        "the in-memory fold stopped working"
    );

    let recovery =
        project_format::CommandJournal::read(&project.join(project_format::JOURNAL_FILE)).unwrap();
    let kind_records = recovery
        .since_last_save()
        .iter()
        .filter(|c| matches!(c, Command::SetLayerKind { .. }))
        .count();
    assert_eq!(
        kind_records, FRAMES,
        "the journal folded the drag after all — if that became true on purpose, \
         say so on `Editor::apply_kind_edit` instead of leaving this test to \
         contradict it"
    );

    // ...and replaying every one of those records reaches the settled value, so
    // the per-frame records cost disk and nothing else.
    let mut replayed = ed.active().unwrap().document.clone();
    for command in recovery.since_last_save() {
        // The replay starts from the *current* document, so re-applying the
        // absolute payloads must be a no-op that ends where the drag ended.
        command.apply(&mut replayed).unwrap();
    }
    let settled = match &replayed.layers.get(layer).unwrap().kind {
        LayerKind::Adjustment(a) => match a.kind {
            layer_model::AdjustmentKind::BrightnessContrast { brightness, .. } => brightness,
            ref other => panic!("not a Brightness/Contrast: {other:?}"),
        },
        other => panic!("not an adjustment layer: {other:?}"),
    };
    assert_eq!(settled, 1.0, "replaying the journal lost the settled value");
}

#[test]
fn two_presses_are_two_undo_steps_and_a_typed_value_stands_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    let depth = ed.active().unwrap().history_depth();

    slide_brightness(&mut ed, layer, 0.2, Some(1));
    slide_brightness(&mut ed, layer, 0.3, Some(1));
    // The pointer came up and went down again: a second gesture, a second step.
    slide_brightness(&mut ed, layer, 0.6, Some(2));
    // No pointer at all — a value typed into the field or nudged with the
    // arrow keys. It has no gesture to be folded into.
    slide_brightness(&mut ed, layer, 0.9, None);

    assert_eq!(ed.active().unwrap().history_depth(), depth + 3);
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.6);
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.3);
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.0);
}

#[test]
fn folding_a_drag_never_rolls_back_somebody_elses_command() {
    // The fold works by undoing the entry the gesture already pushed. A gesture
    // id is a claim about the pointer, not about the history stack, so it is
    // checked against the stack before anything is rolled back.
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());

    slide_brightness(&mut ed, layer, 0.4, Some(5));
    // Something else lands on top mid-gesture.
    let interloper = Layer::raster("interloper");
    let interloper_id = interloper.id;
    ed.apply_command(Command::create_layer(interloper));
    let depth = ed.active().unwrap().history_depth();

    // The same gesture continues. It must push a new step, not eat the layer.
    slide_brightness(&mut ed, layer, 0.7, Some(5));

    assert!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(interloper_id)
            .is_some(),
        "the fold undid a command that was not its own"
    );
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
    assert_eq!(brightness_of(&ed, layer), 0.7);
}

#[test]
fn a_drag_that_outlives_a_tab_switch_does_not_disturb_the_other_document() {
    // The gesture survives, the history it was folding into does not: the user
    // is now looking at a different document with a different stack. The guard
    // is what keeps the stray frame from undoing whatever is on top of *that*
    // one.
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    slide_brightness(&mut ed, layer, 0.4, Some(9));

    let other = write_png(dir.path(), "other.png", 16, 16, 3);
    ed.open_path(&other).unwrap();
    let marker = Layer::raster("marker");
    let marker_id = marker.id;
    ed.apply_command(Command::create_layer(marker));
    let depth = ed.active().unwrap().history_depth();

    // A frame of the old drag, delivered against the new document.
    slide_brightness(&mut ed, layer, 0.6, Some(9));

    assert!(
        ed.active()
            .unwrap()
            .document
            .layers
            .get(marker_id)
            .is_some(),
        "a stray drag frame undid the other document's last command"
    );
    assert_eq!(ed.active().unwrap().history_depth(), depth);
}

#[test]
fn an_undo_mid_drag_stops_the_fold_even_though_the_stack_still_looks_right() {
    // The one case `tops_out_with_kind_edit` cannot answer. After an undo the
    // top of the stack really *is* a kind edit to this layer — an older one,
    // from an earlier gesture — so the guard says yes and folding would eat a
    // step the user still wants. `dispatch` clears the gesture for exactly
    // this, which the guard alone cannot cover.
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    slide_brightness(&mut ed, layer, 0.2, Some(1));
    slide_brightness(&mut ed, layer, 0.5, Some(2));
    let depth = ed.active().unwrap().history_depth();

    // Ctrl+Z while the second drag is still under the pointer.
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.2);
    // ...and the drag delivers one more frame.
    slide_brightness(&mut ed, layer, 0.7, Some(2));

    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth,
        "the stray frame folded into the step the undo had just uncovered"
    );
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(
        brightness_of(&ed, layer),
        0.2,
        "the first drag's step was swallowed"
    );
}

#[test]
fn a_history_panel_jump_mid_drag_stops_the_fold_too() {
    // The History panel's click walks the same timeline Ctrl+Z does, and leaves
    // the stack in the same shape: an older kind edit to this very layer on
    // top. `jump_history` clears the gesture for exactly that reason.
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    slide_brightness(&mut ed, layer, 0.2, Some(1));
    slide_brightness(&mut ed, layer, 0.5, Some(2));
    let depth = ed.active().unwrap().history_depth();

    assert_eq!(ed.jump_history(depth - 1), 1);
    assert_eq!(brightness_of(&ed, layer), 0.2);
    slide_brightness(&mut ed, layer, 0.7, Some(2));

    assert_eq!(
        ed.active().unwrap().history_depth(),
        depth,
        "the stray frame folded into the step the jump had just uncovered"
    );
    ed.dispatch(Action::Undo).unwrap();
    assert_eq!(brightness_of(&ed, layer), 0.2);
}

#[test]
fn a_kind_edit_of_the_wrong_class_is_refused_out_loud() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, layer) = with_adjustment(dir.path());
    ed.apply_kind_edit(crate::chrome::KindEdit {
        layer,
        kind: Box::new(LayerKind::Raster(Default::default())),
        gesture: None,
    });
    let said = ed.status().unwrap_or_default().to_string();
    assert!(
        said.contains("adjustment") && said.contains("raster"),
        "a refused class change said nothing useful: {said}"
    );
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
