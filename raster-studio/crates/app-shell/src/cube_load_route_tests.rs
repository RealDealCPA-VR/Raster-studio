//! W5-B: Color Lookup's "Load .cube file", driven through the real chrome
//! and dialog host — the click on the dialog's Load button, the host's file
//! pick (answered by [`crate::dialog_host::PICKED_CUBE_FOR_TEST`] in place of
//! the person at the file dialog) and the read — and what the dialog then
//! shows.
//!
//! A child of `editor::actions_library` (declared there with `#[path]`).

use std::path::{Path, PathBuf};

use crate::chrome::Chrome;
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
use crate::menu_bridge::{context, resolve_intent};
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use ui::menu::{AdjustmentId, MenuAction};

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

fn opened(dir: &Path) -> Editor {
    let mut ed = Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    );
    let rgba: Vec<u8> = (0..32 * 24u32)
        .flat_map(|i| [(i % 251) as u8, (i / 5 % 256) as u8, 90, 255])
        .collect();
    let path = dir.join("a.png");
    std::fs::write(
        &path,
        raster::encode(raster::ExportFormat::Png, 32, 24, &rgba).unwrap(),
    )
    .unwrap();
    ed.open_path(&path).unwrap();
    ed
}

/// Open Image > Adjustments > Color Lookup, click its Load button with the
/// file dialog answering `picked`, and return every string the dialog then
/// painted, with the chrome (still open on the dialog).
fn load_through_the_dialog(ed: &mut Editor, picked: PathBuf) -> (Chrome, Vec<String>) {
    let mut chrome = Chrome::new();
    let menu_ctx = context(ed, chrome.workspace());
    let intent = resolve_intent(
        MenuAction::ApplyAdjustment(AdjustmentId::ColorLookup),
        &menu_ctx,
        ed,
    )
    .expect("enabled");
    let mut out = crate::chrome::ChromeOutput::default();
    chrome.menu_click(intent, ed, &mut out);
    assert!(chrome.dialog_open(), "Color Lookup opened no dialog");

    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    // A window settles its size and place over its first frames.
    for _ in 0..3 {
        let _ = ctx.run(raw_input(Vec::new()), |ctx| {
            let _ = chrome.ui(ctx, ed);
        });
    }
    let at = ctx
        .read_response(ui::dialogs::adjustment_dialog::lut_load_button_id())
        .expect("the Load button was drawn")
        .rect
        .center();

    crate::dialog_host::PICKED_CUBE_FOR_TEST.with(|p| *p.borrow_mut() = Some(picked));
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    for events in [
        vec![egui::Event::PointerMoved(at)],
        vec![button(true)],
        vec![button(false)],
    ] {
        let _ = ctx.run(raw_input(events), |ctx| {
            let _ = chrome.ui(ctx, ed);
        });
    }
    let asked = crate::dialog_host::PICKED_CUBE_FOR_TEST.with(|p| p.borrow_mut().take());
    assert!(asked.is_none(), "the click never asked the host for a file");

    let full = ctx.run(raw_input(Vec::new()), |ctx| {
        let _ = chrome.ui(ctx, ed);
    });
    let texts = full
        .shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_string()),
            _ => None,
        })
        .collect();
    assert!(chrome.dialog_open(), "the load closed the dialog");
    (chrome, texts)
}

/// W5-B (P1): the Load button read the chosen file whole, on the interaction
/// thread, whatever its size. A file over the cap is refused from its
/// metadata — the dialog says so, naming the size only the metadata knows —
/// and the table is left as it was.
#[test]
fn color_lookup_refuses_an_oversized_cube_file_from_its_load_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path());
    let huge = dir.path().join("huge.cube");
    // Sparse: no byte of it has to exist for the metadata to say the size.
    let size = adjustments::extended::MAX_CUBE_FILE_BYTES * 2;
    std::fs::File::create(&huge).unwrap().set_len(size).unwrap();

    let (mut chrome, texts) = load_through_the_dialog(&mut ed, huge);
    let refusal = format!("the file is {size} bytes");
    assert!(
        texts.iter().any(|t| t.contains(&refusal)),
        "the dialog did not show the refusal {refusal:?}: {texts:?}"
    );
    assert!(chrome
        .dialogs_for_test()
        .active_adjustment_dialog_for_test()
        .invocation()
        .is_identity());
}

/// The control: the same route takes a real cube, so the refusal above is
/// the size check and not a route that never loads anything.
#[test]
fn color_lookup_loads_a_real_cube_file_from_its_load_button() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = opened(dir.path());
    let mut cube = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                cube.push_str(&format!("{} {} {}\n", 1 - r, 1 - g, 1 - b));
            }
        }
    }
    let path = dir.path().join("invert.cube");
    std::fs::write(&path, cube).unwrap();

    let (mut chrome, texts) = load_through_the_dialog(&mut ed, path);
    assert!(
        !texts.iter().any(|t| t.contains("bytes")),
        "a real cube was refused: {texts:?}"
    );
    assert!(!chrome
        .dialogs_for_test()
        .active_adjustment_dialog_for_test()
        .invocation()
        .is_identity());
}
