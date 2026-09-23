//! The header reorder chevron, driven against whatever the default layout is.
//!
//! W2-D moved the wide column to Photopea's stack — `[Properties|Adjustments]
//! [History] [Layers|Channels|Paths]`, Layers as the flexing bottom group —
//! and the two tests that used to pin the chevron's behaviour (one in
//! `wired_controls.rs`, since deleted in favour of this file, and
//! `the_header_reorder_control_moves_a_panel_exactly_one_place` in
//! `app-shell/src/chrome.rs`) were written against the old `[…] [Layers]
//! [History|Color]` shape: they named the bottom group's members and assumed
//! the group above it was one panel wide. This is the same proof — one click
//! moves the active tab's *whole* group exactly one slot up, and the intent
//! carries the absolute destination — stated in terms of the groups the dock
//! reports, so it holds for any preset.

use editor_core::{Document, History};
use ui::dock::{DockSide, PanelId};
use ui::view::ids;
use ui::{Intent, Workspace};

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
    screen: egui::Vec2,
}

impl Harness {
    fn new() -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
        Self {
            ctx,
            workspace: Workspace::new(),
            doc: Document::new(320, 240, "Test"),
            history: History::new(),
            screen: egui::vec2(1400.0, 900.0),
        }
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

    fn rect(&mut self, id: egui::Id) -> egui::Rect {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect
    }

    /// Lay out, then press and release inside the widget with `id`.
    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self.rect(id).center();
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
}

#[test]
fn one_click_on_the_up_chevron_moves_the_bottom_group_of_the_wide_column_one_slot() {
    let mut h = Harness::new();
    let side = DockSide::Right;
    let before = h.workspace.dock.panels_on(side);
    let groups = h.workspace.dock.groups_on(side);
    assert!(groups.len() >= 2, "the wide column holds {groups:?}");
    // The reorder control belongs to the ACTIVE tab of the bottom group.
    let from = groups.len() - 1;
    let bottom: Vec<PanelId> = groups[from].1.clone();
    let panel = bottom
        .iter()
        .copied()
        .find(|p| h.workspace.dock.is_active(*p))
        .expect("the bottom group shows a tab");
    // Essentials: that group is Layers/Channels/Paths, showing Layers.
    assert_eq!(
        bottom,
        vec![PanelId::Layers, PanelId::Channels, PanelId::Paths]
    );
    assert_eq!(panel, PanelId::Layers);

    h.click(ids::panel_menu(panel));
    let intents = h.click(ids::panel_reorder(panel, true));
    // The intent carries the group's new stack index — absolute, idempotent.
    let to = u8::try_from(from - 1).unwrap();
    assert!(
        intents.contains(&Intent::ReorderPanel { panel, to }),
        "reordering emitted {intents:?}"
    );

    // Groups travel whole: the bottom group now sits where the group above it
    // began, and that group follows it down, untouched inside.
    let after = h.workspace.dock.panels_on(side);
    let above: Vec<PanelId> = groups[from - 1].1.clone();
    let at = before.iter().position(|p| *p == above[0]).unwrap();
    let mut expected: Vec<PanelId> = before[..at].to_vec();
    expected.extend(bottom.iter().copied());
    expected.extend(above.iter().copied());
    assert_eq!(after, expected);
    assert_eq!(after.len(), before.len());
    assert_eq!(after.iter().position(|q| *q == panel), Some(at));
    let after_groups = h.workspace.dock.groups_on(side);
    assert_eq!(after_groups[from - 1].1, bottom);
    assert_eq!(after_groups[from].1, above);
    assert!(
        h.workspace.dock.is_active(panel),
        "the moved group keeps its tab"
    );
}
