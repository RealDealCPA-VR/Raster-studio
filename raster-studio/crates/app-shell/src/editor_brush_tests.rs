//! W9-E: the brush engine's new surface, driven through the editor and the
//! chrome — Define Brush Preset from pixels, File ▸ Open of a `.abr`, a
//! sampled tip painted by a real pointer stroke, the stored tips registered
//! at startup, and the brush editor's dynamics surviving an options-bar
//! edit.

use glam::Vec2;
use raster::PixelRect;
use tools::brush::{BrushDynamics, BrushTip, SampledTip, TipId};
use tools::ToolId;
use ui::canvas::PointerInput;
use ui::canvas::PointerPhase;

use crate::action::Action;
use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const W: u32 = 64;
const H: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

/// A white 64x64 canvas with a black bar at x 10..14, y 10..30.
fn bar_canvas() -> Vec<u8> {
    let mut px = vec![255u8; (W * H * 4) as usize];
    for y in 10..30 {
        for x in 10..14 {
            let i = ((y * W + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&[0, 0, 0]);
        }
    }
    px
}

fn editor_on(dir: &std::path::Path, dialogs: ScriptedDialogs, pixels: &[u8]) -> Editor {
    let png = dir.join("canvas.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, W, H, pixels).unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    editor.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
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

fn luma_at(px: &[u8], x: u32, y: u32) -> u8 {
    let i = ((y * W + x) * 4) as usize;
    px[i]
}

fn select(editor: &mut Editor, min: (i32, i32), max: (i32, i32)) {
    let doc = editor.active_mut().unwrap();
    doc.apply(editor_core::Command::SetSelection {
        selection: editor_core::Selection::Rect {
            min: glam::IVec2::new(min.0, min.1),
            max: glam::IVec2::new(max.0, max.1),
        },
    })
    .unwrap();
}

#[test]
fn define_brush_preset_samples_the_selected_pixels_and_a_stroke_stamps_that_shape() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_on(dir.path(), ScriptedDialogs::new(), &bar_canvas());
    ed.set_tool(ToolId::Brush);
    select(&mut ed, (6, 6), (20, 34));

    let message = ed.define_brush_preset().expect("defined");
    assert!(message.contains("inside the selection"), "{message}");

    // The brush now IS the bar: a sampled 4x20 tip at its own size.
    let brush = *ed.brush();
    let BrushTip::Sampled(id) = brush.tip else {
        panic!("Define Brush left a round tip: {brush:?}");
    };
    let tip = tools::brush::sampled_tip(id).expect("registered");
    assert_eq!((tip.width(), tip.height()), (4, 20), "cropped to the ink");
    assert_eq!(brush.size, 20.0);
    assert!(tip.alpha().iter().all(|a| *a == 255), "black = full paint");
    // And the preset store keeps the pixels.
    assert!(ed.presets().tip(asset_store::BlobHash(id.0)).is_some());

    // One click with the Brush on white, away from the original bar: the dab
    // is the bar's shape — tall and thin — not a disc.
    select(&mut ed, (0, 0), (W as i32, H as i32));
    let mut settings = *ed.brush();
    settings.size_pressure = false;
    settings.opacity_pressure = false;
    ed.set_brush(settings);
    let before = composite(&mut ed);
    assert_eq!(luma_at(&before, 45, 32), 255);
    let mut pointer = ToolPointer::new();
    let at = screen(45.0, 32.0);
    pointer.handle(
        &mut ed,
        PointerInput::at(PointerPhase::Down, at),
        false,
        &[],
    );
    pointer.handle(&mut ed, PointerInput::at(PointerPhase::Up, at), false, &[]);
    let after = composite(&mut ed);
    assert!(luma_at(&after, 45, 32) < 64, "the centre was not painted");
    assert!(luma_at(&after, 45, 25) < 64, "the bar is tall");
    assert!(luma_at(&after, 45, 39) < 64, "the bar is tall");
    assert_eq!(luma_at(&after, 50, 32), 255, "a round tip would reach here");
    assert_eq!(luma_at(&after, 40, 32), 255, "a round tip would reach here");
}

#[test]
fn define_brush_preset_refuses_a_selection_with_no_ink() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_on(dir.path(), ScriptedDialogs::new(), &bar_canvas());
    select(&mut ed, (30, 30), (40, 40));
    let err = ed.define_brush_preset().unwrap_err();
    assert!(err.contains("empty"), "{err}");
    assert!(ed.presets().brushes().is_empty());
}

#[test]
fn file_open_of_an_abr_adds_its_brushes_to_the_brushes_panel() {
    let dir = tempfile::tempdir().unwrap();
    let abr = dir.path().join("Grunge Pack.abr");
    let tips = vec![
        (6, 6, vec![200u8; 36]),
        (3, 9, (0..27).map(|i| (i * 9) as u8).collect::<Vec<u8>>()),
    ];
    std::fs::write(&abr, asset_store::abr::write_test_abr(&tips, 2, true)).unwrap();
    let mut ed = editor_on(
        dir.path(),
        ScriptedDialogs::new().opening(&abr),
        &[255u8; (W * H * 4) as usize],
    );
    ed.set_tool(ToolId::Brush);
    let docs_before = ed.documents().len();

    // The chrome exists before the file is opened, as it does in the app.
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    let frame = |chrome: &mut Chrome, ed: &mut Editor| {
        let mut out = ChromeOutput::default();
        let _ = ctx.run(input(Vec::new()), |ctx| out = chrome.ui(ctx, ed));
        out
    };
    frame(&mut chrome, &mut ed);

    ed.dispatch(Action::Open).expect("the brush file opened");
    assert_eq!(
        ed.documents().len(),
        docs_before,
        "a brush file is no document"
    );
    let names: Vec<&str> = ed
        .presets()
        .brushes()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, vec!["Grunge Pack 1", "Grunge Pack 2"]);

    // Drawn, the Brushes panel lists both.
    frame(&mut chrome, &mut ed);
    let listed: Vec<(String, tools::BrushSettings)> = chrome
        .workspace_for_test()
        .brushes
        .presets()
        .iter()
        .map(|p| (p.name.clone(), p.settings))
        .collect();
    let second = listed
        .iter()
        .find(|(n, _)| n == "Grunge Pack 2")
        .expect("the panel lists the imported brush");
    assert!(listed.iter().any(|(n, _)| n == "Grunge Pack 1"));
    assert_eq!(second.1.size, 9.0);
    let BrushTip::Sampled(id) = second.1.tip else {
        panic!("imported as round");
    };
    assert_eq!(tools::brush::sampled_tip(id).unwrap().alpha()[26], 234);

    // Clicking it in the panel makes it the Brush's brush, tip and all.
    let w = chrome.workspace_for_test();
    let index = w
        .brushes
        .presets()
        .iter()
        .position(|p| p.name == "Grunge Pack 2")
        .unwrap();
    let tool = w.palette.active();
    let writes = w.brushes.apply(index, &mut w.options, tool);
    for (key, value) in writes {
        w.emit(ui::Intent::SetToolOption { tool, key, value });
    }
    let out = frame(&mut chrome, &mut ed);
    let applied = out.set_brush.expect("the applied preset reached the brush");
    assert_eq!(applied.tip, BrushTip::Sampled(id));
    assert_eq!(applied.size, 9.0);
}

#[test]
fn a_malformed_abr_is_an_error_not_a_panic_and_adds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let abr = dir.path().join("broken.abr");
    let mut bytes = asset_store::abr::write_test_abr(&[(4, 4, vec![255; 16])], 1, false);
    bytes.truncate(bytes.len() - 5);
    std::fs::write(&abr, &bytes).unwrap();
    let mut ed = editor_on(
        dir.path(),
        ScriptedDialogs::new().opening(&abr),
        &[255u8; (W * H * 4) as usize],
    );
    let docs_before = ed.documents().len();
    let err = ed.dispatch(Action::Open).unwrap_err();
    assert!(
        matches!(err, crate::editor::ActionError::Failed { .. }),
        "{err:?}"
    );
    assert!(ed.presets().brushes().is_empty());
    assert_eq!(ed.documents().len(), docs_before);
    // Garbage with the right extension is refused the same way.
    std::fs::write(&abr, b"not a brush file at all").unwrap();
    assert!(ed.import_abr(&abr).is_err());
}

#[test]
fn stored_tips_are_registered_when_the_editor_starts() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::rooted(dir.path().join("config"));
    // Pixels no other test registers, so only the startup path can.
    let alpha: Vec<u8> = (0..30).map(|i| (i * 7 + 3) as u8).collect();
    let mut store = asset_store::presets::PresetStore::new();
    let hash = store.define_tip(5, 6, alpha.clone());
    store.save(&paths.presets_file()).unwrap();
    let id = TipId(hash.0);
    assert!(tools::brush::sampled_tip(id).is_none(), "not yet");
    let _ed = Editor::with_state(
        paths,
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    let tip = tools::brush::sampled_tip(id).expect("registered at startup");
    assert_eq!(tip.alpha(), &alpha[..]);
    let _ = SampledTip::new(1, 1, vec![0]);
}

#[test]
fn brush_editor_dynamics_survive_an_options_bar_edit() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_on(
        dir.path(),
        ScriptedDialogs::new(),
        &[255u8; (W * H * 4) as usize],
    );
    ed.set_tool(ToolId::Brush);
    let dynamics = BrushDynamics {
        size_jitter: 0.6,
        scatter: 1.0,
        count: 3,
        seed: 42,
        ..Default::default()
    };
    // What confirming the brush editor hands the shell.
    ed.set_brush(tools::BrushSettings {
        dynamics,
        ..*ed.brush()
    });
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input(Vec::new()), |ctx| out = chrome.ui(ctx, &mut ed));
    chrome.workspace_for_test().emit(ui::Intent::SetToolOption {
        tool: ToolId::Brush,
        key: "size",
        value: ui::OptionValue::Float(31.0),
    });
    let _ = ctx.run(input(Vec::new()), |ctx| out = chrome.ui(ctx, &mut ed));
    let brush = out.set_brush.expect("the size edit reached the brush");
    assert_eq!(brush.size, 31.0);
    assert_eq!(
        brush.dynamics, dynamics,
        "the options bar dropped the dynamics"
    );
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
