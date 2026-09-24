//! W10-B: the Layer Comps, Tool Presets, Glyphs, Notes and Character /
//! Paragraph Styles panels, and the Channels panel's alpha rows, driven
//! through the real workspace: each panel is opened in the dock, its controls
//! are found on screen by their stable ids and clicked with real pointer
//! events, and the intents the click raised are applied the way the
//! application applies them.

use editor_core::{Command, Document, History};
use layer_model::text::Alignment;
use layer_model::{Layer, LayerId, LayerKind, LayerMask, MaskId, TextLayer};
use tools::ToolId;
use ui::dock::{LayoutId, PanelId};
use ui::panels::{glyphs, layer_comps, notes, text_styles, tool_presets};
use ui::tool_options::OptionValue;
use ui::{Intent, MenuAction, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
}

impl Harness {
    fn new(doc: Document, panel: PanelId) -> Self {
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

    fn settle(&mut self) {
        for _ in 0..3 {
            let intents = self.frame(Vec::new());
            assert!(
                intents.iter().all(|i| i.as_command().is_none()),
                "an idle frame edited the document: {intents:?}"
            );
        }
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        self.settle();
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self
            .drawn(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .center();
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

    /// Click, then apply every document command the click raised through
    /// history, as the application does. Answers the intents.
    fn click_apply(&mut self, id: egui::Id) -> Vec<Intent> {
        let intents = self.click(id);
        for c in intents.iter().filter_map(Intent::as_command) {
            self.history.apply(&mut self.doc, c.clone()).unwrap();
        }
        intents
    }

    fn edit(&mut self, command: Command) {
        self.history.apply(&mut self.doc, command).unwrap();
    }
}

fn text_layer(name: &str) -> Layer {
    Layer::with_kind(
        name,
        LayerKind::Text(TextLayer {
            text: name.into(),
            ..TextLayer::default()
        }),
    )
}

fn text_of(doc: &Document, id: LayerId) -> &TextLayer {
    match &doc.layers.get(id).unwrap().kind {
        LayerKind::Text(t) => t,
        other => panic!("not text: {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Layer Comps
// ---------------------------------------------------------------------------

#[test]
fn a_layer_comp_restores_visibility_through_the_panel() {
    let mut doc = Document::new(64, 64, "Comps");
    let a = doc.layers.insert_at(Layer::raster("A"), None, 0).unwrap();
    let b = doc.layers.insert_at(Layer::raster("B"), None, 0).unwrap();
    let mut h = Harness::new(doc, PanelId::LayerComps);

    // New Layer Comp records both layers visible.
    h.click_apply(layer_comps::ids::new());
    assert_eq!(h.doc.extras.layer_comps.len(), 1);
    assert_eq!(h.doc.extras.layer_comps[0].name, "Layer Comp 1");

    // Hide A (as the Layers panel's eye would), then apply the comp by
    // clicking its row: A is visible again, B untouched.
    h.edit(Command::SetLayerProperties {
        layer_id: a,
        patch: editor_core::LayerPatch {
            visible: Some(false),
            ..Default::default()
        },
    });
    assert!(!h.doc.layers.get(a).unwrap().visible);
    h.click_apply(layer_comps::ids::row(0));
    assert!(h.doc.layers.get(a).unwrap().visible, "the comp restored A");
    assert!(h.doc.layers.get(b).unwrap().visible);

    // The apply was one undo step.
    h.history.undo(&mut h.doc).unwrap();
    assert!(!h.doc.layers.get(a).unwrap().visible);
}

#[test]
fn update_and_next_step_between_comps() {
    let mut doc = Document::new(64, 64, "Comps");
    let a = doc.layers.insert_at(Layer::raster("A"), None, 0).unwrap();
    let mut h = Harness::new(doc, PanelId::LayerComps);
    h.click_apply(layer_comps::ids::new()); // comp 1: A visible
    let hide = |visible| Command::SetLayerProperties {
        layer_id: a,
        patch: editor_core::LayerPatch {
            visible: Some(visible),
            ..Default::default()
        },
    };
    h.edit(hide(false));
    h.click_apply(layer_comps::ids::new()); // comp 2: A hidden
    assert_eq!(h.doc.extras.layer_comps.len(), 2);
    // Next wraps from comp 2 to comp 1: A shows.
    h.click_apply(layer_comps::ids::next());
    assert!(h.doc.layers.get(a).unwrap().visible);
    assert_eq!(h.doc.extras.last_comp, Some(0));
    // Previous goes back to comp 2: A hides.
    h.click_apply(layer_comps::ids::previous());
    assert!(!h.doc.layers.get(a).unwrap().visible);
    // Delete removes the applied comp.
    h.click_apply(layer_comps::ids::delete());
    assert_eq!(h.doc.extras.layer_comps.len(), 1);
}

// ---------------------------------------------------------------------------
// Tool Presets
// ---------------------------------------------------------------------------

#[test]
fn a_tool_preset_restores_the_tool_and_its_options() {
    let doc = Document::new(64, 64, "Presets");
    let mut h = Harness::new(doc, PanelId::ToolPresets);
    let model = ui::PaletteModel::build();
    h.workspace.palette.activate(&model, ToolId::Brush);
    assert!(h
        .workspace
        .options
        .set(ToolId::Brush, "size", OptionValue::Float(42.0)));

    h.click(tool_presets::ids::new());
    assert_eq!(h.workspace.tool_presets.presets().len(), 1);
    assert_eq!(h.workspace.tool_presets.presets()[0].name, "Brush 1");

    // Change the size and move to another tool.
    h.workspace
        .options
        .set(ToolId::Brush, "size", OptionValue::Float(5.0));
    h.workspace.palette.activate(&model, ToolId::Eraser);

    let intents = h.click(tool_presets::ids::row(0));
    assert_eq!(h.workspace.palette.active(), ToolId::Brush);
    assert_eq!(
        h.workspace.options.get(ToolId::Brush, "size"),
        Some(OptionValue::Float(42.0)),
        "the preset put the size back"
    );
    // The application hears the same intents the options bar would raise.
    assert!(
        intents.contains(&Intent::SelectTool(ToolId::Brush)),
        "{intents:?}"
    );
    assert!(intents.contains(&Intent::SetToolOption {
        tool: ToolId::Brush,
        key: "size",
        value: OptionValue::Float(42.0),
    }));
}

// ---------------------------------------------------------------------------
// Glyphs
// ---------------------------------------------------------------------------

#[test]
fn clicking_a_glyph_inserts_it_into_the_active_text_layer() {
    let mut doc = Document::new(64, 64, "Glyphs");
    let t = doc.layers.insert_at(text_layer("ab"), None, 0).unwrap();
    doc.set_active_layer(Some(t)).unwrap();
    let mut h = Harness::new(doc, PanelId::Glyphs);
    let set = glyphs::glyph_set(&h.ctx, "");
    assert!(set.chars.contains(&'A'), "{:?}", set.chars);
    let intents = h.click(glyphs::ids::cell('A'));
    // The click names the character and the layer; the application decides
    // between the live session's caret and the committed text.
    let insert = Intent::InsertGlyph {
        layer: t,
        text: "A".into(),
    };
    assert!(intents.contains(&insert), "{intents:?}");
    // With no session open, the committed route: appended, one undo step.
    let command = glyphs::insert_glyph(&h.doc, t, "A").unwrap();
    h.edit(command);
    assert_eq!(text_of(&h.doc, t).text, "abA");
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(text_of(&h.doc, t).text, "ab", "one undo step");
}

#[test]
fn a_glyph_click_with_no_text_layer_edits_nothing() {
    let mut doc = Document::new(64, 64, "Glyphs");
    let r = doc.layers.insert_at(Layer::raster("R"), None, 0).unwrap();
    doc.set_active_layer(Some(r)).unwrap();
    let mut h = Harness::new(doc, PanelId::Glyphs);
    let intents = h.click(glyphs::ids::cell('A'));
    assert!(intents.iter().all(|i| i.as_command().is_none()));
    assert!(
        !intents
            .iter()
            .any(|i| matches!(i, Intent::InsertGlyph { .. })),
        "{intents:?}"
    );
}

// ---------------------------------------------------------------------------
// Notes
// ---------------------------------------------------------------------------

#[test]
fn a_note_is_pinned_listed_and_deleted_through_the_panel() {
    let doc = Document::new(200, 100, "Notes");
    let mut h = Harness::new(doc, PanelId::Notes);
    // The first frames fit the canvas to the view; then centre the view
    // where the note should pin (the Navigator's intent).
    h.settle();
    h.workspace.absorb(&Intent::SetViewCenter((40.0, 30.0)));
    h.click_apply(notes::ids::new());
    assert_eq!(h.doc.extras.notes.len(), 1);
    let note = h.doc.extras.notes[0].clone();
    assert_eq!((note.x, note.y), (40.0, 30.0));
    assert_eq!(note.text, notes::NEW_NOTE_TEXT);
    // Its row is drawn: the text field and the buttons.
    assert!(h.drawn(notes::ids::text(note.id)).is_some());
    let intents = h.click(notes::ids::show(note.id));
    assert!(intents.contains(&Intent::SetViewCenter((40.0, 30.0))));
    h.click_apply(notes::ids::delete(note.id));
    assert!(h.doc.extras.notes.is_empty());
}

// ---------------------------------------------------------------------------
// Character and Paragraph Styles
// ---------------------------------------------------------------------------

#[test]
fn a_paragraph_style_change_updates_both_layers_that_use_it() {
    let mut doc = Document::new(64, 64, "Styles");
    let a = doc.layers.insert_at(text_layer("A"), None, 0).unwrap();
    let b = doc.layers.insert_at(text_layer("B"), None, 0).unwrap();
    doc.set_active_layer(Some(a)).unwrap();
    let kind = text_styles::StyleKind::Paragraph;
    let mut h = Harness::new(doc, PanelId::ParagraphStyles);

    // New Paragraph Style from A, then wear it on A and on B.
    h.click_apply(text_styles::ids::new(kind));
    let style = h.doc.extras.paragraph_styles[0].id;
    h.click_apply(text_styles::ids::row(kind, style));
    h.doc.set_active_layer(Some(b)).unwrap();
    h.click_apply(text_styles::ids::row(kind, style));
    assert_eq!(h.doc.extras.layers_with_paragraph(style), vec![a, b]);

    // Centre A's paragraph (the Paragraph panel's edit), then Redefine the
    // style from A: B follows.
    let mut centred = text_of(&h.doc, a).clone();
    centred.paragraph.alignment = Alignment::Center;
    h.edit(Command::SetLayerKind {
        layer_id: a,
        kind: Box::new(LayerKind::Text(centred)),
    });
    assert_ne!(text_of(&h.doc, b).paragraph.alignment, Alignment::Center);
    h.doc.set_active_layer(Some(a)).unwrap();
    h.click_apply(text_styles::ids::redefine(kind));
    assert_eq!(text_of(&h.doc, a).paragraph.alignment, Alignment::Center);
    assert_eq!(
        text_of(&h.doc, b).paragraph.alignment,
        Alignment::Center,
        "redefining the style updated the other layer that uses it"
    );
}

#[test]
fn a_character_style_applies_its_size_to_the_active_text_layer() {
    let mut doc = Document::new(64, 64, "Styles");
    let a = doc.layers.insert_at(text_layer("A"), None, 0).unwrap();
    let b = doc.layers.insert_at(text_layer("B"), None, 0).unwrap();
    doc.set_active_layer(Some(a)).unwrap();
    let kind = text_styles::StyleKind::Character;
    let mut h = Harness::new(doc, PanelId::CharacterStyles);
    let mut big = text_of(&h.doc, a).clone();
    big.size_px = 72.0;
    h.edit(Command::SetLayerKind {
        layer_id: a,
        kind: Box::new(LayerKind::Text(big)),
    });
    h.click_apply(text_styles::ids::new(kind));
    let style = h.doc.extras.character_styles[0].id;
    h.doc.set_active_layer(Some(b)).unwrap();
    h.click_apply(text_styles::ids::row(kind, style));
    assert_eq!(text_of(&h.doc, b).size_px, 72.0);
    assert_eq!(text_of(&h.doc, b).text, "B", "the text itself is kept");
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

#[test]
fn selecting_a_mask_channel_shows_it_alone_and_targets_it() {
    let mut doc = Document::new(64, 64, "Channels");
    let a = doc.layers.insert_at(Layer::raster("A"), None, 0).unwrap();
    let mask = MaskId::new();
    doc.layers.get_mut(a).unwrap().mask = Some(LayerMask::new(mask));
    let mut h = Harness::new(doc, PanelId::Channels);
    let kind = ui::panels::channels::ChannelKind::Mask { layer: a, mask };
    let rows = h.workspace.channels.rows(&h.doc);
    let index = rows.iter().position(|r| r.kind == kind).unwrap();
    // The row itself (its label area, right of the eye and thumbnail).
    let eye = h.drawn(ui::view::ids::channel_eye(index)).unwrap();
    let at = egui::pos2(eye.max.x + eye.width() * 4.0, eye.center().y);
    let intents = h.frame(vec![
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
    ]);
    assert!(
        intents.contains(&Intent::SelectChannel(kind)),
        "{intents:?}"
    );
    assert!(intents.contains(&Intent::SetEditTarget { mask: true }));
    assert!(intents.contains(&Intent::SelectLayers {
        layers: vec![a],
        active: Some(a),
    }));
    assert_eq!(h.workspace.mask_view, ui::MaskViewMode::Grayscale);
}

#[test]
fn an_alpha_rows_eye_opens_and_closes_the_channel() {
    let mut doc = Document::new(64, 64, "Channels");
    doc.saved_selections.push((
        "Alpha 1".into(),
        editor_core::Selection::Rect {
            min: glam::IVec2::new(0, 0),
            max: glam::IVec2::new(8, 8),
        },
    ));
    let mut h = Harness::new(doc, PanelId::Channels);
    let eye = ui::panels::channels::alpha_eye_id(0);
    let intents = h.click(eye);
    assert!(
        intents.contains(&Intent::Action(MenuAction::EditAlphaChannel(0))),
        "{intents:?}"
    );
    assert_eq!(h.workspace.mask_view, ui::MaskViewMode::Grayscale);
    // The application records the open channel; the eye then closes it.
    let layer = LayerId::new();
    h.doc.extras.alpha_edit = Some(layer_model::AlphaEdit { index: 0, layer });
    let intents = h.click(eye);
    assert!(intents.contains(&Intent::Action(MenuAction::CloseAlphaChannel)));
    assert_eq!(h.workspace.mask_view, ui::MaskViewMode::Composite);
}

#[test]
fn every_new_panel_is_in_the_window_menu_list_and_draws() {
    for panel in [
        PanelId::LayerComps,
        PanelId::ToolPresets,
        PanelId::Glyphs,
        PanelId::Notes,
        PanelId::CharacterStyles,
        PanelId::ParagraphStyles,
    ] {
        assert!(PanelId::ALL.contains(&panel));
        let mut h = Harness::new(Document::new(32, 32, "Draw"), panel);
        assert!(
            h.drawn(ui::view::ids::panel_tab(panel)).is_some(),
            "{panel:?} has no tab on screen"
        );
    }
}
