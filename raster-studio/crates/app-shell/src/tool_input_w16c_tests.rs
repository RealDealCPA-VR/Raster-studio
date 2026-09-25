//! W16-C, through the real routes: the Paint Bucket's Fill source set on the
//! options bar (`Chrome::set_tool_option` → `Chrome::tool_options`, the
//! settings a real press is seeded with) fills with the defined pattern, and
//! the options bar's Commit check, clicked in a headless frame of the whole
//! chrome, commits a live Free Transform through `Chrome::confirm_tool` —
//! the step the shell runs for `ChromeOutput::confirm_tool`.

use glam::Vec2;

use editor_core::{Command, Selection};
use raster::{PixelRect, TileCoord};
use tools::{ToolId, ToolSetting};
use ui::canvas::{PointerInput, PointerPhase};

use crate::chrome::{install_theme, Chrome};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const W: u32 = 64;
const H: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);
const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];

/// An opaque white 64x64 document, camera at 100% with the image centred.
fn editor(dir: &std::path::Path) -> Editor {
    let png = dir.join("w16c.png");
    std::fs::write(
        &png,
        raster::encode(
            raster::ExportFormat::Png,
            W,
            H,
            &[255u8; (W * H * 4) as usize],
        )
        .unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    let doc = editor.active_mut().unwrap();
    doc.set_viewport(VIEWPORT);
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::new(W as f32 / 2.0, H as f32 / 2.0);
    editor
}

fn screen(x: f32, y: f32) -> Vec2 {
    VIEWPORT * 0.5 + Vec2::new(x - W as f32 / 2.0, y - H as f32 / 2.0)
}

fn composite(editor: &mut Editor) -> Vec<u8> {
    editor
        .active_mut()
        .unwrap()
        .composite(PixelRect::new(0, 0, W, H))
        .unwrap()
}

fn px(buf: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
}

/// Paint the canvas layer through the command route: `ink(x, y)` decides
/// each pixel of the first tile.
fn paint(editor: &mut Editor, ink: impl Fn(u32, u32) -> [u8; 4]) {
    let mut bytes = Vec::with_capacity(256 * 256 * 4);
    for y in 0..256u32 {
        for x in 0..256u32 {
            bytes.extend_from_slice(&ink(x, y));
        }
    }
    let doc = editor.active_mut().unwrap();
    let layer = doc.document.active_layer().unwrap();
    let hash = doc.tiles.insert_bytes(bytes);
    doc.apply(
        Command::paint_tiles(
            editor_core::PixelTarget::Layer(layer),
            vec![editor_core::TileEdit::set(TileCoord::new(0, 0, 0), hash)],
        )
        .unwrap(),
    )
    .unwrap();
}

/// A 2x2 red/blue checker defined through Edit > Define Pattern, then the
/// canvas put back to white so the fill has a single region to flood.
fn define_checker(editor: &mut Editor) {
    paint(editor, |x, y| {
        if x < 2 && y < 2 {
            if (x + y) % 2 == 0 {
                RED
            } else {
                BLUE
            }
        } else {
            [255, 255, 255, 255]
        }
    });
    editor.active_mut().unwrap().document.selection = Selection::Rect {
        min: glam::IVec2::new(0, 0),
        max: glam::IVec2::new(2, 2),
    };
    editor.define_pattern_from_selection().unwrap();
    editor.active_mut().unwrap().document.selection = Selection::None;
    paint(editor, |_, _| [255, 255, 255, 255]);
    assert!(editor.active_tool_pattern().is_some());
}

/// What the options bar holds for `tool`, through the shell's boundary
/// conversion.
fn seeded(chrome: &Chrome, tool: ToolId) -> Vec<(String, ToolSetting)> {
    chrome
        .tool_options(tool)
        .into_iter()
        .map(|(key, value)| {
            let setting = match value {
                ui::OptionValue::Float(v) => ToolSetting::Float(v),
                ui::OptionValue::Int(v) => ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => ToolSetting::Color(v),
            };
            (key, setting)
        })
        .collect()
}

fn click_at(pointer: &mut ToolPointer, editor: &mut Editor, settings: &[(String, ToolSetting)]) {
    for phase in [PointerPhase::Down, PointerPhase::Up] {
        let out = pointer.handle(
            editor,
            PointerInput::at(phase, screen(32.0, 32.0)),
            false,
            settings,
        );
        assert!(out.failed.is_none(), "the click refused: {:?}", out.failed);
    }
}

#[test]
fn the_bucket_set_to_pattern_on_the_options_bar_fills_the_pattern() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    define_checker(&mut editor);
    editor.set_tool(ToolId::PaintBucket);

    // Foreground (the default): a solid fill of the foreground colour.
    let mut chrome = Chrome::new();
    let mut pointer = ToolPointer::new();
    click_at(
        &mut pointer,
        &mut editor,
        &seeded(&chrome, ToolId::PaintBucket),
    );
    let solid = composite(&mut editor);
    assert_eq!(px(&solid, 32, 32), px(&solid, 33, 32), "a solid fill");

    // Fill: Pattern, picked on the bar.
    paint(&mut editor, |_, _| [255, 255, 255, 255]);
    chrome.set_tool_option(
        ToolId::PaintBucket,
        tools::bucket::FILL_SOURCE_KEY,
        ui::OptionValue::Choice(1),
    );
    let mut pointer = ToolPointer::new();
    click_at(
        &mut pointer,
        &mut editor,
        &seeded(&chrome, ToolId::PaintBucket),
    );
    let after = composite(&mut editor);
    // The pattern tiles from the document origin: (x + y) even is red.
    assert_eq!(
        px(&after, 32, 32),
        RED,
        "the bucket did not fill the pattern"
    );
    assert_eq!(px(&after, 33, 32), BLUE, "the bucket filled a solid");
    assert_eq!(px(&after, 63, 63), RED, "the flood reached the far corner");
}

/// One headless frame of the whole chrome; returns what it asked for.
fn chrome_frame(
    ctx: &egui::Context,
    chrome: &mut Chrome,
    editor: &mut Editor,
    events: Vec<egui::Event>,
) -> crate::chrome::ChromeOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = None;
    let _ = ctx.run(input, |ctx| out = Some(chrome.ui(ctx, editor)));
    out.unwrap()
}

#[test]
fn the_options_bar_commit_check_commits_a_live_free_transform() {
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    // A dark square in the middle, so the transform moves visible ink.
    paint(&mut editor, |x, y| {
        if (24..40).contains(&x) && (24..40).contains(&y) {
            [10, 10, 10, 255]
        } else {
            [255, 255, 255, 255]
        }
    });
    editor.set_tool(ToolId::FreeTransform);
    let depth = editor.active().unwrap().history_depth();
    let before = composite(&mut editor);

    // Drag the quad by (+12, +12): the session is live, nothing committed.
    let mut pointer = ToolPointer::new();
    for (phase, (x, y)) in [
        (PointerPhase::Down, (32.0, 24.0)),
        (PointerPhase::Move, (44.0, 36.0)),
        (PointerPhase::Up, (44.0, 36.0)),
    ] {
        pointer.handle(
            &mut editor,
            PointerInput::at(phase, screen(x, y)),
            false,
            &[],
        );
    }
    pointer.settle_preview(&mut editor);
    assert!(
        pointer.live_geometry().is_some(),
        "the transform is pending"
    );
    assert_eq!(editor.active().unwrap().history_depth(), depth);

    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    let doc = editor.active().map(|d| d.id());
    chrome.publish_tool_geometry(pointer.live_geometry(), doc);
    // The check is `ui::view::toolbar::COMMIT_KEY` ("commit").
    let id = ui::view::ids::tool_option(ToolId::FreeTransform, "commit");
    for _ in 0..3 {
        chrome_frame(&ctx, &mut chrome, &mut editor, Vec::new());
    }
    let at = ctx
        .read_response(id)
        .expect("the Commit check is drawn while the transform is pending")
        .rect
        .center();
    let mut confirmed = false;
    for events in [
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ],
        vec![egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }],
    ] {
        let out = chrome_frame(&ctx, &mut chrome, &mut editor, events);
        confirmed |= out.confirm_tool;
    }
    assert!(confirmed, "clicking the check asked the shell to confirm");
    // The shell's step for `confirm_tool`.
    chrome.confirm_tool(&mut pointer, &mut editor);

    assert!(pointer.live_geometry().is_none(), "the session ended");
    assert_eq!(
        editor.active().unwrap().history_depth(),
        depth + 1,
        "the transform landed as one history step"
    );
    let after = composite(&mut editor);
    assert_ne!(px(&after, 45, 45), px(&before, 45, 45), "the ink moved");

    // With nothing pending the check is gone.
    for _ in 0..3 {
        chrome_frame(&ctx, &mut chrome, &mut editor, Vec::new());
    }
    assert!(
        ctx.read_response(id).is_none(),
        "no pending edit, no Commit"
    );
}
