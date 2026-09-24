//! W9-L: Free Transform's numeric options bar and Warp presets, driven
//! through the shell's own routes only — the key path `window_event` feeds
//! (`on_key`: Ctrl+T, Enter), real chrome frames (`Chrome::ui`) with real
//! egui pointer events on the options bar's fields, and `apply_chrome` for
//! what each frame meant. No test here calls `on_pointer`: the claim is that
//! a typed field reshapes the LIVE quad with no canvas press, and that Enter
//! commits what the fields say.

use super::*;
use winit::keyboard::Key as WKey;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use tools::transform::{keys, NumericTransform, TransformMode, REFERENCE_CENTRE};
use tools::ToolId;

const SIDE: u32 = 100;

/// A white 100x100 image with a black 20..40 square, that square selected,
/// the Brush in hand.
fn shell_with_black_square(dir: &std::path::Path) -> Shell {
    let mut rgba = vec![255u8; (SIDE * SIDE * 4) as usize];
    for y in 20..40u32 {
        for x in 20..40u32 {
            let i = ((y * SIDE + x) * 4) as usize;
            rgba[i..i + 3].copy_from_slice(&[0, 0, 0]);
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
    editor.active_mut().unwrap().document.selection = editor_core::Selection::Rect {
        min: glam::IVec2::new(20, 20),
        max: glam::IVec2::new(40, 40),
    };
    editor.set_tool(ToolId::Brush);
    let mut shell = Shell::new(editor, Vec::new());
    shell.spread_viewport(Vec2::new(400.0, 300.0));
    shell
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

/// One chrome frame carrying `events`, and the shell performing what it
/// meant — the two steps the render loop takes (`chrome.ui`, `apply_chrome`).
fn frame(ctx: &egui::Context, shell: &mut Shell, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(4000.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = crate::chrome::ChromeOutput::default();
    let _ = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
}

fn button(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn rect_of(ctx: &egui::Context, shell: &mut Shell, id: egui::Id) -> egui::Rect {
    frame(ctx, shell, Vec::new());
    ctx.read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was not drawn"))
        .rect
}

/// Drag the options bar's field `key` by `by` points, a frame per step.
fn drag_field(ctx: &egui::Context, shell: &mut Shell, key: &'static str, by: egui::Vec2) {
    let id = ui::view::ids::tool_option(ToolId::FreeTransform, key);
    let from = rect_of(ctx, shell, id).center();
    frame(
        ctx,
        shell,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    for step in 1..=4 {
        let at = from + by * (step as f32 / 4.0);
        frame(ctx, shell, vec![egui::Event::PointerMoved(at)]);
    }
    frame(
        ctx,
        shell,
        vec![
            egui::Event::PointerMoved(from + by),
            button(from + by, false),
        ],
    );
    frame(ctx, shell, vec![egui::Event::PointerGone]);
}

fn click(ctx: &egui::Context, shell: &mut Shell, id: egui::Id) {
    let at = rect_of(ctx, shell, id).center();
    frame(
        ctx,
        shell,
        vec![
            egui::Event::PointerMoved(at),
            button(at, true),
            button(at, false),
        ],
    );
}

fn ctrl_t(shell: &mut Shell) {
    shell.modifiers = ModifiersState::CONTROL;
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Character("t".into()),
        ElementState::Pressed,
        false,
    );
    shell.modifiers = ModifiersState::empty();
}

fn enter(shell: &mut Shell) {
    shell.on_key(
        KeyboardOwner::default(),
        &WKey::Named(NamedKey::Enter),
        ElementState::Pressed,
        false,
    );
}

/// The published quad, read back at the centre reference as the bar reads it.
fn published(
    shell: &Shell,
) -> Option<(
    NumericTransform,
    tools::transform::TransformState,
    TransformMode,
)> {
    shell
        .chrome
        .workspace()
        .canvas
        .sessions
        .transform
        .clone()
        .map(|(state, mode)| {
            (
                NumericTransform::read(&state, REFERENCE_CENTRE),
                state,
                mode,
            )
        })
}

fn held_float(shell: &Shell, key: &str) -> f32 {
    shell
        .chrome
        .tool_options(ToolId::FreeTransform)
        .into_iter()
        .find(|(k, _)| k == key)
        .and_then(|(_, v)| v.as_float())
        .unwrap_or_else(|| panic!("the bar holds no {key}"))
}

fn composite(shell: &mut Shell) -> Vec<u8> {
    shell
        .editor
        .active_mut()
        .unwrap()
        .composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()
}

/// Black pixels along row `y` of a composite.
fn black_in_row(rgba: &[u8], y: u32) -> usize {
    (0..SIDE)
        .filter(|x| {
            let i = ((y * SIDE + x) * 4) as usize;
            rgba[i] < 64 && rgba[i + 3] > 192
        })
        .count()
}

/// Ctrl+T over the selected square, the gizmo published, no pointer sample.
fn begin(ctx: &egui::Context, shell: &mut Shell) {
    frame(ctx, shell, Vec::new());
    ctrl_t(shell);
    frame(ctx, shell, Vec::new());
    assert_eq!(shell.editor.tool(), ToolId::FreeTransform);
    let (n, state, _) = published(shell).expect("precondition: Ctrl+T published the gizmo");
    assert_eq!(state.source, raster::PixelRect::new(20, 20, 20, 20));
    assert!((n.w - 100.0).abs() < 1e-3 && (n.h - 100.0).abs() < 1e-3);
}

#[test]
fn typed_w_then_h_reshape_the_live_quad_with_no_press_and_enter_commits_both() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_black_square(dir.path());
    let ctx = ctx();
    let original = composite(&mut shell);
    assert_eq!(black_in_row(&original, 30), 20);
    begin(&ctx, &mut shell);

    // W, dragged left in the options bar: the published quad follows at
    // once — no canvas press, no pointer sample.
    drag_field(&ctx, &mut shell, keys::W, egui::vec2(-60.0, 0.0));
    let typed_w = held_float(&shell, keys::W);
    assert!(typed_w < 90.0, "the drag shrank W: {typed_w}");
    let (n, _, _) = published(&shell).unwrap();
    assert!(
        (n.w - typed_w).abs() < 1e-3,
        "the live quad's W {} is the typed {typed_w}",
        n.w
    );
    assert!((n.h - 100.0).abs() < 1e-3, "H untouched: {}", n.h);
    assert!(
        shell.pointer.has_pending_commit(),
        "the session is still live"
    );

    // Then H: the first edit survives the second.
    drag_field(&ctx, &mut shell, keys::H, egui::vec2(-60.0, 0.0));
    let typed_h = held_float(&shell, keys::H);
    assert!(typed_h < 90.0, "the drag shrank H: {typed_h}");
    assert!(
        (held_float(&shell, keys::W) - typed_w).abs() < 1e-3,
        "the H edit kept the typed W"
    );
    let (n, _, _) = published(&shell).unwrap();
    assert!((n.w - typed_w).abs() < 1e-3, "W kept: {} vs {typed_w}", n.w);
    assert!(
        (n.h - typed_h).abs() < 1e-3,
        "H landed: {} vs {typed_h}",
        n.h
    );

    // Enter commits what the fields say.
    enter(&mut shell);
    assert!(
        !shell.pointer.has_pending_commit(),
        "Enter ended the session"
    );
    let after = composite(&mut shell);
    let row = black_in_row(&after, 30);
    let expected = (20.0 * typed_w / 100.0).round() as i64;
    assert!(
        (row as i64 - expected).abs() <= 2,
        "row 30 holds {row} black pixels; W {typed_w}% of 20 is {expected}"
    );
    let col = (0..SIDE)
        .filter(|y| {
            let i = ((y * SIDE + 30) * 4) as usize;
            after[i] < 64 && after[i + 3] > 192
        })
        .count() as i64;
    let expected = (20.0 * typed_h / 100.0).round() as i64;
    assert!(
        (col - expected).abs() <= 2,
        "column 30 holds {col} black pixels; H {typed_h}% of 20 is {expected}"
    );
}

#[test]
fn enter_with_no_typed_field_commits_the_untouched_square() {
    // The control for the test above: the same route with no edit leaves
    // the square as it was, so the change there is the typed fields'.
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_black_square(dir.path());
    let ctx = ctx();
    begin(&ctx, &mut shell);
    enter(&mut shell);
    let after = composite(&mut shell);
    assert_eq!(black_in_row(&after, 30), 20);
}

#[test]
fn a_picked_warp_preset_bends_the_live_quad_with_no_press_and_enter_resamples_it() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_black_square(dir.path());
    let ctx = ctx();
    let original = composite(&mut shell);
    begin(&ctx, &mut shell);
    let (_, state, _) = published(&shell).unwrap();
    assert!(state.mesh.is_none(), "precondition: no mesh");

    let arc = tools::transform::WARP_PRESET_LABELS
        .iter()
        .position(|l| *l == "Arc")
        .unwrap();
    click(
        &ctx,
        &mut shell,
        ui::view::ids::tool_option(ToolId::FreeTransform, keys::WARP),
    );
    click(
        &ctx,
        &mut shell,
        ui::view::ids::tool_option_choice(ToolId::FreeTransform, keys::WARP, arc),
    );
    let (_, state, mode) = published(&shell).unwrap();
    assert_eq!(mode, TransformMode::Warp, "the session is in Warp");
    let mesh = state.mesh.expect("the preset set the live mesh");
    let straight =
        tools::transform::warp_preset_mesh(tools::transform::WarpPreset::Arc, state.source, 0.0);
    assert_ne!(mesh.points, straight.points, "the mesh is bent");

    enter(&mut shell);
    assert!(!shell.pointer.has_pending_commit());
    assert_ne!(composite(&mut shell), original, "Enter resampled the warp");
}

/// Drag the options bar's field `key` by `by` points in ONE move frame, so
/// the bar writes exactly one edit.
fn nudge_field(ctx: &egui::Context, shell: &mut Shell, key: &'static str, by: egui::Vec2) {
    let id = ui::view::ids::tool_option(ToolId::FreeTransform, key);
    let from = rect_of(ctx, shell, id).center();
    frame(
        ctx,
        shell,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    frame(ctx, shell, vec![egui::Event::PointerMoved(from + by)]);
    frame(ctx, shell, vec![button(from + by, false)]);
    frame(ctx, shell, vec![egui::Event::PointerGone]);
}

fn held_seq(shell: &Shell) -> Option<i32> {
    shell
        .chrome
        .tool_options(ToolId::FreeTransform)
        .into_iter()
        .find(|(k, _)| k == keys::NUMERIC_SEQ)
        .and_then(|(_, v)| v.as_int())
}

#[test]
fn the_first_edit_after_the_options_bar_reset_reshapes_the_live_quad() {
    // Round-3 review: Reset put the held edit counter back to 0 while the
    // live session still held the last number it applied, so the next
    // single edit was swallowed and Enter committed the old quad.
    let dir = tempfile::tempdir().unwrap();
    let mut shell = shell_with_black_square(dir.path());
    let ctx = ctx();
    begin(&ctx, &mut shell);
    drag_field(&ctx, &mut shell, keys::W, egui::vec2(-60.0, 0.0));
    drag_field(&ctx, &mut shell, keys::H, egui::vec2(-60.0, 0.0));
    assert!(
        held_seq(&shell).unwrap_or(0) > 1,
        "precondition: several edits"
    );
    let reset = ui::view::ids::tool_options_reset(ToolId::FreeTransform);

    for round in 0..2 {
        let before = published(&shell).unwrap().0;
        click(&ctx, &mut shell, reset);
        assert_eq!(
            held_seq(&shell),
            None,
            "round {round}: Reset cleared the bar"
        );
        assert_eq!(
            published(&shell).unwrap().0,
            before,
            "round {round}: Reset alone does not move the quad"
        );
        nudge_field(&ctx, &mut shell, keys::W, egui::vec2(20.0, 0.0));
        let typed_w = held_float(&shell, keys::W);
        assert!(
            (typed_w - before.w).abs() > 1.0,
            "round {round}: the nudge moved W: {typed_w} vs {}",
            before.w
        );
        let (n, _, _) = published(&shell).unwrap();
        assert!(
            (n.w - typed_w).abs() < 1e-3,
            "round {round}: live W {} vs typed {typed_w} (seq {:?})",
            n.w,
            held_seq(&shell)
        );
    }

    let typed_w = held_float(&shell, keys::W);
    enter(&mut shell);
    let after = composite(&mut shell);
    let row = black_in_row(&after, 30);
    let expected = (20.0 * typed_w / 100.0).round() as i64;
    assert!(
        (row as i64 - expected).abs() <= 2,
        "row 30 holds {row} black pixels; typed W {typed_w}% of 20 is {expected}"
    );
}
