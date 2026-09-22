//! Running the whole workspace without a window.
//!
//! The model tests prove *what* a control means. These prove the drawing that
//! shows it actually runs: egui panics on a missing named text style, on two
//! widgets claiming one id, and on a layout that allocates a negative size, and
//! none of those are visible in a model test. Every panel is opened, both
//! themes are installed, and several frames are run — because egui only reports
//! an id clash on the frame *after* it happens.

use editor_core::{Command, Document, History, LayerPatch};
use layer_model::{
    AdjustmentKind, AdjustmentLayer, Layer, LayerKind, LayerMask, MaskId, TextLayer,
};
use ui::dock::{DockSide, LayoutId, PanelId};
use ui::intent::Progress;
use ui::{Intent, Workspace};

/// A document with one of everything the panels branch on.
fn busy_document() -> (Document, History) {
    let mut doc = Document::new(640, 480, "Test Document");
    let mut history = History::new();

    let group = doc
        .layers
        .insert_at(Layer::group("Group"), None, 0)
        .unwrap();
    let raster = doc
        .layers
        .insert_at(Layer::raster("Photo"), Some(group), 0)
        .unwrap();
    doc.layers
        .insert_at(
            Layer::with_kind(
                "Curves",
                LayerKind::Adjustment(AdjustmentLayer {
                    kind: AdjustmentKind::Levels {
                        black: 0.0,
                        white: 1.0,
                        gamma: 1.0,
                    },
                }),
            ),
            None,
            1,
        )
        .unwrap();
    doc.layers
        .insert_at(
            Layer::with_kind(
                "Title",
                LayerKind::Text(TextLayer {
                    text: "Hello".into(),
                    font_family: "Inter".into(),
                    size_px: 32.0,
                    ..Default::default()
                }),
            ),
            None,
            2,
        )
        .unwrap();
    doc.layers
        .insert_at(
            Layer::with_kind(
                "Badge",
                LayerKind::Shape(layer_model::ShapeLayer::from_svg("M0 0 L10 10 Z")),
            ),
            None,
            3,
        )
        .unwrap();
    doc.layers.get_mut(raster).unwrap().mask = Some(LayerMask::new(MaskId::new()));
    doc.set_active_layer(Some(raster)).unwrap();

    history
        .apply(
            &mut doc,
            Command::SetLayerProperties {
                layer_id: raster,
                patch: LayerPatch {
                    opacity: Some(0.8),
                    ..Default::default()
                },
            },
        )
        .expect("apply");
    (doc, history)
}

/// Run `frames` frames with every panel open, in one theme.
fn run(
    theme: design::Theme,
    frames: usize,
    mut prepare: impl FnMut(&mut Workspace),
) -> Vec<Intent> {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, theme);
    let style = design::style_for(theme);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);

    let (doc, history) = busy_document();
    let mut workspace = Workspace::new();
    for panel in PanelId::ALL {
        workspace.dock.set_open(*panel, true);
    }
    workspace.dock.dock(PanelId::Info, DockSide::Bottom);
    prepare(&mut workspace);

    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1600.0, 1000.0),
        )),
        ..Default::default()
    };

    let mut last = Vec::new();
    for _ in 0..frames {
        let _ = ctx.run(input.clone(), |ctx| {
            workspace.ui(ctx, &doc, &history);
        });
        last = workspace.drain_intents();
    }
    last
}

#[test]
fn the_whole_workspace_draws_in_dark() {
    run(design::Theme::Dark, 3, |_| {});
}

#[test]
fn the_whole_workspace_draws_in_light() {
    run(design::Theme::Light, 3, |_| {});
}

#[test]
fn every_workspace_layout_draws() {
    for layout in LayoutId::ALL {
        let l = *layout;
        run(design::Theme::Dark, 2, move |w| w.dock.apply_layout(l));
    }
}

#[test]
fn every_panel_draws_on_its_own() {
    for panel in PanelId::ALL {
        let p = *panel;
        run(design::Theme::Dark, 2, move |w| {
            for other in PanelId::ALL {
                w.dock.set_open(*other, *other == p);
            }
        });
    }
}

#[test]
fn every_tool_draws_its_options_bar() {
    // The options bar is generated from the registry, so a tool with an option
    // shape nothing else has would only show up here.
    let model = ui::PaletteModel::build();
    for tool in tools::ToolId::ALL {
        let t = *tool;
        let m = model.clone();
        run(design::Theme::Dark, 2, move |w| {
            w.palette.activate(&m, t);
        });
    }
}

#[test]
fn every_colour_notation_draws() {
    for notation in ui::panels::color::ColorNotation::ALL {
        let n = *notation;
        run(design::Theme::Light, 2, move |w| {
            w.color.notation = n;
            w.color.set_current([0.2, 0.6, 0.9, 0.5]);
        });
    }
}

#[test]
fn the_status_bar_draws_a_determinate_and_an_indeterminate_operation() {
    run(design::Theme::Dark, 2, |w| {
        w.status.progress = Some(Progress::new("Applying Gaussian Blur", 0.4));
    });
    run(design::Theme::Dark, 2, |w| {
        w.status.progress = Some(Progress::indeterminate("Loading"));
    });
}

#[test]
fn a_document_with_no_layers_draws_its_empty_states() {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let style = design::style_for(design::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);

    let doc = Document::new(64, 64, "Empty");
    let history = History::new();
    let mut workspace = Workspace::new();
    for panel in PanelId::ALL {
        workspace.dock.set_open(*panel, true);
    }
    for _ in 0..2 {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1200.0, 800.0),
                )),
                ..Default::default()
            },
            |ctx| workspace.ui(ctx, &doc, &history),
        );
        workspace.drain_intents();
    }
}

#[test]
fn a_tool_flyout_draws() {
    let model = ui::PaletteModel::build();
    let slot = model
        .slot_of(tools::ToolId::RectMarquee)
        .expect("the marquee is in the palette");
    run(design::Theme::Dark, 2, move |w| {
        w.palette.open_flyout = Some(slot);
    });
}

#[test]
fn a_collapsed_panel_draws_only_its_header() {
    run(design::Theme::Dark, 2, |w| {
        for panel in PanelId::ALL {
            w.dock.set_collapsed(*panel, true);
        }
    });
}

#[test]
fn drawing_emits_nothing_when_nobody_clicks() {
    // A frame with no input must produce no intent. A panel that mirrored its
    // current state into the outbox would replay the frame's starting state
    // forever, undoing anything done in the same frame.
    let intents = run(design::Theme::Dark, 3, |_| {});
    assert!(intents.is_empty(), "an untouched frame emitted {intents:?}");
}

/// C2, the real one: the tool palette's slots must be *visible*, not merely
/// allocated and painted.
///
/// The column was empty in every product shot for a year, and the earlier
/// version of this test passed the whole time, because it counted icon shapes
/// in the frame output — and the icons were all there. What it never asked
/// was what came *after* them: the footer opened with an opaque
/// `rect_filled(ui.max_rect(), ..)` over the whole column, on top of every
/// icon already emitted, and then placed the wells at the top of that rect.
///
/// So this walks `FullOutput::shapes` in paint order and, for every slot the
/// layout says is on screen, finds its last icon shape and asserts that no
/// opaque filled rect emitted after it intersects the slot. It also asserts
/// that at least twenty slots are fully inside their clip at 1440x900 *and* at
/// 1280x720, across three warm-up frames, so the count can neither be clipped
/// away by a short window nor hidden behind a first-frame layout.
///
/// Mutation, verified: restoring the pre-fix `toolbar.rs` (footer after the
/// slots, `rect_filled(ui.max_rect())`) makes this red on slot 0 at frame 1.
#[test]
fn no_opaque_rect_is_painted_over_a_visible_tool_slot_at_either_viewport() {
    fn install(ctx: &egui::Context) {
        design::apply_theme(ctx, design::Theme::Dark);
        let style = design::style_for(design::Theme::Dark);
        ctx.set_style_of(egui::Theme::Dark, style.clone());
        ctx.set_style_of(egui::Theme::Light, style);
    }

    /// Icon ink: the shapes `ui::icons::Icon::paint` emits. Rects are chrome
    /// (fills, strokes, wells) and are what the check is about, so they are
    /// not ink.
    fn is_ink(shape: &egui::Shape) -> bool {
        matches!(
            shape,
            egui::Shape::Path(_) | egui::Shape::LineSegment { .. } | egui::Shape::Circle(_)
        )
    }

    let model = ui::PaletteModel::build();
    let slot_count = model.slots().len();
    assert!(slot_count >= 20, "the registry has only {slot_count} slots");

    for size in [egui::vec2(1440.0, 900.0), egui::vec2(1280.0, 720.0)] {
        let ctx = egui::Context::default();
        install(&ctx);
        let mut w = Workspace::new();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
            ..Default::default()
        };

        for frame in 1..=3 {
            let full = ctx.run(input.clone(), |ctx| {
                ui::view::tool_palette(&mut w, ctx);
                ui::view::tool_options(&mut w, ctx);
            });
            w.drain_intents();
            let shapes = &full.shapes;
            let at = |size: egui::Vec2| format!("{}x{} frame {frame}", size.x, size.y);

            let slots: Vec<(usize, egui::Rect)> = (0..slot_count)
                .filter_map(|i| {
                    ctx.read_response(ui::view::ids::tool_slot(i))
                        .map(|r| (i, r.rect))
                })
                .collect();
            assert_eq!(
                slots.len(),
                slot_count,
                "{}: not every slot allocated a rect",
                at(size)
            );

            // A slot is on screen when its rect has area and the layout
            // clipped its icon to a rect that fully contains it.
            let mut visible = 0usize;
            let mut checked_ink = 0usize;
            for (slot, rect) in &slots {
                assert!(rect.area() > 0.0, "{}: slot {slot} has no area", at(size));
                let ink: Vec<(usize, &egui::epaint::ClippedShape)> = shapes
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        is_ink(&c.shape) && rect.contains(c.shape.visual_bounding_rect().center())
                    })
                    .collect();
                let Some((last_ink, clipped)) = ink.last() else {
                    continue; // culled by the scroll viewport: off screen
                };
                if !clipped.clip_rect.contains_rect(*rect) {
                    continue; // partly under the footer or the window edge
                }
                visible += 1;
                checked_ink += ink.len();
                for (index, later) in shapes.iter().enumerate().skip(last_ink + 1) {
                    if let egui::Shape::Rect(r) = &later.shape {
                        if r.fill.a() == 255 && r.rect.intersects(*rect) {
                            panic!(
                                "{}: slot {slot} at {rect:?} is painted over by an opaque \
                                 rect {:?} (fill {:?}) emitted at shape #{index}, after the \
                                 slot's last icon shape #{last_ink} — the footer overpaint \
                                 is back",
                                at(size),
                                r.rect,
                                r.fill
                            );
                        }
                    }
                }
            }
            assert!(
                visible >= 20,
                "{}: only {visible} of {slot_count} tool slots are fully on screen",
                at(size)
            );
            assert!(
                checked_ink >= visible * 2,
                "{}: {checked_ink} icon shapes over {visible} slots — the glyphs are \
                 not being painted",
                at(size)
            );
        }
    }
}

/// The glyph a slot draws is the slot minus its inset: it must be at least the
/// minimum hit target, or the icons read as specks (they were 8 pt).
#[test]
fn a_tool_slot_glyph_is_at_least_the_minimum_hit_target() {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let style = design::style_for(design::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    let mut w = Workspace::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let full = ctx.run(input, |ctx| ui::view::tool_palette(&mut w, ctx));
    let t = design::Theme::Dark.tokens();
    let min = t.metrics.min_hit_target;

    // The first slot's ink, measured as the union of its shapes.
    let slot = ctx
        .read_response(ui::view::ids::tool_slot(0))
        .expect("slot 0")
        .rect;
    let mut ink = egui::Rect::NOTHING;
    for clipped in &full.shapes {
        if matches!(
            clipped.shape,
            egui::Shape::Path(_) | egui::Shape::LineSegment { .. } | egui::Shape::Circle(_)
        ) {
            let b = clipped.shape.visual_bounding_rect();
            if slot.contains(b.center()) {
                ink = ink.union(b);
            }
        }
    }
    assert!(
        ink.width().max(ink.height()) >= min * 0.75,
        "slot 0's glyph spans only {}x{} pt in a {} pt slot",
        ink.width(),
        ink.height(),
        slot.width()
    );

    // The footer's swap and reset controls carry a glyph, not a red dot: no
    // shape inside either control is painted in the danger colour, and each
    // lays ink across at least half its hit-target square. The glyph box is
    // 12 pt (the 16 pt control less a Hair inset each side); the drawings
    // span about 0.7 of their unit square, so the ink is ~8-9 pt. Before the
    // fix the box was 4 pt and the ink under 3 pt.
    let danger = design::color32(t.palette.color(design::ColorRole::Danger));
    for (name, id) in [
        ("swap", ui::view::ids::color_swap()),
        ("reset", ui::view::ids::color_reset()),
    ] {
        let rect = ctx.read_response(id).expect(name).rect;
        let mut ink = egui::Rect::NOTHING;
        for clipped in &full.shapes {
            let b = clipped.shape.visual_bounding_rect();
            if !rect.contains(b.center()) {
                continue;
            }
            let solid = |stroke: &egui::epaint::PathStroke| match stroke.color {
                egui::epaint::ColorMode::Solid(c) => c,
                egui::epaint::ColorMode::UV(_) => egui::Color32::TRANSPARENT,
            };
            match &clipped.shape {
                egui::Shape::Path(p) => {
                    assert_ne!(p.fill, danger, "{name} is painted as a missing icon");
                    assert_ne!(
                        solid(&p.stroke),
                        danger,
                        "{name} is painted as a missing icon"
                    );
                    ink = ink.union(b);
                }
                egui::Shape::LineSegment { stroke, .. } => {
                    assert_ne!(solid(stroke), danger, "{name} is painted as a missing icon");
                    ink = ink.union(b);
                }
                egui::Shape::Circle(c) => {
                    assert_ne!(c.fill, danger, "{name} is painted as a missing icon");
                    ink = ink.union(b);
                }
                _ => {}
            }
        }
        assert!(
            ink.width().max(ink.height()) >= min * 0.5,
            "{name}'s glyph spans only {}x{} pt",
            ink.width(),
            ink.height()
        );
    }
}
