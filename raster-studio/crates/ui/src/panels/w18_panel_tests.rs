//! W18-I: the Channels, Layer Comps and Memory panels driven through real
//! frames of the workspace: each control is found on screen by its stable
//! id and clicked, double-clicked or typed into with real events, and the
//! document commands the frame raised are applied through history, as the
//! application applies them.

use editor_core::{Command, Document, History, Selection};
use layer_model::Layer;

use crate::dock::{LayoutId, PanelId};
use crate::menu::{MenuAction, PurgeTarget};
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
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
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

    fn frames(&mut self, n: usize) -> Vec<Intent> {
        let mut out = Vec::new();
        for _ in 0..n {
            out.extend(self.frame(Vec::new()));
        }
        out
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        self.frames(3);
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn at(&mut self, id: egui::Id) -> egui::Pos2 {
        self.drawn(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .center()
    }

    fn press_release(at: egui::Pos2) -> Vec<egui::Event> {
        let event = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        };
        vec![egui::Event::PointerMoved(at), event(true), event(false)]
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self.at(id);
        self.frame(Self::press_release(at))
    }

    /// Two clicks one frame apart: egui's double-click.
    fn double_click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self.at(id);
        let mut out = self.frame(Self::press_release(at));
        out.extend(self.frame(Self::press_release(at)));
        out
    }

    fn key(&mut self, key: egui::Key, modifiers: egui::Modifiers) -> Vec<Intent> {
        self.frame(vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }])
    }

    /// Replace the focused field's text with `text`.
    fn retype(&mut self, text: &str) -> Vec<Intent> {
        let mut out = self.key(egui::Key::A, egui::Modifiers::COMMAND);
        out.extend(self.frame(vec![egui::Event::Text(text.to_string())]));
        out
    }
}

fn documents(intents: &[Intent]) -> usize {
    intents.iter().filter(|i| i.as_command().is_some()).count()
}

// ---------------------------------------------------------------------------
// Layer Comps: rename in place
// ---------------------------------------------------------------------------

fn one_layer() -> Document {
    let mut doc = Document::new(32, 32, "comps");
    let layer = Layer::raster("A");
    let id = layer.id;
    Command::create_layer(layer).apply(&mut doc).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    doc
}

/// A double-click on a comp's row opens its name as a field; Enter renames
/// the comp (one undo step, undone by undo) and closes the field.
#[test]
fn a_double_clicked_layer_comp_is_renamed_in_place() {
    use crate::panels::layer_comps::ids;
    let mut h = Harness::new(one_layer(), PanelId::LayerComps);
    let _ = h.click(ids::new());
    assert_eq!(h.doc.extras.layer_comps[0].name, "Layer Comp 1");
    assert!(h.drawn(ids::rename(0)).is_none(), "no field before");
    // Past egui's double-click window, so New's click is not the first.
    let _ = h.frames(30);

    let _ = h.double_click(ids::row(0));
    assert!(h.drawn(ids::rename(0)).is_some(), "the name became a field");
    let depth = h.history.undo_depth();
    let typed = h.retype("Hero");
    assert_eq!(documents(&typed), 0, "typing is not an edit");
    let committed = h.key(egui::Key::Enter, egui::Modifiers::default());
    assert_eq!(documents(&committed), 1, "{committed:?}");
    assert_eq!(h.doc.extras.layer_comps[0].name, "Hero");
    assert_eq!(h.history.undo_depth(), depth + 1, "one undo step");
    assert!(h.drawn(ids::rename(0)).is_none(), "the field closed");

    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.extras.layer_comps[0].name, "Layer Comp 1");
}

/// A click on New and, straight after, one on the comp's row is egui's
/// "double-click" (any second click in its window), but the two clicks hit
/// different controls: the row click applies the comp and opens no field.
#[test]
fn a_click_elsewhere_then_on_a_comp_applies_it_and_opens_no_field() {
    use crate::panels::layer_comps::ids;
    let mut h = Harness::new(one_layer(), PanelId::LayerComps);
    let _ = h.click(ids::new());
    let _ = h.frames(30);
    let new_at = h.at(ids::new());
    let row_at = h.at(ids::row(0));
    // The second New records "Layer Comp 2" and marks it applied.
    let _ = h.frame(Harness::press_release(new_at));
    assert_eq!(h.doc.extras.layer_comps.len(), 2);
    assert_ne!(h.doc.extras.last_comp, Some(0));
    let applied = h.frame(Harness::press_release(row_at));
    assert_eq!(h.doc.extras.last_comp, Some(0), "applied: {applied:?}");
    assert!(h.drawn(ids::rename(0)).is_none(), "no rename field");
}

/// Escape throws the typed name away.
#[test]
fn escape_cancels_a_layer_comp_rename() {
    use crate::panels::layer_comps::ids;
    let mut h = Harness::new(one_layer(), PanelId::LayerComps);
    let _ = h.click(ids::new());
    let _ = h.frames(30);
    let _ = h.double_click(ids::row(0));
    let _ = h.retype("Nope");
    let out = h.key(egui::Key::Escape, egui::Modifiers::default());
    assert_eq!(documents(&out), 0, "{out:?}");
    assert_eq!(h.doc.extras.layer_comps[0].name, "Layer Comp 1");
    assert!(h.drawn(ids::rename(0)).is_none(), "the field closed");
}

// ---------------------------------------------------------------------------
// Channels: rename an alpha channel in place
// ---------------------------------------------------------------------------

fn with_alpha() -> Document {
    let mut doc = one_layer();
    doc.saved_selections.push((
        "Alpha 1".to_string(),
        Selection::Rect {
            min: glam::IVec2::new(2, 2),
            max: glam::IVec2::new(9, 9),
        },
    ));
    doc
}

/// Photopea: "double-click the name of an independent channel to rename
/// it". The saved selection keeps its coverage; the rename is one undo step.
#[test]
fn a_double_clicked_alpha_channel_name_is_renamed_in_place() {
    use crate::panels::channels::{alpha_name_id, alpha_rename_id};
    let mut h = Harness::new(with_alpha(), PanelId::Channels);
    let coverage = h.doc.saved_selections[0].1.clone();
    let opened = h.double_click(alpha_name_id(0));
    assert!(
        !opened.contains(&Intent::Action(MenuAction::LoadSelection)),
        "a double-click does not open Load Selection: {opened:?}"
    );
    assert!(
        h.drawn(alpha_rename_id(0)).is_some(),
        "the name became a field"
    );
    let settled = h.frames(30);
    assert!(
        !settled.contains(&Intent::Action(MenuAction::LoadSelection)),
        "{settled:?}"
    );
    let _ = h.retype("Mask of Sky");
    let committed = h.key(egui::Key::Enter, egui::Modifiers::default());
    assert_eq!(documents(&committed), 1, "{committed:?}");
    assert_eq!(h.doc.saved_selections[0].0, "Mask of Sky");
    assert_eq!(h.doc.saved_selections[0].1, coverage, "coverage kept");
    assert!(h.drawn(alpha_rename_id(0)).is_none(), "the field closed");
    h.history.undo(&mut h.doc).unwrap();
    assert_eq!(h.doc.saved_selections[0].0, "Alpha 1");
}

/// A click somewhere else and, straight after, one on the alpha name is
/// egui's "double-click", but not on the name: no field opens, and the name
/// click is the row's click (Load Selection) once the window passes.
#[test]
fn a_click_elsewhere_then_on_an_alpha_name_opens_no_field() {
    use crate::panels::channels::{alpha_name_id, alpha_rename_id};
    let mut h = Harness::new(with_alpha(), PanelId::Channels);
    let name = h.at(alpha_name_id(0));
    let elsewhere = egui::pos2(name.x, name.y - 60.0);
    let _ = h.frame(Harness::press_release(elsewhere));
    let mut out = h.frame(Harness::press_release(name));
    assert!(h.drawn(alpha_rename_id(0)).is_none(), "no rename field");
    out.extend(h.frames(40));
    assert!(h.drawn(alpha_rename_id(0)).is_none(), "no rename field");
    assert!(
        out.contains(&Intent::Action(MenuAction::LoadSelection)),
        "{out:?}"
    );
}

/// A single click on the name is still the row's click: once a
/// double-click can no longer follow, it asks for Load Selection on it.
#[test]
fn a_single_click_on_an_alpha_name_still_opens_load_selection() {
    use crate::panels::channels::{alpha_name_id, alpha_rename_id};
    let mut h = Harness::new(with_alpha(), PanelId::Channels);
    let mut out = h.click(alpha_name_id(0));
    out.extend(h.frames(40));
    assert_eq!(
        out.iter()
            .filter(|i| **i == Intent::Action(MenuAction::LoadSelection))
            .count(),
        1,
        "{out:?}"
    );
    assert_eq!(
        h.workspace.take_pending_selection_load(),
        Some(crate::SelectionLoadRequest {
            index: 0,
            direct: false
        })
    );
    assert!(h.drawn(alpha_rename_id(0)).is_none());
}

// ---------------------------------------------------------------------------
// Memory panel
// ---------------------------------------------------------------------------

/// Window > Memory is a row of the real Window menu; the panel shows the
/// image data, the history and the clipboard, and its Purge Histories
/// raises Edit > Purge > Histories once there is history to purge.
#[test]
fn the_memory_panel_shows_the_figures_and_purges_through_edit_purge() {
    use crate::menu::Resolution;
    use crate::panels::memory::ids;
    let window = crate::menu::menu_bar(0)
        .into_iter()
        .find(|m| m.title == "Window")
        .expect("a Window menu");
    assert!(window
        .actions()
        .contains(&MenuAction::TogglePanel(PanelId::Memory)));
    assert_eq!(PanelId::ALL.last(), Some(&PanelId::Memory), "appended last");

    let mut h = Harness::new(one_layer(), PanelId::LayerComps);
    let ctx = h.workspace.menu_context(&h.doc, &h.history);
    let Resolution::Enabled(open) = MenuAction::TogglePanel(PanelId::Memory).resolve(&ctx) else {
        panic!("Window > Memory is disabled");
    };
    h.workspace.absorb(&open);
    h.workspace.dock.raise(PanelId::Memory);
    for row in 0..3 {
        assert!(h.drawn(ids::row(row)).is_some(), "row {row} drawn");
    }
    // No history yet: Purge Histories is greyed and raises nothing.
    let out = h.click(ids::purge(PurgeTarget::Histories));
    assert!(out.is_empty(), "{out:?}");

    let layer = Layer::raster("B");
    h.history
        .apply(&mut h.doc, Command::create_layer(layer))
        .unwrap();
    let out = h.click(ids::purge(PurgeTarget::Histories));
    assert_eq!(
        out,
        vec![Intent::Action(MenuAction::Purge(PurgeTarget::Histories))]
    );
    let info = crate::panels::memory::MemoryInfo::of(&h.doc, &h.history, false);
    assert_eq!(info.undo_steps, 1);
    let rows = info.rows();
    assert!(rows[1].1.contains('1'), "{rows:?}");
}
