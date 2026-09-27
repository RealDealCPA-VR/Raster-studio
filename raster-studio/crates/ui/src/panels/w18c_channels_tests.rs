//! W18-C: the Channels panel's colour rows driven through the real
//! workspace frame: a plain click selects one channel and shows it alone, a
//! Shift-click adds a channel (the drawn selection ring follows both rows),
//! the composite row restores all, and a CMYK document lists its inks.

use editor_core::{Document, History};

use crate::dock::{LayoutId, PanelId};
use crate::panels::channels::{ChannelKind, ChannelModel};
use crate::view::ids::channel_eye;
use crate::{Intent, Workspace};

const SCREEN: egui::Vec2 = egui::vec2(1400.0, 900.0);

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
    /// The keyboard modifiers held this frame (egui reads a click's Shift
    /// from the frame's input state, as the real window reports it).
    modifiers: egui::Modifiers,
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
            modifiers: egui::Modifiers::default(),
        }
    }

    /// One frame; answers the intents and the shapes it drew.
    fn frame(
        &mut self,
        events: Vec<egui::Event>,
    ) -> (Vec<Intent>, Vec<egui::epaint::ClippedShape>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            modifiers: self.modifiers,
            ..Default::default()
        };
        let out = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        (self.workspace.drain_intents(), out.shapes)
    }

    fn settle(&mut self) {
        for _ in 0..3 {
            self.frame(vec![egui::Event::PointerGone]);
        }
    }

    fn eye(&mut self, row: usize) -> Option<egui::Rect> {
        self.settle();
        self.ctx.read_response(channel_eye(row)).map(|r| r.rect)
    }

    /// Click row `row` on its label (right of the eye and the thumbnail).
    fn click_row(&mut self, row: usize, shift: bool) -> Vec<Intent> {
        let eye = self.eye(row).expect("the row's eye is drawn");
        let at = egui::pos2(eye.max.x + eye.width() * 4.0, eye.center().y);
        let modifiers = egui::Modifiers {
            shift,
            ..Default::default()
        };
        let press = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers,
        };
        self.modifiers = modifiers;
        let intents = self
            .frame(vec![
                egui::Event::PointerMoved(at),
                press(true),
                press(false),
            ])
            .0;
        self.modifiers = egui::Modifiers::default();
        intents
    }

    /// The rows (by index) the panel draws the selection ring around: a
    /// stroked rectangle spanning the row, well wider than the thumbnail.
    fn ringed_rows(&mut self, rows: usize) -> Vec<usize> {
        self.settle();
        let eyes: Vec<egui::Rect> = (0..rows)
            .map(|i| self.ctx.read_response(channel_eye(i)).unwrap().rect)
            .collect();
        let (_, shapes) = self.frame(vec![egui::Event::PointerGone]);
        let ring = |shape: &egui::Shape, eye: &egui::Rect| match shape {
            egui::Shape::Rect(r) => {
                r.stroke.width > 0.0
                    && r.rect.width() > eye.width() * 6.0
                    && r.rect.contains(eye.center())
            }
            _ => false,
        };
        eyes.iter()
            .enumerate()
            .filter(|(_, eye)| shapes.iter().any(|c| ring(&c.shape, eye)))
            .map(|(i, _)| i)
            .collect()
    }
}

#[test]
fn a_click_selects_one_channel_and_shift_click_adds_another() {
    let mut h = Harness::new(Document::new(64, 48, "rgb"));
    // Row 1 is Red, row 3 Blue (row 0 is the composite).
    let intents = h.click_row(1, false);
    assert!(
        intents.contains(&Intent::SelectChannel(ChannelKind::Component(0))),
        "{intents:?}"
    );
    // Photopea: the clicked channel alone is shown.
    assert!(h.workspace.channels.component_visible(0));
    assert!(!h.workspace.channels.component_visible(1));
    assert!(!h.workspace.channels.component_visible(2));
    assert_eq!(h.ringed_rows(4), vec![1]);

    let intents = h.click_row(3, true);
    let both = ChannelKind::Components {
        mask: 0b101,
        model: ChannelModel::Rgb,
    };
    assert!(
        intents.contains(&Intent::SelectChannel(both)),
        "{intents:?}"
    );
    assert_eq!(h.workspace.channels.selected, both);
    assert_eq!(
        h.workspace.channels.edit_mask(ChannelModel::Rgb),
        Some(0b101)
    );
    assert!(h.workspace.channels.component_visible(2));
    assert_eq!(h.ringed_rows(4), vec![1, 3], "both rows are ringed");

    // The composite row restores every channel.
    let intents = h.click_row(0, false);
    assert!(intents.contains(&Intent::SelectChannel(ChannelKind::Composite)));
    assert_eq!(h.workspace.channels.edit_mask(ChannelModel::Rgb), None);
    assert!(h
        .workspace
        .channels
        .composite_visible(&color::ColorSpace::Srgb));
}

#[test]
fn shift_clicking_every_component_is_the_composite() {
    let mut h = Harness::new(Document::new(64, 48, "rgb"));
    h.click_row(1, false);
    h.click_row(2, true);
    let intents = h.click_row(3, true);
    assert!(intents.contains(&Intent::SelectChannel(ChannelKind::Composite)));
}

#[test]
fn a_cmyk_document_lists_its_inks_without_eyes_and_selects_one() {
    let mut doc = Document::new(64, 48, "cmyk");
    doc.meta.color_mode = editor_core::color_mode::mode::CMYK;
    let mut h = Harness::new(doc);
    let rows = h.workspace.channels.rows(&h.doc);
    let names: Vec<&str> = rows.iter().take(5).map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["CMYK", "Cyan", "Magenta", "Yellow", "Black"]);
    // The composite has its eye; an ink row cannot be previewed alone.
    assert!(h.eye(0).is_some());
    assert!(h.eye(1).is_none(), "an ink row draws no eye");

    // The ink rows have no eye to aim at: walk down from the composite row's
    // label in half-row steps until a click lands on the Black row.
    let eye0 = h.eye(0).unwrap();
    let at = egui::pos2(eye0.max.x + eye0.width() * 4.0, eye0.center().y);
    let mut picked = None;
    for step in 1..12 {
        let y = at.y + eye0.height() * 0.5 * step as f32;
        let p = egui::pos2(at.x, y);
        let press = |pressed| egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        let (intents, _) = h.frame(vec![
            egui::Event::PointerMoved(p),
            press(true),
            press(false),
        ]);
        h.settle();
        if intents.contains(&Intent::SelectChannel(ChannelKind::Components {
            mask: 0b1000,
            model: ChannelModel::Cmyk,
        })) {
            picked = Some(step);
            break;
        }
    }
    assert!(picked.is_some(), "a click on the Black row selects black");
    assert_eq!(
        h.workspace.channels.edit_mask(ChannelModel::Cmyk),
        Some(0b1000)
    );
    // The canvas keeps the composite (no RGB component is hidden).
    assert!(h
        .workspace
        .channels
        .composite_visible(&color::ColorSpace::Srgb));
}
