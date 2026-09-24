//! W11-F: the everyday painting and navigation gestures, driven through the
//! shell's own routes only — `on_modifiers` (what `WindowEvent::
//! ModifiersChanged` calls), `on_key` (what `KeyboardInput` calls),
//! `on_pointer` (what `MouseInput` / `CursorMoved` call) and `on_wheel` (what
//! `MouseWheel` calls). No test here sets a tool other than the Brush or
//! reaches past the shell into the pointer router.
//!
//! * Alt-click with the Brush samples the composite into the foreground and
//!   paints nothing;
//! * a Shift-click paints a straight line from where the last stroke ended;
//! * Ctrl held turns the Brush into the Move tool, and letting go gives the
//!   Brush back;
//! * Ctrl+Space / Alt+Space turn a click into Zoom In / Zoom Out at the point;
//! * Alt + wheel zooms even with the wheel set to pan.

use super::*;
use winit::keyboard::Key as WKey;

use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use tools::ToolId;

const SIDE: u32 = 100;
const RED: [u8; 3] = [230, 20, 30];

/// A white 100x100 image with a red 60..80 square, the Brush in hand with a
/// black foreground, the view at 100% centred on the image.
fn shell_with_red_square(dir: &std::path::Path) -> Shell {
    let mut rgba = vec![255u8; (SIDE * SIDE * 4) as usize];
    for y in 60..80u32 {
        for x in 60..80u32 {
            let i = ((y * SIDE + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&RED);
        }
    }
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
    editor.set_foreground([0.0, 0.0, 0.0, 1.0]);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    let doc = shell.editor.active_mut().unwrap();
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::new(SIDE as f32 / 2.0, SIDE as f32 / 2.0);
    shell
}

/// Where document point `doc` is drawn (the view is unrotated, unmirrored).
fn screen_of(shell: &Shell, doc: Vec2) -> Vec2 {
    let camera = &shell.editor.active().unwrap().camera;
    camera.viewport_center() + (doc - camera.center) * camera.zoom
}

/// One pointer sample at document point `doc`, as `window_event` delivers it.
fn point(shell: &mut Shell, phase: PointerPhase, doc: Vec2) {
    shell.cursor = screen_of(shell, doc);
    shell.on_pointer(phase, PointerButton::Primary, false);
}

/// A click (press and release, no drag) at document point `doc`.
fn click(shell: &mut Shell, doc: Vec2) {
    point(shell, PointerPhase::Down, doc);
    point(shell, PointerPhase::Up, doc);
}

fn composite(shell: &mut Shell) -> Vec<u8> {
    shell
        .editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()
}

fn rgb_at(rgba: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * SIDE + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn is_dark(px: [u8; 3]) -> bool {
    px.iter().all(|c| *c < 80)
}

fn is_white(px: [u8; 3]) -> bool {
    px.iter().all(|c| *c > 240)
}

fn depth(shell: &Shell) -> usize {
    shell.editor.active().unwrap().history_depth()
}

#[test]
fn alt_click_with_the_brush_picks_the_colour_and_paints_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());
    let before = composite(&mut shell);

    shell.on_modifiers(ModifiersState::ALT);
    assert_eq!(shell.editor.effective_tool(), ToolId::Eyedropper);
    click(&mut shell, Vec2::new(70.0, 70.0));

    // The same click with the Eyedropper itself selected, on a twin shell:
    // the Alt-click must have picked exactly what it picks.
    let twin_dir = tempfile::tempdir().unwrap();
    let mut twin = shell_with_red_square(twin_dir.path());
    twin.editor.set_tool(ToolId::Eyedropper);
    click(&mut twin, Vec2::new(70.0, 70.0));
    let want = twin.editor.foreground();
    assert!(
        want[0] > 0.5 && want[1] < 0.1 && want[2] < 0.1,
        "the Eyedropper picked {want:?}, not red"
    );
    assert_eq!(
        shell.editor.foreground(),
        want,
        "Alt-click did not pick the red under it"
    );
    assert_eq!(composite(&mut shell), before, "the Alt-click painted");
    assert_eq!(depth(&shell), 0, "the Alt-click made a history step");

    // Letting go of Alt gives the Brush back, which paints again.
    shell.on_modifiers(ModifiersState::empty());
    assert_eq!(shell.editor.effective_tool(), ToolId::Brush);
    assert_eq!(shell.editor.tool(), ToolId::Brush);
}

#[test]
fn a_shift_click_paints_a_straight_line_from_the_last_stroke() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());

    click(&mut shell, Vec2::new(20.0, 30.0));
    let first = composite(&mut shell);
    assert!(
        is_dark(rgb_at(&first, 20, 30)),
        "the first click painted nothing"
    );
    assert!(is_white(rgb_at(&first, 50, 30)));

    shell.on_modifiers(ModifiersState::SHIFT);
    assert_eq!(shell.editor.effective_tool(), ToolId::Brush);
    click(&mut shell, Vec2::new(80.0, 30.0));
    shell.on_modifiers(ModifiersState::empty());

    let after = composite(&mut shell);
    for x in [30, 40, 50, 60, 70, 80] {
        assert!(
            is_dark(rgb_at(&after, x, 30)),
            "the Shift-click left ({x}, 30) at {:?}: no line from the last stroke",
            rgb_at(&after, x, 30)
        );
    }
    // A line, not a smear: well off the row stays white.
    assert!(is_white(rgb_at(&after, 50, 60)));
    assert_eq!(depth(&shell), 2, "one step per click");
}

#[test]
fn holding_ctrl_turns_the_brush_into_move_and_letting_go_gives_it_back() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());
    let before = composite(&mut shell);
    assert_eq!(rgb_at(&before, 62, 70), RED);

    shell.on_modifiers(ModifiersState::CONTROL);
    assert_eq!(shell.editor.effective_tool(), ToolId::Move);
    assert_eq!(shell.editor.tool(), ToolId::Brush, "the selection moved");
    point(&mut shell, PointerPhase::Down, Vec2::new(70.0, 70.0));
    for x in [72.0, 76.0, 80.0] {
        point(&mut shell, PointerPhase::Move, Vec2::new(x, 70.0));
    }
    point(&mut shell, PointerPhase::Up, Vec2::new(80.0, 70.0));

    let after = composite(&mut shell);
    // The square moved 10px right: its old left edge is uncovered and red
    // now reaches past its old right edge. No black brush ink anywhere.
    assert_ne!(rgb_at(&after, 62, 70), RED, "the layer did not move");
    assert_eq!(rgb_at(&after, 85, 70), RED, "the layer did not move right");
    assert!(
        (0..SIDE * SIDE).all(|i| {
            let (x, y) = (i % SIDE, i / SIDE);
            !(is_dark(rgb_at(&after, x, y)) && after[(i * 4 + 3) as usize] > 192)
        }),
        "the Ctrl-drag painted with the brush"
    );

    shell.on_modifiers(ModifiersState::empty());
    assert_eq!(shell.editor.effective_tool(), ToolId::Brush);
    click(&mut shell, Vec2::new(20.0, 20.0));
    assert!(
        is_dark(rgb_at(&composite(&mut shell), 20, 20)),
        "after Ctrl came up the Brush did not paint"
    );
}

#[test]
fn ctrl_space_click_zooms_in_at_the_point_and_alt_space_zooms_out() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());
    let owner = KeyboardOwner::default();
    let before = composite(&mut shell);
    let target = Vec2::new(70.0, 30.0);
    let at = screen_of(&shell, target);

    // Ctrl down, then Space: the chord the keymap does not bind.
    shell.on_modifiers(ModifiersState::CONTROL);
    shell.on_key(
        owner,
        &WKey::Named(NamedKey::Space),
        ElementState::Pressed,
        false,
    );
    assert_eq!(shell.editor.effective_tool(), ToolId::Zoom);
    let zoom = shell.editor.active().unwrap().camera.zoom;
    shell.cursor = at;
    shell.on_pointer(PointerPhase::Down, PointerButton::Primary, false);
    shell.on_pointer(PointerPhase::Up, PointerButton::Primary, false);
    let camera = shell.editor.active().unwrap().camera.clone();
    assert!(
        camera.zoom > zoom * 1.2,
        "Ctrl+Space click did not zoom in: {zoom} -> {}",
        camera.zoom
    );
    let under = camera.screen_to_image(at);
    assert!(
        (under - target).length() < 1.0,
        "the zoom was not about the click: {under:?} is under it now"
    );

    // Alt instead of Ctrl, Space still held: the click zooms back out.
    shell.on_modifiers(ModifiersState::ALT);
    assert_eq!(shell.editor.effective_tool(), ToolId::Zoom);
    let zoomed = camera.zoom;
    shell.on_pointer(PointerPhase::Down, PointerButton::Primary, false);
    shell.on_pointer(PointerPhase::Up, PointerButton::Primary, false);
    assert!(shell.editor.active().unwrap().camera.zoom < zoomed * 0.9);

    // Space up, Alt up: the Brush again, and nothing was painted.
    shell.on_key(
        owner,
        &WKey::Named(NamedKey::Space),
        ElementState::Released,
        false,
    );
    shell.on_modifiers(ModifiersState::empty());
    assert_eq!(shell.editor.effective_tool(), ToolId::Brush);
    assert_eq!(composite(&mut shell), before);
    assert_eq!(depth(&shell), 0);
}

#[test]
fn alt_wheel_zooms_with_the_wheel_set_to_pan() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());
    let mut prefs = shell.editor.ui_preferences();
    prefs.tools.scroll_wheel_zooms = false;
    shell.apply_chrome(ChromeOutput {
        set_ui_preferences: Some(Box::new(prefs)),
        ..ChromeOutput::default()
    });
    assert!(!shell.editor.preferences().scroll_wheel_zooms);
    let target = Vec2::new(30.0, 40.0);
    shell.cursor = screen_of(&shell, target);
    let zoom = shell.editor.active().unwrap().camera.zoom;

    shell.on_modifiers(ModifiersState::ALT);
    shell.on_wheel(Vec2::new(0.0, 1.0));
    let camera = shell.editor.active().unwrap().camera.clone();
    assert!(camera.zoom > zoom, "Alt + wheel did not zoom");
    assert!(
        (camera.screen_to_image(shell.cursor) - target).length() < 0.5,
        "Alt + wheel did not zoom about the pointer"
    );
}

/// A tool picked with Ctrl still down (Ctrl+T's Free Transform) is the tool
/// that acts: a lend computed for the tool before it never shadows it.
#[test]
fn a_tool_picked_with_ctrl_held_is_not_shadowed_by_the_old_lend() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_red_square(dir.path());
    shell.on_modifiers(ModifiersState::CONTROL);
    assert_eq!(shell.editor.effective_tool(), ToolId::Move);
    shell.editor.set_tool(ToolId::FreeTransform);
    assert_eq!(shell.editor.effective_tool(), ToolId::FreeTransform);
    shell.on_modifiers(ModifiersState::empty());
    assert_eq!(shell.editor.effective_tool(), ToolId::FreeTransform);
}
