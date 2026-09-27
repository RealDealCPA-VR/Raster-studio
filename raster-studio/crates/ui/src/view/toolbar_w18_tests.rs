//! W18-F: the options-bar controls, each driven through a headless frame of
//! the real bar - found by its id where the bar drew it, clicked, and what
//! the click raised (an intent, a tool option, a request for the shell) read
//! back.
//!
//! Test code only (declared `#[cfg(test)]` from `toolbar.rs`), which is what
//! the `no_localized_literals` gate reads that marker for.

use super::w18::*;
use super::*;
use tools::registry::bar_w18::{self as bar, CropBy};

/// The bar's slots back to their start before and after each test.
struct Clean;

impl Clean {
    fn new() -> Self {
        bar::reset();
        Clean
    }
}

impl Drop for Clean {
    fn drop(&mut self) {
        bar::reset();
    }
}

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
                egui::vec2(8000.0, 800.0),
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

fn writes(intents: &[Intent]) -> Vec<(&'static str, OptionValue)> {
    intents
        .iter()
        .filter_map(|i| match i {
            Intent::SetToolOption { key, value, .. } => Some((*key, *value)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_published_held_edit_draws_the_cancel_cross_before_the_commit_check() {
    let _clean = Clean::new();
    for tool in [ToolId::Type, ToolId::PerspectiveCrop, ToolId::Move] {
        let mut bar_ = Bar::new(tool);
        // Nothing published: neither control.
        assert!(bar_.rect(id(tool, CANCEL_KEY)).is_none(), "{tool:?}");
        assert!(bar_.rect(id(tool, COMMIT_KEY)).is_none(), "{tool:?}");
        bar::publish_pending(Some((tool, true)));
        let cross = bar_.rect(id(tool, CANCEL_KEY)).expect("the cross");
        let check = bar_.rect(id(tool, COMMIT_KEY)).expect("the check");
        assert!(cross.max.x <= check.min.x, "{tool:?}: the cross leads");
        // The cross asks the shell to cancel; the check confirms.
        assert!(bar_.click(id(tool, CANCEL_KEY)).is_empty());
        assert!(bar::take_cancel(), "{tool:?}: the cross posted Cancel");
        assert!(bar_
            .click(id(tool, COMMIT_KEY))
            .iter()
            .any(|i| matches!(i, Intent::ConfirmTool)));
        // Published for another tool: not this bar's.
        bar::publish_pending(Some((ToolId::Brush, true)));
        assert!(bar_.rect(id(tool, CANCEL_KEY)).is_none(), "{tool:?}");
        bar::publish_pending(None);
    }
}

#[test]
fn the_move_bar_shows_no_commit_for_the_show_transform_box_alone() {
    let _clean = Clean::new();
    let mut bar_ = Bar::new(ToolId::Move);
    let state = tools::transform::TransformState::new(raster::PixelRect::new(0, 0, 40, 30));
    bar_.w.canvas.sessions.transform = Some((state, tools::transform::TransformMode::Scale));
    bar::publish_pending(Some((ToolId::Move, false)));
    assert!(bar_.rect(id(ToolId::Move, COMMIT_KEY)).is_none());
    bar::publish_pending(Some((ToolId::Move, true)));
    assert!(bar_.rect(id(ToolId::Move, COMMIT_KEY)).is_some());
    assert!(bar_.rect(id(ToolId::Move, CANCEL_KEY)).is_some());
    // Free Transform keeps its session rule, now with the cross as well.
    let mut ft = Bar::new(ToolId::FreeTransform);
    let state = tools::transform::TransformState::new(raster::PixelRect::new(0, 0, 40, 30));
    ft.w.canvas.sessions.transform = Some((state, tools::transform::TransformMode::Scale));
    assert!(ft.rect(id(ToolId::FreeTransform, CANCEL_KEY)).is_some());
}

#[test]
fn the_parametric_shape_bar_shows_only_the_picked_shapes_keys() {
    let _clean = Clean::new();
    let tool = ToolId::Polygon;
    let mut bar_ = Bar::new(tool);
    assert_eq!(
        bar_.w.options.get(tool, "sides"),
        Some(OptionValue::Int(5)),
        "Photopea's polygon starts on five sides"
    );
    let shown = |bar_: &mut Bar, key: &'static str| bar_.rect(id(tool, key)).is_some();
    for (pshape, own) in [
        (0usize, &["sides", "corner_radius"][..]),
        (1, &["sides", "inner_ratio", "corner_radius"][..]),
        (
            2,
            &[
                "weight",
                "head_start",
                "head_end",
                "head_width",
                "head_length",
                "concavity",
            ][..],
        ),
        (3, &["rows", "cols", "border"][..]),
        (4, &["length"][..]),
    ] {
        if pshape > 0 {
            bar_.click(id(tool, "pshape"));
            let intents = bar_.click(super::super::ids::tool_option_choice(
                tool, "pshape", pshape,
            ));
            assert_eq!(
                writes(&intents),
                vec![("pshape", OptionValue::Choice(pshape))]
            );
        }
        for key in tools::shape::PARAMETRIC_KEYS
            .iter()
            .filter(|k| **k != "pshape")
        {
            assert_eq!(
                shown(&mut bar_, key),
                own.contains(key),
                "shape {pshape}: {key}"
            );
        }
        assert!(shown(&mut bar_, "fill"), "the paint keys always show");
    }
}

#[test]
fn the_magnetic_lasso_offers_feather_and_anti_alias_and_the_pencil_smoothing() {
    let _clean = Clean::new();
    let tool = ToolId::MagneticLasso;
    let mut lasso = Bar::new(tool);
    assert!(lasso.rect(id(tool, "feather")).is_some());
    let intents = lasso.click(id(tool, "antialias"));
    assert_eq!(
        writes(&intents),
        vec![("antialias", OptionValue::Bool(false))]
    );
    let mut pencil = Bar::new(ToolId::Pencil);
    assert!(
        pencil.rect(id(ToolId::Pencil, "smoothing")).is_some(),
        "the Pencil's Smooth"
    );
    // ...and the tools the registry builds answer both keys.
    let mut t = tools::registry::make(tool);
    t.set_setting("feather", tools::ToolSetting::Float(4.0))
        .unwrap();
    t.set_setting("antialias", tools::ToolSetting::Bool(false))
        .unwrap();
}

#[test]
fn the_zoom_bar_toggles_in_and_out_and_zoom_and_hand_offer_all_documents() {
    let _clean = Clean::new();
    let mut zoom = Bar::new(ToolId::Zoom);
    let r_in = zoom.rect(id(ToolId::Zoom, ZOOM_IN_KEY)).expect("Zoom In");
    let r_out = zoom.rect(id(ToolId::Zoom, ZOOM_OUT_KEY)).expect("Zoom Out");
    let fit = zoom
        .rect(id(ToolId::Zoom, super::w16k::PIXEL_KEY))
        .expect("Pixel to Pixel");
    assert!(r_in.max.x <= r_out.min.x && r_out.max.x <= fit.min.x);
    zoom.click(id(ToolId::Zoom, ZOOM_OUT_KEY));
    assert!(bar::nav().zoom_out, "Zoom Out is in");
    zoom.click(id(ToolId::Zoom, ZOOM_IN_KEY));
    assert!(!bar::nav().zoom_out, "Zoom In is back");
    zoom.click(id(ToolId::Zoom, ALL_DOCUMENTS_KEY));
    assert!(bar::nav().zoom_all_documents);
    assert!(!bar::nav().hand_all_documents, "each bar its own box");
    let mut hand = Bar::new(ToolId::Hand);
    hand.click(id(ToolId::Hand, ALL_DOCUMENTS_KEY));
    assert!(bar::nav().hand_all_documents);
    assert!(
        hand.rect(id(ToolId::Hand, super::w16k::FIT_KEY)).is_none(),
        "the Hand bar is its one box"
    );
}

#[test]
fn the_clone_bars_alt_toggle_arms_the_source_pick_and_k_is_read_from_the_keyboard() {
    let _clean = Clean::new();
    for tool in SOURCE_TOOLS {
        let mut b = Bar::new(tool);
        b.click(id(tool, SELECT_SOURCE_KEY));
        assert!(bar::select_source_armed(), "{tool:?}");
        b.click(id(tool, SELECT_SOURCE_KEY));
        assert!(!bar::select_source_armed(), "{tool:?}: a second click");
    }
    assert!(Bar::new(ToolId::Brush)
        .rect(id(ToolId::Brush, SELECT_SOURCE_KEY))
        .is_none());
    let mut b = Bar::new(ToolId::CloneStamp);
    let key = |pressed| egui::Event::Key {
        key: egui::Key::K,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    };
    b.frame(vec![key(true)]);
    assert!(bar::source_key_held(), "K down");
    b.frame(vec![key(false)]);
    assert!(!bar::source_key_held(), "K up");
}

#[test]
fn the_bucket_pattern_picker_lists_the_patterns_while_the_fill_is_pattern() {
    let _clean = Clean::new();
    let tool = ToolId::PaintBucket;
    let mut b = Bar::new(tool);
    assert!(
        b.rect(id(tool, PATTERN_PICKER_KEY)).is_none(),
        "not while the Fill is the foreground"
    );
    b.click(id(tool, tools::bucket::FILL_SOURCE_KEY));
    b.click(super::super::ids::tool_option_choice(
        tool,
        tools::bucket::FILL_SOURCE_KEY,
        1,
    ));
    bar::publish_patterns(vec!["Reds".into(), "Blues".into()], Some("Reds".into()));
    b.click(id(tool, PATTERN_PICKER_KEY));
    b.click(pattern_row_id(1));
    assert_eq!(bar::take_pattern_pick().as_deref(), Some("Blues"));
}

#[test]
fn a_crop_by_row_posts_the_box_request_and_crops_nothing() {
    let _clean = Clean::new();
    let mut b = Bar::new(ToolId::Crop);
    b.click(id(ToolId::Crop, super::w16k::CROP_BY_KEY));
    let intents = b.click(super::w16k::crop_by_id(MenuAction::CropToLayer));
    assert!(
        !intents.iter().any(|i| matches!(i, Intent::Action(_))),
        "{intents:?}"
    );
    assert_eq!(bar::take_crop_by(), Some(CropBy::CurrentLayer));
    for (action, by) in super::w16k::CROP_BY.iter().zip(CropBy::ALL) {
        assert_eq!(super::w16k::crop_by_request(*action), by);
    }
}
