//! W10-E: the File-menu gaps, driven the way a user reaches them — the menu
//! row resolved and clicked through the chrome, the dialog the host opened
//! confirmed with Enter in a headless egui frame, the pick that confirmation
//! rode out of the frame performed — and checked on what lands on disk or in
//! the document.

use std::path::{Path, PathBuf};

use editor_core::Command;
use layer_model::{AdjustmentKind, AdjustmentLayer, Layer, LayerKind};
use ui::menu::MenuAction;

use crate::chrome::{Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::file_extras::FileExtrasDialog;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor_with(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    std::fs::write(
        path,
        raster::encode(raster::ExportFormat::Png, w, h, rgba).unwrap(),
    )
    .unwrap();
}

fn solid(w: u32, h: u32, px: [u8; 4]) -> Vec<u8> {
    (0..w * h).flat_map(|_| px).collect()
}

fn raw_input(events: Vec<egui::Event>) -> egui::RawInput {
    egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    }
}

fn enter() -> egui::Event {
    egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

/// Click `action`'s menu row: resolved against the live editor, routed
/// through the chrome exactly as the menu bar routes a click.
fn click(ed: &mut Editor, action: MenuAction) -> (Chrome, ChromeOutput) {
    let mut chrome = Chrome::new();
    let ctx = crate::menu_bridge::context(ed, chrome.workspace());
    let intent = crate::menu_bridge::resolve_intent(action, &ctx, ed)
        .unwrap_or_else(|reason| panic!("{action:?} is disabled: {reason}"));
    let mut out = ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    (chrome, out)
}

/// A settle frame, then Enter, through the host's own `ui`.
fn press_enter(chrome: &mut Chrome) -> ChromeOutput {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let mut out = ChromeOutput::default();
    let host = chrome.dialogs_for_test();
    let _ = ctx.run(raw_input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
    let _ = ctx.run(raw_input(vec![enter()]), |ctx| host.ui(ctx, None, &mut out));
    out
}

/// Perform every pick a frame produced, as the shell does.
fn perform_all(ed: &mut Editor, out: &ChromeOutput) -> Vec<Result<String, String>> {
    out.menu
        .iter()
        .map(|action| crate::menu_bridge::perform(*action, ed))
        .collect()
}

fn decode(path: &Path) -> raster::DecodedImage {
    raster::decode_bytes(&std::fs::read(path).unwrap()).unwrap()
}

// ---------------------------------------------------------------------------
// Batch / Convert Formats
// ---------------------------------------------------------------------------

/// File ▸ Automate ▸ Batch… over three files: the recorded Action (a new
/// Invert adjustment layer) is played on each, and three inverted files land
/// in the destination. A text file in the source is not an image and is not
/// touched.
#[test]
fn batch_over_three_files_writes_three_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("in"), dir.path().join("out"));
    std::fs::create_dir_all(&src).unwrap();
    let colours = [[10u8, 20, 30, 255], [200, 100, 50, 255], [0, 255, 128, 255]];
    for (i, c) in colours.iter().enumerate() {
        write_png(&src.join(format!("img{i}.png")), 6, 4, &solid(6, 4, *c));
    }
    std::fs::write(src.join("notes.txt"), b"not an image").unwrap();

    // Record the Action on an open document.
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let probe = dir.path().join("probe.png");
    write_png(&probe, 4, 4, &solid(4, 4, [1, 2, 3, 255]));
    ed.open_path(&probe).unwrap();
    ed.start_recording();
    ed.apply_command(Command::create_layer(Layer::with_kind(
        "Invert",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }),
    )));
    ed.stop_recording();
    assert_eq!(ed.actions().len(), 1, "the recording is in the library");

    let (mut chrome, _) = click(&mut ed, MenuAction::AutomateBatch);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::Batch(d) => {
            assert_eq!(d.spec().action, Some(0));
            d.set_folder(ui::dialogs::BatchFolder::Source, src.clone());
            d.set_folder(ui::dialogs::BatchFolder::Destination, dst.clone());
        }
        other => panic!("Batch opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::AutomateBatch]);
    let results = perform_all(&mut ed, &out);
    let status = results[0].clone().expect("the batch ran");
    assert!(status.contains("wrote 3 file(s)"), "{status}");

    let mut written: Vec<PathBuf> = std::fs::read_dir(&dst)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    written.sort();
    assert_eq!(written.len(), 3, "{written:?}");
    for (i, c) in colours.iter().enumerate() {
        let out = decode(&dst.join(format!("img{i}.png")));
        assert_eq!((out.width, out.height), (6, 4));
        let want = [255 - c[0], 255 - c[1], 255 - c[2], 255];
        for px in out.rgba8.as_chunks::<4>().0 {
            for k in 0..3 {
                assert!(
                    (i32::from(px[k]) - i32::from(want[k])).abs() <= 1,
                    "img{i}: {px:?} is not the inverse {want:?}"
                );
            }
        }
    }
    assert!(
        !dst.join(crate::automate::ERROR_LOG).exists(),
        "nothing failed"
    );
}

/// Bodies the queueing spawner holds until the test runs them.
static HELD: std::sync::Mutex<Vec<Box<dyn FnOnce() + Send>>> = std::sync::Mutex::new(Vec::new());

/// A spawner that starts nothing: the body waits in [`HELD`].
fn hold(_name: String, body: Box<dyn FnOnce() + Send>) -> std::io::Result<()> {
    HELD.lock().unwrap().push(body);
    Ok(())
}

/// Convert Formats runs on the job worker: the menu pick returns with nothing
/// written and "0 of 4 files" on the status line; the work happens when the
/// worker body runs — here on another thread — and the next poll lands the
/// summary. Three images become half-size JPEGs; a file that is not really a
/// PNG is skipped and named in the error log.
#[test]
fn convert_formats_runs_on_the_worker_and_logs_what_failed() {
    let dir = tempfile::tempdir().unwrap();
    let (src, dst) = (dir.path().join("in"), dir.path().join("out"));
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..3 {
        write_png(
            &src.join(format!("p{i}.png")),
            8,
            6,
            &solid(8, 6, [40 * i as u8, 90, 160, 255]),
        );
    }
    std::fs::write(src.join("broken.png"), b"this is not a png").unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    ed.set_spawner(hold);

    let (mut chrome, _) = click(&mut ed, MenuAction::ConvertFormats);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::Batch(d) => {
            let mut spec = d.spec().clone();
            spec.source = src.clone();
            spec.destination = dst.clone();
            spec.format = ui::dialogs::BatchFormat::Jpeg;
            spec.quality = 80;
            spec.scale_percent = 50;
            d.set_spec(spec);
        }
        other => panic!("Convert Formats opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::ConvertFormats]);
    let started = perform_all(&mut ed, &out).remove(0).expect("started");
    assert_eq!(started, "Convert Formats: 0 of 4 files");
    assert_eq!(ed.status(), Some("Convert Formats: 0 of 4 files"));
    assert!(crate::automate::pending());
    assert!(
        !dst.join("p0.jpg").exists(),
        "nothing ran on the calling thread"
    );
    assert_eq!(crate::automate::poll(&mut ed), None, "still running");
    let body = HELD
        .lock()
        .unwrap()
        .pop()
        .expect("the job was handed to the spawner");
    std::thread::spawn(body).join().unwrap();
    let summary = crate::automate::poll(&mut ed).expect("the batch finished");
    assert!(summary.contains("wrote 3 file(s), 1 failed"), "{summary}");
    assert_eq!(ed.status(), Some(summary.as_str()));
    for i in 0..3 {
        let out = decode(&dst.join(format!("p{i}.jpg")));
        assert_eq!((out.width, out.height), (4, 3), "50% of 8 x 6");
    }
    let log = std::fs::read_to_string(dst.join(crate::automate::ERROR_LOG)).unwrap();
    assert!(log.contains("broken.png"), "{log}");
    assert!(!crate::automate::pending());
}

// ---------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------

/// Image ▸ Variables: a text variable on a text layer and a visibility
/// variable on a red block, a two-row CSV imported on the Data Sets page, and
/// Export writes one file per set, each exactly the document with that set's
/// text substituted and that set's visibility applied. Preview then lands one
/// set on the live document as one undo step.
#[test]
fn a_variables_csv_with_two_rows_exports_two_files_with_the_substituted_text() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("sets");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut ed = editor_with(
        dir.path(),
        ScriptedDialogs::new().exporting_folder(&out_dir),
    );
    let base = dir.path().join("card.png");
    write_png(&base, 64, 32, &solid(64, 32, [255, 255, 255, 255]));
    ed.open_path(&base).unwrap();
    let title = Layer::with_kind(
        "Title",
        LayerKind::Text(layer_model::text::TextLayer::legacy(
            "Placeholder",
            "DejaVu Sans",
            12.0,
        )),
    );
    let title_id = title.id;
    ed.apply_command(Command::create_layer(title));
    let badge = Layer::raster("Badge");
    let badge_id = badge.id;
    ed.apply_command(Command::create_layer(badge));
    let paint = {
        let doc = ed.active_mut().unwrap();
        let mut rgba = vec![0u8; 64 * 32 * 4];
        for y in 20..30 {
            for x in 40..60 {
                let i = (y * 64 + x) * 4;
                rgba[i..i + 4].copy_from_slice(&[255, 0, 0, 255]);
            }
        }
        crate::menu_bridge::pixels::write_layer(doc, badge_id, &rgba, "Badge").unwrap()
    };
    ed.apply_command(paint);

    // Define: bind through the dialog the menu opens, confirm with Enter.
    let (mut chrome, _) = click(&mut ed, MenuAction::DefineVariables);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::Variables(d) => {
            // Top of the stack first: Badge, then Title, then the image.
            d.bind(0, ui::dialogs::VariableKind::Visibility, Some("badge"));
            d.bind(1, ui::dialogs::VariableKind::Text, Some("title"));
        }
        other => panic!("Define opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::DefineVariables]);
    let saved = perform_all(&mut ed, &out).remove(0).expect("defined");
    assert!(saved.contains("2 defined"), "{saved}");

    // Data Sets: import a CSV through the Import request, export.
    let csv = dir.path().join("sets.csv");
    std::fs::write(&csv, "title,badge\r\nHello,true\r\n\"Good, bye\",false\r\n").unwrap();
    let (mut chrome, _) = click(&mut ed, MenuAction::DataSets);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::Variables(d) => {
            assert_eq!(d.defs().len(), 2, "the definitions were kept");
            assert_eq!(d.load_csv(&std::fs::read_to_string(&csv).unwrap()), Ok(2));
            d.set_request(ui::dialogs::VariablesRequest::Export(
                ui::dialogs::BatchFormat::Png,
            ));
        }
        other => panic!("Data Sets opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::DataSets]);
    let exported = perform_all(&mut ed, &out).remove(0).expect("exported");
    assert!(exported.contains("exported 2 data set(s)"), "{exported}");

    let (defs, sets) = crate::variables::stored(ed.active().unwrap().id());
    assert_eq!(sets.len(), 2);
    let files: Vec<PathBuf> = sets
        .iter()
        .map(|s| out_dir.join(format!("card_{}.png", raster::sanitize_file_stem(&s.name))))
        .collect();
    let open = ed.active().unwrap();
    let rect = raster::PixelRect::new(0, 0, 64, 32);
    for ((set, file), (text, badge_on)) in sets
        .iter()
        .zip(&files)
        .zip([("Hello", true), ("Good, bye", false)])
    {
        let staged = crate::variables::staged_document(&open.document, &defs, set);
        let LayerKind::Text(t) = &staged.layers.get(title_id).unwrap().kind else {
            panic!("the title is still a text layer");
        };
        assert_eq!(t.text, text, "{} substitutes the text", set.name);
        assert_eq!(staged.layers.get(badge_id).unwrap().visible, badge_on);
        let want = compositor::composite_region(
            &staged,
            &open.tiles,
            rect,
            0,
            compositor::CompositeOptions::default(),
        )
        .unwrap()
        .to_rgba8(&staged.meta.color_space);
        let got = decode(file);
        assert_eq!(got.rgba8, want, "{} is that set's document", file.display());
        let red = &got.rgba8[(25 * 64 + 50) * 4..(25 * 64 + 50) * 4 + 3];
        assert_eq!(
            red == [255, 0, 0],
            badge_on,
            "the badge follows its variable"
        );
    }
    // The live document was not touched by the export.
    let LayerKind::Text(t) = &ed
        .active()
        .unwrap()
        .document
        .layers
        .get(title_id)
        .unwrap()
        .kind
    else {
        panic!()
    };
    assert_eq!(t.text, "Placeholder");

    // Preview the second set: one undo step, undone by one Undo.
    let depth = ed.active().unwrap().history_depth();
    let spec = ui::dialogs::VariablesSpec {
        defs: defs.clone(),
        sets: sets.clone(),
        request: ui::dialogs::VariablesRequest::Preview(1),
    };
    crate::variables::perform(&mut ed, spec).expect("previewed");
    let doc = &ed.active().unwrap().document;
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
    assert!(!doc.layers.get(badge_id).unwrap().visible);
    let LayerKind::Text(t) = &doc.layers.get(title_id).unwrap().kind else {
        panic!()
    };
    assert_eq!(t.text, "Good, bye");
}

// ---------------------------------------------------------------------------
// Export Color Lookup
// ---------------------------------------------------------------------------

/// File ▸ Export ▸ Color Lookup Tables…: a document whose only visible
/// adjustment is an identity Levels exports the identity cube (a hidden
/// Invert is left out); showing the Invert exports its inverse. The file is
/// read back through the Color Lookup adjustment's own `.cube` parser.
#[test]
fn an_exported_identity_stack_lut_is_identity() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("luts");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut ed = editor_with(
        dir.path(),
        ScriptedDialogs::new()
            .exporting_folder(&out_dir)
            .exporting_folder(&out_dir),
    );
    let base = dir.path().join("look.png");
    write_png(&base, 4, 4, &solid(4, 4, [9, 9, 9, 255]));
    ed.open_path(&base).unwrap();
    ed.apply_command(Command::create_layer(Layer::with_kind(
        "Levels",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Levels {
                black: 0.0,
                white: 1.0,
                gamma: 1.0,
            },
        }),
    )));
    let mut invert = Layer::with_kind(
        "Invert",
        LayerKind::Adjustment(AdjustmentLayer {
            kind: AdjustmentKind::Invert,
        }),
    );
    invert.visible = false;
    let invert_id = invert.id;
    ed.apply_command(Command::create_layer(invert));

    let export = |ed: &mut Editor, name: &str| -> adjustments::Lut3d {
        let (mut chrome, _) = click(ed, MenuAction::ExportColorLookup);
        match chrome.dialogs_for_test().active_file_extras_for_test() {
            FileExtrasDialog::ExportLut(d) => d.set_spec(ui::dialogs::ExportLutSpec {
                grid: ui::dialogs::LutGrid::Small,
                title: name.to_string(),
            }),
            other => panic!("Export Color Lookup opened {other:?}"),
        }
        let out = press_enter(&mut chrome);
        assert_eq!(out.menu, vec![MenuAction::ExportColorLookup]);
        let status = perform_all(ed, &out).remove(0).expect("exported");
        assert!(status.contains("17-point"), "{status}");
        let text = std::fs::read_to_string(out_dir.join(format!("{name}.cube"))).unwrap();
        adjustments::Lut3d::parse_cube("x", &text).expect("a valid .cube")
    };

    let lut = export(&mut ed, "identity");
    assert_eq!(lut.size(), 17);
    assert_eq!(lut.name(), "identity");
    let identity = adjustments::Lut3d::identity(17);
    let worst = lut
        .table()
        .iter()
        .zip(identity.table())
        .flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs()))
        .fold(0.0f32, f32::max);
    assert!(
        worst < 1e-4,
        "the identity stack exports the identity: off by {worst}"
    );

    ed.apply_command(Command::SetLayerProperties {
        layer_id: invert_id,
        patch: editor_core::LayerPatch {
            visible: Some(true),
            ..Default::default()
        },
    });
    // Bottom to top: the Levels was made first, so it applies first.
    let order: Vec<_> = crate::file_extras::visible_adjustments(&ed.active().unwrap().document)
        .into_iter()
        .map(|(kind, _)| matches!(kind, AdjustmentKind::Invert))
        .collect();
    assert_eq!(order, vec![false, true], "Levels under Invert");
    let inverted = export(&mut ed, "inverted");
    for (a, b) in inverted.table().iter().zip(identity.table()) {
        for k in 0..3 {
            assert!((a[k] - (1.0 - b[k])).abs() < 1e-3, "{a:?} vs 1 - {b:?}");
        }
    }
}

// ---------------------------------------------------------------------------
// Vectorize Bitmap
// ---------------------------------------------------------------------------

/// Image ▸ Vectorize Bitmap… on a two-colour image: two shape layers, one
/// per colour, whose union covers every pixel of the canvas with the right
/// colour away from the disc's rim; the source is hidden; one Undo removes
/// the lot.
#[test]
fn vectorizing_a_two_colour_image_yields_two_shape_layers_whose_union_covers_the_image() {
    let dir = tempfile::tempdir().unwrap();
    let (w, h) = (40u32, 30u32);
    let (red, blue) = ([220u8, 30, 30, 255], [20u8, 40, 200, 255]);
    let rgba: Vec<u8> = (0..h)
        .flat_map(|y| (0..w).map(move |x| (x, y)))
        .flat_map(|(x, y)| {
            let (dx, dy) = (x as f64 + 0.5 - 20.0, y as f64 + 0.5 - 15.0);
            if dx * dx + dy * dy <= 81.0 {
                red
            } else {
                blue
            }
        })
        .collect();
    let path = dir.path().join("two.png");
    write_png(&path, w, h, &rgba);
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    ed.open_path(&path).unwrap();
    let source = ed.active().unwrap().document.active_layer().unwrap();
    let before: Vec<_> = ed.active().unwrap().document.layers.iter_depth_first();
    let depth = ed.active().unwrap().history_depth();

    let (mut chrome, _) = click(&mut ed, MenuAction::VectorizeBitmap);
    assert!(matches!(
        chrome.dialogs_for_test().active_file_extras_for_test(),
        FileExtrasDialog::Vectorize(_)
    ));
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::VectorizeBitmap]);
    let status = perform_all(&mut ed, &out).remove(0).expect("vectorized");
    assert!(status.contains("2 colour(s)"), "{status}");

    let open = ed.active_mut().unwrap();
    assert_eq!(open.history_depth(), depth + 1, "one undo step");
    let shapes: Vec<_> = open
        .document
        .layers
        .iter_depth_first()
        .into_iter()
        .filter(|id| !before.contains(id))
        .collect();
    assert_eq!(shapes.len(), 2, "one shape layer per colour");
    for id in &shapes {
        assert!(matches!(
            open.document.layers.get(*id).unwrap().kind,
            LayerKind::Shape(_)
        ));
    }
    assert!(
        !open.document.layers.get(source).unwrap().visible,
        "source hidden"
    );
    let composite = open.composite(open.canvas_rect()).unwrap();
    let mut rim = 0;
    for (i, px) in composite.as_chunks::<4>().0.iter().enumerate() {
        assert_eq!(px[3], 255, "pixel {i} is covered by the shapes' union");
        let want = &rgba[i * 4..i * 4 + 3];
        if (0..3).any(|k| (i32::from(px[k]) - i32::from(want[k])).abs() > 8) {
            // Only the anti-aliased rim of the smoothed disc may differ.
            let (x, y) = ((i as u32 % w) as f64 + 0.5, (i as u32 / w) as f64 + 0.5);
            let r = ((x - 20.0).powi(2) + (y - 15.0).powi(2)).sqrt();
            assert!(
                (r - 9.0).abs() <= 1.5,
                "pixel ({x}, {y}) off the rim differs: {px:?}"
            );
            rim += 1;
        }
    }
    assert!(rim < 80, "{rim} rim pixels differ");
    ed.dispatch(crate::Action::Undo).unwrap();
    let doc = &ed.active().unwrap().document;
    assert!(shapes.iter().all(|id| !doc.layers.contains(*id)));
    assert!(doc.layers.get(source).unwrap().visible);
}

// ---------------------------------------------------------------------------
// Export PDF
// ---------------------------------------------------------------------------

/// File ▸ Export ▸ PDF… on an A4 page: the file parses — header, every xref
/// row on its object, the trailer naming the catalog and the info
/// dictionary, the A4 media box, File Info's title — and it is exactly the
/// raster PDF encoder's page for the flattened composite.
#[test]
fn pdf_export_parses() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("pdf");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut ed = editor_with(
        dir.path(),
        ScriptedDialogs::new().exporting_folder(&out_dir),
    );
    let (w, h) = (12u32, 8u32);
    let rgba: Vec<u8> = (0..w * h)
        .flat_map(|i| [i as u8 * 2, 90, 200, 255])
        .collect();
    let src = dir.path().join("poster.png");
    write_png(&src, w, h, &rgba);
    ed.open_path(&src).unwrap();
    crate::file_extras::set_file_info(
        &mut ed,
        raster::metadata::XmpFields {
            title: "Poster".into(),
            ..Default::default()
        },
    )
    .unwrap();

    let (mut chrome, _) = click(&mut ed, MenuAction::ExportPdf);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::ExportPdf(d) => d.set_spec(ui::dialogs::PdfExportSpec {
            page: ui::dialogs::PdfPageSize::A4,
            ..Default::default()
        }),
        other => panic!("Export PDF opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::ExportPdf]);
    perform_all(&mut ed, &out).remove(0).expect("exported");

    let pdf = std::fs::read(out_dir.join("poster.pdf")).unwrap();
    assert!(pdf.starts_with(b"%PDF-"));
    assert!(pdf.ends_with(b"%%EOF\n"));
    let text = String::from_utf8_lossy(&pdf).into_owned();
    let startxref: usize = text[text.rfind("startxref\n").unwrap() + 10..]
        .lines()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(pdf[startxref..].starts_with(b"xref\n0 7\n"));
    let rows = startxref + b"xref\n0 7\n".len();
    for n in 1..7usize {
        let off: usize = std::str::from_utf8(&pdf[rows + n * 20..rows + n * 20 + 10])
            .unwrap()
            .parse()
            .unwrap();
        assert!(pdf[off..].starts_with(format!("{n} 0 obj").as_bytes()));
    }
    assert!(text.contains("/Root 1 0 R /Info 6 0 R"));
    assert!(text.contains("/MediaBox [0 0 595.276 841.890]"), "A4");
    assert!(text.contains("/Title (Poster)"));
    // The image: byte for byte what the tested raster encoder writes for
    // the flattened composite on A4 with this File Info (its own test
    // inflates the stream back to the samples).
    let open = ed.active_mut().unwrap();
    let composite = open.composite(open.canvas_rect()).unwrap();
    assert_eq!(
        composite, rgba,
        "an opaque single-layer document flattens to itself"
    );
    let info = raster::pdf::PdfInfo {
        title: "Poster".into(),
        ..Default::default()
    };
    let want =
        raster::pdf::encode_pdf_on_page(w, h, &composite, raster::pdf::PdfPage::A4, Some(&info));
    assert_eq!(pdf, want, "the page holds the flattened pixels");
}

// ---------------------------------------------------------------------------
// File Info XMP and EXIF through Export As
// ---------------------------------------------------------------------------

/// File ▸ File Info… edits the XMP fields; Export As, opened afterwards from
/// the menu, carries them and the JPEG source's EXIF into the files the
/// export job writes: the title round-trips through the PNG, and the JPEG
/// carries both. Unticking the Metadata box writes neither.
#[test]
fn the_xmp_title_round_trips_through_png_and_exif_is_kept_on_jpeg_export() {
    let dir = tempfile::tempdir().unwrap();
    let out_dir = dir.path().join("export");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    // A JPEG source carrying an EXIF block.
    let exif = b"II*\0\x08\0\0\0\0\0\0\0\0\0".to_vec();
    let jpeg = raster::encode(
        raster::ExportFormat::Jpeg(92),
        8,
        8,
        &solid(8, 8, [90, 60, 30, 255]),
    )
    .unwrap();
    let src = dir.path().join("photo.jpg");
    std::fs::write(
        &src,
        raster::metadata::embed_jpeg_exif(&jpeg, &exif).unwrap(),
    )
    .unwrap();
    ed.open_path(&src).unwrap();

    let (mut chrome, _) = click(&mut ed, MenuAction::FileInfo);
    match chrome.dialogs_for_test().active_file_extras_for_test() {
        FileExtrasDialog::FileInfo(d) => d.set_fields(raster::metadata::XmpFields {
            title: "Harbour <at> dusk".into(),
            author: "A. Painter".into(),
            keywords: vec!["sea".into(), "boats".into()],
            copyright: "(c) 2026".into(),
            ..Default::default()
        }),
        other => panic!("File Info opened {other:?}"),
    }
    let out = press_enter(&mut chrome);
    assert_eq!(out.menu, vec![MenuAction::FileInfo]);
    perform_all(&mut ed, &out).remove(0).expect("stored");
    assert!(!ed.file_info_open(), "the XMP dialog, not the facts window");

    let (mut chrome, _) = click(&mut ed, MenuAction::Export(raster::ExportFormat::Png));
    let job = {
        let host = chrome.dialogs_for_test();
        let crate::dialog_host::ActiveDialog::ExportAs(dialog) = host.active_for_test() else {
            panic!("Export As did not open");
        };
        dialog.add_entry();
        dialog.entry_mut(1).unwrap().preset.format = raster::ExportFormat::Jpeg(90);
        dialog.entry_mut(1).unwrap().suffix = "-j".into();
        let job = dialog.job();
        dialog.set_embed_metadata(false);
        (job, dialog.job())
    };
    let by_ext = |dir: &Path, ext: &str| -> Vec<u8> {
        let path = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|e| e == ext))
            .unwrap_or_else(|| panic!("no .{ext} in {}", dir.display()));
        std::fs::read(path).unwrap()
    };
    ed.request_export(job.0, out_dir.clone());
    let png = by_ext(&out_dir, "png");
    let packet = raster::metadata::read_xmp(&png).expect("the PNG carries XMP");
    let fields = raster::metadata::XmpFields::from_packet(&packet);
    assert_eq!(fields.title, "Harbour <at> dusk");
    assert_eq!(fields.keywords, vec!["sea", "boats"]);
    assert_eq!(raster::metadata::read_exif(&png), None);
    let jpg = by_ext(&out_dir, "jpg");
    assert_eq!(
        raster::metadata::read_exif(&jpg).as_deref(),
        Some(&exif[..])
    );
    assert_eq!(
        raster::metadata::XmpFields::from_packet(&raster::metadata::read_xmp(&jpg).unwrap()).author,
        "A. Painter"
    );

    let plain_dir = dir.path().join("plain");
    std::fs::create_dir_all(&plain_dir).unwrap();
    ed.request_export(job.1, plain_dir.clone());
    assert_eq!(raster::metadata::read_xmp(&by_ext(&plain_dir, "png")), None);
    assert_eq!(
        raster::metadata::read_exif(&by_ext(&plain_dir, "jpg")),
        None
    );
}
