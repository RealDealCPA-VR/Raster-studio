//! The serialised model and its relationship to `layer_model::TextLayer`.

use serde_json::json;
use text_engine::{
    is_available, resolve_style, Alignment, CharStyle, FontSlant, FontWeight, KernAdjustment,
    LineHeight, ParagraphStyle, ScriptPosition, StyleOverride, StyleRun, TextFrame, TextRun,
};

#[test]
fn the_engine_reports_itself_available() {
    assert!(
        is_available(),
        "the placeholder is gone; the rest of this crate's tests are the justification"
    );
}

#[test]
fn a_text_layer_round_trips_through_a_text_run() {
    let layer = layer_model::TextLayer {
        text: "Hello, layer".to_string(),
        font_family: "DejaVu Sans".to_string(),
        size_px: 24.0,
    };
    let run = TextRun::from(&layer);
    assert_eq!(run.text, layer.text);
    assert_eq!(run.style.family, layer.font_family);
    assert_eq!(run.style.size_px, layer.size_px);
    assert!(run.runs.is_empty());
    assert_eq!(run.frame, TextFrame::Point);

    let back = layer_model::TextLayer::from(&run);
    assert_eq!(back, layer, "the three stored fields survive the trip");

    // And the owning conversions agree with the borrowing ones.
    assert_eq!(layer_model::TextLayer::from(run.clone()), back);
    assert_eq!(TextRun::from(layer.clone()), run);
}

#[test]
fn the_default_text_layer_round_trips_too() {
    let layer = layer_model::TextLayer::default();
    let back = layer_model::TextLayer::from(TextRun::from(&layer));
    assert_eq!(
        back, layer,
        "no field may be silently coerced on the way through"
    );
}

#[test]
fn a_rich_run_degrades_to_the_stored_three_fields() {
    let run = TextRun::paragraph("styled", "DejaVu Sans", 18.0, 300.0)
        .with_runs(vec![StyleRun::new(
            0,
            3,
            StyleOverride::default().with_weight(FontWeight::BOLD),
        )])
        .with_origin([5.0, 6.0]);
    let layer = layer_model::TextLayer::from(&run);
    assert_eq!(layer.text, "styled");
    assert_eq!(layer.font_family, "DejaVu Sans");
    assert_eq!(layer.size_px, 18.0);
}

#[test]
fn the_model_round_trips_through_json() {
    let run = TextRun::paragraph("a\nb", "DejaVu Sans", 21.5, 120.0)
        .with_runs(vec![
            StyleRun::new(
                0,
                1,
                StyleOverride::default()
                    .with_weight(FontWeight::SEMI_BOLD)
                    .with_slant(FontSlant::Italic)
                    .with_color([0.1, 0.2, 0.3, 1.0])
                    .with_script(ScriptPosition::Subscript)
                    .with_underline(true),
            ),
            StyleRun::new(2, 3, StyleOverride::default().with_family("DejaVu Serif")),
        ])
        .with_kerning(vec![KernAdjustment::new(1, -35.0)])
        .with_paragraph(ParagraphStyle {
            alignment: Alignment::Justify,
            line_height: LineHeight::Absolute(30.0),
            first_line_indent: 12.0,
            space_before: 3.0,
            space_after: 4.0,
        })
        .with_origin([7.0, 8.0]);

    let text = serde_json::to_string(&run).expect("serialises");
    let back: TextRun = serde_json::from_str(&text).expect("deserialises");
    assert_eq!(back, run);
}

#[test]
fn the_serialised_shape_is_stable() {
    let run = TextRun::point("hi", "DejaVu Sans", 12.0);
    let value = serde_json::to_value(&run).expect("serialises");
    assert_eq!(
        value,
        json!({
            "text": "hi",
            "style": {
                "family": "DejaVu Sans",
                "size_px": 12.0,
                "weight": 400,
                "slant": "Normal",
                "color": [0.0, 0.0, 0.0, 1.0],
                "underline": false,
                "strikethrough": false,
                "script": "Normal",
                "tracking": 0.0,
                "ligatures": true,
                "kerning": true,
                "allow_synthetic_bold": true,
                "allow_synthetic_italic": true
            },
            "runs": [],
            "paragraph": {
                "alignment": "Left",
                // The stored value is an `f32`; widening it here keeps the
                // comparison exact rather than smuggling in an `f64` literal.
                "line_height": { "Multiple": f64::from(1.2_f32) },
                "first_line_indent": 0.0,
                "space_before": 0.0,
                "space_after": 0.0
            },
            "frame": "Point",
            "kerning": [],
            "origin": [0.0, 0.0]
        })
    );
}

#[test]
fn a_boxed_frame_serialises_as_a_named_variant() {
    let run = TextRun::paragraph("hi", "DejaVu Sans", 12.0, 80.0);
    let value = serde_json::to_value(&run).expect("serialises");
    assert_eq!(
        value["frame"],
        json!({ "Box": { "width": 80.0, "height": null } })
    );
    assert_eq!(run.wrap_width(), Some(80.0));
    assert_eq!(TextRun::point("hi", "x", 1.0).wrap_width(), None);
}

#[test]
fn missing_fields_fall_back_to_defaults() {
    // Older documents that only carry the text must still load.
    let run: TextRun = serde_json::from_str(r#"{"text":"legacy"}"#).expect("deserialises");
    assert_eq!(run.text, "legacy");
    assert_eq!(
        run,
        TextRun {
            text: "legacy".to_string(),
            ..TextRun::default()
        }
    );
    assert_eq!(run.style, CharStyle::default());
    assert_eq!(run.style.size_px, 16.0);
    assert_eq!(run.paragraph.line_height, LineHeight::Multiple(1.2));

    let style: CharStyle = serde_json::from_str(r#"{"size_px":9.0}"#).expect("deserialises");
    assert_eq!(style.size_px, 9.0);
    assert_eq!(style.weight, FontWeight::NORMAL);
    assert!(style.ligatures);
}

#[test]
fn style_runs_resolve_in_order_with_later_runs_winning() {
    let base = CharStyle {
        family: "Base".to_string(),
        size_px: 10.0,
        ..CharStyle::default()
    };
    let runs = vec![
        StyleRun::new(0, 6, StyleOverride::default().with_weight(FontWeight::BOLD)),
        StyleRun::new(
            2,
            4,
            StyleOverride::default()
                .with_weight(FontWeight::LIGHT)
                .with_size_px(20.0),
        ),
    ];

    assert_eq!(resolve_style(&base, &runs, 0).weight, FontWeight::BOLD);
    assert_eq!(resolve_style(&base, &runs, 2).weight, FontWeight::LIGHT);
    assert_eq!(resolve_style(&base, &runs, 2).size_px, 20.0);
    assert_eq!(resolve_style(&base, &runs, 4).weight, FontWeight::BOLD);
    assert_eq!(resolve_style(&base, &runs, 4).size_px, 10.0);
    assert_eq!(resolve_style(&base, &runs, 9), base, "outside every run");
    // Inheritance: the family was never overridden.
    assert_eq!(resolve_style(&base, &runs, 3).family, "Base");
}

#[test]
fn weight_synthesis_uses_a_tolerance_not_equality() {
    assert!(FontWeight::BOLD.needs_synthesis(FontWeight::NORMAL));
    assert!(!FontWeight::BOLD.needs_synthesis(FontWeight::BOLD));
    assert!(
        !FontWeight::BOLD.needs_synthesis(FontWeight::SEMI_BOLD),
        "600 is close enough to stand in for 700"
    );
    assert!(!FontWeight::NORMAL.needs_synthesis(FontWeight::BLACK));
}

#[test]
fn script_positions_scale_and_shift() {
    assert_eq!(ScriptPosition::Normal.size_factor(), 1.0);
    assert_eq!(ScriptPosition::Normal.baseline_shift(40.0), 0.0);
    assert!(ScriptPosition::Superscript.baseline_shift(40.0) < 0.0);
    assert!(ScriptPosition::Subscript.baseline_shift(40.0) > 0.0);
    assert_eq!(
        ScriptPosition::Superscript.size_factor(),
        ScriptPosition::Subscript.size_factor()
    );

    let style = CharStyle {
        size_px: 40.0,
        script: ScriptPosition::Superscript,
        ..CharStyle::default()
    };
    assert!((style.effective_size_px() - 40.0 * 0.583).abs() < 1e-3);
}

#[test]
fn line_height_resolves_against_the_base_size() {
    assert_eq!(LineHeight::Multiple(1.5).resolve(20.0), 30.0);
    assert_eq!(LineHeight::Absolute(33.0).resolve(20.0), 33.0);
    assert_eq!(LineHeight::default(), LineHeight::Multiple(1.2));
}
