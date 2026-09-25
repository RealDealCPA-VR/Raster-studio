//! W13X-8: Sketch, XD and Figma files open through the real routes as
//! layers: an artboard holding a shape layer, a text layer and a raster
//! layer, drawn on the canvas, with an import report for what did not map;
//! a damaged document falls back to its preview and says why; File > Revert
//! rebuilds the layers.

use std::path::{Path, PathBuf};

use layer_model::{LayerKind, ShapeStrokeAlign};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::shell::Shell;

/// The same synthetic files `raster`'s own tests read.
#[path = "../../raster/src/formats/design_fixtures.rs"]
mod fixtures;

/// The 4x3 green image every fixture holds.
fn image() -> Vec<u8> {
    let px: Vec<u8> = (0..4 * 3).flat_map(|_| [0u8, 200, 0, 255]).collect();
    raster::encode(raster::ExportFormat::Png, 4, 3, &px).unwrap()
}

/// Reads back what the editor reported: the editor owns its
/// `Box<dyn FileDialogs>`, so the notices sit behind an `Rc`.
#[derive(Default, Clone)]
struct NoticeSpy {
    inner: std::rc::Rc<std::cell::RefCell<ScriptedDialogs>>,
    notices: std::rc::Rc<std::cell::RefCell<Vec<(String, String)>>>,
}

impl crate::dialogs::FileDialogs for NoticeSpy {
    fn pick_open_file(&mut self) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_open_file()
    }
    fn pick_place_file(&mut self) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_place_file()
    }
    fn pick_replace_file(&mut self) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_replace_file()
    }
    fn pick_open_project(&mut self) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_open_project()
    }
    fn pick_save_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_save_path(suggested)
    }
    fn pick_export_path(&mut self, suggested: &Path) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_export_path(suggested)
    }
    fn pick_export_folder(&mut self) -> Option<PathBuf> {
        self.inner.borrow_mut().pick_export_folder()
    }
    fn confirm_close(&mut self, _document: &str) -> crate::dialogs::CloseChoice {
        crate::dialogs::CloseChoice::Cancel
    }
    fn confirm_recover(&mut self, _document: &str) -> bool {
        false
    }
    fn report_error(&mut self, title: &str, message: &str) {
        panic!("unexpected error {title}: {message}");
    }
    fn report_notice(&mut self, title: &str, message: &str) {
        self.notices
            .borrow_mut()
            .push((title.to_string(), message.to_string()));
    }
}

fn editor(dir: &Path, spy: &NoticeSpy) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(spy.clone()),
    )
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The active document composited, as the canvas shows it.
fn shown(ed: &Editor) -> (Vec<u8>, u32, u32) {
    let doc = ed.active().unwrap();
    let rect = doc.canvas_rect();
    let canvas = compositor::composite_region(
        &doc.document,
        &doc.tiles,
        rect,
        0,
        compositor::CompositeOptions::default(),
    )
    .unwrap();
    (
        canvas.to_rgba8(&doc.document.meta.color_space),
        rect.width,
        rect.height,
    )
}

fn at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

/// The opened document is one artboard (at the canvas origin, 200x100,
/// white) holding, bottom to top, the "Box" shape layer (red fill, blue
/// inside stroke, 50% opacity), the "Title" text layer and the "Photo"
/// raster layer; the canvas shows them.
fn assert_design_document(ed: &Editor, text_y: f32) {
    let doc = ed.active().expect("a document opened");
    let layers = &doc.document.layers;
    assert_eq!(
        (doc.document.width(), doc.document.height()),
        (200, 100),
        "the canvas is the artboard"
    );
    let boards = layer_model::artboard::artboards(layers);
    assert_eq!(boards.len(), 1, "one artboard");
    let (group, board) = boards[0];
    assert_eq!(
        (board.x, board.y, board.width, board.height),
        (0, 0, 200, 100)
    );
    assert_eq!(layers.root(), &[group]);
    let LayerKind::Group(g) = &layers.get(group).unwrap().kind else {
        panic!("the artboard is a group")
    };
    assert_eq!(layers.get(group).unwrap().name, "Board");
    // Top-most first: image, text, shape, then the background plate.
    let names: Vec<&str> = g
        .children
        .iter()
        .map(|id| layers.get(*id).unwrap().name.as_str())
        .collect();
    assert_eq!(names, ["Photo", "Title", "Box", "Artboard Background"]);

    let photo = layers.get(g.children[0]).unwrap();
    assert!(
        matches!(photo.kind, LayerKind::Raster(_)),
        "{:?}",
        photo.kind
    );

    let title = layers.get(g.children[1]).unwrap();
    let LayerKind::Text(t) = &title.kind else {
        panic!("a text layer, got {:?}", title.kind)
    };
    assert_eq!(
        (t.text.as_str(), t.font_family.as_str(), t.size_px),
        ("Hello", "Helvetica", 24.0)
    );
    assert_eq!(t.style.weight, layer_model::text::Weight::BOLD);
    // Blue, stored in linear light.
    assert_eq!(t.style.fill, [0.0, 0.0, 1.0, 1.0]);
    let origin = title.transform.translation;
    assert!(
        (origin.x - 60.0).abs() < 1e-4 && (origin.y - text_y).abs() < 1e-4,
        "text at {origin:?}"
    );

    let shape = layers.get(g.children[2]).unwrap();
    let LayerKind::Shape(s) = &shape.kind else {
        panic!("a shape layer, got {:?}", shape.kind)
    };
    assert_eq!(s.fill, Some([1.0, 0.0, 0.0, 1.0]));
    assert_eq!(shape.opacity, 0.5);
    assert_eq!(shape.transform.translation, glam::vec2(10.0, 10.0));

    // The canvas: white board, the half-opaque red box, the green image.
    let (rgba, w, _) = shown(ed);
    assert_eq!(at(&rgba, w, 150, 80), [255, 255, 255, 255], "white board");
    assert_eq!(at(&rgba, w, 11, 41), [0, 200, 0, 255], "the image");
    let boxed = at(&rgba, w, 30, 20);
    assert!(
        boxed[0] == 255 && boxed[1] > 100 && boxed[1] < 250 && boxed[1] == boxed[2],
        "half-opaque red over white: {boxed:?}"
    );
    if let Some(stroke) = &s.stroke {
        assert_eq!(stroke.align, ShapeStrokeAlign::Inside);
        assert_eq!(stroke.width_px, 2.0);
        // The inside stroke is blue-tinted at the box's inner edge.
        let edge = at(&rgba, w, 10, 20);
        assert!(edge[2] > edge[1], "blue stroke at the edge: {edge:?}");
    }
}

/// File > Open (the picker) of a Sketch file: an artboard holding a
/// rectangle, a text and an image as three layers, and the import report
/// names the shadow and the symbol instance that did not map.
#[test]
fn file_open_of_a_sketch_file_makes_an_artboard_of_three_layers() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "screens.sketch",
        &fixtures::sketch_file(&image()),
    );
    let spy = NoticeSpy::default();
    spy.inner.borrow_mut().open_files.push(path.clone());
    let mut ed = editor(dir.path(), &spy);
    ed.dispatch(Action::Open).unwrap();
    ed.poll_imports();
    assert_eq!(ed.documents().len(), 1);
    assert_design_document(&ed, 10.0);
    let status = ed.status().unwrap_or_default().to_string();
    assert!(
        status.contains("its layers: 1 artboard and 3 layers from page \"Page 1\""),
        "{status}"
    );
    let notices = spy.notices.borrow();
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (title, message) = &notices[0];
    assert_eq!(title, "Sketch import report");
    assert!(message.contains("\"Box\" has a drop shadow"), "{message}");
    assert!(message.contains("symbol instance \"Button\""), "{message}");
    assert!(!ed.active().unwrap().is_dirty(), "opening is not an edit");
}

/// A dropped XD file opens the same three layers in an artboard.
#[test]
fn a_dropped_xd_file_makes_an_artboard_of_three_layers() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "app.xd", &fixtures::xd_file(&image()));
    let spy = NoticeSpy::default();
    let mut shell = Shell::new(editor(dir.path(), &spy), Vec::new());
    shell.on_dropped_files(&[path]);
    assert_eq!(shell.editor().documents().len(), 1);
    // XD's point text sits on its baseline (34); the box starts 0.8 em up.
    assert_design_document(shell.editor(), 34.0 - 19.2);
    let notices = spy.notices.borrow();
    assert_eq!(notices[0].0, "Adobe XD import report", "{notices:?}");
}

/// A Figma file (File > Open Recent's route) opens the same layers; File >
/// Revert after deleting the artboard puts them back.
#[test]
fn a_figma_file_opens_as_layers_and_revert_rebuilds_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "design.fig",
        &fixtures::fig_file(&image(), &fixtures::stored_deflate),
    );
    let spy = NoticeSpy::default();
    let mut ed = editor(dir.path(), &spy);
    ed.open_any(&path).unwrap();
    assert_design_document(&ed, 10.0);
    let board = ed.active().unwrap().document.layers.root()[0];
    ed.apply_command(editor_core::Command::DeleteLayer { layer_id: board });
    assert!(ed.active().unwrap().document.layers.root().is_empty());
    ed.revert_active().unwrap();
    assert_design_document(&ed, 10.0);
}

/// A Sketch file whose document is damaged opens its preview, and the
/// status line says the layers could not be read and why.
#[test]
fn a_damaged_sketch_document_falls_back_to_its_preview_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let px: Vec<u8> = (0..6 * 4).flat_map(|_| [5u8, 150, 90, 255]).collect();
    let preview = raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap();
    let zip = fixtures::build_zip(&[
        ("document.json", b"{\"pages\": ["),
        ("previews/preview.png", &preview),
    ]);
    let path = write(dir.path(), "broken.sketch", &zip);
    let spy = NoticeSpy::default();
    let mut ed = editor(dir.path(), &spy);
    ed.open_any(&path).unwrap();
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("embedded preview image"), "{status}");
    assert!(status.contains("its layers could not be read"), "{status}");
    assert!(
        status.contains("document.json is not valid JSON"),
        "{status}"
    );
    let (rgba, w, _) = shown(&ed);
    assert_eq!((w, at(&rgba, w, 2, 2)), (6, [5, 150, 90, 255]));
}
