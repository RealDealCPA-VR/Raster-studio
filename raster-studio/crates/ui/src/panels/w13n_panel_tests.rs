//! W13-N: the Styles, Document Info and Guide Guy panels driven through the
//! real workspace: each is reached from the Window menu's own row (every
//! `PanelId::ALL` member is one), opened in the dock, drawn in a headless
//! egui frame, and clicked with real pointer events; what a click asks for
//! is read off the workspace's outbox, exactly as the application drains it.

use editor_core::{Command, Document, History};
use layer_model::{Layer, LayerEffects, ShadowEffect};

use crate::dock::{LayoutId, PanelId};
use crate::menu::MenuAction;
use crate::panels::{doc_info, guide_guy, styles};
use crate::{Intent, Resolution, Workspace};

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
        Self {
            ctx,
            workspace,
            doc,
            history: History::new(),
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> (egui::FullOutput, Vec<Intent>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let out = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        let intents = self.workspace.drain_intents();
        (out, intents)
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        self.ctx.read_response(id).map(|r| r.rect)
    }

    /// Click the centre of `id`; the intents that frame queued.
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
        let (_, intents) = self.frame(vec![
            egui::Event::PointerMoved(at),
            press(true),
            press(false),
        ]);
        intents
    }

    /// Window > <panel>: the row is in the real Window menu, and its
    /// resolution opens the panel in the dock.
    fn open_from_the_window_menu(&mut self, panel: PanelId) {
        let window = crate::menu::menu_bar(0)
            .into_iter()
            .find(|m| m.title == "Window")
            .expect("a Window menu");
        let row = MenuAction::TogglePanel(panel);
        assert!(
            window.actions().contains(&row),
            "Window > {panel:?} is a row"
        );
        assert_eq!(row.label(), panel.title());
        let ctx = self.workspace.menu_context(&self.doc, &self.history);
        let Resolution::Enabled(intent) = row.resolve(&ctx) else {
            panic!("Window > {panel:?} is disabled");
        };
        self.workspace.absorb(&intent);
        assert!(self.workspace.dock.is_open(panel));
        self.workspace.dock.raise(panel);
    }

    fn add(&mut self, layer: Layer) -> layer_model::LayerId {
        let id = layer.id;
        self.history
            .apply(&mut self.doc, Command::create_layer(layer))
            .unwrap();
        self.doc.set_active_layer(Some(id)).unwrap();
        id
    }
}

#[test]
fn the_three_panels_are_appended_to_the_window_list() {
    let n = PanelId::ALL.len();
    assert_eq!(
        &PanelId::ALL[n - 3..],
        &[PanelId::Styles, PanelId::DocumentInfo, PanelId::GuideGuy],
        "appended last: a saved dock stores placements by position"
    );
    for panel in [PanelId::Styles, PanelId::DocumentInfo, PanelId::GuideGuy] {
        assert_eq!(
            PanelId::ALL
                .iter()
                .filter(|p| p.key() == panel.key())
                .count(),
            1,
            "{panel:?}'s key is its own"
        );
    }
}

/// Window > Styles: a click on a swatch asks to apply that style; before the
/// row is taken nothing of the panel is drawn.
#[test]
fn window_styles_lists_the_styles_and_a_click_applies_one() {
    let mut h = Harness::new(Document::new(400, 300, "styles"));
    h.add(Layer::raster("Art"));
    let shadow = LayerEffects {
        drop_shadow: Some(ShadowEffect::default()),
        ..LayerEffects::default()
    };
    styles::StylesView {
        styles: vec![
            ("Plain".into(), LayerEffects::default()),
            ("Shadow".into(), shadow),
        ],
    }
    .publish(&h.ctx);
    assert!(
        h.drawn(styles::ids::tile(1)).is_none(),
        "a closed panel draws nothing"
    );

    h.open_from_the_window_menu(PanelId::Styles);
    // The frame data the application refreshes each frame.
    let intents = h.click(styles::ids::tile(1));
    assert!(
        intents.contains(&Intent::Action(MenuAction::ApplyStyleAt(1))),
        "the click asks for style 1: {intents:?}"
    );
    assert!(
        h.drawn(styles::ids::tile(0)).is_some(),
        "every style is a swatch"
    );
}

/// The footer's New Style saves the active layer's style — live only when
/// the layer has one.
#[test]
fn new_style_saves_the_active_layers_style() {
    let mut h = Harness::new(Document::new(400, 300, "styles"));
    let mut layer = Layer::raster("Styled");
    layer.effects.drop_shadow = Some(ShadowEffect::default());
    h.add(layer);
    h.open_from_the_window_menu(PanelId::Styles);
    let intents = h.click(styles::ids::new());
    assert!(
        intents.contains(&Intent::Action(MenuAction::DefineStylePreset)),
        "{intents:?}"
    );
    h.add(Layer::raster("Plain"));
    let intents = h.click(styles::ids::new());
    assert!(
        !intents.contains(&Intent::Action(MenuAction::DefineStylePreset)),
        "a layer with no style has nothing to save"
    );
}

/// Window > Document Info draws the document's facts.
#[test]
fn window_document_info_shows_the_documents_facts() {
    let mut doc = Document::new(640, 480, "facts");
    doc.meta.bit_depth = 16;
    let mut h = Harness::new(doc);
    h.add(Layer::raster("A"));
    h.add(Layer::raster("B"));
    assert!(h.drawn(doc_info::ids::row(0)).is_none());
    h.open_from_the_window_menu(PanelId::DocumentInfo);
    assert!(
        h.drawn(doc_info::ids::row(0)).is_some(),
        "the size row is drawn"
    );
    let (out, _) = h.frame(Vec::new());
    let painted: Vec<String> = out
        .shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Text(t) => Some(t.galley.text().to_string()),
            _ => None,
        })
        .collect();
    let info = doc_info::DocInfo::of(&h.doc, h.workspace.canvas.resolution_ppi);
    for (_, value) in info.rows() {
        assert!(
            painted.contains(&value),
            "{value:?} is not painted: {painted:?}"
        );
    }
    assert!(painted.contains(&"640 x 480 px".to_string()));
    assert!(painted.contains(&"2".to_string()), "two layers");
}

/// Window > Guide Guy: Apply lays the previewed guides out as ONE SetGuides.
#[test]
fn window_guide_guy_applies_its_layout_as_one_command() {
    let mut h = Harness::new(Document::new(1000, 500, "guides"));
    h.open_from_the_window_menu(PanelId::GuideGuy);
    assert!(
        h.drawn(guide_guy::ids::preview()).is_some(),
        "the preview is drawn"
    );
    let spec = guide_guy::spec(&h.ctx);
    let expected = spec.command(&h.doc).expect("the default lays guides out");
    let intents = h.click(guide_guy::ids::apply());
    let applied: Vec<&Intent> = intents
        .iter()
        .filter(|i| matches!(i, Intent::Document(Command::SetGuides { .. })))
        .collect();
    assert_eq!(applied, vec![&Intent::Document(expected.clone())]);
    // Applied through history it is one undo step.
    let Intent::Document(command) = applied[0].clone() else {
        unreachable!()
    };
    h.history.apply(&mut h.doc, command).unwrap();
    assert_eq!(
        h.doc.guides.list.len(),
        10,
        "7 vertical and 3 horizontal guides"
    );
    h.history.undo(&mut h.doc).unwrap();
    assert!(h.doc.guides.list.is_empty());
}
