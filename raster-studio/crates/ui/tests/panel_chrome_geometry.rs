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

// ---------------------------------------------------------------------------
// W2-D: two right columns, the flexible Layers group, Histogram, Info sample
// ---------------------------------------------------------------------------

use ui::dock::{ids as dock_ids, DockSide};

/// Every string one frame painted.
fn painted_text(shapes: &[egui::epaint::ClippedShape]) -> Vec<String> {
    shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) => Some(text.galley.text().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn essentials_draws_a_narrow_right_column_inside_the_wide_one() {
    let mut h = Harness::with_document(one_layer_document());
    h.settle();
    let narrow = h.rect(dock_ids::column(DockSide::RightNarrow));
    let wide = h.rect(dock_ids::column(DockSide::Right));
    assert!(narrow.width() > 0.0 && wide.width() > 0.0);
    assert!(
        narrow.right() <= wide.left() + 1.0,
        "the narrow column ({narrow:?}) is not left of the wide one ({wide:?})"
    );
    assert!(
        narrow.width() < wide.width(),
        "the inner column ({}) is not the narrow one ({})",
        narrow.width(),
        wide.width()
    );
    // Both columns reach the same edges: they are two columns of one dock,
    // not a panel floating beside another.
    assert!((narrow.top() - wide.top()).abs() < 1.0);
    assert!((narrow.bottom() - wide.bottom()).abs() < 1.0);
    // The wide column keeps Layers; the narrow one holds the reporting and
    // colour panels, and its three-tab strip fits inside it.
    assert!(wide.contains_rect(h.rect(ids::panel_tab(PanelId::Layers))));
    for panel in [PanelId::Navigator, PanelId::Info, PanelId::Histogram] {
        let tab = h.rect(ids::panel_tab(panel));
        assert!(
            narrow.contains_rect(tab),
            "{panel:?}'s tab ({tab:?}) is outside the narrow column ({narrow:?})"
        );
    }
    let overflow = h.rect(ids::panel_menu(PanelId::Navigator));
    let last_tab = h.rect(ids::panel_tab(PanelId::Histogram));
    assert!(
        last_tab.right() <= overflow.left(),
        "the tab strip ({last_tab:?}) runs into the header buttons ({overflow:?})"
    );
    // Photopea's pairings: Colour is tabbed with Swatches in the narrow
    // column; Channels and Paths are tabbed with Layers in the wide one.
    for panel in [PanelId::Color, PanelId::Swatches] {
        let tab = h.rect(ids::panel_tab(panel));
        assert!(
            narrow.contains_rect(tab),
            "{panel:?}'s tab ({tab:?}) is outside the narrow column ({narrow:?})"
        );
    }
    for panel in [PanelId::Channels, PanelId::Paths] {
        let tab = h.rect(ids::panel_tab(panel));
        assert!(
            wide.contains_rect(tab),
            "{panel:?}'s tab ({tab:?}) is outside the wide column ({wide:?})"
        );
    }
    // The wide column stacks Properties over History over Layers.
    let properties = h.rect(dock_ids::group_of(PanelId::Properties));
    let history = h.rect(dock_ids::group_of(PanelId::History));
    let layers = h.rect(dock_ids::group_of(PanelId::Layers));
    assert!(
        properties.bottom() <= history.top() + 1.0 && history.bottom() <= layers.top() + 1.0,
        "the wide column is not Properties / History / Layers: {properties:?} {history:?} {layers:?}"
    );
}

#[test]
fn the_layers_group_takes_the_leftover_height_and_no_column_ends_in_dead_space() {
    // Photopea: the fixed groups keep their natural height and the Layers
    // group — the bottom of the wide column — gets whatever the window
    // leaves. So the Layers group's bottom edge *is* the column's bottom edge
    // at either window height, and 180pt more window is 180pt more Layers.
    let mut layers_heights = Vec::new();
    let mut fixed_heights = Vec::new();
    for height in [900.0_f32, 1080.0] {
        let mut h = Harness::with_document(one_layer_document());
        h.screen = egui::vec2(1400.0, height);
        h.settle();
        let t = h.tokens();
        let wide = h.rect(dock_ids::column(DockSide::Right));
        let layers = h.rect(dock_ids::group_of(PanelId::Layers));
        let history = h.rect(dock_ids::group_of(PanelId::History));
        let properties = h.rect(dock_ids::group_of(PanelId::Properties));
        assert!(
            (layers.bottom() - wide.bottom()).abs() <= 1.5,
            "at {height}pt the Layers group ends at {} but the column at {}: {} pt of dead space",
            layers.bottom(),
            wide.bottom(),
            wide.bottom() - layers.bottom()
        );
        // Stacked in order — Properties, History, Layers — each inside the
        // column, nothing overlapping.
        assert!(
            properties.bottom() <= history.top() + 1.0,
            "{properties:?} vs {history:?}"
        );
        assert!(
            history.bottom() <= layers.top() + 1.0,
            "{history:?} vs {layers:?}"
        );
        assert!(
            wide.contains_rect(layers.shrink(0.5)),
            "{layers:?} left {wide:?}"
        );
        // The footer is pinned to the group's bottom edge, not scrolled away:
        // inside the group, and within a control of its edge.
        let footer = h.rect(ids::new_layer());
        assert!(
            layers.contains_rect(footer),
            "footer {footer:?} left {layers:?}"
        );
        assert!(
            layers.bottom() - footer.bottom() <= t.metrics.control_height * 2.0,
            "the footer ({footer:?}) is not pinned to the Layers group's edge ({layers:?})"
        );
        layers_heights.push(layers.height());
        fixed_heights.push((properties.height(), history.height()));

        // The narrow column has no Layers, so its last group — Brushes —
        // stretches instead, and its list scrolls inside the stretch; the
        // group's bottom is the column's bottom (the fit itself is
        // `every_narrow_group_fits_inside_its_column_at_900_and_1080`).
        let narrow = h.rect(dock_ids::column(DockSide::RightNarrow));
        let last = h.rect(dock_ids::group_of(PanelId::Brushes));
        assert!(
            (narrow.bottom() - last.bottom()).abs() <= 1.5,
            "at {height}pt the narrow column ends at {} but its last group at {}: {} pt of dead space",
            narrow.bottom(),
            last.bottom(),
            narrow.bottom() - last.bottom()
        );
    }
    // 180pt more window is 180pt more Layers group; the fixed groups did not
    // move by a point.
    let grew = layers_heights[1] - layers_heights[0];
    assert!(
        (grew - 180.0).abs() <= 2.0,
        "the Layers group grew {grew}pt for a 180pt taller window: {layers_heights:?}"
    );
    assert_eq!(
        fixed_heights[0], fixed_heights[1],
        "a fixed group changed height"
    );
}

/// W2-X (d): the narrow column at 900pt used to end with the Brushes group
/// cut off — its preset list took its natural height under two fixed groups
/// and ran past the window, footer and all. The same rule as the wide column
/// applies now: the fixed groups keep their height, the last group takes the
/// rest and scrolls its list inside it. So every group of the narrow column
/// lies inside the column at both window heights, stacked in order with no
/// overlap, and the last one ends on the column's edge.
#[test]
fn every_narrow_group_fits_inside_its_column_at_900_and_1080() {
    let mut brushes_heights = Vec::new();
    for height in [900.0_f32, 1080.0] {
        let mut h = Harness::with_document(one_layer_document());
        h.screen = egui::vec2(1400.0, height);
        h.settle();
        let column = h.rect(dock_ids::column(DockSide::RightNarrow));
        let groups = h.workspace.dock.groups_on(DockSide::RightNarrow);
        assert_eq!(groups.len(), 3, "Essentials' narrow column: {groups:?}");
        let leads: Vec<PanelId> = groups.iter().map(|(_, m)| m[0]).collect();
        assert_eq!(
            leads,
            vec![PanelId::Navigator, PanelId::Color, PanelId::Brushes]
        );
        let rects: Vec<egui::Rect> = leads
            .iter()
            .map(|p| h.rect(dock_ids::group_of(*p)))
            .collect();
        for (panel, rect) in leads.iter().zip(&rects) {
            assert!(
                column.contains_rect(rect.shrink(0.5)),
                "at {height}pt the {panel:?} group ({rect:?}) is not inside the narrow column ({column:?})"
            );
            // The group's tab strip is inside its group, so a group that
            // scrolled its header away would fail here too.
            let tab = h.rect(ids::panel_tab(*panel));
            assert!(
                rect.contains_rect(tab.shrink(0.5)),
                "at {height}pt {panel:?}'s tab ({tab:?}) left its group ({rect:?})"
            );
        }
        for pair in rects.windows(2) {
            assert!(
                pair[0].bottom() <= pair[1].top() + 1.0,
                "at {height}pt the narrow groups overlap: {:?} over {:?}",
                pair[0],
                pair[1]
            );
        }
        let last = rects[2];
        assert!(
            (last.bottom() - column.bottom()).abs() <= 1.5,
            "at {height}pt the Brushes group ends at {} but the column at {}",
            last.bottom(),
            column.bottom()
        );
        brushes_heights.push(last.height());
    }
    // 180pt more window is 180pt more Brushes group: the stretch is the
    // list's, not the fixed groups'.
    let grew = brushes_heights[1] - brushes_heights[0];
    assert!(
        (grew - 180.0).abs() <= 2.0,
        "the Brushes group grew {grew}pt for a 180pt taller window: {brushes_heights:?}"
    );
}

/// The rectangles painted strictly inside `plot` that are not the well
/// itself: the histogram's bars.
fn bars_inside(shapes: &[egui::epaint::ClippedShape], plot: egui::Rect) -> usize {
    shapes
        .iter()
        .filter(|clipped| match &clipped.shape {
            egui::Shape::Rect(r) => {
                r.fill.a() > 0
                    && plot.expand(0.5).contains_rect(r.rect)
                    && r.rect.width() < plot.width() * 0.5
                    && r.rect.height() > 0.0
            }
            _ => false,
        })
        .count()
}

#[test]
fn the_histogram_draws_bars_for_the_composite_it_was_given() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Histogram);
    let shapes = h.settle();
    let plot = h.rect(dock_ids::histogram_plot());
    assert_eq!(
        bars_inside(&shapes, plot),
        0,
        "with no composite there is nothing to count, so nothing is drawn"
    );

    // A 4x4 composite: half mid-grey, half saturated red.
    let mut rgba: Vec<u8> = Vec::new();
    for i in 0..16 {
        rgba.extend_from_slice(if i % 2 == 0 {
            &[128, 128, 128, 255]
        } else {
            &[255, 0, 0, 255]
        });
    }
    h.workspace.set_composite_preview(&h.ctx, 1, 4, 4, &rgba);
    let shapes = h.settle();
    let plot = h.rect(dock_ids::histogram_plot());
    let bars = bars_inside(&shapes, plot);
    assert!(
        bars >= 1,
        "the histogram drew {bars} bars for a real composite"
    );
    // Every bar stands on the plot's floor.
    for clipped in &shapes {
        if let egui::Shape::Rect(r) = &clipped.shape {
            if r.rect.width() < plot.width() * 0.5 && plot.expand(0.5).contains_rect(r.rect) {
                assert!(
                    (r.rect.bottom() - plot.bottom()).abs() <= 1.5,
                    "{:?}",
                    r.rect
                );
            }
        }
    }
    // The same generation is not recounted; a new one is.
    assert_eq!(h.workspace.histogram.generation(), Some(1));
    h.workspace.set_composite_preview(&h.ctx, 2, 4, 4, &rgba);
    assert_eq!(h.workspace.histogram.generation(), Some(2));
}

/// W2-X (b): the curves are painted from the design tokens' data roles —
/// `ChannelRed` / `ChannelGreen` / `ChannelBlue` at the curve alpha — not
/// from literal primaries. The grey-and-red composite above fills bins in
/// all three channels, so all three tints, and nothing else, land on the
/// plot's floor.
#[test]
fn the_histogram_curves_are_painted_in_the_token_channel_colours() {
    use ui::panels::histogram::{channel_role, curve_alpha_byte};
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Histogram);
    let mut rgba: Vec<u8> = Vec::new();
    for i in 0..16 {
        rgba.extend_from_slice(if i % 2 == 0 {
            &[128, 128, 128, 255]
        } else {
            &[255, 0, 0, 255]
        });
    }
    h.workspace.set_composite_preview(&h.ctx, 1, 4, 4, &rgba);
    let shapes = h.settle();
    let plot = h.rect(dock_ids::histogram_plot());
    let t = h.tokens();
    let expected: std::collections::BTreeSet<[u8; 4]> = (0..3)
        .map(|i| {
            design::color32(
                t.palette
                    .color(channel_role(i))
                    .with_alpha(curve_alpha_byte()),
            )
            .to_array()
        })
        .collect();
    assert_eq!(expected.len(), 3, "three distinct channel tints");
    let painted: std::collections::BTreeSet<[u8; 4]> = shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Rect(r)
                if r.fill.a() > 0
                    && plot.expand(0.5).contains_rect(r.rect)
                    && r.rect.width() < plot.width() * 0.5
                    && r.rect.height() > 0.0 =>
            {
                Some(r.fill.to_array())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        painted, expected,
        "the bars are not painted in exactly the three token channel tints"
    );
    // A control on the pin itself: the literal primaries the panel used to
    // paint are not what the tokens say, in either theme.
    let literal_red = egui::Color32::from_rgba_unmultiplied(255, 0, 0, curve_alpha_byte());
    assert!(
        !painted.contains(&literal_red.to_array()),
        "the red curve is still the literal primary"
    );
}

/// W2-X (c): with a document open and no composite counted yet the panel
/// says it is waiting — the application hands the composite over after its
/// next frame — and only the no-document placeholder (the 0×0 document the
/// application draws the chrome against) says to open one.
#[test]
fn the_histogram_says_it_is_waiting_while_a_document_has_no_composite_yet() {
    let waiting = ui::strings::tr("ui.docks.histogram.waiting");
    let none = ui::strings::tr("ui.docks.histogram.no.composite");
    assert!(!waiting.is_empty() && !none.is_empty() && waiting != none);

    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Histogram);
    let text = painted_text(&h.settle());
    assert!(
        text.iter().any(|s| s == waiting),
        "a document is open but the panel does not say it is waiting: {text:?}"
    );
    assert!(
        !text.iter().any(|s| s == none),
        "a document is open but the panel says to open one: {text:?}"
    );

    let mut h = Harness::with_document(Document::new(0, 0, ""));
    h.only(PanelId::Histogram);
    let text = painted_text(&h.settle());
    assert!(
        text.iter().any(|s| s == none),
        "nothing is open but the panel does not say so: {text:?}"
    );
    assert!(!text.iter().any(|s| s == waiting), "{text:?}");
}

/// W2-X (b): the Channels panel's per-component thumbnails are tinted from
/// the same token roles the Histogram's curves use — the composite texture
/// is drawn once per component in `ChannelRed`, `ChannelGreen`, `ChannelBlue`.
#[test]
fn the_channel_thumbnails_are_tinted_in_the_token_channel_colours() {
    use ui::panels::histogram::channel_role;
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Channels);
    let rgba: Vec<u8> = std::iter::repeat_n([200u8, 100, 50, 255], 16)
        .flatten()
        .collect();
    h.workspace.set_composite_preview(&h.ctx, 1, 4, 4, &rgba);
    let shapes = h.settle();
    let t = h.tokens();
    // egui 0.29 draws an image as a textured mesh whose vertex colour is the
    // tint; the untextured default id is the font atlas, not a thumbnail.
    let tints: Vec<egui::Color32> = shapes
        .iter()
        .filter_map(|clipped| match &clipped.shape {
            egui::Shape::Mesh(mesh) if mesh.texture_id != egui::TextureId::default() => {
                mesh.vertices.first().map(|v| v.color)
            }
            _ => None,
        })
        .collect();
    assert!(
        !tints.is_empty(),
        "no thumbnail was drawn from the composite"
    );
    for i in 0..3 {
        let want = design::color32(t.palette.color(channel_role(i)));
        assert!(
            tints.contains(&want),
            "component {i}'s thumbnail is not tinted {want:?}: {tints:?}"
        );
    }
    let literal_red = egui::Color32::from_rgba_unmultiplied(255, 0, 0, 255);
    assert!(
        !tints.contains(&literal_red),
        "a thumbnail is still tinted the literal primary"
    );
}

#[test]
fn the_info_rows_show_the_sample_the_application_set() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Info);
    let shapes = h.settle();
    let before = painted_text(&shapes);
    assert!(
        !before.iter().any(|t| t == "255, 128, 0"),
        "the sample is on screen before it was set: {before:?}"
    );
    assert!(h.ctx.read_response(dock_ids::info_value("RGB")).is_some());

    h.workspace.set_info_sample(Some([1.0, 0.5, 0.0, 1.0]));
    let shapes = h.settle();
    let after = painted_text(&shapes);
    assert!(
        after.iter().any(|t| t == "255, 128, 0"),
        "the RGB row did not show the sample: {after:?}"
    );
    assert!(
        after.iter().any(|t| t == "#FF8000"),
        "the Hex row did not show the sample: {after:?}"
    );
    // The rows sit inside the panel: RGB above Hex, both under Pointer.
    let pointer = h.rect(dock_ids::info_value("Pointer"));
    let rgb = h.rect(dock_ids::info_value("RGB"));
    let hex = h.rect(dock_ids::info_value("Hex"));
    assert!(pointer.bottom() <= rgb.top() && rgb.bottom() <= hex.top());

    // Off the image, the rows go back to the dash rather than a stale colour.
    h.workspace.set_info_sample(None);
    let shapes = h.settle();
    assert!(!painted_text(&shapes).iter().any(|t| t == "#FF8000"));
}

#[test]
fn the_channels_footer_is_routed_or_greyed_never_silent() {
    let mut h = Harness::with_document(one_layer_document());
    h.only(PanelId::Channels);
    let shapes = h.settle();
    let t = h.tokens();
    let disabled = design::color32(t.palette.text(design::TextRole::Disabled));
    // No selection, no saved selection, no alpha store: all four are drawn,
    // and drawn *disabled* — a grey button with a reason, not a live no-op.
    for name in ["load", "save", "new", "delete"] {
        let rect = h.rect(dock_ids::channel_action(name));
        let inks = colours_inside(&shapes, rect);
        assert!(
            inks.contains(&disabled),
            "the {name} action is not painted disabled although nothing can act: {inks:?}"
        );
        let response = h
            .ctx
            .read_response(dock_ids::channel_action(name))
            .expect("drawn");
        assert!(
            !response.sense.click,
            "the {name} action senses clicks while it can do nothing"
        );
    }
    // The footer sits under the rows, inside the panel.
    let eye = h.rect(ids::channel_eye(1));
    let load = h.rect(dock_ids::channel_action("load"));
    assert!(load.top() >= eye.bottom(), "{load:?} vs {eye:?}");
}

/// Round 3: the Load action used to fire the Select menu's `LoadSelection`
/// whenever a mask row was selected and a selection had been saved — a button
/// labelled "Load channel as selection" that restored the last *saved*
/// selection instead. No intent loads a mask as the selection in this build,
/// so the button must be greyed with that reason even in the exact state that
/// used to make it live: a mask channel selected, saved selections present.
#[test]
fn the_channels_load_action_stays_greyed_when_a_saved_selection_would_have_made_it_live() {
    use layer_model::{LayerMask, MaskId};
    use ui::panels::channels::ChannelKind;

    let mut doc = one_layer_document();
    let layer = doc.layers.iter_depth_first()[0];
    let mask = MaskId::new();
    doc.layers.get_mut(layer).unwrap().mask = Some(LayerMask::new(mask));
    let mut h = Harness::with_document(doc);
    h.only(PanelId::Channels);
    h.workspace.channels.selected = ChannelKind::Mask { layer, mask };
    h.workspace.saved_selections = 2;
    // Control: in this state the menu's own Load Selection IS enabled, so a
    // routed button would have come out live.
    let context = h.workspace.menu_context(&h.doc, &h.history);
    assert!(matches!(
        ui::menu::MenuAction::LoadSelection.resolve(&context),
        ui::menu::Resolution::Enabled(_)
    ));

    let shapes = h.settle();
    let t = h.tokens();
    let disabled = design::color32(t.palette.text(design::TextRole::Disabled));
    let rect = h.rect(dock_ids::channel_action("load"));
    let inks = colours_inside(&shapes, rect);
    assert!(
        inks.contains(&disabled),
        "Load is painted live although nothing loads a mask as a selection: {inks:?}"
    );
    let response = h
        .ctx
        .read_response(dock_ids::channel_action("load"))
        .expect("drawn");
    assert!(
        !response.sense.click,
        "Load senses clicks: it would fire LoadSelection, not load the channel"
    );
    // The reason is the mask-route one, not the no-saved-selection one.
    let reason = ui::strings::tr("ui.docks.channels.no.mask.route");
    assert!(!reason.is_empty(), "the reason key is not registered");
}

// ---------------------------------------------------------------------------
// W3-J: inline rename, name search, Character / Paragraph typography, and the
// Properties Transform block - driven through real frames of the workspace,
// with the emitted intents applied to the document the way the shell does.
// ---------------------------------------------------------------------------

mod w3j {
    use editor_core::{Command, Document, History};
    use layer_model::{Layer, LayerId, LayerKind, TextLayer};
    use ui::dock::{LayoutId, PanelId};
    use ui::panels::layers::ids as layer_ids;
    use ui::panels::properties::ids as prop_ids;
    use ui::{Intent, Workspace};

    /// A harness that keeps the intents and applies them, so a test reads the
    /// document the panel actually produced.
    struct Live {
        ctx: egui::Context,
        workspace: Workspace,
        doc: Document,
        history: History,
        time: f64,
    }

    impl Live {
        fn new(doc: Document, panel: PanelId) -> Self {
            let ctx = egui::Context::default();
            design::apply_theme(&ctx, design::Theme::Dark);
            let style = design::style_for(design::Theme::Dark);
            ctx.set_style_of(egui::Theme::Dark, style.clone());
            ctx.set_style_of(egui::Theme::Light, style);
            let mut workspace = Workspace::new();
            workspace.dock.apply_layout(LayoutId::Minimal);
            workspace.dock.set_open(panel, true);
            let mut live = Self {
                ctx,
                workspace,
                doc,
                history: History::new(),
                time: 0.0,
            };
            live.settle();
            live
        }

        /// One frame; its intents come back unapplied.
        fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
            // Far enough apart that two frames never read as a double click
            // unless a test asks for one.
            self.time += 1.0;
            self.frame_at(self.time, events)
        }

        fn frame_at(&mut self, time: f64, events: Vec<egui::Event>) -> Vec<Intent> {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1400.0, 900.0),
                )),
                events,
                time: Some(time),
                ..Default::default()
            };
            let _ = self.ctx.run(input, |ctx| {
                self.workspace.ui(ctx, &self.doc, &self.history);
            });
            self.workspace.drain_intents()
        }

        fn settle(&mut self) {
            for _ in 0..3 {
                self.frame(Vec::new());
            }
        }

        fn rect(&mut self, id: egui::Id) -> egui::Rect {
            self.ctx
                .read_response(id)
                .unwrap_or_else(|| panic!("{id:?} was not drawn"))
                .rect
        }

        fn drawn(&self, id: egui::Id) -> bool {
            self.ctx.read_response(id).is_some()
        }

        fn press_release(at: egui::Pos2) -> Vec<egui::Event> {
            vec![
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
            ]
        }

        fn click_at(&mut self, at: egui::Pos2) -> Vec<Intent> {
            self.frame(Self::press_release(at))
        }

        /// Two clicks 50 ms apart: egui's double click.
        fn double_click(&mut self, id: egui::Id) -> Vec<Intent> {
            let at = self.rect(id).center();
            self.time += 1.0;
            let t = self.time;
            let mut out = self.frame_at(t, Self::press_release(at));
            out.extend(self.frame_at(t + 0.05, Self::press_release(at)));
            self.time = t + 0.05;
            out
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

        fn type_text(&mut self, text: &str) -> Vec<Intent> {
            let mut out = Vec::new();
            for ch in text.chars() {
                out.extend(self.frame(vec![egui::Event::Text(ch.to_string())]));
            }
            out
        }

        /// Click the field at `at`, select what is in it, type `text`, press
        /// Enter: a keyboard edit of a numeric or text field, start to commit.
        fn enter_value_at(&mut self, at: egui::Pos2, text: &str) -> Vec<Intent> {
            let mut out = self.click_at(at);
            out.extend(self.key(
                egui::Key::A,
                egui::Modifiers {
                    command: true,
                    ..Default::default()
                },
            ));
            out.extend(self.type_text(text));
            out.extend(self.key(egui::Key::Enter, egui::Modifiers::default()));
            out.extend(self.frame(Vec::new()));
            out
        }

        /// The numeric field of the `slider_row` labelled `label`.
        fn slider_field(&mut self, label: &str) -> egui::Pos2 {
            self.rect(egui::Id::new(("raster-numeric-field", label)))
                .center()
        }

        /// Apply document edits the way the shell does: a command through
        /// history, a kind edit as `SetLayerKind`. Returns how many applied.
        fn apply(&mut self, intents: Vec<Intent>) -> usize {
            let mut applied = 0;
            for intent in intents {
                let command = match intent {
                    Intent::Document(command) => command,
                    Intent::EditLayerKind { layer, kind } => Command::SetLayerKind {
                        layer_id: layer,
                        kind,
                    },
                    _ => continue,
                };
                self.history.apply(&mut self.doc, command).expect("apply");
                applied += 1;
            }
            applied
        }

        fn text_layer(&self, id: LayerId) -> TextLayer {
            match &self.doc.layers.get(id).expect("layer").kind {
                LayerKind::Text(t) => t.clone(),
                _ => panic!("not text"),
            }
        }
    }

    fn doc_with(layers: &[Layer]) -> (Document, Vec<LayerId>) {
        let mut doc = Document::new(400, 300, "Test");
        let mut ids = Vec::new();
        for layer in layers {
            ids.push(doc.layers.push_root(layer.clone()).unwrap());
        }
        doc.set_active_layer(ids.last().copied()).unwrap();
        (doc, ids)
    }

    fn text_doc(text: &str, size: f32) -> (Document, LayerId) {
        let (doc, ids) = doc_with(&[Layer::with_kind(
            "Type",
            LayerKind::Text(TextLayer::legacy(text, "sans-serif", size)),
        )]);
        (doc, ids[0])
    }

    fn line_width(layer: &TextLayer) -> f32 {
        let run = text_engine::TextRun::from(layer);
        compositor::text_caret_rect(&run, run.text.len()).x - compositor::text_caret_rect(&run, 0).x
    }

    // -- Layers: inline rename and the name search ---------------------------

    #[test]
    fn a_double_click_on_the_name_renames_the_layer_as_one_history_entry() {
        let (doc, ids) = doc_with(&[Layer::raster("Base"), Layer::raster("Top")]);
        let mut h = Live::new(doc, PanelId::Layers);
        let base = ids[0];

        let opened = h.double_click(layer_ids::name_label(base));
        assert_eq!(h.apply(opened), 0, "opening the rename edits nothing");
        h.frame(Vec::new());
        assert!(
            h.drawn(layer_ids::rename_field(base)),
            "the field replaces the label"
        );
        // The whole name is selected, so typing replaces it.
        let mut typed = h.type_text("Sky");
        typed.extend(h.key(egui::Key::Enter, egui::Modifiers::default()));
        typed.extend(h.frame(Vec::new()));
        typed.extend(h.frame(Vec::new()));
        assert_eq!(h.apply(typed), 1, "Enter commits exactly one command");
        assert_eq!(h.doc.layers.get(base).unwrap().name, "Sky");
        assert_eq!(h.history.undo_depth(), 1, "one undo step");
        assert!(!h.drawn(layer_ids::rename_field(base)), "the field closed");
        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(h.doc.layers.get(base).unwrap().name, "Base");
    }

    #[test]
    fn escape_cancels_the_inline_rename_and_changes_nothing() {
        let (doc, ids) = doc_with(&[Layer::raster("Base")]);
        let mut h = Live::new(doc, PanelId::Layers);
        let base = ids[0];
        let mut out = h.double_click(layer_ids::name_label(base));
        h.frame(Vec::new());
        out.extend(h.type_text("Nope"));
        out.extend(h.key(egui::Key::Escape, egui::Modifiers::default()));
        out.extend(h.frame(Vec::new()));
        out.extend(h.frame(Vec::new()));
        assert_eq!(h.apply(out), 0, "Escape emits nothing");
        assert_eq!(h.doc.layers.get(base).unwrap().name, "Base");
        assert!(!h.drawn(layer_ids::rename_field(base)));
    }

    #[test]
    fn the_search_field_hides_rows_whose_name_does_not_match() {
        let (doc, ids) = doc_with(&[
            Layer::raster("Background"),
            Layer::raster("Sky"),
            Layer::raster("Skyline"),
        ]);
        let mut h = Live::new(doc, PanelId::Layers);
        let row = ui::view::ids::layer_row;
        for id in &ids {
            assert!(h.drawn(row(*id)), "every row shows before a search");
        }
        let at = h.rect(layer_ids::search_field()).center();
        let mut out = h.click_at(at);
        out.extend(h.type_text("SKY"));
        out.extend(h.frame(Vec::new()));
        assert_eq!(h.apply(out), 0, "a search is view state, not an edit");
        assert_eq!(h.workspace.layers.search, "SKY");
        assert!(!h.drawn(row(ids[0])), "Background is hidden");
        assert!(
            h.drawn(row(ids[1])) && h.drawn(row(ids[2])),
            "both Sky rows show"
        );
    }

    // -- Character and Paragraph ---------------------------------------------

    #[test]
    fn character_horizontal_scale_200_doubles_the_laid_out_width() {
        if compositor::no_fonts() {
            return; // nothing can be measured on a machine with no fonts
        }
        let (doc, id) = text_doc("Hello scaled world", 32.0);
        let mut h = Live::new(doc, PanelId::Character);
        let before = line_width(&h.text_layer(id));
        assert!(before > 50.0, "real text: {before}");
        let at = h.slider_field(ui::strings::tr("ui.docks.character.hscale"));
        let out = h.enter_value_at(at, "200");
        assert!(h.apply(out) >= 1, "the field commits a kind edit");
        let layer = h.text_layer(id);
        assert_eq!(layer.style.horizontal_scale, 2.0);
        let after = line_width(&layer);
        assert!(
            (after - 2.0 * before).abs() < 1.0,
            "200 % doubles the width: {before} -> {after}"
        );
    }

    #[test]
    fn character_baseline_shift_moves_the_glyph_bounds_up() {
        if compositor::no_fonts() {
            return;
        }
        let (doc, id) = text_doc("Shift", 40.0);
        let mut h = Live::new(doc, PanelId::Character);
        let before = ui::panels::properties::Transform::frame(
            &h.doc,
            id,
            &ui::panels::properties::RasterInks::default(),
        )
        .expect("ink");
        let at = h.slider_field(ui::strings::tr("ui.docks.character.baseline.shift"));
        let out = h.enter_value_at(at, "12");
        assert!(h.apply(out) >= 1);
        assert_eq!(h.text_layer(id).style.baseline_shift, 12.0);
        let after = ui::panels::properties::Transform::frame(
            &h.doc,
            id,
            &ui::panels::properties::RasterInks::default(),
        )
        .expect("ink");
        assert!(
            (before.y - after.y - 12.0).abs() <= 1.0,
            "the compositor's bounds rose by the shift: {} -> {}",
            before.y,
            after.y
        );
        assert_eq!(before.height, after.height, "a shift, not a resize");
    }

    #[test]
    fn paragraph_left_indent_moves_the_first_glyph() {
        if compositor::no_fonts() {
            return;
        }
        let (doc, id) = text_doc("Indented", 24.0);
        let mut h = Live::new(doc, PanelId::Paragraph);
        let first_x = |layer: &TextLayer| {
            compositor::text_caret_rect(&text_engine::TextRun::from(layer), 0).x
        };
        let before = first_x(&h.text_layer(id));
        let at = h.slider_field(ui::strings::tr("ui.docks.paragraph.indent.left"));
        let out = h.enter_value_at(at, "30");
        assert!(h.apply(out) >= 1);
        let layer = h.text_layer(id);
        assert_eq!(layer.paragraph.left_indent, 30.0);
        assert!((first_x(&layer) - before - 30.0).abs() < 1e-3);
    }

    #[test]
    fn with_no_text_layer_the_character_panel_edits_the_type_tool_size() {
        let (doc, _) = doc_with(&[Layer::raster("Base")]);
        let mut h = Live::new(doc, PanelId::Character);
        let at = h.slider_field("Size");
        let out = h.enter_value_at(at, "72");
        let writes: Vec<_> = out
            .iter()
            .filter_map(|i| match i {
                Intent::SetToolOption { tool, key, value } => Some((*tool, *key, *value)),
                _ => None,
            })
            .collect();
        assert!(
            writes.contains(&(tools::ToolId::Type, "size_px", ui::OptionValue::Float(72.0))),
            "{writes:?}"
        );
        assert_eq!(
            h.workspace.options.get(tools::ToolId::Type, "size_px"),
            Some(ui::OptionValue::Float(72.0))
        );
    }

    // -- Properties: the Transform block -------------------------------------

    #[test]
    fn the_properties_w_field_resizes_the_layer_as_one_undo_step() {
        let (doc, ids) = doc_with(&[Layer::with_kind(
            "Box",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg("M10 20 H110 V70 H10 Z")),
        )]);
        let id = ids[0];
        let mut h = Live::new(doc, PanelId::Properties);
        let before = ui::panels::properties::Transform::frame(
            &h.doc,
            id,
            &ui::panels::properties::RasterInks::default(),
        )
        .expect("a frame");
        assert_eq!((before.width, before.height), (100.0, 50.0));
        // The block is a disclosure, closed at first; open it.
        assert!(!h.drawn(prop_ids::transform_w(id)), "closed until opened");
        let toggle = h.rect(prop_ids::transform_toggle()).center();
        h.click_at(toggle);
        h.settle();
        let at = h.rect(prop_ids::transform_w(id)).center();
        let out = h.enter_value_at(at, "250");
        assert_eq!(h.apply(out), 1, "one command");
        assert_eq!(h.history.undo_depth(), 1, "one undo step");
        let after = ui::panels::properties::Transform::frame(
            &h.doc,
            id,
            &ui::panels::properties::RasterInks::default(),
        )
        .unwrap();
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(after.x, 10.0) && near(after.width, 250.0), "{after:?}");
        assert!(near(after.y, 20.0) && near(after.height, 50.0), "{after:?}");
        h.history.undo(&mut h.doc).unwrap();
        assert_eq!(
            ui::panels::properties::Transform::frame(
                &h.doc,
                id,
                &ui::panels::properties::RasterInks::default()
            )
            .unwrap(),
            before
        );
    }

    /// A 400 x 300 document whose one raster layer holds a 100 x 100 opaque
    /// dab at the origin - inside one stored 256 x 256 tile - with the ink
    /// published the way the chrome publishes it every frame.
    fn raster_dab_live() -> (Live, LayerId) {
        use editor_core::{PixelTarget, TileEdit};
        let (mut doc, ids) = doc_with(&[Layer::with_kind(
            "Paint",
            LayerKind::Raster(Default::default()),
        )]);
        let id = ids[0];
        let side = raster::TILE_SIZE as usize;
        let mut bytes = vec![0u8; side * side * 4];
        for y in 0..100usize {
            for x in 0..100usize {
                let i = (y * side + x) * 4;
                bytes[i..i + 4].copy_from_slice(&[200, 30, 30, 255]);
            }
        }
        let mut source = compositor::MemoryTileSource::new();
        let hash = source.insert_bytes(bytes);
        let paint = Command::paint_tiles(
            PixelTarget::Layer(id),
            [TileEdit::set(raster::TileCoord::new(0, 0, 0), hash)],
        )
        .unwrap();
        History::new().apply(&mut doc, paint).unwrap();
        let inks = ui::panels::properties::RasterInks::measure(&doc, &source, id);
        let mut h = Live::new(doc, PanelId::Properties);
        inks.publish(&h.ctx);
        let toggle = h.rect(prop_ids::transform_toggle()).center();
        h.click_at(toggle);
        h.settle();
        (h, id)
    }

    fn raster_frame(h: &Live, id: LayerId) -> ui::panels::properties::LayerFrame {
        let inks = ui::panels::properties::RasterInks::published(&h.ctx);
        ui::panels::properties::Transform::frame(&h.doc, id, &inks).expect("a frame")
    }

    #[test]
    fn align_right_puts_a_raster_dab_flush_against_the_right_edge() {
        let (mut h, id) = raster_dab_live();
        let f = raster_frame(&h, id);
        assert_eq!((f.x, f.width), (0.0, 100.0), "the dab, not the 256 tile");
        let picker = h.rect(prop_ids::align_picker(id)).center();
        h.click_at(picker);
        h.settle();
        let right = h
            .rect(prop_ids::align(ui::panels::properties::AlignEdge::Right))
            .center();
        let out = h.click_at(right);
        assert_eq!(h.apply(out), 1, "one command");
        let f = raster_frame(&h, id);
        assert_eq!((f.x, f.width), (300.0, 100.0), "flush with 400: {f:?}");
    }

    #[test]
    fn the_w_field_on_a_raster_layer_scales_the_pixels_not_the_tile() {
        let (mut h, id) = raster_dab_live();
        let at = h.rect(prop_ids::transform_w(id)).center();
        let out = h.enter_value_at(at, "200");
        assert_eq!(h.apply(out), 1, "one command");
        assert_eq!(h.history.undo_depth(), 1, "one undo step");
        let f = raster_frame(&h, id);
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(f.x, 0.0) && near(f.width, 200.0), "{f:?}");
        assert!(near(f.height, 100.0), "{f:?}");
    }

    #[test]
    fn text_shape_and_smart_object_pages_draw_their_own_controls() {
        let (doc, ids) = doc_with(&[Layer::with_kind(
            "Box",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg("M0 0 H40 V40 H0 Z")),
        )]);
        let h = Live::new(doc, PanelId::Properties);
        assert!(h.drawn(prop_ids::shape_fill_enabled(ids[0])));
        assert!(h.drawn(prop_ids::transform_toggle()));

        let (doc, id) = text_doc("Hi", 20.0);
        let h = Live::new(doc, PanelId::Properties);
        assert!(h.drawn(prop_ids::text_family(id)));

        let (doc, _) = doc_with(&[Layer::with_kind(
            "Smart",
            LayerKind::SmartObject(layer_model::SmartObjectLayer {
                asset: layer_model::AssetId::new(),
                linked: false,
            }),
        )]);
        let mut h = Live::new(doc, PanelId::Properties);
        let at = h.rect(prop_ids::replace_contents()).center();
        let out = h.click_at(at);
        assert!(
            out.iter()
                .any(|i| matches!(i, Intent::Action(ui::menu::MenuAction::ReplaceContents))),
            "Replace Contents routes to the menu action: {out:?}"
        );
        assert!(h.drawn(prop_ids::edit_contents()));
    }

    // -- Round 2: the Type tool's default style (spec item 5) ----------------

    /// The Type tool as the shell drives it: the workspace's held options
    /// forwarded through `set_setting` (the shell's boundary conversion), one
    /// click, and the text layer that click creates.
    fn created_by_type_tool(workspace: &Workspace) -> TextLayer {
        use raster::PixelRect;
        use tools::tool::{PointerEvent, ToolContext};
        use tools::ToolSetting;
        let mut tool = tools::registry::make(tools::ToolId::Type);
        for (key, value) in workspace.options.held(tools::ToolId::Type) {
            let setting = match value {
                ui::OptionValue::Float(v) => ToolSetting::Float(v),
                ui::OptionValue::Int(v) => ToolSetting::Int(v),
                ui::OptionValue::Bool(v) => ToolSetting::Bool(v),
                ui::OptionValue::Choice(v) => ToolSetting::Choice(v),
                ui::OptionValue::Color(v) => ToolSetting::Color(v),
            };
            tool.set_setting(&key, setting)
                .unwrap_or_else(|e| panic!("Type/{key}: {e}"));
        }
        let mut tiles = tools::MemoryTiles::new();
        let mut ctx = ToolContext::new(&mut tiles, PixelRect::new(0, 0, 256, 256));
        tool.on_pointer_down(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        tool.on_pointer_up(&mut ctx, PointerEvent::at(10.0, 10.0))
            .unwrap();
        let cmds = ctx.drain();
        let Some(Command::CreateLayer { layer }) = cmds.first() else {
            panic!("a Type click creates a layer: {cmds:?}");
        };
        match &layer.kind {
            LayerKind::Text(t) => t.clone(),
            other => panic!("not text: {other:?}"),
        }
    }

    fn type_option_writes(out: &[Intent]) -> Vec<(&'static str, ui::OptionValue)> {
        out.iter()
            .filter_map(|i| match i {
                Intent::SetToolOption { tool, key, value } if *tool == tools::ToolId::Type => {
                    Some((*key, *value))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn with_no_text_layer_character_sets_the_type_defaults_the_next_layer_is_created_with() {
        let (doc, _) = doc_with(&[Layer::raster("Base")]);
        let mut h = Live::new(doc, PanelId::Character);
        let plain = created_by_type_tool(&h.workspace);
        assert_eq!(plain.style.horizontal_scale, 1.0, "the untouched default");

        let at = h.slider_field(ui::strings::tr("ui.docks.character.hscale"));
        let out = h.enter_value_at(at, "200");
        assert_eq!(
            h.apply(out.clone()),
            0,
            "no document edit: nothing is selected"
        );
        let writes = type_option_writes(&out);
        assert!(
            writes.contains(&("horizontal_scale", ui::OptionValue::Float(200.0))),
            "{writes:?}"
        );
        assert!(
            writes.iter().all(|(k, _)| *k == "horizontal_scale"),
            "only the edited value is written: {writes:?}"
        );
        let at = h.slider_field(ui::strings::tr("ui.docks.character.baseline.shift"));
        let out = h.enter_value_at(at, "12");
        assert!(
            type_option_writes(&out).contains(&("baseline_shift", ui::OptionValue::Float(12.0)))
        );

        // Held on the workspace, and seeded into the next layer the Type tool
        // creates - through the real tool.
        let seeded = created_by_type_tool(&h.workspace);
        assert_eq!(seeded.style.horizontal_scale, 2.0);
        assert_eq!(seeded.style.baseline_shift, 12.0);

        // ...where the engine lays it out twice as wide as the plain default.
        if !compositor::no_fonts() {
            let with_text = |mut t: TextLayer| {
                t.text = "Hello scaled world".into();
                t
            };
            let (a, b) = (
                line_width(&with_text(plain)),
                line_width(&with_text(seeded)),
            );
            assert!(a > 20.0 && (b - 2.0 * a).abs() < 1.0, "{a} -> {b}");
        }

        // The panel reads the defaults back: the field shows 200.
        h.settle();
        assert_eq!(
            ui::panels::text::type_defaults(&h.workspace.options)
                .style
                .horizontal_scale,
            2.0
        );
    }

    #[test]
    fn with_no_text_layer_paragraph_sets_the_type_default_indents_and_spacing() {
        let (doc, _) = doc_with(&[Layer::raster("Base")]);
        let mut h = Live::new(doc, PanelId::Paragraph);
        let at = h.slider_field(ui::strings::tr("ui.docks.paragraph.indent.left"));
        let out = h.enter_value_at(at, "30");
        assert!(
            type_option_writes(&out).contains(&("left_indent", ui::OptionValue::Float(30.0))),
            "{out:?}"
        );
        let at = h.slider_field("Before");
        let out = h.enter_value_at(at, "8");
        assert!(type_option_writes(&out).contains(&("space_before", ui::OptionValue::Float(8.0))));

        let seeded = created_by_type_tool(&h.workspace);
        assert_eq!(seeded.paragraph.left_indent, 30.0);
        assert_eq!(seeded.paragraph.space_before, 8.0);
        if !compositor::no_fonts() {
            let mut layer = seeded;
            layer.text = "Indented".into();
            let mut plain = layer.clone();
            plain.paragraph.left_indent = 0.0;
            let first_x =
                |t: &TextLayer| compositor::text_caret_rect(&text_engine::TextRun::from(t), 0).x;
            assert!((first_x(&layer) - first_x(&plain) - 30.0).abs() < 1e-3);
        }
    }

    // -- Round 2: manual kerning needs two characters --------------------------

    /// Every string the frame painted.
    fn painted_texts(h: &mut Live) -> Vec<String> {
        fn walk(shape: &egui::Shape, out: &mut Vec<String>) {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        h.time += 1.0;
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1400.0, 900.0),
            )),
            time: Some(h.time),
            ..Default::default()
        };
        let (workspace, doc, history) = (&mut h.workspace, &h.doc, &h.history);
        let output = h.ctx.run(input, |ctx| workspace.ui(ctx, doc, history));
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    #[test]
    fn manual_kerning_is_offered_only_where_there_is_a_pair_to_kern() {
        let (doc, _) = text_doc("A", 20.0);
        let mut h = Live::new(doc, PanelId::Character);
        let texts = painted_texts(&mut h);
        assert!(
            texts.iter().any(|t| t == "Metrics"),
            "the kerning row is drawn"
        );
        assert!(
            !texts.iter().any(|t| t == "Manual"),
            "one character: no Manual segment to jump back from"
        );

        let (doc, id) = text_doc("AV", 20.0);
        let mut h = Live::new(doc, PanelId::Character);
        let texts = painted_texts(&mut h);
        assert!(texts.iter().any(|t| t == "Manual"));
        // Picking it sticks: the table is non-empty and reads back as Manual.
        let run = text_engine::TextRun::from(&h.text_layer(id));
        let mut manual = run.clone();
        assert!(ui::panels::text::Character::set_kerning_mode(
            &mut manual,
            ui::panels::text::KerningMode::Manual,
            50.0
        ));
        assert_eq!(
            ui::panels::text::Character::kerning_mode(&manual).0,
            ui::panels::text::KerningMode::Manual
        );
    }

    // -- Round 2: the shape page's corner radius -------------------------------

    #[test]
    fn the_shape_radius_field_rounds_a_rectangle_and_undo_restores_it() {
        let (doc, ids) = doc_with(&[Layer::with_kind(
            "Box",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg("M10 20 H110 V70 H10 Z")),
        )]);
        let id = ids[0];
        let mut h = Live::new(doc, PanelId::Properties);
        assert_eq!(
            ui::panels::properties::ShapeProperties::corner_radius(&h.doc, id),
            Some(0.0),
            "a plain rectangle reads as radius 0"
        );
        assert!(h.drawn(prop_ids::shape_radius(id)));
        let at = h.slider_field(ui::strings::tr("ui.docks.shape.radius"));
        let out = h.enter_value_at(at, "12");
        // The field commits as it is typed ("1", then "12"): each change is
        // its own kind edit, and this harness applies each as its own history
        // entry, so the test undoes every one it applied.
        let applied = h.apply(out);
        assert!(applied >= 1, "the field edits the path");
        let LayerKind::Shape(shape) = &h.doc.layers.get(id).unwrap().kind else {
            panic!("still a shape");
        };
        assert!(shape.path_svg.contains('C'), "the corners are curves now");
        let radius = ui::panels::properties::ShapeProperties::corner_radius(&h.doc, id)
            .expect("still a rounded rectangle");
        assert!((radius - 12.0).abs() < 1e-3, "{radius}");
        // The box did not move or grow.
        let frame = ui::panels::properties::Transform::frame(
            &h.doc,
            id,
            &ui::panels::properties::RasterInks::default(),
        )
        .unwrap();
        assert!((frame.x - 10.0).abs() < 1e-3 && (frame.width - 100.0).abs() < 1e-3);
        for _ in 0..applied {
            h.history.undo(&mut h.doc).unwrap();
        }
        assert_eq!(
            ui::panels::properties::ShapeProperties::corner_radius(&h.doc, id),
            Some(0.0)
        );
    }

    #[test]
    fn a_path_with_no_corners_to_round_gets_no_radius_slider() {
        let (doc, ids) = doc_with(&[Layer::with_kind(
            "Tri",
            LayerKind::Shape(layer_model::ShapeLayer::from_svg("M0 0 L80 0 L40 60 Z")),
        )]);
        let h = Live::new(doc, PanelId::Properties);
        assert!(
            h.drawn(prop_ids::shape_fill_enabled(ids[0])),
            "the shape page is up"
        );
        assert!(!h.drawn(prop_ids::shape_radius(ids[0])));
        assert_eq!(
            ui::panels::properties::ShapeProperties::corner_radius(&h.doc, ids[0]),
            None
        );
    }
}
