//! W5-C: the transform sessions that must be on screen before any canvas
//! click, driven through the shell's own routes — the key path
//! `window_event` feeds (`on_key`), a real chrome frame (`Chrome::ui`) and
//! `apply_chrome` for what that frame meant. No test here ever calls
//! `on_pointer`: the claim is that no pointer sample is needed.

use super::*;
use winit::keyboard::Key as WKey;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

/// One 100x100 image, 20..40 square selected, Brush in hand.
fn shell_with_selection(dir: &std::path::Path) -> Shell {
    let png = dir.join("a.png");
    std::fs::write(
        &png,
        raster::encode(
            raster::ExportFormat::Png,
            100,
            100,
            &vec![200u8; 100 * 100 * 4],
        )
        .unwrap(),
    )
    .unwrap();
    let mut editor = crate::editor::Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(20, 20),
        max: glam::IVec2::new(40, 40),
    };
    editor.set_tool(tools::ToolId::Brush);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    shell
}

/// One chrome frame, and the shell performing what it meant — exactly the
/// two steps the render loop takes (`chrome.ui` then `apply_chrome`).
fn frame(ctx: &egui::Context, shell: &mut Shell) {
    let mut out = crate::chrome::ChromeOutput::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

fn published_source(shell: &Shell) -> Option<raster::PixelRect> {
    shell
        .chrome
        .workspace()
        .canvas
        .sessions
        .transform
        .as_ref()
        .map(|(state, _)| state.source)
}

fn ctrl(shell: &mut Shell, c: &str) {
    shell.modifiers = ModifiersState::CONTROL;
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Character(c.into()),
        ElementState::Pressed,
        false,
    );
    shell.modifiers = ModifiersState::empty();
}

fn escape(shell: &mut Shell) {
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Named(NamedKey::Escape),
        ElementState::Pressed,
        false,
    );
}

#[test]
fn ctrl_t_from_the_keyboard_publishes_the_gizmo_with_no_pointer_sample() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_selection(dir.path());
    let ctx = ctx();
    frame(&ctx, &mut shell);
    assert_eq!(published_source(&shell), None, "precondition: no gizmo");

    ctrl(&mut shell, "t");
    // The chord is emitted into the chrome; the next frame performs it.
    frame(&ctx, &mut shell);
    assert_eq!(shell.editor.tool(), tools::ToolId::FreeTransform);
    assert_eq!(
        published_source(&shell),
        Some(raster::PixelRect::new(20, 20, 20, 20)),
        "Ctrl+T published its gizmo over the selection in the frame that performed it"
    );
}

#[test]
fn a_transform_menu_mode_item_publishes_the_gizmo_and_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_selection(dir.path());
    let ctx = ctx();
    frame(&ctx, &mut shell);
    // Edit > Transform > Rotate, as the menu bar emits it.
    shell
        .chrome
        .emit(ui::Intent::Action(ui::MenuAction::Transform(
            ui::menu::TransformOp::Rotate,
        )));
    frame(&ctx, &mut shell);
    assert_eq!(shell.editor.tool(), tools::ToolId::FreeTransform);
    let (state, mode) = shell
        .chrome
        .workspace()
        .canvas
        .sessions
        .transform
        .clone()
        .expect("Edit > Transform > Rotate published its gizmo with no click");
    assert_eq!(state.source, raster::PixelRect::new(20, 20, 20, 20));
    assert_eq!(
        mode,
        tools::transform::TransformMode::Rotate,
        "the item's mode reached the options bar and the session"
    );
}

#[test]
fn escape_after_ctrl_t_hands_the_palette_back_and_leaves_nothing_stale() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_selection(dir.path());
    shell.editor.set_tool(tools::ToolId::RectMarquee);
    let ctx = ctx();
    frame(&ctx, &mut shell);
    ctrl(&mut shell, "t");
    frame(&ctx, &mut shell);
    assert!(published_source(&shell).is_some(), "precondition: gizmo up");

    escape(&mut shell);
    assert_eq!(
        shell.editor.tool(),
        tools::ToolId::RectMarquee,
        "Escape returns to the tool Ctrl+T was pressed from"
    );
    assert_eq!(published_source(&shell), None, "Escape took the gizmo down");

    // Later: Free Transform picked from the palette with the Brush in hand,
    // a session begun, and Enter. The palette must not jump to the stale
    // RectMarquee of the abandoned Ctrl+T.
    shell.editor.set_tool(tools::ToolId::Brush);
    frame(&ctx, &mut shell);
    shell.editor.set_tool(tools::ToolId::FreeTransform);
    frame(&ctx, &mut shell);
    crate::tool_input::request_free_transform(tools::ToolId::FreeTransform);
    frame(&ctx, &mut shell);
    assert!(
        shell.pointer.has_pending_commit(),
        "precondition: a session"
    );
    let out = shell.pointer.commit(&mut shell.editor);
    assert_eq!(out.failed, None, "{out:?}");
    assert_eq!(
        shell.editor.tool(),
        tools::ToolId::FreeTransform,
        "a palette-picked Free Transform stays put after Enter"
    );
}

#[test]
fn ticking_show_transform_controls_publishes_the_box_with_no_pointer_sample() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_selection(dir.path());
    shell.editor.active_mut().unwrap().document.selection = editor_core::Selection::None;
    shell.editor.set_tool(tools::ToolId::Move);
    let ctx = ctx();
    frame(&ctx, &mut shell);
    assert_eq!(published_source(&shell), None, "precondition: no box");
    shell.chrome.set_tool_option(
        tools::ToolId::Move,
        "show_transform",
        ui::OptionValue::Bool(true),
    );
    frame(&ctx, &mut shell);
    assert_eq!(
        published_source(&shell),
        Some(raster::PixelRect::new(0, 0, 100, 100)),
        "the ticked option framed the layer in the next frame"
    );
    shell.chrome.set_tool_option(
        tools::ToolId::Move,
        "show_transform",
        ui::OptionValue::Bool(false),
    );
    frame(&ctx, &mut shell);
    assert_eq!(published_source(&shell), None, "unticked: the box goes");
}
