//! W13-M: Photopea's gestures, driven through the shell's own routes only —
//! `on_key` (what `KeyboardInput` calls), `on_wheel` (what `MouseWheel`
//! calls), and real chrome frames (`Chrome::ui` + `Shell::apply_chrome`, the
//! two steps the render loop takes) for the Layers panel's mask thumbnail.
//!
//! * Shift + a tool letter steps through the tool's group;
//! * Alt+Ctrl+F reaches Filter > Last Filter (Photopea's chord);
//! * the wheel follows Photopea's modifiers (Alt inverts the zoom preference,
//!   Ctrl pans sideways);
//! * Shift+click on a mask thumbnail disables the mask (a red cross is drawn
//!   over the thumbnail) and enables it again; Alt+click shows the mask alone
//!   on the canvas; the backslash key toggles the rubylith overlay, backquote
//!   the mask alone, Escape puts the image back — on the key route, with or
//!   without the Layers panel painting the row;
//! * a repeated bare tool letter keeps the tool;
//! * Ctrl+F opens Find (Help > Search Commands), Photopea's Ctrl+F;
//! * File > Print as PDF… keeps the PDF route as its own row.

use super::*;
use winit::keyboard::Key as WKey;

use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use tools::ToolId;

const SIDE: u32 = 64;

fn shell_with_image(dir: &std::path::Path) -> Shell {
    let rgba = vec![200u8; (SIDE * SIDE * 4) as usize];
    let png = dir.join("a.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    editor.set_tool(ToolId::Brush);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    shell
}

fn press(shell: &mut Shell, mods: ModifiersState, key: &str) {
    shell.on_modifiers(mods);
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Character(key.into()),
        ElementState::Pressed,
        false,
    );
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Character(key.into()),
        ElementState::Released,
        false,
    );
    shell.on_modifiers(ModifiersState::empty());
}

#[test]
fn shift_and_a_tool_letter_steps_through_the_group_on_the_key_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let group = tools::registry::by_shortcut('m');
    assert!(group.len() > 1, "the marquee group has several tools");
    // With Shift held the platform reports the capital letter.
    press(&mut shell, ModifiersState::SHIFT, "M");
    assert_eq!(shell.editor.tool(), group[0], "Shift+M enters the group");
    press(&mut shell, ModifiersState::SHIFT, "M");
    assert_eq!(shell.editor.tool(), group[1], "Shift+M steps to the next");
    for _ in 1..group.len() {
        press(&mut shell, ModifiersState::SHIFT, "M");
    }
    assert_eq!(shell.editor.tool(), group[0], "Shift+M wraps round");
    // Every tool letter answers to Shift, and no Shift+letter is a menu chord
    // a tool letter would shadow.
    for key in crate::action::ToolKey::all() {
        let chord = Chord {
            ctrl_or_cmd: false,
            alt: false,
            shift: true,
            key: Key::Char(key.char()),
        };
        assert_eq!(
            shell.editor.keymap().resolve_any(&chord),
            Some(Resolved::App(Action::SelectTool(key))),
            "Shift+{key}"
        );
        assert_eq!(
            shell.editor.keymap().menu_action_for(&chord),
            None,
            "Shift+{key} is also a menu chord"
        );
    }
}

#[test]
fn alt_ctrl_f_is_last_filter_on_the_key_route() {
    let dir = tempfile::tempdir().unwrap();
    let shell = shell_with_image(dir.path());
    // The chord `on_key` builds for Alt+Ctrl+F, resolved the way it is.
    let chord = chord_from_key(
        &WKey::Character("f".into()),
        ModifiersState::CONTROL | ModifiersState::ALT,
    )
    .unwrap();
    assert_eq!(
        shell.editor.keymap().resolve_any(&chord),
        Some(Resolved::Menu(ui::MenuAction::LastFilter))
    );
}

#[test]
fn the_wheel_follows_photopeas_modifiers_on_the_shell_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let camera = |s: &Shell| {
        let c = &s.editor.active().unwrap().camera;
        (c.center, c.zoom)
    };
    // The preference on (a plain wheel zooms): Alt + wheel pans instead.
    assert!(shell.editor.preferences().scroll_wheel_zooms);
    let (c0, z0) = camera(&shell);
    shell.on_modifiers(ModifiersState::ALT);
    shell.on_wheel(Vec2::new(0.0, 1.0));
    let (c1, z1) = camera(&shell);
    assert_eq!(z1, z0, "Alt + wheel must not zoom while the wheel zooms");
    assert_ne!(c1.y, c0.y, "Alt + wheel pans vertically");
    // Ctrl + wheel zooms while the wheel zooms.
    shell.on_modifiers(ModifiersState::CONTROL);
    shell.on_wheel(Vec2::new(0.0, 1.0));
    let (_, z2) = camera(&shell);
    assert_ne!(z2, z1, "Ctrl + wheel zooms while the wheel zooms");
    // Alt + Ctrl + wheel pans sideways.
    shell.on_modifiers(ModifiersState::CONTROL | ModifiersState::ALT);
    let (c2, _) = camera(&shell);
    shell.on_wheel(Vec2::new(0.0, 1.0));
    let (c3, z3) = camera(&shell);
    assert_eq!(z3, z2);
    assert_ne!(c3.x, c2.x, "Alt + Ctrl + wheel pans horizontally");
    assert_eq!(c3.y, c2.y, "Alt + Ctrl + wheel pans horizontally");
}

// ---- the mask thumbnail -------------------------------------------------

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

/// One chrome frame carrying `events` with `modifiers` held, and the shell
/// performing what it meant. Returns the shapes the frame painted.
fn frame(
    ctx: &egui::Context,
    shell: &mut Shell,
    modifiers: egui::Modifiers,
    events: Vec<egui::Event>,
) -> Vec<egui::epaint::ClippedShape> {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 900.0),
        )),
        modifiers,
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let full = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
    full.shapes
}

fn active_layer(shell: &Shell) -> layer_model::LayerId {
    shell
        .editor
        .active()
        .unwrap()
        .document
        .active_layer()
        .expect("an active layer")
}

fn mask_enabled(shell: &Shell) -> Option<bool> {
    let id = active_layer(shell);
    let doc = &shell.editor.active().unwrap().document;
    doc.layers.get(id)?.mask.as_ref().map(|m| m.enabled)
}

/// Where the Layers panel drew `layer`'s mask thumbnail.
fn mask_thumb(ctx: &egui::Context, shell: &mut Shell, layer: layer_model::LayerId) -> egui::Rect {
    frame(ctx, shell, egui::Modifiers::NONE, Vec::new());
    ctx.read_response(ui::view::ids::layer_mask_thumb(layer))
        .expect("the mask thumbnail is drawn")
        .rect
}

fn click(ctx: &egui::Context, shell: &mut Shell, at: egui::Pos2, modifiers: egui::Modifiers) {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    frame(ctx, shell, modifiers, vec![egui::Event::PointerMoved(at)]);
    frame(ctx, shell, modifiers, vec![button(true)]);
    frame(ctx, shell, modifiers, vec![button(false)]);
    frame(
        ctx,
        shell,
        egui::Modifiers::NONE,
        vec![egui::Event::PointerGone],
    );
}

/// The red strokes a frame painted inside `rect` — the disabled-mask cross.
fn red_strokes_in(shapes: &[egui::epaint::ClippedShape], rect: egui::Rect) -> usize {
    let danger = design::color32(
        design::Theme::Dark
            .tokens()
            .palette
            .color(design::ColorRole::Danger),
    );
    shapes
        .iter()
        .filter(|c| match &c.shape {
            egui::Shape::LineSegment { points, stroke } => {
                stroke.color == egui::epaint::ColorMode::Solid(danger)
                    && rect.expand(1.0).contains(points[0])
                    && rect.expand(1.0).contains(points[1])
            }
            _ => false,
        })
        .count()
}

/// A shell whose active layer carries a pixel mask, added through the Layer
/// menu's own door (Layer > Layer Mask > Reveal All).
fn shell_with_mask(dir: &std::path::Path, ctx: &egui::Context) -> Shell {
    let mut shell = shell_with_image(dir);
    shell.perform_menu_chord(ui::MenuAction::Mask(ui::menu::MaskOp::RevealAll));
    frame(ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    assert_eq!(mask_enabled(&shell), Some(true), "the layer has a mask");
    shell
}

#[test]
fn shift_click_on_the_mask_thumbnail_disables_it_with_a_red_cross_and_enables_it_again() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let mut shell = shell_with_mask(dir.path(), &ctx);
    let layer = active_layer(&shell);
    let rect = mask_thumb(&ctx, &mut shell, layer);
    let shapes = frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    assert_eq!(
        red_strokes_in(&shapes, rect),
        0,
        "an enabled mask is not crossed"
    );

    click(&ctx, &mut shell, rect.center(), egui::Modifiers::SHIFT);
    assert_eq!(
        mask_enabled(&shell),
        Some(false),
        "Shift+click disables the mask"
    );
    let rect = mask_thumb(&ctx, &mut shell, layer);
    let shapes = frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    assert_eq!(
        red_strokes_in(&shapes, rect),
        2,
        "a disabled mask is crossed out in red over its thumbnail"
    );
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Composite);

    click(&ctx, &mut shell, rect.center(), egui::Modifiers::SHIFT);
    assert_eq!(
        mask_enabled(&shell),
        Some(true),
        "Shift+click enables it again"
    );
    // One undo step each way.
    shell.perform(Action::Undo);
    assert_eq!(mask_enabled(&shell), Some(false));
}

#[test]
fn alt_click_on_the_mask_thumbnail_shows_the_mask_alone_and_again_puts_the_image_back() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let mut shell = shell_with_mask(dir.path(), &ctx);
    let layer = active_layer(&shell);
    let rect = mask_thumb(&ctx, &mut shell, layer);
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Composite);

    click(&ctx, &mut shell, rect.center(), egui::Modifiers::ALT);
    assert_eq!(
        shell.chrome.mask_view(),
        ui::MaskViewMode::Grayscale,
        "Alt+click shows the mask alone"
    );
    assert_eq!(
        mask_enabled(&shell),
        Some(true),
        "Alt+click does not toggle"
    );
    click(&ctx, &mut shell, rect.center(), egui::Modifiers::ALT);
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Composite);

    // Alt+Shift+click shows it as the overlay instead.
    click(
        &ctx,
        &mut shell,
        rect.center(),
        egui::Modifiers::ALT | egui::Modifiers::SHIFT,
    );
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Overlay);
    assert_eq!(mask_enabled(&shell), Some(true));
}

/// A key the platform names (Escape and the like), pressed and released on
/// the shell's key route.
fn press_named(shell: &mut Shell, key: winit::keyboard::NamedKey) {
    for state in [ElementState::Pressed, ElementState::Released] {
        shell.on_key(KeyboardOwner::default(), &WKey::Named(key), state, false);
    }
}

#[test]
fn the_backslash_key_toggles_the_rubylith_overlay_on_the_key_route() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let mut shell = shell_with_mask(dir.path(), &ctx);
    // Close every dock, the Layers panel included: the key is the canvas's,
    // not the panel's (Photopea's `mskView` toggle in its key handler).
    shell.editor.dispatch(Action::TogglePanels).ok();
    press(&mut shell, ModifiersState::empty(), "\\");
    assert_eq!(
        shell.chrome.mask_view(),
        ui::MaskViewMode::Overlay,
        "backslash shows the mask as a rubylith"
    );
    press(&mut shell, ModifiersState::empty(), "\\");
    assert_eq!(
        shell.chrome.mask_view(),
        ui::MaskViewMode::Composite,
        "backslash again puts the image back"
    );

    // The render loop's half of the same press (egui sees the key too) must
    // not toggle it a second time: one press, one change.
    let key = |pressed| egui::Event::Key {
        key: egui::Key::Backslash,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    press(&mut shell, ModifiersState::empty(), "\\");
    frame(&ctx, &mut shell, egui::Modifiers::NONE, vec![key(true)]);
    frame(&ctx, &mut shell, egui::Modifiers::NONE, vec![key(false)]);
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Overlay);

    // Backquote shows the mask alone, and Escape puts the image back.
    press(&mut shell, ModifiersState::empty(), "`");
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Grayscale);
    press_named(&mut shell, winit::keyboard::NamedKey::Escape);
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Composite);

    // Without a mask on the active layer the keys do nothing.
    let dir2 = tempfile::tempdir().unwrap();
    let mut plain = shell_with_image(dir2.path());
    press(&mut plain, ModifiersState::empty(), "\\");
    press(&mut plain, ModifiersState::empty(), "`");
    assert_eq!(plain.chrome.mask_view(), ui::MaskViewMode::Composite);
}

#[test]
fn the_backslash_key_reaches_a_masked_layer_inside_a_folded_group() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let mut shell = shell_with_mask(dir.path(), &ctx);
    let masked = active_layer(&shell);
    // Group the masked layer (Layer > Group Layers) and fold the group, so
    // the Layers panel paints no row for it.
    shell.perform_menu_chord(ui::MenuAction::GroupLayers);
    frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    let doc = &shell.editor.active().unwrap().document;
    let group = doc
        .layers
        .parent_of(masked)
        .expect("the layer is inside a group");
    shell
        .chrome
        .workspace_for_test()
        .layers
        .set_expanded(group, false);
    // Make the masked layer the active one again, inside the folded group.
    shell
        .editor
        .active_mut()
        .unwrap()
        .document
        .set_active_layer(Some(masked))
        .unwrap();
    frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    press(&mut shell, ModifiersState::empty(), "\\");
    assert_eq!(shell.chrome.mask_view(), ui::MaskViewMode::Overlay);
}

#[test]
fn a_repeated_bare_tool_letter_keeps_the_tool_on_the_key_route() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_image(dir.path());
    let group = tools::registry::by_shortcut('m');
    press(&mut shell, ModifiersState::empty(), "m");
    assert_eq!(shell.editor.tool(), group[0], "M enters the marquee group");
    press(&mut shell, ModifiersState::empty(), "m");
    assert_eq!(
        shell.editor.tool(),
        group[0],
        "M again keeps the tool (Photopea)"
    );
    press(&mut shell, ModifiersState::SHIFT, "M");
    assert_eq!(shell.editor.tool(), group[1]);
    press(&mut shell, ModifiersState::empty(), "m");
    assert_eq!(shell.editor.tool(), group[1], "the bare letter keeps it");
}

#[test]
fn ctrl_f_opens_find_the_command_search_on_the_key_route() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ctx();
    let mut shell = shell_with_image(dir.path());
    let chord = chord_from_key(&WKey::Character("f".into()), ModifiersState::CONTROL).unwrap();
    assert_eq!(
        shell.editor.keymap().resolve_any(&chord),
        Some(Resolved::Menu(ui::MenuAction::CommandSearch))
    );
    press(&mut shell, ModifiersState::CONTROL, "f");
    for _ in 0..2 {
        frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    }
    assert!(
        matches!(
            shell.chrome.dialogs_for_test().active_for_test(),
            crate::dialog_host::ActiveDialog::CommandSearch(_)
        ),
        "Ctrl+F opens Find (Help > Search Commands)"
    );
}

#[test]
fn print_as_pdf_is_its_own_file_menu_row_and_writes_the_pdf() {
    let dir = tempfile::tempdir().unwrap();
    let rgba = vec![200u8; (SIDE * SIDE * 4) as usize];
    let png = dir.path().join("a.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let target = dir.path().join("printed.pdf");
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.path().join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new().exporting_to(target.clone())),
    );
    editor.open_path(&png).unwrap();
    let mut shell = Shell::new(editor, Vec::new());
    let file = ui::menu::menu_bar(0)
        .into_iter()
        .find(|m| m.title == "File")
        .expect("a File menu");
    assert!(
        file.actions().contains(&ui::MenuAction::PrintAsPdf),
        "File has a Print as PDF row"
    );
    let ctx = ctx();
    shell.perform_menu_chord(ui::MenuAction::PrintAsPdf);
    frame(&ctx, &mut shell, egui::Modifiers::NONE, Vec::new());
    let bytes = std::fs::read(&target).expect("Print as PDF wrote the file");
    assert!(bytes.starts_with(b"%PDF"));
}
