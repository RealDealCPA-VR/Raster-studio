//! W11-I: the CSS panel (Photopea's Window > CSS), driven through the real
//! workspace: the panel is reached from the Window menu's list (every
//! `PanelId::ALL` member is a Window-menu row), opened in the dock, drawn in
//! a headless egui frame, and its Copy button clicked with real pointer
//! events. What reaches the clipboard is the frame's own platform output.

use editor_core::{Command, Document, History};
use layer_model::fill::{FillLayer, FillSource, GradientFill};
use layer_model::text::{Alignment, Weight};
use layer_model::{
    Gradient, GradientStop, GradientStyle, Layer, LayerId, LayerKind, ShadowEffect, ShapeLayer,
    TextLayer,
};
use ui::dock::{LayoutId, PanelId};
use ui::panels::css::{self, ids};
use ui::panels::properties::RasterInks;
use ui::Workspace;

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
        workspace.dock.set_open(PanelId::Css, true);
        workspace.dock.raise(PanelId::Css);
        Self {
            ctx,
            workspace,
            doc,
            history: History::new(),
        }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> egui::FullOutput {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, SCREEN)),
            events,
            ..Default::default()
        };
        let out = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        let _ = self.workspace.drain_intents();
        out
    }

    fn drawn(&mut self, id: egui::Id) -> Option<egui::Rect> {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        self.ctx.read_response(id).map(|r| r.rect)
    }

    /// Click the Copy button; the text the frame put on the clipboard.
    fn copy(&mut self) -> String {
        let at = self
            .drawn(ids::copy())
            .expect("the CSS panel's Copy button was not drawn")
            .center();
        let out = self.frame(vec![
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
        out.platform_output.copied_text
    }

    fn add(&mut self, layer: Layer) -> LayerId {
        let id = layer.id;
        self.history
            .apply(&mut self.doc, Command::create_layer(layer))
            .unwrap();
        self.doc.set_active_layer(Some(id)).unwrap();
        id
    }
}

fn rounded_rect_svg(x: f64, y: f64, w: f64, h: f64, r: f64) -> String {
    let b = vector::Bounds::from_xywh(x, y, w, h);
    vector::to_svg(&vector::shapes::rounded_rect(
        b,
        vector::CornerRadii::uniform(r),
    ))
}

/// Window > CSS: the row is in the real Window menu, its resolution opens
/// the panel in the dock, and the next frame draws the panel's body (its
/// Copy button); before the row is taken, nothing of the panel is drawn.
#[test]
fn window_css_opens_the_panel_and_draws_its_body() {
    let mut h = Harness::new(Document::new(400, 300, "web"));
    h.add(Layer::with_kind(
        "Box",
        LayerKind::Shape(ShapeLayer {
            fill: Some([0.0, 0.5, 1.0, 1.0]),
            ..ShapeLayer::from_svg(rounded_rect_svg(10.0, 10.0, 50.0, 20.0, 4.0))
        }),
    ));
    h.workspace.dock.set_open(PanelId::Css, false);
    assert!(
        h.drawn(ids::copy()).is_none(),
        "a closed CSS panel draws nothing"
    );

    // Appended after the W10-B panels (W13-N's panels follow it): a saved
    // dock stores placements by position, so its place never moves.
    let at = PanelId::ALL.iter().position(|p| *p == PanelId::Css);
    assert_eq!(
        at.map(|i| PanelId::ALL[i - 1]),
        Some(PanelId::ParagraphStyles),
        "appended after Paragraph Styles"
    );
    let window = ui::menu::menu_bar(0)
        .into_iter()
        .find(|m| m.title == "Window")
        .expect("a Window menu");
    let row = ui::MenuAction::TogglePanel(PanelId::Css);
    assert!(window.actions().contains(&row), "Window > CSS is a row");
    assert_eq!(row.label(), "CSS");
    let ctx = h.workspace.menu_context(&h.doc, &h.history);
    let ui::Resolution::Enabled(intent) = row.resolve(&ctx) else {
        panic!("Window > CSS is disabled");
    };
    h.workspace.absorb(&intent);
    assert!(h.workspace.dock.is_open(PanelId::Css));
    assert!(
        h.drawn(ids::copy()).is_some(),
        "the opened CSS panel draws its body"
    );
}

#[test]
fn a_rounded_shape_with_a_drop_shadow_copies_its_box_radius_colour_and_shadow() {
    let mut h = Harness::new(Document::new(400, 300, "web"));
    let mut shape = Layer::with_kind(
        "Button",
        LayerKind::Shape(ShapeLayer {
            fill: Some([1.0, 0.0, 0.0, 1.0]),
            ..ShapeLayer::from_svg(rounded_rect_svg(20.0, 30.0, 120.0, 40.0, 8.0))
        }),
    );
    shape.opacity = 0.5;
    shape.effects.drop_shadow = Some(ShadowEffect {
        color: [0.0, 0.0, 0.0, 1.0],
        opacity: 0.5,
        angle_deg: 90.0,
        use_global_light: false,
        distance_px: 4.0,
        spread: 0.0,
        size_px: 6.0,
        ..ShadowEffect::default()
    });
    let id = h.add(shape);

    let copied = h.copy();
    let expected = css::layer_css(&h.doc, id, &RasterInks::default()).unwrap();
    assert_eq!(
        copied, expected,
        "Copy puts exactly the panel's CSS on the clipboard"
    );
    for want in [
        "position: absolute;",
        "left: 20px;",
        "top: 30px;",
        "width: 120px;",
        "height: 40px;",
        "opacity: 0.5;",
        "background-color: #ff0000;",
        "border-radius: 8px;",
        // Light from straight above (90 degrees): the shadow falls 4px down.
        "box-shadow: 0px 4px 6px 0px rgba(0, 0, 0, 0.5);",
    ] {
        assert!(copied.contains(want), "missing {want:?} in:\n{copied}");
    }
    assert!(h.drawn(ids::code()).is_some(), "the code block is drawn");
}

#[test]
fn a_gradient_fill_layer_and_a_text_layer_give_background_and_font_css() {
    let mut h = Harness::new(Document::new(400, 300, "web"));
    let fill = Layer::with_kind(
        "Sky",
        LayerKind::Fill(FillLayer::new(FillSource::Gradient(GradientFill {
            gradient: Gradient {
                stops: vec![
                    GradientStop {
                        position: 0.0,
                        color: [0.0, 0.0, 1.0, 1.0],
                        midpoint: 0.5,
                    },
                    GradientStop {
                        position: 1.0,
                        color: [1.0, 1.0, 1.0, 1.0],
                        midpoint: 0.5,
                    },
                ],
                ..Gradient::default()
            },
            style: GradientStyle::Linear,
            angle_deg: 0.0,
            ..GradientFill::default()
        }))),
    );
    h.add(fill);
    let copied = h.copy();
    assert!(
        copied.contains("background: linear-gradient(90deg, #0000ff 0%, #ffffff 100%);"),
        "{copied}"
    );

    let mut text = TextLayer {
        text: "Hello".into(),
        font_family: "Inter".into(),
        size_px: 24.0,
        ..TextLayer::default()
    };
    text.style.weight = Weight::BOLD;
    text.style.fill = [1.0, 1.0, 1.0, 1.0];
    text.paragraph.alignment = Alignment::Center;
    h.add(Layer::with_kind("Title", LayerKind::Text(text)));
    let copied = h.copy();
    for want in [
        "color: #ffffff;",
        "font-family: \"Inter\";",
        "font-size: 24px;",
        "font-weight: 700;",
        "text-align: center;",
    ] {
        assert!(copied.contains(want), "missing {want:?} in:\n{copied}");
    }
}
