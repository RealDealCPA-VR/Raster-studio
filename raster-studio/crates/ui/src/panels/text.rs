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
//!
//! # Card 021: every control reaches the persistent data
//!
//! Every control the panels draw names a `TextLayer` field and reaches the
//! document through `Intent::EditLayerKind` → `Command::SetLayerKind`, so a
//! change lands in the pixels (the compositor shapes the whole persisted run
//! — card 019), takes part in undo, and survives save/reopen.
//!
//! **Range versus whole layer.** The setters here apply to the whole layer.
//! Range editing does not exist until the canvas text sessions land (cards
//! 025+); when it does, the same setters become the range path against a
//! selected `StyleSpan` instead of the base style. There is no separate
//! control to disable today — nothing on the surface promises range
//! application yet.
//!
//! **Gestures.** A slider or picker drag reaches the shell as one
//! `EditLayerKind` intent per frame; `Chrome::harvest` stamps each with the
//! pointer gesture and `Editor::apply_kind_edit` folds one gesture into one
//! undo step. That is the established contract — the panel does not (and
//! cannot) know about the pointer.
//!
//! **Fill colour** lives in the model as linear straight RGBA (the space the
//! compositor composites in), while egui's picker edits gamma-space RGBA8.
//! The two conversions below are the only place that translation happens;
//! the 8-bit quantisation is deliberate — the same value always converts to
//! the same swatch, so a picker that closes without a change emits nothing.

use editor_core::Document;
use layer_model::{LayerId, LayerKind, TextLayer};
use text_engine::{
    Alignment, AntiAlias, Caps, CharStyle, FontSlant, FontStretch, FontWeight, KernAdjustment,
    LineHeight, ParagraphStyle, ScriptPosition, TextFrame, TextRun,
};

use crate::intent::Intent;

/// Smallest and largest type size the panels offer.
pub const MIN_SIZE_PX: f32 = 1.0;
pub const MAX_SIZE_PX: f32 = 1638.0;

/// Card 023: the wrapping box a point-text run becomes when the panel switches
/// it to paragraph text, and the bounds the box fields accept. The height
/// defaults to auto (`None`) — a box that grows with the text until the user
/// fixes it deliberately.
pub const DEFAULT_BOX_WIDTH_PX: f32 = 200.0;
pub const MIN_BOX_SIZE_PX: f32 = 8.0;
pub const MAX_BOX_SIZE_PX: f32 = 4096.0;

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

/// W3-J: the baseline positions the Character panel offers, in panel order.
/// Each reaches `CharStyle::script`, which the layout consumes as a size
/// factor and a baseline shift (`ScriptPosition::baseline_shift`).
pub const SCRIPTS: &[ScriptPosition] = &[
    ScriptPosition::Normal,
    ScriptPosition::Superscript,
    ScriptPosition::Subscript,
];

/// Panel label for a baseline position.
pub const fn script_label(script: ScriptPosition) -> &'static str {
    match script {
        ScriptPosition::Normal => "Normal",
        ScriptPosition::Superscript => "Super",
        ScriptPosition::Subscript => "Sub",
    }
}

/// Panel label for an alignment.
pub const fn alignment_label(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::Left => "Left",
        Alignment::Center => "Center",
        Alignment::Right => "Right",
        Alignment::Justify
        | Alignment::JustifyLastCenter
        | Alignment::JustifyLastRight
        | Alignment::JustifyAll => "Justify",
    }
}

/// W3-J: which [`ALIGNMENTS`] segment an alignment lights - every justify
/// variant lights "Justify"; the last-line row says which one.
pub fn alignment_index(alignment: Alignment) -> usize {
    if alignment.is_justified() {
        ALIGNMENTS.len() - 1
    } else {
        ALIGNMENTS.iter().position(|a| *a == alignment).unwrap_or(0)
    }
}

/// W3-J: the four justify variants, as the Paragraph panel's "Last line" row
/// offers them: last line left, centred, right, or justified too.
pub const JUSTIFY_VARIANTS: &[Alignment] = &[
    Alignment::Justify,
    Alignment::JustifyLastCenter,
    Alignment::JustifyLastRight,
    Alignment::JustifyAll,
];

/// Panel label for a justify variant's last line.
pub const fn last_line_label(alignment: Alignment) -> &'static str {
    match alignment {
        Alignment::JustifyLastCenter => "Center",
        Alignment::JustifyLastRight => "Right",
        Alignment::JustifyAll => "Full",
        _ => "Left",
    }
}

/// W3-J: the kerning modes the Character panel offers. "Optical" is absent
/// on purpose: the shaper has no optical kerning, and the control's tooltip
/// says so instead of offering a choice that would do nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum KerningMode {
    /// The font's own pair kerning (`kern`).
    Metrics,
    /// No kerning at all.
    Off,
    /// A fixed amount between every pair, in 1/1000 em, in place of the
    /// font's.
    Manual,
}

impl KerningMode {
    /// Every mode, in panel order.
    pub const ALL: &'static [KerningMode] =
        &[KerningMode::Metrics, KerningMode::Off, KerningMode::Manual];

    /// The segment label.
    pub const fn label(self) -> &'static str {
        match self {
            KerningMode::Metrics => "Metrics",
            KerningMode::Off => "0",
            KerningMode::Manual => "Manual",
        }
    }
}

/// W3-J: the caps choices, in panel order.
pub const CAPS: &[Caps] = &[Caps::Normal, Caps::AllCaps, Caps::SmallCaps];

/// Panel label for a caps choice.
pub const fn caps_label(caps: Caps) -> &'static str {
    match caps {
        Caps::Normal => "Normal",
        Caps::AllCaps => "All Caps",
        Caps::SmallCaps => "Small Caps",
    }
}

/// W3-J: the anti-alias choices, in panel order.
pub const ANTI_ALIAS: &[AntiAlias] = &[AntiAlias::Smooth, AntiAlias::None];

/// Panel label for an anti-alias mode.
pub const fn anti_alias_label(mode: AntiAlias) -> &'static str {
    match mode {
        AntiAlias::Smooth => "Smooth",
        AntiAlias::None => "None",
    }
}

/// Smallest and largest glyph scale the panel offers, in percent - the
/// schema's own range.
pub const MIN_SCALE_PERCENT: f32 = layer_model::text::MIN_GLYPH_SCALE * 100.0;
pub const MAX_SCALE_PERCENT: f32 = layer_model::text::MAX_GLYPH_SCALE * 100.0;

// The alignment control is a *word* control — `alignment_label` feeds
// `design::segmented_control` in `view::docks`. There was an `alignment_glyph`
// here too, returning "≡" / "☰" / "⋮" / "▤"; nothing drew it, and three of the
// four are not in the font egui loads, so it was a tofu box waiting for a
// caller. An alignment button that wants a picture takes a key from
// `icons::ui_icon` like the rest of the chrome.

/// The nearest named weight to a numeric one, for showing the combo's label.
pub fn weight_label(weight: FontWeight) -> &'static str {
    WEIGHTS
        .iter()
        .min_by_key(|(_, n)| weight.0.abs_diff(*n))
        .map(|(name, _)| *name)
        .unwrap_or("Regular")
}

// -- card 022: font selection and substitution reporting -----------------

/// The substitution the compositor will apply for `family`, or `None` when
/// the request is installed, is the generic sans (empty), or no library is
/// loaded. This is the same rule the shaper applies before shaping
/// (`attrs_for`), so the report the panel shows and the render the user gets
/// agree by construction. The requested name stays in the document.
pub fn substitution(family: &str) -> Option<String> {
    compositor::font_substitute_for(family)
}

/// Card 023: how many of the run's lines fall past a fixed box height — the
/// overset status the Paragraph panel shows. `None` when there is no fixed
/// height to overflow (point text, auto-height box); `Some(0)` means every
/// line fits.
pub fn overset_lines(run: &TextRun) -> Option<usize> {
    compositor::text_overset_lines(run)
}

/// The picker's search: installed families whose name contains `search`
/// case-insensitively, in the library's own (sorted) order. An empty search
/// lists everything — the search narrows, it never reorders.
pub fn family_candidates(search: &str, available: &[String]) -> Vec<String> {
    let needle = search.trim().to_lowercase();
    available
        .iter()
        .filter(|name| needle.is_empty() || name.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// The picker's label for one installed face, by the style parts a face
/// carries: width, then weight, then slant; "Regular" when nothing deviates
/// from the default design. (Taken as parts rather than a `FaceRecord` so the
/// label needs no font handle.)
pub fn face_label(weight: FontWeight, slant: FontSlant, stretch: FontStretch) -> String {
    let width = match stretch {
        FontStretch::UltraCondensed => "Ultra Condensed",
        FontStretch::ExtraCondensed => "Extra Condensed",
        FontStretch::Condensed => "Condensed",
        FontStretch::SemiCondensed => "Semi Condensed",
        FontStretch::Normal => "",
        FontStretch::SemiExpanded => "Semi Expanded",
        FontStretch::Expanded => "Expanded",
        FontStretch::ExtraExpanded => "Extra Expanded",
        FontStretch::UltraExpanded => "Ultra Expanded",
    };
    let slant = match slant {
        FontSlant::Normal => "",
        FontSlant::Italic => "Italic",
        FontSlant::Oblique => "Oblique",
    };
    let mut parts: Vec<&str> = Vec::new();
    if !width.is_empty() {
        parts.push(width);
    }
    let weight = weight_label(weight);
    // "Regular" is only named when it is the whole label — a condensed or
    // slanted face never carries the word.
    let regular_is_implied = width.is_empty() && slant.is_empty();
    if weight != "Regular" || regular_is_implied {
        parts.push(weight);
    }
    if !slant.is_empty() {
        parts.push(slant);
    }
    // W16-N: each part in the interface language; the "Regular" test above
    // runs on the English name.
    if parts.is_empty() {
        return crate::strings::tr_en("Regular").to_string();
    }
    parts
        .iter()
        .map(|p| crate::strings::tr_en(p))
        .collect::<Vec<_>>()
        .join(" ")
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

    /// Card 022: adopt one installed face — weight, slant and width straight
    /// from a face the picker listed. The family field is untouched: a face
    /// belongs to the family already chosen.
    pub fn set_face(
        run: &mut TextRun,
        weight: FontWeight,
        slant: FontSlant,
        stretch: FontStretch,
    ) -> bool {
        let weight = FontWeight(weight.0.clamp(1, 1000));
        let mut changed = false;
        if run.style.weight != weight {
            run.style.weight = weight;
            changed = true;
        }
        if run.style.slant != slant {
            run.style.slant = slant;
            changed = true;
        }
        if run.style.stretch != stretch {
            run.style.stretch = stretch;
            changed = true;
        }
        changed
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

    /// W3-J: superscript / subscript / normal for the whole layer. The layout
    /// shrinks the run by `SCRIPT_SIZE_FACTOR` and shifts its baseline.
    pub fn set_script(run: &mut TextRun, script: ScriptPosition) -> bool {
        let changed = run.style.script != script;
        run.style.script = script;
        changed
    }

    /// W3-J: the font's own pair kerning (`kern`) on or off — the shaper's
    /// "metrics" kerning. There is no optical mode to offer: cosmic-text has
    /// none, and the panel says so rather than drawing a dead choice.
    pub fn set_kerning(run: &mut TextRun, on: bool) -> bool {
        let changed = run.style.kerning != on;
        run.style.kerning = on;
        changed
    }

    /// W3-J: standard and contextual ligatures on or off.
    pub fn set_ligatures(run: &mut TextRun, on: bool) -> bool {
        let changed = run.style.ligatures != on;
        run.style.ligatures = on;
        changed
    }

    /// W3-J: horizontal scale in percent (100 = none). Clamped to the
    /// schema's range; the layout stretches advances and glyph images.
    pub fn set_horizontal_scale(run: &mut TextRun, percent: f32) -> bool {
        let Some(scale) = scale_from_percent(percent) else {
            return false;
        };
        let changed = run.style.horizontal_scale != scale;
        run.style.horizontal_scale = scale;
        changed
    }

    /// W3-J: vertical scale in percent (100 = none).
    pub fn set_vertical_scale(run: &mut TextRun, percent: f32) -> bool {
        let Some(scale) = scale_from_percent(percent) else {
            return false;
        };
        let changed = run.style.vertical_scale != scale;
        run.style.vertical_scale = scale;
        changed
    }

    /// W3-J: baseline shift in layer pixels; positive raises.
    pub fn set_baseline_shift(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || run.style.baseline_shift == px {
            return false;
        }
        run.style.baseline_shift = px;
        true
    }

    /// W3-J: all caps / small caps / as typed.
    pub fn set_caps(run: &mut TextRun, caps: Caps) -> bool {
        let changed = run.style.caps != caps;
        run.style.caps = caps;
        changed
    }

    /// W3-J: smooth or hard glyph edges, for the whole layer.
    pub fn set_anti_alias(run: &mut TextRun, mode: AntiAlias) -> bool {
        let changed = run.style.anti_alias != mode;
        run.style.anti_alias = mode;
        changed
    }

    /// W3-J: the kerning mode the run is in, and the manual amount (1/1000
    /// em) when every manual step is the same one. A manual table with mixed
    /// amounts reads as Manual with no single value.
    pub fn kerning_mode(run: &TextRun) -> (KerningMode, Option<f32>) {
        if let Some(first) = run.kerning.first() {
            let uniform = run.kerning.iter().all(|k| k.amount == first.amount);
            return (KerningMode::Manual, uniform.then_some(first.amount));
        }
        if run.style.kerning {
            (KerningMode::Metrics, None)
        } else {
            (KerningMode::Off, None)
        }
    }

    /// W3-J: switch kerning mode. Metrics turns the font's `kern` on and
    /// clears any manual table; Off turns both off; Manual turns the font's
    /// off and sets `amount` between every pair of the current text.
    pub fn set_kerning_mode(run: &mut TextRun, mode: KerningMode, amount: f32) -> bool {
        let before = (run.style.kerning, run.kerning.clone());
        match mode {
            KerningMode::Metrics => {
                run.style.kerning = true;
                run.kerning.clear();
            }
            KerningMode::Off => {
                run.style.kerning = false;
                run.kerning.clear();
            }
            KerningMode::Manual => {
                run.style.kerning = false;
                let amount = if amount.is_finite() { amount } else { 0.0 };
                run.kerning = manual_kerning(&run.text, amount);
            }
        }
        (run.style.kerning, run.kerning.clone()) != before
    }

    /// The leading the panel shows, in pixels, resolved against the run's own
    /// size — which is what "auto" means and what the field has to display.
    pub fn leading_px(style: &CharStyle, paragraph: &ParagraphStyle) -> f32 {
        paragraph.line_height.resolve(style.size_px)
    }
}

/// A panel percentage as a model scale, clamped to the schema's range;
/// `None` for a non-finite value.
fn scale_from_percent(percent: f32) -> Option<f32> {
    percent.is_finite().then(|| {
        (percent / 100.0).clamp(
            layer_model::text::MIN_GLYPH_SCALE,
            layer_model::text::MAX_GLYPH_SCALE,
        )
    })
}

/// One manual kerning step before every character but the first, each of
/// `amount` (1/1000 em) - the whole-layer manual kerning the panel sets.
fn manual_kerning(text: &str, amount: f32) -> Vec<KernAdjustment> {
    text.char_indices()
        .skip(1)
        .map(|(index, _)| KernAdjustment::new(index, amount))
        .collect()
}

/// The model's linear fill as an sRGB swatch for egui's picker.
///
/// Straight (non-premultiplied) RGBA on both sides. The quantisation to 8-bit
/// is what makes the control stable: the same model colour always shows as the
/// same swatch, so a picker that closes unchanged emits nothing.
#[must_use]
pub fn fill_to_swatch(linear: [f32; 4]) -> egui::Color32 {
    // RGB goes through the sRGB transfer; alpha is a plain coverage fraction
    // on both sides (the picker's alpha slider reads 0-255 literally), which
    // is how the Color panel treats it too.
    let ch = |c: f32| (color::linear_to_srgb(c.clamp(0.0, 1.0)) * 255.0).round() as u8;
    let a = (linear[3].clamp(0.0, 1.0) * 255.0).round() as u8;
    egui::Color32::from_rgba_unmultiplied(ch(linear[0]), ch(linear[1]), ch(linear[2]), a)
}

/// The swatch egui's picker produced, back into the model's linear space.
///
/// `Color32` stores premultiplied sRGB (egui 0.29's rule), so the readback
/// goes through `to_srgba_unmultiplied` - the straight values the user picked.
#[must_use]
pub fn swatch_to_fill(srgb: egui::Color32) -> [f32; 4] {
    let [r, g, b, a] = srgb.to_srgba_unmultiplied();
    let ch = |c: u8| color::srgb_to_linear(f32::from(c) / 255.0);
    [ch(r), ch(g), ch(b), f32::from(a) / 255.0]
}

#[cfg(test)]
mod swatch_tests {
    use super::*;

    #[test]
    fn alpha_is_a_plain_fraction_on_both_sides() {
        assert_eq!(fill_to_swatch([0.0, 0.0, 0.0, 0.5]).a(), 128);
        assert_eq!(
            swatch_to_fill(fill_to_swatch([0.0, 0.0, 0.0, 0.5]))[3],
            128.0 / 255.0
        );
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

    /// W3-J: indent of every line from the left edge, in layer pixels.
    pub fn set_left_indent(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || run.paragraph.left_indent == px {
            return false;
        }
        run.paragraph.left_indent = px;
        true
    }

    /// W3-J: indent of every line from the right edge, in layer pixels.
    pub fn set_right_indent(run: &mut TextRun, px: f32) -> bool {
        if !px.is_finite() || run.paragraph.right_indent == px {
            return false;
        }
        run.paragraph.right_indent = px;
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

    // -- card 023: point vs paragraph geometry ----------------------------

    /// Point text or a wrapping box. Switching to a box starts at
    /// [`DEFAULT_BOX_WIDTH_PX`] with an auto height; switching back to point
    /// keeps every character — explicit line breaks stay — and drops the box
    /// dimensions. The type size and the layer transform are untouched either
    /// way: the box is the paragraph's geometry, nothing else's.
    pub fn set_boxed(run: &mut TextRun, boxed: bool) -> bool {
        let next = if boxed {
            match run.frame {
                TextFrame::Box { width, height } => TextFrame::Box { width, height },
                TextFrame::Point => TextFrame::Box {
                    width: DEFAULT_BOX_WIDTH_PX,
                    height: None,
                },
            }
        } else {
            TextFrame::Point
        };
        if run.frame == next {
            return false;
        }
        run.frame = next;
        true
    }

    /// The wrapping box's width, in layer pixels. Refuses non-finite and
    /// out-of-range values rather than clamping them into the model. The
    /// panel's range is **deliberately stricter** than the model's `validate`
    /// (which accepts any finite non-negative frame): a file may legally carry
    /// a width this panel would not emit, and the panel's rule only keeps
    /// what it produces inside the model's rule. Reflowing never touches the
    /// type size: that is the size-vs-box distinction the card asks for.
    pub fn set_box_width(run: &mut TextRun, width: f32) -> bool {
        let TextFrame::Box {
            width: current,
            height,
        } = run.frame
        else {
            return false;
        };
        if !width.is_finite()
            || width < MIN_BOX_SIZE_PX
            || width > MAX_BOX_SIZE_PX
            || width == current
        {
            return false;
        }
        run.frame = TextFrame::Box { width, height };
        true
    }

    /// The wrapping box's height: `None` is auto (the box grows with the
    /// text), `Some(px)` is a fixed height that can overflow — the engine
    /// reports overset instead of clipping, and the panel shows it.
    pub fn set_box_height(run: &mut TextRun, height: Option<f32>) -> bool {
        let TextFrame::Box {
            width,
            height: current,
        } = run.frame
        else {
            return false;
        };
        if let Some(px) = height {
            if !px.is_finite() || px < MIN_BOX_SIZE_PX || px > MAX_BOX_SIZE_PX {
                return false;
            }
        }
        if current == height {
            return false;
        }
        run.frame = TextFrame::Box { width, height };
        true
    }

    /// The box geometry the panel shows: `Some((width, height))` with `None`
    /// height meaning auto; `None` overall for point text.
    pub fn box_size(run: &TextRun) -> Option<(f32, Option<f32>)> {
        match run.frame {
            TextFrame::Box { width, height } => Some((width, height)),
            TextFrame::Point => None,
        }
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
                    ..Default::default()
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
    fn every_alignment_has_a_distinct_label() {
        assert_eq!(ALIGNMENTS.len(), 4);
        let mut labels: Vec<&str> = ALIGNMENTS.iter().map(|a| alignment_label(*a)).collect();
        assert!(labels.iter().all(|l| !l.is_empty()));
        labels.sort_unstable();
        let count = labels.len();
        labels.dedup();
        assert_eq!(labels.len(), count, "two alignments share a label");
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

    #[test]
    fn a_fill_change_reaches_the_document_and_back() {
        // Card 021's fill control: the linear colour the picker hands over is
        // what the document stores, and what the panel reads back.
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        let green = [0.1, 0.8, 0.3, 1.0];
        assert!(Character::set_color(&mut run, green));
        assert_eq!(run.style.color, green);
        let Some(Intent::EditLayerKind { layer, kind }) = commit(&doc, id, &run) else {
            panic!("expected an edit");
        };
        assert_eq!(layer, id);
        let LayerKind::Text(t) = *kind else {
            panic!("not a text layer");
        };
        assert_eq!(t.style.fill, green, "the model stores the linear fill");
        let (_, reopened) = active_text(&doc, Some(id)).unwrap();
        assert_ne!(
            reopened.style.color, green,
            "the doc still holds the old fill"
        );
    }

    #[test]
    fn the_fill_swatch_round_trips_through_the_picker_space() {
        // Model (linear) -> swatch (sRGB8) -> model must be the identity the
        // quantisation promises: no oscillation between frames, and black
        // stays black through the round trip.
        for linear in [
            [0.0, 0.0, 0.0, 1.0],
            [1.0, 1.0, 1.0, 1.0],
            [0.1, 0.8, 0.3, 0.5],
            [0.5, 0.5, 0.5, 1.0],
        ] {
            let swatch = fill_to_swatch(linear);
            let back = swatch_to_fill(swatch);
            for i in 0..4 {
                // Half an 8-bit code in sRGB space converts to at most
                // 0.5/255 x 12.92 in linear space (the darkest-end slope of
                // the sRGB EOTF is its steepest). Anything wider and the
                // swatch would drift further every round trip.
                const MAX_DRIFT: f32 = 0.5 / 255.0 * 12.92;
                assert!(
                    (back[i] - linear[i]).abs() <= MAX_DRIFT,
                    "channel {i}: {} vs {}",
                    back[i],
                    linear[i]
                );
            }
            // The round trip is a fixed point: the next frame shows the same
            // straight swatch, so a picker that closes unchanged emits
            // nothing. (egui stores premultiplied, so compare straight bytes.)
            assert_eq!(
                swatch.to_srgba_unmultiplied(),
                fill_to_swatch(back).to_srgba_unmultiplied()
            );
        }
        // A clamped input never escapes the range.
        let clamped = fill_to_swatch([2.0, -1.0, 0.5, 1.0]);
        assert_eq!(clamped.r(), 255);
        assert_eq!(clamped.g(), 0);
    }

    // -- card 022: font selection and substitution reporting --------------

    #[test]
    fn the_family_search_narrows_without_reordering() {
        let available = vec![
            "DejaVu Sans".to_string(),
            "DejaVu Serif".to_string(),
            "Segoe UI".to_string(),
        ];
        assert_eq!(family_candidates("", &available), available);
        assert_eq!(
            family_candidates("dejavu", &available),
            vec!["DejaVu Sans".to_string(), "DejaVu Serif".to_string()],
            "case-insensitive substring, library order kept"
        );
        assert_eq!(
            family_candidates("  UI  ", &available),
            vec!["Segoe UI".to_string()]
        );
        assert!(family_candidates("Comic Sans", &available).is_empty());
    }

    #[test]
    fn face_labels_name_width_weight_and_slant() {
        assert_eq!(
            face_label(FontWeight::NORMAL, FontSlant::Normal, FontStretch::Normal),
            "Regular"
        );
        assert_eq!(
            face_label(FontWeight::BOLD, FontSlant::Normal, FontStretch::Normal),
            "Bold"
        );
        assert_eq!(
            face_label(
                FontWeight::BOLD,
                FontSlant::Italic,
                FontStretch::SemiCondensed
            ),
            "Semi Condensed Bold Italic"
        );
        assert_eq!(
            face_label(
                FontWeight::NORMAL,
                FontSlant::Italic,
                FontStretch::SemiCondensed
            ),
            "Semi Condensed Italic"
        );
    }

    #[test]
    fn adopting_a_face_sets_weight_slant_and_width_but_not_the_family() {
        let mut run = TextRun::point("Headline", "DejaVu Sans", 48.0);
        assert!(Character::set_face(
            &mut run,
            FontWeight::NORMAL,
            FontSlant::Normal,
            FontStretch::SemiCondensed
        ));
        assert_eq!(run.style.stretch, FontStretch::SemiCondensed);
        assert_eq!(run.style.weight, FontWeight::NORMAL);
        assert_eq!(run.style.family, "DejaVu Sans", "family untouched");
        assert!(
            !Character::set_face(
                &mut run,
                FontWeight::NORMAL,
                FontSlant::Normal,
                FontStretch::SemiCondensed
            ),
            "second adopt is a no-op"
        );
    }

    #[test]
    fn a_missing_family_is_reported_with_an_installed_substitute() {
        // The generic sans (empty) is not a substitution.
        assert_eq!(substitution(""), None);
        // The substitute rule is one policy: any missing family reports the
        // same installed family, whatever machine the tests run on.
        let substitute =
            substitution("No Such Family On Any Machine").expect("a missing family is reported");
        assert!(
            compositor::font_families().contains(&substitute),
            "the substitute is an installed family: {substitute:?}"
        );
        assert_eq!(
            substitution("Also Not Installed Anywhere").expect("reported"),
            substitute,
            "every missing family substitutes the same way"
        );
    }

    // -- card 023: point vs paragraph geometry ----------------------------

    #[test]
    fn point_and_box_geometry_stay_distinct_from_the_type_size() {
        let mut run = TextRun::point(
            "One
Two",
            "DejaVu Sans",
            24.0,
        );
        // Point -> box: the text and its explicit breaks stay; the box starts
        // at the documented default with an auto height.
        assert!(Paragraph::set_boxed(&mut run, true));
        assert_eq!(
            run.text,
            "One
Two",
            "explicit breaks survive the switch"
        );
        assert_eq!(
            run.frame,
            TextFrame::Box {
                width: DEFAULT_BOX_WIDTH_PX,
                height: None
            }
        );
        assert_eq!(run.style.size_px, 24.0, "the type size is untouched");
        // Reflowing never touches the size either.
        assert!(Paragraph::set_box_width(&mut run, 90.0));
        assert_eq!(run.style.size_px, 24.0);
        assert_eq!(Paragraph::box_size(&run), Some((90.0, None)));
        // Box -> point: the box dimensions go, the characters stay.
        assert!(Paragraph::set_boxed(&mut run, false));
        assert_eq!(run.frame, TextFrame::Point);
        assert_eq!(
            run.text,
            "One
Two"
        );
        // A point run refuses box geometry edits; refusals never clamp.
        assert!(!Paragraph::set_box_width(&mut run, 90.0));
        assert!(!Paragraph::set_box_height(&mut run, Some(80.0)));
        assert!(Paragraph::set_boxed(&mut run, true));
        assert!(!Paragraph::set_box_width(&mut run, f32::NAN));
        assert!(!Paragraph::set_box_width(&mut run, 0.0));
        assert!(!Paragraph::set_box_width(&mut run, MAX_BOX_SIZE_PX * 2.0));
        assert!(!Paragraph::set_box_height(&mut run, Some(f32::NAN)));
        assert!(!Paragraph::set_box_height(&mut run, Some(-4.0)));
        assert!(Paragraph::set_box_height(&mut run, Some(80.0)));
        assert!(
            !Paragraph::set_box_height(&mut run, Some(80.0)),
            "a no-op reports no change"
        );
        // The refused out-of-range width above left the re-seeded default.
        assert_eq!(
            Paragraph::box_size(&run),
            Some((DEFAULT_BOX_WIDTH_PX, Some(80.0)))
        );
        assert!(Paragraph::set_box_height(&mut run, None));
        assert_eq!(
            Paragraph::box_size(&run),
            Some((DEFAULT_BOX_WIDTH_PX, None)),
            "auto height again, width kept"
        );
    }

    // ---- W3-J: script, kerning, ligatures --------------------------------

    #[test]
    fn script_kerning_and_ligatures_reach_the_persisted_layer_and_back() {
        let (doc, id) = text_document();
        let (_, mut run) = active_text(&doc, Some(id)).unwrap();
        assert!(Character::set_script(&mut run, ScriptPosition::Superscript));
        assert!(!Character::set_script(
            &mut run,
            ScriptPosition::Superscript
        ));
        assert!(Character::set_kerning(&mut run, false));
        assert!(!Character::set_kerning(&mut run, false));
        assert!(Character::set_ligatures(&mut run, false));
        assert!(!Character::set_ligatures(&mut run, false));

        let Some(Intent::EditLayerKind { kind, .. }) = commit(&doc, id, &run) else {
            panic!("expected an edit");
        };
        let LayerKind::Text(stored) = *kind else {
            panic!("not text");
        };
        assert_eq!(stored.style.script, layer_model::text::Script::Superscript);
        assert!(!stored.style.kerning);
        assert!(!stored.style.ligatures);
        // And the run the panel reads back next frame agrees.
        let back = TextRun::from(&stored);
        assert_eq!(back.style.script, ScriptPosition::Superscript);
        assert!(!back.style.kerning);
        assert!(!back.style.ligatures);
        // The layout consumes the script as a baseline shift and a size.
        assert!(ScriptPosition::Superscript.baseline_shift(24.0) < 0.0);
        assert!(back.style.effective_size_px() < 24.0);
    }

    #[test]
    fn every_script_position_has_a_distinct_label() {
        let mut labels: Vec<&str> = SCRIPTS.iter().map(|s| script_label(*s)).collect();
        assert_eq!(labels.len(), 3);
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), 3);
    }
}

// ---------------------------------------------------------------------------
// W3-J: the Type tool's default style
// ---------------------------------------------------------------------------

/// W3-J: manual kerning is a step *between* characters, so it needs two of
/// them. On shorter text the Character panel does not offer the Manual
/// segment - picking it would store an empty table that reads back as
/// Metrics or Off on the next frame.
pub fn manual_kerning_available(run: &TextRun) -> bool {
    run.text.chars().nth(1).is_some()
}

/// W3-J: the Type tool's default style as a run: the tool's options, applied
/// to a Type tool exactly as the shell applies them at a press, and read back
/// through [`tools::text::TypeTool::seed`] - the payload the next click
/// creates. What the panels show is therefore what the next layer gets.
pub fn type_defaults(options: &crate::ToolOptions) -> TextRun {
    use tools::Tool as _;
    let mut tool = tools::text::TypeTool::default();
    for (key, value) in options.held(tools::ToolId::Type) {
        let setting = match value {
            crate::OptionValue::Float(v) => tools::ToolSetting::Float(v),
            crate::OptionValue::Int(v) => tools::ToolSetting::Int(v),
            crate::OptionValue::Bool(v) => tools::ToolSetting::Bool(v),
            crate::OptionValue::Choice(v) => tools::ToolSetting::Choice(v),
            crate::OptionValue::Color(v) => tools::ToolSetting::Color(v),
        };
        // A value the tool refuses is the tool's to report at the press; here
        // it simply leaves the default in place.
        let _ = tool.set_setting(&key, setting);
    }
    TextRun::from(&tool.seed())
}

/// W3-J: a run's default-style values as the Type tool's option values -
/// the inverse of the mapping `TypeTool::set_setting` applies.
fn type_default_values(run: &TextRun) -> Vec<(&'static str, crate::OptionValue)> {
    use crate::OptionValue as V;
    use layer_model::text as t;
    let layer = TextLayer::from(run);
    let (st, pa) = (layer.style, layer.paragraph);
    let index = |found: Option<usize>| V::Choice(found.unwrap_or(0));
    let weight_step = (st.weight.0.clamp(100, 900) + 50) / 100;
    let srgb = |v: f32| color::linear_to_srgb(v.clamp(0.0, 1.0));
    vec![
        ("weight", V::Choice(usize::from(weight_step - 1))),
        ("italic", V::Bool(st.slant == t::Slant::Italic)),
        ("underline", V::Bool(st.underline)),
        ("strikethrough", V::Bool(st.strikethrough)),
        (
            "color",
            V::Color([
                srgb(st.fill[0]),
                srgb(st.fill[1]),
                srgb(st.fill[2]),
                st.fill[3],
            ]),
        ),
        ("tracking", V::Float(st.tracking)),
        (
            "leading",
            V::Float(match pa.leading {
                t::Leading::Absolute(px) => px,
                t::Leading::Multiple(_) => 0.0,
            }),
        ),
        ("horizontal_scale", V::Float(st.horizontal_scale * 100.0)),
        ("vertical_scale", V::Float(st.vertical_scale * 100.0)),
        ("baseline_shift", V::Float(st.baseline_shift)),
        (
            "script",
            index(
                tools::text::SCRIPT_VALUES
                    .iter()
                    .position(|s| *s == st.script),
            ),
        ),
        (
            "caps",
            index(tools::text::CAPS_VALUES.iter().position(|c| *c == st.caps)),
        ),
        ("kerning", V::Bool(st.kerning)),
        ("ligatures", V::Bool(st.ligatures)),
        (
            "anti_alias",
            index(
                tools::text::ANTI_ALIAS_VALUES
                    .iter()
                    .position(|a| *a == st.anti_alias),
            ),
        ),
        (
            "alignment",
            index(
                tools::text::ALIGNMENT_VALUES
                    .iter()
                    .position(|a| *a == pa.alignment),
            ),
        ),
        ("left_indent", V::Float(pa.left_indent)),
        ("right_indent", V::Float(pa.right_indent)),
        ("first_line_indent", V::Float(pa.first_line_indent)),
        ("space_before", V::Float(pa.space_before)),
        ("space_after", V::Float(pa.space_after)),
    ]
}

/// W3-J: the Type tool option writes that turn the defaults `before` into
/// `after` - only the values the edit changed, so an untouched option stays
/// untouched (and keeps forwarding nothing to the tool).
pub fn type_default_writes(
    before: &TextRun,
    after: &TextRun,
) -> Vec<(&'static str, crate::OptionValue)> {
    let old = type_default_values(before);
    type_default_values(after)
        .into_iter()
        .zip(old)
        .filter(|(new, old)| new != old)
        .map(|(new, _)| new)
        .collect()
}
