//! W18-I: application routes — a file dropped on or off the canvas, and
//! Quick Export of the selected layers at a scale, as PNG or SVG — driven
//! through the real shell, chrome, menu bridge and file dialogs.

use super::*;

use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 40;
const H: u32 = 30;

fn png(dir: &std::path::Path, name: &str, rgba: [u8; 4], w: u32, h: u32) -> std::path::PathBuf {
    let path = dir.join(name);
    let pixels: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, w, h, &pixels).unwrap(),
    )
    .unwrap();
    path
}

fn editor(dir: &std::path::Path, dialogs: ScriptedDialogs) -> Editor {
    let mut editor = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    editor
        .open_path(&png(dir, "base.png", [255, 0, 0, 255], W, H))
        .unwrap();
    editor
}

/// Lay out one frame of the real chrome, so the shell knows where the
/// canvas area is.
fn lay_out(shell: &mut Shell) -> crate::interaction_geometry::CanvasArea {
    let ctx = egui::Context::default();
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            ..Default::default()
        },
        |ctx| {
            let _ = shell.chrome.ui(ctx, &mut shell.editor);
        },
    );
    shell
        .chrome
        .canvas_area_px()
        .expect("a frame laid out a canvas")
}

fn layer_count(shell: &Shell) -> usize {
    shell
        .editor
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len()
}

/// Photopea places a file dropped on the canvas and opens one dropped
/// anywhere else as a new document. The same image dropped on the canvas
/// area adds a layer; dropped on the menu bar (off the canvas) it opens as
/// its own document and the first one is untouched.
#[test]
fn a_file_dropped_off_the_canvas_opens_and_on_the_canvas_places() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = Shell::new(editor(dir.path(), ScriptedDialogs::new()), Vec::new());
    let area = lay_out(&mut shell);
    let dropped = png(dir.path(), "dropped.png", [0, 0, 255, 255], 8, 8);

    let docs = shell.editor.documents().len();
    let layers = layer_count(&shell);
    let centre = area.origin + area.size * 0.5;
    shell.on_dropped_files_at(std::slice::from_ref(&dropped), Some(centre));
    assert_eq!(
        shell.editor.documents().len(),
        docs,
        "on the canvas: no tab"
    );
    assert_eq!(layer_count(&shell), layers + 1, "on the canvas: placed");

    let menu_bar = Vec2::new(area.origin.x + 4.0, 1.0);
    assert!(menu_bar.y < area.origin.y, "{area:?}");
    shell.on_dropped_files_at(&[dropped], Some(menu_bar));
    assert_eq!(
        shell.editor.documents().len(),
        docs + 1,
        "off the canvas: a new document"
    );
    assert_eq!(
        layer_count(&shell),
        1,
        "the new document is the image alone"
    );
}

/// A drop with no position (a caller that has none) still places, as it
/// always did.
#[test]
fn a_drop_with_no_position_places() {
    let dir = tempfile::tempdir().unwrap();
    let mut shell = Shell::new(editor(dir.path(), ScriptedDialogs::new()), Vec::new());
    let _ = lay_out(&mut shell);
    let layers = layer_count(&shell);
    let dropped = png(dir.path(), "dropped.png", [0, 0, 255, 255], 8, 8);
    shell.on_dropped_files(&[dropped]);
    assert_eq!(layer_count(&shell), layers + 1);
}

/// Two layers — "Blue" (left half blue) and "Green" (right half green) —
/// over the red image, both selected.
fn two_selected(dir: &std::path::Path, target: &std::path::Path) -> Editor {
    let mut editor = editor(dir, ScriptedDialogs::new().exporting_to(target));
    let mut ids = Vec::new();
    for (name, colour, left) in [
        ("Blue", [0, 0, 255, 255], true),
        ("Green", [0, 255, 0, 255], false),
    ] {
        let layer = layer_model::Layer::raster(name);
        let id = layer.id;
        editor.apply_command(editor_core::Command::create_layer(layer));
        let mut rgba = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                if (x < W / 2) == left {
                    let i = ((y * W + x) * 4) as usize;
                    rgba[i..i + 4].copy_from_slice(&colour);
                }
            }
        }
        let paint = {
            let doc = editor.active_mut().unwrap();
            crate::menu_bridge::pixels::write_layer(doc, id, &rgba, "Fixture").unwrap()
        };
        editor.apply_command(paint);
        ids.push(id);
    }
    editor.set_layer_selection(ids.clone(), Some(ids[1]));
    editor
}

fn px(rgba: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// One frame of the real chrome over the shell's editor, its output applied
/// the way the shell applies it (menu picks performed).
fn chrome_frame(
    shell: &mut Shell,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    let mut out = crate::chrome::ChromeOutput::default();
    let full = ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            out = shell.chrome.ui(ctx, &mut shell.editor);
        },
    );
    shell.apply_chrome(out);
    full
}

/// Where `text` was painted, if it was.
fn text_at(full: &egui::FullOutput, text: &str) -> Option<egui::Pos2> {
    fn walk(shape: &egui::Shape, text: &str) -> Option<egui::Pos2> {
        match shape {
            egui::Shape::Text(t) if t.galley.text() == text => {
                Some(t.galley.rect.translate(t.pos.to_vec2()).center())
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| walk(s, text)),
            _ => None,
        }
    }
    full.shapes.iter().find_map(|c| walk(&c.shape, text))
}

fn click_text(shell: &mut Shell, ctx: &egui::Context, text: &str) {
    let full = chrome_frame(shell, ctx, Vec::new());
    let at = text_at(&full, text).unwrap_or_else(|| panic!("{text:?} is not drawn"));
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    let _ = chrome_frame(
        shell,
        ctx,
        vec![egui::Event::PointerMoved(at), button(true), button(false)],
    );
    let _ = chrome_frame(shell, ctx, Vec::new());
}

/// Photopea's quick export takes a scale, PNG or SVG, and the selected
/// layers. The Quick Export row opens the Quick Export window; 2x and
/// Export… there ask where (the Export picker, offered `<doc>@2x.png`) and
/// write one PNG per selected layer, each the layer alone at twice the
/// canvas size, named after the pick and the layer with the suffix kept. An
/// export is no edit.
#[test]
fn quick_export_writes_each_selected_layer_at_the_scale_picked_in_its_window() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("icons@2x.png");
    let mut shell = Shell::new(two_selected(dir.path(), &target), Vec::new());
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let _ = chrome_frame(&mut shell, &ctx, Vec::new());
    let depth = shell.editor.active().unwrap().history_depth();
    // The File row names what the window writes: the selected layers, in
    // the format picked there (not "Layer as PNG").
    assert_eq!(
        ui::menu::MenuAction::QuickExportLayer.label(),
        "Quick Export Selected Layers…"
    );
    let opened =
        crate::menu_bridge::perform(ui::menu::MenuAction::QuickExportLayer, &mut shell.editor);
    assert!(opened.is_ok(), "{opened:?}");
    assert!(crate::tool_input::quick_export::window_is_open());
    // A new window is laid out unseen on its first frame (egui's sizing
    // pass), so it is read on the second.
    let _ = chrome_frame(&mut shell, &ctx, Vec::new());
    let full = chrome_frame(&mut shell, &ctx, Vec::new());
    for label in ["PNG", "SVG", "1x", "2x", "3x", "4x"] {
        assert!(text_at(&full, label).is_some(), "the window offers {label}");
    }
    assert!(
        text_at(&full, "0.5x").is_none(),
        "Photopea's Scale for exported files is 1x to 4x"
    );
    assert!(
        text_at(&full, "2 selected layers, each to its own file").is_some(),
        "the window says what it writes"
    );
    assert!(
        !dir.path().join("icons-Blue@2x.png").exists(),
        "nothing yet"
    );
    click_text(&mut shell, &ctx, "2x");
    click_text(&mut shell, &ctx, "Export…");
    assert!(!crate::tool_input::quick_export::window_is_open(), "closed");
    assert_eq!(shell.editor.active().unwrap().history_depth(), depth);

    let blue = raster::decode_path(&dir.path().join("icons-Blue@2x.png")).unwrap();
    let green = raster::decode_path(&dir.path().join("icons-Green@2x.png")).unwrap();
    for image in [&blue, &green] {
        assert_eq!((image.width, image.height), (W * 2, H * 2));
    }
    assert_eq!(px(&blue.rgba8, W * 2, 4, 4), [0, 0, 255, 255]);
    assert_eq!(
        px(&blue.rgba8, W * 2, W * 2 - 4, 4)[3],
        0,
        "red and green hidden"
    );
    assert_eq!(px(&green.rgba8, W * 2, W * 2 - 4, 4), [0, 255, 0, 255]);
    assert_eq!(px(&green.rgba8, W * 2, 4, 4)[3], 0, "red and blue hidden");
    assert!(!target.exists(), "the pick names the set, not a file");
}

/// SVG in the Quick Export window writes the active layer alone as an SVG
/// document at canvas size, the picked name's extension set to `.svg`; the
/// other layers are not in it.
#[test]
fn quick_export_as_svg_from_its_window_writes_the_layer_alone() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mark.png");
    let editor = two_selected(dir.path(), &target);
    let mut shell = Shell::new(editor, Vec::new());
    let only = shell
        .editor
        .active()
        .unwrap()
        .document
        .active_layer()
        .unwrap();
    shell.editor.set_layer_selection(vec![only], Some(only));
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let _ = chrome_frame(&mut shell, &ctx, Vec::new());
    crate::menu_bridge::perform(ui::menu::MenuAction::QuickExportLayer, &mut shell.editor).unwrap();
    let _ = chrome_frame(&mut shell, &ctx, Vec::new());
    let full = chrome_frame(&mut shell, &ctx, Vec::new());
    assert!(text_at(&full, "The active layer, alone").is_some());
    click_text(&mut shell, &ctx, "SVG");
    click_text(&mut shell, &ctx, "Export…");
    let svg_path = dir.path().join("mark.svg");
    let svg = std::fs::read_to_string(&svg_path).unwrap();
    assert!(svg.contains("<svg"), "{svg}");
    assert!(svg.contains(&format!("width=\"{W}\"")), "{svg}");
    assert!(!target.exists(), "the format set the extension");
    // Each visible raster layer is one embedded image: the active layer
    // (Green) alone is in it, Blue and the red base are not.
    assert_eq!(svg.matches("<image ").count(), 1, "{svg}");
}

#[test]
fn the_export_suffix_names_a_scale() {
    use crate::tool_input::quick_export::scale_suffix;
    assert_eq!(scale_suffix("icon@2x"), ("icon", Some(2.0)));
    assert_eq!(scale_suffix("icon@0.5x"), ("icon", Some(0.5)));
    assert_eq!(scale_suffix("icon"), ("icon", None));
    assert_eq!(scale_suffix("me@home"), ("me@home", None));
    assert_eq!(scale_suffix("big@50x"), ("big@50x", None));
}

/// Photopea's More ▸ Use WebGL is Window ▸ Use GPU here: a checked row of
/// the Window menu, on by default. Clicked, it stores "off" through the
/// shell's own preferences path, the status line says it applies at the next
/// start, and the saved file keeps it — the start reads it to ask for the
/// software adapter (`render::context::adapter_attempts`).
#[test]
fn window_use_gpu_is_a_stored_switch_that_the_next_start_reads() {
    use ui::menu::{Entry, MenuAction};
    let dir = tempfile::tempdir().unwrap();
    let mut shell = Shell::new(editor(dir.path(), ScriptedDialogs::new()), Vec::new());
    let menus = crate::menu_bridge::menus(&shell.editor);
    let window = menus.iter().find(|m| m.title == "Window").unwrap();
    assert!(
        window
            .entries
            .iter()
            .any(|e| matches!(e, Entry::Item(MenuAction::ToggleUseGpu))),
        "Window lists Use GPU"
    );
    let ctx = crate::menu_bridge::context(&mut shell.editor, &ui::Workspace::new());
    assert_eq!(MenuAction::ToggleUseGpu.checked(&ctx), Some(true), "on");
    let intent = crate::menu_bridge::resolve_intent(MenuAction::ToggleUseGpu, &ctx, &shell.editor)
        .expect("the row is live");
    let pick = crate::menu_bridge::pick(&intent, &shell.editor).expect("the row has a route");
    let mut out = crate::chrome::ChromeOutput::default();
    crate::menu_bridge::record(pick, &mut out);
    shell.apply_chrome(out);
    assert!(!shell.editor.preferences().use_gpu, "stored off");
    assert!(
        shell
            .editor
            .status()
            .is_some_and(|s| s.contains("software renderer") && s.contains("next start")),
        "{:?}",
        shell.editor.status()
    );
    let ctx = crate::menu_bridge::context(&mut shell.editor, &ui::Workspace::new());
    assert_eq!(MenuAction::ToggleUseGpu.checked(&ctx), Some(false));
    shell.editor.persist().unwrap();
    let saved = Preferences::load(&AppPaths::rooted(dir.path().join("config")).preferences_file());
    assert!(!saved.use_gpu, "the next start reads Use GPU off");
    assert!(
        render::context::adapter_attempts(saved.use_gpu)[0].force_fallback_adapter,
        "and asks for the software adapter first"
    );
}
