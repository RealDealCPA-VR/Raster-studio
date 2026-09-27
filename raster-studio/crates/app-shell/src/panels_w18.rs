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
    let mut editor = editor(dir, ScriptedDialogs::new().saving_to(target));
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

/// Quick Export with two layers selected and a `@2x` name: one PNG per
/// layer, each the layer alone, at twice the canvas size, named after the
/// pick and the layer with the suffix kept; an export is no edit.
#[test]
fn quick_export_writes_each_selected_layer_at_the_picked_scale() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("icons@2x.png");
    let mut editor = two_selected(dir.path(), &target);
    let depth = editor.active().unwrap().history_depth();
    let status = crate::menu_bridge::perform(ui::menu::MenuAction::QuickExportLayer, &mut editor);
    assert!(status.is_ok(), "{status:?}");
    assert_eq!(editor.active().unwrap().history_depth(), depth);

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

/// Quick Export to a `.svg` writes the layer alone as an SVG document at
/// canvas size.
#[test]
fn quick_export_to_an_svg_name_writes_svg() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("mark.svg");
    let mut editor = two_selected(dir.path(), &target);
    let only = editor.active().unwrap().document.active_layer().unwrap();
    editor.set_layer_selection(vec![only], Some(only));
    crate::menu_bridge::perform(ui::menu::MenuAction::QuickExportLayer, &mut editor).unwrap();
    let svg = std::fs::read_to_string(&target).unwrap();
    assert!(svg.contains("<svg"), "{svg}");
    assert!(svg.contains(&format!("width=\"{W}\"")), "{svg}");
    assert!(!dir.path().join("mark.png").exists());
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
