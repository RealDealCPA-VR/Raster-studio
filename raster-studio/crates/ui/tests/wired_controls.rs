//! Every control that changes something must *say* it changed something.
//!
//! `clicking_the_real_thing.rs` proves the layer rows and the tool palette are
//! wired. This file covers the rest of the chrome: the menu bar, the tool
//! options bar, the status bar's zoom, the Layers panel's blend / opacity /
//! fill / locks and its footer, the History panel's stack and snapshots, the
//! Adjustments grid, the tool-options Reset, the gradient stop editor, the
//! Channels toggles, the Navigator's pan, the panel-move controls and the
//! Properties panel's adjustment editor. Each one is found on screen by its
//! stable id, driven, and the resulting intent asserted — so "drawn but wired
//! to nothing" fails here rather than being discovered.
//!
//! That claim was once broader than the file: a review deleted the command
//! emission from every menu row, every tool option, the Layers panel's opacity
//! and blend, the History rows and the Adjustments tiles, and the suite stayed
//! green. Each of those deletions now turns something in here red.
//!
//! The options bar is covered arm by arm, not just through the shared `emit`
//! closure: Float, Int, Bool and Choice each have their own driven control, so
//! a wiring bug in one arm cannot hide behind the others. `Color` is the one
//! arm with no click test, and `no_tool_declares_a_colour_option_so_that_arm_is_undrawn`
//! is why — it goes red the day a tool ships one.
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

    /// The whole response, for the tests that ask what a control *senses*
    /// rather than where it is.
    fn response_of(&mut self, id: egui::Id) -> Option<egui::Response> {
        self.settle();
        self.ctx.read_response(id)
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

    /// Press at `from`, walk the pointer to `to` over several frames, release.
    ///
    /// A slider or a `DragValue` only follows a pointer that *moves*: egui
    /// calls a press a drag once it has travelled further than
    /// `max_click_dist`, so a press and release in one frame changes nothing.
    /// Every frame's intents are collected, so a value emitted mid-drag counts.
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

    /// Drag the control with `id`, starting in its middle and moving `by`.
    fn drag_control(&mut self, id: egui::Id, by: egui::Vec2) -> Vec<Intent> {
        let from = self.rect(id).center();
        self.drag(from, from + by)
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

    /// Type `text` the way a keyboard does: the key press, the character it
    /// produced, then the release — one character per frame.
    ///
    /// [`Harness::type_into`] sends only the `Text` event, which no shortcut
    /// table ever looks at. A real keystroke carries a `Key` too, and that is
    /// the one the workspace routes to `keys::tool_for_key` when no field has
    /// focus — so a field that failed to take focus turns typing into tool
    /// switching. Only this spelling can catch that.
    fn type_keys(&mut self, text: &str) -> Vec<Intent> {
        let mut out = Vec::new();
        for ch in text.chars() {
            let key = egui::Key::from_name(&ch.to_string())
                .unwrap_or_else(|| panic!("egui has no key for {ch:?}"));
            out.extend(self.frame(vec![
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
                egui::Event::Text(ch.to_string()),
                egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: egui::Modifiers::default(),
                },
            ]));
        }
        out
    }

    /// Make `tool` the active tool, so its options bar is the one on screen.
    fn use_tool(&mut self, tool: tools::ToolId) {
        let model = ui::PaletteModel::build();
        self.workspace.palette.activate(&model, tool);
        self.settle();
    }

    /// Open a top-level menu by its title. Its rows are only drawn — and so
    /// only findable — while it is open.
    fn open_menu(&mut self, title: &'static str) {
        self.click(ids::menu_title(title));
    }
}

fn command() -> egui::Modifiers {
    egui::Modifiers {
        command: true,
        ..Default::default()
    }
}

/// Every `Intent::SetToolOption` a run of frames produced, in order.
fn option_writes(intents: &[Intent]) -> Vec<(tools::ToolId, &'static str, ui::OptionValue)> {
    intents
        .iter()
        .filter_map(|i| match i {
            Intent::SetToolOption { tool, key, value } => Some((*tool, *key, *value)),
            _ => None,
        })
        .collect()
}

/// Every layer patch a run of frames produced, in order.
fn patches(intents: &[Intent]) -> Vec<(layer_model::LayerId, editor_core::LayerPatch)> {
    intents
        .iter()
        .filter_map(Intent::as_command)
        .filter_map(|c| match c {
            editor_core::Command::SetLayerProperties { layer_id, patch } => {
                Some((*layer_id, patch.clone()))
            }
            _ => None,
        })
        .collect()
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
    let to = u8::try_from(before.len() - 2).unwrap();
    assert!(
        intents.contains(&Intent::ReorderPanel { panel: last, to }),
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
// The status bar's zoom
//
// It was worse than a dead control. The readout swapped itself for a
// `TextEdit` only on the frame *after* the click, so the field appeared
// without focus; `ctx.wants_keyboard_input()` stayed false, and
// `Workspace::handle_keys` went on handing the keystrokes that followed to
// `keys::tool_for_key`. Clicking the zoom and typing switched tools.
// ---------------------------------------------------------------------------

#[test]
fn one_click_on_the_zoom_field_is_enough_to_type_a_zoom_into_it() {
    let mut h = Harness::new();
    assert_eq!(h.workspace.status.zoom, 1.0);

    // One click. The second one is what used to be needed.
    h.click(ids::status_zoom());
    assert!(
        h.ctx.wants_keyboard_input(),
        "the zoom field did not take focus, so the next keystroke goes to the tool shortcuts"
    );

    let mut typed = h.key(egui::Key::A, command());
    typed.extend(h.type_keys("200"));
    assert!(
        !typed.iter().any(|i| matches!(i, Intent::SelectTool(_))),
        "typing into the zoom field reached the tool shortcuts: {typed:?}"
    );
    assert!(
        !typed.iter().any(|i| matches!(i, Intent::SetZoom(_))),
        "a half-typed zoom was committed: {typed:?}"
    );

    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    let zooms: Vec<f32> = intents
        .iter()
        .filter_map(|i| match i {
            Intent::SetZoom(z) => Some(*z),
            _ => None,
        })
        .collect();
    assert_eq!(zooms.len(), 1, "committing the zoom emitted {intents:?}");
    assert!(
        (zooms[0] - 2.0).abs() < 1e-6,
        "typing 200 asked for {zooms:?}"
    );
}

#[test]
fn a_letter_typed_into_the_zoom_field_does_not_switch_tools() {
    let mut h = Harness::new();
    let before = h.workspace.palette.active();
    assert_ne!(before, tools::ToolId::Eraser);

    h.click(ids::status_zoom());
    // `e` is the Eraser's key. Before the field took focus on its first click
    // this quietly changed tools under the user.
    let typed = h.type_keys("e");
    assert!(
        !typed.iter().any(|i| matches!(i, Intent::SelectTool(_))),
        "typing `e` into the zoom field emitted {typed:?}"
    );
    assert_eq!(h.workspace.palette.active(), before);
}

#[test]
fn an_unreadable_zoom_is_dropped_rather_than_guessed_at() {
    let mut h = Harness::new();
    h.click(ids::status_zoom());
    h.key(egui::Key::A, command());
    h.type_keys("abc");
    let intents = h.key(egui::Key::Enter, egui::Modifiers::default());
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::SetZoom(_))),
        "committing nonsense emitted {intents:?}"
    );
}

// ---------------------------------------------------------------------------
// The menu bar
//
// Everything menu-shaped was tested at the `MenuAction::resolve` level or
// through the keyboard. Nothing proved the drawn row posts its intent — the
// emit could be deleted from `view::menu_bar::item` and the suite stayed green.
// ---------------------------------------------------------------------------

#[test]
fn clicking_a_menu_row_posts_the_intent_that_row_resolves_to() {
    let mut h = Harness::new();
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    let expected = MenuAction::NewDocument
        .resolve(&context)
        .intent()
        .cloned()
        .expect("File ▸ New is available with a document open");

    h.open_menu("File");
    let intents = h.click(ids::menu_item(MenuAction::NewDocument));
    assert!(
        intents.contains(&expected),
        "clicking File ▸ New emitted {intents:?}"
    );
}

#[test]
fn a_menu_row_disabled_in_this_context_posts_nothing_when_clicked() {
    let mut h = Harness::new();
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    assert!(
        matches!(MenuAction::Undo.resolve(&context), Resolution::Disabled(_)),
        "nothing has been done yet, so Undo must be unavailable"
    );

    h.open_menu("Edit");
    let intents = h.click(ids::menu_item(MenuAction::Undo));
    assert!(
        intents.is_empty(),
        "clicking a disabled Edit ▸ Undo emitted {intents:?}"
    );
}

#[test]
fn clicking_a_checked_menu_toggle_asks_for_the_opposite_state() {
    use ui::ViewFlag;

    let mut h = Harness::new();
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    let was = MenuAction::ToggleView(ViewFlag::Rulers)
        .checked(&context)
        .expect("Rulers is a toggle and reports its state");

    h.open_menu("View");
    let intents = h.click(ids::menu_item(MenuAction::ToggleView(ViewFlag::Rulers)));
    assert!(
        intents.contains(&Intent::SetViewFlag {
            flag: ViewFlag::Rulers,
            on: !was,
        }),
        "View ▸ Rulers was {was} and emitted {intents:?}"
    );

    // …and with the flag flipped the same row asks for the opposite again, so
    // it reports the live state rather than a constant. (Nothing here absorbs
    // the intent: applying it is the application's job, and the workspace only
    // says what was asked for.)
    h.workspace.view_flags.set(ViewFlag::Rulers, !was);
    h.open_menu("View");
    let intents = h.click(ids::menu_item(MenuAction::ToggleView(ViewFlag::Rulers)));
    assert!(
        intents.contains(&Intent::SetViewFlag {
            flag: ViewFlag::Rulers,
            on: was,
        }),
        "with Rulers {} the row emitted {intents:?}",
        !was
    );
}

/// Every `Edit ▸ Transform` row the bar lists.
fn transform_rows() -> Vec<MenuAction> {
    ui::menu::menu_bar(0)
        .iter()
        .flat_map(ui::menu::Menu::actions)
        .filter(|a| matches!(a, MenuAction::Transform(_)))
        .collect()
}

#[test]
fn a_submenu_with_nothing_available_in_it_does_not_open() {
    // Every Transform op needs a layer. With none selected the Edit ▸ Transform
    // submenu must refuse to open rather than showing a list of dead rows.
    let mut h = Harness::with_document(Document::new(320, 240, "Test"));
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    let live = transform_rows()
        .into_iter()
        .filter(|a| a.resolve(&context).is_enabled())
        .count();
    assert_eq!(live, 0, "a Transform op is available without a layer");

    h.open_menu("Edit");
    h.click(ids::menu_submenu("Transform"));
    for action in transform_rows() {
        assert!(
            !h.is_drawn(ids::menu_item(action)),
            "{action:?} opened out of a submenu that has nothing available"
        );
    }
}

#[test]
fn the_same_submenu_opens_once_something_in_it_is_available() {
    // The control for the test above: without this, "nothing was drawn" would
    // also be satisfied by a submenu that never opens at all.
    let mut doc = Document::new(320, 240, "Test");
    let id = doc.layers.push_root(Layer::raster("Pixels")).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    let mut h = Harness::with_document(doc);
    h.frame(Vec::new());
    let context = h.workspace.menu_context(&h.doc, &h.history);
    assert!(
        transform_rows()
            .into_iter()
            .any(|a| a.resolve(&context).is_enabled()),
        "a pixel layer makes at least one Transform op available"
    );

    h.open_menu("Edit");
    h.click(ids::menu_submenu("Transform"));
    assert!(
        transform_rows()
            .into_iter()
            .any(|a| h.is_drawn(ids::menu_item(a))),
        "the Transform submenu did not open"
    );
}

// ---------------------------------------------------------------------------
// The tool options bar
//
// One control of every `OptionKind`, driven on screen. The `emit` closure in
// `view::toolbar::option_control` could be gutted — deleting
// `Intent::SetToolOption` for brush size, hardness, opacity, flow, the
// selection mode, the gradient shape, every checkbox and every colour — and
// all 953 tests still passed.
// ---------------------------------------------------------------------------

#[test]
fn dragging_a_float_option_writes_it_and_says_so() {
    let mut h = Harness::new();
    let tool = tools::ToolId::Brush;
    h.use_tool(tool);
    assert_eq!(
        h.workspace.options.get(tool, "size"),
        Some(ui::OptionValue::Float(24.0))
    );

    let intents = h.drag_control(ids::tool_option(tool, "size"), egui::vec2(24.0, 0.0));
    let writes = option_writes(&intents);
    let last = writes
        .last()
        .copied()
        .unwrap_or_else(|| panic!("dragging Size emitted {intents:?}"));
    assert_eq!(last.0, tool);
    assert_eq!(last.1, "size");
    match last.2 {
        ui::OptionValue::Float(v) => assert!(v > 24.0, "Size went the wrong way: {v}"),
        other => panic!("Size emitted {other:?}"),
    }
    // The intent and the workspace agree, so an application that follows the
    // stream paints with the size on screen.
    assert_eq!(h.workspace.options.get(tool, "size"), Some(last.2));
}

/// The `Int` arm of `option_control` has its own `response.changed()` test and
/// its own `emit` call, so gutting the shared closure is not enough to prove it
/// wired: a per-arm break in the arm nobody drives would pass unseen. Four
/// controls ship on this arm — Polygon `sides`, Star `points`, Patch `Width`
/// and Eyedropper `Sample Size` — so drive one of them for real.
#[test]
fn dragging_an_int_option_writes_it_and_says_so() {
    let mut h = Harness::new();
    let tool = tools::ToolId::Polygon;
    h.use_tool(tool);
    assert_eq!(
        h.workspace.options.get(tool, "sides"),
        Some(ui::OptionValue::Int(6))
    );

    let intents = h.drag_control(ids::tool_option(tool, "sides"), egui::vec2(24.0, 0.0));
    let writes = option_writes(&intents);
    let last = writes
        .last()
        .copied()
        .unwrap_or_else(|| panic!("dragging Sides emitted {intents:?}"));
    assert_eq!(last.0, tool);
    assert_eq!(last.1, "sides");
    match last.2 {
        ui::OptionValue::Int(v) => assert!(v > 6, "Sides went the wrong way: {v}"),
        other => panic!("Sides emitted {other:?}"),
    }
    // The intent and the workspace agree, so an application that follows the
    // stream draws the polygon the bar is showing.
    assert_eq!(h.workspace.options.get(tool, "sides"), Some(last.2));
}

/// Float, Int, Bool and Choice each have a click-level test above; `Color` has
/// none, because no tool in the registry declares one, so the bar never draws
/// that arm and there is nothing on screen to drive. That is a fact about the
/// schema, not a decision, so pin it: the day a tool ships a colour option this
/// goes red and asks for the fifth test rather than letting an undriven arm in
/// unnoticed.
#[test]
fn no_tool_declares_a_colour_option_so_that_arm_is_undrawn() {
    let with_colour: Vec<&str> = tools::registry::all()
        .iter()
        .flat_map(|info| info.options)
        .filter(|o| matches!(o.kind, tools::OptionKind::Color { .. }))
        .map(|o| o.key)
        .collect();
    assert!(
        with_colour.is_empty(),
        "a colour option now ships ({with_colour:?}) — drive it from the options \
         bar the way the Float and Int tests do"
    );
}

#[test]
fn clicking_a_bool_option_writes_it_and_says_so() {
    let mut h = Harness::new();
    let tool = tools::ToolId::Gradient;
    h.use_tool(tool);
    assert_eq!(
        h.workspace.options.get(tool, "dither"),
        Some(ui::OptionValue::Bool(true))
    );

    let intents = h.click(ids::tool_option(tool, "dither"));
    assert_eq!(
        option_writes(&intents),
        vec![(tool, "dither", ui::OptionValue::Bool(false))],
        "clicking Dither emitted {intents:?}"
    );
    assert_eq!(
        h.workspace.options.get(tool, "dither"),
        Some(ui::OptionValue::Bool(false))
    );
}

#[test]
fn picking_a_choice_option_writes_it_and_the_reader_follows() {
    use selection::BooleanOp;

    let mut h = Harness::new();
    let tool = tools::ToolId::RectMarquee;
    h.use_tool(tool);
    assert_eq!(
        h.workspace
            .options
            .selection_options(tool)
            .expect("the marquee declares a mode")
            .mode,
        BooleanOp::Replace
    );

    // The entries only exist while the drop-down is open.
    assert!(!h.is_drawn(ids::tool_option_choice(tool, "mode", 1)));
    h.click(ids::tool_option(tool, "mode"));
    let intents = h.click(ids::tool_option_choice(tool, "mode", 1));
    assert_eq!(
        option_writes(&intents),
        vec![(tool, "mode", ui::OptionValue::Choice(1))],
        "picking Add emitted {intents:?}"
    );
    assert_eq!(
        h.workspace.options.selection_options(tool).unwrap().mode,
        BooleanOp::Add,
        "the drop-down wrote an index the selection reader disagrees with"
    );
}

#[test]
fn picking_a_paint_blend_mode_writes_it_and_the_reader_follows() {
    use layer_model::BlendMode;

    let mut h = Harness::new();
    let tool = tools::ToolId::Brush;
    h.use_tool(tool);
    assert_eq!(
        h.workspace.options.blend_mode(tool),
        Some(BlendMode::Normal)
    );

    let multiply = BlendMode::ALL
        .iter()
        .position(|m| *m == BlendMode::Multiply)
        .expect("Multiply is a blend mode");
    let key = ui::tool_options::BLEND_MODE_KEY;
    h.click(ids::tool_option(tool, key));
    let intents = h.click(ids::tool_option_choice(tool, key, multiply));
    assert_eq!(
        option_writes(&intents),
        vec![(tool, key, ui::OptionValue::Choice(multiply))],
        "picking Multiply emitted {intents:?}"
    );
    assert_eq!(
        h.workspace.options.blend_mode(tool),
        Some(BlendMode::Multiply)
    );
}

// ---------------------------------------------------------------------------
// The Layers panel's blend, opacity, fill and locks
//
// The task names these verbatim. Only the pure builders were tested, which
// says nothing about whether the drawn control calls them.
// ---------------------------------------------------------------------------

/// Two raster layers with the top one active.
fn two_layer_document() -> (Document, layer_model::LayerId, layer_model::LayerId) {
    let mut doc = Document::new(320, 240, "Test");
    let top = doc.layers.insert_at(Layer::raster("Top"), None, 0).unwrap();
    let bottom = doc
        .layers
        .insert_at(Layer::raster("Bottom"), None, 1)
        .unwrap();
    doc.set_active_layer(Some(top)).unwrap();
    (doc, top, bottom)
}

#[test]
fn dragging_the_layers_opacity_slider_patches_the_active_layer() {
    let (doc, top, bottom) = two_layer_document();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Layers);

    let rect = h.rect(ids::layer_opacity());
    // A quarter of the way in from the left: firmly on the slider rather than
    // on the numeric field at its right-hand end.
    let from = egui::pos2(rect.left() + rect.width() * 0.25, rect.center().y);
    let intents = h.drag(from, egui::pos2(rect.left(), rect.center().y));
    let (layer, patch) = patches(&intents)
        .pop()
        .unwrap_or_else(|| panic!("dragging Opacity emitted {intents:?}"));
    assert_eq!(layer, top, "the slider patched the wrong layer");
    assert_ne!(layer, bottom);
    let opacity = patch.opacity.expect("the patch carries an opacity");
    assert!(opacity < 0.5, "dragging Opacity to the left gave {opacity}");
    assert_eq!(patch.fill_opacity, None, "Opacity also wrote Fill");
    assert_eq!(patch.blend_mode, None);
}

#[test]
fn dragging_the_layers_fill_slider_patches_fill_and_not_opacity() {
    let (doc, top, _bottom) = two_layer_document();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Layers);

    let rect = h.rect(ids::layer_fill());
    let from = egui::pos2(rect.left() + rect.width() * 0.25, rect.center().y);
    let intents = h.drag(from, egui::pos2(rect.left(), rect.center().y));
    let (layer, patch) = patches(&intents)
        .pop()
        .unwrap_or_else(|| panic!("dragging Fill emitted {intents:?}"));
    assert_eq!(layer, top);
    let fill = patch.fill_opacity.expect("the patch carries a fill");
    assert!(fill < 0.5, "dragging Fill to the left gave {fill}");
    assert_eq!(patch.opacity, None, "Fill also wrote Opacity");
}

#[test]
fn picking_a_blend_mode_in_the_layers_panel_patches_the_active_layer() {
    use layer_model::BlendMode;

    let (doc, top, _bottom) = two_layer_document();
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Layers);

    assert!(!h.is_drawn(ids::layer_blend_option(BlendMode::Multiply)));
    h.click(ids::layer_blend());
    let intents = h.click(ids::layer_blend_option(BlendMode::Multiply));
    let (layer, patch) = patches(&intents)
        .pop()
        .unwrap_or_else(|| panic!("picking Multiply emitted {intents:?}"));
    assert_eq!(layer, top);
    assert_eq!(patch.blend_mode, Some(BlendMode::Multiply));
    assert_eq!(patch.opacity, None);
}

#[test]
fn each_lock_glyph_engages_its_own_lock_and_no_other() {
    use ui::view::LockToggle;

    for toggle in LockToggle::ALL {
        let (doc, top, _bottom) = two_layer_document();
        let mut h = Harness::with_document(doc);
        h.only(PanelId::Layers);

        let intents = h.click(ids::layer_lock(toggle));
        let (layer, patch) = patches(&intents)
            .pop()
            .unwrap_or_else(|| panic!("clicking {toggle:?} emitted {intents:?}"));
        assert_eq!(layer, top);
        let locks = patch.locked.expect("the patch carries a lock state");
        let mut expected = layer_model::LockState::default();
        match toggle {
            LockToggle::Transparency => expected.transparency = true,
            LockToggle::Pixels => expected.pixels = true,
            LockToggle::Position => expected.position = true,
            LockToggle::All => expected.all = true,
        }
        assert_eq!(locks, expected, "{toggle:?} set the wrong flag");
    }
}

// ---------------------------------------------------------------------------
// The History panel and the Adjustments panel
// ---------------------------------------------------------------------------

/// A document with `edits` applied, so the History panel has rows to click.
fn edited_document(edits: usize) -> (Document, History) {
    let mut doc = Document::new(320, 240, "Test");
    let mut history = History::new();
    for i in 0..edits {
        history
            .apply(
                &mut doc,
                editor_core::Command::create_layer(Layer::raster(format!("L{i}"))),
            )
            .expect("apply");
    }
    (doc, history)
}

#[test]
fn clicking_a_history_row_jumps_by_the_distance_to_that_row() {
    let (doc, history) = edited_document(3);
    let mut h = Harness::with_document(doc);
    h.history = history;
    h.only(PanelId::History);

    // The stack is Open + three edits, and the document sits on the last one.
    let intents = h.click(ids::history_row(1));
    assert!(
        intents.contains(&Intent::HistoryJump(
            ui::panels::history::HistoryJump::undo(2)
        )),
        "clicking row 1 of a 3-edit stack emitted {intents:?}"
    );
}

#[test]
fn clicking_the_history_row_the_document_is_already_on_emits_nothing() {
    let (doc, history) = edited_document(3);
    let mut h = Harness::with_document(doc);
    h.history = history;
    h.only(PanelId::History);

    let intents = h.click(ids::history_row(3));
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::HistoryJump(_))),
        "clicking the current row emitted {intents:?}"
    );
}

#[test]
fn a_snapshot_whose_steps_were_discarded_is_drawn_inert() {
    let (doc, history) = edited_document(2);
    let mut h = Harness::with_document(doc);
    h.history = history;
    h.only(PanelId::History);
    h.workspace.snapshots.push(ui::panels::history::Snapshot {
        name: "Live".into(),
        index: 1,
    });
    h.workspace.snapshots.push(ui::panels::history::Snapshot {
        name: "Stale".into(),
        index: 99,
    });

    let live = h
        .response_of(ids::history_snapshot(0))
        .expect("the live snapshot is drawn");
    assert!(live.sense.click, "a reachable snapshot is not clickable");
    let stale = h
        .response_of(ids::history_snapshot(1))
        .expect("the stale snapshot is still drawn");
    assert!(
        !stale.sense.click,
        "a snapshot naming discarded steps is still clickable"
    );

    let intents = h.click(ids::history_snapshot(1));
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::HistoryJump(_))),
        "clicking a stale snapshot emitted {intents:?}"
    );

    let intents = h.click(ids::history_snapshot(0));
    assert!(
        intents.contains(&Intent::HistoryJump(
            ui::panels::history::HistoryJump::undo(1)
        )),
        "clicking a live snapshot emitted {intents:?}"
    );
}

#[test]
fn clicking_an_adjustments_tile_creates_a_layer_of_that_kind() {
    use ui::menu::AdjustmentId;

    let mut h = Harness::new();
    h.only(PanelId::Adjustments);
    let intents = h.click(ids::adjustment_tile(AdjustmentId::Threshold));
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("clicking the Threshold tile emitted {intents:?}"));
    // Not compared whole: every `Layer` is built with a fresh id, so the two
    // commands can never be equal. The kind is what the tile chose.
    match command {
        editor_core::Command::CreateLayer { layer } => match &layer.kind {
            layer_model::LayerKind::Adjustment(a) => assert_eq!(
                a.kind,
                layer_model::AdjustmentKind::Threshold { level: 0.5 },
                "the tile created the wrong adjustment"
            ),
            other => panic!("the Threshold tile created a {other:?}"),
        },
        other => panic!("the Threshold tile emitted {other:?}"),
    }

    // …and a different tile is a different adjustment, so the grid is not one
    // button repeated.
    let intents = h.click(ids::adjustment_tile(AdjustmentId::Invert));
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("clicking the Invert tile emitted {intents:?}"));
    match command {
        editor_core::Command::CreateLayer { layer } => match &layer.kind {
            layer_model::LayerKind::Adjustment(a) => {
                assert_eq!(a.kind, layer_model::AdjustmentKind::Invert)
            }
            other => panic!("the Invert tile created a {other:?}"),
        },
        other => panic!("the Invert tile emitted {other:?}"),
    }
}

#[test]
fn the_layers_footer_buttons_create_a_raster_layer_and_a_group() {
    let mut h = Harness::new();
    h.only(PanelId::Layers);

    let intents = h.click(ids::new_layer());
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("the + button emitted {intents:?}"));
    match command {
        editor_core::Command::CreateLayer { layer, .. } => assert!(
            matches!(layer.kind, layer_model::LayerKind::Raster(_)),
            "+ created a {:?}",
            layer.kind
        ),
        other => panic!("+ emitted {other:?}"),
    }

    let intents = h.click(ids::new_group());
    let command = intents
        .iter()
        .find_map(Intent::as_command)
        .unwrap_or_else(|| panic!("the group button emitted {intents:?}"));
    match command {
        editor_core::Command::CreateLayer { layer, .. } => assert!(
            matches!(layer.kind, layer_model::LayerKind::Group(_)),
            "the group button created a {:?}",
            layer.kind
        ),
        other => panic!("the group button emitted {other:?}"),
    }
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
