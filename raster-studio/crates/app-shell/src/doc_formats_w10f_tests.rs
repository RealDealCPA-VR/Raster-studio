//! W10-F: the new file formats through the product's own routes.
//!
//! Opening goes through [`Editor::open_path`] - the road drag-and-drop,
//! recent files and startup files take (and `OpenDocument::open_image`,
//! which File > Open's job mirrors). Exporting goes through
//! [`Editor::dispatch`]`(Action::Export)` with the picker answering a path -
//! File > Export itself, its worker included.

use std::path::{Path, PathBuf};

use crate::action::Action;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// 6x4 opaque pixels, every one distinct.
fn pixels() -> Vec<u8> {
    (0..24u32)
        .flat_map(|i| [(i * 10) as u8, (200 - i * 5) as u8, (i * 3) as u8, 255])
        .collect()
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// The active document's canvas size and composite.
fn active(ed: &mut Editor) -> (u32, u32, Vec<u8>) {
    let doc = ed.active_mut().expect("a document opened");
    let (w, h) = (doc.document.width(), doc.document.height());
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    (w, h, rgba)
}

#[test]
fn ppm_pgm_pbm_dds_xcf_and_jxl_open_as_documents() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let px = pixels();

    for (name, format) in [
        ("a.ppm", raster::ExportFormat::Ppm),
        ("a.dds", raster::ExportFormat::Dds),
    ] {
        let path = write(
            dir.path(),
            name,
            &raster::encode(format, 6, 4, &px).unwrap(),
        );
        ed.open_path(&path)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let (w, h, rgba) = active(&mut ed);
        assert_eq!((w, h), (6, 4), "{name}");
        assert_eq!(rgba, px, "{name} opens with its exact pixels");
    }

    // PGM: grey; PBM: black and white.
    let pgm = write(dir.path(), "g.pgm", b"P2\n2 1\n255\n10 250\n");
    ed.open_path(&pgm).unwrap();
    assert_eq!(active(&mut ed).2, [10, 10, 10, 255, 250, 250, 250, 255]);
    let pbm = write(dir.path(), "b.pbm", b"P1\n2 1\n1 0\n");
    ed.open_path(&pbm).unwrap();
    assert_eq!(active(&mut ed).2, [0, 0, 0, 255, 255, 255, 255, 255]);

    // GIMP XCF: two layers, offsets and a half-opaque top layer (raster's
    // `the_committed_xcf_fixture_is_the_test_writers_output` pins what it
    // holds), opened as two layers whose composite is the decoder's
    // flattened image.
    let path = write(
        dir.path(),
        "two.xcf",
        include_bytes!("../../raster/src/formats/testdata/two_layers.xcf"),
    );
    let flat = raster::decode_surface_path(&path, raster::ImportLimits::default())
        .unwrap()
        .into_decoded_image();
    ed.open_path(&path).unwrap();
    let names: Vec<String> = {
        let layers = &ed.active_mut().unwrap().document.layers;
        layers
            .root()
            .iter()
            .map(|id| layers.get(*id).unwrap().name.clone())
            .collect()
    };
    assert_eq!(names, ["top", "bottom"], "the .xcf opens layered");
    let (w, h, rgba) = active(&mut ed);
    assert_eq!((w, h), (flat.width, flat.height));
    assert_eq!((w, h), (8, 6));
    // Outside the half-opaque top layer (3x2 at (2, 1)) the composite is
    // the decoder's flattened image exactly; under it the two differ in
    // blend space (the document composites in linear light, as GIMP 2.10
    // does for its default modes; the flat decoder mixes 8-bit values).
    for y in 0..6usize {
        for x in 0..8usize {
            if (2..5).contains(&x) && (1..3).contains(&y) {
                continue;
            }
            let i = (y * 8 + x) * 4;
            assert_eq!(rgba[i..i + 4], flat.rgba8[i..i + 4], "({x}, {y})");
        }
    }
    // Under the top layer at (3, 2): blue at half opacity over red.
    let i = (2 * 8 + 3) * 4;
    assert!(rgba[i] > 100 && rgba[i + 2] > 100, "{:?}", &rgba[i..i + 4]);

    // JPEG XL (libjxl's lossless file).
    let jxl = write(
        dir.path(),
        "ramp.jxl",
        include_bytes!("../../raster/src/formats/testdata/ramp_6x4_rgba_lossless.jxl"),
    );
    ed.open_path(&jxl).unwrap();
    let (w, h, rgba) = active(&mut ed);
    assert_eq!((w, h), (6, 4));
    // (1, 2): R = 40, G = 120, B = 200, A = 255 - 50.
    let i = (2 * 6 + 1) * 4;
    assert_eq!(rgba[i + 3], 205);
}

#[test]
fn a_psb_opens_as_a_layered_document_and_an_avif_is_refused_by_name() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let psb = write(
        dir.path(),
        "big.psb",
        include_bytes!("../../psd/src/testdata/two_layers.psb"),
    );
    ed.open_path(&psb).unwrap();
    let doc = ed.active_mut().unwrap();
    assert_eq!((doc.document.width(), doc.document.height()), (5, 4));
    let names: Vec<String> = doc
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .filter_map(|id| doc.document.layers.get(id).map(|l| l.name.clone()))
        .collect();
    assert!(
        names.iter().any(|n| n == "back") && names.iter().any(|n| n == "top"),
        "the .psb's layers are layers: {names:?}"
    );

    let avif = write(
        dir.path(),
        "photo.avif",
        include_bytes!("../../raster/src/formats/testdata/quadrants_16x12_libaom_420.avif"),
    );
    let err = ed.open_path(&avif).unwrap_err().to_string();
    assert!(err.contains("AVIF"), "{err}");
}

#[test]
fn file_export_writes_ppm_pgm_pbm_dds_and_avif() {
    let dir = tempfile::tempdir().unwrap();
    let targets: Vec<PathBuf> = ["out.ppm", "out.pgm", "out.pbm", "out.dds", "out.avif"]
        .iter()
        .map(|n| dir.path().join(n))
        .collect();
    let mut dialogs = ScriptedDialogs::new();
    for t in &targets {
        dialogs = dialogs.exporting_to(t.clone());
    }
    let mut ed = editor(dir.path(), dialogs);
    let px = pixels();
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &px).unwrap(),
    );
    ed.open_path(&src).unwrap();
    for t in &targets {
        ed.dispatch(Action::Export)
            .unwrap_or_else(|e| panic!("{}: {e}", t.display()));
        ed.poll_exports();
        assert!(t.exists(), "{} was not written", t.display());
    }
    let read = |p: &Path| {
        raster::decode_surface_path(p, raster::ImportLimits::default())
            .unwrap()
            .into_decoded_image()
    };
    assert_eq!(
        read(&targets[0]).rgba8,
        px,
        "PPM is exact for an opaque image"
    );
    assert_eq!(read(&targets[3]).rgba8, px, "uncompressed DDS is exact");
    let pgm = read(&targets[1]);
    assert_eq!(pgm.rgba8[0], pgm.rgba8[1], "PGM is grey");
    let pbm = read(&targets[2]);
    assert!(pbm.rgba8.chunks(4).all(|p| p[0] == 0 || p[0] == 255));
    let avif = std::fs::read(&targets[4]).unwrap();
    assert_eq!(&avif[4..12], b"ftypavif", "an AVIF file");
}

/// A red rectangle shape layer and a line of text on the opened `src.png`.
fn with_shape_and_text(ed: &mut Editor) {
    use editor_core::Command;
    use layer_model::{Layer, LayerKind};
    let doc = ed.active_mut().unwrap();
    let mut rect = Layer::with_kind(
        "Rect",
        LayerKind::Shape(layer_model::ShapeLayer {
            path_svg: "M 0 0 L 3 0 L 3 2 L 0 2 Z".to_string(),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            ..layer_model::ShapeLayer::default()
        }),
    );
    rect.transform = glam::Affine2::from_translation(glam::Vec2::new(1.0, 1.0));
    doc.apply(Command::create_layer(rect)).unwrap();
    let text = Layer::with_kind(
        "Caption",
        LayerKind::Text(layer_model::TextLayer {
            text: "Hello".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 3.0,
            ..layer_model::TextLayer::default()
        }),
    );
    doc.apply(Command::create_layer(text)).unwrap();
}

/// Round 2: File > Export to `x.svg` - the picker offers SVG, `act_export`
/// accepts it, and the worker (`jobs::run_file_export`) writes the shape as
/// a `<path>`, the text as a `<text>` and the raster background as an
/// embedded `<image>`.
#[test]
fn file_export_to_svg_writes_shape_paths_and_text() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("vector.svg");
    let request = crate::dialogs::ExportPickerRequest::next(Path::new("/work/photo.png"));
    assert!(
        request.filters.iter().any(|(_, e)| *e == ["svg"]),
        "the export picker offers SVG"
    );
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new().exporting_to(target.clone()),
    );
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &pixels()).unwrap(),
    );
    ed.open_path(&src).unwrap();
    with_shape_and_text(&mut ed);
    ed.dispatch(Action::Export)
        .unwrap_or_else(|e| panic!("File > Export to .svg: {e}"));
    ed.poll_exports();
    let svg = std::fs::read_to_string(&target).expect("the .svg was written");
    assert!(
        svg.contains("<path d=\"M 0 0 L 3 0 L 3 2 L 0 2 Z\" transform=\"matrix(1 0 0 1 1 1)\" fill=\"#ff0000\""),
        "{svg}"
    );
    assert!(
        svg.contains("<text ") && svg.contains(">Hello</tspan>"),
        "{svg}"
    );
    assert_eq!(svg.matches("<image ").count(), 1, "{svg}");
}

/// Round 2: Export As with an SVG row at 100% writes the same vector SVG;
/// a 200% SVG row stays the codec's embedded-image SVG at its scaled size.
#[test]
fn export_as_svg_at_100_percent_writes_shape_paths_and_text() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    let src = write(
        dir.path(),
        "src.png",
        &raster::encode(raster::ExportFormat::Png, 6, 4, &pixels()).unwrap(),
    );
    ed.open_path(&src).unwrap();
    with_shape_and_text(&mut ed);
    let job = ui::dialogs::ExportJob {
        base_name: "shot".to_string(),
        entries: vec![
            ui::dialogs::ExportEntry::new("", raster::ExportFormat::Svg, 1.0),
            ui::dialogs::ExportEntry::new("_2x", raster::ExportFormat::Svg, 2.0),
        ],
    };
    ed.request_export(job, out.clone());
    ed.poll_exports();
    let svg = std::fs::read_to_string(out.join("shot.svg")).expect("shot.svg written");
    assert!(
        svg.contains("<path d=\"M 0 0 L 3 0 L 3 2 L 0 2 Z\""),
        "{svg}"
    );
    assert!(svg.contains(">Hello</tspan>"), "{svg}");
    let scaled = std::fs::read_to_string(out.join("shot_2x.svg")).expect("shot_2x.svg written");
    assert!(!scaled.contains("<path "), "{scaled}");
    assert!(scaled.contains("<image "), "{scaled}");
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

/// Round 2: File > Open of a GIMP `.xcf` (the import job) builds the layer
/// tree - names, offsets (off-canvas pixels kept), opacity, visibility,
/// groups, blend modes - and shows an "XCF import report" naming what did
/// not map (Grain merge, GIMP mode 50), not what did (Dissolve,
/// pass-through). The fixture is pinned to raster's XCF test writer by
/// `the_committed_layered_xcf_fixture_is_the_test_writers_output`.
#[test]
fn file_open_of_an_xcf_builds_its_layers_and_reports_what_did_not_map() {
    use layer_model::{BlendMode, GroupBlending, LayerKind};
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "modes.xcf",
        include_bytes!("../../raster/src/formats/testdata/layered_modes.xcf"),
    );
    let spy = NoticeSpy::default();
    spy.inner.borrow_mut().open_files.push(path.clone());
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.path()),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(spy.clone()),
    );
    ed.dispatch(Action::Open).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while ed.imports_pending() && std::time::Instant::now() < deadline {
        ed.poll_imports();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let doc = ed.active_mut().expect("the .xcf opened");
    let rgba = doc.composite(doc.canvas_rect()).unwrap();
    let doc = &*doc;
    let layers = &doc.document.layers;
    let named = |name: &str| {
        layers
            .iter_depth_first()
            .into_iter()
            .find_map(|id| layers.get(id).filter(|l| l.name == name))
            .unwrap_or_else(|| panic!("no layer {name:?}"))
    };
    let top: Vec<String> = layers
        .root()
        .iter()
        .map(|id| layers.get(*id).unwrap().name.clone())
        .collect();
    assert_eq!(
        top,
        ["g", "grain", "odd", "hidden", "speckle", "off", "bottom"],
        "one layer per GIMP layer, top first"
    );
    let g = named("g");
    let LayerKind::Group(group) = &g.kind else {
        panic!("g is a group")
    };
    assert_eq!(group.blending, GroupBlending::PassThrough);
    assert!((g.opacity - 200.0 / 255.0).abs() < 1e-3, "{}", g.opacity);
    assert_eq!(group.children.len(), 1);
    assert_eq!(named("inside").blend_mode, BlendMode::Multiply);
    assert_eq!(named("speckle").blend_mode, BlendMode::Dissolve);
    assert_eq!(named("grain").blend_mode, BlendMode::Normal);
    assert!(!named("hidden").visible);
    assert!(named("bottom").visible);

    // Pixels: "odd" is the top visible layer at (0, 0); (5, 3) is "bottom"
    // alone ("hidden" is hidden); "off" hangs off the corner and keeps its
    // off-canvas pixel in the tile store.
    let at = |x: usize, y: usize| rgba[(y * 6 + x) * 4..(y * 6 + x) * 4 + 4].to_vec();
    assert_eq!(at(0, 0), [1, 2, 3, 255]);
    assert_eq!(at(5, 3), [200, 50, 50, 255]);
    let off_id = layers
        .iter_depth_first()
        .into_iter()
        .find(|id| layers.get(*id).is_some_and(|l| l.name == "off"))
        .unwrap();
    let off_tiles = doc
        .document
        .pixels
        .tiles(editor_core::pixels::PixelKey::Layer(off_id))
        .map(|m| m.len())
        .unwrap_or(0);
    assert!(
        off_tiles >= 2,
        "the off-canvas pixel has a tile of its own: {off_tiles}"
    );

    let notices = spy.notices.borrow();
    assert_eq!(notices.len(), 1, "{notices:?}");
    let (title, message) = &notices[0];
    assert_eq!(title, "XCF import report");
    assert!(
        message.contains("\"grain\"") && message.contains("Grain merge"),
        "{message}"
    );
    assert!(
        message.contains("\"odd\"") && message.contains("blend mode 50"),
        "{message}"
    );
    assert!(
        !message.contains("Dissolve"),
        "Dissolve maps exactly: {message}"
    );
    assert!(!message.contains("passes through"), "{message}");
}
