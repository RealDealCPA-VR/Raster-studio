//! W16-C: the options bar shows and accepts Photopea's numbers (Tolerance
//! 0-255, Opacity in %), offers the Paint Bucket's Fill source, and draws a
//! Commit check while an edit is pending — each driven through a headless
//! frame of the real bar, reading the drawn text and the emitted intents.
//!
//! Test code only (declared `#[cfg(test)]` from `toolbar.rs`), which is what
//! the `no_localized_literals` gate reads that marker for.

use super::*;
use raster::PixelRect;
use tools::transform::{TransformMode, TransformState};

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

    /// One frame; returns the intents and every string the frame painted.
    fn frame_with_text(&mut self, events: Vec<egui::Event>) -> (Vec<Intent>, Vec<String>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(4000.0, 400.0),
            )),
            events,
            ..Default::default()
        };
        let w = &mut self.w;
        let out = self.ctx.run(input, |ctx| tool_options(w, ctx));
        let mut texts = Vec::new();
        collect_text(out.shapes.iter().map(|c| &c.shape), &mut texts);
        (self.w.drain_intents(), texts)
    }

    fn frame(&mut self, events: Vec<egui::Event>) -> Vec<Intent> {
        self.frame_with_text(events).0
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

    /// Click a numeric field (which selects its text), type `text` and press
    /// Enter: what a user does to set a number.
    fn type_into(&mut self, id: egui::Id, text: &str) -> Vec<Intent> {
        let mut out = self.click(id);
        out.extend(self.frame(vec![egui::Event::Text(text.to_owned())]));
        out.extend(self.frame(vec![
            key(egui::Key::Enter, true),
            key(egui::Key::Enter, false),
        ]));
        out.extend(self.frame(Vec::new()));
        out
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

fn key(key: egui::Key, pressed: bool) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::default(),
    }
}

fn collect_text<'a>(shapes: impl Iterator<Item = &'a egui::Shape>, out: &mut Vec<String>) {
    for shape in shapes {
        match shape {
            egui::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            egui::Shape::Vec(inner) => collect_text(inner.iter(), out),
            _ => {}
        }
    }
}

fn last_float(intents: &[Intent], key: &str) -> Option<f32> {
    intents.iter().rev().find_map(|i| match i {
        Intent::SetToolOption {
            key: k,
            value: OptionValue::Float(v),
            ..
        } if *k == key => Some(*v),
        _ => None,
    })
}

#[test]
fn typing_32_into_the_bucket_tolerance_stores_32_over_255() {
    let tool = ToolId::PaintBucket;
    let mut bar = Bar::new(tool);
    // Start away from the default (32 / 255) so typing 32 is a change.
    assert!(bar
        .w
        .options
        .set(tool, "tolerance", OptionValue::Float(100.0 / 255.0)));
    let id = super::super::ids::tool_option(tool, "tolerance");
    let intents = bar.type_into(id, "32");
    let stored = last_float(&intents, "tolerance").expect("typing 32 wrote the Tolerance");
    assert!(
        (stored - 32.0 / 255.0).abs() < 1e-6,
        "typing 32 stored {stored}, not 32/255 (a raw fraction field clamps it to 1.0 and \
         selects everything)"
    );
    assert_eq!(
        bar.w.options.get(tool, "tolerance"),
        Some(OptionValue::Float(stored))
    );
    // ...and the field reads the level back, not the fraction.
    let (_, texts) = bar.frame_with_text(Vec::new());
    assert!(
        texts.iter().any(|t| t == "32"),
        "the Tolerance field shows 32: {texts:?}"
    );
}

#[test]
fn opacity_shows_100_percent_on_the_brush_and_the_bucket() {
    for tool in [ToolId::Brush, ToolId::PaintBucket] {
        let mut bar = Bar::new(tool);
        bar.frame(Vec::new());
        let (_, texts) = bar.frame_with_text(Vec::new());
        assert!(
            texts.iter().any(|t| t == "100%"),
            "{tool:?}'s Opacity reads 100%: {texts:?}"
        );
        assert!(
            !texts.iter().any(|t| t == "1"),
            "{tool:?} still shows a raw 1: {texts:?}"
        );
    }
    // The brush size is in pixels and its spacing a percentage.
    let mut bar = Bar::new(ToolId::Brush);
    bar.frame(Vec::new());
    let (_, texts) = bar.frame_with_text(Vec::new());
    assert!(texts.iter().any(|t| t == "24 px"), "{texts:?}");
    assert!(texts.iter().any(|t| t == "25%"), "{texts:?}");
}

#[test]
fn typing_50_into_opacity_stores_one_half() {
    let tool = ToolId::Brush;
    let mut bar = Bar::new(tool);
    let intents = bar.type_into(super::super::ids::tool_option(tool, "opacity"), "50");
    assert_eq!(last_float(&intents, "opacity"), Some(0.5));
}

#[test]
fn the_bucket_offers_foreground_or_pattern_as_its_fill() {
    let tool = ToolId::PaintBucket;
    let key = tools::bucket::FILL_SOURCE_KEY;
    let mut bar = Bar::new(tool);
    bar.click(super::super::ids::tool_option(tool, key));
    let intents = bar.click(super::super::ids::tool_option_choice(tool, key, 1));
    assert!(
        intents.iter().any(|i| matches!(
            i,
            Intent::SetToolOption { tool: t, key: k, value: OptionValue::Choice(1) }
                if *t == tool && *k == key
        )),
        "picking Pattern wrote the Fill source: {intents:?}"
    );
}

#[test]
fn the_commit_check_shows_only_while_an_edit_is_pending_and_confirms_it() {
    let tool = ToolId::FreeTransform;
    let id = super::super::ids::tool_option(tool, COMMIT_KEY);
    let mut bar = Bar::new(tool);
    assert!(bar.rect(id).is_none(), "no pending transform, no Commit");

    bar.w.canvas.sessions.transform = Some((
        TransformState::new(PixelRect::new(0, 0, 200, 100)),
        TransformMode::Scale,
    ));
    assert!(bar.rect(id).is_some(), "a live transform shows Commit");
    assert_eq!(bar.click(id), vec![Intent::ConfirmTool]);

    // A crop box is a pending edit too.
    let mut crop = Bar::new(ToolId::Crop);
    crop.w.canvas.sessions.crop = Some(crate::canvas::DocRect::from_corners(
        glam::Vec2::new(0.0, 0.0),
        glam::Vec2::new(10.0, 10.0),
    ));
    let crop_id = super::super::ids::tool_option(ToolId::Crop, COMMIT_KEY);
    assert_eq!(crop.click(crop_id), vec![Intent::ConfirmTool]);
}

/// Round 2: the Blur / Sharpen Strength is Photopea's 1-100 % (its
/// `strn` option), not the tool's raw radius or amount. The Sharpen's
/// default 1x of 4x reads 25%, and typing 100 stores the top, 4x.
#[test]
fn blur_and_sharpen_strength_are_percentages_on_the_bar() {
    let tool = ToolId::Sharpen;
    let mut bar = Bar::new(tool);
    bar.frame(Vec::new());
    let (_, texts) = bar.frame_with_text(Vec::new());
    assert!(
        texts.iter().any(|t| t == "25%"),
        "the Sharpen's Strength reads 25%: {texts:?}"
    );
    let intents = bar.type_into(super::super::ids::tool_option(tool, "amount"), "100");
    let stored = last_float(&intents, "amount").expect("typing 100 wrote the Strength");
    assert!((stored - 4.0).abs() < 1e-4, "100% stored {stored}, not 4");

    let tool = ToolId::Blur;
    let mut bar = Bar::new(tool);
    let intents = bar.type_into(super::super::ids::tool_option(tool, "radius"), "50");
    let stored = last_float(&intents, "radius").expect("typing 50 wrote the Strength");
    assert!(
        (stored - 32.0).abs() < 1e-3,
        "50% stored {stored}, not 32 px"
    );
    let (_, texts) = bar.frame_with_text(Vec::new());
    assert!(texts.iter().any(|t| t == "50%"), "{texts:?}");
}
