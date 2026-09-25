//! W13X-4: the Channels panel menu driven through the real workspace — the
//! panel opened in the dock, its header's overflow button clicked, the menu
//! rows clicked with real pointer events, the New Spot Channel dialog
//! confirmed with Enter — and what each click asks for read off the
//! workspace's outbox, exactly as the application drains it.

use editor_core::spot::SpotChannel;
use editor_core::{Command, Document, History, Selection};
use glam::IVec2;

use crate::dock::{LayoutId, PanelId};
use crate::menu::MenuAction;
use crate::panels::channels::spot_ids;
use crate::{Intent, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
}

impl Harness {
    fn new(doc: Document) -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        let mut workspace = Workspace::new();
        workspace.dock.apply_layout(LayoutId::Minimal);
        if !workspace.dock.is_open(PanelId::Channels) {
            workspace.absorb(&Intent::SetPanelOpen {
                panel: PanelId::Channels,
                open: true,
            });
        }
        workspace.dock.raise(PanelId::Channels);
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

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self
            .drawn(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .center();
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        self.frame(vec![
            egui::Event::PointerMoved(at),
            press(true),
            press(false),
        ])
    }

    fn press(&mut self, key: egui::Key) -> Vec<Intent> {
        self.frame(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::default(),
        }])
    }

    /// The Channels header's overflow button, then the menu row `row`.
    fn menu_row(&mut self, row: &'static str) -> Vec<Intent> {
        let _ = self.click(crate::view::ids::panel_menu(PanelId::Channels));
        assert_eq!(self.workspace.panel_menu, Some(PanelId::Channels));
        self.click(spot_ids::menu_row(row))
    }
}

fn selected_doc() -> Document {
    let mut doc = Document::new(64, 48, "spot");
    doc.selection = Selection::Rect {
        min: IVec2::new(4, 4),
        max: IVec2::new(20, 12),
    };
    doc
}

#[test]
fn merge_channels_in_the_channels_menu_raises_the_merge_row() {
    let mut h = Harness::new(selected_doc());
    let intents = h.menu_row("merge");
    assert!(
        intents.contains(&Intent::Action(MenuAction::MergeChannels)),
        "{intents:?}"
    );
    assert_eq!(h.workspace.panel_menu, None, "the menu closes after a pick");
}

#[test]
fn new_spot_channel_in_the_channels_menu_opens_the_dialog_and_ok_appends_one_step() {
    let mut h = Harness::new(selected_doc());
    let intents = h.menu_row("new-spot");
    assert!(intents.is_empty(), "opening the dialog is not an edit");
    let dialog = h
        .workspace
        .channels
        .spot_dialog
        .as_mut()
        .expect("the row opened New Spot Channel");
    assert_eq!(dialog.name(), "Spot Color 1");
    assert!(
        h.drawn(spot_ids::dialog_swatch()).is_some(),
        "the dialog is drawn"
    );
    let dialog = h.workspace.channels.spot_dialog.as_mut().unwrap();
    dialog.set_ink([0, 128, 255]);
    dialog.set_solidity(35);
    let intents = h.press(egui::Key::Enter);
    assert!(h.workspace.channels.spot_dialog.is_none(), "OK closes it");
    let [Intent::Document(command @ Command::SetSpotChannels { channels })] = intents.as_slice()
    else {
        panic!("OK queued {intents:?}");
    };
    assert_eq!(
        channels.as_slice(),
        &[SpotChannel {
            coverage: h.doc.selection.clone(),
            ..SpotChannel::empty("Spot Color 1", [0, 128, 255], 35)
        }]
    );

    // Applied the way the application applies it: one undo step, and the
    // panel then lists the channel with its ink swatch.
    h.history.apply(&mut h.doc, command.clone()).unwrap();
    assert_eq!(h.history.undo_depth(), 1);
    assert!(
        h.drawn(spot_ids::spot_swatch(0)).is_some(),
        "the spot channel has its row"
    );
}

#[test]
fn escape_cancels_new_spot_channel_without_an_edit() {
    let mut h = Harness::new(selected_doc());
    let _ = h.menu_row("new-spot");
    assert!(h.workspace.channels.spot_dialog.is_some());
    let _ = h.drawn(spot_ids::dialog_swatch());
    let intents = h.press(egui::Key::Escape);
    assert!(h.workspace.channels.spot_dialog.is_none());
    assert!(
        !intents
            .iter()
            .any(|i| matches!(i, Intent::Document(Command::SetSpotChannels { .. }))),
        "{intents:?}"
    );
}
