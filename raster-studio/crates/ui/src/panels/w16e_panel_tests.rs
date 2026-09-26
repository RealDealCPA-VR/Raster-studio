//! W16-E: the panel menus, Layer Comps flags and Last Document State, the
//! Notes author, and the mask popups, driven through real frames of the
//! workspace: each control is found on screen by its stable id and clicked
//! (or typed into) with real events, and the document commands the frame
//! raised are applied through history, as the application applies them.

use editor_core::{Command, Document, History};
use layer_model::{Layer, LayerId, LayerMask, MaskId, VectorMask};

use crate::dock::{LayoutId, PanelId};
use crate::menu::{MaskOp, MenuAction, VectorMaskOp};
use crate::panels::panel_menus_w16::{self as menus, ids, Library, ViewMode};
use crate::{Intent, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
}

impl Harness {
    fn new(doc: Document, panel: PanelId) -> Self {
        let _ = menus::take_requests();
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        let mut workspace = Workspace::new();
        workspace.dock.apply_layout(LayoutId::Minimal);
        workspace.dock.set_open(panel, true);
        workspace.dock.raise(panel);
        Self {
            ctx,
            workspace,
            doc,
            history: History::new(),
        }
    }

    /// One frame; the document commands it raised are applied.
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let _ = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        let intents = self.workspace.drain_intents();
        for c in intents.iter().filter_map(Intent::as_command) {
            self.history.apply(&mut self.doc, c.clone()).unwrap();
        }
        intents
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        self.settle();
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn press(&mut self, id: egui::Id, button: egui::PointerButton) -> Vec<Intent> {
        let at = self
            .drawn(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .center();
        let event = |pressed| egui::Event::PointerButton {
            pos: at,
            button,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(vec![
            egui::Event::PointerMoved(at),
            event(true),
            event(false),
        ])
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        self.press(id, egui::PointerButton::Primary)
    }

    fn right_click(&mut self, id: egui::Id) -> Vec<Intent> {
        self.press(id, egui::PointerButton::Secondary)
    }

    fn menu(&mut self, panel: PanelId, row: &str) -> Vec<Intent> {
        let _ = self.click(crate::view::ids::panel_menu(panel));
        self.click(ids::menu_row(panel, row))
    }

    /// Click the field `id`, replace its text with `text`, press Enter.
    fn type_into(&mut self, id: egui::Id, text: &str) -> Vec<Intent> {
        let _ = self.click(id);
        let select_all = egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        };
        let mut out = self.frame(vec![select_all, egui::Event::Text(text.to_string())]);
        out.extend(self.frame(vec![egui::Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }]));
        out
    }
}

/// The Brushes grid's tile `index` (`view::docks::brush_tile_id`, which is
/// private to the view module).
fn brush_tile(index: usize) -> egui::Id {
    egui::Id::new(("raster-brush-tile", index))
}

fn one_layer() -> (Document, LayerId) {
    let mut doc = Document::new(64, 48, "t");
    let a = doc.layers.insert_at(Layer::raster("A"), None, 0).unwrap();
    doc.set_active_layer(Some(a)).unwrap();
    (doc, a)
}

// ---------------------------------------------------------------------------
// Swatches / Brushes: Name Change, Delete, Tiles/List
// ---------------------------------------------------------------------------

#[test]
fn the_swatches_menu_renames_deletes_and_lists_the_swatch_last_clicked() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Swatches);
    let before = h.workspace.swatches.swatches().to_vec();
    let _ = h.click(ids::swatch_tile(2));
    assert_eq!(menus::selected(&h.ctx, Library::Swatches), Some(2));

    let _ = h.menu(PanelId::Swatches, "rename");
    let _ = h.type_into(ids::rename_field(Library::Swatches), "Ocean");
    let after = h.workspace.swatches.swatches().to_vec();
    assert_eq!(after.len(), before.len());
    assert_eq!(after[2].name, "Ocean", "renamed in place");
    assert_eq!(after[2].rgba, before[2].rgba, "same colour");

    let _ = h.menu(PanelId::Swatches, "delete");
    assert_eq!(h.workspace.swatches.len(), before.len() - 1);
    assert!(h
        .workspace
        .swatches
        .swatches()
        .iter()
        .all(|s| s.name != "Ocean"));

    // Tiles/List: the swatches are drawn as named rows instead of tiles.
    assert!(h.drawn(ids::list_row(Library::Swatches, 0)).is_none());
    let _ = h.menu(PanelId::Swatches, "tiles-list");
    assert_eq!(menus::view_mode(&h.ctx, Library::Swatches), ViewMode::List);
    assert!(h.drawn(ids::list_row(Library::Swatches, 0)).is_some());
    assert!(h.drawn(ids::swatch_tile(0)).is_none());
}

#[test]
fn the_brushes_menu_renames_a_brush_and_lists_the_brushes() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Brushes);
    let _ = h.click(brush_tile(1));
    let _ = h.menu(PanelId::Brushes, "rename");
    let _ = h.type_into(ids::rename_field(Library::Brushes), "Mine");
    assert_eq!(h.workspace.brushes.get(1).unwrap().name, "Mine");
    let _ = h.menu(PanelId::Brushes, "tiles-list");
    assert!(h.drawn(ids::list_row(Library::Brushes, 1)).is_some());
    assert!(h.drawn(brush_tile(1)).is_none());
}

#[test]
fn the_history_menu_takes_a_snapshot_and_asks_the_application_to_clear() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::History);
    let _ = h.menu(PanelId::History, "snapshot");
    assert_eq!(h.workspace.snapshots.len(), 1);
    // Nothing to clear yet: the row is greyed, posts nothing.
    let _ = h.menu(PanelId::History, "clear");
    assert!(menus::take_requests().is_empty());
    assert_eq!(h.workspace.panel_menu, Some(PanelId::History), "still open");
    h.workspace.panel_menu = None;
    let command = Command::create_layer(Layer::raster("B"));
    h.history.apply(&mut h.doc, command).unwrap();
    let _ = h.menu(PanelId::History, "clear");
    assert_eq!(
        menus::take_requests(),
        vec![menus::PanelRequest::ClearHistory]
    );
    assert!(
        h.workspace.snapshots.is_empty(),
        "the snapshots went with it"
    );
}

// ---------------------------------------------------------------------------
// Layer Comps: flags and the Last Document State
// ---------------------------------------------------------------------------

fn translation(doc: &Document, id: LayerId) -> glam::Vec2 {
    doc.layers.get(id).unwrap().transform.translation
}

fn edit(h: &mut Harness, id: LayerId, visible: bool, at: glam::Vec2) {
    let patch = editor_core::LayerPatch {
        visible: Some(visible),
        transform: Some(glam::Affine2::from_translation(at).to_cols_array()),
        ..Default::default()
    };
    h.history
        .apply(
            &mut h.doc,
            Command::SetLayerProperties {
                layer_id: id,
                patch,
            },
        )
        .unwrap();
}

/// A comp with its Position flag cleared puts back visibility but leaves
/// the layer where it is — Photopea's "only what it flags".
#[test]
fn a_comp_restores_only_the_aspects_its_flags_name() {
    let (doc, a) = one_layer();
    let mut h = Harness::new(doc, PanelId::LayerComps);
    let _ = h.click(crate::panels::layer_comps::ids::new());
    assert_eq!(h.doc.extras.layer_comps.len(), 1);
    let _ = h.click(crate::panels::layer_comps::ids::flag(0, 1));
    assert!(!h.doc.extras.layer_comps[0].flags.position, "Position off");
    assert!(h.doc.extras.layer_comps[0].flags.visibility);

    edit(&mut h, a, false, glam::vec2(10.0, 5.0));
    let _ = h.click(crate::panels::layer_comps::ids::row(0));
    let layer = h.doc.layers.get(a).unwrap();
    assert!(layer.visible, "visibility is flagged: put back");
    assert_eq!(
        translation(&h.doc, a),
        glam::vec2(10.0, 5.0),
        "position is not flagged: left alone"
    );

    // With Position back on, the same apply moves the layer home too.
    let _ = h.click(crate::panels::layer_comps::ids::flag(0, 1));
    let _ = h.click(crate::panels::layer_comps::ids::row(0));
    assert_eq!(translation(&h.doc, a), glam::Vec2::ZERO);
}

/// Applying a comp from the Last Document State keeps that state, and its
/// row puts it back.
#[test]
fn the_last_document_state_row_puts_back_what_a_comp_replaced() {
    let (doc, a) = one_layer();
    let mut h = Harness::new(doc, PanelId::LayerComps);
    let _ = h.click(crate::panels::layer_comps::ids::new());
    // Back in the Last Document State (no comp applied), with A hidden and
    // moved.
    let leave = editor_core::extras::edit_extras(&h.doc, |x| x.last_comp = None);
    h.history.apply(&mut h.doc, leave).unwrap();
    edit(&mut h, a, false, glam::vec2(7.0, 3.0));

    let _ = h.click(crate::panels::layer_comps::ids::row(0));
    assert!(h.doc.layers.get(a).unwrap().visible);
    assert_eq!(translation(&h.doc, a), glam::Vec2::ZERO);
    assert!(h.doc.extras.last_document_state.is_some(), "kept on apply");

    let _ = h.click(crate::panels::layer_comps::ids::last_state());
    assert!(!h.doc.layers.get(a).unwrap().visible, "hidden again");
    assert_eq!(translation(&h.doc, a), glam::vec2(7.0, 3.0), "moved again");
    assert_eq!(h.doc.extras.last_comp, None);
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

#[test]
fn a_notes_author_is_typed_into_its_field_as_one_undo_step() {
    let (mut doc, _) = one_layer();
    let (add, id) = editor_core::extras::add_note(&doc, 5.0, 5.0, "", "Check this");
    add.apply(&mut doc).unwrap();
    let mut h = Harness::new(doc, PanelId::Notes);
    let depth = h.history.undo_depth();
    let _ = h.type_into(crate::panels::notes::ids::author(id), "Val");
    assert_eq!(h.doc.extras.notes[0].author, "Val");
    assert_eq!(h.doc.extras.notes[0].text, "Check this");
    assert_eq!(h.history.undo_depth(), depth + 1);
}

// ---------------------------------------------------------------------------
// Mask popups
// ---------------------------------------------------------------------------

#[test]
fn the_raster_mask_popup_deletes_and_applies_the_mask() {
    let (mut doc, a) = one_layer();
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    let mut h = Harness::new(doc, PanelId::Layers);
    for (key, op) in [("delete", MaskOp::Delete), ("apply", MaskOp::Apply)] {
        let _ = h.right_click(crate::view::ids::layer_mask_thumb(a));
        let intents = h.click(ids::mask_item(a, key));
        assert!(
            intents.contains(&Intent::Action(MenuAction::Mask(op))),
            "{key}: {intents:?}"
        );
    }
}

#[test]
fn the_vector_mask_menu_disables_and_deletes_the_vector_mask() {
    let (mut doc, a) = one_layer();
    let mut mask = LayerMask::new(MaskId::new());
    mask.vector = Some(Box::new(VectorMask::new("M0 0 L64 0 L0 48 Z")));
    doc.layers.get_mut(a).unwrap().mask = Some(mask);
    let mut h = Harness::new(doc, PanelId::Layers);
    for (key, op) in [
        ("toggle", VectorMaskOp::Toggle),
        ("delete", VectorMaskOp::Delete),
    ] {
        let _ = h.right_click(crate::view::ids::layer_vector_mask_thumb(a));
        let intents = h.click(ids::vector_mask_item(a, key));
        assert!(
            intents.contains(&Intent::Action(MenuAction::VectorMask(op))),
            "{key}: {intents:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Preset menus: Photopea's order, Define New
// ---------------------------------------------------------------------------

/// Asserts the panel's menu rows were drawn, each reading before the next.
fn assert_menu_order(h: &mut Harness, panel: PanelId, order: &[&str]) {
    let rects: Vec<egui::Rect> = order
        .iter()
        .map(|key| {
            h.drawn(ids::menu_row(panel, key))
                .unwrap_or_else(|| panic!("{panel:?}: {key} was not drawn"))
        })
        .collect();
    for (pair, keys) in rects.windows(2).zip(order.windows(2)) {
        let (a, b) = (pair[0], pair[1]);
        let reads_before = a.max.y <= b.min.y + 0.5
            || ((a.center().y - b.center().y).abs() < 0.5 && a.max.x <= b.min.x + 0.5);
        assert!(
            reads_before,
            "{panel:?}: {} is not before {}: {a:?} {b:?}",
            keys[0], keys[1]
        );
    }
}

/// The Swatches menu is Photopea's folder gallery menu (`gF.Zd` in its app
/// bundle): Open .ACO, Export as .ACO, Name Change, Delete, Tiles/List,
/// Define New, New Folder. The Brushes menu is its `cq` gallery menu:
/// Define New, Thumbnails/List, Load, Export as, Name Change, Delete.
#[test]
fn the_preset_menu_rows_read_in_photopeas_order() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Swatches);
    let _ = h.click(crate::view::ids::panel_menu(PanelId::Swatches));
    assert_menu_order(
        &mut h,
        PanelId::Swatches,
        &[
            "open",
            "export",
            "rename",
            "delete",
            "tiles-list",
            "define-new",
            "new-folder",
        ],
    );

    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Brushes);
    let _ = h.click(crate::view::ids::panel_menu(PanelId::Brushes));
    assert_menu_order(
        &mut h,
        PanelId::Brushes,
        &[
            "define-new",
            "tiles-list",
            "open",
            "export",
            "rename",
            "delete",
        ],
    );
    assert!(h
        .drawn(ids::menu_row(PanelId::Brushes, "new-folder"))
        .is_none());
}

/// Swatches > Define New adds the current colour; Brushes > Define New
/// captures the tool's brush as a new preset.
#[test]
fn define_new_adds_the_current_colour_and_the_current_brush() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Swatches);
    let rgba = crate::panels::color::parse_hex("336699").expect("a hex colour");
    h.workspace.color.set_current(rgba);
    let before = h.workspace.swatches.len();
    let _ = h.menu(PanelId::Swatches, "define-new");
    assert_eq!(h.workspace.swatches.len(), before + 1);
    let added = h.workspace.swatches.swatches().last().unwrap().clone();
    assert_eq!(added.rgba, rgba);

    h.workspace.dock.set_open(PanelId::Brushes, true);
    h.workspace.dock.raise(PanelId::Brushes);
    let brushes = h.workspace.brushes.len();
    let _ = h.menu(PanelId::Brushes, "define-new");
    assert_eq!(h.workspace.brushes.len(), brushes + 1);
}

// ---------------------------------------------------------------------------
// Layer Comps: the Appearance flag
// ---------------------------------------------------------------------------

/// A comp flagged Appearance only puts back the opacity and blend mode it
/// recorded and leaves visibility and position as they are.
#[test]
fn a_comp_flagged_appearance_only_restores_opacity_not_visibility() {
    let (doc, a) = one_layer();
    let mut h = Harness::new(doc, PanelId::LayerComps);
    let _ = h.click(crate::panels::layer_comps::ids::new());
    let _ = h.click(crate::panels::layer_comps::ids::flag(0, 0));
    let _ = h.click(crate::panels::layer_comps::ids::flag(0, 1));
    let flags = h.doc.extras.layer_comps[0].flags;
    assert!(!flags.visibility && !flags.position && flags.appearance);

    let patch = editor_core::LayerPatch {
        visible: Some(false),
        opacity: Some(0.25),
        blend_mode: Some(layer_model::BlendMode::Multiply),
        transform: Some(glam::Affine2::from_translation(glam::vec2(4.0, 2.0)).to_cols_array()),
        ..Default::default()
    };
    h.history
        .apply(
            &mut h.doc,
            Command::SetLayerProperties { layer_id: a, patch },
        )
        .unwrap();
    let _ = h.click(crate::panels::layer_comps::ids::row(0));
    let layer = h.doc.layers.get(a).unwrap();
    assert_eq!(layer.opacity, 1.0, "appearance is flagged: put back");
    assert_eq!(layer.blend_mode, layer_model::BlendMode::Normal);
    assert!(!layer.visible, "visibility is not flagged: left hidden");
    assert_eq!(translation(&h.doc, a), glam::vec2(4.0, 2.0), "not moved");
}

// ---------------------------------------------------------------------------
// Channels: a spot channel row's options and delete buttons
// ---------------------------------------------------------------------------

fn spot_doc() -> Document {
    let (mut doc, _) = one_layer();
    doc.selection = editor_core::Selection::Rect {
        min: glam::IVec2::ZERO,
        max: glam::IVec2::new(8, 8),
    };
    let command = editor_core::spot::new_spot_channel(&doc, "Gold", [200, 150, 20], 50);
    command.apply(&mut doc).unwrap();
    doc.selection = editor_core::Selection::None;
    doc
}

/// The options button opens Spot Channel Options on that channel; OK
/// rewrites its name, ink and solidity as one undo step, keeping its
/// coverage. The bin deletes it as one undo step.
#[test]
fn a_spot_channel_rows_options_edit_it_and_its_bin_deletes_it() {
    let mut h = Harness::new(spot_doc(), PanelId::Channels);
    let coverage = h.doc.spot_channels[0].coverage.clone();
    let _ = h.click(ids::spot_edit(0));
    let dialog = h
        .workspace
        .channels
        .spot_dialog
        .as_mut()
        .expect("the options button opened the dialog");
    assert_eq!(dialog.name(), "Gold");
    assert_eq!(dialog.ink(), [200, 150, 20]);
    assert_eq!(dialog.solidity(), 50);
    dialog.set_name("Silver");
    dialog.set_ink([180, 180, 190]);
    dialog.set_solidity(80);
    let depth = h.history.undo_depth();
    let _ = h.frame(vec![egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }]);
    assert!(h.workspace.channels.spot_dialog.is_none(), "OK closes it");
    assert_eq!(h.doc.spot_channels.len(), 1, "edited, not added");
    let spot = &h.doc.spot_channels[0];
    assert_eq!(
        (spot.name.as_str(), spot.ink, spot.solidity),
        ("Silver", [180, 180, 190], 80)
    );
    assert_eq!(spot.coverage, coverage, "coverage kept");
    assert_eq!(h.history.undo_depth(), depth + 1);

    let _ = h.click(ids::spot_delete(0));
    assert!(h.doc.spot_channels.is_empty());
    assert_eq!(h.history.undo_depth(), depth + 2);
}

// ---------------------------------------------------------------------------
// Styles: no Define New; Swatches: folders
// ---------------------------------------------------------------------------

/// Photopea's `cq` leaves Define New off the Styles menu (and only the
/// Swatches list, its folder gallery, has New Folder): the Styles menu
/// draws Tiles/List, Open, Export, Name Change and Delete and nothing else.
#[test]
fn the_styles_menu_has_no_define_new_and_no_new_folder() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Styles);
    let _ = h.click(crate::view::ids::panel_menu(PanelId::Styles));
    for key in ["tiles-list", "open", "export", "rename", "delete"] {
        assert!(
            h.drawn(ids::menu_row(PanelId::Styles, key)).is_some(),
            "{key} was not drawn"
        );
    }
    assert!(h
        .drawn(ids::menu_row(PanelId::Styles, "define-new"))
        .is_none());
    assert!(h
        .drawn(ids::menu_row(PanelId::Styles, "new-folder"))
        .is_none());
    // The Brushes menu keeps Define New and has no folders either.
    h.workspace.dock.set_open(PanelId::Brushes, true);
    h.workspace.dock.raise(PanelId::Brushes);
    let _ = h.click(crate::view::ids::panel_menu(PanelId::Brushes));
    assert!(h
        .drawn(ids::menu_row(PanelId::Brushes, "define-new"))
        .is_some());
    assert!(h
        .drawn(ids::menu_row(PanelId::Brushes, "new-folder"))
        .is_none());
}

impl Harness {
    /// Press on `from`, drag past the threshold, carry it to `to`, let go.
    fn drag(&mut self, from: egui::Id, to: egui::Id) {
        let a = self
            .drawn(from)
            .unwrap_or_else(|| panic!("{from:?} was not drawn"))
            .center();
        let b = self
            .drawn(to)
            .unwrap_or_else(|| panic!("{to:?} was not drawn"))
            .center();
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let _ = self.frame(vec![egui::Event::PointerMoved(a), button(a, true)]);
        let _ = self.frame(vec![egui::Event::PointerMoved(a + egui::vec2(0.0, 12.0))]);
        let _ = self.frame(vec![egui::Event::PointerMoved(b)]);
        let _ = self.frame(vec![egui::Event::PointerMoved(b)]);
        let _ = self.frame(vec![button(b, false)]);
    }
}

/// Swatches > New Folder makes an open folder named "New folder" and opens
/// Name Change on it; a swatch dragged onto its header goes into it (and is
/// drawn under it); the chevron closes it; a click on the header makes the
/// menu act on the folder: Export writes the folder's swatches, Delete
/// removes the folder and the swatches in it.
#[test]
fn new_folder_holds_the_swatches_dragged_onto_it_and_the_menu_acts_on_it() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Swatches);
    let palette = h.workspace.swatches.swatches().to_vec();
    assert!(h.drawn(ids::swatch_folder(0)).is_none());

    let _ = h.menu(PanelId::Swatches, "new-folder");
    let folders = menus::swatch_folders(&h.ctx);
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].name, "New folder");
    assert!(folders[0].open);
    assert!(h.drawn(ids::swatch_folder(0)).is_some(), "header drawn");
    let _ = h.type_into(ids::folder_rename_field(), "Warm");
    assert_eq!(menus::swatch_folders(&h.ctx)[0].name, "Warm");

    // Drag Red (palette index 6) onto the folder header.
    let red = menus::swatch_key(palette[6].rgba);
    let loose_before = h.drawn(ids::swatch_tile(6)).unwrap();
    h.drag(ids::swatch_tile(6), ids::swatch_folder(0));
    assert_eq!(menus::swatch_folders(&h.ctx)[0].members, vec![red]);
    assert_eq!(h.workspace.swatches.len(), palette.len(), "moved, not lost");
    let header = h.drawn(ids::swatch_folder(0)).unwrap();
    let inside = h.drawn(ids::swatch_tile(6)).expect("drawn in the folder");
    assert!(inside.min.y >= header.max.y - 0.5, "under the header");
    assert_ne!(inside, loose_before);

    // The chevron closes the folder: its swatch is no longer drawn.
    let _ = h.click(ids::swatch_folder_toggle(0));
    assert!(!menus::swatch_folders(&h.ctx)[0].open);
    assert!(h.drawn(ids::swatch_tile(6)).is_none());
    let _ = h.click(ids::swatch_folder_toggle(0));
    assert!(h.drawn(ids::swatch_tile(6)).is_some());

    // A click on the header: Export writes just the folder's swatches.
    let _ = h.click(ids::swatch_folder(0));
    assert_eq!(menus::selected_folder(&h.ctx), Some(0));
    assert_eq!(menus::selected(&h.ctx, Library::Swatches), None);
    let _ = menus::take_requests();
    let _ = h.menu(PanelId::Swatches, "export");
    assert_eq!(
        menus::take_requests(),
        vec![menus::PanelRequest::ExportSwatches(vec![(
            palette[6].name.clone(),
            palette[6].rgba
        )])]
    );

    // Delete takes the folder and its swatch.
    let _ = h.click(ids::swatch_folder(0));
    let _ = h.menu(PanelId::Swatches, "delete");
    assert!(menus::swatch_folders(&h.ctx).is_empty());
    assert_eq!(h.workspace.swatches.len(), palette.len() - 1);
    assert!(h
        .workspace
        .swatches
        .swatches()
        .iter()
        .all(|s| menus::swatch_key(s.rgba) != red));
}

/// A swatch in a folder dragged onto a top-level swatch comes back out,
/// after the swatch it was dropped on; one dragged onto a swatch in a
/// folder goes into that folder after it. List rows drag the same way.
#[test]
fn a_swatch_dragged_onto_a_swatch_joins_that_swatchs_folder_or_the_top_level() {
    let (doc, _) = one_layer();
    let mut h = Harness::new(doc, PanelId::Swatches);
    let palette = h.workspace.swatches.swatches().to_vec();
    let key = |i: usize| menus::swatch_key(palette[i].rgba);
    let _ = h.menu(PanelId::Swatches, "new-folder");
    let _ = h.type_into(ids::folder_rename_field(), "Reds");
    h.drag(ids::swatch_tile(6), ids::swatch_folder(0));
    // Orange (7) dropped on Red (6), which is in the folder: in, after Red.
    h.drag(ids::swatch_tile(7), ids::swatch_tile(6));
    assert_eq!(
        menus::swatch_folders(&h.ctx)[0].members,
        vec![key(6), key(7)]
    );

    // As List rows: Red dropped on Black (0, top level) comes out after it.
    let _ = h.menu(PanelId::Swatches, "tiles-list");
    h.drag(
        ids::list_row(Library::Swatches, 6),
        ids::list_row(Library::Swatches, 0),
    );
    assert_eq!(menus::swatch_folders(&h.ctx)[0].members, vec![key(7)]);
    let now: Vec<[u8; 4]> = h
        .workspace
        .swatches
        .swatches()
        .iter()
        .map(|s| menus::swatch_key(s.rgba))
        .collect();
    assert_eq!(&now[..2], &[key(0), key(6)], "Red now follows Black");
    assert_eq!(now.len(), palette.len());
}
