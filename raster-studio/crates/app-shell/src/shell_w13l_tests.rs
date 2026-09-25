//! W13-L: the Animation panel's Timeline mode reaches the canvas, driven
//! through the shell's own route only: real chrome frames (`Chrome::ui` +
//! `Shell::apply_chrome`, the two steps the render loop takes) with pointer
//! events on the panel's ruler and its Play button.
//!
//! * a ruler drag seeks the document on every frame, before the release, so
//!   the canvas composite (`OpenDocument::composite`) follows the pointer,
//!   and the whole canvas is marked for redraw: a seek is no Command, so no
//!   tile is dirtied by an edit, and `Presenter::sync` redraws only
//!   `take_dirty()` tiles (none = `UploadPlan::Nothing`, a frozen canvas);
//! * Play seeks it on each frame it advances, and Stop leaves it there;
//! * none of it is a history step or makes the document dirty, so Undo after
//!   a scrub takes back the last edit.

use super::*;

use crate::chrome::ChromeOutput;
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use editor_core::timeline::{self, KeyProperty};
use ui::panels::animation::timeline::ids as tl_ids;

const SIDE: u32 = 32;

/// A shell with one opaque red layer that fades in over the timeline's
/// 3000 ms (opacity 0 at 0 ms, 1 at 3000 ms), saved, so the document starts
/// clean; the Animation panel open.
fn shell_with_fade(dir: &std::path::Path) -> (Shell, layer_model::LayerId) {
    let rgba: Vec<u8> = (0..SIDE * SIDE).flat_map(|_| [255u8, 0, 0, 255]).collect();
    let png = dir.join("red.png");
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
    let id = *editor
        .active()
        .unwrap()
        .document
        .layers
        .root()
        .first()
        .unwrap();
    let c = timeline::set_mode(&editor.active().unwrap().document, true).unwrap();
    editor.apply_command(c);
    let c = timeline::add_key(
        &editor.active().unwrap().document,
        id,
        KeyProperty::Opacity,
        3000,
    )
    .unwrap();
    editor.apply_command(c);
    let c = editor_core::Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            opacity: Some(0.0),
            ..Default::default()
        },
    };
    editor.apply_command(c);
    let c = timeline::add_key(
        &editor.active().unwrap().document,
        id,
        KeyProperty::Opacity,
        0,
    )
    .unwrap();
    editor.apply_command(c);
    let project = dir.join("fade.rstudio");
    editor
        .active_mut()
        .unwrap()
        .save_to(&project, "test")
        .unwrap();
    assert!(!editor.active().unwrap().is_dirty());
    let mut shell = Shell::new(editor, Vec::new());
    // The Animation panel on screen, alone in a minimal layout, as the ui
    // crate's own panel tests stage it.
    let dock = &mut shell.chrome.workspace_for_test().dock;
    dock.apply_layout(ui::LayoutId::Minimal);
    dock.set_open(ui::dock::PanelId::Animation, true);
    (shell, id)
}

fn frame(ctx: &egui::Context, shell: &mut Shell, time: f64, events: Vec<egui::Event>) {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        time: Some(time),
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input, |ctx| {
        out = shell.chrome.ui(ctx, &mut shell.editor);
    });
    shell.apply_chrome(out);
}

fn button(pos: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    }
}

/// The alpha of the canvas's top-left pixel, composited the way the
/// presenter composites it.
fn canvas_alpha(shell: &mut Shell) -> u8 {
    let open = shell.editor.active_mut().unwrap();
    open.composite(raster::PixelRect::new(0, 0, SIDE, SIDE))
        .unwrap()[3]
}

/// Whether the presenter's next `sync` redraws the whole canvas: takes the
/// outstanding invalidation exactly as `Presenter::sync` does, so each call
/// sees only what happened since the previous one.
fn presenter_redraws_all(shell: &mut Shell) -> bool {
    shell.editor.active_mut().unwrap().take_dirty().is_all()
}

fn depth(shell: &Shell) -> usize {
    shell.editor.active().unwrap().history_depth()
}

fn dirty(shell: &Shell) -> bool {
    shell.editor.active().unwrap().is_dirty()
}

fn playhead(shell: &Shell) -> u32 {
    shell.editor.active().unwrap().document.timeline.current_ms
}

#[test]
fn a_ruler_drag_scrubs_the_canvas_live_with_no_history_step() {
    let dir = tempfile::tempdir().unwrap();
    let (mut shell, id) = shell_with_fade(dir.path());
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    for _ in 0..3 {
        frame(&ctx, &mut shell, 0.0, Vec::new());
    }
    let ruler = ctx
        .read_response(tl_ids::ruler())
        .expect("the Timeline ruler was drawn")
        .rect;
    let steps_before = depth(&shell);
    assert_eq!(canvas_alpha(&mut shell), 0, "faded out at 0 ms");

    let y = ruler.center().y;
    let from = egui::pos2(ruler.left() + 1.0, y);
    let to = egui::pos2(ruler.center().x, y);
    frame(
        &ctx,
        &mut shell,
        0.0,
        vec![egui::Event::PointerMoved(from), button(from, true)],
    );
    // Mid-drag, before any release: the canvas follows the pointer, and the
    // presenter is told to redraw it on every frame of the drag.
    let _ = presenter_redraws_all(&mut shell);
    let mut alphas = Vec::new();
    for step in 1..=4 {
        let at = from + (to - from) * (step as f32 / 4.0);
        frame(&ctx, &mut shell, 0.0, vec![egui::Event::PointerMoved(at)]);
        assert!(
            presenter_redraws_all(&mut shell),
            "drag step {step}: the presenter must redraw the scrubbed canvas"
        );
        alphas.push(canvas_alpha(&mut shell));
    }
    assert!(
        alphas.windows(2).all(|w| w[1] > w[0]) && alphas[3] > 100,
        "the canvas fades in as the pointer moves along the ruler: {alphas:?}"
    );
    let at = playhead(&shell);
    assert!((1400..=1600).contains(&at), "{at}");
    let opacity = shell
        .editor
        .active()
        .unwrap()
        .document
        .layers
        .get(id)
        .unwrap()
        .opacity;
    assert!((opacity - at as f32 / 3000.0).abs() < 1e-3, "{opacity}");
    frame(&ctx, &mut shell, 0.0, vec![button(to, false)]);

    assert_eq!(depth(&shell), steps_before, "a scrub is no history step");
    assert!(!dirty(&shell), "a scrub does not dirty the document");

    // Undo takes back the last edit (the 0 ms key), not the scrub; the
    // playhead stays where the scrub left it.
    let at = playhead(&shell);
    shell.editor.dispatch(crate::Action::Undo).unwrap();
    let doc = &shell.editor.active().unwrap().document;
    assert_eq!(
        doc.timeline.track(id).unwrap().opacity.len(),
        1,
        "the last key was undone"
    );
    assert_eq!(doc.timeline.current_ms, at, "the playhead did not move");
}

#[test]
fn playback_scrubs_the_canvas_each_frame_and_stop_leaves_no_history() {
    let dir = tempfile::tempdir().unwrap();
    let (mut shell, _) = shell_with_fade(dir.path());
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    for _ in 0..3 {
        frame(&ctx, &mut shell, 0.0, Vec::new());
    }
    let play = ctx
        .read_response(ui::panels::animation::ids::play())
        .expect("Play was drawn")
        .rect
        .center();
    let steps_before = depth(&shell);
    frame(
        &ctx,
        &mut shell,
        0.0,
        vec![egui::Event::PointerMoved(play), button(play, true)],
    );
    frame(&ctx, &mut shell, 0.0, vec![button(play, false)]);
    let _ = presenter_redraws_all(&mut shell);
    let mut alphas = Vec::new();
    for secs in [0.75, 1.5, 2.25] {
        frame(&ctx, &mut shell, secs, Vec::new());
        assert!(
            presenter_redraws_all(&mut shell),
            "playback at {secs} s: the presenter must redraw the canvas"
        );
        alphas.push(canvas_alpha(&mut shell));
    }
    assert!(
        alphas.windows(2).all(|w| w[1] > w[0]),
        "the canvas fades in while playing: {alphas:?}"
    );
    assert!(
        (100..=160).contains(&alphas[1]),
        "about half at 1.5 s: {alphas:?}"
    );
    // Stop (the same button) leaves the canvas on the frame it stopped at.
    let play = ctx
        .read_response(ui::panels::animation::ids::play())
        .unwrap()
        .rect
        .center();
    frame(
        &ctx,
        &mut shell,
        2.25,
        vec![egui::Event::PointerMoved(play), button(play, true)],
    );
    frame(&ctx, &mut shell, 2.25, vec![button(play, false)]);
    let at = playhead(&shell);
    assert!((2200..=2300).contains(&at), "{at}");
    frame(&ctx, &mut shell, 5.0, Vec::new());
    assert_eq!(playhead(&shell), at, "stopped");
    assert_eq!(depth(&shell), steps_before, "playback left no history");
    assert!(!dirty(&shell), "nor a dirty flag");
}
