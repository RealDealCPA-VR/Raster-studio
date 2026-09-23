//! W7-B: a defined pattern reaches the canvas through the Layer Style
//! dialog, and survives a save and a reopen.
//!
//! The route is the product's: the preset store Edit > Define Pattern fills,
//! the dialog the Layer ▸ Layer Style ▸ Pattern Overlay… row opens (through
//! the real [`DialogHost`]), the Enter that confirms it, the command the
//! editor applies, the document's own composite, and a `.rstudio` package
//! written and read back.

use crate::chrome::ChromeOutput;
use crate::dialog_host::DialogHost;
use crate::dialogs::ScriptedDialogs;
use crate::doc::{DocumentId, OpenDocument};
use crate::editor::Editor;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;
use ui::dialogs::EffectKind;

/// A 3x2 pattern of six distinct opaque sRGB colours.
const SIX: [[u8; 4]; 6] = [
    [230, 20, 20, 255],
    [20, 200, 40, 255],
    [30, 40, 220, 255],
    [240, 220, 30, 255],
    [20, 210, 220, 255],
    [200, 30, 210, 255],
];

fn six_at(x: usize, y: usize) -> [u8; 4] {
    SIX[(y % 2) * 3 + (x % 3)]
}

fn editor(dir: &std::path::Path) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(ScriptedDialogs::new()),
    )
}

#[track_caller]
fn assert_tiled(rgba: &[u8], width: usize, height: usize, what: &str) {
    for y in 0..height {
        for x in 0..width {
            let i = (y * width + x) * 4;
            let got = &rgba[i..i + 4];
            let want = six_at(x, y);
            for c in 0..4 {
                assert!(
                    got[c].abs_diff(want[c]) <= 1,
                    "{what}: ({x},{y}) is {got:?}, the pattern says {want:?}"
                );
            }
        }
    }
}

#[test]
fn a_defined_pattern_chosen_in_layer_style_composites_and_survives_save_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let png = dir.path().join("white.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, 12, 8, &[255u8; 12 * 8 * 4]).unwrap(),
    )
    .unwrap();
    let mut ed = editor(&dir.path().join("config"));
    ed.open_path(&png).unwrap();
    ed.presets_mut()
        .define_pattern(asset_store::presets::PatternPreset {
            name: "Six".into(),
            width: 3,
            height: 2,
            rgba8: SIX.concat(),
        });

    // Before: the white layer composites white.
    let region = ed.active().unwrap().canvas_rect();
    let before = ed.active_mut().unwrap().composite(region).unwrap();
    assert!(before.iter().all(|b| *b == 255), "the fixture starts white");

    // Layer ▸ Layer Style ▸ Pattern Overlay… opens the dialog with the
    // defined pattern on offer.
    let mut host = DialogHost::default();
    assert!(host.open_for_menu_action(
        &ui::menu::MenuAction::LayerStyle(ui::menu::EffectSlot::PatternOverlay),
        &ed
    ));
    {
        let dialog = host.active_layer_style_dialog_for_test();
        assert_eq!(
            dialog
                .patterns()
                .iter()
                .map(|p| p.name())
                .collect::<Vec<_>>(),
            ["Six"],
            "the host hands the preset store's patterns to the dialog"
        );
        dialog.set_enabled(EffectKind::PatternOverlay, true);
        dialog.select(EffectKind::PatternOverlay);
        assert!(dialog.set_overlay_pattern(0));
    }
    // A settle frame, then Enter confirms: one command rides out.
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let input = |events: Vec<egui::Event>| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ChromeOutput::default();
    let _ = ctx.run(input(Vec::new()), |ctx| host.ui(ctx, None, &mut out));
    let enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    };
    let _ = ctx.run(input(vec![enter]), |ctx| host.ui(ctx, None, &mut out));
    assert!(!host.is_open(), "Enter confirmed the dialog");
    assert_eq!(out.commands.len(), 1, "{:?}", out.commands);
    for command in out.commands {
        ed.apply_command(command);
    }

    // The canvas shows the pattern, tiled from the layer's corner.
    let after = ed.active_mut().unwrap().composite(region).unwrap();
    assert_tiled(&after, 12, 8, "after Apply Style");

    // Save, reopen: the pattern is in the package, not only in this session.
    let project = dir.path().join("styled.rstudio");
    ed.active_mut()
        .unwrap()
        .save_to(&project, "test")
        .expect("the save succeeds");
    let mut reopened = OpenDocument::open_project(DocumentId(4242), &project, 50).unwrap();
    let layer = reopened.document.active_layer().unwrap();
    let tile = reopened
        .document
        .layers
        .get(layer)
        .unwrap()
        .effects
        .pattern_overlay
        .as_ref()
        .and_then(|o| o.pattern.tile.clone())
        .expect("the reopened document still carries the pattern");
    assert_eq!(tile.rgba8(), SIX.concat().as_slice());
    let region = reopened.canvas_rect();
    let reread = reopened.composite(region).unwrap();
    assert_tiled(&reread, 12, 8, "after save and reopen");
    assert_eq!(reread, after, "the reopened composite is the saved one");
}
