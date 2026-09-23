//! W5-D: the everyday flows, driven through the shell's own routes — the
//! key path `window_event` feeds, `apply_chrome` for what a chrome frame
//! meant, a real headless egui frame for the Layers and Properties controls,
//! and the editor's File ▸ Open / Save As actions.

use super::*;
use winit::keyboard::Key as WKey;

use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn write_png(dir: &std::path::Path, name: &str, side: u32) -> PathBuf {
    let png = dir.join(name);
    std::fs::write(
        &png,
        raster::encode(
            raster::ExportFormat::Png,
            side,
            side,
            &vec![200u8; (side * side * 4) as usize],
        )
        .unwrap(),
    )
    .unwrap();
    png
}

fn editor_with(dir: &std::path::Path, dialogs: ScriptedDialogs) -> crate::editor::Editor {
    crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// One 16x16 image in a 200x160 window at 100%, centred — screen `(100, 80)`
/// is document `(8, 8)`.
fn shell_ready(dir: &std::path::Path) -> Shell {
    let png = write_png(dir, "a.png", 16);
    let mut editor = editor_with(dir, ScriptedDialogs::new());
    editor.open_path(&png).unwrap();
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(200.0, 160.0));
    let doc = shell.editor.active_mut().unwrap();
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::new(8.0, 8.0);
    shell
}

fn click_canvas(shell: &mut Shell, doc: Vec2) {
    shell.cursor = Vec2::new(100.0, 80.0) + doc - Vec2::new(8.0, 8.0);
    shell.on_pointer(PointerPhase::Down, PointerButton::Primary, false);
    shell.on_pointer(PointerPhase::Up, PointerButton::Primary, false);
}

fn press(shell: &mut Shell, key: WKey, mods: ModifiersState) {
    shell.modifiers = mods;
    shell.on_key(KeyboardOwner::default(), &key, ElementState::Pressed, false);
}

fn type_text(shell: &mut Shell, text: &str) {
    for c in text.chars() {
        press(
            shell,
            WKey::Character(c.to_string().as_str().into()),
            ModifiersState::empty(),
        );
    }
}

/// Every text layer's run in the active document.
fn text_runs(shell: &Shell) -> Vec<String> {
    let doc = shell.editor.active().unwrap();
    doc.document
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| doc.document.layers.get(id))
        .filter_map(|l| match &l.kind {
            layer_model::LayerKind::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .collect()
}

/// Start a Type-tool run with a canvas click and type `text` into it.
fn typing(dir: &std::path::Path, text: &str) -> Shell {
    let mut shell = shell_ready(dir);
    shell.editor.set_tool(tools::ToolId::Type);
    click_canvas(&mut shell, Vec2::new(4.0, 8.0));
    assert!(shell.pointer.is_text_editing(), "the click opened a run");
    type_text(&mut shell, text);
    shell
}

#[test]
fn switching_tools_while_typing_commits_the_text_instead_of_deleting_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = typing(dir.path(), "Hi");
    let depth = shell.editor.active().unwrap().history_depth();

    // A tool button click, exactly what the toolbar's frame reports.
    shell.apply_chrome(ChromeOutput {
        select_tool: Some(tools::ToolId::Brush),
        ..Default::default()
    });

    assert!(!shell.pointer.is_text_editing(), "the run ended");
    assert_eq!(
        text_runs(&shell),
        vec!["Hi".to_string()],
        "the typed text survives the tool switch"
    );
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        depth + 1,
        "the run landed as one undoable entry"
    );
}

#[test]
fn escape_commits_the_typed_text_as_photopea_does() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = typing(dir.path(), "Yo");
    press(
        &mut shell,
        WKey::Named(NamedKey::Escape),
        ModifiersState::empty(),
    );
    assert!(!shell.pointer.is_text_editing(), "Escape ends the run");
    assert_eq!(
        text_runs(&shell),
        vec!["Yo".to_string()],
        "Escape keeps what was typed"
    );
}

#[test]
fn altgr_characters_type_into_a_live_text_run() {
    // Windows reports AltGr as Ctrl+Alt: `@`, `€` and `{` on non-US layouts.
    let dir = tempfile::tempdir().unwrap();
    let mut shell = typing(dir.path(), "a");
    let altgr = ModifiersState::CONTROL | ModifiersState::ALT;
    for c in ["@", "\u{20ac}", "{", "\\"] {
        press(&mut shell, WKey::Character(c.into()), altgr);
    }
    press(
        &mut shell,
        WKey::Named(NamedKey::Enter),
        ModifiersState::CONTROL,
    );
    assert_eq!(
        text_runs(&shell),
        vec!["a@\u{20ac}{\\".to_string()],
        "every AltGr character landed in the text"
    );
}

#[test]
fn bound_ctrl_alt_chords_still_reach_the_keymap_mid_typing() {
    // Round 2 control: Ctrl+Alt is also a real chord family. Ctrl+Alt+J is
    // Duplicate Layer and Ctrl+Alt+Z Undo — typed mid-run they must act, not
    // land as letters, while an AltGr `@` beside them still types.
    let dir = tempfile::tempdir().unwrap();
    let mut shell = typing(dir.path(), "a");
    let chord = ModifiersState::CONTROL | ModifiersState::ALT;
    press(&mut shell, WKey::Character("@".into()), chord);
    press(&mut shell, WKey::Character("j".into()), chord);
    press(
        &mut shell,
        WKey::Character("S".into()),
        chord | ModifiersState::SHIFT,
    );
    press(&mut shell, WKey::Character("z".into()), chord);
    let runs = text_runs(&shell);
    assert!(
        runs.iter()
            .all(|r| !r.contains('j') && !r.contains('S') && !r.contains('z')),
        "a bound Ctrl+Alt chord was typed as text: {runs:?}"
    );
    press(
        &mut shell,
        WKey::Named(NamedKey::Enter),
        ModifiersState::CONTROL,
    );
    let runs = text_runs(&shell);
    assert!(
        runs.iter().any(|r| r == "a@"),
        "the AltGr character still typed: {runs:?}"
    );
}

#[test]
fn ctrl_alt_j_mid_typing_duplicates_the_layer() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = typing(dir.path(), "a");
    let layers = |shell: &Shell| {
        let doc = shell.editor.active().unwrap();
        doc.document.layers.iter_depth_first().len()
    };
    let before = layers(&shell);
    press(
        &mut shell,
        WKey::Character("j".into()),
        ModifiersState::CONTROL | ModifiersState::ALT,
    );
    assert!(
        layers(&shell) > before,
        "Ctrl+Alt+J reached the keymap (Duplicate Layer) instead of the text"
    );
}

// ------------------------------------------------------------------ masks

fn frame_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

/// Draw settling frames (the docks size themselves over the first few);
/// return the last one's painted strings.
fn settle(ctx: &egui::Context, shell: &mut Shell) -> Vec<(String, egui::Rect)> {
    let mut texts = Vec::new();
    for _ in 0..6 {
        let full = ctx.run(frame_input(Vec::new()), |ctx| {
            let _ = shell.chrome.ui(ctx, &mut shell.editor);
        });
        texts = full
            .shapes
            .iter()
            .filter_map(|clipped| match &clipped.shape {
                egui::Shape::Text(t) => Some((
                    t.galley.text().to_string(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                _ => None,
            })
            .collect();
    }
    texts
}

/// A press and release at `pos`; returns what that frame meant.
fn click_at(ctx: &egui::Context, shell: &mut Shell, pos: egui::Pos2) -> ChromeOutput {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(
        frame_input(vec![
            egui::Event::PointerMoved(pos),
            button(true),
            button(false),
        ]),
        |ctx| {
            out = shell.chrome.ui(ctx, &mut shell.editor);
        },
    );
    out
}

fn click_layers_mask_button(shell: &mut Shell) {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    settle(&ctx, shell);
    let pos = ctx
        .read_response(ui::view::ids::layer_mask())
        .expect("the Layers panel drew its mask button")
        .rect
        .center();
    let out = click_at(&ctx, shell, pos);
    shell.apply_chrome(out);
}

fn active_layer_mask(shell: &Shell) -> bool {
    let doc = shell.editor.active().unwrap();
    let id = doc.document.active_layer().unwrap();
    doc.document.layers.get(id).unwrap().mask.is_some()
}

#[test]
fn the_layers_mask_button_masks_the_selection_and_aims_painting_at_the_mask() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());
    shell.editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(0, 0),
        max: glam::IVec2::new(8, 16),
    };
    assert!(!shell.editor.edit_target_is_mask());

    click_layers_mask_button(&mut shell);

    assert!(active_layer_mask(&shell), "the button attached a mask");
    assert!(
        shell.editor.edit_target_is_mask(),
        "painting now lands on the new mask, not the image"
    );
    let rect = shell.editor.active().unwrap().canvas_rect();
    let composite = shell.editor.active_mut().unwrap().composite(rect).unwrap();
    // Row 0: pixel 2 is inside the selection, pixel 12 outside it.
    assert_ne!(composite[2 * 4 + 3], 0, "inside the selection shows");
    assert_eq!(
        composite[12 * 4 + 3],
        0,
        "outside the selection is hidden: the mask came from the selection"
    );
}

/// After a real chrome frame: is the mask well's target badge drawn on the
/// active row, and do the Properties draw the mask's own controls?
fn drawn_mask_focus(ctx: &egui::Context, shell: &mut Shell) -> (bool, bool, bool) {
    let texts = settle(ctx, shell);
    let id = shell
        .editor
        .active()
        .unwrap()
        .document
        .active_layer()
        .unwrap();
    let badge = ctx
        .read_response(ui::view::ids::mask_target_badge(id))
        .is_some();
    let mask_props = texts.iter().any(|(t, _)| t == "Density");
    let focus =
        shell.chrome.workspace().property_focus == ui::panels::properties::PropertyFocus::Mask;
    (badge, mask_props, focus)
}

/// The Properties toggle's painted "Layer" segment (the one beside "Mask").
fn layer_segment(texts: &[(String, egui::Rect)]) -> Option<egui::Pos2> {
    let mask = mask_segment(texts)?;
    texts
        .iter()
        .filter(|(t, r)| {
            t == "Layer" && (r.center().y - mask.y).abs() < 2.0 && r.center().x < mask.x
        })
        .map(|(_, r)| r.center())
        .max_by(|a, b| a.x.total_cmp(&b.x))
}

#[test]
fn after_add_mask_the_chrome_draws_the_mask_as_the_target_and_layer_goes_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());
    shell
        .chrome
        .workspace_for_test()
        .dock
        .set_open(ui::PanelId::Properties, true);
    click_layers_mask_button(&mut shell);
    assert!(shell.editor.edit_target_is_mask());

    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let (badge, mask_props, focus) = drawn_mask_focus(&ctx, &mut shell);
    assert!(focus, "the Properties toggle shows Mask");
    assert!(badge, "the mask well carries the target badge");
    assert!(mask_props, "Properties shows the mask (Density/Feather)");

    // Clicking "Layer" is a real change now, and goes back to the content.
    let texts = settle(&ctx, &mut shell);
    let pos = layer_segment(&texts).expect("the toggle's Layer segment is painted");
    let out = click_at(&ctx, &mut shell, pos);
    assert_eq!(
        out.edit_target,
        Some(crate::edit_target::EditTargetKind::Content),
        "the Layer segment aims back at the content"
    );
    shell.apply_chrome(out);
    assert!(!shell.editor.edit_target_is_mask());
    let (badge, mask_props, focus) = drawn_mask_focus(&ctx, &mut shell);
    assert!(
        !focus && !badge && !mask_props,
        "the chrome follows back to Layer"
    );
}

#[test]
fn after_the_menu_adds_a_mask_the_chrome_draws_the_mask_as_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());
    shell
        .chrome
        .workspace_for_test()
        .dock
        .set_open(ui::PanelId::Properties, true);
    shell.apply_chrome(ChromeOutput {
        menu: vec![ui::MenuAction::Mask(ui::menu::MaskOp::RevealAll)],
        ..Default::default()
    });
    assert!(shell.editor.edit_target_is_mask());
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let (badge, mask_props, focus) = drawn_mask_focus(&ctx, &mut shell);
    assert!(
        focus && badge && mask_props,
        "badge={badge} props={mask_props} focus={focus}"
    );
}

#[test]
fn the_layers_mask_button_without_a_selection_reveals_all_and_targets_the_mask() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());

    click_layers_mask_button(&mut shell);

    assert!(active_layer_mask(&shell), "the button attached a mask");
    assert!(shell.editor.edit_target_is_mask(), "the mask is the target");
    let rect = shell.editor.active().unwrap().canvas_rect();
    let composite = shell.editor.active_mut().unwrap().composite(rect).unwrap();
    assert!(
        composite.iter().skip(3).step_by(4).all(|&a| a != 0),
        "Reveal All hides nothing"
    );
}

#[test]
fn layer_mask_from_the_menu_route_aims_painting_at_the_mask() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());
    shell.apply_chrome(ChromeOutput {
        menu: vec![ui::MenuAction::Mask(ui::menu::MaskOp::HideAll)],
        ..Default::default()
    });
    assert!(active_layer_mask(&shell));
    assert!(
        shell.editor.edit_target_is_mask(),
        "Layer > Layer Mask > Hide All aims painting at the mask"
    );
}

/// The Properties panel's Layer|Mask control: the painted "Mask" segment,
/// found as the "Mask" galley sharing a row with a "Layer" galley.
fn mask_segment(texts: &[(String, egui::Rect)]) -> Option<egui::Pos2> {
    texts
        .iter()
        .filter(|(t, _)| t == "Mask")
        .find(|(_, mask)| {
            texts.iter().any(|(t, layer)| {
                t == "Layer"
                    && (layer.center().y - mask.center().y).abs() < 2.0
                    && layer.center().x < mask.center().x
                    && mask.center().x - layer.center().x < 200.0
            })
        })
        .map(|(_, r)| r.center())
}

#[test]
fn the_layer_mask_toggle_is_inert_without_a_mask_and_live_with_one() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_ready(dir.path());
    shell
        .chrome
        .workspace_for_test()
        .dock
        .set_open(ui::PanelId::Properties, true);
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);

    // No mask: clicking "Mask" must not aim anything at a mask.
    let texts = settle(&ctx, &mut shell);
    let pos = mask_segment(&texts).expect("the Properties toggle is painted");
    let out = click_at(&ctx, &mut shell, pos);
    assert_eq!(
        out.edit_target, None,
        "a maskless layer offers no Mask target"
    );
    shell.apply_chrome(out);
    assert!(!shell.editor.edit_target_is_mask());

    // The control: with a mask attached (and the target put back on the
    // content) the same click does aim at the mask.
    shell.apply_chrome(ChromeOutput {
        menu: vec![ui::MenuAction::Mask(ui::menu::MaskOp::RevealAll)],
        ..Default::default()
    });
    shell
        .editor
        .set_edit_target_kind(crate::edit_target::EditTargetKind::Content);
    shell.chrome.workspace_for_test().property_focus = ui::panels::properties::PropertyFocus::Layer;
    let texts = settle(&ctx, &mut shell);
    let pos = mask_segment(&texts).expect("the toggle is still painted");
    let out = click_at(&ctx, &mut shell, pos);
    assert_eq!(
        out.edit_target,
        Some(crate::edit_target::EditTargetKind::Mask),
        "with a mask the toggle works"
    );
}

// ------------------------------------------------------- Save As and Open

#[test]
fn save_as_renames_the_tab_and_the_window_to_the_saved_file() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    let target = dir.path().join("poster.rstudio");
    let mut editor = editor_with(dir.path(), ScriptedDialogs::new().saving_to(target.clone()));
    editor.open_path(&png).unwrap();
    assert_eq!(editor.active().unwrap().title(), "a.png");

    editor.dispatch(crate::action::Action::SaveAs).unwrap();
    editor.wait_for_saves();

    assert!(target.join(project_format::MANIFEST_FILE).is_file());
    assert_eq!(
        editor.active().unwrap().title(),
        "poster",
        "the tab follows Save As"
    );
    assert!(
        editor.window_title().contains("poster"),
        "the window title follows: {}",
        editor.window_title()
    );
    assert!(!editor.has_unsaved_work(), "the landed save is clean");
    // The package the worker wrote carries the name too.
    let mut again = editor_with(dir.path(), ScriptedDialogs::new());
    again.open_path(&target).unwrap();
    assert_eq!(again.active().unwrap().title(), "poster");
}

#[test]
fn a_failed_save_as_renames_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    // A file where the target's parent folder should be: the worker's write
    // is refused by the OS.
    let blocker = dir.path().join("blocker.txt");
    std::fs::write(&blocker, b"not a folder").unwrap();
    let target = blocker.join("poster.rstudio");
    let mut editor = editor_with(dir.path(), ScriptedDialogs::new().saving_to(target.clone()));
    editor.open_path(&png).unwrap();
    let window_before = editor.window_title();

    // The async route: the error is reported on the status line, not here.
    let _ = editor.dispatch(crate::action::Action::SaveAs);
    editor.wait_for_saves();

    assert!(
        editor
            .status()
            .is_some_and(|s| s.starts_with("Save failed")),
        "the save really failed: {:?}",
        editor.status()
    );
    assert!(!target.exists());
    let doc = editor.active().unwrap();
    assert_eq!(
        doc.title(),
        "a.png",
        "a refused Save As keeps the tab's name"
    );
    assert_eq!(doc.project_path(), None);
    assert_eq!(editor.window_title(), window_before, "and the window's");
}

#[test]
fn file_open_on_a_manifest_opens_the_project_package() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    let package = dir.path().join("kept.rstudio");
    {
        let mut editor = editor_with(dir.path(), ScriptedDialogs::new());
        editor.open_path(&png).unwrap();
        editor
            .active_mut()
            .unwrap()
            .save_to(&package, "test")
            .unwrap();
    }
    let manifest = package.join(project_format::MANIFEST_FILE);
    // File > Open (and the start screen's Open card) is `Action::Open`.
    let mut editor = editor_with(dir.path(), ScriptedDialogs::new().opening(manifest));
    editor.dispatch(crate::action::Action::Open).unwrap();

    let doc = editor.active().expect("the project opened");
    assert_eq!(doc.project_path(), Some(package.as_path()));
    assert_eq!(doc.title(), "kept");

    // The folder itself, and any file inside a `*.rstudio`, map the same way.
    assert_eq!(
        crate::editor::Editor::project_package_for(&package),
        Some(package.clone())
    );
    let inner = std::fs::read_dir(&package)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .find(|p| p.is_dir())
        .map(|d| d.join("anything.bin"))
        .unwrap_or_else(|| package.join("anything.bin"));
    assert_eq!(
        crate::editor::Editor::project_package_for(&inner),
        Some(package.clone())
    );
    assert_eq!(crate::editor::Editor::project_package_for(&png), None);
}

fn saved_package(dir: &std::path::Path) -> PathBuf {
    let png = write_png(dir, "a.png", 16);
    let package = dir.join("kept.rstudio");
    let mut editor = editor_with(dir, ScriptedDialogs::new());
    editor.open_path(&png).unwrap();
    editor
        .active_mut()
        .unwrap()
        .save_to(&package, "test")
        .unwrap();
    package
}

#[test]
fn a_manifest_on_the_command_line_opens_its_project() {
    // `raster-studio kept.rstudio/manifest.json`: `Shell::resumed` hands the
    // startup files to `Editor::open_paths` (it needs a real window, so the
    // test makes that same call), which opens each through `open_path`.
    let dir = tempfile::tempdir().unwrap();
    let package = saved_package(dir.path());
    let manifest = package.join(project_format::MANIFEST_FILE);
    let mut editor = editor_with(dir.path(), ScriptedDialogs::new());
    let opened = editor.open_paths(&[manifest]);
    assert_eq!(opened.len(), 1, "status: {:?}", editor.status());
    let doc = editor.active().expect("the startup file opened");
    assert_eq!(doc.project_path(), Some(package.as_path()));
    assert_eq!(doc.title(), "kept");
}

#[test]
fn a_manifest_dropped_on_an_empty_window_opens_its_project() {
    let dir = tempfile::tempdir().unwrap();
    let package = saved_package(dir.path());
    let manifest = package.join(project_format::MANIFEST_FILE);
    let mut shell = Shell::new(editor_with(dir.path(), ScriptedDialogs::new()), Vec::new());
    assert!(shell.editor.active().is_none());
    shell.on_dropped_files(&[manifest]);
    let doc = shell.editor.active().expect("the dropped manifest opened");
    assert_eq!(doc.project_path(), Some(package.as_path()));
}

#[test]
fn the_open_picker_lists_projects_first() {
    let filters = crate::dialogs::open_file_filters();
    let (_, first) = &filters[0];
    assert!(first.contains(&"rstudio") && first.contains(&"json"));
    assert!(
        first.contains(&"png"),
        "images stay one click away: {first:?}"
    );
    assert!(filters
        .iter()
        .any(|(name, ext)| *name == "Raster Studio project" && ext.contains(&"json")));
}

#[test]
fn a_shot_run_leaves_the_recent_files_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    let editor = editor_with(dir.path(), ScriptedDialogs::new());
    let mut shell = Shell::with_shot(editor, Vec::new(), Some(dir.path().join("shot.png")));
    shell.editor.open_path(&png).unwrap();
    assert!(
        shell.editor.recent().is_empty(),
        "a --shot fixture was recorded: {:?}",
        shell.editor.recent().entries()
    );

    // The control: the same open without --shot records.
    let editor = editor_with(dir.path(), ScriptedDialogs::new());
    let mut shell = Shell::with_shot(editor, Vec::new(), None);
    shell.editor.open_path(&png).unwrap();
    assert_eq!(shell.editor.recent().len(), 1);
}

#[test]
fn save_as_psd_renames_the_tab_to_the_psd() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    let target = dir.path().join("layered.psd");
    let mut editor = editor_with(
        dir.path(),
        ScriptedDialogs::new().exporting_to(target.clone()),
    );
    editor.open_path(&png).unwrap();
    crate::layer_ops::save_as_psd(&mut editor).unwrap();
    editor.wait_for_saves();
    assert!(target.is_file(), "the PSD was written");
    assert_eq!(editor.active().unwrap().title(), "layered");
}

#[test]
fn a_synchronous_save_to_names_the_document_and_the_package_after_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let png = write_png(dir.path(), "a.png", 16);
    let package = dir.path().join("named.rstudio");
    let mut editor = editor_with(dir.path(), ScriptedDialogs::new());
    editor.open_path(&png).unwrap();
    editor
        .active_mut()
        .unwrap()
        .save_to(&package, "test")
        .unwrap();
    assert_eq!(editor.active().unwrap().title(), "named");
    // The package itself carries the name: reopening shows it.
    let mut again = editor_with(dir.path(), ScriptedDialogs::new());
    again.open_path(&package).unwrap();
    assert_eq!(again.active().unwrap().title(), "named");
}
