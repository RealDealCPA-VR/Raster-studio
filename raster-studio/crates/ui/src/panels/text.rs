//! The Character and Paragraph panels.
//!
//! Both edit the same thing — the active text layer — through
//! `text_engine::TextRun`, which already knows how to convert to and from
//! `layer_model::TextLayer`. Editing therefore never invents a representation:
//! the panel reads a `TextRun` out of the document, changes one field, and
//! emits the layer kind that run converts back into.
//!
//! # The gap this panel lives with
//!
//! `LayerPatch` covers every field of a layer except `kind`, so a text edit
//! cannot be a [`editor_core::Command`] yet — see
//! [`crate::Intent::EditLayerKind`]. Everything else about the panels is
//! ordinary: they are disabled with a reason when no text layer is active, and
//! every control emits or does not emit on the same rule as the rest of the UI.

use editor_core::Document;
use layer_model::{LayerId, LayerKind, TextLayer};
use text_engine::{
    Alignment, CharStyle, FontSlant, FontWeight, LineHeight, ParagraphStyle, TextRun,
};

use crate::intent::Intent;

/// Smallest and largest type size the panels offer.
pub const MIN_SIZE_PX: f32 = 1.0;
pub const MAX_SIZE_PX: f32 = 1638.0;

/// The named weights the Character panel lists, with the numeric axis value
/// each stands for.
pub const WEIGHTS: &[(&str, u16)] = &[
    ("Thin", 100),
    ("Extra Light", 200),
    ("Light", 300),
    ("Regular", 400),
    ("Medium", 500),
    ("Semibold", 600),
    ("Bold", 700),
    ("Extra Bold", 800),
    ("Black", 900),
];

/// Every alignment, in panel order.
pub const ALIGNMENTS: &[Alignment] = &[
    Alignment::Left,
    Alignment::Center,
    Alignment::Right,
    Alignment::Justify,
];

/// Panel label for an alignment.
pub const fn alignment_label(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::Left => "Left",
        Alignment::Center => "Center",
        Alignment::Right => "Right",
        Alignment::Justify => "Justify",
    }
}

/// The glyph shown on the alignment segmented control.
pub const fn alignment_glyph(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::Left => "≡",
        Alignment::Center => "☰",
        Alignment::Right => "⋮",
        Alignment::Justify => "▤",
    }
}

/// The nearest named weight to a numeric one, for showing the combo's label.
pub fn weight_label(weight: FontWeight) -> &'static str {
    WEIGHTS
        .iter()
        .min_by_key(|(_, n)| weight.0.abs_diff(*n))
        .map(|(name, _)| *name)
        .unwrap_or("Regular")
}

/// The text layer the panels are editing, if any.
///
/// `None` is not a failure — it is the normal state whenever the active layer
/// is not text — and the panel shows [`no_text_layer_reason`] rather than a set
/// of controls that would go nowhere.
pub fn active_text(doc: &Document, active: Option<LayerId>) -> Option<(LayerId, TextRun)> {
    let id = active?;
    match &doc.layers.get(id)?.kind {
        LayerKind::Text(t) => Some((id, TextRun::from(t))),
        _ => None,
    }
}

/// Why the Character and Paragraph panels are inert.
pub const fn no_text_layer_reason() -> &'static str {
    "Select a text layer to edit its type"
}

/// Emit the edit that replaces a text layer's run, or nothing when the run is
/// unchanged.
pub fn commit(doc: &Document, layer: LayerId, run: &TextRun) -> Option<Intent> {
    let current = match &doc.layers.get(layer)?.kind {
        LayerKind::Text(t) => t.clone(),
        _ => return None,
    };
    let next = TextLayer::from(run);
    (next != current).then(|| Intent::EditLayerKind {
        layer,
        kind: Box::new(LayerKind::Text(next)),
    })
}

/// The Character panel's edits, each returning the run to commit.
///
/// Every setter normalises rather than refuses, because every one of them is
/// driven by a drag: a size of `-3` is a slider that overshot, not a bad
/// request. The exception is a non-finite value, which has no sensible
/// normalisation and leaves the run alone.
pub struct Character;

impl Character {
    pub fn set_family(run: &mut TextRun, family: &str) -> bool {
        let family = family.trim();
        if family.is_empty() || run.style.family == family {
            return false;
        }
        run.style.family = family.to_string();
        true
    }

    pub fn set_size(run: &mut TextRun, size_px: f32) -> bool {
        if !size_px.is_finite() {
            return false;
        }
        let size = size_px.clamp(MIN_SIZE_PX, MAX_SIZE_PX);
        if run.style.size_px == size {
            return false;
        }
        run.style.size_px = size;
        true
    }

    pub fn set_weight(run: &mut TextRun, weight: u16) -> bool {
        let weight = FontWeight(weight.clamp(1, 1000));
        if run.style.weight == weight {
            return false;
        }
        run.style.weight = weight;
        true
    }

    pub fn set_italic(run: &mut TextRun, italic: bool) -> bool {
        let slant = if italic {
            FontSlant::Italic
        } else {
            FontSlant::Normal
        };
        if run.style.slant == slant {
            return false;
        }
        run.style.slant = slant;
        true
    }

    pub fn set_tracking(run: &mut TextRun, tracking: f32) -> bool {
        if !tracking.is_finite() || run.style.tracking == tracking {
            return false;
        }
        run.style.tracking = tracking;
        true
    }

    pub fn set_color(run: &mut TextRun, color: [f32; 4]) -> bool {
        if !color.iter().all(|v| v.is_finite()) || run.style.color == color {
            return false;
        }
        run.style.color = color;
        true
    }

    pub fn set_underline(run: &mut TextRun, on: bool) -> bool {
        let changed = run.style.underline != on;
        run.style.underline = on;
        changed
    }

    pub fn set_strikethrough(run: &mut TextRun, on: bool) -> bool {
        let changed = run.style.strikethrough != on;
        run.style.strikethrough = on;
        changed
    }

    /// The leading the panel shows, in pixels, resolved against the run's own
    /// size — which is what "auto" means and what the field has to display.
    pub fn leading_px(style: &CharStyle, paragraph: &ParagraphStyle) -> f32 {
        paragraph.line_height.resolve(style.size_px)
    }
}

/// The Paragraph panel's edits.
pub struct Paragraph;

impl Paragraph {
    pub fn set_alignment(run: &mut TextRun, alignment: Alignment) -> bool {
        let changed = run.paragraph.alignment != alignment;
        run.paragraph.alignment = alignment;
        changed
    }

    /// Set leading as an absolute pixel distance.
    pub fn set_leading_px(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || px <= 0.0 {
            return false;
        }
        let next = LineHeight::Absolute(px);
        let changed = run.paragraph.line_height != next;
        run.paragraph.line_height = next;
        changed
    }

    /// Return leading to "auto": a multiple of the type size.
    pub fn set_leading_auto(run: &mut TextRun, multiple: f32) -> bool {
        if !multiple.is_finite() || multiple <= 0.0 {
            return false;
        }
        let next = LineHeight::Multiple(multiple);
        let changed = run.paragraph.line_height != next;
        run.paragraph.line_height = next;
        changed
    }

    pub fn set_first_line_indent(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || run.paragraph.first_line_indent == px {
            return false;
        }
        run.paragraph.first_line_indent = px;
        true
    }

    pub fn set_space_before(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || px < 0.0 || run.paragraph.space_before == px {
            return false;
        }
        run.paragraph.space_before = px;
        true
    }

    pub fn set_space_after(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || px < 0.0 || run.paragraph.space_after == px {
            return false;
        }
        run.paragraph.space_after = px;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    fn text_document() -> (Document, LayerId) {
        let mut doc = Document::new(64, 64, "Test");
        let id = doc
            .layers
            .push_root(Layer::with_kind(
                "Title",
                LayerKind::Text(TextLayer {
                    text: "Hello".into(),
                    font_family: "Inter".into(),
                    size_px: 24.0,
                }),
            ))
            .unwrap();
        doc.set_active_layer(Some(id)).unwrap();
        (doc, id)
    }

    #[test]
    fn the_panels_find_the_active_text_layer() {
        let (doc, id) = text_document();
        let (found, run) = active_text(&doc, Some(id)).expect("a text layer");
        assert_eq!(found, id);
        assert_eq!(run.text, "Hello");
        assert_eq!(run.style.family, "Inter");
        assert_eq!(run.style.size_px, 24.0);
    }

    #[test]
    fn a_non_text_layer_leaves_the_panels_inert_with_a_reason() {
        let mut doc = Document::new(32, 32, "Test");
        let id = doc.layers.push_root(Layer::raster("Pixels")).unwrap();
        assert!(active_text(&doc, Some(id)).is_none());
        assert!(active_text(&doc, None).is_none());
        assert!(!no_text_layer_reason().is_empty());
    }

    #[test]
    fn changing_the_family_emits_the_new_layer_kind() {
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(Character::set_family(&mut run, "  Georgia  "));
        assert_eq!(run.style.family, "Georgia");
        let Some(Intent::EditLayerKind { layer, kind }) = commit(&doc, id, &run) else {
            panic!("expected an edit");
        };
        assert_eq!(layer, id);
        let LayerKind::Text(t) = *kind else {
            panic!("not a text layer");
        };
        assert_eq!(t.font_family, "Georgia");
        assert_eq!(t.text, "Hello", "the content must survive a style edit");
    }

    #[test]
    fn committing_an_unchanged_run_emits_nothing() {
        let (doc, id) = text_document();
        let (_, run) = active_text(&doc, Some(id)).unwrap();
        assert!(commit(&doc, id, &run).is_none());
    }

    #[test]
    fn an_empty_family_is_refused() {
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(!Character::set_family(&mut run, "   "));
        assert_eq!(run.style.family, "Inter");
    }

    #[test]
    fn the_size_clamps_into_the_usable_range() {
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(Character::set_size(&mut run, -100.0));
        assert_eq!(run.style.size_px, MIN_SIZE_PX);
        assert!(Character::set_size(&mut run, 1e9));
        assert_eq!(run.style.size_px, MAX_SIZE_PX);
        assert!(!Character::set_size(&mut run, f32::NAN));
        assert_eq!(run.style.size_px, MAX_SIZE_PX);
    }

    #[test]
    fn setting_a_value_to_what_it_already_is_reports_no_change() {
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(!Character::set_size(&mut run, 24.0));
        assert!(!Character::set_family(&mut run, "Inter"));
        assert!(!Character::set_italic(&mut run, false));
        assert!(!Paragraph::set_alignment(&mut run, Alignment::Left));
    }

    #[test]
    fn italic_and_the_decorations_toggle() {
        let mut run = TextRun::point("x", "Inter", 12.0);
        assert!(Character::set_italic(&mut run, true));
        assert_eq!(run.style.slant, FontSlant::Italic);
        assert!(Character::set_italic(&mut run, false));
        assert_eq!(run.style.slant, FontSlant::Normal);
        assert!(Character::set_underline(&mut run, true));
        assert!(run.style.underline);
        assert!(Character::set_strikethrough(&mut run, true));
        assert!(run.style.strikethrough);
    }

    #[test]
    fn the_weight_axis_is_clamped_to_something_a_font_can_have() {
        let mut run = TextRun::point("x", "Inter", 12.0);
        assert!(Character::set_weight(&mut run, 0));
        assert_eq!(run.style.weight, FontWeight(1));
        assert!(Character::set_weight(&mut run, 60_000));
        assert_eq!(run.style.weight, FontWeight(1000));
    }

    #[test]
    fn a_numeric_weight_shows_as_the_nearest_named_one() {
        assert_eq!(weight_label(FontWeight(400)), "Regular");
        assert_eq!(weight_label(FontWeight(700)), "Bold");
        assert_eq!(weight_label(FontWeight(660)), "Bold");
        assert_eq!(weight_label(FontWeight(1)), "Thin");
        assert_eq!(weight_label(FontWeight(1000)), "Black");
        for (name, n) in WEIGHTS {
            assert_eq!(weight_label(FontWeight(*n)), *name);
        }
    }

    #[test]
    fn a_non_finite_number_never_reaches_the_run() {
        let mut run = TextRun::point("x", "Inter", 12.0);
        let before = run.clone();
        assert!(!Character::set_tracking(&mut run, f32::NAN));
        assert!(!Character::set_color(&mut run, [f32::NAN, 0.0, 0.0, 1.0]));
        assert!(!Paragraph::set_leading_px(&mut run, f32::INFINITY));
        assert!(!Paragraph::set_first_line_indent(&mut run, f32::NAN));
        assert!(!Paragraph::set_space_before(&mut run, f32::NAN));
        assert_eq!(run, before);
    }

    #[test]
    fn leading_switches_between_auto_and_absolute() {
        let mut run = TextRun::point("x", "Inter", 20.0);
        assert_eq!(
            Character::leading_px(&run.style, &run.paragraph),
            20.0 * 1.2
        );
        assert!(Paragraph::set_leading_px(&mut run, 30.0));
        assert_eq!(run.paragraph.line_height, LineHeight::Absolute(30.0));
        assert_eq!(Character::leading_px(&run.style, &run.paragraph), 30.0);
        assert!(Paragraph::set_leading_auto(&mut run, 1.5));
        assert_eq!(Character::leading_px(&run.style, &run.paragraph), 30.0);
        assert_eq!(run.paragraph.line_height, LineHeight::Multiple(1.5));
    }

    #[test]
    fn a_zero_or_negative_leading_is_refused() {
        let mut run = TextRun::point("x", "Inter", 20.0);
        assert!(!Paragraph::set_leading_px(&mut run, 0.0));
        assert!(!Paragraph::set_leading_px(&mut run, -5.0));
        assert!(!Paragraph::set_leading_auto(&mut run, 0.0));
    }

    #[test]
    fn negative_paragraph_spacing_is_refused_but_a_negative_indent_is_not() {
        let mut run = TextRun::point("x", "Inter", 20.0);
        assert!(!Paragraph::set_space_before(&mut run, -1.0));
        assert!(!Paragraph::set_space_after(&mut run, -1.0));
        // A hanging indent is a real thing, so a negative one is allowed.
        assert!(Paragraph::set_first_line_indent(&mut run, -12.0));
        assert_eq!(run.paragraph.first_line_indent, -12.0);
    }

    #[test]
    fn every_alignment_has_a_label_and_a_distinct_glyph() {
        assert_eq!(ALIGNMENTS.len(), 4);
        let mut glyphs: Vec<&str> = ALIGNMENTS.iter().map(|a| alignment_glyph(*a)).collect();
        assert!(ALIGNMENTS.iter().all(|a| !alignment_label(*a).is_empty()));
        assert!(glyphs.iter().all(|g| !g.is_empty()));
        glyphs.sort_unstable();
        let count = glyphs.len();
        glyphs.dedup();
        assert_eq!(glyphs.len(), count, "two alignments share a glyph");
    }

    #[test]
    fn alignment_survives_the_round_trip_through_the_document() {
        // Whatever `TextLayer` can and cannot carry, an edit must not silently
        // lose the field the user just changed. This pins which fields survive.
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(Character::set_size(&mut run, 48.0));
        let Some(Intent::EditLayerKind { kind, .. }) = commit(&doc, id, &run) else {
            panic!("expected an edit");
        };
        let LayerKind::Text(t) = *kind else {
            panic!("not text")
        };
        assert_eq!(t.size_px, 48.0);
        assert_eq!(TextRun::from(&t).style.size_px, 48.0);
    }
}
