//! W18-I: the library exports and the swatch folders, driven through the
//! application's own chrome: the panel is docked, its menu is opened and
//! its rows clicked with real pointer events, and what the export wrote is
//! read back by the readers File ▸ Open uses.

use std::path::Path;

use layer_model::LayerEffects;
use ui::dock::PanelId;
use ui::panels::panel_menus_w16::{self as menus, ids};
use ui::Intent;

use crate::chrome::{install_theme, Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::editor::Editor;
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

/// The headless window: the chrome over the editor, each frame's commands
/// and menu picks applied the way the shell applies them.
struct Window {
    ctx: egui::Context,
    chrome: Chrome,
    time: f64,
}

impl Window {
    fn new(panel: PanelId) -> Self {
        let _ = menus::take_requests();
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let w = chrome.workspace_for_test();
        w.dock.apply_layout(ui::dock::LayoutId::Minimal);
        if !w.dock.is_open(panel) {
            w.absorb(&Intent::SetPanelOpen { panel, open: true });
        }
        w.dock.raise(panel);
        Self {
            ctx,
            chrome,
            time: 0.0,
        }
    }

    fn frame(&mut self, ed: &mut Editor, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            time: Some(self.time),
            ..Default::default()
        };
        self.time += 1.0;
        let mut out = ChromeOutput::default();
        let chrome = &mut self.chrome;
        let _ = self.ctx.run(input, |ctx| out = chrome.ui(ctx, ed));
        for command in std::mem::take(&mut out.commands) {
            ed.apply_command(command);
        }
        for action in out.menu {
            let _ = crate::menu_bridge::perform(action, ed);
        }
    }

    fn settle(&mut self, ed: &mut Editor) {
        for _ in 0..3 {
            self.frame(ed, Vec::new());
        }
    }

    fn click(&mut self, ed: &mut Editor, id: egui::Id) {
        self.settle(ed);
        let at = self
            .ctx
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
        self.frame(
            ed,
            vec![egui::Event::PointerMoved(at), press(true), press(false)],
        );
        // The request a click posted is taken on the next frame.
        self.frame(ed, Vec::new());
    }

    fn menu(&mut self, ed: &mut Editor, panel: PanelId, row: &str) {
        self.click(ed, ui::view::ids::panel_menu(panel));
        self.click(ed, ids::menu_row(panel, row));
    }
}

/// Brushes ▸ Export as .ABR names each brush and carries its dynamics: the
/// file read back gives the panel's names, diameters, spacing and jitters,
/// not just the tips.
#[test]
fn brushes_export_as_abr_writes_names_and_dynamics() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mine.abr");
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().saving_to(&out));
    let mut win = Window::new(PanelId::Brushes);
    let mut settings = tools::BrushSettings {
        size: 24.0,
        spacing: 0.4,
        hardness: 0.5,
        ..Default::default()
    };
    settings.dynamics.size_jitter = 0.35;
    settings.dynamics.scatter = 2.0;
    settings.dynamics.count = 3;
    settings.dynamics.opacity_jitter = 0.5;
    win.chrome.workspace_for_test().brushes.restore([
        ui::panels::brushes::BrushPreset {
            name: "Scatter Soft".to_string(),
            settings,
        },
        ui::panels::brushes::BrushPreset {
            name: "Plain".to_string(),
            settings: tools::BrushSettings::default(),
        },
    ]);
    win.menu(&mut ed, PanelId::Brushes, "export");
    assert!(
        out.exists(),
        "nothing was written; status {:?}",
        ed.status()
    );

    let bytes = std::fs::read(&out).unwrap();
    let read = asset_store::abr::parse_abr_presets(&bytes).unwrap();
    let names: Vec<&str> = read.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Scatter Soft", "Plain"]);
    let soft = &read[0];
    assert!((soft.diameter - 24.0).abs() < 1e-3, "{soft:?}");
    assert!((soft.spacing - 0.4).abs() < 1e-3, "{soft:?}");
    assert!((soft.hardness - 0.5).abs() < 1e-3, "{soft:?}");
    assert!((soft.dynamics.size_jitter - 0.35).abs() < 1e-3, "{soft:?}");
    assert!((soft.dynamics.scatter - 2.0).abs() < 1e-3, "{soft:?}");
    assert_eq!(soft.dynamics.count, 3);
    assert!(
        (soft.dynamics.opacity_jitter - 0.5).abs() < 1e-3,
        "{soft:?}"
    );
    // The tips alone still read as they did, for any tips-only reader.
    assert_eq!(asset_store::abr::parse_abr(&bytes).unwrap().len(), 2);
}

/// Styles ▸ Export as .ASL writes the pattern a Pattern Overlay names into
/// the library and a Blending Options object into the style: the importer
/// File ▸ Open uses gets the overlay back with its pixels.
#[test]
fn styles_export_as_asl_writes_patterns_and_blending_options() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mine.asl");
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().saving_to(&out));
    let pixels = vec![
        255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255,
    ];
    let tile = layer_model::effects::PatternTile::new("Checks", 2, 2, pixels.clone()).unwrap();
    let styled = LayerEffects {
        pattern_overlay: Some(layer_model::effects::PatternOverlayEffect {
            pattern: layer_model::effects::PatternFill {
                tile: Some(tile),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };
    ed.presets_mut()
        .define_style("Checked", serde_json::to_string(&styled).unwrap());
    let mut win = Window::new(PanelId::Styles);
    win.menu(&mut ed, PanelId::Styles, "export");
    assert!(
        out.exists(),
        "nothing was written; status {:?}",
        ed.status()
    );

    let bytes = std::fs::read(&out).unwrap();
    let back = crate::menu_bridge::asl_import::styles_from_asl(&bytes).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].name, "Checked");
    let overlay = back[0]
        .effects
        .pattern_overlay
        .as_ref()
        .unwrap_or_else(|| panic!("no pattern overlay; unmapped {:?}", back[0].unmapped));
    let tile = overlay.pattern.tile.as_ref().expect("the pattern's pixels");
    assert_eq!((tile.width(), tile.height()), (2, 2));
    assert_eq!(tile.rgba8(), pixels.as_slice());

    let library = asset_store::asl::parse_asl(&bytes).unwrap();
    assert!(
        !library.patterns.is_empty(),
        "the library carries the pattern"
    );
    let mut cur = psd::bytes::Cursor::new(&library.styles[0].style_descriptor);
    assert_eq!(cur.u32().unwrap(), 16);
    let styl = psd::Descriptor::read(&mut cur, &psd::ReadOptions::default()).unwrap();
    let blend = styl.descriptor("blendOptions").expect("blending options");
    assert_eq!(blend.number("Opct"), Some(100.0));
    assert_eq!(blend.number("fillOpacity"), Some(100.0));
}

/// The Swatches list's folders outlive the session: a folder made from the
/// Swatches menu (and renamed, and holding a swatch) is written beside the
/// presets file, and a fresh window over the same configuration lists it.
#[test]
fn swatch_folders_are_kept_across_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new());
    let file = super::swatch_folders_file(&ed);
    let mut win = Window::new(PanelId::Swatches);
    win.menu(&mut ed, PanelId::Swatches, "new-folder");
    let mut folders = menus::swatch_folders(&win.ctx);
    assert_eq!(folders.len(), 1, "New Folder made a folder");
    folders[0].name = "Brand".to_string();
    // A swatch of the palette (a member the palette lacks is pruned).
    let first = win.chrome.workspace().swatches.swatches()[0].rgba;
    folders[0].members.push(menus::swatch_key(first));
    menus::set_swatch_folders(&win.ctx, folders.clone());
    win.settle(&mut ed);
    assert!(file.exists(), "the folders were written");

    let mut later = Window::new(PanelId::Swatches);
    assert!(menus::swatch_folders(&later.ctx).is_empty());
    later.settle(&mut ed);
    assert_eq!(menus::swatch_folders(&later.ctx), folders);

    // Deleting every folder is kept too.
    menus::set_swatch_folders(&later.ctx, Vec::new());
    later.settle(&mut ed);
    let again = Window::new(PanelId::Swatches);
    let mut again = again;
    again.settle(&mut ed);
    assert!(menus::swatch_folders(&again.ctx).is_empty());
}
