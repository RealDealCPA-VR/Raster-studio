//! W13X-7 through the real routes: a multi-page PDF dropped / opened from
//! the command line (`Editor::open_paths`) parks its import dialog, the
//! chrome's own frame (`Chrome::ui`) shows it, the page is dropped by a
//! pointer click on its thumbnail, Enter confirms, and the path the chrome
//! hands back (`ChromeOutput::open_recent`) is opened the way the shell
//! applies it (`shell.rs`: `self.editor.open_paths(&[path])`). A Paint.NET
//! file opens its layers through the same `open_paths`.

use std::path::{Path, PathBuf};

use super::*;
use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::menu_bridge::pixels::read_layer;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use raster::codec::formats::vector_docs::pdn::fixture::{self, Options};

fn editor(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A well-formed PDF, one page per `(width, height, content stream)`.
fn build_pdf(pages: &[(u32, u32, &str)]) -> Vec<u8> {
    let mut objects: Vec<String> = Vec::new();
    let n = pages.len();
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 3 + i * 2)).collect();
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {n} >>",
        kids.join(" ")
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
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
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

/// Page 1 green 10x10 pt, page 2 blue 20x8 pt, page 3 yellow 6x6 pt.
fn three_pages() -> Vec<u8> {
    build_pdf(&[
        (10, 10, "0 1 0 rg 0 0 10 10 re f"),
        (20, 8, "0 0 1 rg 0 0 20 8 re f"),
        (6, 6, "1 1 0 rg 0 0 6 6 re f"),
    ])
}

fn ctx() -> egui::Context {
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    ctx
}

/// One whole chrome frame carrying `events`.
fn frame(
    chrome: &mut Chrome,
    ctx: &egui::Context,
    ed: &mut Editor,
    events: Vec<egui::Event>,
) -> ChromeOutput {
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input, |ctx| out = chrome.ui(ctx, ed));
    out
}

fn click(at: egui::Pos2) -> Vec<egui::Event> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    vec![egui::Event::PointerMoved(at), button(true), button(false)]
}

fn key(key: egui::Key) -> Vec<egui::Event> {
    vec![egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]
}

/// Frames until page `index`'s thumbnail holds still; its rectangle.
fn settled_tile(
    chrome: &mut Chrome,
    ctx: &egui::Context,
    ed: &mut Editor,
    index: usize,
) -> egui::Rect {
    let id = ui::dialogs::pdf_import::PdfImportDialog::page_id(index);
    let mut last = None;
    let mut same = 0;
    for _ in 0..30 {
        frame(chrome, ctx, ed, Vec::new());
        let rect = ctx.read_response(id).map(|r| r.rect);
        match rect {
            Some(r) if rect == last => {
                same += 1;
                if same >= 4 {
                    return r;
                }
            }
            _ => same = 0,
        }
        last = rect;
    }
    panic!("page {index}'s thumbnail never settled");
}

fn boards(ed: &Editor) -> Vec<layer_model::Artboard> {
    layer_model::artboard::artboards(&ed.active().unwrap().document.layers)
        .into_iter()
        .map(|(_, a)| a)
        .collect()
}

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

fn layer_names(ed: &Editor) -> Vec<String> {
    let doc = ed.active().unwrap();
    doc.document
        .layers
        .iter_depth_first()
        .into_iter()
        .map(|id| doc.document.layers.get(id).unwrap().name.clone())
        .collect()
}

/// Open `pdf` from the command line / a drop, let the chrome show the
/// dialog, click page 2's thumbnail off, set 144 dpi, press Enter, and open
/// what the chrome hands back as the shell does.
fn open_pages_1_and_3_at_144(dir: &Path) -> (Editor, PathBuf) {
    let pdf = write(dir, "brochure.pdf", &three_pages());
    let mut ed = editor(dir);
    assert!(ed.open_paths(std::slice::from_ref(&pdf)).is_empty());
    assert!(ed.documents().is_empty(), "nothing opens before the answer");
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("choose the pages to open"), "{status}");

    let mut chrome = Chrome::new();
    let ctx = ctx();
    let tile = settled_tile(&mut chrome, &ctx, &mut ed, 1);
    assert!(chrome.dialog_open(), "the chrome shows the import dialog");
    {
        let dialog = chrome.dialogs_for_test().active_pdf_import_for_test();
        assert_eq!(dialog.page_count(), 3);
        assert_eq!(dialog.thumbnails_loaded(), 3, "a thumbnail per page");
        assert!((0..3).all(|i| dialog.is_selected(i)), "every page to start");
    }
    let out = frame(&mut chrome, &ctx, &mut ed, click(tile.center()));
    assert!(out.open_recent.is_none());
    {
        let dialog = chrome.dialogs_for_test().active_pdf_import_for_test();
        assert!(!dialog.is_selected(1), "the click dropped page 2");
        assert!(dialog.is_selected(0) && dialog.is_selected(2));
        dialog.set_dpi(144);
    }
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    let out = frame(&mut chrome, &ctx, &mut ed, key(egui::Key::Enter));
    assert!(!chrome.dialog_open(), "Enter confirms");
    assert_eq!(out.open_recent.as_deref(), Some(pdf.as_path()));
    assert!(ed.documents().is_empty(), "the chrome opens nothing itself");
    // The shell's apply (`shell.rs`: open_recent -> open_paths).
    ed.open_paths(&[out.open_recent.unwrap()]);
    (ed, pdf)
}

#[test]
fn a_three_page_pdf_opens_pages_1_and_3_at_144_dpi_through_the_import_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let (ed, _) = open_pages_1_and_3_at_144(dir.path());
    assert_eq!(ed.documents().len(), 1, "one document");
    let rects: Vec<_> = boards(&ed)
        .iter()
        .map(|a| (a.x, a.y, a.width, a.height))
        .collect();
    assert_eq!(
        rects,
        vec![(0, 0, 20, 20), (256, 0, 12, 12)],
        "pages 1 and 3 only, each twice its size in points"
    );
    let names = layer_names(&ed);
    assert!(names.iter().any(|n| n == "Page 1"), "{names:?}");
    assert!(names.iter().any(|n| n == "Page 3"), "{names:?}");
    assert!(!names.iter().any(|n| n == "Page 2"), "{names:?}");
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("(pages 1 and 3) at 144 dpi"), "{status}");
    let (rgba, w) = shown(&ed);
    assert_eq!(at(&rgba, w, 19, 19), [0, 255, 0, 255], "page 1 fills 20x20");
    assert_eq!(
        at(&rgba, w, 256 + 11, 11),
        [255, 255, 0, 255],
        "page 3 fills 12x12"
    );
    let doc = ed.active().unwrap();
    assert!(
        !doc.is_dirty() && !doc.history.can_undo(),
        "opening is not an edit"
    );
}

#[test]
fn revert_reads_back_the_pages_and_resolution_the_dialog_chose() {
    let dir = tempfile::tempdir().unwrap();
    let (mut ed, _) = open_pages_1_and_3_at_144(dir.path());
    let before = boards(&ed);
    let saved = shown(&ed);
    let first = ed.active().unwrap().document.layers.root()[0];
    ed.apply_command(editor_core::Command::DeleteLayer { layer_id: first });
    assert_eq!(boards(&ed).len(), 1);
    let message = ed.revert_active().unwrap();
    assert!(message.starts_with("Reverted to"), "{message}");
    assert_eq!(boards(&ed), before);
    assert_eq!(shown(&ed), saved);
}

#[test]
fn separate_documents_open_one_document_per_chosen_page_and_escape_opens_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = write(dir.path(), "deck.pdf", &three_pages());
    let mut ed = editor(dir.path());
    let mut chrome = Chrome::new();
    let ctx = ctx();

    // Escape: nothing opens, nothing is handed back.
    ed.open_paths(std::slice::from_ref(&pdf));
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    assert!(chrome.dialog_open());
    let out = frame(&mut chrome, &ctx, &mut ed, key(egui::Key::Escape));
    assert!(!chrome.dialog_open() && out.open_recent.is_none());
    assert!(ed.documents().is_empty());

    ed.open_paths(std::slice::from_ref(&pdf));
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    {
        let dialog = chrome.dialogs_for_test().active_pdf_import_for_test();
        dialog.select(0, false);
        dialog.set_mode(ui::dialogs::pdf_import::PdfOpenMode::SeparateDocuments);
        dialog.set_dpi(36);
    }
    let out = frame(&mut chrome, &ctx, &mut ed, key(egui::Key::Enter));
    ed.open_paths(&[out.open_recent.expect("confirmed")]);
    let docs = ed.documents();
    assert_eq!(docs.len(), 2, "pages 2 and 3, one document each");
    let sizes: Vec<_> = docs
        .iter()
        .map(|d| (d.document.width(), d.document.height()))
        .collect();
    assert_eq!(sizes, vec![(10, 4), (3, 3)], "half their size in points");
    assert_eq!(docs[0].document.meta.title, "deck.pdf - Page 2");
    assert_eq!(docs[1].document.meta.title, "deck.pdf - Page 3");
    assert!(
        boards(&ed).is_empty(),
        "a page of its own is not an artboard"
    );
}

fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    (0..w * h).flat_map(|_| px).collect()
}

fn two_layer_pdn(options: Options) -> Vec<u8> {
    let bottom = solid(6, 4, [200, 10, 20, 255]);
    let mut top = solid(6, 4, [0, 0, 0, 0]);
    top[..4].copy_from_slice(&[10, 220, 30, 255]);
    fixture::pdn_file(
        6,
        4,
        &[
            fixture::Layer {
                name: "Background",
                visible: true,
                opacity: 255,
                blend: "Normal",
                is_background: true,
                rgba: &bottom,
            },
            fixture::Layer {
                name: "Ink",
                visible: false,
                opacity: 102,
                blend: "Multiply",
                is_background: false,
                rgba: &top,
            },
        ],
        options,
    )
}

#[test]
fn a_two_layer_paint_dot_net_file_opens_as_two_layers() {
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "art.pdn", &two_layer_pdn(Options::default()));
    let mut ed = editor(dir.path());
    assert_eq!(ed.open_paths(std::slice::from_ref(&path)).len(), 0);
    assert_eq!(ed.documents().len(), 1);
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (6, 4));
    let order = doc.document.layers.root().to_vec();
    assert_eq!(order.len(), 2, "two layers");
    let (top, bottom) = (
        doc.document.layers.get(order[0]).unwrap(),
        doc.document.layers.get(order[1]).unwrap(),
    );
    assert_eq!(top.name, "Ink");
    assert!(!top.visible);
    assert!((top.opacity - 0.4).abs() < 1e-6, "{}", top.opacity);
    assert_eq!(top.blend_mode, layer_model::BlendMode::Multiply);
    assert_eq!(bottom.name, "Background");
    assert!(bottom.visible);
    assert_eq!(bottom.opacity, 1.0);
    assert_eq!(bottom.blend_mode, layer_model::BlendMode::Normal);
    assert_eq!(doc.document.active_layer(), Some(order[0]));
    let ink = read_layer(doc, order[0]);
    assert_eq!(&ink[..8], &[10, 220, 30, 255, 0, 0, 0, 0]);
    let paper = read_layer(doc, order[1]);
    assert_eq!(&paper[20..24], &[200, 10, 20, 255]);
    assert!(!doc.is_dirty() && !doc.history.can_undo());
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("its 2 Paint.NET layers"), "{status}");
}

#[test]
fn a_paint_dot_net_file_whose_layers_cannot_be_read_opens_its_thumbnail_and_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let thumb: &'static [u8] = Box::leak(
        raster::encode(
            raster::ExportFormat::Png,
            3,
            2,
            &solid(3, 2, [9, 99, 199, 255]),
        )
        .unwrap()
        .into_boxed_slice(),
    );
    let good = two_layer_pdn(Options {
        thumb_png: Some(thumb),
        ..Options::default()
    });
    // Keep the header (and its thumbnail); cut the object graph short.
    let header = 7 + (usize::from(good[4]) | usize::from(good[5]) << 8);
    let cut = &good[..header + 40];
    let path = write(dir.path(), "cut.pdn", cut);
    let mut ed = editor(dir.path());
    ed.open_paths(std::slice::from_ref(&path));
    assert_eq!(ed.documents().len(), 1, "the thumbnail opens");
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (3, 2));
    let status = ed.status().unwrap_or_default().to_string();
    assert!(status.contains("flattened thumbnail"), "{status}");
    assert!(status.contains("its layers could not be read"), "{status}");
}

fn png_thumb(w: u32, h: u32) -> &'static [u8] {
    Box::leak(
        raster::encode(
            raster::ExportFormat::Png,
            w,
            h,
            &solid(w, h, [9, 99, 199, 255]),
        )
        .unwrap()
        .into_boxed_slice(),
    )
}

fn root_names(ed: &Editor) -> Vec<String> {
    let doc = ed.active().unwrap();
    doc.document
        .layers
        .root()
        .iter()
        .map(|id| doc.document.layers.get(*id).unwrap().name.clone())
        .collect()
}

#[test]
fn revert_of_a_paint_dot_net_file_reads_its_layers_back_not_its_thumbnail() {
    let dir = tempfile::tempdir().unwrap();
    // A 3x2 thumbnail in front of a 6x4, two-layer document.
    let bytes = two_layer_pdn(Options {
        thumb_png: Some(png_thumb(3, 2)),
        ..Options::default()
    });
    let path = write(dir.path(), "art.pdn", &bytes);
    let mut ed = editor(dir.path());
    ed.open_paths(std::slice::from_ref(&path));
    assert_eq!(root_names(&ed), vec!["Ink", "Background"]);
    let saved = shown(&ed);
    let top = ed.active().unwrap().document.layers.root()[0];
    ed.apply_command(editor_core::Command::DeleteLayer { layer_id: top });
    assert_eq!(root_names(&ed), vec!["Background"]);

    let message = ed.revert_active().unwrap();
    assert!(message.starts_with("Reverted to"), "{message}");
    assert_eq!(root_names(&ed), vec!["Ink", "Background"]);
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (6, 4));
    let order = doc.document.layers.root().to_vec();
    let ink = doc.document.layers.get(order[0]).unwrap();
    assert!(!ink.visible && (ink.opacity - 0.4).abs() < 1e-6);
    assert_eq!(ink.blend_mode, layer_model::BlendMode::Multiply);
    assert_eq!(shown(&ed), saved, "the same pixels as the open");
    assert!(!ed.active().unwrap().is_dirty());
}

#[test]
fn revert_of_layers_whose_file_no_longer_yields_them_refuses_instead_of_flattening() {
    let dir = tempfile::tempdir().unwrap();
    let good = two_layer_pdn(Options {
        thumb_png: Some(png_thumb(3, 2)),
        ..Options::default()
    });
    let path = write(dir.path(), "art.pdn", &good);
    let mut ed = editor(dir.path());
    ed.open_paths(std::slice::from_ref(&path));
    assert_eq!(root_names(&ed), vec!["Ink", "Background"]);
    // The file on disk loses its object graph; its thumbnail stays.
    let header = 7 + (usize::from(good[4]) | usize::from(good[5]) << 8);
    std::fs::write(&path, &good[..header + 40]).unwrap();
    let err = ed.revert_active().unwrap_err();
    assert!(err.contains("); nothing was reverted"), "{err}");
    assert!(!err.contains("  "), "no run of spaces in {err:?}");
    assert_eq!(root_names(&ed), vec!["Ink", "Background"]);
    let doc = ed.active().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (6, 4));
}

#[test]
fn two_multi_page_pdfs_opened_together_each_ask_in_turn() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.pdf", &three_pages());
    let b = write(dir.path(), "b.pdf", &three_pages());
    let mut ed = editor(dir.path());
    let mut chrome = Chrome::new();
    let ctx = ctx();
    // One drop (or command line) naming both.
    ed.open_paths(&[a.clone(), b.clone()]);
    assert!(ed.documents().is_empty());

    let mut opened = Vec::new();
    for (expected, page) in [(&a, 0usize), (&b, 2usize)] {
        frame(&mut chrome, &ctx, &mut ed, Vec::new());
        assert!(chrome.dialog_open(), "the dialog for {expected:?} is up");
        {
            let dialog = chrome.dialogs_for_test().active_pdf_import_for_test();
            assert_eq!(dialog.page_count(), 3);
            for i in 0..3 {
                dialog.select(i, i == page);
            }
        }
        let out = frame(&mut chrome, &ctx, &mut ed, key(egui::Key::Enter));
        assert_eq!(out.open_recent.as_deref(), Some(expected.as_path()));
        ed.open_paths(&[out.open_recent.unwrap()]);
        opened.push(names_of_active(&ed));
    }
    frame(&mut chrome, &ctx, &mut ed, Vec::new());
    assert!(!chrome.dialog_open(), "nothing else was asked for");
    assert_eq!(ed.documents().len(), 2, "one document per file");
    assert_eq!(opened, vec![vec!["Page 1"], vec!["Page 3"]]);
    assert_eq!(ed.documents()[0].document.meta.title, "a.pdf");
    assert_eq!(ed.documents()[1].document.meta.title, "b.pdf");
}

/// The distinct page names in the active artboard document (an artboard
/// and its page layer share one).
fn names_of_active(ed: &Editor) -> Vec<String> {
    let mut names: Vec<String> = layer_names(ed)
        .into_iter()
        .filter(|n| n.starts_with("Page "))
        .collect();
    names.dedup();
    names
}

#[test]
fn the_same_pdf_asked_for_twice_before_an_answer_asks_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a.pdf", &three_pages());
    let mut ed = editor(dir.path());
    ed.open_paths(&[a.clone(), a.clone()]);
    let first = take_pending().expect("asked");
    assert_eq!(first.path, a);
    assert!(take_pending().is_none(), "asked once");
}

#[test]
fn the_dialogs_resolution_range_is_the_renderers() {
    // The dialog blocks what the renderer would refuse, and no more.
    let d = ui::dialogs::pdf_import::PdfImportDialog::new("x.pdf", 2, Vec::new());
    assert_eq!(d.dpi(), raster::codec::formats::pdf::DEFAULT_DPI);
    let bytes = three_pages();
    let limits = raster::ImportLimits::default();
    for dpi in [pdf::MIN_DPI - 1, pdf::MAX_DPI + 1] {
        assert!(pdf::render_selected(&bytes, &[0], dpi, limits).is_err());
    }
    for dpi in [pdf::MIN_DPI, pdf::MAX_DPI] {
        assert!(pdf::render_selected(&bytes, &[0], dpi, limits).is_ok());
    }
}
