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
    assert_eq!(
        slot_count, 20,
        "Photopea's column is twenty slots; the registry gave {slot_count}"
    );

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
            // Nineteen slots plus the three-row footer fit a 720 pt window
            // whole: none may be scrolled off or tucked under the footer.
            assert_eq!(
                visible,
                slot_count,
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

/// W2-C: the column is Photopea's — nineteen slots in Photopea's order, Free
/// Transform off it — and the footer carries Photopea's `Q` and `F` under the
/// wells, drawn with real ink.
///
/// A real route, not a helper: the frame is drawn through
/// `ui::view::tool_palette`, the controls are found by their ids, and the
/// clicks go in as egui pointer events. Q must come out as the *same*
/// `Intent::Action(MenuAction::ToggleQuickMask)` the Select menu raises and
/// draw engaged *only* from the flag the shell mirrors in — never from its
/// own click. F is live (W2-X): it has click sense, a click raises a
/// screen-mode cycle *request* on the palette state — no intent, and the
/// mirrored mode itself does not move — and it draws engaged only from the
/// `ScreenMode` the shell mirrors in (both full-screen modes light it,
/// Standard clears it). Both glyphs sit in the secondary text role at rest.
#[test]
fn the_column_is_photopeas_and_q_and_f_sit_under_the_wells() {
    use ui::palette::{quick_mask_control, screen_mode_control, ScreenMode};
    use ui::MenuAction;

    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let style = design::style_for(design::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    let t = design::Theme::Dark.tokens();
    let mut w = Workspace::new();

    let frame = |w: &mut Workspace, events: Vec<egui::Event>| {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1440.0, 900.0),
            )),
            events,
            ..Default::default()
        };
        let full = ctx.run(input, |ctx| ui::view::tool_palette(w, ctx));
        (full.shapes, w.drain_intents())
    };
    let click = |at: egui::Pos2| {
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
    };
    let accent = design::color32(t.palette.color(design::ColorRole::AccentSubtle));
    let engaged_fill = |shapes: &[egui::epaint::ClippedShape], rect: egui::Rect| {
        shapes.iter().any(|c| match &c.shape {
            egui::Shape::Rect(r) => r.fill == accent && r.rect.contains_rect(rect.shrink(1.0)),
            _ => false,
        })
    };

    // Two warm-up frames: egui settles panel layout on the second.
    frame(&mut w, Vec::new());
    let (shapes, intents) = frame(&mut w, Vec::new());
    assert!(
        intents.is_empty(),
        "an untouched palette emitted {intents:?}"
    );

    // The column.
    let model = ui::PaletteModel::build();
    assert_eq!(
        model.slot_ids(),
        vec![
            "move",
            "marquee",
            "lasso",
            "wand",
            "crop",
            "eyedropper",
            "heal",
            "brush",
            "clone",
            "history",
            "eraser",
            "gradient",
            "blur",
            "tone",
            "pen",
            "type",
            "path",
            "shape",
            "hand",
            "zoom",
        ]
    );
    assert_eq!(model.slot_of(tools::ToolId::FreeTransform), None);
    let slot_rects: Vec<egui::Rect> = (0..model.slots().len())
        .map(|i| {
            ctx.read_response(ui::view::ids::tool_slot(i))
                .unwrap_or_else(|| panic!("slot {i} was not drawn"))
                .rect
        })
        .collect();
    for pair in slot_rects.windows(2) {
        assert!(
            pair[1].top() >= pair[0].bottom(),
            "slots are drawn top to bottom in model order: {pair:?}"
        );
    }
    let strip = slot_rects
        .iter()
        .fold(egui::Rect::NOTHING, |acc, r| acc.union(*r));

    // The footer: Q under swap, F under reset, both inside the column and
    // below the wells, and both carrying a glyph rather than the missing-icon
    // mark.
    let rect_of = |id: egui::Id, name: &str| {
        ctx.read_response(id)
            .unwrap_or_else(|| panic!("{name} was not drawn"))
            .rect
    };
    let swap = rect_of(ui::view::ids::color_swap(), "swap");
    let reset = rect_of(ui::view::ids::color_reset(), "reset");
    let quick = rect_of(quick_mask_control(), "quick mask");
    let screen = rect_of(screen_mode_control(), "screen mode");
    assert!(
        quick.top() >= swap.bottom(),
        "Q is not under swap: {quick:?} vs {swap:?}"
    );
    assert!(
        screen.top() >= reset.bottom(),
        "F is not under reset: {screen:?} vs {reset:?}"
    );
    assert!(
        quick.top() > strip.bottom(),
        "Q is not under the slot column"
    );
    assert!(
        (quick.left() - swap.left()).abs() < 0.5,
        "Q is not aligned with swap"
    );
    assert!(
        (screen.left() - reset.left()).abs() < 0.5,
        "F is not aligned with reset"
    );
    assert!(screen.left() >= quick.right(), "F is not to the right of Q");
    let column = egui::Rect::from_x_y_ranges(
        strip.left() - design::Space::Small.pt()..=strip.right() + design::Space::Small.pt(),
        strip.top()..=f32::INFINITY,
    );
    assert!(
        column.contains_rect(quick),
        "Q is outside the column: {quick:?}"
    );
    assert!(
        column.contains_rect(screen),
        "F is outside the column: {screen:?}"
    );

    let danger = design::color32(t.palette.color(design::ColorRole::Danger));
    let ink_of = |shapes: &[egui::epaint::ClippedShape], rect: egui::Rect, name: &str| {
        let mut ink = egui::Rect::NOTHING;
        for clipped in shapes {
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
        ink
    };
    let min = t.metrics.min_hit_target;
    for (name, rect) in [("quick mask", quick), ("screen mode", screen)] {
        let ink = ink_of(&shapes, rect, name);
        assert!(
            ink.width().max(ink.height()) >= min * 0.5,
            "{name}'s glyph spans only {}x{} pt",
            ink.width(),
            ink.height()
        );
    }
    assert!(
        !engaged_fill(&shapes, quick),
        "Q reads as engaged before anyone engaged it"
    );

    // Q: the Select menu's own action comes out, and nothing else — the
    // control does not declare itself engaged, because the editor may refuse
    // the toggle (no document). It still reads plain on the next frame.
    assert!(!w.palette.quick_mask);
    let (_, intents) = frame(&mut w, click(quick.center()));
    assert_eq!(
        intents,
        vec![Intent::Action(MenuAction::ToggleQuickMask)],
        "Q did not raise the quick-mask action"
    );
    assert!(
        !w.palette.quick_mask,
        "Q declared itself engaged before the editor said so"
    );
    let (shapes, intents) = frame(&mut w, Vec::new());
    assert!(intents.is_empty());
    assert!(
        !engaged_fill(&shapes, quick),
        "Q lit up on a click alone, with nothing mirrored from the editor"
    );
    // The engaged look follows the flag the shell mirrors from
    // `Editor::quick_mask()`: set, it is an opaque accent fill exactly over
    // the control; cleared, the fill goes.
    w.palette.quick_mask = true;
    let (shapes, _) = frame(&mut w, Vec::new());
    assert!(
        engaged_fill(&shapes, quick),
        "the engaged quick-mask control has no accent fill"
    );
    w.palette.quick_mask = false;
    let (shapes, _) = frame(&mut w, Vec::new());
    assert!(
        !engaged_fill(&shapes, quick),
        "Q stays lit after the mirrored flag cleared"
    );
    assert!(
        !engaged_fill(&shapes, screen),
        "F reads as engaged in Standard mode"
    );

    // F: live. Its click is a *request* — the chrome performs the
    // application's screen-mode action against the editor and mirrors the
    // mode back — so the control raises no intent, leaves the mode where it
    // was, and reads as engaged only from the mirrored value.
    let f_response = ctx
        .read_response(screen_mode_control())
        .expect("F was drawn");
    assert!(f_response.sense.click, "F has no click sense");
    let q_response = ctx
        .read_response(quick_mask_control())
        .expect("Q was drawn");
    assert!(q_response.sense.click, "Q lost its click sense");
    assert_eq!(w.palette.screen_mode, ScreenMode::Standard);
    assert!(
        !w.palette.take_screen_mode_cycle(),
        "a cycle request exists before any click"
    );
    let (shapes, intents) = frame(&mut w, click(screen.center()));
    assert!(intents.is_empty(), "F raised {intents:?}");
    assert!(
        w.palette.take_screen_mode_cycle(),
        "F's click raised no cycle request"
    );
    assert!(
        !w.palette.take_screen_mode_cycle(),
        "taking the request did not clear it"
    );
    assert_eq!(
        w.palette.screen_mode,
        ScreenMode::Standard,
        "F cycled the screen mode itself instead of asking"
    );
    assert!(
        !engaged_fill(&shapes, screen),
        "F reads as engaged after a click alone"
    );
    // The mirrored full-screen modes light it; Standard clears it.
    for mode in [ScreenMode::FullScreenWithMenu, ScreenMode::FullScreen] {
        w.palette.screen_mode = mode;
        let (shapes, _) = frame(&mut w, Vec::new());
        assert!(engaged_fill(&shapes, screen), "{mode:?} does not light F");
    }
    w.palette.screen_mode = ScreenMode::Standard;
    let (shapes, _) = frame(&mut w, Vec::new());
    assert!(
        !engaged_fill(&shapes, screen),
        "F stays lit after the mirrored mode cleared"
    );

    // Both glyphs are painted in the secondary role in their plain state —
    // F is a live control now, not a disabled one. Read from a frame with the
    // pointer parked away from both, so no hover state is in the picture.
    let disabled = design::color32(t.palette.text(design::TextRole::Disabled));
    let secondary = design::color32(t.palette.text(design::TextRole::Secondary));
    assert_ne!(
        disabled, secondary,
        "the theme cannot tell disabled from plain"
    );
    let ink_colors = |shapes: &[egui::epaint::ClippedShape], rect: egui::Rect| {
        let mut out: Vec<egui::Color32> = Vec::new();
        let mut push = |c: egui::Color32| {
            if c.a() > 0 && !out.contains(&c) {
                out.push(c);
            }
        };
        let solid = |stroke: &egui::epaint::PathStroke| match stroke.color {
            egui::epaint::ColorMode::Solid(c) => c,
            egui::epaint::ColorMode::UV(_) => egui::Color32::TRANSPARENT,
        };
        for clipped in shapes {
            if !rect.contains(clipped.shape.visual_bounding_rect().center()) {
                continue;
            }
            match &clipped.shape {
                egui::Shape::Path(p) => {
                    push(p.fill);
                    push(solid(&p.stroke));
                }
                egui::Shape::LineSegment { stroke, .. } => push(solid(stroke)),
                egui::Shape::Circle(c) => {
                    push(c.fill);
                    push(c.stroke.color);
                }
                _ => {}
            }
        }
        out
    };
    let (shapes, _) = frame(
        &mut w,
        vec![egui::Event::PointerMoved(egui::pos2(720.0, 450.0))],
    );
    let f_ink = ink_colors(&shapes, screen);
    assert!(!f_ink.is_empty(), "F has no glyph");
    assert!(
        f_ink.iter().all(|c| *c == secondary),
        "F's plain glyph is not in the secondary role: {f_ink:?} vs {secondary:?}"
    );
    let q_ink = ink_colors(&shapes, quick);
    assert!(
        q_ink.iter().all(|c| *c == secondary),
        "Q's plain glyph is not in the secondary role: {q_ink:?} vs {secondary:?}"
    );
}

/// W2-X: the options bar is part of the header band under the menu bar — the
/// header shade, not the panel shade the columns use. Read from the real
/// panel frame `ui::view::tool_options` paints.
#[test]
fn the_options_bar_is_painted_in_the_header_shade() {
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let style = design::style_for(design::Theme::Dark);
    ctx.set_style_of(egui::Theme::Dark, style.clone());
    ctx.set_style_of(egui::Theme::Light, style);
    let t = design::Theme::Dark.tokens();
    let mut w = Workspace::new();
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1440.0, 900.0),
        )),
        ..Default::default()
    };
    let mut shapes = Vec::new();
    for _ in 0..2 {
        let full = ctx.run(input.clone(), |ctx| ui::view::tool_options(&mut w, ctx));
        shapes = full.shapes;
    }
    let band =
        egui::containers::panel::PanelState::load(&ctx, egui::Id::new("raster-tool-options"))
            .expect("the options bar was drawn")
            .rect;
    let header = design::color32(t.palette.color(design::ColorRole::SurfaceHeader));
    let panel = design::color32(t.palette.color(design::ColorRole::SurfacePanel));
    assert_ne!(
        header, panel,
        "the theme cannot tell the header from a panel"
    );
    let fills: Vec<egui::Color32> = shapes
        .iter()
        .filter_map(|c| match &c.shape {
            egui::Shape::Rect(r)
                if (r.rect.min - band.min).length() < 1.0
                    && (r.rect.max - band.max).length() < 1.0 =>
            {
                Some(r.fill)
            }
            _ => None,
        })
        .collect();
    assert!(
        fills.contains(&header),
        "the options bar {band:?} is not filled with the header shade: {fills:?}"
    );
    assert!(
        !fills.contains(&panel),
        "the options bar is still filled with the panel shade: {fills:?}"
    );
}
