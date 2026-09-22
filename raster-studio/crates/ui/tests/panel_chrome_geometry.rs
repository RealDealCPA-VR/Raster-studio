//! The panel chrome, measured off the frame rather than trusted.
//!
//! W0-E: every working button in the Layers footer, the kind-filter row, the
//! panel headers and the lock row was drawn in `TextRole::Disabled`, because
//! each was an `icon_toggle` with `on = false`; panel glyphs came out at 8pt
//! inside a 16pt target; and a History row's highlight and click target were
//! the width of its label. None of that is visible to a click test — a grey
//! button still emits its command — so these tests read the *painted shapes*
//! and the *interaction rects* egui reports for one real frame of the
//! workspace, with the same document shape the wiring tests use.

use editor_core::{Document, History};
use layer_model::Layer;
use ui::dock::{LayoutId, PanelId};
use ui::view::ids;
use ui::Workspace;

struct Harness {
    ctx: egui::Context,
    workspace: Workspace,
    doc: Document,
    history: History,
    screen: egui::Vec2,
}

impl Harness {
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

    /// Show exactly one panel, as `wired_controls` does.
    fn only(&mut self, panel: PanelId) {
        self.workspace.dock.apply_layout(LayoutId::Minimal);
        self.workspace.dock.set_open(panel, true);
    }

    /// One quiet frame; the shapes it painted come back untessellated.
    fn frame(&mut self) -> Vec<egui::epaint::ClippedShape> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, self.screen)),
            ..Default::default()
        };
        let output = self.ctx.run(input, |ctx| {
            self.workspace.ui(ctx, &self.doc, &self.history);
        });
        let _ = self.workspace.drain_intents();
        output.shapes
    }

    /// Scroll bars appear on the frame after their content overflows, so the
    /// layout is read from the third quiet frame.
    fn settle(&mut self) -> Vec<egui::epaint::ClippedShape> {
        self.frame();
        self.frame();
        self.frame()
    }

    fn rect(&mut self, id: egui::Id) -> egui::Rect {
        self.ctx
            .read_response(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .rect
    }

    fn tokens(&self) -> &'static design::Tokens {
        design::current_theme(&self.ctx).tokens()
    }
}

/// A document with one raster layer, active — the state that arms the footer.
fn one_layer_document() -> Document {
    let mut doc = Document::new(320, 240, "Test");
    let id = doc.layers.push_root(Layer::raster("Base")).unwrap();
    doc.set_active_layer(Some(id)).unwrap();
    doc
}

/// A document with `edits` applied, so the History panel has rows.
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

/// Every solid colour a shape lays down, strokes and fills alike, recursing
/// into `Shape::Vec`. Text is skipped: the footer paints no text and a label's
/// colour is not what these tests are about.
fn ink_colours(shape: &egui::Shape, out: &mut Vec<(egui::Rect, egui::Color32)>) {
    use egui::epaint::ColorMode;
    use egui::Shape;
    let bounds = shape.visual_bounding_rect();
    let mut push = |c: egui::Color32| {
        if c.a() > 0 {
            out.push((bounds, c));
        }
    };
    match shape {
        Shape::Vec(inner) => {
            for s in inner {
                ink_colours(s, out);
            }
        }
        Shape::LineSegment { stroke, .. } => {
            if let ColorMode::Solid(c) = stroke.color {
                push(c);
            }
        }
        Shape::Path(path) => {
            push(path.fill);
            if let ColorMode::Solid(c) = path.stroke.color {
                push(c);
            }
        }
        Shape::Circle(circle) => {
            push(circle.fill);
            push(circle.stroke.color);
        }
        Shape::Rect(rect) => {
            push(rect.fill);
            push(rect.stroke.color);
        }
        _ => {}
    }
}

/// The colours painted inside `rect` on this frame.
fn colours_inside(shapes: &[egui::epaint::ClippedShape], rect: egui::Rect) -> Vec<egui::Color32> {
    let mut inks = Vec::new();
    for clipped in shapes {
        ink_colours(&clipped.shape, &mut inks);
    }
    // A hairline of slack: a stroke inset by half its width still counts as
    // inside the button it was painted for.
    let inside = rect.expand(1.0);
    inks.into_iter()
        .filter(|(bounds, _)| inside.contains_rect(*bounds))
        .map(|(_, c)| c)
        .collect()
}

/// The Layers footer's icon buttons, in the order Photopea draws them.
fn footer_icons() -> [(&'static str, egui::Id); 6] {
    [
        ("link", ids::layer_link()),
        ("mask", ids::layer_mask()),
        ("adjustment", ids::layer_adjustment()),
        ("group", ids::new_group()),
        ("new layer", ids::new_layer()),
        ("delete", ids::layer_delete()),
    ]
}

#[test]
fn the_layers_footer_actions_are_not_painted_in_the_disabled_colour() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Layers);
    let shapes = h.settle();
    let t = h.tokens();
    let disabled = design::color32(t.palette.text(design::TextRole::Disabled));
    let secondary = design::color32(t.palette.text(design::TextRole::Secondary));
    let primary = design::color32(t.palette.text(design::TextRole::Primary));
    assert_ne!(
        disabled, secondary,
        "the palette must tell an idle glyph from a disabled one for this to prove anything"
    );

    for (name, id) in footer_icons() {
        let rect = h.rect(id);
        let inks = colours_inside(&shapes, rect);
        assert!(
            !inks.is_empty(),
            "the {name} button painted nothing inside {rect:?}; the scan is not reaching it"
        );
        assert!(
            !inks.contains(&disabled),
            "the {name} button works but is painted in the disabled colour: {inks:?}"
        );
        assert!(
            inks.iter().any(|c| *c == secondary || *c == primary),
            "the {name} button's glyph is in neither the idle nor the active text colour: {inks:?}"
        );
    }
}

#[test]
fn the_layers_footer_runs_link_fx_mask_adjustment_group_new_and_delete_at_the_right() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Layers);
    h.settle();

    let order = [
        ("link", ids::layer_link()),
        ("fx", ids::layer_fx()),
        ("mask", ids::layer_mask()),
        ("adjustment", ids::layer_adjustment()),
        ("group", ids::new_group()),
        ("new layer", ids::new_layer()),
        ("delete", ids::layer_delete()),
    ];
    let rects: Vec<(&str, egui::Rect)> = order.iter().map(|(n, id)| (*n, h.rect(*id))).collect();
    for pair in rects.windows(2) {
        let (left_name, left) = pair[0];
        let (right_name, right) = pair[1];
        assert!(
            left.center().x < right.center().x,
            "{left_name} ({:?}) should be left of {right_name} ({:?})",
            left.center(),
            right.center()
        );
        assert!(
            (left.center().y - right.center().y).abs() < left.height() * 0.5,
            "{left_name} and {right_name} are not on one row"
        );
    }
    // Delete stands alone at the right-hand end: the gap between it and the
    // new-layer button is wider than a button, not the hairline between
    // neighbours.
    let (_, new_layer) = rects[5];
    let (_, delete) = rects[6];
    assert!(
        delete.left() - new_layer.right() > delete.width(),
        "delete ({delete:?}) is packed against new layer ({new_layer:?}) rather than right-aligned"
    );
}

#[test]
fn panel_icon_buttons_are_a_full_control_tall_not_a_minimum_hit_target() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Layers);
    h.settle();
    let t = h.tokens();
    assert!(
        t.metrics.control_height > t.metrics.min_hit_target,
        "the two metrics must differ for this to prove anything"
    );

    let mut checked = Vec::new();
    for (name, id) in footer_icons() {
        checked.push((name, h.rect(id)));
    }
    checked.push(("filter: all", h.rect(ids::layer_filter_all())));
    checked.push((
        "filter: text",
        h.rect(ids::layer_filter(ui::menu::LayerClass::Text)),
    ));
    checked.push((
        "lock: pixels",
        h.rect(ids::layer_lock(ui::view::LockToggle::Pixels)),
    ));
    checked.push(("header overflow", h.rect(ids::panel_menu(PanelId::Layers))));
    for (name, rect) in checked {
        assert!(
            rect.width() >= t.metrics.control_height && rect.height() >= t.metrics.control_height,
            "{name} is {}x{}, smaller than one control ({})",
            rect.width(),
            rect.height(),
            t.metrics.control_height
        );
    }
}

#[test]
fn the_kind_filter_row_sits_above_the_blend_block_and_the_footer_below_the_rows() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Layers);
    h.settle();

    let filter = h.rect(ids::layer_filter_all());
    let blend = h.rect(ids::layer_blend());
    let opacity = h.rect(ids::layer_opacity());
    let lock = h.rect(ids::layer_lock(ui::view::LockToggle::All));
    let fill = h.rect(ids::layer_fill());
    let footer = h.rect(ids::new_layer());

    assert!(
        filter.bottom() <= blend.top(),
        "the filter row ({filter:?}) is not above the blend combo ({blend:?})"
    );
    // Row one: blend and opacity share a line; row two: locks and fill.
    assert!(
        (blend.center().y - opacity.center().y).abs() < blend.height(),
        "blend ({blend:?}) and opacity ({opacity:?}) are not on one row"
    );
    assert!(
        (lock.center().y - fill.center().y).abs() < lock.height(),
        "lock ({lock:?}) and fill ({fill:?}) are not on one row"
    );
    assert!(
        blend.bottom() <= lock.top(),
        "the lock row ({lock:?}) is not under the blend row ({blend:?})"
    );
    assert!(
        fill.bottom() <= footer.top(),
        "the footer ({footer:?}) is not below the blend block ({fill:?})"
    );
}

#[test]
fn a_history_rows_click_target_spans_the_panel_not_the_label() {
    let (doc, history) = edited_document(3);
    let mut h = Harness::with_document(doc);
    h.history = history;
    h.only(PanelId::History);
    h.settle();
    let t = h.tokens();

    // The header's tab sits at the panel's left edge and its overflow button
    // at the right, so together they measure the panel's width from the
    // frame itself rather than from a constant.
    let tab = h.rect(ids::panel_tab(PanelId::History));
    let menu = h.rect(ids::panel_menu(PanelId::History));
    let panel_width = menu.right() - tab.left();

    for index in 0..=3 {
        let row = h.rect(ids::history_row(index));
        assert_eq!(
            row.height(),
            t.metrics.list_row_height,
            "row {index} is not one list row tall: {row:?}"
        );
        assert!(
            row.left() <= tab.left(),
            "row {index} ({row:?}) starts right of the panel's tab ({tab:?})"
        );
        assert!(
            row.right() >= menu.left(),
            "row {index} ({row:?}) stops short of the panel's right edge ({menu:?}): \
             the click target is the label, not the row"
        );
        assert!(
            row.width() > panel_width * 0.8,
            "row {index} is {} wide in a {panel_width} panel",
            row.width()
        );
    }
}

#[test]
fn the_history_snapshot_control_is_a_footer_action_under_the_last_row() {
    let (doc, history) = edited_document(2);
    let mut h = Harness::with_document(doc);
    h.history = history;
    h.only(PanelId::History);
    h.settle();
    let t = h.tokens();

    let last_row = h.rect(ids::history_row(2));
    let snapshot = h.rect(ids::history_new_snapshot());
    assert!(
        snapshot.top() >= last_row.bottom(),
        "the snapshot action ({snapshot:?}) is not under the last row ({last_row:?})"
    );
    // An icon action, one control square — not a text button the width of a
    // word that reads as one more history row.
    assert!(
        (snapshot.width() - t.metrics.control_height).abs() < 0.5
            && (snapshot.height() - t.metrics.control_height).abs() < 0.5,
        "the snapshot control is {}x{}, not a {} square",
        snapshot.width(),
        snapshot.height(),
        t.metrics.control_height
    );
    assert!(
        snapshot.width() < last_row.width() * 0.5,
        "the snapshot control ({snapshot:?}) is as wide as a row ({last_row:?})"
    );
}
