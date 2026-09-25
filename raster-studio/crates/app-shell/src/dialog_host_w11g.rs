//! W11-G: what the dialog host needs the editor for when it answers Help >
//! Keyboard Shortcut Sheet, Help > Search Commands and the keyboard-only
//! layer-step / blend-mode chords.

use editor_core::{Command, LayerPatch};
use ui::menu::MenuAction;

use crate::keymap::{menu_aliases, menu_bindings, shortcut_of_chord, Chord, Resolved};
use crate::menu_bridge::Pick;
use crate::Editor;

/// Every chord the live keymap answers, with what it performs: the
/// application table (user overrides included) and the menu table beneath
/// it, each chord resolved through [`crate::keymap::Keymap::resolve_any`] so
/// the sheet shows what the key really does.
pub(super) fn sheet_rows(editor: &Editor) -> Vec<ui::dialogs::ShortcutRow> {
    let keymap = editor.keymap();
    let mut chords: Vec<Chord> = keymap.bindings().into_iter().map(|b| b.chord).collect();
    chords.extend(menu_bindings().into_iter().map(|(c, _)| c));
    chords.extend(menu_aliases().into_iter().map(|(c, _)| c));
    chords.sort();
    chords.dedup();
    chords
        .into_iter()
        .filter_map(|chord| {
            let command = match keymap.resolve_any(&chord)? {
                Resolved::App(action) => action.label().to_string(),
                Resolved::Menu(menu) => menu.label(),
            };
            let spelled = shortcut_of_chord(&chord)
                .map(|s| s.to_string())
                .unwrap_or_else(|| chord.to_string());
            Some(ui::dialogs::ShortcutRow {
                chord: spelled,
                command,
            })
        })
        .collect()
}

/// Whether `action` is usable now, asked the way the menu bar asks: the
/// document half of the menu context (what `&Editor` can give) resolved,
/// then the bridge's own route check.
fn enabled(action: MenuAction, ctx: &ui::MenuContext, editor: &Editor) -> bool {
    match action.resolve(ctx) {
        ui::menu::Resolution::Enabled(intent) => {
            crate::menu_bridge::pick(&intent, editor).is_some()
        }
        ui::menu::Resolution::Disabled(_) => false,
    }
}

/// The command palette over every menu-bar row enabled right now.
///
/// `frame` is the menu context the chrome built this frame
/// (`crate::menu_bridge::context`: the editor's clipboard, revert source,
/// theme and the workspace's dock, view flags, stored selections and last
/// filter), handed to the host by `Chrome::ui` through
/// [`super::DialogHost::set_menu_context`] before any intent is routed, so
/// the palette offers exactly the rows the menu bar enables. Without one (a
/// host driven without a chrome frame) the context is the part `&Editor`
/// alone can give: the document, recent files, revert source, internal
/// clipboard, open-document count and theme.
pub(super) fn command_palette(
    editor: &Editor,
    frame: Option<&ui::MenuContext>,
) -> ui::dialogs::CommandSearchDialog {
    let ctx = match frame {
        Some(ctx) => ctx.clone(),
        None => editor_context(editor),
    };
    let menus = crate::menu_bridge::menus(editor);
    ui::dialogs::CommandSearchDialog::from_menus(&menus, |a| {
        a != MenuAction::CommandSearch && enabled(a, &ctx, editor)
    })
}

/// The menu context `&Editor` alone can give (no workspace).
fn editor_context(editor: &Editor) -> ui::MenuContext {
    let mut ctx = match editor.active() {
        Some(open) => ui::MenuContext::from_document(&open.document, &open.history),
        None => ui::MenuContext::default(),
    };
    ctx.recent_files = editor
        .recent()
        .entries()
        .iter()
        .map(|p| p.display().to_string())
        .collect();
    ctx.has_path = editor.revert_source().is_some();
    ctx.clipboard.pixels = editor.clipboard().is_some();
    ctx.open_documents = editor.documents().len();
    ctx.theme = editor.preferences().theme.resolve(design::Theme::Dark);
    ctx
}

/// Photoshop semantics for the blend-mode chords: with a painting tool
/// active (one [`tools::composites_strokes`] names: Brush, Pencil, Clone
/// Stamp, Pattern Stamp, Gradient, Paint Bucket) Shift+Alt+letter and
/// Shift+Plus / Shift+Minus set that tool's options-bar Mode
/// ([`tools::BLEND_MODE_KEY`]), not the active layer's.
///
/// `None` when the chord is not the tool's to answer (not a blend chord, or
/// the tool does not paint): the layer answers it through [`chord_pick`].
/// `Some(None)` when it is the tool's and the Mode already is that mode.
pub(crate) fn paint_chord_pick(
    action: MenuAction,
    tool: tools::ToolId,
    options: &ui::ToolOptions,
) -> Option<Option<Pick>> {
    if !tools::composites_strokes(tool) {
        return None;
    }
    let now = options.blend_mode(tool)?;
    let mode = match action {
        MenuAction::BlendModeChord(mode) => mode,
        MenuAction::CycleBlendMode(forward) => ui::menu::cycle_blend_mode(now, forward),
        _ => return None,
    };
    let index = layer_model::BlendMode::ALL
        .iter()
        .position(|m| *m == mode)?;
    Some((mode != now).then(|| {
        Pick::Workspace(Box::new(ui::Intent::SetToolOption {
            tool,
            key: tools::BLEND_MODE_KEY,
            value: ui::OptionValue::Choice(index),
        }))
    }))
}

/// The pick a layer-step or blend-mode chord makes, or nothing when there
/// is nothing to act on.
pub(super) fn chord_pick(action: MenuAction, editor: &Editor) -> Option<Pick> {
    let open = editor.active()?;
    let doc = &open.document;
    match action {
        MenuAction::SelectLayerStep(step) => {
            let order = doc.layers.iter_depth_first();
            let current = doc
                .active_layer()
                .and_then(|id| order.iter().position(|l| *l == id));
            let target = order[step.target(current, order.len())?];
            Some(Pick::SelectLayers(vec![target], Some(target)))
        }
        MenuAction::BlendModeChord(_) | MenuAction::CycleBlendMode(_) => {
            let id = doc.active_layer()?;
            let now = doc.layers.get(id)?.blend_mode;
            let mode = match action {
                MenuAction::BlendModeChord(mode) => mode,
                MenuAction::CycleBlendMode(forward) => ui::menu::cycle_blend_mode(now, forward),
                _ => return None,
            };
            (mode != now).then(|| {
                Pick::Command(Command::SetLayerProperties {
                    layer_id: id,
                    patch: LayerPatch {
                        blend_mode: Some(mode),
                        ..Default::default()
                    },
                })
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::{install_theme, Chrome, ChromeOutput};
    use crate::dialog_host::ActiveDialog;
    use crate::dialogs::ScriptedDialogs;
    use crate::prefs::{AppPaths, Preferences};
    use crate::recent::RecentFiles;
    use ui::menu::{FilterId, LayerStep};

    /// An 8 x 8 image with three layers (the image and two new ones), the
    /// top one active.
    fn editor(dir: &std::path::Path) -> Editor {
        let png = dir.join("a.png");
        std::fs::write(
            &png,
            raster::encode(raster::ExportFormat::Png, 8, 8, &[9u8; 8 * 8 * 4]).unwrap(),
        )
        .unwrap();
        let mut ed = Editor::with_state(
            AppPaths::rooted(dir.join("config")),
            Preferences::default(),
            RecentFiles::new(),
            Box::new(ScriptedDialogs::new()),
        );
        ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
        ed.open_path(&png).unwrap();
        ed.dispatch(crate::action::Action::NewLayer).unwrap();
        ed.dispatch(crate::action::Action::NewLayer).unwrap();
        ed
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        }
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

    /// One real chrome frame; the outputs the shell's `apply_chrome` would
    /// apply for these routes (layer selection, commands) are applied the
    /// same way.
    fn frame(chrome: &mut Chrome, ed: &mut Editor, ctx: &egui::Context, events: Vec<egui::Event>) {
        let mut out = ChromeOutput::default();
        let _ = ctx.run(input(events), |ctx| out = chrome.ui(ctx, ed));
        if let Some((layers, active)) = out.select_layers.take() {
            ed.set_layer_selection(layers, active);
        }
        for command in out.commands.drain(..) {
            ed.apply_command(command);
        }
    }

    fn active_blend(ed: &Editor) -> layer_model::BlendMode {
        let doc = &ed.active().unwrap().document;
        doc.layers
            .get(doc.active_layer().unwrap())
            .unwrap()
            .blend_mode
    }

    /// Help > Search Commands: typing "Gaussian" into the palette and
    /// pressing Enter opens the Gaussian Blur dialog, through the same road a
    /// menu click takes.
    #[test]
    fn the_command_search_finds_gaussian_and_running_it_opens_its_dialog() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome.emit(ui::Intent::Action(MenuAction::CommandSearch));
        for _ in 0..2 {
            frame(&mut chrome, &mut ed, &ctx, Vec::new());
        }
        match chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::CommandSearch(p) => {
                assert!(p.entries().len() > 100, "{}", p.entries().len());
                assert!(p
                    .entries()
                    .iter()
                    .all(|e| e.action != MenuAction::CommandSearch));
            }
            _ => panic!("the palette did not open"),
        }
        // Typed into the focused search field, as a user types.
        frame(
            &mut chrome,
            &mut ed,
            &ctx,
            vec![egui::Event::Text("Gaussian".into())],
        );
        match chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::CommandSearch(p) => {
                assert_eq!(p.query(), "Gaussian");
                assert_eq!(
                    p.selected(),
                    Some(MenuAction::Filter(FilterId::GaussianBlur))
                );
            }
            _ => panic!("the palette closed"),
        }
        frame(&mut chrome, &mut ed, &ctx, vec![key(egui::Key::Enter)]);
        frame(&mut chrome, &mut ed, &ctx, Vec::new());
        match chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::Filter(dialog) => {
                assert_eq!(dialog.spec().id, FilterId::GaussianBlur)
            }
            _ => panic!("running Gaussian Blur did not open its dialog"),
        }
    }

    /// The palette offers exactly the rows the menu bar enables this frame:
    /// Edit > Paste right after a Copy, File > Revert for a document opened
    /// from a file, and every workspace-gated row (dock, view, ruler unit,
    /// stored selections, last filter) resolved against the chrome's own
    /// per-frame context, not a document-only one.
    #[test]
    fn the_command_search_enables_what_the_menu_bar_enables() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.dispatch(crate::action::Action::Copy).unwrap();
        assert!(ed.clipboard().is_some(), "Copy filled the clipboard");
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        // A workspace-only state: the rulers read in inches.
        let inches = ui::dialogs::units::Unit::Inches;
        chrome.emit(ui::Intent::SetRulerUnit(inches));
        frame(&mut chrome, &mut ed, &ctx, Vec::new());
        chrome.emit(ui::Intent::Action(MenuAction::CommandSearch));
        for _ in 0..2 {
            frame(&mut chrome, &mut ed, &ctx, Vec::new());
        }
        let live = crate::menu_bridge::context(&mut ed, chrome.workspace());
        let menus = crate::menu_bridge::menus(&ed);
        let expected: Vec<MenuAction> = ui::dialogs::CommandSearchDialog::from_menus(&menus, |a| {
            a != MenuAction::CommandSearch && enabled(a, &live, &ed)
        })
        .entries()
        .iter()
        .map(|e| e.action)
        .collect();
        let got: Vec<MenuAction> = match chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::CommandSearch(p) => p.entries().iter().map(|e| e.action).collect(),
            _ => panic!("the palette did not open"),
        };
        assert!(got.contains(&MenuAction::Paste), "Paste after a Copy");
        assert!(got.contains(&MenuAction::Revert), "Revert for a file");
        assert!(
            !got.contains(&MenuAction::SetRulerUnit(inches)),
            "the rulers already read in inches"
        );
        assert!(got.contains(&MenuAction::SetRulerUnit(ui::dialogs::units::Unit::Pixels)));
        assert_eq!(got, expected, "the palette and the menu bar disagree");
    }

    /// Help > Keyboard Shortcut Sheet lists the live keymap, a user rebind
    /// included, and filters by typing.
    #[test]
    fn the_shortcut_sheet_lists_the_live_keymap() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let rows = sheet_rows(&ed);
        // The sheet spells a chord the way the menu bar does on this platform
        // ("Ctrl+Shift+P" on Windows/Linux, "Shift+Cmd+P" on macOS), so the
        // expectations are spelled through the same function.
        let spell = |chord: &str| {
            let c: crate::keymap::Chord = chord.parse().expect("a chord");
            crate::keymap::shortcut_of_chord(&c)
                .map(|s| s.to_string())
                .unwrap_or_else(|| c.to_string())
        };
        let has = |rows: &[ui::dialogs::ShortcutRow], command: &str, chord: &str| {
            let want = spell(chord);
            rows.iter().any(|r| r.command == command && r.chord == want)
        };
        assert!(has(&rows, "Search Commands…", "Ctrl+Shift+P"), "{rows:?}");
        assert!(has(&rows, "Keyboard Shortcut Sheet…", "Shift+/"));
        assert!(has(&rows, "Fade…", "Ctrl+Shift+F"));
        assert!(has(&rows, "Select Layer Below", "Alt+["));
        assert!(has(&rows, "Blend Mode: Multiply", "Alt+Shift+M"));
        // A rebind shows on the next open: Ctrl+Shift+F12 bound to Export.
        let free = crate::keymap::Chord {
            ctrl_or_cmd: true,
            alt: false,
            shift: true,
            key: crate::keymap::Key::Function(12),
        };
        ed.keymap_mut()
            .bind(free, crate::action::Action::Export)
            .expect("the chord is free");
        let f12 = spell("Ctrl+Shift+F12");
        assert!(sheet_rows(&ed).iter().any(|r| r.chord == f12));

        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        chrome.emit(ui::Intent::Action(MenuAction::ShortcutSheet));
        for _ in 0..2 {
            frame(&mut chrome, &mut ed, &ctx, Vec::new());
        }
        frame(
            &mut chrome,
            &mut ed,
            &ctx,
            vec![egui::Event::Text("search commands".into())],
        );
        match chrome.dialogs_for_test().active_for_test() {
            ActiveDialog::ShortcutSheet(sheet) => {
                let visible = sheet.visible();
                // W13-M: Ctrl+F (Photopea's Find) reaches it too.
                assert_eq!(visible.len(), 2, "{visible:?}");
                let chords: Vec<&str> = visible.iter().map(|r| r.chord.as_str()).collect();
                assert!(
                    chords.contains(&spell("Ctrl+Shift+P").as_str()),
                    "{chords:?}"
                );
                assert!(chords.contains(&spell("Ctrl+F").as_str()), "{chords:?}");
            }
            _ => panic!("the sheet did not open"),
        }
        frame(&mut chrome, &mut ed, &ctx, vec![key(egui::Key::Escape)]);
        assert!(!chrome.dialog_open(), "Escape closes the sheet");
    }

    /// Alt+[ / Alt+] / Alt+, / Alt+. walk the layer stack, and the Shift+Alt
    /// letters and Shift+Plus / Shift+Minus set the active layer's blend
    /// mode, each through the intent the chord emits.
    #[test]
    fn the_layer_and_blend_chords_act_on_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let order = ed.active().unwrap().document.layers.iter_depth_first();
        assert_eq!(order.len(), 3);
        let active = |ed: &Editor| ed.active().unwrap().document.active_layer();
        assert_eq!(active(&ed), Some(order[0]), "the newest layer is on top");
        let mut chord = |ed: &mut Editor, action: MenuAction| {
            chrome.emit(ui::Intent::Action(action));
            for _ in 0..2 {
                frame(&mut chrome, ed, &ctx, Vec::new());
            }
        };
        chord(&mut ed, MenuAction::SelectLayerStep(LayerStep::Below));
        assert_eq!(active(&ed), Some(order[1]));
        chord(&mut ed, MenuAction::SelectLayerStep(LayerStep::Bottom));
        assert_eq!(active(&ed), Some(order[2]));
        chord(&mut ed, MenuAction::SelectLayerStep(LayerStep::Above));
        assert_eq!(active(&ed), Some(order[1]));
        chord(&mut ed, MenuAction::SelectLayerStep(LayerStep::Top));
        assert_eq!(active(&ed), Some(order[0]));

        let depth = ed.active().unwrap().history_depth();
        chord(
            &mut ed,
            MenuAction::BlendModeChord(layer_model::BlendMode::Multiply),
        );
        assert_eq!(active_blend(&ed), layer_model::BlendMode::Multiply);
        assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
        chord(&mut ed, MenuAction::CycleBlendMode(true));
        assert_eq!(
            active_blend(&ed),
            ui::menu::cycle_blend_mode(layer_model::BlendMode::Multiply, true)
        );
        chord(&mut ed, MenuAction::CycleBlendMode(false));
        assert_eq!(active_blend(&ed), layer_model::BlendMode::Multiply);
        // The mode it already has is no step at all.
        let depth = ed.active().unwrap().history_depth();
        chord(
            &mut ed,
            MenuAction::BlendModeChord(layer_model::BlendMode::Multiply),
        );
        assert_eq!(ed.active().unwrap().history_depth(), depth);
    }

    /// Photoshop semantics: with a painting tool active the Shift+Alt letter
    /// chords and Shift+Plus / Shift+Minus set the tool's options-bar Mode
    /// (the value the chrome forwards to the stroke), and the layer's blend
    /// mode and the history are untouched. A non-painting tool (Eraser)
    /// leaves them to the layer again.
    #[test]
    fn the_blend_chords_set_the_painting_tools_mode_not_the_layers() {
        use layer_model::BlendMode;
        use tools::ToolId;
        let dir = tempfile::tempdir().unwrap();
        let mut ed = editor(dir.path());
        ed.set_tool(ToolId::Brush);
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let chord = |chrome: &mut Chrome, ed: &mut Editor, action: MenuAction| {
            chrome.emit(ui::Intent::Action(action));
            for _ in 0..2 {
                frame(chrome, ed, &ctx, Vec::new());
            }
        };
        let brush_mode = |chrome: &Chrome| chrome.workspace().options.blend_mode(ToolId::Brush);
        let layer_before = active_blend(&ed);
        let depth = ed.active().unwrap().history_depth();
        assert_eq!(brush_mode(&chrome), Some(BlendMode::Normal));

        chord(
            &mut chrome,
            &mut ed,
            MenuAction::BlendModeChord(BlendMode::Multiply),
        );
        assert_eq!(brush_mode(&chrome), Some(BlendMode::Multiply));
        assert_eq!(
            active_blend(&ed),
            layer_before,
            "the layer is not the brush"
        );
        assert_eq!(
            ed.active().unwrap().history_depth(),
            depth,
            "no document step"
        );
        // The forward set the stroke is built from carries the Mode.
        let multiply = BlendMode::ALL
            .iter()
            .position(|m| *m == BlendMode::Multiply);
        assert!(chrome
            .tool_options(ToolId::Brush)
            .iter()
            .any(|(k, v)| k == tools::BLEND_MODE_KEY
                && *v == ui::OptionValue::Choice(multiply.unwrap())));

        chord(&mut chrome, &mut ed, MenuAction::CycleBlendMode(true));
        assert_eq!(
            brush_mode(&chrome),
            Some(ui::menu::cycle_blend_mode(BlendMode::Multiply, true))
        );
        chord(&mut chrome, &mut ed, MenuAction::CycleBlendMode(false));
        assert_eq!(brush_mode(&chrome), Some(BlendMode::Multiply));
        assert_eq!(active_blend(&ed), layer_before);
        assert_eq!(ed.active().unwrap().history_depth(), depth);

        // The Eraser has no Mode: the chord goes to the layer.
        ed.set_tool(ToolId::Eraser);
        chord(
            &mut chrome,
            &mut ed,
            MenuAction::BlendModeChord(BlendMode::Screen),
        );
        assert_eq!(active_blend(&ed), BlendMode::Screen);
        assert_eq!(brush_mode(&chrome), Some(BlendMode::Multiply));
    }
}
