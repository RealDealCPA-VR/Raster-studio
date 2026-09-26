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

/// Every string one frame of the bar painted, trimmed.
fn painted(bar: &mut Bar) -> Vec<String> {
    fn walk(shapes: &[egui::Shape], out: &mut Vec<String>) {
        for shape in shapes {
            match shape {
                egui::Shape::Text(t) => out.push(t.galley.text().trim().to_string()),
                egui::Shape::Vec(inner) => walk(inner, out),
                _ => {}
            }
        }
    }
    for _ in 0..2 {
        bar.frame(Vec::new());
    }
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(6000.0, 800.0),
        )),
        ..Default::default()
    };
    let w = &mut bar.w;
    let full = bar.ctx.run(input, |ctx| tool_options(w, ctx));
    let shapes: Vec<egui::Shape> = full.shapes.into_iter().map(|c| c.shape).collect();
    let mut out = Vec::new();
    walk(&shapes, &mut out);
    out
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

#[test]
fn the_zoom_bar_is_photopeas_pixel_to_pixel_and_fit_the_area() {
    let mut bar = Bar::new(ToolId::Zoom);
    let words = painted(&mut bar);
    for caption in ["Pixel to Pixel", "Fit The Area"] {
        assert!(words.iter().any(|w| w == caption), "{caption}: {words:?}");
    }
    assert_eq!(
        actions(&bar.click(id(ToolId::Zoom, PIXEL_KEY))),
        vec![MenuAction::Zoom(ZoomCommand::ActualPixels)]
    );
    assert_eq!(
        actions(&bar.click(id(ToolId::Zoom, FIT_KEY))),
        vec![MenuAction::Zoom(ZoomCommand::FitOnScreen)]
    );
    // Photopea's Hand bar has neither (its one control, All Documents, is
    // not built), nor has a tool without the row.
    for tool in [ToolId::Hand, ToolId::Brush] {
        let mut other = Bar::new(tool);
        assert!(other.rect(id(tool, FIT_KEY)).is_none(), "{tool:?}");
        assert!(other.rect(id(tool, PIXEL_KEY)).is_none(), "{tool:?}");
    }
}

#[test]
fn the_rotate_view_bar_types_an_angle_and_resets() {
    use crate::panels::panel_menus_w16::{take_requests, PanelRequest};
    let _ = take_requests();
    let mut bar = Bar::new(ToolId::RotateView);
    let words = painted(&mut bar);
    for caption in ["Angle", "Reset"] {
        assert!(words.iter().any(|w| w == caption), "{caption}: {words:?}");
    }
    // Enter commits what was typed as the Navigator's own request.
    bar.click(id(ToolId::RotateView, ROTATE_ANGLE_KEY));
    bar.frame(vec![
        key(egui::Key::A, egui::Modifiers::COMMAND),
        egui::Event::Text("30".to_string()),
    ]);
    bar.frame(vec![key(egui::Key::Enter, egui::Modifiers::default())]);
    assert_eq!(take_requests(), vec![PanelRequest::SetViewAngle(30.0)]);
    assert_eq!(
        actions(&bar.click(id(ToolId::RotateView, ROTATE_RESET_KEY))),
        vec![MenuAction::ResetViewRotation]
    );
}

#[test]
fn the_bar_captions_are_drawn_in_the_active_language() {
    crate::strings::with_locale(crate::strings::Locale::De, || {
        let mut crop = Bar::new(ToolId::Crop);
        crop.click(id(ToolId::Crop, CROP_BY_KEY));
        let words = painted(&mut crop);
        for caption in ["Alle Ebenen", "Aktuelle Ebene", "Auswahl"] {
            assert!(words.iter().any(|w| w == caption), "{caption}: {words:?}");
        }
        let mut marquee = Bar::new(ToolId::RectMarquee);
        let words = painted(&mut marquee);
        assert!(
            words.iter().any(|w| w == "Kante verbessern\u{2026}"),
            "Refine Edge in German: {words:?}"
        );
        assert!(!words.iter().any(|w| w.starts_with("Refine Edge")));
        let mut rotate = Bar::new(ToolId::RotateView);
        let words = painted(&mut rotate);
        assert!(words.iter().any(|w| w == "Winkel"), "{words:?}");
        assert!(words.iter().any(|w| w == "Zur\u{fc}cksetzen"), "{words:?}");
    });
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
    // Every selection bar has Refine Edge; only the wand group (Magic
    // Wand, Quick Selection, Object Selection) leads with Select Subject.
    for tool in SELECTION_TOOLS {
        let mut bar = Bar::new(tool);
        assert!(bar.rect(id(tool, REFINE_EDGE_KEY)).is_some(), "{tool:?}");
        let wand_group = matches!(
            tool,
            ToolId::MagicWand | ToolId::QuickSelect | ToolId::ObjectSelection
        );
        assert_eq!(
            bar.rect(id(tool, SELECT_SUBJECT_KEY)).is_some(),
            wand_group,
            "{tool:?}: Select Subject only on the wand group"
        );
    }
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
    // In Photopea's words (its string table's 17.0, 17.1, 11.12.0, 17.2).
    let words = painted(&mut bar);
    for caption in ["All Layers", "Current Layer", "Trim", "Selection"] {
        assert!(words.iter().any(|w| w == caption), "{caption}: {words:?}");
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
