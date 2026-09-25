//! W16-K: the options-bar rows, each driven through a headless frame of the
//! real bar - the button found by its id where the bar drew it, clicked, and
//! the intent the click raised read back.
//!
//! Test code only (declared `#[cfg(test)]` from `toolbar.rs`), which is what
//! the `no_localized_literals` gate reads that marker for.

use super::w16k::*;
use super::*;
use crate::menu::{ArtboardSide, WarpTextItem, ZoomCommand};

struct Bar {
    ctx: egui::Context,
    w: Workspace,
}

impl Bar {
    fn new(tool: ToolId) -> Self {
        let ctx = egui::Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut w = Workspace::new();
        w.absorb(&Intent::SelectTool(tool));
        Self { ctx, w }
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(6000.0, 800.0),
            )),
            events,
            ..Default::default()
        };
        let w = &mut self.w;
        let _ = self.ctx.run(input, |ctx| tool_options(w, ctx));
        self.w.drain_intents()
    }

    fn rect(&mut self, id: egui::Id) -> Option<egui::Rect> {
        for _ in 0..3 {
            self.frame(Vec::new());
        }
        self.ctx.read_response(id).map(|r| r.rect)
    }

    fn click(&mut self, id: egui::Id) -> Vec<Intent> {
        let at = self
            .rect(id)
            .unwrap_or_else(|| panic!("{id:?} was not drawn"))
            .center();
        self.frame(vec![
            egui::Event::PointerMoved(at),
            press(at, true),
            press(at, false),
        ])
    }
}

fn press(at: egui::Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    }
}

fn id(tool: ToolId, key: &'static str) -> egui::Id {
    super::super::ids::tool_option(tool, key)
}

fn actions(intents: &[Intent]) -> Vec<MenuAction> {
    intents
        .iter()
        .filter_map(|i| match i {
            Intent::Action(a) => Some(*a),
            _ => None,
        })
        .collect()
}

#[test]
fn the_view_tools_bars_fit_zoom_to_100_and_reset_the_rotation() {
    for tool in [ToolId::Hand, ToolId::Zoom] {
        let mut bar = Bar::new(tool);
        assert_eq!(
            actions(&bar.click(id(tool, FIT_KEY))),
            vec![MenuAction::Zoom(ZoomCommand::FitOnScreen)],
            "{tool:?}"
        );
        assert_eq!(
            actions(&bar.click(id(tool, PIXEL_KEY))),
            vec![MenuAction::Zoom(ZoomCommand::ActualPixels)],
            "{tool:?}"
        );
    }
    let mut bar = Bar::new(ToolId::RotateView);
    assert_eq!(
        actions(&bar.click(id(ToolId::RotateView, ROTATE_RESET_KEY))),
        vec![MenuAction::ResetViewRotation]
    );
    // A tool without the row does not draw it.
    let mut brush = Bar::new(ToolId::Brush);
    assert!(brush.rect(id(ToolId::Brush, FIT_KEY)).is_none());
}

#[test]
fn selection_bars_offer_refine_edge_and_the_wand_group_select_subject() {
    let mut marquee = Bar::new(ToolId::RectMarquee);
    assert_eq!(
        actions(&marquee.click(id(ToolId::RectMarquee, REFINE_EDGE_KEY))),
        vec![MenuAction::RefineEdge]
    );
    assert!(
        marquee
            .rect(id(ToolId::RectMarquee, SELECT_SUBJECT_KEY))
            .is_none(),
        "the marquee has no Select Subject in Photopea"
    );
    let mut wand = Bar::new(ToolId::MagicWand);
    assert_eq!(
        actions(&wand.click(id(ToolId::MagicWand, SELECT_SUBJECT_KEY))),
        vec![MenuAction::SelectSubject]
    );
    assert_eq!(
        actions(&wand.click(id(ToolId::MagicWand, REFINE_EDGE_KEY))),
        vec![MenuAction::RefineEdge]
    );
}

#[test]
fn the_type_bar_warps_and_converts_the_text() {
    let mut bar = Bar::new(ToolId::Type);
    assert_eq!(
        actions(&bar.click(id(ToolId::Type, TYPE_WARP_KEY))),
        vec![MenuAction::WarpText(WarpTextItem::Dialog)]
    );
    assert_eq!(
        actions(&bar.click(id(ToolId::Type, TYPE_CONVERT_KEY))),
        vec![MenuAction::ConvertTextToShape]
    );
}

#[test]
fn the_brush_picker_applies_a_preset_as_the_brushes_panel_does() {
    let mut bar = Bar::new(ToolId::Brush);
    let before = bar
        .w
        .options
        .get(ToolId::Brush, "size")
        .and_then(OptionValue::as_float);
    // Open the list, then pick the preset whose size differs from the tool's.
    let index = bar
        .w
        .brushes
        .presets()
        .iter()
        .position(|p| Some(p.settings.size) != before)
        .expect("a preset of another size");
    let want = bar.w.brushes.presets()[index].settings.size;
    bar.click(id(ToolId::Brush, BRUSH_PICKER_KEY));
    let intents = bar.click(brush_preset_id(ToolId::Brush, index));
    assert!(
        intents.iter().any(|i| matches!(
            i,
            Intent::SetToolOption {
                tool: ToolId::Brush,
                key: "size",
                value: OptionValue::Float(v),
            } if *v == want
        )),
        "{intents:?}"
    );
    assert_eq!(bar.w.brushes.active(), Some(index));
    // The Pencil's bar has the picker too; a tool that paints no brush not.
    let mut pencil = Bar::new(ToolId::Pencil);
    assert!(pencil.rect(id(ToolId::Pencil, BRUSH_PICKER_KEY)).is_some());
    let mut crop = Bar::new(ToolId::Crop);
    assert!(crop.rect(id(ToolId::Crop, BRUSH_PICKER_KEY)).is_none());
}

#[test]
fn crop_by_lists_photopeas_four_rows_and_current_layer_crops_to_it() {
    let mut bar = Bar::new(ToolId::Crop);
    bar.click(id(ToolId::Crop, CROP_BY_KEY));
    for action in CROP_BY {
        assert!(bar.rect(crop_by_id(action)).is_some(), "{action:?} row");
    }
    assert_eq!(
        actions(&bar.click(crop_by_id(MenuAction::CropToLayer))),
        vec![MenuAction::CropToLayer]
    );
}

#[test]
fn the_artboard_bar_adds_a_neighbour_on_each_side() {
    let mut bar = Bar::new(ToolId::Artboard);
    for (side, key) in ARTBOARD_ADD_KEYS {
        assert_eq!(
            actions(&bar.click(id(ToolId::Artboard, key))),
            vec![MenuAction::ArtboardNeighbour(side)]
        );
    }
    assert_eq!(ARTBOARD_ADD_KEYS[0].0, ArtboardSide::Left);
}

#[test]
fn the_pencil_bar_draws_its_two_pressure_toggles() {
    let mut bar = Bar::new(ToolId::Pencil);
    for key in ["size_pressure", "opacity_pressure"] {
        assert_eq!(
            bar.w
                .options
                .get(ToolId::Pencil, key)
                .and_then(OptionValue::as_bool),
            Some(false),
            "{key} starts off"
        );
        let intents = bar.click(id(ToolId::Pencil, key));
        assert!(
            intents.contains(&Intent::SetToolOption {
                tool: ToolId::Pencil,
                key,
                value: OptionValue::Bool(true),
            }),
            "{key}: {intents:?}"
        );
    }
}
