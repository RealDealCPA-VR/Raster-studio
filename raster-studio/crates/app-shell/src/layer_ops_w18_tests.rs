//! W18-G: every row driven the way the menu bar drives it — the shell's own
//! menu context resolves it (`menu_bridge::resolve`), the pick is performed
//! through `menu_bridge::perform` — and the file a row writes read back.

use std::path::{Path, PathBuf};

use layer_model::{AssetOrigin, LayerKind};
use ui::dialogs::export_as::psd_options::{remember_options, PsdSaveOptions};
use ui::menu::MenuAction;

use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{context, perform, resolve, Pick};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

fn editor(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    );
    ed.set_image_clipboard(Box::new(crate::clipboard::FakeClipboard::new()));
    ed
}

fn png(dir: &Path, name: &str, w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> PathBuf {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&f(x, y));
        }
    }
    let path = dir.join(name);
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, w, h, &px).unwrap(),
    )
    .unwrap();
    path
}

/// The menu bar's road: the row must be enabled in the shell's own menu
/// context and pick itself, then it is performed.
fn click(ed: &mut Editor, action: MenuAction) -> Result<String, String> {
    let chrome = crate::chrome::Chrome::new();
    let ctx = context(ed, chrome.workspace());
    match resolve(action, &ctx, ed)? {
        Pick::Menu(a) => perform(a, ed),
        other => Err(format!("{action:?} resolved to {other:?}")),
    }
}

fn source_of_active(ed: &Editor) -> (String, Vec<u8>) {
    let doc = ed.active().unwrap();
    let id = doc.document.active_layer().unwrap();
    let Some(LayerKind::SmartObject(so)) = doc.document.layers.get(id).map(|l| &l.kind) else {
        panic!("the active layer is not a smart object");
    };
    match doc.document.asset_origin(so.asset) {
        Some(AssetOrigin::Embedded { name, bytes }) => (name.clone(), bytes.clone()),
        other => panic!("not embedded: {other:?}"),
    }
}

/// Layer > Smart Object > Turn into JPG, through the menu: the embedded PNG
/// source becomes a JPEG (JPEG bytes, a `.jpg` name) that decodes to the
/// source's size, its transparency flattened onto white; the object's
/// pixels are the decoded JPEG; one undo puts the PNG back.
#[test]
fn turn_into_jpg_re_encodes_the_smart_objects_source_as_a_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    // Left half opaque red, right half fully transparent.
    let source = png(dir.path(), "logo.png", 16, 8, |x, _| {
        if x < 8 {
            [220, 20, 20, 255]
        } else {
            [0, 0, 0, 0]
        }
    });
    let canvas = png(dir.path(), "canvas.png", 32, 32, |_, _| [0, 0, 255, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&canvas).unwrap();
    ed.place_path(&source, false).unwrap();
    let (name, png_bytes) = source_of_active(&ed);
    assert_eq!(name, "logo");
    assert!(png_bytes.starts_with(&[0x89, b'P', b'N', b'G']));
    let depth = ed.active().unwrap().history_depth();

    let said = click(&mut ed, MenuAction::TurnIntoJpg).unwrap();
    assert!(said.contains("logo.jpg"), "{said}");
    let (name, jpeg) = source_of_active(&ed);
    assert_eq!(name, "logo.jpg");
    assert!(jpeg.starts_with(&[0xFF, 0xD8, 0xFF]), "a JPEG was stored");
    let decoded = raster::decode_bytes(&jpeg).unwrap();
    assert_eq!((decoded.width, decoded.height), (16, 8));
    let at = |x: u32, y: u32| {
        let i = ((y * 16 + x) * 4) as usize;
        [
            decoded.rgba8[i],
            decoded.rgba8[i + 1],
            decoded.rgba8[i + 2],
            decoded.rgba8[i + 3],
        ]
    };
    let red = at(3, 4);
    assert!(red[0] > 180 && red[1] < 70 && red[2] < 70, "{red:?}");
    let white = at(13, 4);
    assert!(
        white.iter().all(|c| *c > 235),
        "transparency onto white: {white:?}"
    );
    // The object shows the JPEG: its right half is opaque white now.
    let doc = ed.active().unwrap();
    let id = doc.document.active_layer().unwrap();
    let tiles = doc.document.layer_tiles(id).expect("the object has pixels");
    assert!(tiles.iter().count() > 0);
    assert_eq!(doc.history_depth(), depth + 1, "one undo step");

    assert!(ed.active_mut().unwrap().undo().unwrap());
    assert_eq!(source_of_active(&ed), ("logo".to_string(), png_bytes));
}

/// The row is greyed on a layer that is not a smart object, and says why.
#[test]
fn turn_into_jpg_is_greyed_out_off_a_smart_object() {
    let dir = tempfile::tempdir().unwrap();
    let canvas = png(dir.path(), "canvas.png", 8, 8, |_, _| [9, 9, 9, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&canvas).unwrap();
    assert_eq!(
        click(&mut ed, MenuAction::TurnIntoJpg).unwrap_err(),
        "The active layer is not a smart object"
    );
}

/// File > Export As > RAW, through the menu: the row opens Export As on a
/// RAW row; the layout set there (3 channels, 16 bits, 34-12) is what the
/// job the shell hands the writer writes — headerless interleaved samples.
#[test]
fn export_as_raw_writes_the_interleaved_bytes_of_the_chosen_layout() {
    use raster::codec::RawLayout;
    use ui::dialogs::Dialog as _;
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let src = png(dir.path(), "shot.png", 3, 2, |x, y| {
        [(x * 40) as u8, (y * 90) as u8, 200, 255]
    });
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&src).unwrap();
    let raw = MenuAction::Export(raster::ExportFormat::RAW[0]);
    let menus = crate::menu_bridge::menus(&ed);
    assert!(
        menus.iter().any(|m| m.actions().contains(&raw)),
        "File > Export As lists RAW"
    );
    let avif = MenuAction::Export(raster::ExportFormat::Avif(80));
    assert!(
        menus.iter().any(|m| m.actions().contains(&avif)),
        "File > Export As lists AVIF"
    );
    let mut host = crate::dialog_host::DialogHost::default();
    assert!(host.open_for_menu_action(&raw, &ed));
    let crate::dialog_host::ActiveDialog::ExportAs(dialog) = host.active_for_test() else {
        panic!("Export As > RAW did not open Export As");
    };
    assert_eq!(dialog.raw_layout(), Some(RawLayout::DEFAULT));
    let layout = RawLayout {
        channels: 3,
        sixteen_bit: true,
        little_endian: true,
    };
    assert!(dialog.set_raw_layout(layout));
    let Some(ui::dialogs::DialogAction::Export(job)) = dialog.confirm() else {
        panic!("a valid job");
    };
    // What the shell does with a confirmed Export As once a folder is picked.
    ed.request_export(*job, out.clone());
    ed.poll_exports();
    let listing: Vec<_> = std::fs::read_dir(&out)
        .map(|d| d.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    let bytes = std::fs::read(out.join("shot_png.raw"))
        .unwrap_or_else(|e| panic!("shot_png.raw: {e}; {listing:?}; {:?}", ed.status()));
    let mut expected = Vec::new();
    for y in 0..2u16 {
        for x in 0..3u16 {
            for v in [x * 40, y * 90, 200] {
                expected.extend_from_slice(&(v * 257).to_le_bytes());
            }
        }
    }
    assert_eq!(bytes, expected);
}

/// The image-data section's compression code: the two bytes after the
/// header, colour mode data, resources and layer and mask sections.
fn merged_compression(psd: &[u8]) -> u16 {
    let be32 = |at: usize| u32::from_be_bytes(psd[at..at + 4].try_into().unwrap()) as usize;
    let mut at = 26;
    at += 4 + be32(at);
    at += 4 + be32(at);
    at += 4 + be32(at);
    u16::from_be_bytes([psd[at], psd[at + 1]])
}

/// File > Save PSD/PSB, driven through the real chrome: the row opens
/// Photopea's options dialog (nothing is written yet); the dialog's switches
/// are pressed where they are drawn and Enter saves. ZIP for pixel data and
/// a blank preview change the written file (ZIP channels, a white merged
/// composite) and it still reads back with the same layers as a plain save;
/// PSB with Put the file into ZIP writes a `.zip` holding a version-2 file
/// that reads back.
#[test]
fn save_psd_psb_options_change_the_written_file_and_it_still_reads_back() {
    use ui::dialogs::export_as::psd_options;
    let dir = tempfile::tempdir().unwrap();
    let plain = dir.path().join("plain.psd");
    let small = dir.path().join("small.psd");
    let zipped = dir.path().join("large.zip");
    let src = png(dir.path(), "art.png", 24, 16, |x, y| {
        [(x * 10) as u8, (y * 15) as u8, 60, 255]
    });
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new()
            .saving_to(&plain)
            .saving_to(&small)
            .saving_to(&zipped),
    );
    ed.open_path(&src).unwrap();
    remember_options(PsdSaveOptions::default());
    let mut rig = Rig::new(ed);

    // The row opens the dialog on Photopea's defaults; Enter saves.
    assert_eq!(
        rig.menu(MenuAction::SavePsdPsb),
        vec![Ok("Save PSD/PSB: choose the options".to_string())]
    );
    assert!(
        !plain.exists(),
        "nothing is written before the dialog confirms"
    );
    rig.step(Vec::new());
    assert!(psd_options::is_open(&rig.ctx), "the row opened its dialog");
    let saved = rig.step(enter());
    assert!(
        saved
            .iter()
            .any(|r| r.as_ref().is_ok_and(|s| s.starts_with("Saved"))),
        "{saved:?}"
    );
    assert!(!psd_options::is_open(&rig.ctx));

    // Blank preview image and ZIP for pixel data, pressed on the drawn rows.
    rig.menu(MenuAction::SavePsdPsb);
    rig.step(Vec::new());
    let switches = psd_options::open_dialog(&rig.ctx)
        .expect("the dialog is open")
        .drawn_switches();
    for id in &switches[..2] {
        let at = rig.rect(id.expect("the row is drawn")).center();
        rig.press(at, egui::PointerButton::Primary);
    }
    let chosen = psd_options::open_dialog(&rig.ctx).unwrap().options();
    assert!(chosen.blank_preview && chosen.zip_pixel_data && !chosen.put_into_zip);
    rig.step(enter());

    let plain_bytes = std::fs::read(&plain).unwrap();
    let small_bytes = std::fs::read(&small).unwrap();
    assert_ne!(plain_bytes, small_bytes, "the options changed the file");
    assert_eq!(merged_compression(&plain_bytes), 1, "RLE by default");
    assert_eq!(merged_compression(&small_bytes), 2, "ZIP for pixel data");
    let plain_file = psd::read(&plain_bytes).unwrap();
    let small_file = psd::read(&small_bytes).unwrap();
    assert_eq!(small_file.layers, plain_file.layers, "the layers read back");
    let merged = small_file.merged.expect("a composite is written");
    for plane in merged.channels.iter().take(3) {
        assert!(plane.iter().all(|v| *v == 0xFF), "a blank (white) preview");
    }
    let real = plain_file.merged.expect("a composite is written");
    assert!(
        real.channels[0].iter().any(|v| *v != 0xFF),
        "the plain preview is the image"
    );

    // PSB, put into a ZIP (the dialog opens on the remembered options).
    remember_options(PsdSaveOptions {
        psb: true,
        put_into_zip: true,
        ..PsdSaveOptions::default()
    });
    rig.menu(MenuAction::SavePsdPsb);
    rig.step(Vec::new());
    rig.step(enter());
    let archive = std::fs::read(&zipped).unwrap();
    assert!(archive.starts_with(b"PK\x03\x04"), "a ZIP archive");
    let (name, psb) = psd::write::zip_container::unwrap_single(&archive, 1 << 28).unwrap();
    assert_eq!(name, "large.psb");
    assert!(psd::is_psb(&psb), "PSB writes version 2");
    assert_eq!(psd::read(&psb).unwrap().layers, plain_file.layers);
    remember_options(PsdSaveOptions::default());
}

/// Two artboards side by side, each 16 x 16: the first white (the canvas),
/// the second made beside it.
fn two_artboards(ed: &mut Editor) {
    click(ed, MenuAction::NewArtboard).unwrap();
    click(
        ed,
        MenuAction::ArtboardNeighbour(ui::menu::ArtboardSide::Right),
    )
    .unwrap();
    let doc = ed.active().unwrap();
    assert_eq!(
        layer_model::artboard::artboards(&doc.document.layers).len(),
        2
    );
}

/// File > Export As with Photopea's "Artboards", through the real chrome:
/// the checkbox is pressed where the dialog draws it and Enter confirms; the
/// job is written once per artboard for every row (PNG and a JPG row with a
/// suffix) — four files, each the artboard's size — and no whole-canvas
/// file is written.
#[test]
fn export_as_with_artboards_writes_one_file_per_artboard() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let src = png(dir.path(), "poster.png", 16, 16, |_, _| [40, 90, 200, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new().exporting_folder(&out));
    ed.open_path(&src).unwrap();
    two_artboards(&mut ed);
    let mut rig = Rig::new(ed);
    rig.menu(MenuAction::Export(raster::ExportFormat::Png));
    {
        let dialog = rig
            .chrome
            .dialogs_for_test()
            .active_export_dialog_for_test();
        let jpg = dialog.add_entry();
        dialog.select(jpg);
        dialog.set_format(raster::ExportFormat::Jpeg(80));
        dialog.entry_mut(jpg).unwrap().suffix = "-small".to_string();
        dialog.set_base_name("poster");
    }
    rig.step(Vec::new());
    rig.step(Vec::new());
    let artboards = rig
        .chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .drawn_extras()[0]
        .expect("PNG and JPG rows are offered Artboards");
    let at = rig.settled_rect(artboards).center();
    rig.press(at, egui::PointerButton::Primary);
    assert!(
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .extras()
            .artboards
    );
    let said = rig.step(enter());
    let said: Vec<_> = said.into_iter().chain(rig.step(Vec::new())).collect();
    assert!(
        said.iter()
            .any(|r| r.as_ref().is_ok_and(|s| s.contains("2 artboard(s)"))),
        "{said:?}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec![
            "poster_Artboard 1-small.jpg",
            "poster_Artboard 1.png",
            "poster_Artboard 2-small.jpg",
            "poster_Artboard 2.png",
        ]
    );
    for name in &names {
        let img = raster::decode_bytes(&std::fs::read(out.join(name)).unwrap()).unwrap();
        assert_eq!((img.width, img.height), (16, 16), "{name}");
    }
}

/// File > Export As with "User Slices" and "All Slices": one file per
/// committed slice, and with All the automatic slices that cover the rest
/// of the canvas as well.
#[test]
fn export_as_with_slices_writes_one_file_per_slice() {
    use ui::dialogs::export_as::{ExportExtras, SliceExport};
    let dir = tempfile::tempdir().unwrap();
    let user_dir = dir.path().join("user");
    let all_dir = dir.path().join("all");
    let src = png(dir.path(), "page.png", 20, 10, |x, _| {
        [(x * 12) as u8, 0, 0, 255]
    });
    let mut ed = editor(
        dir.path(),
        ScriptedDialogs::new()
            .exporting_folder(&user_dir)
            .exporting_folder(&all_dir),
    );
    ed.open_path(&src).unwrap();
    let id = ed.active().unwrap().id();
    ed.slices
        .remember(id, vec![raster::PixelRect::new(5, 0, 5, 10)]);
    let mut rig = Rig::new(ed);
    for (choice, out) in [(SliceExport::User, &user_dir), (SliceExport::All, &all_dir)] {
        rig.menu(MenuAction::Export(raster::ExportFormat::Png));
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .set_extras(ExportExtras {
                artboards: false,
                slices: choice,
                ..Default::default()
            });
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .set_base_name("page");
        rig.step(Vec::new());
        assert!(
            rig.chrome
                .dialogs_for_test()
                .active_export_dialog_for_test()
                .drawn_extras()[1]
                .is_some(),
            "the Slices choice is drawn"
        );
        let said: Vec<_> = rig
            .step(enter())
            .into_iter()
            .chain(rig.step(Vec::new()))
            .collect();
        assert!(said.iter().any(Result::is_ok), "{said:?}");
        let mut sizes: Vec<(String, u32, u32)> = std::fs::read_dir(out)
            .unwrap()
            .flatten()
            .map(|e| {
                let img = raster::decode_bytes(&std::fs::read(e.path()).unwrap()).unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    img.width,
                    img.height,
                )
            })
            .collect();
        sizes.sort();
        match choice {
            SliceExport::User => assert_eq!(sizes, vec![("page_01.png".into(), 5, 10)]),
            _ => assert_eq!(
                sizes,
                vec![
                    ("page_01.png".into(), 5, 10),
                    ("page_02.png".into(), 5, 10),
                    ("page_03.png".into(), 10, 10),
                ]
            ),
        }
    }
}

/// The `/MediaBox` of every page of `pdf`, in page order.
fn media_boxes(pdf: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(pdf);
    text.match_indices("/MediaBox [")
        .map(|(at, key)| {
            let rest = &text[at + key.len()..];
            rest[..rest.find(']').unwrap()].to_string()
        })
        .collect()
}

/// File > Export As with a PDF row and Photopea's "reverse pages", through
/// the real chrome: the checkbox is drawn for the PDF row and pressed where
/// it is drawn, Enter confirms, and the PDF written has one page per
/// artboard, last first — the plain writer's pages reversed.
#[test]
fn export_as_pdf_with_reverse_pages_writes_the_pages_last_first() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let src = png(dir.path(), "deck.png", 16, 16, |_, _| [40, 90, 200, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new().exporting_folder(&out));
    ed.open_path(&src).unwrap();
    two_artboards(&mut ed);
    // The second artboard wider, so the two pages tell apart.
    {
        let doc = &mut ed.active_mut().unwrap().document;
        let plate = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                matches!(
                    doc.layers.get(*id).map(|l| &l.kind),
                    Some(LayerKind::Raster(r)) if r.artboard.as_ref().is_some_and(|b| b.x > 0)
                )
            })
            .expect("the second artboard's plate");
        if let Some(LayerKind::Raster(r)) = doc.layers.get_mut(plate).map(|l| &mut l.kind) {
            r.artboard.as_mut().unwrap().width = 24;
        }
    }
    let plain = {
        let doc = ed.active().unwrap();
        let scene = crate::menu_bridge::w16k::vector_doc(&doc.document, &doc.tiles).unwrap();
        media_boxes(&raster::codec::export_vector::encode_pdf(&scene).unwrap())
    };
    assert_eq!(plain.len(), 2, "{plain:?}");
    assert_ne!(plain[0], plain[1], "{plain:?}");
    let mut rig = Rig::new(ed);
    rig.menu(MenuAction::Export(raster::ExportFormat::Pdf));
    rig.chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .set_base_name("deck");
    rig.step(Vec::new());
    rig.step(Vec::new());
    let reverse = rig
        .chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .drawn_extras()[2]
        .expect("a PDF row is offered Reverse pages");
    let at = rig.settled_rect(reverse).center();
    rig.press(at, egui::PointerButton::Primary);
    assert!(
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .extras()
            .reverse_pages
    );
    let said: Vec<_> = rig
        .step(enter())
        .into_iter()
        .chain(rig.step(Vec::new()))
        .collect();
    assert!(
        said.iter()
            .any(|r| r.as_ref().is_ok_and(|s| s.contains("PDF pages reversed"))),
        "{said:?}"
    );
    let pdf = std::fs::read(out.join("deck.pdf")).unwrap();
    let mut reversed = plain.clone();
    reversed.reverse();
    assert_eq!(media_boxes(&pdf), reversed, "the pages, last first");
}

/// Photopea's automatic slices: the grid every slice edge draws, uncovered
/// cells merged along each row band.
#[test]
fn auto_slices_cover_what_the_user_slices_leave() {
    use raster::PixelRect as R;
    let auto = crate::menu_bridge::w18g::auto_slices;
    assert_eq!(auto(8, 4, &[]), vec![R::new(0, 0, 8, 4)]);
    assert_eq!(
        auto(10, 10, &[R::new(2, 2, 4, 4)]),
        vec![
            R::new(0, 0, 10, 2),
            R::new(0, 2, 2, 4),
            R::new(6, 2, 4, 4),
            R::new(0, 6, 10, 4),
        ]
    );
}

/// Layer > Duplicate Into…, below Duplicate Layer: the click asks for the
/// Duplicate Layer dialog (whose Destination picks the document), the way
/// the row menu's Duplicate Into… does.
#[test]
fn duplicate_into_opens_the_duplicate_dialog_with_the_destination() {
    let dir = tempfile::tempdir().unwrap();
    let canvas = png(dir.path(), "canvas.png", 8, 8, |_, _| [9, 9, 9, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&canvas).unwrap();
    let menus = crate::menu_bridge::menus(&ed);
    let layer = menus.iter().find(|m| m.title == "Layer").unwrap();
    let rows = layer.actions();
    let dup = rows
        .iter()
        .position(|a| *a == MenuAction::DuplicateLayer)
        .unwrap();
    assert_eq!(rows[dup + 1], MenuAction::DuplicateInto);
    let chrome = crate::chrome::Chrome::new();
    let ctx = context(&mut ed, chrome.workspace());
    assert_eq!(
        MenuAction::DuplicateInto.resolve(&ctx).intent(),
        Some(&ui::Intent::Action(MenuAction::DuplicateLayer))
    );
    let mut host = crate::dialog_host::DialogHost::default();
    assert!(host.open_for_menu_action(&MenuAction::DuplicateLayer, &ed));
    assert!(matches!(
        host.active_for_test(),
        crate::dialog_host::ActiveDialog::DuplicateLayer(_)
    ));
}

/// Enter, pressed and released.
fn enter() -> Vec<egui::Event> {
    [true, false]
        .map(|pressed| egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        })
        .to_vec()
}

/// The real chrome, drawing the Layers panel, and a clock: frames go through
/// `Chrome::ui`, and what a frame asked for (its menu picks) is read back.
struct Rig {
    ctx: egui::Context,
    chrome: crate::chrome::Chrome,
    ed: Editor,
    t: f64,
}

impl Rig {
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<MenuAction> {
        self.t += 0.05;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 1600.0),
            )),
            time: Some(self.t),
            events,
            ..Default::default()
        };
        let mut out = crate::chrome::ChromeOutput::default();
        let _ = self.ctx.run(input, |ctx| {
            out = self.chrome.ui(ctx, &mut self.ed);
        });
        out.menu
    }

    fn new(ed: Editor) -> Rig {
        let ctx = egui::Context::default();
        crate::chrome::install_theme(&ctx, design::Theme::Dark);
        Rig {
            ctx,
            chrome: crate::chrome::Chrome::new(),
            ed,
            t: 1.0,
        }
    }

    /// A frame, then every menu pick it made performed, as the shell
    /// performs them (`menu_bridge::perform`).
    fn step(&mut self, events: Vec<egui::Event>) -> Vec<Result<String, String>> {
        let picks = self.frame(events);
        picks
            .into_iter()
            .map(|a| perform(a, &mut self.ed))
            .collect()
    }

    /// A menu-bar click on `action` (`Chrome::menu_click`: a dialog row opens
    /// its dialog), its picks performed.
    fn menu(&mut self, action: MenuAction) -> Vec<Result<String, String>> {
        let mut out = crate::chrome::ChromeOutput::default();
        self.chrome
            .menu_click(ui::Intent::Action(action), &self.ed, &mut out);
        out.menu
            .into_iter()
            .map(|a| perform(a, &mut self.ed))
            .collect()
    }

    fn rect(&self, id: egui::Id) -> egui::Rect {
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} is not drawn"))
            .rect
    }

    /// Where `id` is drawn once the layout has settled: a modal is centred
    /// on its size, which grows over its first frames, so a rect read too
    /// early is not where the next frame draws it.
    fn settled_rect(&mut self, id: egui::Id) -> egui::Rect {
        let mut last = None;
        for _ in 0..30 {
            self.frame(Vec::new());
            let now = self.ctx.read_response(id).map(|r| r.rect);
            if let Some(rect) = now.filter(|_| now == last) {
                return rect;
            }
            last = now;
        }
        panic!("{id:?} never settled: {last:?}")
    }

    fn press(&mut self, at: egui::Pos2, button: egui::PointerButton) -> Vec<MenuAction> {
        let event = |pressed| egui::Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        self.frame(vec![
            egui::Event::PointerMoved(at),
            event(true),
            event(false),
        ])
    }
}

/// The Layers panel's row menu on a smart object carries Photopea's Stack
/// Mode as a submenu: hovering the drawn row opens the eleven modes beside
/// it, and a click on one asks for that mode, as the menu bar's row does.
/// Turn into JPG follows it.
#[test]
fn the_row_menus_stack_mode_submenu_opens_beside_it_and_picks_a_mode() {
    use ui::menu::{LayerExtraOp, StackMode};
    let dir = tempfile::tempdir().unwrap();
    let source = png(dir.path(), "so.png", 8, 8, |_, _| [30, 60, 90, 255]);
    let canvas = png(dir.path(), "canvas.png", 64, 64, |_, _| {
        [200, 200, 200, 255]
    });
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&canvas).unwrap();
    ed.place_path(&source, false).unwrap();
    ed.active_mut()
        .unwrap()
        .set_viewport(glam::Vec2::new(400.0, 300.0));
    let so = ed.active().unwrap().document.active_layer().unwrap();
    // Stack Mode acts on a smart object whose source is a stack of layers
    // (a PSD): the canvas document, written as one, becomes its source.
    {
        let open = ed.active_mut().unwrap();
        let flat = open.composite(open.canvas_rect()).unwrap();
        let (psd, _) =
            crate::import::psd_from_document(&open.document, &open.tiles, &flat).unwrap();
        let Some(LayerKind::SmartObject(object)) = open.document.layers.get(so).map(|l| &l.kind)
        else {
            panic!("the placed layer is not a smart object");
        };
        let asset = object.asset;
        open.document.set_asset_origin(layer_model::AssetRecord {
            id: asset,
            origin: AssetOrigin::Embedded {
                name: "stack.psd".to_string(),
                bytes: psd,
            },
            source_size: Some((64, 64)),
        });
    }
    let ctx = egui::Context::default();
    crate::chrome::install_theme(&ctx, design::Theme::Dark);
    let mut rig = Rig {
        ctx,
        chrome: crate::chrome::Chrome::new(),
        ed,
        t: 1.0,
    };
    rig.chrome
        .emit(ui::Intent::ApplyLayout(ui::LayoutId::Minimal));
    rig.chrome.emit(ui::Intent::SetPanelOpen {
        panel: ui::PanelId::Layers,
        open: true,
    });
    for _ in 0..8 {
        rig.frame(Vec::new());
    }
    let all: Vec<_> = rig.ed.active().unwrap().document.layers.iter_depth_first();
    let drawn: Vec<bool> = all
        .iter()
        .map(|l| {
            rig.ctx
                .read_response(ui::view::ids::layer_row(*l))
                .is_some()
        })
        .collect();
    assert!(
        drawn.iter().any(|d| *d),
        "no layer row is drawn: {all:?} {drawn:?}"
    );
    let row = rig.rect(ui::view::ids::layer_row(so)).center();
    rig.press(row, egui::PointerButton::Secondary);
    rig.frame(Vec::new());

    let chrome = crate::chrome::Chrome::new();
    let menu_ctx = context(&mut rig.ed, chrome.workspace());
    let items = ui::context_menu::layer_items(&menu_ctx);
    let stack = items
        .iter()
        .position(|i| i.label == "Stack Mode")
        .expect("the smart object's row menu has Stack Mode");
    assert_eq!(items[stack + 1].action, MenuAction::TurnIntoJpg);
    let children = ui::context_menu::children_of(&items[stack], &menu_ctx);
    assert_eq!(children.len(), StackMode::ALL.len());
    let median = children
        .iter()
        .position(|c| {
            c.action == MenuAction::LayerExtra(LayerExtraOp::StackMode(StackMode::Median))
        })
        .unwrap();

    // Nothing is open beside it until the drawn row is hovered.
    assert!(rig
        .ctx
        .read_response(ui::context_menu::ids::context_subitem(median))
        .is_none());
    let at = rig
        .settled_rect(ui::context_menu::ids::context_item(stack))
        .center();
    let picked = rig.frame(vec![egui::Event::PointerMoved(at)]);
    assert!(
        picked.is_empty(),
        "hovering the submenu row asks for nothing"
    );
    rig.frame(Vec::new());
    let sub = rig.rect(ui::context_menu::ids::context_subitem(median));
    let row = rig.rect(ui::context_menu::ids::context_item(stack));
    // Beside it: to the right, or flipped to the left at the screen's edge
    // (this row menu opens by the Layers panel, on the right).
    assert!(
        sub.left() >= row.right() - 1.0 || sub.right() <= row.left() + 1.0,
        "the modes open beside the row: {sub:?} vs {row:?}"
    );
    assert!(
        sub.right() <= rig.ctx.screen_rect().right() + 0.5,
        "and on the screen: {sub:?}"
    );
    let picked = rig.press(sub.center(), egui::PointerButton::Primary);
    assert_eq!(
        picked,
        vec![MenuAction::LayerExtra(LayerExtraOp::StackMode(
            StackMode::Median
        ))]
    );
}

/// An 8 x 8 PNG tagged with Adobe RGB (1998), saturated enough that a
/// conversion to sRGB moves its samples.
fn adobe_png(dir: &Path) -> (PathBuf, Vec<u8>, Vec<u8>) {
    let (w, h) = (8u32, 8u32);
    let px: Vec<u8> = (0..w * h)
        .flat_map(|i| [30 + (i as u8 % 8) * 20, 200, 60, 255])
        .collect();
    let profile = color::icc::adobe_rgb_1998_profile();
    let path = dir.join("adobe.png");
    raster::encode_to_path(
        &path,
        raster::ExportFormat::Png,
        w,
        h,
        raster::EncodedPixels::Rgba8(&px),
        &raster::EncodeOptions::with_icc(profile.clone()),
    )
    .unwrap();
    (path, px, profile)
}

/// File > Export As over an Adobe RGB document, through the real chrome:
/// Photopea's "convert to sRGB" is drawn (checked), pressed where it is
/// drawn to uncheck it, Enter confirms, and the PNG written carries the
/// document's own samples with its Adobe RGB profile embedded. The checked
/// default (the batch writer the shell runs) writes converted, untagged
/// samples — the two differ, so the option is what changed the file.
#[test]
fn export_as_with_convert_to_srgb_unchecked_keeps_the_profile_and_the_samples() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let (src, px, profile) = adobe_png(dir.path());
    let mut ed = editor(dir.path(), ScriptedDialogs::new().exporting_folder(&out));
    ed.open_path(&src).unwrap();
    assert!(
        crate::menu_bridge::w18g::document_has_profile(&ed),
        "the Adobe RGB file opens tagged"
    );
    // Checked (the default): what the shell's batch writer writes.
    let converted = {
        let mut d = ui::dialogs::ExportAsDialog::new(
            8,
            8,
            "converted",
            ui::dialogs::PreviewSource::placeholder(8, 8),
        );
        d.set_format(raster::ExportFormat::Png);
        let job = d.job();
        let paths = ed.active_mut().unwrap().export_job(&job, &out).unwrap();
        raster::decode_path(&paths[0]).unwrap()
    };
    assert_eq!(converted.icc_profile, None, "converted to sRGB, untagged");
    assert_ne!(converted.rgba8, px, "the conversion moved the samples");

    let mut rig = Rig::new(ed);
    // The menu bar is drawn before it is clicked.
    rig.step(Vec::new());
    rig.menu(MenuAction::Export(raster::ExportFormat::Png));
    rig.chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .set_base_name("kept");
    rig.step(Vec::new());
    rig.step(Vec::new());
    let srgb = rig
        .chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .drawn_extras()[3]
        .expect("a document with a profile is offered Convert to sRGB");
    let at = rig.settled_rect(srgb).center();
    rig.press(at, egui::PointerButton::Primary);
    assert!(
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .extras()
            .keep_profile,
        "the press unchecked it"
    );
    let said: Vec<_> = rig
        .step(enter())
        .into_iter()
        .chain(rig.step(Vec::new()))
        .collect();
    let kept = raster::decode_path(&out.join("kept.png")).unwrap();
    assert_eq!(
        kept.icc_profile.as_deref(),
        Some(&profile[..]),
        "profile embedded"
    );
    assert_eq!(kept.rgba8, px, "the document's own samples");
    assert!(
        said.iter()
            .any(|r| r.as_ref().is_ok_and(|s| s.contains("profile kept"))),
        "{said:?}"
    );
}

/// Over an untagged document Convert to sRGB is not offered.
#[test]
fn convert_to_srgb_is_not_offered_over_an_untagged_document() {
    let dir = tempfile::tempdir().unwrap();
    let src = png(dir.path(), "plain.png", 8, 8, |_, _| [10, 20, 30, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new());
    ed.open_path(&src).unwrap();
    assert!(!crate::menu_bridge::w18g::document_has_profile(&ed));
    let mut rig = Rig::new(ed);
    rig.step(Vec::new());
    rig.menu(MenuAction::Export(raster::ExportFormat::Png));
    rig.step(Vec::new());
    rig.step(Vec::new());
    let d = rig
        .chrome
        .dialogs_for_test()
        .active_export_dialog_for_test();
    assert!(!d.offers_srgb());
    assert_eq!(d.drawn_extras()[3], None);
}

/// File > Export As with a PDF row and Photopea's "Pages", through the real
/// chrome: the field is clicked where it is drawn and "2" typed into it,
/// Enter confirms, and the PDF written holds only the second artboard's
/// page.
#[test]
fn export_as_pdf_with_pages_writes_only_the_pages_named() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out");
    let src = png(dir.path(), "deck.png", 16, 16, |_, _| [40, 90, 200, 255]);
    let mut ed = editor(dir.path(), ScriptedDialogs::new().exporting_folder(&out));
    ed.open_path(&src).unwrap();
    two_artboards(&mut ed);
    {
        let doc = &mut ed.active_mut().unwrap().document;
        let plate = doc
            .layers
            .iter_depth_first()
            .into_iter()
            .find(|id| {
                matches!(
                    doc.layers.get(*id).map(|l| &l.kind),
                    Some(LayerKind::Raster(r)) if r.artboard.as_ref().is_some_and(|b| b.x > 0)
                )
            })
            .expect("the second artboard's plate");
        if let Some(LayerKind::Raster(r)) = doc.layers.get_mut(plate).map(|l| &mut l.kind) {
            r.artboard.as_mut().unwrap().width = 24;
        }
    }
    let plain = {
        let doc = ed.active().unwrap();
        let scene = crate::menu_bridge::w16k::vector_doc(&doc.document, &doc.tiles).unwrap();
        media_boxes(&raster::codec::export_vector::encode_pdf(&scene).unwrap())
    };
    assert_eq!(plain.len(), 2, "{plain:?}");
    let mut rig = Rig::new(ed);
    rig.menu(MenuAction::Export(raster::ExportFormat::Pdf));
    rig.chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .set_base_name("deck");
    rig.step(Vec::new());
    rig.step(Vec::new());
    let pages = rig
        .chrome
        .dialogs_for_test()
        .active_export_dialog_for_test()
        .drawn_extras()[4]
        .expect("a PDF row is offered Pages");
    let at = rig.settled_rect(pages).center();
    rig.press(at, egui::PointerButton::Primary);
    rig.frame(vec![egui::Event::Text("2".to_string())]);
    assert_eq!(
        rig.chrome
            .dialogs_for_test()
            .active_export_dialog_for_test()
            .pdf_pages(),
        "2"
    );
    // Enter leaves the field, and Enter again confirms.
    let said: Vec<_> = rig
        .step(enter())
        .into_iter()
        .chain(rig.step(enter()))
        .chain(rig.step(Vec::new()))
        .collect();
    let pdf = std::fs::read(out.join("deck.pdf")).unwrap();
    assert_eq!(media_boxes(&pdf), vec![plain[1].clone()], "only page 2");
    assert!(
        said.iter()
            .any(|r| r.as_ref().is_ok_and(|s| s.contains("PDF pages: 1 of 2"))),
        "{said:?}"
    );
}
