//! Every control that changes something must *say* it changed something.
//!
//! `clicking_the_real_thing.rs` proves the layers panel and the tool palette
//! are wired. This file covers the controls a review found drawn but silent:
//! the tool-options Reset, the gradient stop editor, the Channels toggles, the
//! Navigator's pan, the panel-move controls, and the Properties panel's
//! adjustment editor. Each one is found on screen by its stable id, clicked,
//! and the resulting intent asserted — so "drawn but wired to nothing" fails
//! here rather than being discovered.
//!
//! It also pins the two facts a drawn frame used to destroy: the dock's
//! saved-layout identity, and the viewport the Navigator measures the document
//! against.

use editor_core::{Document, History};
use layer_model::{AdjustmentLayer, Layer, LayerKind};
use ui::dock::{DockSide, LayoutId, PanelId};
use ui::panels::channels::ChannelKind;
use ui::view::ids;
use ui::{Intent, MenuAction, Resolution, Workspace};

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
    screen: egui::Vec2,
}

impl Harness {
    fn new() -> Self {
        Self::with_document(Document::new(320, 240, "Test"))
    }

    fn with_document(doc: Document) -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        Self {
            ctx,
            workspace: Workspace::new(),
            doc,
            history: History::new(),
            screen: egui::vec2(1400.0, 900.0),
        }
    }

    /// Show exactly one panel.
    ///
    /// A rail with five panels stacked in it pushes the last one's controls
    /// off the bottom of a 900pt window, where a click cannot reach them —
    /// which is a true fact about the layout and a useless one to test
    /// against. Each panel test opens only the panel it is about.
    fn only(&mut self, panel: PanelId) {
        self.workspace.dock.apply_layout(LayoutId::Minimal);
        self.workspace.dock.set_open(panel, true);
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
            events,
            ..Default::default()
        };
        let _ = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        self.workspace.drain_intents()
    }

    /// Run frames until the layout stops moving.
    ///
    /// Scroll bars appear on the frame *after* their content overflows, so a
    /// rectangle read from the first frame can have shifted by the time the
    /// click lands on it. Three quiet frames is enough for every panel here.
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

    fn is_drawn(&mut self, id: egui::Id) -> bool {
        self.settle();
        self.ctx.read_response(id).is_some()
    }

    fn click_at(&mut self, at: egui::Pos2) -> Vec<Intent> {
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

    /// Lay out once, then press and release inside the widget with `id`.
    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self.rect(id).center();
        self.click_at(at)
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<Intent> {
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

    /// Focus the text field with `id`, select what is in it, and type `text`
    /// one character per frame.
    ///
    /// One character per frame is the whole point: a field that re-seeds its
    /// buffer from the document every frame throws each keystroke away, and
    /// only a whole-string paste — which arrives in a single frame — appears
    /// to work. Whatever the typing itself emitted comes back, so a test can
    /// assert that nothing was committed early.
    fn type_into(&mut self, id: egui::Id, text: &str) -> Vec<Intent> {
        let mut out = self.click(id);
        out.extend(self.key(
            egui::Key::A,
            egui::Modifiers {
                command: true,
                ..Default::default()
            },
        ));
        for ch in text.chars() {
            out.extend(self.frame(vec![egui::Event::Text(ch.to_string())]));
        }
        out
    }
}

fn adjustment_document() -> Document {
    let mut doc = Document::new(320, 240, "Test");
    let id = doc
        .layers
        .push_root(Layer::with_kind(
            "Curves",
            LayerKind::Adjustment(AdjustmentLayer {
                // Curves has too many parameters for a dock, which is the
                // branch that draws "Open editor…".
                kind: layer_model::AdjustmentKind::Curves {
                    points: vec![[0.0, 0.0], [1.0, 1.0]],
                },
            }),
        ))
        .unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    doc
}

// ---------------------------------------------------------------------------
// The dock keeps its identity across a drawn frame
// ---------------------------------------------------------------------------

#[test]
fn drawing_a_frame_does_not_wipe_the_docks_saved_layout() {
    // The rail hands its measured width back every frame. It used to clear the
    // layout unconditionally, so after one frame Window ▸ Workspace showed no
    // checkmark against the arrangement actually in use.
    let mut h = Harness::new();
    assert_eq!(h.workspace.dock.layout(), Some(LayoutId::Essentials));
    for _ in 0..3 {
        h.frame(Vec::new());
        assert_eq!(
            h.workspace.dock.layout(),
            Some(LayoutId::Essentials),
            "a drawn frame lost the layout"
        );
    }
    let context = h.workspace.menu_context(&h.doc, &h.history);
    assert_eq!(
        MenuAction::ApplyLayout(LayoutId::Essentials).checked(&context),
        Some(true)
    );
    assert_eq!(
        MenuAction::ApplyLayout(LayoutId::Painting).checked(&context),
        Some(false)
    );
    // ...and re-applying the layout in use is honestly reported as no change.
    assert!(!h
        .workspace
        .absorb(&Intent::ApplyLayout(LayoutId::Essentials)));
    assert!(h.workspace.absorb(&Intent::ApplyLayout(LayoutId::Painting)));
}

#[test]
fn a_real_width_change_still_gives_up_the_layout() {
    let mut h = Harness::new();
    h.frame(Vec::new());
    let wider = h.workspace.dock.right_width() + 40.0;
    h.workspace.dock.set_side_width(DockSide::Right, wider);
    assert_eq!(h.workspace.dock.layout(), None);
}

// ---------------------------------------------------------------------------
// Panels really move
// ---------------------------------------------------------------------------

#[test]
fn moving_a_panel_across_sides_through_the_header_control() {
    let mut h = Harness::new();
    assert_eq!(
        h.workspace.dock.placement(PanelId::Layers).side,
        DockSide::Right
    );
    // The move controls are behind the header's "⋯" disclosure.
    assert!(!h.is_drawn(ids::panel_dock(PanelId::Layers, DockSide::Left)));
    h.click(ids::panel_menu(PanelId::Layers));
    assert_eq!(h.workspace.panel_menu, Some(PanelId::Layers));

    let intents = h.click(ids::panel_dock(PanelId::Layers, DockSide::Left));
    assert!(
        intents.contains(&Intent::DockPanel {
            panel: PanelId::Layers,
            side: DockSide::Left,
        }),
        "moving the layers panel emitted {intents:?}"
    );
    assert_eq!(
        h.workspace.dock.placement(PanelId::Layers).side,
        DockSide::Left
    );
    assert!(h
        .workspace
        .dock
        .panels_on(DockSide::Left)
        .contains(&PanelId::Layers));
    assert!(!h
        .workspace
        .dock
        .panels_on(DockSide::Right)
        .contains(&PanelId::Layers));
    // The disclosure closes behind the move rather than being left open over
    // a panel that is no longer there.
    assert_eq!(h.workspace.panel_menu, None);
}

#[test]
fn a_panel_moved_to_the_bottom_is_drawn_on_the_bottom_rail() {
    let mut h = Harness::new();
    h.click(ids::panel_menu(PanelId::History));
    let intents = h.click(ids::panel_dock(PanelId::History, DockSide::Bottom));
    assert!(intents.contains(&Intent::DockPanel {
        panel: PanelId::History,
        side: DockSide::Bottom,
    }));
    assert_eq!(
        h.workspace.dock.panels_on(DockSide::Bottom),
        vec![PanelId::History]
    );
    // The bottom rail draws it: its header is on screen again.
    assert!(h.is_drawn(ids::panel_menu(PanelId::History)));
}

#[test]
fn reordering_a_panel_within_its_side_through_the_header_control() {
    let mut h = Harness::new();
    let before = h.workspace.dock.panels_on(DockSide::Right);
    assert!(before.len() >= 2);
    let last = *before.last().unwrap();

    h.click(ids::panel_menu(last));
    let intents = h.click(ids::panel_reorder(last, true));
    assert!(
        intents.contains(&Intent::ReorderPanel {
            panel: last,
            up: true
        }),
        "reordering emitted {intents:?}"
    );
    let after = h.workspace.dock.panels_on(DockSide::Right);
    assert_ne!(after, before);
    assert_eq!(after.len(), before.len());
    assert_eq!(after[after.len() - 2], last);
}

#[test]
fn the_move_control_for_the_side_a_panel_is_already_on_does_nothing() {
    let mut h = Harness::new();
    h.click(ids::panel_menu(PanelId::Layers));
    let before = h.workspace.dock.clone();
    let intents = h.click(ids::panel_dock(PanelId::Layers, DockSide::Right));
    assert!(intents.is_empty(), "a no-op move emitted {intents:?}");
    assert_eq!(h.workspace.dock, before);
}

// ---------------------------------------------------------------------------
// Tool options
// ---------------------------------------------------------------------------

#[test]
fn resetting_a_tools_options_emits_an_intent() {
    let mut h = Harness::new();
    let tool = h.workspace.palette.active();
    // Reset is drawn disabled while there is nothing to reset.
    h.frame(Vec::new());
    assert!(h.workspace.options.is_default(tool));
    let intents = h.click(ids::tool_options_reset(tool));
    assert!(
        intents.is_empty(),
        "resetting an untouched tool emitted {intents:?}"
    );

    // Change something, and Reset comes alive and says so.
    assert!(h
        .workspace
        .options
        .set(tool, "size", ui::OptionValue::Float(123.0)));
    let intents = h.click(ids::tool_options_reset(tool));
    assert!(
        intents.contains(&Intent::ResetToolOptions(tool)),
        "Reset emitted {intents:?}"
    );
    assert!(h.workspace.options.is_default(tool));
}

#[test]
fn a_reset_intent_is_absorbed_back_into_the_workspace() {
    let mut w = Workspace::new();
    let tool = tools::ToolId::Brush;
    w.options.set(tool, "size", ui::OptionValue::Float(200.0));
    assert!(w.absorb(&Intent::ResetToolOptions(tool)));
    assert_eq!(w.options.brush_settings(tool).size, 24.0);
    assert!(!w.absorb(&Intent::ResetToolOptions(tool)));
}

#[test]
fn editing_the_gradient_ramp_emits_the_whole_ramp() {
    let mut h = Harness::new();
    let model = ui::PaletteModel::build();
    h.workspace
        .palette
        .activate(&model, tools::ToolId::Gradient);
    let tool = tools::ToolId::Gradient;

    // The stop editor is behind the ramp swatch.
    assert!(!h.is_drawn(ids::gradient_add_stop(tool)));
    h.click(ids::gradient_swatch(tool));
    assert!(h.is_drawn(ids::gradient_add_stop(tool)));

    let before = h.workspace.options.gradient(tool).stops.len();
    let intents = h.click(ids::gradient_add_stop(tool));
    let ramp = intents
        .iter()
        .find_map(|i| match i {
            Intent::SetToolGradient { tool: t, gradient } if *t == tool => Some(gradient.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("adding a stop emitted {intents:?}"));
    assert_eq!(ramp.stops.len(), before + 1);
    assert_eq!(h.workspace.options.gradient(tool), *ramp);

    // ...and the intent round-trips into another workspace, which is what
    // makes it worth emitting.
    let mut other = Workspace::new();
    assert!(other.absorb(&Intent::SetToolGradient {
        tool,
        gradient: ramp.clone(),
    }));
    assert_eq!(other.options.gradient(tool), *ramp);
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

#[test]
fn hiding_a_colour_channel_emits_an_intent() {
    let mut h = Harness::new();
    h.only(PanelId::Channels);
    // Row 1 is the first component; row 0 is the composite.
    let intents = h.click(ids::channel_eye(1));
    assert!(
        intents.contains(&Intent::SetChannelVisible {
            channel: ChannelKind::Component(0),
            visible: false,
        }),
        "hiding the red channel emitted {intents:?}"
    );
    assert!(!h.workspace.channels.component_visible(0));

    // The composite toggle carries its own intent too.
    let intents = h.click(ids::channel_eye(0));
    assert!(
        intents.iter().any(|i| matches!(
            i,
            Intent::SetChannelVisible {
                channel: ChannelKind::Composite,
                ..
            }
        )),
        "the composite toggle emitted {intents:?}"
    );
}

#[test]
fn selecting_a_channel_emits_an_intent_and_absorbs_back() {
    let mut h = Harness::new();
    h.only(PanelId::Channels);
    let row = h.rect(ids::channel_eye(2));
    // Click the row itself, to the right of its eye.
    let at = egui::pos2(row.right() + row.width() * 3.0, row.center().y);
    let intents = h.click_at(at);
    let selected = intents.iter().find_map(|i| match i {
        Intent::SelectChannel(k) => Some(*k),
        _ => None,
    });
    assert_eq!(selected, Some(ChannelKind::Component(1)));

    let mut other = Workspace::new();
    assert!(other.absorb(&Intent::SelectChannel(ChannelKind::Component(1))));
    assert_eq!(other.channels.selected, ChannelKind::Component(1));
    assert!(!other.absorb(&Intent::SelectChannel(ChannelKind::Component(1))));
}

#[test]
fn the_channel_chords_the_panel_prints_work_through_the_real_frame_loop() {
    let mut h = Harness::new();
    h.only(PanelId::Channels);
    h.frame(Vec::new());
    let intents = h.frame(vec![
        egui::Event::Key {
            key: egui::Key::Num3,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                ..Default::default()
            },
        },
        egui::Event::Key {
            key: egui::Key::Num3,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: egui::Modifiers {
                command: true,
                ..Default::default()
            },
        },
    ]);
    assert!(
        intents.contains(&Intent::SelectChannel(ChannelKind::Component(0))),
        "Ctrl+3 emitted {intents:?}"
    );
    // Isolating one component leaves the others off, which is what the chord
    // means everywhere else this chord exists.
    assert!(h.workspace.channels.component_visible(0));
    assert!(!h.workspace.channels.component_visible(1));
    assert!(!h.workspace.channels.component_visible(2));
}

// ---------------------------------------------------------------------------
// Navigator
// ---------------------------------------------------------------------------

#[test]
fn dragging_the_navigator_proxy_moves_the_canvas_camera() {
    let mut h = Harness::new();
    h.only(PanelId::Navigator);
    let rect = h.rect(ids::navigator_proxy());
    let middle = rect.center();

    // The canvas camera starts centred on the document, and `view_center` is a
    // readout *of that camera* rather than a number kept beside it — so the
    // middle of the proxy is the view the workspace already has, and clicking
    // it asks for nothing. A point or two of tolerance, because a scroll bar
    // can appear under the pointer and narrow the proxy by its own width
    // between the frame that measured it and the frame that clicks it.
    let quiet = h.click_at(middle);
    assert!(
        !quiet.iter().any(|i| matches!(i, Intent::SetViewCenter(_))),
        "a pan to where the camera already is emitted {quiet:?}"
    );
    assert!((h.workspace.view_center.0 - 160.0).abs() < 8.0);
    assert!((h.workspace.view_center.1 - 120.0).abs() < 8.0);

    // …and clicking away from the middle moves the camera itself.
    let at = middle + egui::vec2(rect.width() * 0.25, 0.0);
    let intents = h.click_at(at);
    let center = intents
        .iter()
        .find_map(|i| match i {
            Intent::SetViewCenter(c) => Some(*c),
            _ => None,
        })
        .unwrap_or_else(|| panic!("panning the navigator emitted {intents:?}"));
    assert!(center.0 > 160.0, "the pan went the wrong way: {center:?}");
    assert_eq!(h.workspace.view_center, center);
    assert!(
        (h.workspace.canvas.view.camera.center.x - center.0).abs() < 1e-3,
        "the Navigator moved a readout but not the camera: {:?}",
        h.workspace.canvas.view.camera.center
    );

    let mut other = Workspace::new();
    assert!(other.absorb(&Intent::SetViewCenter(center)));
    assert_eq!(other.view_center, center);
    assert_eq!(other.canvas.view.camera.center.x, center.0);
}

#[test]
fn fit_is_computed_against_the_viewport_that_is_actually_on_screen() {
    let mut h = Harness::new();
    h.only(PanelId::Navigator);
    h.frame(Vec::new());
    let measured = h.workspace.viewport;
    assert_ne!(
        measured,
        (1280.0, 720.0),
        "the viewport was never measured; it is still the constructed default"
    );
    assert!(measured.0 > 0.0 && measured.0 < h.screen.x);

    let wide = h
        .click(ids::navigator_fit())
        .iter()
        .find_map(|i| match i {
            Intent::SetZoom(z) => Some(*z),
            _ => None,
        })
        .expect("Fit emits a zoom");

    // Shrink the window and Fit must answer differently.
    h.screen = egui::vec2(700.0, 500.0);
    h.frame(Vec::new());
    assert_ne!(h.workspace.viewport, measured);
    let narrow = h
        .click(ids::navigator_fit())
        .iter()
        .find_map(|i| match i {
            Intent::SetZoom(z) => Some(*z),
            _ => None,
        })
        .expect("Fit emits a zoom");
    assert!(narrow < wide, "fit {narrow} was not tighter than {wide}");
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

#[test]
fn whatever_the_properties_panel_offers_for_an_adjustment_is_enabled_there() {
    // The assertion that was missing: the button drawn on an adjustment layer
    // must resolve to `Enabled` in the very context it is drawn in. It used to
    // emit `ApplyAdjustment`, which that same context refuses.
    let mut h = Harness::with_document(adjustment_document());
    h.only(PanelId::Properties);
    let intents = h.click(ids::adjustment_editor());
    let action = intents
        .iter()
        .find_map(Intent::as_action)
        .unwrap_or_else(|| panic!("Open editor… emitted {intents:?}"));

    let context = h.workspace.menu_context(&h.doc, &h.history);
    match action.resolve(&context) {
        Resolution::Enabled(intent) => assert_eq!(intent, Intent::Action(action)),
        Resolution::Disabled(reason) => {
            panic!("Properties offers {action:?}, which is disabled here: {reason}")
        }
    }
    assert_ne!(
        action,
        MenuAction::ApplyAdjustment(ui::menu::AdjustmentId::Curves),
        "the panel is still emitting the destructive Image ▸ Adjustments action"
    );
}

// ---------------------------------------------------------------------------
// The text fields
//
// All three were drawn, all three were dead, and all three died the same way:
// `let mut buf = <value from the document>;` re-runs every frame, so the
// keystroke is gone before any commit condition can see it. Each test types
// one character per frame — the way a person does — and asserts on what comes
// out.
// ---------------------------------------------------------------------------

#[test]
fn typing_a_name_into_the_properties_panel_renames_the_layer() {
    use editor_core::{Command, LayerPatch};

    let mut doc = Document::new(320, 240, "Test");
    let id = doc.layers.push_root(Layer::raster("Before")).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Properties);

    let typed = h.type_into(ids::layer_name(id), "After");
    assert!(
        !typed.iter().any(|i| matches!(i, Intent::Document(_))),
        "typing alone should not touch the document: {typed:?}"
    );

    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("committing the name emitted {intents:?}"));
    assert_eq!(
        command,
        &Command::SetLayerProperties {
            layer_id: id,
            patch: LayerPatch {
                name: Some("After".into()),
                ..Default::default()
            },
        }
    );
}

#[test]
fn a_name_left_unchanged_commits_nothing() {
    let mut doc = Document::new(320, 240, "Test");
    let id = doc.layers.push_root(Layer::raster("Before")).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Properties);

    h.click(ids::layer_name(id));
    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::Document(_))),
        "a field nobody edited emitted {intents:?}"
    );
}

#[test]
fn typing_a_font_family_into_the_character_panel_restyles_the_run() {
    use layer_model::{LayerKind, TextLayer};

    let mut doc = Document::new(320, 240, "Test");
    let id = doc
        .layers
        .push_root(Layer::with_kind(
            "Title",
            LayerKind::Text(TextLayer {
                text: "Hello".into(),
                font_family: "Inter".into(),
                size_px: 24.0,
            }),
        ))
        .unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Character);

    h.type_into(ids::character_family(id), "Georgia");
    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    let kind = intents
        .iter()
        .find_map(|i| match i {
            Intent::EditLayerKind { layer, kind } if *layer == id => Some(kind.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("committing the family emitted {intents:?}"));
    match *kind {
        LayerKind::Text(ref t) => assert_eq!(t.font_family, "Georgia"),
        ref other => panic!("the Character panel rewrote the layer as {other:?}"),
    }
}

#[test]
fn typing_a_hex_colour_sets_the_foreground_exactly_once() {
    use ui::panels::color::ColorNotation;

    let mut h = Harness::new();
    h.only(PanelId::Color);
    h.workspace.color.notation = ColorNotation::Hex;

    let typed = h.type_into(ids::color_hex(), "3366CC");
    // Each keystroke used to produce `<the whole current colour><one char>`,
    // which never parses — so the field could only ever be pasted into.
    assert!(
        !typed.iter().any(|i| matches!(i, Intent::SetForeground(_))),
        "a half-typed colour was committed: {typed:?}"
    );

    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    let set: Vec<[f32; 4]> = intents
        .iter()
        .filter_map(|i| match i {
            Intent::SetForeground(rgba) => Some(*rgba),
            _ => None,
        })
        .collect();
    assert_eq!(set.len(), 1, "committing the hex emitted {intents:?}");
    let expected = [
        f32::from(0x33u8) / 255.0,
        f32::from(0x66u8) / 255.0,
        f32::from(0xCCu8) / 255.0,
    ];
    for (got, want) in set[0].iter().zip(expected.iter()) {
        assert!((got - want).abs() < 1e-5, "{:?} is not #3366CC", set[0]);
    }
    assert_eq!(h.workspace.color.hex(), "#3366CC");
}

// ---------------------------------------------------------------------------
// The tool-palette fly-out has a way out
// ---------------------------------------------------------------------------

fn variant_slot() -> usize {
    ui::PaletteModel::build()
        .slot_of(tools::ToolId::RectMarquee)
        .expect("the marquees share a slot")
}

#[test]
fn clicking_a_slot_a_second_time_opens_its_flyout_and_a_third_shuts_it() {
    let mut h = Harness::new();
    let slot = variant_slot();

    h.click(ids::tool_slot(slot));
    assert_eq!(
        h.workspace.palette.open_flyout, None,
        "selecting a tool should not open its fly-out"
    );
    h.click(ids::tool_slot(slot));
    assert_eq!(h.workspace.palette.open_flyout, Some(slot));
    // The one that used to be impossible: `activate` cleared the flag and the
    // caller toggled it straight back on.
    h.click(ids::tool_slot(slot));
    assert_eq!(
        h.workspace.palette.open_flyout, None,
        "the fly-out re-opened itself"
    );
}

#[test]
fn a_click_away_from_the_flyout_dismisses_it() {
    let mut h = Harness::new();
    let slot = variant_slot();
    h.click(ids::tool_slot(slot));
    h.click(ids::tool_slot(slot));
    assert_eq!(h.workspace.palette.open_flyout, Some(slot));

    // A window with no title bar has no close control, and a window is not a
    // popup, so before this there was no way out but picking a tool.
    h.click_at(egui::pos2(h.screen.x * 0.6, h.screen.y * 0.7));
    assert_eq!(h.workspace.palette.open_flyout, None);
}

#[test]
fn picking_a_variant_from_the_flyout_selects_it_and_closes() {
    let mut h = Harness::new();
    let model = ui::PaletteModel::build();
    let slot = variant_slot();
    let variant = model.slots()[slot].tools[1];

    h.click(ids::tool_slot(slot));
    h.click(ids::tool_slot(slot));
    assert_eq!(h.workspace.palette.open_flyout, Some(slot));

    let intents = h.click(ids::flyout_tool(slot, variant));
    assert!(
        intents.contains(&Intent::SelectTool(variant)),
        "picking {variant:?} from the fly-out emitted {intents:?}"
    );
    assert_eq!(h.workspace.palette.active(), variant);
    assert_eq!(h.workspace.palette.open_flyout, None);
}

// ---------------------------------------------------------------------------
// The whole bar
// ---------------------------------------------------------------------------

#[test]
fn every_shortcut_hint_the_channels_panel_prints_resolves_to_something() {
    let doc = Document::new(64, 64, "Test");
    let channels = ui::panels::channels::ChannelsState::new();
    let rows = channels.rows(&doc);
    let printed: Vec<String> = rows.iter().filter_map(|r| r.shortcut_label()).collect();
    assert!(!printed.is_empty());
    for row in &rows {
        let Some(chord) = row.shortcut() else {
            continue;
        };
        // The panel's chord must not be claimed by a menu item, which would
        // swallow it before the panel ever saw the key.
        assert_eq!(
            ui::menu::action_for_shortcut(chord, 10),
            None,
            "{chord} is claimed by a menu item as well"
        );
        assert_eq!(
            channels.kind_for_digit(&doc, row.shortcut_digit.unwrap()),
            Some(row.kind)
        );
    }
}
