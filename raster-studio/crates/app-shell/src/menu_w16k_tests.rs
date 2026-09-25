//! W16-K: each gap the final parity audit named, driven the way a user
//! reaches it - the menu row clicked through the chrome and its output
//! applied as the shell applies it, the chord resolved through the keymap,
//! the Export As row's job run by the export worker, the file opened
//! through `Editor::open_paths`, the Script window's buttons pressed in a
//! headless frame - and checked on the document, the window or the file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use editor_core::Command;
use layer_model::{Layer, LayerKind};
use raster::codec::export_vector as ev;
use raster::ExportFormat;
use ui::menu::{ArtboardSide, MenuAction, ScreenModeItem, WarpTextItem};

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::keymap::{Chord, Key, Keymap};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor_in(dir: &Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

/// A `w` x `h` white document, the only one open.
fn editor_with_doc(dir: &Path, w: u32, h: u32) -> Editor {
    let mut ed = editor_in(dir);
    ed.new_document_with(
        w,
        h,
        "Doc",
        crate::import::BlankBackground::Solid {
            rgba8: [255, 255, 255, 255],
            depth: raster::BitDepth::Eight,
        },
    )
    .unwrap();
    ed
}

/// Click `action` in the menu bar (the chrome's own click handler) and apply
/// what it put in the output the way the shell does.
fn click(ed: &mut Editor, action: MenuAction) -> Result<(), String> {
    let mut chrome = Chrome::new();
    let ctx = super::context(ed, chrome.workspace());
    let intent = super::resolve_intent(action, &ctx, ed)?;
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    for command in out.commands {
        ed.apply_command(command);
    }
    for action in out.menu {
        super::perform(action, ed)?;
    }
    for action in out.actions {
        ed.dispatch(action).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The actions under `path` (submenu labels, outermost first) of `title`.
fn submenu(ed: &Editor, title: &str, path: &[&str]) -> Vec<MenuAction> {
    let bar = super::menus(ed);
    let menu = bar.iter().find(|m| m.title == title).expect("the menu");
    let mut entries = &menu.entries;
    for label in path {
        entries = entries
            .iter()
            .find_map(|e| match e {
                ui::menu::Entry::Submenu { label: l, entries } if l == label => Some(entries),
                _ => None,
            })
            .unwrap_or_else(|| panic!("no {label} submenu"));
    }
    entries.iter().flat_map(ui::menu::Entry::actions).collect()
}

fn boards(ed: &Editor) -> Vec<layer_model::Artboard> {
    let doc = &ed.active().unwrap().document;
    layer_model::artboard::artboards(&doc.layers)
        .into_iter()
        .map(|(_, b)| b)
        .collect()
}

// ---------------------------------------------------------------------------
// View > Mode, View > Show > Paths
// ---------------------------------------------------------------------------

#[test]
fn view_mode_rows_set_the_screen_mode() {
    use ui::palette::ScreenMode;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 8, 8);
    let rows = submenu(&ed, "View", &["Mode"]);
    assert_eq!(
        rows,
        ScreenModeItem::ALL
            .iter()
            .map(|m| MenuAction::SetScreenMode(*m))
            .collect::<Vec<_>>()
    );
    for (item, want) in [
        (ScreenModeItem::Fullscreen, ScreenMode::FullScreen),
        (
            ScreenModeItem::MenuBarAndCanvas,
            ScreenMode::FullScreenWithMenu,
        ),
        (ScreenModeItem::Standard, ScreenMode::Standard),
        (ScreenModeItem::Fullscreen, ScreenMode::FullScreen),
    ] {
        click(&mut ed, MenuAction::SetScreenMode(item)).unwrap();
        assert_eq!(ed.screen_mode(), want, "{item:?}");
    }
}

#[test]
fn view_show_lists_paths_and_it_toggles_as_a_view_flag() {
    let dir = tempfile::tempdir().unwrap();
    let ed = editor_with_doc(dir.path(), 8, 8);
    let show = submenu(&ed, "View", &["Show"]);
    let paths = MenuAction::ToggleView(ui::ViewFlag::Paths);
    assert_eq!(show.first(), Some(&paths), "Paths sits above Slices");
    let mut w = ui::Workspace::new();
    assert!(
        w.view_flags.get(ui::ViewFlag::Paths),
        "paths show by default"
    );
    let ctx = super::context(&mut editor_with_doc(dir.path(), 8, 8), &w);
    let ui::menu::Resolution::Enabled(intent) = paths.resolve(&ctx) else {
        panic!("View > Show > Paths is greyed");
    };
    w.absorb(&intent);
    assert!(!w.view_flags.get(ui::ViewFlag::Paths));
}

// ---------------------------------------------------------------------------
// Chords
// ---------------------------------------------------------------------------

#[test]
fn fill_is_shift_f5_and_camera_raw_is_shift_ctrl_a_on_the_keymap() {
    let keymap = Keymap::default();
    let shift_f5 = Chord {
        ctrl_or_cmd: false,
        alt: false,
        shift: true,
        key: Key::Function(5),
    };
    assert_eq!(
        keymap.menu_action_for(&shift_f5),
        Some(MenuAction::FillDialog)
    );
    let camera_raw = Chord::ctrl_shift(Key::character('a'));
    assert_eq!(
        keymap.menu_action_for(&camera_raw),
        Some(MenuAction::Filter(ui::menu::FilterId::CameraRaw))
    );
    assert!(
        keymap.resolve(&camera_raw).is_none(),
        "no app action takes it first"
    );
    // Shift+F is Fill no longer.
    let shift_f = Chord {
        ctrl_or_cmd: false,
        alt: false,
        shift: true,
        key: Key::character('f'),
    };
    assert_ne!(
        keymap.menu_action_for(&shift_f),
        Some(MenuAction::FillDialog)
    );
}

// ---------------------------------------------------------------------------
// Layer > New > Artboard, the Artboard bar's + buttons, Crop by
// ---------------------------------------------------------------------------

#[test]
fn new_artboard_makes_the_canvas_then_a_neighbour_and_undoes_in_one_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 40, 30);
    assert!(submenu(&ed, "Layer", &["New"]).contains(&MenuAction::NewArtboard));
    click(&mut ed, MenuAction::NewArtboard).unwrap();
    assert_eq!(boards(&ed).len(), 1);
    let first = boards(&ed)[0];
    assert_eq!(
        (first.x, first.y, first.width, first.height),
        (0, 0, 40, 30)
    );
    assert_eq!(first.background, [1.0, 1.0, 1.0, 1.0]);
    // The plate is painted (its colour over its rect, the Artboard tool's
    // rule), so the artboard shows and exports through the raster path.
    {
        let doc = &ed.active().unwrap().document;
        let group = doc.active_layer().expect("the new artboard is active");
        let (plate, _) = layer_model::artboard::artboard_of(&doc.layers, group).unwrap();
        assert!(
            doc.pixels
                .tiles(editor_core::PixelKey::Layer(plate))
                .is_some_and(|t| t.iter().next().is_some()),
            "the plate has no pixels"
        );
    }

    click(&mut ed, MenuAction::NewArtboard).unwrap();
    let second = boards(&ed)
        .into_iter()
        .find(|b| b.x != 0)
        .expect("a second artboard");
    assert_eq!(
        (second.x, second.y, second.width, second.height),
        (40 + super::w16k::ARTBOARD_GAP, 0, 40, 30)
    );
    // The bar's + Below adds under the active (the second) artboard.
    click(&mut ed, MenuAction::ArtboardNeighbour(ArtboardSide::Below)).unwrap();
    assert!(boards(&ed)
        .iter()
        .any(|b| (b.x, b.y) == (second.x, 30 + super::w16k::ARTBOARD_GAP)));
    assert_eq!(boards(&ed).len(), 3);
    ed.active_mut().unwrap().undo().unwrap();
    assert_eq!(boards(&ed).len(), 2, "one Ctrl+Z takes one artboard back");
}

#[test]
fn a_neighbour_needs_an_artboard_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 8, 8);
    let err = click(&mut ed, MenuAction::ArtboardNeighbour(ArtboardSide::Left)).unwrap_err();
    assert!(err.contains("artboard"), "{err}");
}

/// Paint a `w` x `h` red block at `(x, y)` on a new layer and make it active.
fn red_layer(ed: &mut Editor, x: u32, y: u32, w: u32, h: u32) -> layer_model::LayerId {
    let (dw, dh) = {
        let d = &ed.active().unwrap().document;
        (d.width(), d.height())
    };
    let mut rgba = vec![0u8; (dw * dh * 4) as usize];
    for yy in y..y + h {
        for xx in x..x + w {
            let i = ((yy * dw + xx) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
        }
    }
    let layer = Layer::raster("Red");
    let id = layer.id;
    ed.apply_command(Command::create_layer(layer));
    let paint = super::pixels::write_layer(ed.active_mut().unwrap(), id, &rgba, "Red").unwrap();
    ed.apply_command(paint);
    ed.set_layer_selection(vec![id], Some(id));
    id
}

#[test]
fn crop_by_current_layer_crops_the_canvas_to_its_ink() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 20, 16);
    red_layer(&mut ed, 3, 4, 6, 5);
    // The Crop bar's row raises the menu action the chrome performs.
    click(&mut ed, MenuAction::CropToLayer).unwrap();
    let d = &ed.active().unwrap().document;
    assert_eq!((d.width(), d.height()), (6, 5));
}

// ---------------------------------------------------------------------------
// Layer > Text > Warp Style > Custom
// ---------------------------------------------------------------------------

#[test]
fn warp_style_lists_custom_and_it_warps_the_text_with_the_free_mesh() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 16, 16);
    let custom = MenuAction::WarpText(WarpTextItem::Style(layer_model::text::WarpStyle::Custom));
    assert!(submenu(&ed, "Layer", &["Text", "Warp Style"]).contains(&custom));
    let text = Layer::with_kind(
        "T",
        LayerKind::Text(layer_model::TextLayer {
            text: "Hi".to_string(),
            ..layer_model::TextLayer::default()
        }),
    );
    let id = text.id;
    ed.apply_command(Command::create_layer(text));
    ed.set_layer_selection(vec![id], Some(id));
    click(&mut ed, custom).unwrap();
    let LayerKind::Text(t) = &ed.active().unwrap().document.layers.get(id).unwrap().kind else {
        panic!("still a text layer");
    };
    assert_eq!(t.warp.style, layer_model::text::WarpStyle::Custom);
}

// ---------------------------------------------------------------------------
// File > Export As > PDF / EMF / DXF
// ---------------------------------------------------------------------------

/// Two artboards (made through Layer > New > Artboard) with a red square
/// shape inside the first.
fn two_boards_with_a_shape(dir: &Path) -> Editor {
    let mut ed = editor_with_doc(dir, 40, 30);
    click(&mut ed, MenuAction::NewArtboard).unwrap();
    let first = ed.active().unwrap().document.active_layer().unwrap();
    click(&mut ed, MenuAction::NewArtboard).unwrap();
    let shape = Layer::with_kind(
        "Square",
        LayerKind::Shape(layer_model::ShapeLayer {
            path_svg: "M 5 5 L 15 5 L 15 15 L 5 15 Z".to_string(),
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            ..layer_model::ShapeLayer::default()
        }),
    );
    let shape_id = shape.id;
    ed.apply_command(Command::Transaction {
        label: "Square".into(),
        commands: vec![
            Command::create_layer(shape),
            Command::MoveLayer {
                layer_id: shape_id,
                parent: Some(first),
                index: 0,
            },
        ],
    });
    ed
}

fn export(ed: &mut Editor, format: ExportFormat, out: &Path) -> PathBuf {
    let job = ui::dialogs::ExportJob {
        base_name: "boards".to_string(),
        entries: vec![ui::dialogs::ExportEntry::new("", format, 1.0)],
    };
    ed.request_export(job, out.to_path_buf());
    ed.poll_exports();
    out.join(format!("boards.{}", format.extension()))
}

#[test]
fn export_as_lists_pdf_emf_and_dxf_and_opens_the_dialog_on_them() {
    let dir = tempfile::tempdir().unwrap();
    let ed = editor_with_doc(dir.path(), 8, 8);
    let rows = submenu(&ed, "File", &["Export As"]);
    for format in ExportFormat::VECTOR {
        assert!(rows.contains(&MenuAction::Export(format)), "{format:?}");
        let mut host = crate::dialog_host::DialogHost::default();
        assert!(host.open_for_menu_action(&MenuAction::Export(format), &ed));
        assert_eq!(host.active_export_dialog_for_test().format(), format);
    }
}

#[test]
fn a_pdf_export_is_one_vector_page_per_artboard() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = two_boards_with_a_shape(dir.path());
    let path = export(&mut ed, ExportFormat::Pdf, &dir.path().join("out"));
    let bytes = std::fs::read(&path).expect("the PDF was written");
    assert_eq!(
        raster::codec::formats::pdf::page_count(&bytes).unwrap(),
        2,
        "one page per artboard, through the hayro reader"
    );
    // Vector: the plates and the square are paths, no image was embedded.
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("/Subtype /Image"), "an image was embedded");
    let pages =
        raster::codec::formats::pdf::render_pages(&bytes, raster::ImportLimits::default()).unwrap();
    let page = &pages.pages[0];
    assert_eq!((page.width, page.height), (40, 30));
    let rgba = page.pixels.clone().into_rgba8();
    let at = |x: u32, y: u32| {
        let i = ((y * page.width + x) * 4) as usize;
        [rgba[i], rgba[i + 1], rgba[i + 2]]
    };
    assert_eq!(at(10, 10), [255, 0, 0], "the square");
    assert_eq!(at(30, 25), [255, 255, 255], "the white artboard");
}

#[test]
fn emf_and_dxf_exports_carry_the_vector_layers() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = two_boards_with_a_shape(dir.path());
    let emf = std::fs::read(export(&mut ed, ExportFormat::Emf, &dir.path().join("e"))).unwrap();
    assert_eq!(&emf[40..44], b" EMF");
    let drawn = raster::codec::decode_surface_bytes(&emf, raster::ImportLimits::default()).unwrap();
    let rgba = drawn.pixels.into_rgba8();
    let i = ((10 * drawn.width + 10) * 4) as usize;
    assert_eq!(&rgba[i..i + 3], &[255, 0, 0], "the square, as a GDI path");
    let dxf =
        std::fs::read_to_string(export(&mut ed, ExportFormat::Dxf, &dir.path().join("d"))).unwrap();
    // The plate's rectangle and the square: two closed polylines.
    assert_eq!(dxf.matches("\nPOLYLINE\n").count(), 2, "{dxf}");
    assert!(dxf.contains("62\n1\n"), "the square is red (ACI 1)");
}

#[test]
fn a_page_with_a_blend_mode_goes_out_as_one_image() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 12, 10);
    let id = red_layer(&mut ed, 0, 0, 12, 10);
    ed.apply_command(Command::SetLayerProperties {
        layer_id: id,
        patch: editor_core::LayerPatch {
            blend_mode: Some(layer_model::BlendMode::Multiply),
            ..editor_core::LayerPatch::default()
        },
    });
    let scene = {
        let open = ed.active().unwrap();
        super::w16k::vector_doc(&open.document, &open.tiles).unwrap()
    };
    assert_eq!(scene.pages.len(), 1);
    assert_eq!(scene.pages[0].counts(), (0, 1));
}

// ---------------------------------------------------------------------------
// File > Open: one-page PDF, and one decode per flat open
// ---------------------------------------------------------------------------

fn one_page_pdf() -> Vec<u8> {
    ev::encode_pdf(&ev::VectorDoc {
        pages: vec![ev::Page {
            width: 12,
            height: 7,
            items: vec![ev::Item::Image(ev::PlacedImage {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                rgba: vec![255, 0, 0, 255],
            })],
        }],
        title: String::new(),
    })
    .unwrap()
}

#[test]
fn a_one_page_pdf_asks_through_the_import_dialog_and_an_ai_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("card.pdf");
    std::fs::write(&pdf, one_page_pdf()).unwrap();
    let mut ed = editor_in(dir.path());
    let opened = ed.open_paths(std::slice::from_ref(&pdf));
    assert!(opened.is_empty(), "nothing opens before the dialog answers");
    assert!(ed.documents().is_empty());
    crate::editor::open_any::w13x7::accept_import_defaults_for_test(&mut ed);
    assert_eq!(ed.documents().len(), 1, "the confirmed page opened");

    let ai = dir.path().join("card.ai");
    std::fs::write(&ai, one_page_pdf()).unwrap();
    let mut ed = editor_in(dir.path());
    ed.open_paths(std::slice::from_ref(&ai));
    assert_eq!(ed.documents().len(), 1, "an .ai opens as a picture");
    assert!(crate::editor::open_any::w13x7::take_pending().is_none());
}

static HEIC: &[u8] =
    include_bytes!("../../raster/src/formats/testdata/w15a_grey_64x128_irot90_alpha.heic");
static WORKER_DECODES: AtomicUsize = AtomicUsize::new(0);

/// A stand-in for the decode worker: it counts a decode of this test's HEIC
/// (one call is one worker process) and answers every other file exactly as
/// a process with no worker does, so no other test sees a difference.
fn counting_worker(
    kind: raster::codec::formats::heif::HeifKind,
    bytes: &[u8],
    limits: raster::ImportLimits,
) -> Result<raster::DecodedSurface, raster::CodecError> {
    if bytes != HEIC {
        return Err(raster::codec::formats::heif::no_worker_refusal(kind));
    }
    WORKER_DECODES.fetch_add(1, Ordering::SeqCst);
    raster::codec::formats::heif::decode_in_this_process(kind, bytes, limits)
}

fn inline(_: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    body();
    Ok(())
}

#[test]
fn a_flat_heic_open_starts_one_decode_worker() {
    raster::codec::formats::heif::install_isolated_decoder(counting_worker);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.heic");
    std::fs::write(&path, HEIC).unwrap();
    let before = WORKER_DECODES.load(Ordering::SeqCst);
    let rx = crate::jobs::spawn_import_with(path, 1, 8, inline);
    match rx.recv().expect("the import completes") {
        crate::jobs::ImportOutcome::Image { decoded, .. } => {
            let img = decoded.expect("the HEIC decodes");
            assert_eq!((img.width, img.height), (128, 64), "rotated by irot");
        }
        _ => panic!("a flat image outcome"),
    }
    assert_eq!(
        WORKER_DECODES.load(Ordering::SeqCst) - before,
        1,
        "one flat open, one worker"
    );
}

// ---------------------------------------------------------------------------
// File > Script: demos and saved scripts
// ---------------------------------------------------------------------------

fn script_frame(ctx: &egui::Context, events: Vec<egui::Event>) -> Vec<ui::Intent> {
    let mut intents = Vec::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 1000.0),
        )),
        events,
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| {
        crate::script::draw_window(ctx, &mut |i| intents.push(i));
    });
    intents
}

fn script_click(ctx: &egui::Context, id: egui::Id) -> Vec<ui::Intent> {
    for _ in 0..3 {
        script_frame(ctx, Vec::new());
    }
    let at = ctx
        .read_response(id)
        .unwrap_or_else(|| panic!("{id:?} was not drawn"))
        .rect
        .center();
    let press = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    script_frame(
        ctx,
        vec![egui::Event::PointerMoved(at), press(true), press(false)],
    )
}

#[test]
fn the_script_window_runs_a_demo_and_saves_and_deletes_a_script() {
    use ui::dialogs::ScriptDialog;
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with_doc(dir.path(), 50, 60);
    red_layer(&mut ed, 0, 0, 10, 10);
    let layers_before = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len();
    click(&mut ed, MenuAction::Script).unwrap();
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);

    // A demo loads into the code box; Run runs it through File > Script.
    let grid = crate::script::DEMOS
        .iter()
        .position(|(n, _)| *n == "Grid")
        .unwrap();
    script_click(&ctx, ScriptDialog::demo_id(grid));
    assert_eq!(
        crate::script::with_window(|w| w.source().to_string()).as_deref(),
        Some(crate::script::DEMOS[grid].1)
    );
    let run = crate::script::with_window(|w| w.run_rect())
        .flatten()
        .unwrap();
    let at = run.center();
    let intents = script_frame(
        &ctx,
        vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ],
    );
    assert_eq!(intents, vec![ui::Intent::Action(MenuAction::Script)]);
    super::perform(MenuAction::Script, &mut ed).unwrap();
    let layers_after = ed
        .active()
        .unwrap()
        .document
        .layers
        .iter_depth_first()
        .len();
    assert_eq!(
        layers_after,
        layers_before + 29,
        "the grid demo made 29 copies"
    );

    // Save keeps the source in the scripts folder and lists it.
    crate::script::with_window(|w| {
        w.set_source("alert(1)");
        w.set_save_name("Mine");
    });
    let intents = script_click(&ctx, ScriptDialog::save_id());
    assert_eq!(intents, vec![ui::Intent::Action(MenuAction::Script)]);
    super::perform(MenuAction::Script, &mut ed).unwrap();
    let file = crate::script::scripts_dir(&ed).join("Mine.jsx");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "alert(1)");
    assert_eq!(
        crate::script::with_window(|w| w.saved().to_vec()).unwrap(),
        vec![("Mine".to_string(), "alert(1)".to_string())]
    );
    // A fresh window lists it again (it outlives the window).
    crate::script::with_window(|w| w.set_source(""));
    script_click(&ctx, ScriptDialog::saved_id(0));
    assert_eq!(
        crate::script::with_window(|w| w.source().to_string()).as_deref(),
        Some("alert(1)")
    );
    // Delete removes the file and the row.
    script_click(&ctx, ScriptDialog::delete_id(0));
    super::perform(MenuAction::Script, &mut ed).unwrap();
    assert!(!file.exists());
    assert!(crate::script::with_window(|w| w.saved().is_empty()).unwrap());
}
