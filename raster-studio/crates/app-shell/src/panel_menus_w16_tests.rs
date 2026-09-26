//! W16-E: the panel menus driven through the application's own chrome: the
//! panel is docked, its header's overflow button and menu rows are clicked
//! with real pointer events, and each frame's output is applied the way the
//! shell applies it. What an export writes is read back by the importer File
//! > Open uses.

use std::path::Path;

use editor_core::{Command, Selection};
use glam::IVec2;
use layer_model::LayerEffects;
use ui::dock::PanelId;
use ui::menu::MenuAction;
use ui::panels::panel_menus_w16::{ids, Library};
use ui::Intent;

use super::*;
use crate::chrome::{install_theme, Chrome, ChromeOutput};
use crate::dialogs::ScriptedDialogs;
use crate::prefs::{AppPaths, Preferences};
use crate::recent::RecentFiles;

const W: u32 = 40;
const H: u32 = 30;

fn editor_with(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    Editor::with_state(
        AppPaths::rooted(dir.join("config")),
        Preferences::default(),
        RecentFiles::new(),
        Box::new(dialogs),
    )
}

/// A document whose left half is white and right half black, opaque.
fn half_white(dir: &Path, dialogs: ScriptedDialogs) -> Editor {
    let mut rgba = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..H {
        for x in 0..W {
            let v = if x < W / 2 { 255 } else { 0 };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let png = dir.join("half.png");
    std::fs::write(
        &png,
        raster::encode(raster::ExportFormat::Png, W, H, &rgba).unwrap(),
    )
    .unwrap();
    let mut ed = editor_with(dir, dialogs);
    ed.open_path(&png).unwrap();
    ed
}

/// The headless window: the chrome over the editor, each frame's commands
/// and menu picks applied the way the shell applies them.
struct Window {
    ctx: egui::Context,
    chrome: Chrome,
    /// Seconds, a second a frame: two clicks on one spot are two clicks,
    /// not a double-click.
    time: f64,
}

impl Window {
    fn new(panel: PanelId) -> Self {
        // The request queue is per thread; start from an empty one.
        let _ = ui::panels::panel_menus_w16::take_requests();
        let ctx = egui::Context::default();
        install_theme(&ctx, design::Theme::Dark);
        let mut chrome = Chrome::new();
        let w = chrome.workspace_for_test();
        // Minimal, so the one panel under test has the column to itself and
        // every row it grows stays on screen.
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

    fn frame_with(
        &mut self,
        ed: &mut Editor,
        events: Vec<egui::Event>,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        self.frame_held(ed, events, egui::Modifiers::default())
    }

    /// A frame with `modifiers` held (egui reads the held keys from the
    /// frame's input, not from the pointer event).
    fn frame_held(
        &mut self,
        ed: &mut Editor,
        events: Vec<egui::Event>,
        modifiers: egui::Modifiers,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            events,
            modifiers,
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
        out.menu
            .into_iter()
            .map(|a| (a, crate::menu_bridge::perform(a, ed)))
            .collect()
    }

    fn settle(&mut self, ed: &mut Editor) {
        for _ in 0..3 {
            self.frame_with(ed, Vec::new());
        }
    }

    fn rect(&mut self, ed: &mut Editor, id: egui::Id) -> egui::Rect {
        self.settle(ed);
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect
    }

    fn click_at(
        &mut self,
        ed: &mut Editor,
        at: egui::Pos2,
        modifiers: egui::Modifiers,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        let mut done = self.frame_held(
            ed,
            vec![egui::Event::PointerMoved(at), press(true), press(false)],
            modifiers,
        );
        // The request a click posted is taken on the next frame.
        done.extend(self.frame_with(ed, Vec::new()));
        done
    }

    fn click(
        &mut self,
        ed: &mut Editor,
        id: egui::Id,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        let at = self.rect(ed, id).center();
        self.click_at(ed, at, egui::Modifiers::default())
    }

    fn menu(
        &mut self,
        ed: &mut Editor,
        panel: PanelId,
        row: &str,
    ) -> Vec<(MenuAction, Result<String, String>)> {
        let _ = self.click(ed, ui::view::ids::panel_menu(panel));
        // The Channels menu's rows keep the W13X-4 ids.
        let id = if panel == PanelId::Channels {
            ui::panels::channels::spot_ids::menu_row(match row {
                "new" => "new",
                _ => "delete",
            })
        } else {
            ids::menu_row(panel, row)
        };
        self.click(ed, id)
    }

    fn type_and_enter(&mut self, ed: &mut Editor, text: &str) {
        let select_all = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        self.frame_with(ed, vec![select_all, egui::Event::Text(text.to_string())]);
        self.frame_with(
            ed,
            vec![egui::Event::Key {
                key: egui::Key::Enter,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
        );
        self.settle(ed);
    }
}

// ---------------------------------------------------------------------------
// Preset menus: Export as .ACO / .ABR / .ASL, read back by File > Open
// ---------------------------------------------------------------------------

/// Swatches > Export as .ACO writes every swatch the panel shows, and the
/// `.aco` File > Open reads gives the same names and colours back.
#[test]
fn swatches_export_as_aco_reads_back_through_the_aco_importer() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mine.aco");
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().saving_to(&out));
    let mut win = Window::new(PanelId::Swatches);
    let _ = win.menu(&mut ed, PanelId::Swatches, "export");
    assert!(
        out.exists(),
        "nothing was written; status {:?}",
        ed.status()
    );
    let shown: Vec<(String, [f32; 4])> = win
        .chrome
        .workspace()
        .swatches
        .swatches()
        .iter()
        .map(|s| (s.name.clone(), s.rgba))
        .collect();
    assert!(!shown.is_empty());
    let asset_store::resources::Resource::Swatches(loaded) =
        asset_store::resources::load(&out).unwrap()
    else {
        panic!("not a swatch file");
    };
    assert_eq!(loaded.items.len(), shown.len());
    for (item, (name, rgba)) in loaded.items.iter().zip(&shown) {
        assert_eq!(&item.name, name);
        let byte = |v: f32| (v * 255.0).round() as i32;
        for (got, want) in item.rgba.iter().zip(rgba).take(3) {
            assert_eq!(byte(*got), byte(*want), "{name}");
        }
    }
}

/// Brushes > Export as .ABR writes one sampled tip per brush, and File >
/// Open of that `.abr` adds exactly those tips to the Brushes panel.
#[test]
fn brushes_export_as_abr_reads_back_through_the_abr_importer() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mine.abr");
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().saving_to(&out));
    let mut win = Window::new(PanelId::Brushes);
    let presets: Vec<tools::BrushSettings> = win
        .chrome
        .workspace()
        .brushes
        .presets()
        .iter()
        .map(|p| p.settings)
        .collect();
    let _ = win.menu(&mut ed, PanelId::Brushes, "export");
    assert!(
        out.exists(),
        "nothing was written; status {:?}",
        ed.status()
    );
    let read = asset_store::abr::parse_abr(&std::fs::read(&out).unwrap()).unwrap();
    let expected: Vec<_> = presets.iter().filter_map(tip_plane).collect();
    assert_eq!(read.len(), presets.len());
    assert_eq!(read, expected, "the tips read back are the tips written");

    // And through File > Open: every tip becomes a brush of the panel.
    let mut other = editor_with(dir.path(), ScriptedDialogs::new().opening(&out));
    let before = tools::brush::library_len();
    other.dispatch(crate::action::Action::Open).unwrap();
    assert_eq!(tools::brush::library_len() - before, presets.len());
}

/// Styles > Export as .ASL writes the style presets, and the `.asl` reader
/// File > Open uses gives the same names and effects back.
#[test]
fn styles_export_as_asl_reads_back_through_the_asl_importer() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("mine.asl");
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().saving_to(&out));
    let red = LayerEffects {
        color_overlay: Some(layer_model::effects::ColorOverlayEffect {
            color: [1.0, 0.0, 0.0, 1.0],
            ..Default::default()
        }),
        ..Default::default()
    };
    ed.presets_mut()
        .define_style("Red", serde_json::to_string(&red).unwrap());
    let mut win = Window::new(PanelId::Styles);
    let _ = win.menu(&mut ed, PanelId::Styles, "export");
    assert!(
        out.exists(),
        "nothing was written; status {:?}",
        ed.status()
    );
    let back =
        crate::menu_bridge::asl_import::styles_from_asl(&std::fs::read(&out).unwrap()).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].name, "Red");
    let overlay = back[0].effects.color_overlay.as_ref().expect("the overlay");
    assert_eq!(overlay.color, [1.0, 0.0, 0.0, 1.0]);
}

/// Swatches > Open .ACO… runs File > Open, and the swatches of the file it
/// picks join the Swatches panel.
#[test]
fn swatches_open_aco_adds_the_files_swatches_to_the_panel() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("brand.aco");
    let written = vec![
        ("Brand Teal".to_string(), [0.0, 0.5019608, 0.5019608, 1.0]),
        ("Brand Sand".to_string(), [0.8, 0.7019608, 0.5019608, 1.0]),
    ];
    std::fs::write(&file, asset_store::resources::aco::write(&written)).unwrap();
    let mut ed = editor_with(dir.path(), ScriptedDialogs::new().opening(&file));
    let mut win = Window::new(PanelId::Swatches);
    let before = win.chrome.workspace().swatches.len();
    let _ = win.menu(&mut ed, PanelId::Swatches, "open");
    win.settle(&mut ed);
    let swatches = win.chrome.workspace().swatches.swatches().to_vec();
    assert_eq!(swatches.len(), before + 2, "status {:?}", ed.status());
    let names: Vec<&str> = swatches[before..].iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["Brand Teal", "Brand Sand"]);
}

/// Styles > Name Change and Delete act on the style last clicked, in the
/// preset store (and so the preferences file).
#[test]
fn a_style_is_renamed_and_deleted_from_the_styles_menu() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    let json = serde_json::to_string(&LayerEffects::default()).unwrap();
    ed.presets_mut().define_style("One", json.clone());
    ed.presets_mut().define_style("Two", json);
    let mut win = Window::new(PanelId::Styles);
    let _ = win.click(&mut ed, ui::panels::styles::ids::tile(1));
    let _ = win.menu(&mut ed, PanelId::Styles, "rename");
    let field = win.rect(&mut ed, ids::rename_field(Library::Styles));
    let _ = win.click_at(&mut ed, field.center(), egui::Modifiers::default());
    win.type_and_enter(&mut ed, "Second");
    let names: Vec<&str> = ed
        .presets()
        .styles()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["One", "Second"], "status {:?}", ed.status());

    let _ = win.click(&mut ed, ui::panels::styles::ids::tile(0));
    let _ = win.menu(&mut ed, PanelId::Styles, "delete");
    let names: Vec<&str> = ed
        .presets()
        .styles()
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["Second"]);
    let stored = asset_store::presets::PresetStore::load(&ed.paths().presets_file());
    assert_eq!(
        stored.styles().len(),
        1,
        "the delete reached the presets file"
    );
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

fn ctrl() -> egui::Modifiers {
    egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    }
}

fn coverage_at(sel: &Selection, x: i32, y: i32) -> f32 {
    sel.coverage_at(IVec2::new(x, y))
}

/// A Ctrl+click on the composite channel row loads the composite's
/// luminosity as the selection: the white half selected, the black half not.
#[test]
fn ctrl_click_on_a_channel_row_loads_it_as_the_selection() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    let mut win = Window::new(PanelId::Channels);
    let eye = win.rect(&mut ed, ui::view::ids::channel_eye(0));
    // The row, right of the eye and the thumbnail: the channel's name.
    let at = egui::pos2(eye.right() + eye.width() * 4.0, eye.center().y);
    let depth = ed.active().unwrap().history_depth();
    let _ = win.click_at(&mut ed, at, ctrl());
    let sel = ed.active().unwrap().document.selection.clone();
    assert!(
        !sel.is_none(),
        "no selection was loaded; status {:?}",
        ed.status()
    );
    assert_eq!(coverage_at(&sel, 2, 2), 1.0, "white is selected");
    assert_eq!(coverage_at(&sel, W as i32 - 2, 2), 0.0, "black is not");
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1, "one step");

    // The red component row (index 1) loads its own values the same way.
    ed.apply_command(Command::SetSelection {
        selection: Selection::None,
    });
    let red = win.rect(&mut ed, ui::view::ids::channel_eye(1));
    let at = egui::pos2(red.right() + red.width() * 4.0, red.center().y);
    let _ = win.click_at(&mut ed, at, ctrl());
    let sel = ed.active().unwrap().document.selection.clone();
    assert_eq!(coverage_at(&sel, 2, 2), 1.0);
    assert_eq!(coverage_at(&sel, W as i32 - 2, 2), 0.0);
}

/// The Channels footer: New adds an empty alpha channel, and a spot channel
/// picked in the list is loaded as the selection by Load and removed by
/// Delete (one undo step each).
#[test]
fn the_channels_footer_news_loads_and_deletes_the_current_channel() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    let mut win = Window::new(PanelId::Channels);
    let _ = win.click(&mut ed, ui::dock::ids::channel_action("new"));
    let doc = &ed.active().unwrap().document;
    assert_eq!(doc.saved_selections.len(), 1, "status {:?}", ed.status());
    assert_eq!(doc.saved_selections[0].0, "Alpha 1");
    assert!(doc.saved_selections[0].1.bounds().is_none(), "all black");

    // A spot channel covering the top-left corner.
    let spot = {
        let doc = &mut ed.active_mut().unwrap().document;
        doc.selection = Selection::Rect {
            min: IVec2::ZERO,
            max: IVec2::new(4, 4),
        };
        let c = editor_core::spot::new_spot_channel(doc, "Gold", [200, 150, 20], 50);
        doc.selection = Selection::None;
        c
    };
    ed.apply_command(spot);
    let _ = win.click(&mut ed, ids::spot_row(0));
    let _ = win.click(&mut ed, ui::dock::ids::channel_action("load"));
    let sel = ed.active().unwrap().document.selection.clone();
    assert_eq!(coverage_at(&sel, 1, 1), 1.0, "the spot's coverage loaded");
    assert_eq!(coverage_at(&sel, 10, 10), 0.0);

    let depth = ed.active().unwrap().history_depth();
    let _ = win.click(&mut ed, ids::spot_row(0));
    let _ = win.click(&mut ed, ui::dock::ids::channel_action("delete"));
    assert!(ed.active().unwrap().document.spot_channels.is_empty());
    assert_eq!(ed.active().unwrap().history_depth(), depth + 1);
}

/// The Channels panel menu's New and Delete: New adds an alpha channel;
/// with it open for editing (its eye), Delete removes it.
#[test]
fn the_channels_menu_news_and_deletes_an_alpha_channel() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    let mut win = Window::new(PanelId::Channels);
    let _ = win.menu(&mut ed, PanelId::Channels, "new");
    let _ = win.menu(&mut ed, PanelId::Channels, "new");
    let names: Vec<String> = ed
        .active()
        .unwrap()
        .document
        .saved_selections
        .iter()
        .map(|(n, _)| n.clone())
        .collect();
    assert_eq!(names, ["Alpha 1", "Alpha 2"]);
    let _ = win.click(&mut ed, ui::panels::channels::alpha_eye_id(0));
    assert!(
        ed.active().unwrap().document.extras.alpha_edit.is_some(),
        "the eye opened it"
    );
    let _ = win.menu(&mut ed, PanelId::Channels, "delete");
    let doc = &ed.active().unwrap().document;
    let names: Vec<&str> = doc
        .saved_selections
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    assert_eq!(names, ["Alpha 2"], "status {:?}", ed.status());
    assert!(doc.extras.alpha_edit.is_none());
}

// ---------------------------------------------------------------------------
// History, Navigator
// ---------------------------------------------------------------------------

/// History > Clear History forgets every step and keeps the document.
#[test]
fn clear_history_from_the_history_menu_drops_every_step() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    ed.dispatch(crate::action::Action::NewLayer).unwrap();
    ed.dispatch(crate::action::Action::NewLayer).unwrap();
    let layers = ed.active().unwrap().document.layers.len();
    assert!(ed.active().unwrap().history_depth() >= 2);
    let mut win = Window::new(PanelId::History);
    let _ = win.menu(&mut ed, PanelId::History, "clear");
    let open = ed.active().unwrap();
    assert_eq!(open.history.undo_depth() + open.history.redo_depth(), 0);
    assert_eq!(open.document.layers.len(), layers, "the document is kept");
}

/// Navigator > Angle turns the document camera to what is typed.
#[test]
fn the_navigator_angle_field_turns_the_view() {
    let dir = tempfile::tempdir().unwrap();
    let mut ed = half_white(dir.path(), ScriptedDialogs::new());
    let mut win = Window::new(PanelId::Navigator);
    let field = win.rect(&mut ed, ids::navigator_angle());
    let _ = win.click_at(&mut ed, field.center(), egui::Modifiers::default());
    win.type_and_enter(&mut ed, "45");
    let rotation = ed.active().unwrap().camera.rotation;
    assert!(
        (rotation - 45f32.to_radians()).abs() < 1e-4,
        "the camera is at {} degrees",
        rotation.to_degrees()
    );
}

#[test]
fn a_round_tip_is_drawn_at_its_diameter_solid_to_its_hardness() {
    let settings = tools::BrushSettings {
        size: 21.0,
        hardness: 0.5,
        ..tools::BrushSettings::default()
    };
    let tip = tip_plane(&settings).unwrap();
    assert_eq!((tip.width, tip.height), (21, 21));
    assert_eq!(tip.alpha8[10 * 21 + 10], 255, "the centre is solid");
    assert_eq!(tip.alpha8[0], 0, "the corner is outside the disc");
    let rim = tip.alpha8[10 * 21 + 18];
    assert!(rim > 0 && rim < 255, "the soft edge fades: {rim}");
}

#[test]
fn the_composite_channel_loads_luminosity_and_a_component_its_values() {
    let px = [200u8, 100, 0, 255, 255, 255, 255, 0];
    let lum = channel_coverage(&px, ui::panels::channels::ChannelKind::Composite).unwrap();
    assert_eq!(
        lum,
        vec![119, 0],
        "Rec. 601 luminosity; transparent is unselected"
    );
    let red = channel_coverage(&px, ui::panels::channels::ChannelKind::Component(0)).unwrap();
    assert_eq!(red, vec![200, 0]);
}
