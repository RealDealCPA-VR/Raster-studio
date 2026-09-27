//! W18-F, through the shell's own routes: the options bar's Cancel cross and
//! Commit check for a Perspective Crop quad and a Type run (the bar drawn by
//! the whole chrome, the cancel performed by the shell's per-frame step
//! `ToolPointer::begin_pending_session`), a held Show Transform Controls drag
//! that Enter lands, Crop by setting the crop box, the Paint Bucket pattern
//! pick, the Zoom bar's Zoom Out and the Zoom / Hand bars' All Documents, and
//! the Clone Stamp's Alt toggle and K key - each through
//! `ToolPointer::handle`, the route every canvas sample takes.

use glam::Vec2;

use editor_core::{Command, Selection};
use raster::{PixelRect, TileCoord};
use tools::registry::bar_w18::{self as bar, CropBy, NavPrefs};
use tools::{Modifiers, ToolId, ToolSetting};
use ui::canvas::{PointerInput, PointerPhase};

use crate::chrome::{install_theme, Chrome};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::tool_input::ToolPointer;

const SIDE: u32 = 64;
const VIEWPORT: Vec2 = Vec2::new(400.0, 300.0);

/// The bar's slots back to their start before and after each test, so a
/// reused test thread inherits nothing.
struct Clean;

impl Clean {
    fn new() -> Self {
        bar::reset();
        Clean
    }
}

impl Drop for Clean {
    fn drop(&mut self) {
        bar::reset();
    }
}

/// A 64x64 document whose left half is dark and right half white, camera
/// at 100% with the image centred.
fn editor(dir: &std::path::Path) -> Editor {
    let png = dir.join("w18f.png");
    let mut rgba = Vec::with_capacity((SIDE * SIDE * 4) as usize);
    for _y in 0..SIDE {
        for x in 0..SIDE {
            rgba.extend_from_slice(&if x < SIDE / 2 {
                [20, 20, 20, 255]
            } else {
                [250, 250, 250, 255]
            });
        }
    }
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, SIDE, SIDE, &rgba).unwrap(),
    )
    .unwrap();
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    editor.open_path(&png).unwrap();
    frame_camera(&mut editor);
    editor
}

fn frame_camera(editor: &mut Editor) {
    let doc = editor.active_mut().unwrap();
    doc.set_viewport(VIEWPORT);
    doc.camera.zoom = 1.0;
    doc.camera.center = Vec2::splat(SIDE as f32 / 2.0);
}

fn screen(x: f32, y: f32) -> Vec2 {
    VIEWPORT * 0.5 + Vec2::new(x, y) - Vec2::splat(SIDE as f32 / 2.0)
}

fn send(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    phase: PointerPhase,
    (x, y): (f32, f32),
    m: Modifiers,
    settings: &[(String, ToolSetting)],
) -> usize {
    let mut input = PointerInput::at(phase, screen(x, y));
    input.modifiers = m;
    pointer.handle(editor, input, false, settings).steps
}

/// Press, move through, release; the history steps it landed.
fn drag(
    pointer: &mut ToolPointer,
    editor: &mut Editor,
    pts: &[(f32, f32)],
    settings: &[(String, ToolSetting)],
) -> usize {
    let m = Modifiers::NONE;
    let mut steps = send(pointer, editor, PointerPhase::Down, pts[0], m, settings);
    for p in &pts[1..] {
        steps += send(pointer, editor, PointerPhase::Move, *p, m, settings);
    }
    steps
        + send(
            pointer,
            editor,
            PointerPhase::Up,
            *pts.last().unwrap(),
            m,
            settings,
        )
}

fn depth(editor: &Editor) -> usize {
    editor.active().unwrap().history_depth()
}

/// Paint the active layer's first tile through the command route.
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

/// A click on `id` where the chrome drew it.
fn chrome_click(ctx: &egui::Context, chrome: &mut Chrome, editor: &mut Editor, id: egui::Id) {
    for _ in 0..3 {
        chrome_frame(ctx, chrome, editor, Vec::new());
    }
    let at = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} is not drawn"))
        .rect
        .center();
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    chrome_frame(
        ctx,
        chrome,
        editor,
        vec![egui::Event::PointerMoved(at), button(true)],
    );
    chrome_frame(ctx, chrome, editor, vec![button(false)]);
}

fn drawn(ctx: &egui::Context, chrome: &mut Chrome, editor: &mut Editor, id: egui::Id) -> bool {
    for _ in 0..3 {
        chrome_frame(ctx, chrome, editor, Vec::new());
    }
    ctx.read_response(id).is_some()
}

#[test]
fn the_cancel_cross_drawn_by_the_chrome_drops_a_perspective_crop_quad() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::PerspectiveCrop);
    let mut pointer = ToolPointer::new();
    drag(&mut pointer, &mut editor, &[(8.0, 8.0), (40.0, 40.0)], &[]);
    // The shell's frame: the geometry and the held edit are published.
    assert!(matches!(
        pointer.live_geometry(),
        Some((_, tools::SessionGeometry::PerspectiveCrop { .. }))
    ));
    assert_eq!(bar::pending_for(ToolId::PerspectiveCrop), Some(true));
    let before = depth(&editor);

    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    let cancel = ui::view::ids::tool_option(ToolId::PerspectiveCrop, "w18_cancel");
    let commit = ui::view::ids::tool_option(ToolId::PerspectiveCrop, "commit");
    assert!(drawn(&ctx, &mut chrome, &mut editor, commit), "the check");
    chrome_click(&ctx, &mut chrome, &mut editor, cancel);
    // The shell's per-frame step performs the posted Cancel.
    pointer.begin_pending_session(&mut editor, &[]);
    assert!(pointer.live_geometry().is_none(), "the quad is gone");
    assert_eq!(depth(&editor), before, "a cancel writes no history");
    assert_eq!(bar::pending_for(ToolId::PerspectiveCrop), Some(false));
    assert!(
        !drawn(&ctx, &mut chrome, &mut editor, cancel),
        "nothing held, no cross"
    );
}

#[test]
fn a_type_run_is_published_as_held_and_the_cross_cancels_it() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::Type);
    let mut pointer = ToolPointer::new();
    let layers = editor.active().unwrap().document.layers.len();
    drag(&mut pointer, &mut editor, &[(10.0, 20.0)], &[]);
    assert!(pointer.is_text_editing(), "the click opened a run");
    let _ = pointer.live_geometry();
    assert_eq!(bar::pending_for(ToolId::Type), Some(true));
    bar::post_cancel();
    pointer.begin_pending_session(&mut editor, &[]);
    assert!(!pointer.is_text_editing(), "the cross ended the run");
    assert_eq!(
        editor.active().unwrap().document.layers.len(),
        layers,
        "a cancelled new run leaves no layer"
    );
    let _ = pointer.live_geometry();
    assert_eq!(bar::pending_for(ToolId::Type), Some(false));
}

#[test]
fn a_show_transform_controls_drag_is_held_until_enter_lands_it() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::Move);
    let settings = vec![("show_transform".to_string(), ToolSetting::Bool(true))];
    let mut pointer = ToolPointer::new();
    pointer.begin_pending_session(&mut editor, &settings);
    let before = depth(&editor);
    let _ = pointer.live_geometry();
    assert_eq!(
        bar::pending_for(ToolId::Move),
        Some(false),
        "the box alone is not a held edit"
    );
    let steps = drag(
        &mut pointer,
        &mut editor,
        &[(64.0, 64.0), (48.0, 48.0), (32.0, 32.0)],
        &settings,
    );
    assert_eq!(steps, 0, "the release keeps the session open");
    assert_eq!(depth(&editor), before);
    let _ = pointer.live_geometry();
    assert_eq!(bar::pending_for(ToolId::Move), Some(true));
    let outcome = pointer.commit(&mut editor);
    assert_eq!(outcome.failed, None);
    assert_eq!(depth(&editor), before + 1, "Enter lands it as one step");
    let doc = &editor.active().unwrap().document;
    let m = doc
        .layers
        .get(doc.active_layer().unwrap())
        .unwrap()
        .transform;
    assert!((m.matrix2.x_axis.x - 0.5).abs() < 1e-4, "{m:?}");
}

#[test]
fn crop_by_current_layer_clicked_on_the_bar_sets_the_box_and_enter_crops() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    // Ink only in 16..40 on a transparent layer.
    paint(&mut editor, |x, y| {
        if (16..40).contains(&x) && (16..40).contains(&y) {
            [200, 0, 0, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    editor.set_tool(ToolId::Crop);
    let mut pointer = ToolPointer::new();
    let before = depth(&editor);
    let ctx = egui::Context::default();
    install_theme(&ctx, design::Theme::Dark);
    let mut chrome = Chrome::new();
    let opener = ui::view::ids::tool_option(ToolId::Crop, "w16k_crop_by");
    chrome_click(&ctx, &mut chrome, &mut editor, opener);
    chrome_click(
        &ctx,
        &mut chrome,
        &mut editor,
        opener.with(ui::menu::MenuAction::CropToLayer),
    );
    assert_eq!(depth(&editor), before, "the row crops nothing by itself");
    // The shell's per-frame step gives the Crop tool the box.
    assert!(pointer.begin_pending_session(&mut editor, &[]));
    let Some((_, tools::SessionGeometry::Crop { rect, .. })) = pointer.live_geometry() else {
        panic!("the crop box is up");
    };
    assert_eq!(rect, [Vec2::new(16.0, 16.0), Vec2::new(40.0, 40.0)]);
    assert!(pointer.has_pending_commit(), "it waits for the commit");
    let outcome = pointer.commit(&mut editor);
    assert_eq!(outcome.failed, None);
    let doc = editor.active().unwrap();
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (24, 24),
        "Enter crops to the layer's bounds"
    );
}

#[test]
fn crop_by_selection_and_trim_box_what_they_name() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.active_mut().unwrap().document.selection = Selection::Rect {
        min: glam::IVec2::new(4, 6),
        max: glam::IVec2::new(20, 30),
    };
    assert_eq!(
        crate::tool_input::options_w18::crop_by_rect(&mut editor, CropBy::Selection),
        Some(PixelRect::new(4, 6, 16, 24))
    );
    // The opaque image trims to itself; a box beyond nothing is none.
    assert_eq!(
        crate::tool_input::options_w18::crop_by_rect(&mut editor, CropBy::Trim),
        Some(PixelRect::new(0, 0, SIDE, SIDE))
    );
    editor.active_mut().unwrap().document.selection = Selection::None;
    assert_eq!(
        crate::tool_input::options_w18::crop_by_rect(&mut editor, CropBy::Selection),
        None
    );
    // Only the Crop tool takes the request.
    editor.set_tool(ToolId::Brush);
    let mut pointer = ToolPointer::new();
    bar::post_crop_by(CropBy::AllLayers);
    assert!(!pointer.begin_pending_session(&mut editor, &[]));
    assert!(pointer.live_geometry().is_none());
}

#[test]
fn the_bucket_pattern_pick_makes_that_pattern_active() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    for (name, rgb) in [("Reds", [255u8, 0, 0]), ("Blues", [0, 0, 255])] {
        editor
            .presets_mut()
            .define_pattern(asset_store::presets::PatternPreset {
                name: name.to_string(),
                width: 1,
                height: 1,
                rgba8: vec![rgb[0], rgb[1], rgb[2], 255],
            });
    }
    editor.set_tool(ToolId::PaintBucket);
    let mut pointer = ToolPointer::new();
    pointer.begin_pending_session(&mut editor, &[]);
    let (names, _) = bar::patterns();
    assert_eq!(names, vec!["Reds".to_string(), "Blues".to_string()]);
    bar::post_pattern_pick("Reds".to_string());
    pointer.begin_pending_session(&mut editor, &[]);
    assert_eq!(editor.active_pattern().unwrap().name, "Reds");
    assert_eq!(bar::patterns().1.as_deref(), Some("Reds"));
}

#[test]
fn zoom_out_flips_the_click_and_all_documents_zoom_and_pan_every_document() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::Zoom);
    let mut pointer = ToolPointer::new();
    let zoom = |editor: &Editor| editor.active().unwrap().camera.zoom;
    drag(&mut pointer, &mut editor, &[(32.0, 32.0)], &[]);
    assert!(zoom(&editor) > 1.01, "a click zooms in: {}", zoom(&editor));
    frame_camera(&mut editor);
    bar::set_nav(NavPrefs {
        zoom_out: true,
        ..NavPrefs::default()
    });
    drag(&mut pointer, &mut editor, &[(32.0, 32.0)], &[]);
    assert!(
        zoom(&editor) < 0.99,
        "Zoom Out steps out: {}",
        zoom(&editor)
    );

    // A second document; the first is active again.
    let png = dir.path().join("second.png");
    std::fs::write(
        &png,
        raster::encode(
            raster::ExportFormat::Png,
            SIDE,
            SIDE,
            &[128u8; (SIDE * SIDE * 4) as usize],
        )
        .unwrap(),
    )
    .unwrap();
    editor.open_path(&png).unwrap();
    frame_camera(&mut editor);
    let second = editor.active().unwrap().id();
    editor.activate(0).unwrap();
    frame_camera(&mut editor);
    let other = |editor: &Editor| {
        editor
            .documents()
            .iter()
            .find(|d| d.id() == second)
            .map(|d| (d.camera.zoom, d.camera.center))
            .unwrap()
    };
    // All Documents off: the other document stays.
    bar::set_nav(NavPrefs::default());
    drag(&mut pointer, &mut editor, &[(32.0, 32.0)], &[]);
    assert_eq!(other(&editor).0, 1.0);
    frame_camera(&mut editor);
    bar::set_nav(NavPrefs {
        zoom_all_documents: true,
        ..NavPrefs::default()
    });
    drag(&mut pointer, &mut editor, &[(32.0, 32.0)], &[]);
    let step = zoom(&editor);
    assert!(
        (other(&editor).0 - step).abs() < 1e-3,
        "the other document took the same step: {} vs {step}",
        other(&editor).0
    );
    // The Hand's All Documents pans the other one the same way.
    frame_camera(&mut editor);
    editor.set_tool(ToolId::Hand);
    let before = other(&editor);
    bar::set_nav(NavPrefs {
        hand_all_documents: true,
        ..NavPrefs::default()
    });
    drag(
        &mut pointer,
        &mut editor,
        &[(32.0, 32.0), (22.0, 27.0), (12.0, 22.0)],
        &[],
    );
    let moved = editor.active().unwrap().camera.center - Vec2::splat(32.0);
    assert!(
        moved.length() > 1.0,
        "the active document panned: {moved:?}"
    );
    let after = other(&editor);
    let other_moved = (after.1 - before.1) * after.0;
    let active_moved = moved * editor.active().unwrap().camera.zoom;
    assert!(
        (other_moved - active_moved).length() < 1e-2,
        "the same pan on screen: {other_moved:?} vs {active_moved:?}"
    );
}

#[test]
fn the_alt_toggle_and_the_k_key_pick_the_clone_source() {
    let _clean = Clean::new();
    let dir = tempfile::tempdir().unwrap();
    let mut editor = editor(dir.path());
    editor.set_tool(ToolId::CloneStamp);
    let stroke = [(44.0, 20.0), (50.0, 20.0), (56.0, 20.0)];
    // No source: the stroke is refused.
    let mut pointer = ToolPointer::new();
    assert_eq!(drag(&mut pointer, &mut editor, &stroke, &[]), 0);

    // The Alt toggle: the click picks the source, then the toggle pops out.
    bar::arm_select_source(true);
    assert_eq!(drag(&mut pointer, &mut editor, &[(10.0, 20.0)], &[]), 0);
    assert!(!bar::select_source_armed(), "the toggle popped out");
    assert_eq!(
        drag(&mut pointer, &mut editor, &stroke, &[]),
        1,
        "the stroke clones from the picked source"
    );

    // K held: the same pick, on a fresh tool.
    let mut pointer = ToolPointer::new();
    editor.set_tool(ToolId::Brush);
    editor.set_tool(ToolId::CloneStamp);
    bar::set_source_key_held(true);
    assert_eq!(drag(&mut pointer, &mut editor, &[(10.0, 40.0)], &[]), 0);
    bar::set_source_key_held(false);
    assert_eq!(
        drag(
            &mut pointer,
            &mut editor,
            &[(44.0, 40.0), (50.0, 40.0), (56.0, 40.0)],
            &[]
        ),
        1
    );
}

/// The samples of the selection strictly between unselected and selected.
fn soft_samples(editor: &Editor) -> usize {
    let sel = &editor.active().unwrap().document.selection;
    let mut n = 0;
    for y in 0..SIDE as i32 {
        for x in 0..SIDE as i32 {
            let c = sel.coverage_at(glam::IVec2::new(x, y));
            if c > 0.02 && c < 0.98 {
                n += 1;
            }
        }
    }
    n
}

#[test]
fn the_magnetic_lasso_bars_feather_softens_the_closed_outline() {
    let _clean = Clean::new();
    let outline = [
        (40.0, 12.0),
        (56.0, 12.0),
        (56.0, 52.0),
        (40.0, 52.0),
        (40.0, 12.0),
    ];
    let lasso = |feather: f32| {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::MagneticLasso);
        let settings = vec![
            ("feather".to_string(), ToolSetting::Float(feather)),
            ("antialias".to_string(), ToolSetting::Bool(false)),
            // The outline follows the drag rather than the one ink edge.
            ("search_radius".to_string(), ToolSetting::Int(1)),
            ("edge_weight".to_string(), ToolSetting::Float(0.0)),
        ];
        let mut pointer = ToolPointer::new();
        drag(&mut pointer, &mut editor, &outline, &settings);
        let sel = &editor.active().unwrap().document.selection;
        assert!(!sel.is_none(), "the outline closed into a selection");
        soft_samples(&editor)
    };
    assert_eq!(lasso(0.0), 0, "no feather, no anti-alias: a hard edge");
    assert!(lasso(6.0) > 20, "the bar's Feather softens the edge");
}

#[test]
fn the_pencil_bars_smoothing_reaches_the_stroke() {
    let _clean = Clean::new();
    let zigzag: Vec<(f32, f32)> = (0..12)
        .map(|i| (36.0 + 2.0 * i as f32, if i % 2 == 0 { 20.0 } else { 40.0 }))
        .collect();
    let stroke = |smoothing: f32| {
        let dir = tempfile::tempdir().unwrap();
        let mut editor = editor(dir.path());
        editor.set_tool(ToolId::Pencil);
        // Smoothing is a brush-shared key: the bar's value reaches the
        // stroke through the Pencil's brush (`chrome::brush_from_options`
        // writes it there), which the press hands the tool.
        let brush = tools::BrushSettings {
            size: 2.0,
            smoothing,
            ..*editor.brush()
        };
        editor.set_brush(brush);
        let mut pointer = ToolPointer::new();
        assert_eq!(drag(&mut pointer, &mut editor, &zigzag, &[]), 1);
        let doc = editor.active_mut().unwrap();
        let canvas = doc.canvas_rect();
        doc.composite(canvas).unwrap()
    };
    let raw = stroke(0.0);
    let smoothed = stroke(0.9);
    let differ = raw
        .as_chunks::<4>()
        .0
        .iter()
        .zip(smoothed.as_chunks::<4>().0)
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differ > 20,
        "Smoothing 90% draws a different (smoother) line: {differ} pixels differ"
    );
}
