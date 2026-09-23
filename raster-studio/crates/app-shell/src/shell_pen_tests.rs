//! W7-A: pen/touch input driven through the shell's own route — real winit
//! `WindowEvent`s (`Touch`, `MouseInput`, `CursorMoved`, `Focused`) handed to
//! `on_pointer_window_event`, the method `window_event` delegates them to
//! after egui has seen them. The events are synthetic; a physical pen is
//! still needed to confirm the OS's order.

use super::*;
use winit::event::{DeviceId, Force, TouchPhase};

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const SIDE: u32 = 32;

/// One opaque grey 32x32 image in a 200x160 window at 100%, centred — screen
/// `(100, 80)` is document `(16, 16)` — with the Brush selected, painting
/// red, size 12, Size from Pressure on.
fn shell_with_brush(dir: &std::path::Path) -> Shell {
    let png = dir.join("a.png");
    std::fs::write(
        &png,
        raster::encode(
            raster::ExportFormat::Png,
            SIDE,
            SIDE,
            &vec![200u8; (SIDE * SIDE * 4) as usize],
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
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(200.0, 160.0));
    let doc = shell.editor.active_mut().unwrap();
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::new(16.0, 16.0);
    shell.editor.set_tool(tools::ToolId::Brush);
    shell.editor.set_foreground([1.0, 0.0, 0.0, 1.0]);
    shell
        .chrome
        .set_tool_option(tools::ToolId::Brush, "size", ui::OptionValue::Float(12.0));
    shell.chrome.set_tool_option(
        tools::ToolId::Brush,
        "size_pressure",
        ui::OptionValue::Bool(true),
    );
    shell
}

/// The window position of document point `doc`.
fn at(doc: Vec2) -> PhysicalPosition<f64> {
    let screen = Vec2::new(100.0, 80.0) + doc - Vec2::new(16.0, 16.0);
    PhysicalPosition::new(screen.x as f64, screen.y as f64)
}

fn touch_id(shell: &mut Shell, id: u64, phase: TouchPhase, doc: Vec2, force: Option<f64>) {
    shell.on_pointer_window_event(
        WindowEvent::Touch(winit::event::Touch {
            device_id: DeviceId::dummy(),
            phase,
            location: at(doc),
            force: force.map(Force::Normalized),
            id,
        }),
        false,
    );
}

fn touch(shell: &mut Shell, phase: TouchPhase, doc: Vec2, force: Option<f64>) {
    touch_id(shell, 7, phase, doc, force);
}

fn mouse_button(shell: &mut Shell, state: ElementState) {
    shell.on_pointer_window_event(
        WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state,
            button: MouseButton::Left,
        },
        false,
    );
}

fn cursor_to(shell: &mut Shell, doc: Vec2) {
    shell.on_pointer_window_event(
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: at(doc),
        },
        false,
    );
}

/// Whether document pixel `(x, y)` was painted red.
fn painted(rgba: &[u8], x: u32, y: u32) -> bool {
    let i = ((y * SIDE + x) * 4) as usize;
    rgba[i] > 230 && rgba[i + 1] < 150
}

fn composite(shell: &mut Shell) -> Vec<u8> {
    let doc = shell.editor.active_mut().unwrap();
    let rect = doc.canvas_rect();
    doc.composite(rect).unwrap()
}

fn painted_count(shell: &mut Shell) -> usize {
    let rgba = composite(shell);
    (0..SIDE)
        .flat_map(|y| (0..SIDE).map(move |x| (x, y)))
        .filter(|&(x, y)| painted(&rgba, x, y))
        .count()
}

/// A single tap at the centre with a normalized force.
fn tap_painted(force: f64) -> usize {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_brush(dir.path());
    let c = Vec2::new(16.0, 16.0);
    touch(&mut shell, TouchPhase::Started, c, Some(force));
    touch(&mut shell, TouchPhase::Ended, c, Some(force));
    assert_eq!(
        shell.pen_pressure, 1.0,
        "a lifted contact returns the pointer to mouse (full) pressure"
    );
    painted_count(&mut shell)
}

#[test]
fn a_touch_force_reaches_the_stroke_as_the_dab_radius() {
    let light = tap_painted(0.2);
    let full = tap_painted(1.0);
    assert!(light > 0, "a light touch painted nothing");
    assert!(
        light < full,
        "force 0.2 must land a smaller dab than force 1.0: {light} vs {full} pixels"
    );
}

#[test]
fn a_touch_stroke_is_one_history_entry() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_brush(dir.path());
    let before = shell.editor.active().unwrap().history_depth();
    touch(
        &mut shell,
        TouchPhase::Started,
        Vec2::new(6.0, 16.0),
        Some(0.5),
    );
    for x in [10.0, 14.0, 18.0, 22.0] {
        touch(&mut shell, TouchPhase::Moved, Vec2::new(x, 16.0), Some(0.8));
    }
    touch(
        &mut shell,
        TouchPhase::Ended,
        Vec2::new(26.0, 16.0),
        Some(0.8),
    );
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        before + 1,
        "a touch stroke must be exactly one undoable step"
    );
    let rgba = composite(&mut shell);
    assert!(
        painted(&rgba, 16, 16),
        "the stroke did not reach the canvas"
    );
}

#[test]
fn mouse_events_during_a_touch_are_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_brush(dir.path());
    let before = shell.editor.active().unwrap().history_depth();
    touch(
        &mut shell,
        TouchPhase::Started,
        Vec2::new(16.0, 26.0),
        Some(1.0),
    );
    // The OS's emulated mouse for the same contact: a press and a move to
    // the top-left corner. Were they routed, the stroke would reach (2, 2).
    mouse_button(&mut shell, ElementState::Pressed);
    cursor_to(&mut shell, Vec2::new(2.0, 2.0));
    touch(
        &mut shell,
        TouchPhase::Moved,
        Vec2::new(18.0, 26.0),
        Some(1.0),
    );
    touch(
        &mut shell,
        TouchPhase::Ended,
        Vec2::new(20.0, 26.0),
        Some(1.0),
    );
    // The emulated release arrives after the contact ended: still dropped.
    mouse_button(&mut shell, ElementState::Released);
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        before + 1,
        "the emulated mouse made a second step (double-counted input)"
    );
    let rgba = composite(&mut shell);
    assert!(painted(&rgba, 18, 26), "the touch stroke did not paint");
    assert!(
        !painted(&rgba, 2, 2),
        "a mouse move during the touch steered the stroke"
    );
    // After the contact, the real mouse routes again.
    cursor_to(&mut shell, Vec2::new(3.0, 3.0));
    mouse_button(&mut shell, ElementState::Pressed);
    mouse_button(&mut shell, ElementState::Released);
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        before + 2,
        "the mouse no longer paints after a touch"
    );
}

#[test]
fn losing_focus_mid_contact_frees_the_mouse_and_new_contacts() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_brush(dir.path());
    // A light contact goes down, and its lift is lost: the window loses focus
    // (Alt+Tab, a system prompt) and winit on Windows never reports it
    // cancelled.
    touch(
        &mut shell,
        TouchPhase::Started,
        Vec2::new(16.0, 16.0),
        Some(0.2),
    );
    shell.on_pointer_window_event(WindowEvent::Focused(false), false);
    assert!(!shell.pen.is_active(), "the lost contact is still down");
    assert_eq!(
        shell.pen_pressure, 1.0,
        "the lost contact's pressure outlived it"
    );
    let after_focus = shell.editor.active().unwrap().history_depth();
    // The mouse paints again, at full pressure.
    cursor_to(&mut shell, Vec2::new(4.0, 4.0));
    mouse_button(&mut shell, ElementState::Pressed);
    cursor_to(&mut shell, Vec2::new(8.0, 4.0));
    mouse_button(&mut shell, ElementState::Released);
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        after_focus + 1,
        "the mouse is still swallowed after focus loss"
    );
    let rgba = composite(&mut shell);
    assert!(painted(&rgba, 6, 4), "the mouse stroke did not paint");
    // And a new contact (a different id) is routed.
    touch_id(
        &mut shell,
        8,
        TouchPhase::Started,
        Vec2::new(24.0, 26.0),
        None,
    );
    touch_id(
        &mut shell,
        8,
        TouchPhase::Ended,
        Vec2::new(24.0, 26.0),
        None,
    );
    assert_eq!(
        shell.editor.active().unwrap().history_depth(),
        after_focus + 2,
        "a new contact is ignored after focus loss"
    );
}
