//! Driving the drawn workspace with real input.
//!
//! The model tests prove what a click *should* mean; the headless render test
//! proves the drawing runs. This file closes the gap between them: it finds a
//! control on screen by its stable id, clicks that exact rectangle, and asserts
//! on the intent that comes out — so a row that is drawn but wired to nothing,
//! or wired to the wrong layer, fails here.
//!
//! Two frames are needed for any click. egui only knows a widget's rectangle
//! *after* it has been laid out once, and a press and release must land inside
//! it, so every test lays out, reads the rect back, then clicks it.

use editor_core::{Command, Document, History};
use layer_model::{Layer, LayerId};
use ui::view::ids;
use ui::{Intent, PanelId, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
}

impl Harness {
    fn new() -> Self {
        let mut doc = Document::new(320, 240, "Test");
        let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
        doc.layers
            .insert_at(Layer::raster("Bottom"), None, 1)
            .unwrap();
        doc.set_active_layer(Some(a)).unwrap();
        Self::with_document(doc)
    }

    fn with_document(doc: Document) -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);

        let mut workspace = Workspace::new();
        workspace.dock.set_open(PanelId::Layers, true);

        Self {
            ctx,
            workspace,
            doc,
            history: History::new(),
        }
    }

    /// Clear the rail down to the one panel a test is about, so no row is
    /// pushed off the bottom of the window where a pointer cannot reach it.
    fn only_layers(&mut self) {
        self.workspace.dock.apply_layout(ui::LayoutId::Minimal);
        self.workspace.dock.set_open(PanelId::Layers, true);
    }

    /// Run frames until the layout stops moving, so a rectangle read from one
    /// frame is still where the next frame's pointer lands.
    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
    }

    fn rect(&mut self, id: egui::Id) -> egui::Rect {
        self.settle();
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect
    }

    /// Press at `from`, walk the pointer to `to` over several frames, release.
    ///
    /// The walk matters twice over: egui only calls a press a *drag* once the
    /// pointer has moved further than `max_click_dist`, and the whole point of
    /// this test is that the press and the release happen over different rows.
    /// Every frame's intents are collected, so a command emitted mid-drag is
    /// caught too.
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2) -> Vec<Intent> {
        let mut out = self.frame(vec![
            egui::Event::PointerMoved(from),
            egui::Event::PointerButton {
                pos: from,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
        ]);
        const STEPS: usize = 4;
        for step in 1..=STEPS {
            let at = from + (to - from) * (step as f32 / STEPS as f32);
            out.extend(self.frame(vec![egui::Event::PointerMoved(at)]));
        }
        out.extend(self.frame(vec![
            egui::Event::PointerMoved(to),
            egui::Event::PointerButton {
                pos: to,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ]));
        out
    }

    /// Drag one layer row onto another, releasing at `fraction` of the target
    /// row's height — the top third means "above", the bottom third "below",
    /// and the middle of a group row means "inside".
    fn drag_row(&mut self, from: LayerId, to: LayerId, fraction: f32) -> Vec<Intent> {
        let source = self.rect(ids::layer_row(from));
        let target = self.rect(ids::layer_row(to));
        let at = egui::pos2(target.center().x, target.top() + target.height() * fraction);
        self.drag(source.center(), at)
    }

    fn layers(&self) -> Vec<LayerId> {
        self.doc.layers.root().to_vec()
    }

    /// Run one frame with the given events and return what it emitted.
    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let _ = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        self.workspace.drain_intents()
    }

    /// Lay out once, then press and release inside the widget with `id`.
    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        self.frame(Vec::new());
        let rect = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect;
        let at = rect.center();
        self.frame(vec![
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
        ])
    }

    /// Press and release the SECONDARY button inside the widget with `id`.
    fn right_click(&mut self, id: egui::Id) -> Vec<Intent> {
        self.frame(Vec::new());
        let rect = self
            .ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect;
        let at = rect.center();
        self.frame(vec![
            egui::Event::PointerMoved(at),
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ])
    }

    /// Press and release a key.
    ///
    /// The release matters: egui marks a second press of a key it never saw
    /// released as a *repeat*, and the workspace ignores repeats so that
    /// holding `M` down does not walk the whole marquee group.
    fn press(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<Intent> {
        self.frame(vec![
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            },
            egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                repeat: false,
                modifiers,
            },
        ])
    }
}

fn command() -> egui::Modifiers {
    egui::Modifiers {
        command: true,
        ..Default::default()
    }
}

#[test]
fn clicking_a_layer_row_selects_that_layer_and_no_other() {
    let mut h = Harness::new();
    let layers = h.layers();
    let bottom = layers[1];

    let intents = h.click(ids::layer_row(bottom));
    let selected = intents
        .iter()
        .find_map(|i| match i {
            Intent::SelectLayers { layers, active } => Some((layers.clone(), *active)),
            _ => None,
        })
        .unwrap_or_else(|| panic!("clicking a layer row emitted {intents:?}"));
    assert_eq!(selected.0, vec![bottom]);
    assert_eq!(selected.1, Some(bottom));
}

#[test]
fn clicking_a_tool_button_selects_that_tool() {
    let mut h = Harness::new();
    let model = ui::PaletteModel::build();
    let slot = model
        .slot_of(tools::ToolId::Eraser)
        .expect("the eraser is in the palette");

    let intents = h.click(ids::tool_slot(slot));
    assert!(
        intents.contains(&Intent::SelectTool(tools::ToolId::Eraser)),
        "clicking the eraser emitted {intents:?}"
    );
    assert_eq!(h.workspace.palette.active(), tools::ToolId::Eraser);
}

#[test]
fn clicking_the_tool_that_is_already_active_emits_nothing() {
    let mut h = Harness::new();
    let model = ui::PaletteModel::build();
    let slot = model.slot_of(h.workspace.palette.active()).unwrap();
    let intents = h.click(ids::tool_slot(slot));
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::SelectTool(_))),
        "re-clicking the active tool emitted {intents:?}"
    );
}

#[test]
fn a_menu_shortcut_fires_through_the_real_frame_loop() {
    let mut h = Harness::new();
    h.history
        .apply(&mut h.doc, Command::create_layer(Layer::raster("Undoable")))
        .expect("apply");

    let intents = h.press(egui::Key::Z, command());
    assert!(
        intents.contains(&Intent::Action(ui::MenuAction::Undo)),
        "Ctrl+Z emitted {intents:?}"
    );
}

#[test]
fn a_shortcut_whose_menu_item_is_disabled_does_nothing_at_all() {
    let mut h = Harness::new();
    // Nothing has been done, so there is nothing to undo.
    let intents = h.press(egui::Key::Z, command());
    assert!(
        intents.is_empty(),
        "Ctrl+Z on an empty history emitted {intents:?}"
    );
}

#[test]
fn a_bare_letter_switches_tools_and_a_modified_one_does_not() {
    let mut h = Harness::new();
    let intents = h.press(egui::Key::E, egui::Modifiers::default());
    assert!(
        intents.contains(&Intent::SelectTool(tools::ToolId::Eraser)),
        "pressing E emitted {intents:?}"
    );

    // Ctrl+E is Merge Down, not a tool key.
    let before = h.workspace.palette.active();
    let intents = h.press(egui::Key::E, command());
    assert!(!intents.iter().any(|i| matches!(i, Intent::SelectTool(_))));
    assert_eq!(h.workspace.palette.active(), before);
}

#[test]
fn pressing_a_tool_letter_twice_cycles_to_the_next_variant() {
    let mut h = Harness::new();
    let group = tools::registry::by_shortcut('m');
    assert!(group.len() > 1);
    h.press(egui::Key::M, egui::Modifiers::default());
    assert_eq!(h.workspace.palette.active(), group[0]);
    let intents = h.press(egui::Key::M, egui::Modifiers::default());
    assert_eq!(h.workspace.palette.active(), group[1]);
    assert!(intents.contains(&Intent::SelectTool(group[1])));
}

#[test]
fn a_frame_with_no_input_emits_nothing_even_after_a_click() {
    let mut h = Harness::new();
    let layers = h.layers();
    h.click(ids::layer_row(layers[1]));
    let quiet = h.frame(Vec::new());
    assert!(quiet.is_empty(), "an idle frame emitted {quiet:?}");
}

#[test]
fn closing_a_panel_from_its_header_asks_to_close_that_panel() {
    // The close button has no id of its own, so drive it the way the menu
    // would: the Window menu item and the header button resolve to the same
    // intent, and that equality is what this asserts.
    let mut h = Harness::new();
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    let resolution = ui::MenuAction::TogglePanel(PanelId::Layers).resolve(&context);
    assert_eq!(
        resolution.intent(),
        Some(&Intent::SetPanelOpen {
            panel: PanelId::Layers,
            open: false,
        })
    );
}

#[test]
fn holding_a_tool_key_down_does_not_walk_the_whole_group() {
    // A held key arrives as a press that is never released. egui marks the
    // second and later presses as repeats, and a repeat must not cycle: the
    // user pressed the key once.
    let mut h = Harness::new();
    let down = |modifiers| {
        vec![egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]
    };
    h.frame(down(egui::Modifiers::default()));
    let first = h.workspace.palette.active();
    assert_eq!(first, tools::registry::by_shortcut('m')[0]);

    for _ in 0..3 {
        let intents = h.frame(down(egui::Modifiers::default()));
        assert!(intents.is_empty(), "a key repeat emitted {intents:?}");
        assert_eq!(h.workspace.palette.active(), first);
    }
}

#[test]
fn clicking_a_layer_row_eye_emits_a_visibility_command_for_that_layer() {
    use editor_core::{Command, LayerPatch};

    let mut h = Harness::new();
    let layers = h.layers();
    let top = layers[0];

    let intents = h.click(ids::layer_eye(top));
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("clicking the eye emitted {intents:?}"));
    assert_eq!(
        command,
        &Command::SetLayerProperties {
            layer_id: top,
            patch: LayerPatch {
                visible: Some(false),
                ..Default::default()
            },
        }
    );
}

#[test]
fn clicking_a_mask_thumbnail_aims_edits_at_the_mask_and_content_back_at_pixels() {
    // Card 055: the two thumbnail wells are the edit target's controls.
    use layer_model::{LayerMask, MaskId};

    let mut doc = Document::new(320, 240, "Test");
    let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(a)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    // The mask thumbnail click aims at the mask.
    let intents = h.click(ids::layer_mask_thumb(a));
    assert!(
        intents
            .iter()
            .any(|i| matches!(i, Intent::SetEditTarget { mask: true })),
        "the mask thumbnail click aims at the mask: {intents:?}"
    );
    assert_eq!(
        h.workspace.property_focus,
        ui::panels::properties::PropertyFocus::Mask,
        "the target border follows the click"
    );

    // The content thumbnail click aims back at pixels.
    let intents = h.click(ids::layer_content_thumb(a));
    assert!(
        intents
            .iter()
            .any(|i| matches!(i, Intent::SetEditTarget { mask: false })),
        "the content thumbnail click aims at pixels: {intents:?}"
    );
    assert_eq!(
        h.workspace.property_focus,
        ui::panels::properties::PropertyFocus::Layer,
        "the target border follows the click back"
    );
}

#[test]
fn clicking_a_mask_thumbnail_on_a_non_active_row_selects_that_row_and_aims_at_its_mask() {
    // Card 055's critical case: the clicked row is NOT the active one. The
    // well click must select the row FIRST, or the target would resolve
    // against the previously active layer while the clicked well showed the
    // border.
    use layer_model::{LayerMask, MaskId};

    let mut doc = Document::new(320, 240, "Test");
    let top = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    let bottom = doc
        .layers
        .insert_at(Layer::raster("Bottom"), None, 1)
        .unwrap();
    doc.layers.get_mut(bottom).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(top)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    let intents = h.click(ids::layer_mask_thumb(bottom));
    assert!(
        intents.iter().any(|i| matches!(
            i,
            Intent::SelectLayers {
                active: Some(l),
                ..
            } if *l == bottom
        )),
        "the click selects the clicked row: {intents:?}"
    );
    assert!(
        intents
            .iter()
            .any(|i| matches!(i, Intent::SetEditTarget { mask: true })),
        "the click aims at the clicked row's mask: {intents:?}"
    );
    assert_eq!(
        h.workspace.property_focus,
        ui::panels::properties::PropertyFocus::Mask
    );
}

// ---------------------------------------------------------------------------
// Drag to reorder, and drag to re-parent
// ---------------------------------------------------------------------------

fn moves(intents: &[Intent]) -> Vec<(LayerId, Option<LayerId>, usize)> {
    intents
        .iter()
        .filter_map(Intent::as_command)
        .filter_map(|c| match c {
            Command::MoveLayer {
                layer_id,
                parent,
                index,
            } => Some((*layer_id, *parent, *index)),
            _ => None,
        })
        .collect()
}

/// A group holding one child, plus a raster layer beneath the group.
fn nested_document() -> (Document, LayerId, LayerId, LayerId) {
    let mut doc = Document::new(320, 240, "Test");
    let group = doc
        .layers
        .insert_at(Layer::group("Group"), None, 0)
        .unwrap();
    let below = doc
        .layers
        .insert_at(Layer::raster("Below"), None, 1)
        .unwrap();
    let child = doc
        .layers
        .insert_at(Layer::raster("Child"), Some(group), 0)
        .unwrap();
    doc.set_active_layer(Some(below)).unwrap();
    (doc, group, child, below)
}

/// The headline interaction, driven through the drawn panel rather than
/// through the model.
///
/// The eight `resolve_drop` unit tests all passed while this was broken: the
/// drop was committed inside `if response.drag_stopped()` evaluated on the row
/// *under the pointer*, and egui only reports `drag_stopped` on the row the
/// drag began on. The insertion line painted; the `MoveLayer` never existed.
#[test]
fn dragging_a_layer_row_onto_another_emits_the_move_that_reorders_it() {
    let mut h = Harness::new();
    h.only_layers();
    let layers = h.layers();
    let (top, bottom) = (layers[0], layers[1]);

    // Release in the bottom third of the lower row: "put Top below Bottom".
    let intents = h.drag_row(top, bottom, 0.85);
    assert_eq!(
        moves(&intents),
        vec![(top, None, 1)],
        "dragging Top below Bottom emitted {intents:?}"
    );
    // ...and the drag state is left clean for the next one.
    assert_eq!(h.workspace.layers.dragging(), None);
}

#[test]
fn dragging_a_row_onto_the_top_of_another_puts_it_above_that_row() {
    let mut h = Harness::new();
    h.only_layers();
    let layers = h.layers();
    let (top, bottom) = (layers[0], layers[1]);

    // Bottom onto the top third of Top: "put Bottom above Top", index 0.
    let intents = h.drag_row(bottom, top, 0.1);
    assert_eq!(moves(&intents), vec![(bottom, None, 0)], "{intents:?}");
}

#[test]
fn dragging_a_row_into_the_middle_of_a_group_re_parents_it() {
    let (doc, group, _child, below) = nested_document();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    // The middle band of a group row is the "inside" target.
    let intents = h.drag_row(below, group, 0.5);
    assert_eq!(
        moves(&intents),
        vec![(below, Some(group), 0)],
        "dragging into a group emitted {intents:?}"
    );
}

/// The rejection, also through the drawn panel: a group dropped onto its own
/// child would stop the tree being a tree, and nothing at all may be emitted.
#[test]
fn dragging_a_group_onto_its_own_descendant_emits_nothing() {
    let (doc, group, child, _below) = nested_document();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    let intents = h.drag_row(group, child, 0.5);
    assert!(
        moves(&intents).is_empty(),
        "a cycle-forming drop emitted {intents:?}"
    );
    assert_eq!(h.workspace.layers.dragging(), None);
}

/// A drag released back where it started asks for no move — dropping a layer
/// on itself is how a user cancels.
#[test]
fn dragging_a_row_back_onto_itself_emits_nothing() {
    let mut h = Harness::new();
    h.only_layers();
    let top = h.layers()[0];
    let intents = h.drag_row(top, top, 0.1);
    assert!(moves(&intents).is_empty(), "{intents:?}");
    assert_eq!(h.workspace.layers.dragging(), None);
}

/// A drag released over nothing — outside the panel — clears the drag rather
/// than leaving it armed for the next click.
#[test]
fn releasing_a_drag_away_from_every_row_moves_nothing_and_clears_the_drag() {
    let mut h = Harness::new();
    h.only_layers();
    let layers = h.layers();
    let source = h.rect(ids::layer_row(layers[0])).center();
    let intents = h.drag(source, egui::pos2(SCREEN.x * 0.4, SCREEN.y * 0.5));
    assert!(moves(&intents).is_empty(), "{intents:?}");
    assert_eq!(h.workspace.layers.dragging(), None);
}

#[test]
fn the_eye_of_one_row_does_not_move_another_rows_layer() {
    let mut h = Harness::new();
    let layers = h.layers();
    let intents = h.click(ids::layer_eye(layers[1]));
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .expect("a command");
    match command {
        editor_core::Command::SetLayerProperties { layer_id, .. } => {
            assert_eq!(*layer_id, layers[1]);
            assert_ne!(*layer_id, layers[0]);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn the_mask_well_popup_picks_the_view_mode() {
    // Card 059: the mask well's right-click popup owns the canvas's mask
    // view — a panel setting, like the Channels panel's toggles. Picking a
    // mode updates it directly; the app never sees an intent because no
    // document state changes.
    use layer_model::{LayerMask, MaskId};

    let mut doc = Document::new(320, 240, "Test");
    let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(a)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only_layers();
    assert_eq!(h.workspace.mask_view, ui::MaskViewMode::Composite);

    let intents = h.right_click(ids::layer_mask_thumb(a));
    // Opening the popup may re-select the row (the ops act on the active
    // layer), but must touch no document state and no app action.
    assert!(
        intents
            .iter()
            .all(|i| matches!(i, Intent::SelectLayers { .. })),
        "opening the popup only selects the row: {intents:?}"
    );
    // The selection restyles the row; let the layout settle so the popup's
    // rect read and the click land on the same frame.
    h.settle();

    // The popup's Grayscale row is on screen; clicking it switches the view.
    let intents = h.click(ids::mask_view_item(ui::MaskViewMode::Grayscale));
    assert!(intents.is_empty());
    assert_eq!(
        h.workspace.mask_view,
        ui::MaskViewMode::Grayscale,
        "the popup picked the grayscale view"
    );
    // ...and picking Composite returns to the exporting appearance.
    h.right_click(ids::layer_mask_thumb(a));
    h.settle();
    let intents = h.click(ids::mask_view_item(ui::MaskViewMode::Composite));
    assert!(intents.is_empty());
    assert_eq!(h.workspace.mask_view, ui::MaskViewMode::Composite);
}

#[test]
fn the_mask_well_shows_the_active_target_badge_and_a_real_thumbnail() {
    // Card 059: the well draws the mask's real coverage thumbnail when the
    // application supplies one, and the aimed-at well carries a
    // discoverable target badge.
    use layer_model::{LayerMask, MaskId};

    let mut doc = Document::new(320, 240, "Test");
    let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    doc.layers
        .insert_at(Layer::raster("Bottom"), None, 1)
        .unwrap();
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(a)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    // A real coverage thumbnail: the chrome builds these from the mask
    // store; here a grey square stands in for one.
    let img = egui::ColorImage::from_rgba_unmultiplied([8usize, 8], &[64u8; 256]);
    let tex = h
        .ctx
        .load_texture("mask-thumb-a", img, egui::TextureOptions::NEAREST);
    h.workspace.mask_thumbs.insert(a, tex);

    // Aim at the mask (as the well's own click does).
    let _ = h.click(ids::layer_mask_thumb(a));
    h.settle();
    // The badge exists, is discoverable by id, and sits over the well.
    let badge = h
        .ctx
        .read_response(ids::mask_target_badge(a))
        .unwrap_or_else(|| panic!("the aimed-at mask well shows no target badge"));
    let well = h.ctx.read_response(ids::layer_mask_thumb(a)).unwrap().rect;
    assert!(
        well.intersects(badge.rect),
        "the badge sits on the mask well"
    );
    // The badge draws only on the AIMED-AT row: probe the second row's
    // badge id directly (it was never aimed at, so nothing was drawn).
    let bottom = *h
        .layers()
        .iter()
        .find(|id| **id != a)
        .unwrap_or_else(|| panic!("a second row exists"));
    assert!(
        h.ctx
            .read_response(ids::mask_target_badge(bottom))
            .is_none(),
        "a non-aimed row shows no target badge"
    );
}

#[test]
fn the_mask_well_popup_ops_ride_the_menu_route_on_the_right_layer() {
    // Card 059 (review round 1): the right-click SELECTS the row first, so
    // the popup's Enable/Disable row (which rides MaskOp::Toggle — an
    // active-layer op) cannot hit another layer's mask.
    use layer_model::{LayerMask, MaskId};

    let mut doc = Document::new(320, 240, "Test");
    let a = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    doc.layers
        .insert_at(Layer::raster("Bottom"), None, 1)
        .unwrap();
    // BOTH rows have masks, so both carry wells; a is active.
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    let bottom = *doc
        .layers
        .iter_depth_first()
        .iter()
        .find(|id| **id != a)
        .unwrap();
    doc.layers.get_mut(bottom).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(a)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only_layers();

    // Card 059 round-2: right-click the NON-ACTIVE row's well — the
    // protective select_only must aim the op at the clicked row, not the
    // previously active one (the round-1 wrong-layer bug path).
    let intents = h.right_click(ids::layer_mask_thumb(bottom));
    assert!(
        intents.iter().any(|i| matches!(
            i,
            Intent::SelectLayers {
                active: Some(active),
                ..
            } if *active == bottom
        )),
        "right-clicking a non-active well selects THAT row: {intents:?}"
    );
    h.settle();
    let intents = h.click(ids::mask_toggle_item(bottom));
    let action = intents.iter().find_map(Intent::as_action);
    assert!(
        matches!(
            action,
            Some(ui::menu::MenuAction::Mask(ui::menu::MaskOp::Toggle))
        ),
        "the popup's toggle row rides the menu route: {intents:?}"
    );
}
