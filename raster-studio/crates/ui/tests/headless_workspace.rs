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
