//! W11-D: every open route reaches the library importers, Paste with no
//! document opens the clipboard's image, and File > Revert is one undoable
//! history step.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use psd::bytes::Sink;
use psd::{Descriptor, Value};
use ui::menu::MenuAction;

use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{self, Pick};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use crate::shell::Shell;

fn editor(dir: &Path) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// A 16 x 12 PNG, every pixel `value`.
fn png(dir: &Path, name: &str, value: u8) -> PathBuf {
    write(
        dir,
        name,
        &raster::encode(raster::ExportFormat::Png, 16, 12, &[value; 16 * 12 * 4]).unwrap(),
    )
}

// ------------------------------------------------------------ fixtures

fn abr() -> Vec<u8> {
    asset_store::abr::write_test_abr(&[(6, 6, vec![200u8; 36])], 2, true)
}

fn asl() -> Vec<u8> {
    let effects = layer_model::LayerEffects {
        color_overlay: Some(layer_model::ColorOverlayEffect {
            blend_mode: layer_model::BlendMode::Normal,
            color: [1.0, 0.0, 0.0, 1.0],
            opacity: 1.0,
        }),
        ..Default::default()
    };
    let (lfx2, _) = psd::effects::export_effects(&effects).unwrap();
    let mut cur = psd::bytes::Cursor::new(&lfx2);
    cur.u32().unwrap();
    cur.u32().unwrap();
    let lefx = Descriptor::read(&mut cur, &psd::ReadOptions::default()).unwrap();
    let mut styl = Descriptor::new("Styl");
    styl.push("Lefx", Value::Descriptor(lefx)).unwrap();
    let mut sink = Sink::new();
    sink.u32(16);
    styl.write(&mut sink).unwrap();
    let bytes = sink.into_inner();
    asset_store::asl::write_asl(&[("Red Fill", "s-1", &bytes)])
}

fn pat() -> Vec<u8> {
    let pattern = psd::pattern::PsdPattern {
        name: "Red blue".to_string(),
        id: "id".into(),
        width: 2,
        height: 1,
        rgba8: vec![255, 0, 0, 255, 0, 0, 255, 255],
    };
    let mut out = b"8BPT".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&psd::pattern::encode_block(&[pattern]));
    out
}

fn grd() -> Vec<u8> {
    let obj = |class: &str, items: Vec<(&str, Value)>| {
        let mut d = Descriptor::new(class);
        for (k, v) in items {
            d.push(k, v).unwrap();
        }
        Value::Descriptor(d)
    };
    let stop = |r: f64, g: f64, b: f64, at: i32| {
        obj(
            "Clrt",
            vec![
                (
                    "Clr ",
                    obj(
                        "RGBC",
                        vec![
                            ("Rd  ", Value::Double(r)),
                            ("Grn ", Value::Double(g)),
                            ("Bl  ", Value::Double(b)),
                        ],
                    ),
                ),
                ("Lctn", Value::Integer(at)),
                ("Mdpn", Value::Integer(50)),
            ],
        )
    };
    let gradient = obj(
        "Grdn",
        vec![
            ("Nm  ", Value::Text("Teal to orange".into())),
            ("Intr", Value::Double(4096.0)),
            (
                "Clrs",
                Value::List(vec![
                    stop(0.0, 128.0, 128.0, 0),
                    stop(255.0, 128.0, 0.0, 4096),
                ]),
            ),
            ("Trns", Value::List(vec![])),
        ],
    );
    let mut root = Descriptor::new("null");
    root.push("GrdL", Value::List(vec![gradient])).unwrap();
    let mut s = Sink::new();
    s.tag(b"8BGR");
    s.u16(5);
    s.u32(16);
    root.write(&mut s).unwrap();
    s.into_inner()
}

fn csh() -> Vec<u8> {
    let mut s = Sink::new();
    s.tag(b"cush");
    s.u32(2);
    s.u32(1);
    s.unicode_string("Triangle");
    s.align_to(4);
    s.u32(1);
    let body = s.begin_len();
    s.pascal_string("tri", 1);
    for v in [0u32, 0, 1, 1] {
        s.u32(v);
    }
    let fx = |v: f64| (v * f64::from(1u32 << 24)) as i32;
    s.u16(0);
    s.u16(3);
    s.zeros(22);
    for [x, y] in [[0.5, 0.0], [1.0, 1.0], [0.0, 1.0]] {
        s.u16(2);
        for _ in 0..3 {
            s.i32(fx(y));
            s.i32(fx(x));
        }
    }
    s.end_len(body);
    s.into_inner()
}

fn aco() -> Vec<u8> {
    let mut s = Sink::new();
    s.u16(1);
    s.u16(1);
    s.u16(0);
    for v in [0u16, 32768, 32768] {
        s.u16(v);
    }
    s.u16(0);
    s.u16(2);
    s.u16(1);
    s.u16(0);
    for v in [0u16, 32768, 32768] {
        s.u16(v);
    }
    s.u16(0);
    s.unicode_string("Brand teal");
    s.into_inner()
}

/// A one-swatch Adobe Swatch Exchange file.
fn ase() -> Vec<u8> {
    let mut entry = Vec::new();
    let name: Vec<u16> = "Brand plum\0".encode_utf16().collect();
    entry.extend_from_slice(&(name.len() as u16).to_be_bytes());
    for unit in name {
        entry.extend_from_slice(&unit.to_be_bytes());
    }
    entry.extend_from_slice(b"RGB ");
    for v in [0.5f32, 0.25, 0.5] {
        entry.extend_from_slice(&v.to_bits().to_be_bytes());
    }
    entry.extend_from_slice(&2u16.to_be_bytes());
    let mut out = b"ASEF".to_vec();
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&0u16.to_be_bytes());
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&(entry.len() as u32).to_be_bytes());
    out.extend_from_slice(&entry);
    out
}

fn icc_rgb(description: &str) -> Vec<u8> {
    let mut desc = b"desc\0\0\0\0".to_vec();
    desc.extend_from_slice(&(description.len() as u32 + 1).to_be_bytes());
    desc.extend_from_slice(description.as_bytes());
    desc.push(0);
    let offset = 128 + 4 + 12;
    let mut p = vec![0u8; 128];
    p[0..4].copy_from_slice(&((offset + desc.len()) as u32).to_be_bytes());
    p[12..16].copy_from_slice(b"mntr");
    p[16..20].copy_from_slice(b"RGB ");
    p[36..40].copy_from_slice(b"acsp");
    p.extend_from_slice(&1u32.to_be_bytes());
    p.extend_from_slice(b"desc");
    p.extend_from_slice(&(offset as u32).to_be_bytes());
    p.extend_from_slice(&(desc.len() as u32).to_be_bytes());
    p.extend_from_slice(&desc);
    p
}

/// An inverting 2-point 3D LUT.
fn cube() -> String {
    let mut text = String::from("TITLE \"Invert\"\nLUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                text.push_str(&format!("{} {} {}\n", 1 - r, 1 - g, 1 - b));
            }
        }
    }
    text
}

fn layer_count(ed: &Editor) -> usize {
    ed.active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len()
}

// ------------------------------------------------------------ drag and drop

/// Every library file dropped on an open document reaches its importer —
/// counted in the library it feeds — and none is placed as a picture or
/// opens a tab. Before W11-D the drop handler sent all of them to
/// `place_path`, which failed to decode them as images.
#[test]
fn dropping_each_library_file_on_a_document_reaches_its_importer() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut ed = editor(d);
    ed.open_path(&png(d, "doc.png", 90)).unwrap();
    let mut shell = Shell::new(ed, Vec::new());
    let docs = shell.editor().documents().len();
    let layers = layer_count(shell.editor());

    let drop = |shell: &mut Shell, path: PathBuf| {
        shell.on_dropped_files(&[path]);
        let status = shell.editor().status().unwrap_or_default().to_string();
        assert!(!status.starts_with("Drop failed"), "{status}");
        status
    };

    // An image dropped on the open document is still placed as a layer.
    shell.on_dropped_files(&[png(d, "portrait.png", 200)]);
    assert_eq!(shell.editor().documents().len(), docs);
    assert_eq!(layer_count(shell.editor()), layers + 1);

    let brushes = shell.editor().presets().brushes().len();
    drop(&mut shell, write(d, "Grunge.abr", &abr()));
    assert_eq!(shell.editor().presets().brushes().len(), brushes + 1);

    let styles = shell.editor().presets().styles().len();
    drop(&mut shell, write(d, "library.asl", &asl()));
    assert_eq!(shell.editor().presets().styles().len(), styles + 1);

    let patterns = shell.editor().presets().patterns().len();
    drop(&mut shell, write(d, "tiles.pat", &pat()));
    assert_eq!(shell.editor().presets().patterns().len(), patterns + 1);

    let gradients = shell.editor().presets().gradients().len();
    drop(&mut shell, write(d, "ramps.grd", &grd()));
    assert_eq!(shell.editor().presets().gradients().len(), gradients + 1);

    let shapes = shell.editor().presets().shapes().len();
    drop(&mut shell, write(d, "shapes.csh", &csh()));
    assert_eq!(shell.editor().presets().shapes().len(), shapes + 1);

    let status = drop(&mut shell, write(d, "brand.aco", &aco()));
    assert!(status.contains("Loaded 1 swatch"), "{status}");
    let status = drop(&mut shell, write(d, "brand.ase", &ase()));
    assert!(status.contains("Loaded 1 swatch"), "{status}");

    let profile = icc_rgb("Test RGB");
    drop(&mut shell, write(d, "test.icc", &profile));
    match &shell.editor().active().unwrap().document.meta.color_space {
        color::ColorSpace::IccProfile { profile: p, .. } => assert_eq!(p, &profile),
        other => panic!("the dropped profile was not assigned: {other:?}"),
    }

    let font_bytes = dejavu::sans_mono::regular();
    let mut probe = text_engine::FontLibrary::empty();
    probe.load_bytes(font_bytes.to_vec());
    let family = probe.family_names()[0].clone();
    let status = drop(&mut shell, write(d, "Extra.ttf", font_bytes));
    assert!(status.starts_with("Loaded font"), "{status}");
    assert!(compositor::font_families().contains(&family));
    let status = drop(&mut shell, write(d, "Extra.otf", font_bytes));
    assert!(status.starts_with("Loaded font"), "{status}");

    // A `.cube` becomes a Color Lookup adjustment layer: the one library
    // drop that adds a layer.
    drop(&mut shell, write(d, "Invert.cube", cube().as_bytes()));
    let doc = shell.editor().active().unwrap();
    let top = doc.document.active_layer().unwrap();
    match &doc.document.layers.get(top).unwrap().kind {
        layer_model::LayerKind::Adjustment(a) => match &a.kind {
            layer_model::AdjustmentKind::ColorLookup { name, size, .. } => {
                assert_eq!((name.as_str(), *size), ("Invert", 2));
            }
            other => panic!("not a colour lookup: {other:?}"),
        },
        other => panic!("not an adjustment layer: {other:?}"),
    }

    assert_eq!(
        shell.editor().documents().len(),
        docs,
        "no library file opens a tab"
    );
    assert_eq!(
        layer_count(shell.editor()),
        layers + 2,
        "only the portrait and the .cube add layers; no library file was placed"
    );
}

/// With no document open a dropped brush file still only feeds the Brushes
/// panel, and a dropped `.cube` says it needs a document.
#[test]
fn dropping_a_library_file_on_an_empty_window_opens_no_document() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut shell = Shell::new(editor(d), Vec::new());
    let brushes = shell.editor().presets().brushes().len();
    shell.on_dropped_files(&[write(d, "Grunge.abr", &abr())]);
    assert_eq!(shell.editor().presets().brushes().len(), brushes + 1);
    assert!(shell.editor().documents().is_empty());

    shell.on_dropped_files(&[write(d, "Invert.cube", cube().as_bytes())]);
    assert!(shell.editor().documents().is_empty());
    let status = shell.editor().status().unwrap_or_default();
    assert!(status.contains("open a document"), "{status}");
}

// ---------------------------------------------------- recent files, startup

/// File > Open Recent (and the command line) route through `open_paths`:
/// a brush file there adds its brushes, raises no error and opens no tab.
#[test]
fn recent_files_open_of_an_abr_adds_its_brushes() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let file = write(d, "Grunge.abr", &abr());
    let mut ed = editor(d);
    let brushes = ed.presets().brushes().len();
    let opened = ed.open_paths(std::slice::from_ref(&file));
    assert!(opened.is_empty(), "a brush file is not a document");
    assert!(ed.documents().is_empty());
    assert_eq!(ed.presets().brushes().len(), brushes + 1);
    let status = ed.status().unwrap_or_default();
    assert!(status.starts_with("Added 1 brush"), "{status}");

    // The same through the one routing function every route shares (a
    // second file: the same file again would redefine the same name).
    let second = write(d, "Chalk.abr", &abr());
    assert_eq!(ed.open_any(&second), Ok(None));
    assert_eq!(ed.presets().brushes().len(), brushes + 2);
    let image = png(d, "doc.png", 10);
    assert!(matches!(ed.open_any(&image), Ok(Some(_))));
    assert_eq!(ed.documents().len(), 1);
}

// ------------------------------------------------------------ paste

/// An OS clipboard that counts its image reads, so a test can pin how many
/// times one Paste decodes the clipboard image.
struct CountingClipboard {
    inner: crate::clipboard::FakeClipboard,
    reads: Arc<AtomicUsize>,
}

impl crate::clipboard::ImageClipboard for CountingClipboard {
    fn get_image(
        &mut self,
    ) -> Result<Option<crate::clipboard::ClipboardImage>, crate::clipboard::ClipboardError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get_image()
    }

    fn set_image(
        &mut self,
        image: &crate::clipboard::ClipboardImage,
    ) -> Result<(), crate::clipboard::ClipboardError> {
        self.inner.set_image(image)
    }
}

/// Paste with no document open (Photopea): the menu item is live, and it
/// opens a document the clipboard image's size holding that image.
#[test]
fn paste_with_no_document_opens_a_document_of_the_clipboard_size() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let (w, h) = (5u32, 3u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [(i * 10) as u8, 100, 200, 255])
        .collect();
    let mut os = crate::clipboard::FakeClipboard::new();
    os.seed(crate::clipboard::ClipboardImage::validate(w, h, rgba.clone()).unwrap());
    let reads = Arc::new(AtomicUsize::new(0));
    ed.set_image_clipboard(Box::new(CountingClipboard {
        inner: os,
        reads: Arc::clone(&reads),
    }));
    assert!(ed.active().is_none());

    let context = menu_bridge::context(&mut ed, &ui::Workspace::new());
    let pick = menu_bridge::resolve(MenuAction::Paste, &context, &ed)
        .expect("Paste is live with an image on the clipboard");
    let Pick::Menu(action) = pick else {
        panic!("Paste resolved to {pick:?}");
    };
    reads.store(0, Ordering::SeqCst);
    menu_bridge::perform(action, &mut ed).expect("the paste");
    assert_eq!(
        reads.load(Ordering::SeqCst),
        1,
        "the perform reads the OS clipboard once: the new document is sized          from the same read that is pasted"
    );

    let doc = ed.active().expect("Paste opened a document");
    assert_eq!((doc.document.width(), doc.document.height()), (w, h));
    assert_eq!(
        shown(&mut ed),
        rgba,
        "the document shows the clipboard's image"
    );
}

/// An empty clipboard with no document: Paste is greyed and does nothing.
#[test]
fn paste_with_no_document_and_an_empty_clipboard_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    let context = menu_bridge::context(&mut ed, &ui::Workspace::new());
    assert_eq!(
        menu_bridge::resolve(MenuAction::Paste, &context, &ed).unwrap_err(),
        "The clipboard is empty"
    );
    assert!(ed.new_document_for_paste(None).is_err());
    assert!(ed.documents().is_empty());
}

// ------------------------------------------------------------ revert

/// Paint the whole of `layer` one colour, one undo step.
fn paint(ed: &mut Editor, layer: layer_model::LayerId, value: u8) {
    let doc = ed.active_mut().unwrap();
    let (w, h) = (doc.document.width(), doc.document.height());
    let grid = raster::TileGrid::from_rgba8(w, h, &vec![value; (w * h * 4) as usize]).unwrap();
    let mut edits = Vec::new();
    for (coord, tile) in grid.iter() {
        let hash = doc.tiles.insert_bytes(tile.data().to_vec());
        edits.push(editor_core::pixels::TileEdit::set(coord, hash));
    }
    let command =
        editor_core::Command::paint_tiles(editor_core::pixels::PixelTarget::Layer(layer), edits)
            .unwrap();
    ed.apply_command(command);
}

fn shown(ed: &mut Editor) -> Vec<u8> {
    let doc = ed.active_mut().unwrap();
    let rect = doc.canvas_rect();
    doc.composite(rect).unwrap()
}

/// Click File > Revert through the menu's own resolution and perform.
fn revert_via_menu(ed: &mut Editor) -> Result<String, String> {
    let context = menu_bridge::context(ed, &ui::Workspace::new());
    match menu_bridge::resolve(MenuAction::Revert, &context, ed)? {
        Pick::Menu(action) => menu_bridge::perform(action, ed),
        other => panic!("Revert resolved to {other:?}"),
    }
}

fn check_revert(ed: &mut Editor) {
    let saved = shown(ed);
    let base = ed.active().unwrap().document.active_layer().unwrap();
    let layers = layer_count(ed);
    // Unsaved changes: a repaint, a new locked layer, a selection.
    paint(ed, base, 7);
    let extra = layer_model::Layer::raster("Extra");
    let extra_id = extra.id;
    ed.apply_command(editor_core::Command::create_layer(extra));
    paint(ed, extra_id, 30);
    ed.apply_command(editor_core::Command::SetLayerProperties {
        layer_id: extra_id,
        patch: editor_core::LayerPatch {
            locked: Some(layer_model::LockState {
                all: true,
                ..Default::default()
            }),
            ..Default::default()
        },
    });
    let edited = shown(ed);
    assert_ne!(edited, saved);
    assert_eq!(layer_count(ed), layers + 1);
    assert!(ed.active().unwrap().is_dirty());

    let message = revert_via_menu(ed).expect("Revert is live and runs");
    assert!(message.starts_with("Reverted to"), "{message}");
    assert_eq!(shown(ed), saved, "Revert restores the saved pixels");
    assert_eq!(layer_count(ed), layers, "Revert restores the saved layers");
    assert!(
        !ed.active().unwrap().is_dirty(),
        "the document matches its file"
    );
    assert_eq!(
        ed.active().unwrap().history.undo_label(),
        Some(super::REVERT_LABEL),
        "Revert is one history step"
    );
    // With nothing left to take back, Revert greys out.
    let context = menu_bridge::context(ed, &ui::Workspace::new());
    assert!(menu_bridge::resolve(MenuAction::Revert, &context, ed).is_err());

    // Undo takes the Revert back: the edits, the locked layer, all of it.
    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(shown(ed), edited, "Undo brings the edits back");
    assert_eq!(layer_count(ed), layers + 1);
    assert!(ed
        .active()
        .unwrap()
        .document
        .layers
        .get(extra_id)
        .is_some_and(|l| l.locked.all));
    // And Redo reverts again.
    assert!(ed.active_mut().unwrap().redo().unwrap());
    assert_eq!(shown(ed), saved);
    assert_eq!(layer_count(ed), layers);
}

/// File > Revert on a document opened from an image reloads the image.
#[test]
fn revert_of_an_image_restores_its_pixels_and_is_undoable() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "photo.png", 120)).unwrap();
    // A clean document has nothing to revert.
    let context = menu_bridge::context(&mut ed, &ui::Workspace::new());
    assert_eq!(
        menu_bridge::resolve(MenuAction::Revert, &context, &ed).unwrap_err(),
        "The document has no unsaved changes"
    );
    check_revert(&mut ed);
}

/// File > Revert on a saved project reloads the package — the same layer
/// ids come back, so tiles painted since the save are cleared, not kept.
#[test]
fn revert_of_a_project_restores_the_saved_package() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.open_path(&png(dir.path(), "photo.png", 120)).unwrap();
    let base = ed.active().unwrap().document.active_layer().unwrap();
    paint(&mut ed, base, 60);
    let package = dir.path().join("kept.rstudio");
    ed.active_mut().unwrap().save_to(&package, "test").unwrap();
    assert!(!ed.active().unwrap().is_dirty());
    assert_eq!(ed.revert_source().as_deref(), Some(package.as_path()));
    check_revert(&mut ed);
}

/// A document that was never on disk has nothing to revert to.
#[test]
fn revert_of_a_new_document_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path());
    ed.new_document_with(
        8,
        8,
        "Untitled",
        crate::import::BlankBackground::Transparent,
    )
    .unwrap();
    let base = ed.active().unwrap().document.active_layer().unwrap();
    paint(&mut ed, base, 9);
    let context = menu_bridge::context(&mut ed, &ui::Workspace::new());
    assert_eq!(
        menu_bridge::resolve(MenuAction::Revert, &context, &ed).unwrap_err(),
        "The document has never been saved"
    );
    assert!(ed.revert_active().is_err());
}
