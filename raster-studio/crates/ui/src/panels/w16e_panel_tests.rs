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

#[test]
fn zz_tmp_diag_frames() {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let style = design::style_for(design::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    let raw = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1400.0, 900.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(raw(), |_| {});
    let doc = Document::new(320, 240, "Test");
    let history = History::new();
    let mut w = Workspace::new();
    w.palette
        .activate(&crate::PaletteModel::build(), tools::ToolId::FreeTransform);
    let mut all = Vec::new();
    for i in 0..6 {
        if i == 3 {
            w.canvas.sessions.transform = Some((
                tools::transform::TransformState::new(raster::PixelRect::new(20, 20, 200, 160)),
                tools::transform::TransformMode::Scale,
            ));
        }
        if i == 4 {
            w.canvas.sessions.clear();
        }
        let out = ctx.run(raw(), |ctx| w.ui(ctx, &doc, &history));
        let mut buckets: std::collections::BTreeMap<String, usize> = Default::default();
        for s in &out.shapes {
            let c = s.clip_rect;
            *buckets
                .entry(format!(
                    "{:.0},{:.0},{:.0},{:.0}",
                    c.min.x, c.min.y, c.max.x, c.max.y
                ))
                .or_default() += 1;
        }
        all.push((out.shapes.len(), buckets));
    }
    for (i, (n, b)) in all.iter().enumerate() {
        eprintln!("frame {i}: {n}");
        if i > 0 {
            for (k, v) in b {
                if all[i - 1].1.get(k) != Some(v) {
                    eprintln!("   {k}: {:?} -> {v}", all[i - 1].1.get(k));
                }
            }
            for (k, v) in &all[i - 1].1 {
                if !b.contains_key(k) {
                    eprintln!("   {k}: {v} -> gone");
                }
            }
        }
    }
}
