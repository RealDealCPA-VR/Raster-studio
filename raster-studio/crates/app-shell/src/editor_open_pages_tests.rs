//! W13-D: the real open routes reach the new document formats: File > Open
//! (the picker) makes one artboard per PDF page, a drop / File > Open
//! Recent opens the preview formats and says what was opened, a bare Figma
//! canvas is refused by name, and File > Revert rebuilds the artboards.

use std::path::{Path, PathBuf};

use raster::ExportFormat;
use ui::menu::MenuAction;

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{self, Pick};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::shell::Shell;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A well-formed PDF, one page per `(width, height, content stream)`.
fn build_pdf(pages: &[(u32, u32, &str)]) -> Vec<u8> {
    let mut objects: Vec<String> = vec!["<< /Type /Catalog /Pages 2 0 R >>".into()];
    let kids: Vec<String> = (0..pages.len())
        .map(|i| format!("{} 0 R", 3 + i * 2))
        .collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.join(" "),
        pages.len()
    ));
    for (i, (w, h, content)) in pages.iter().enumerate() {
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] /Contents {} 0 R >>",
            4 + i * 2
        ));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len() + 1
        ));
    }
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for o in offsets {
        out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Three pages: a green 10x10, a 20x8 whose left half is blue (the right
/// half unpainted, so the white artboard shows), and a yellow 6x6.
fn three_pages() -> Vec<u8> {
    build_pdf(&[
        (10, 10, "0 1 0 rg 0 0 10 10 re f"),
        (20, 8, "0 0 1 rg 0 0 10 8 re f"),
        (6, 6, "1 1 0 rg 0 0 6 6 re f"),
    ])
}

/// The active document composited, as the canvas shows it.
fn shown(ed: &Editor) -> (Vec<u8>, u32) {
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
    (canvas.to_rgba8(&doc.document.meta.color_space), rect.width)
}

fn at(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

fn boards(ed: &Editor) -> Vec<layer_model::Artboard> {
    layer_model::artboard::artboards(&ed.active().unwrap().document.layers)
        .into_iter()
        .map(|(_, a)| a)
        .collect()
}

/// File > Open of a three-page PDF: one document, one artboard per page,
/// each page's pixels inside its own artboard, nothing to undo, clean.
#[test]
fn file_open_of_a_multi_page_pdf_makes_one_artboard_per_page() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "brochure.pdf", &three_pages());
    let mut ed = editor(dir.path(), ScriptedDialogs::new().opening(&pdf));
    ed.dispatch(Action::Open).unwrap();
    ed.poll_imports();
    // W13X-7: a multi-page PDF asks first; take its opening answer.
    crate::editor::open_any::w13x7::accept_import_defaults_for_test(&mut ed);
    assert_eq!(ed.documents().len(), 1, "one document");
    let boards = boards(&ed);
    let rects: Vec<_> = boards
        .iter()
        .map(|a| (a.x, a.y, a.width, a.height))
        .collect();
    assert_eq!(
        rects,
        vec![(0, 0, 10, 10), (256, 0, 20, 8), (512, 0, 6, 6)],
        "one artboard per page, the page's size, on tile boundaries"
    );
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("3 pages, one artboard each"), "{status}");
    {
        let doc = ed.active().unwrap();
        assert!(!doc.is_dirty(), "opening is not an edit");
        assert!(!doc.history.can_undo(), "nothing to undo");
        let active = doc.document.active_layer().unwrap();
        assert_eq!(doc.document.layers.get(active).unwrap().name, "Page 1");
    }
    let (rgba, w) = shown(&ed);
    assert_eq!(at(&rgba, w, 5, 5), [0, 255, 0, 255], "page 1");
    assert_eq!(
        at(&rgba, w, 256 + 4, 4),
        [0, 0, 255, 255],
        "page 2's blue half"
    );
    assert_eq!(
        at(&rgba, w, 256 + 15, 4),
        [255, 255, 255, 255],
        "page 2's unpainted half shows its white artboard"
    );
    assert_eq!(at(&rgba, w, 512 + 3, 3), [255, 255, 0, 255], "page 3");
}

/// A one-page PDF is a plain picture, not an artboard.
#[test]
fn a_single_page_pdf_opens_as_one_picture() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(
        dir.path(),
        "card.ai",
        &build_pdf(&[(12, 7, "1 0 0 rg 0 0 12 7 re f")]),
    );
    let mut shell = Shell::new(editor(dir.path(), ScriptedDialogs::new()), Vec::new());
    shell.on_dropped_files(&[pdf]);
    let ed = shell.editor();
    assert_eq!(ed.documents().len(), 1);
    assert!(boards(ed).is_empty());
    let (rgba, w) = shown(ed);
    assert_eq!(w, 12);
    assert_eq!(at(&rgba, w, 6, 3), [255, 0, 0, 255]);
}

/// A DOS EPS with a TIFF preview.
fn eps() -> Vec<u8> {
    let px: Vec<u8> = (0..4 * 3).flat_map(|_| [30u8, 60, 220, 255]).collect();
    let tiff = raster::encode(ExportFormat::Tiff, 4, 3, &px).unwrap();
    let ps = b"%!PS-Adobe-3.0 EPSF-3.0\n";
    let mut v = vec![0xC5, 0xD0, 0xD3, 0xC6];
    let tiff_at = 30 + ps.len() as u32;
    for x in [30, ps.len() as u32, 0, 0, tiff_at, tiff.len() as u32] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.extend_from_slice(&[0xFF, 0xFF]);
    v.extend_from_slice(ps);
    v.extend_from_slice(&tiff);
    v
}

/// A placeable WMF: a red rectangle filling a 20 x 10 px frame.
fn wmf() -> Vec<u8> {
    let rec = |func: u16, params: &[i16]| {
        let mut v = ((3 + params.len()) as u32).to_le_bytes().to_vec();
        v.extend_from_slice(&func.to_le_bytes());
        for p in params {
            v.extend_from_slice(&p.to_le_bytes());
        }
        v
    };
    let mut v = vec![0xD7, 0xCD, 0xC6, 0x9A, 0, 0];
    for c in [0i16, 0, 300, 150] {
        v.extend_from_slice(&c.to_le_bytes());
    }
    v.extend_from_slice(&1440u16.to_le_bytes());
    v.extend_from_slice(&[0; 6]);
    v.extend_from_slice(&[1, 0, 9, 0, 0, 3]);
    v.extend_from_slice(&[0; 12]);
    let red = [i16::from_le_bytes([255, 0]), 0];
    for r in [
        rec(0x02FA, &[5, 0, 0, 0, 0]),
        rec(0x012D, &[0]),
        rec(0x02FC, &[0, red[0], red[1], 0]),
        rec(0x012D, &[1]),
        rec(0x041B, &[150, 300, 0, 0]),
        rec(0, &[]),
    ] {
        v.extend_from_slice(&r);
    }
    v
}

/// A stored ZIP of `entries`.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let local = out.len() as u32;
        out.extend_from_slice(b"PK\x03\x04");
        out.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(name.len() as u16).to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(data);
        central.extend_from_slice(b"PK\x01\x02");
        central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(data.len() as u32).to_le_bytes());
        central.extend_from_slice(&(name.len() as u16).to_le_bytes());
        central.extend_from_slice(&[0; 12]);
        central.extend_from_slice(&local.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let dir_at = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&dir_at.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    out
}

fn png(w: u32, h: u32, c: [u8; 4]) -> Vec<u8> {
    let px: Vec<u8> = (0..w * h).flat_map(|_| c).collect();
    raster::encode(ExportFormat::Png, w, h, &px).unwrap()
}

/// Each preview format through a real route, each with a status line that
/// says what was read: an EPS dropped on an empty window opens its TIFF
/// preview; a WMF from File > Open Recent / the command line (`open_paths`)
/// draws; a Sketch file from File > Open (the picker) opens its preview; a
/// bare Figma canvas dropped is refused by name and opens nothing.
#[test]
fn the_preview_formats_open_through_every_route_and_say_what_was_read() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();

    let mut shell = Shell::new(editor(d, ScriptedDialogs::new()), Vec::new());
    shell.on_dropped_files(&[write(d, "logo.eps", &eps())]);
    let status = shell.editor().status().unwrap_or_default().to_string();
    assert!(status.contains("embedded TIFF preview"), "{status}");
    let (rgba, w) = shown(shell.editor());
    assert_eq!((w, at(&rgba, w, 1, 1)), (4, [30, 60, 220, 255]));

    let mut ed = editor(d, ScriptedDialogs::new());
    ed.open_paths(&[write(d, "chart.wmf", &wmf())]);
    assert_eq!(ed.documents().len(), 1);
    let (rgba, w) = shown(&ed);
    assert_eq!((w, at(&rgba, w, 10, 5)), (20, [255, 0, 0, 255]));
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("WMF drawing records"), "{status}");

    let sketch = write(
        d,
        "screens.sketch",
        &zip(&[
            ("document.json", b"{}"),
            ("previews/preview.png", &png(9, 5, [5, 150, 90, 255])),
        ]),
    );
    let mut ed = editor(d, ScriptedDialogs::new().opening(&sketch));
    ed.dispatch(Action::Open).unwrap();
    ed.poll_imports();
    assert_eq!(ed.documents().len(), 1);
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("embedded preview image"), "{status}");
    let (rgba, w) = shown(&ed);
    assert_eq!((w, at(&rgba, w, 4, 2)), (9, [5, 150, 90, 255]));

    let mut empty = Shell::new(editor(d, ScriptedDialogs::new()), Vec::new());
    empty.on_dropped_files(&[write(d, "canvas.fig", b"fig-kiwi\x0f\0\0\0...")]);
    assert!(empty.editor().documents().is_empty(), "nothing opened");
    let status = empty.editor().status().unwrap_or_default().to_string();
    assert!(status.starts_with("Drop failed"), "{status}");
    assert!(status.contains("fig-kiwi"), "{status}");
}

/// File > Revert of a multi-page PDF puts its artboards back as one step.
#[test]
fn revert_of_a_multi_page_pdf_restores_the_artboards() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "brochure.pdf", &three_pages());
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_any(&pdf).unwrap();
    // W13X-7: a multi-page PDF asks first; take its opening answer.
    crate::editor::open_any::w13x7::accept_import_defaults_for_test(&mut ed);
    let saved = shown(&ed);
    let before = boards(&ed);
    // An edit: delete the first artboard group.
    let first = ed.active().unwrap().document.layers.root()[0];
    ed.apply_command(editor_core::Command::DeleteLayer { layer_id: first });
    assert_eq!(boards(&ed).len(), 2);
    let context = menu_bridge::context(&mut ed, &ui::Workspace::new());
    let message = match menu_bridge::resolve(MenuAction::Revert, &context, &ed).unwrap() {
        Pick::Menu(action) => menu_bridge::perform(action, &mut ed).unwrap(),
        other => panic!("Revert resolved to {other:?}"),
    };
    assert!(message.starts_with("Reverted to"), "{message}");
    assert_eq!(boards(&ed), before, "every page artboard is back");
    assert_eq!(shown(&ed), saved, "and every page's pixels");
}
