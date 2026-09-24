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

// ---- W8-A: Opacity from Pressure, a pen's zero-pressure touch-down, and the
// brush ring following a hovering pen ----

/// The brush from [`shell_with_brush`] with Size from Pressure off (every dab
/// the same size) and Opacity from Pressure on, both set through the options
/// bar the way a user sets them.
fn shell_with_opacity_pressure(dir: &std::path::Path) -> Shell {
    let mut shell = shell_with_brush(dir);
    brush_option(&mut shell, "size_pressure", false);
    brush_option(&mut shell, "opacity_pressure", true);
    shell
}

/// Flip one of the Brush's options-bar switches the way its checkbox does:
/// the intent the control emits, harvested by a real chrome frame, whose
/// `set_brush` the shell applies (`brush_from_options`).
fn brush_option(shell: &mut Shell, key: &'static str, on: bool) {
    shell.chrome.emit(ui::Intent::SetToolOption {
        tool: tools::ToolId::Brush,
        key,
        value: ui::OptionValue::Bool(on),
    });
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut out = crate::chrome::ChromeOutput::default();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
    let brush = shell.editor.brush_for(tools::ToolId::Brush);
    let held = match key {
        "opacity_pressure" => brush.opacity_pressure,
        "size_pressure" => brush.size_pressure,
        other => unreachable!("not a switch this file flips: {other}"),
    };
    assert_eq!(held, on, "the options bar's {key} did not reach the brush");
}

/// The alpha the paint landed with at document pixel `(x, y)`, read back
/// from the alpha channel: the image under it is `200/255` opaque, and a
/// source-over dab of alpha `a` leaves `a + (1 - a) * 200/255`, which is
/// linear in `a` whatever colour space the channels composite in.
fn paint_alpha(rgba: &[u8], x: u32, y: u32) -> f32 {
    let out = rgba[((y * SIDE + x) * 4 + 3) as usize] as f32;
    (out - 200.0) / (255.0 - 200.0)
}

/// One tap at document `at` from contact `id`.
fn tap(shell: &mut Shell, id: u64, at: Vec2, force: Option<f64>) {
    touch_id(shell, id, TouchPhase::Started, at, force);
    touch_id(shell, id, TouchPhase::Ended, at, force);
}

#[test]
fn opacity_from_pressure_scales_the_dab_alpha_through_the_shell() {
    let c = Vec2::new(16.0, 16.0);
    let alpha_of = |force: f64| {
        let dir = tempfile::tempdir().unwrap();
        let mut shell = shell_with_opacity_pressure(dir.path());
        tap(&mut shell, 7, c, Some(force));
        paint_alpha(&composite(&mut shell), 16, 16)
    };
    let full = alpha_of(1.0);
    let quarter = alpha_of(0.25);
    assert!(full > 0.95, "a full-pressure tap is opaque: {full}");
    let ratio = quarter / full;
    assert!(
        (ratio - 0.25).abs() < 0.03,
        "force 0.25 must lay ~25% of the full-pressure alpha: {quarter} vs {full} ({ratio})"
    );
    // Off, the same light tap is opaque: the switch is what scales it.
    let dir = tempfile::tempdir().unwrap();
    let mut off = shell_with_opacity_pressure(dir.path());
    brush_option(&mut off, "opacity_pressure", false);
    tap(&mut off, 7, c, Some(0.25));
    let unscaled = paint_alpha(&composite(&mut off), 16, 16);
    assert!(unscaled > 0.95, "Opacity from Pressure off: {unscaled}");
}

#[test]
fn a_pen_touching_down_at_zero_pressure_lays_a_zero_weight_first_dab() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_opacity_pressure(dir.path());
    // The pen has reported forces before (an earlier stroke, far away).
    tap(&mut shell, 7, Vec2::new(4.0, 4.0), Some(0.8));
    assert!(paint_alpha(&composite(&mut shell), 4, 4) > 0.5);
    // Its next contact touches down at zero pressure: winit reports no force.
    tap(&mut shell, 7, Vec2::new(24.0, 24.0), None);
    let alpha = paint_alpha(&composite(&mut shell), 24, 24);
    assert!(
        alpha < 0.01,
        "a pen's zero-pressure touch-down painted at full pressure: {alpha}"
    );
    // A finger (an id that never reported a force) is still full pressure,
    // even after the pen has been seen.
    tap(&mut shell, 9, Vec2::new(24.0, 8.0), None);
    let finger = paint_alpha(&composite(&mut shell), 24, 8);
    assert!(
        finger > 0.95,
        "a finger touch must paint at full pressure: {finger}"
    );
}

#[test]
fn fingers_held_through_a_focus_loss_are_not_read_as_a_hovering_pen() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_opacity_pressure(dir.path());
    // A finger (5) drives, a second finger (6) is down and ignored, and the
    // window loses focus with both still on the glass.
    touch_id(
        &mut shell,
        5,
        TouchPhase::Started,
        Vec2::new(4.0, 4.0),
        None,
    );
    touch_id(
        &mut shell,
        6,
        TouchPhase::Started,
        Vec2::new(8.0, 8.0),
        None,
    );
    shell.on_pointer_window_event(WindowEvent::Focused(false), false);
    let cursor = shell.cursor;
    // Both keep moving: neither moves the pointer as a hovering pen would.
    touch_id(
        &mut shell,
        5,
        TouchPhase::Moved,
        Vec2::new(20.0, 20.0),
        None,
    );
    touch_id(
        &mut shell,
        6,
        TouchPhase::Moved,
        Vec2::new(22.0, 22.0),
        None,
    );
    assert_eq!(shell.cursor, cursor, "a lost finger's move was a hover");
    assert!(
        !shell.pen.is_pen(5) && !shell.pen.is_pen(6),
        "a lost finger was marked a pen"
    );
    touch_id(
        &mut shell,
        5,
        TouchPhase::Ended,
        Vec2::new(20.0, 20.0),
        None,
    );
    touch_id(
        &mut shell,
        6,
        TouchPhase::Ended,
        Vec2::new(22.0, 22.0),
        None,
    );
    // Later fingers reusing those ids (Windows reuses contact ids) still
    // paint at full pressure with Opacity from Pressure on.
    tap(&mut shell, 5, Vec2::new(24.0, 8.0), None);
    tap(&mut shell, 6, Vec2::new(24.0, 24.0), None);
    let rgba = composite(&mut shell);
    for (x, y) in [(24, 8), (24, 24)] {
        let alpha = paint_alpha(&rgba, x, y);
        assert!(
            alpha > 0.95,
            "a finger reusing a lost id painted below full pressure at ({x}, {y}): {alpha}"
        );
    }
}

/// The window is 1400x900 physical pixels at scale 1; the 32x32 image at
/// zoom 4 is centred on the screen.
const WIDE: Vec2 = Vec2::new(1400.0, 900.0);
const RING_ZOOM: f32 = 4.0;

fn shell_for_ring(dir: &std::path::Path) -> Shell {
    let mut shell = shell_with_brush(dir);
    shell.spread_viewport(WIDE);
    let doc = shell.editor.active_mut().unwrap();
    doc.camera.zoom = RING_ZOOM;
    doc.camera.center = Vec2::new(16.0, 16.0);
    shell
}

/// One chrome frame with no egui pointer events at all: whatever position
/// the ring is drawn at came from the shell's pen route, not egui's pointer.
fn chrome_frame(shell: &mut Shell, ctx: &egui::Context) -> egui::FullOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(WIDE.x, WIDE.y),
        )),
        ..Default::default()
    };
    let mut out = crate::chrome::ChromeOutput::default();
    let full = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
    full
}

/// The centre and mean radius of every closed brush-outline ring painted in
/// the ring's over-colour.
fn ring_centres(full: &egui::FullOutput, ctx: &egui::Context) -> Vec<(egui::Pos2, f32)> {
    let over = ui::canvas::CanvasStyle::from_context(ctx).brush_ring_over;
    full.shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Path(p)
                if p.closed
                    && p.points.len() == ui::canvas::brush_cursor::OUTLINE_SEGMENTS
                    && p.stroke.color == egui::epaint::ColorMode::Solid(over) =>
            {
                let n = p.points.len() as f32;
                let sum = p
                    .points
                    .iter()
                    .fold(egui::Vec2::ZERO, |a, q| a + q.to_vec2());
                let centre = (sum / n).to_pos2();
                let r = p.points.iter().map(|q| (*q - centre).length()).sum::<f32>() / n;
                Some((centre, r))
            }
            _ => None,
        })
        .collect()
}

fn hover_touch(shell: &mut Shell, id: u64, at: egui::Pos2) {
    shell.on_pointer_window_event(
        WindowEvent::Touch(winit::event::Touch {
            device_id: DeviceId::dummy(),
            phase: TouchPhase::Moved,
            location: PhysicalPosition::new(at.x as f64, at.y as f64),
            force: None,
            id,
        }),
        false,
    );
}

#[test]
fn the_brush_ring_follows_a_hovering_pen() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_for_ring(dir.path());
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    for _ in 0..3 {
        let _ = chrome_frame(&mut shell, &ctx);
    }
    // The Brush's full-pressure dab at the view's zoom.
    let ring_radius = shell.editor.brush_for(tools::ToolId::Brush).size * RING_ZOOM / 2.0;
    let near = |rings: &[(egui::Pos2, f32)], at: egui::Pos2| {
        rings
            .iter()
            .any(|(c, r)| (*c - at).length() < 0.5 && (r - ring_radius).abs() < 0.5)
    };
    // Nothing points at the canvas yet: no ring.
    let idle = chrome_frame(&mut shell, &ctx);
    assert!(ring_centres(&idle, &ctx).is_empty());
    // A pen hovering in range: winit's Touch `Moved` with nothing down.
    for at in [egui::pos2(700.0, 450.0), egui::pos2(660.0, 480.0)] {
        hover_touch(&mut shell, 3, at);
        assert_eq!(
            shell.cursor,
            Vec2::new(at.x, at.y),
            "the hover moved the pointer"
        );
        assert!(!shell.pen.is_active(), "a hover is not a contact");
        let full = chrome_frame(&mut shell, &ctx);
        let rings = ring_centres(&full, &ctx);
        assert!(
            near(&rings, at),
            "the ring must sit on the hovering pen at {at:?}: {rings:?} {:?}",
            shell.chrome.extras_report()
        );
        assert!(shell.chrome.extras_report().brush_ring);
    }
    // The hover laid no paint.
    assert_eq!(painted_count(&mut shell), 0);
    // The mouse moves: the ring is egui's pointer's again, not the pen's.
    shell.on_pointer_window_event(
        WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(500.0, 300.0),
        },
        false,
    );
    let after = chrome_frame(&mut shell, &ctx);
    assert!(
        !near(&ring_centres(&after, &ctx), egui::pos2(660.0, 480.0)),
        "the ring stayed on the pen after the mouse moved"
    );
}
