//! The persisted text schema (plan card 015).
//!
//! One data representation for everything a styled text layer keeps: the
//! string, the base character style, sparse style spans, paragraph settings,
//! the point/box frame, and manual kerning. **text-engine depends on this
//! crate**, so the vocabulary lives here and text-engine consumes it — one
//! text model, no second representation, no dependency cycle.
//!
//! # One authority for family and size
//!
//! `TextLayer::font_family` and `TextLayer::size_px` are the *only* family and
//! size fields at the layer level. The base style carries everything except
//! them, so a legacy alias cannot disagree with a rich one; a span may still
//! override either for its own bytes, exactly like every other override.
//!
//! # Legacy compatibility
//!
//! The three legacy fields (`text`, `font_family`, `size_px`) keep their names
//! and meanings, and every rich field is `#[serde(default)]`: a document that
//! stored `{"text", "font_family", "size_px"}` deserializes unchanged, with
//! defaults that preserve what the old renderer produced — **black, regular,
//! auto-leading, point text, no tracking** — so existing layers render
//! identical pixels (card 018 pins the bytes).
//!
//! Colours are **linear, straight (non-premultiplied) RGBA**, matching
//! `text_engine::CharStyle`, so conversion is a field copy, not a conversion.

use serde::{Deserialize, Serialize};

/// A requested font weight. 400 is regular, 700 bold — the CSS/OpenType
/// scale, so a family's named weights map onto it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Weight(pub u16);

impl Weight {
    /// Regular.
    pub const NORMAL: Weight = Weight(400);
    /// Bold.
    pub const BOLD: Weight = Weight(700);
}

impl Default for Weight {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Slant request. Synthetic slant is allowed unless the features say no.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Slant {
    #[default]
    Normal,
    Italic,
}

/// Horizontal face width — the OpenType `usWidthClass` scale. Mirrors
/// `text_engine::FontStretch` so conversion is a variant-for-variant copy;
/// card 022 uses it to reach a family's condensed faces when installed.
/// Variant order is width order, narrowest first.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
pub enum Stretch {
    UltraCondensed,
    ExtraCondensed,
    Condensed,
    SemiCondensed,
    #[default]
    Normal,
    SemiExpanded,
    Expanded,
    ExtraExpanded,
    UltraExpanded,
}

/// Sub/superscript placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Script {
    #[default]
    Normal,
    Superscript,
    Subscript,
}

/// Capitalisation applied at layout time (W3-J). The stored text keeps the
/// case the user typed; only the shaped glyphs change, so turning the option
/// off gives the original text back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Caps {
    /// The text as typed.
    #[default]
    Normal,
    /// Every lowercase letter shapes as its capital.
    AllCaps,
    /// Lowercase letters shape as capitals at the small-cap size.
    SmallCaps,
}

/// How glyph edges are rasterised (W3-J). The scaler has one smooth mode, so
/// that is the only smooth choice offered; `None` thresholds the coverage to
/// hard, aliased edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AntiAlias {
    /// Hard edges: every pixel is either ink or not.
    None,
    /// Smooth, grey-scale coverage.
    #[default]
    Smooth,
}

/// Horizontal alignment of a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Alignment {
    /// Flush against the start edge.
    #[default]
    Left,
    /// Centred.
    Center,
    /// Flush against the end edge.
    Right,
    /// Both edges flush; word spaces absorb the slack (last line stays flush).
    Justified,
    /// Justified, last line centred (W3-J).
    JustifyLastCenter,
    /// Justified, last line flush right (W3-J).
    JustifyLastRight,
    /// Justified, last line stretched to both edges too (W3-J).
    JustifyAll,
}

/// Leading: a multiple of the size (auto) or an absolute distance in layer
/// pixels.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum Leading {
    Multiple(f32),
    Absolute(f32),
}

impl Default for Leading {
    fn default() -> Self {
        // The shaper's own auto leading.
        Leading::Multiple(1.0)
    }
}

/// The base character style: what every byte inherits before spans apply.
///
/// Deliberately **no family and no size** — those are
/// [`TextLayer::font_family`] and [`TextLayer::size_px`], the single
/// authority. Everything else mirrors `text_engine::CharStyle` so conversion
/// is a field copy.
///
/// `#[serde(default)]` at the struct level: a payload written before a field
/// existed (the document version's serde defaults ARE the migration) loads
/// with that field's default instead of failing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BaseStyle {
    pub weight: Weight,
    pub slant: Slant,
    /// Requested face width (`usWidthClass`), so condensed faces persist.
    pub stretch: Stretch,
    /// Fill colour, linear straight RGBA. Black preserves legacy text.
    pub fill: [f32; 4],
    pub underline: bool,
    pub strikethrough: bool,
    pub script: Script,
    /// Tracking in 1/1000 em.
    pub tracking: f32,
    pub ligatures: bool,
    /// Apply the font's own `kern` feature.
    pub kerning: bool,
    /// Allow synthesising bold when the family has no bold face.
    pub synthetic_bold: bool,
    /// Allow synthesising italic when the family has no italic face.
    pub synthetic_italic: bool,
    /// W3-J: horizontal glyph scale, 1.0 = 100 %.
    pub horizontal_scale: f32,
    /// W3-J: vertical glyph scale, 1.0 = 100 %.
    pub vertical_scale: f32,
    /// W3-J: baseline shift in layer pixels; positive raises the text.
    pub baseline_shift: f32,
    /// W3-J: all caps / small caps.
    pub caps: Caps,
    /// W3-J: edge rasterisation, for the whole layer.
    pub anti_alias: AntiAlias,
}

impl Default for BaseStyle {
    fn default() -> Self {
        Self {
            weight: Weight::NORMAL,
            slant: Slant::Normal,
            stretch: Stretch::Normal,
            fill: [0.0, 0.0, 0.0, 1.0],
            underline: false,
            strikethrough: false,
            script: Script::Normal,
            tracking: 0.0,
            ligatures: true,
            kerning: true,
            synthetic_bold: true,
            synthetic_italic: true,
            horizontal_scale: 1.0,
            vertical_scale: 1.0,
            baseline_shift: 0.0,
            caps: Caps::Normal,
            anti_alias: AntiAlias::Smooth,
        }
    }
}

/// A sparse per-range patch: only the fields present change the base style.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleOverride {
    pub family: Option<String>,
    pub size_px: Option<f32>,
    pub weight: Option<Weight>,
    pub slant: Option<Slant>,
    pub stretch: Option<Stretch>,
    pub fill: Option<[f32; 4]>,
    pub underline: Option<bool>,
    pub strikethrough: Option<bool>,
    pub script: Option<Script>,
    pub tracking: Option<f32>,
}

/// One styled range: `[start, end)` in **bytes** of [`TextLayer::text`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleSpan {
    pub start: usize,
    pub end: usize,
    pub style: StyleOverride,
}

/// Paragraph settings, applied per paragraph (lines split on newlines).
///
/// `#[serde(default)]`: the W3-J indents load as zero from older payloads.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Paragraph {
    pub alignment: Alignment,
    pub leading: Leading,
    /// Indent of the first visual line of every paragraph, in layer pixels.
    pub first_line_indent: f32,
    pub space_before: f32,
    pub space_after: f32,
    /// W3-J: indent of every line from the start edge, in layer pixels.
    pub left_indent: f32,
    /// W3-J: indent of every line from the end edge, in layer pixels.
    pub right_indent: f32,
    /// W7-F: vertical type — the text runs top to bottom in columns that
    /// advance right to left (the Vertical Type tools set it). Appended and
    /// omitted while `false`, so documents that predate it load and save
    /// unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vertical: bool,
}

impl Default for Paragraph {
    fn default() -> Self {
        Self {
            alignment: Alignment::Left,
            leading: Leading::default(),
            first_line_indent: 0.0,
            space_before: 0.0,
            space_after: 0.0,
            left_indent: 0.0,
            right_indent: 0.0,
            vertical: false,
        }
    }
}

/// How the text is placed: a single anchor, or a box it wraps inside.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum Frame {
    /// Point text: no wrapping, the block is as wide as its widest line.
    #[default]
    Point,
    /// Text wraps at `width`; `height` is advisory — layout never clips,
    /// overset is reported instead.
    Box { width: f32, height: Option<f32> },
}

/// One manual kerning step: space inserted before the character at `index',
/// in 1/1000 em. Negative tightens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Kern {
    pub index: usize,
    pub amount: f32,
}

/// The persisted text layer: the legacy three fields plus the rich
/// vocabulary, every rich field defaulted so legacy documents load unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextLayer {
    /// The text. `\n`, `\r\n`, `\r` and `\n\r` all start a new paragraph.
    #[serde(default)]
    pub text: String,
    /// The single authority for the family (base level).
    #[serde(default)]
    pub font_family: String,
    /// The single authority for the size, in layer pixels (base level).
    #[serde(default = "default_size_px")]
    pub size_px: f32,
    /// Base character style for every byte that no span overrides.
    #[serde(default)]
    pub style: BaseStyle,
    /// Sparse styled ranges, applied in order over the base style.
    #[serde(default)]
    pub spans: Vec<StyleSpan>,
    #[serde(default)]
    pub paragraph: Paragraph,
    #[serde(default)]
    pub frame: Frame,
    /// Manual kerning adjustments, in byte order.
    #[serde(default)]
    pub kerning: Vec<Kern>,
}

fn default_size_px() -> f32 {
    16.0
}

impl Default for TextLayer {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_family: String::new(),
            size_px: default_size_px(),
            style: BaseStyle::default(),
            spans: Vec::new(),
            paragraph: Paragraph::default(),
            frame: Frame::default(),
            kerning: Vec::new(),
        }
    }
}

impl TextLayer {
    /// The legacy three-field layer, exactly as documents stored before rich
    /// text existed: black, regular, auto-leading point text.
    pub fn legacy(text: impl Into<String>, font_family: impl Into<String>, size_px: f32) -> Self {
        Self {
            text: text.into(),
            font_family: font_family.into(),
            size_px,
            ..Self::default()
        }
    }

    /// `true` when this layer carries nothing beyond the legacy three fields —
    /// the predicate "conversion cannot lose anything" relies on (card 016).
    pub fn is_legacy(&self) -> bool {
        *self == Self::legacy(self.text.clone(), self.font_family.clone(), self.size_px)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The acceptance scene's two headlines, as the schema must represent
    /// them: a large white bold condensed headline and a smaller green
    /// secondary line with one styled word.
    #[test]
    fn the_acceptance_scene_schema_round_trips_through_serde() {
        let headline = TextLayer {
            text: "THUMBS".to_string(),
            font_family: "DejaVu Sans Condensed".to_string(),
            size_px: 120.0,
            style: BaseStyle {
                weight: Weight::BOLD,
                fill: [1.0, 1.0, 1.0, 1.0],
                ..BaseStyle::default()
            },
            ..TextLayer::default()
        };
        let subhead = TextLayer {
            text: "Weekly digest".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 36.0,
            style: BaseStyle {
                fill: [0.1, 0.7, 0.3, 1.0],
                ..BaseStyle::default()
            },
            spans: vec![StyleSpan {
                start: 7,
                end: 13,
                style: StyleOverride {
                    weight: Some(Weight::BOLD),
                    fill: Some([1.0, 1.0, 1.0, 1.0]),
                    ..StyleOverride::default()
                },
            }],
            paragraph: Paragraph {
                alignment: Alignment::Center,
                ..Paragraph::default()
            },
            frame: Frame::Box {
                width: 800.0,
                height: Some(96.0),
            },
            kerning: vec![Kern {
                index: 7,
                amount: -20.0,
            }],
        };

        for layer in [headline, subhead] {
            let bytes = serde_json::to_vec(&layer).unwrap();
            let back: TextLayer = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(back, layer, "the schema round-trips without loss");
        }
    }

    /// A legacy three-field document loads unchanged: black, regular,
    /// auto-leading point text — the defaults preserve what the old renderer
    /// produced, so legacy layers keep their pixels.
    #[test]
    fn a_legacy_three_field_layer_loads_with_its_old_meaning() {
        let legacy = serde_json::json!({
            "text": "Headline",
            "font_family": "DejaVu Sans",
            "size_px": 48.0
        });
        let layer: TextLayer = serde_json::from_value(legacy).unwrap();
        assert_eq!(layer.text, "Headline");
        assert_eq!(layer.font_family, "DejaVu Sans");
        assert_eq!(layer.size_px, 48.0);
        assert_eq!(layer.style, BaseStyle::default(), "black, regular");
        assert_eq!(layer.style.weight, Weight::NORMAL);
        assert_eq!(layer.style.fill, [0.0, 0.0, 0.0, 1.0]);
        assert!(layer.spans.is_empty());
        assert_eq!(layer.frame, Frame::Point);
        assert!(layer.is_legacy(), "nothing beyond the three fields");
    }

    /// Card 022: the stretch field defaults, and a payload written before the
    /// field existed still loads (serde defaults are the migration).
    #[test]
    fn a_style_without_a_stretch_field_loads_with_the_default_width() {
        let payload = serde_json::json!({
            "text": "Headline",
            "font_family": "DejaVu Sans",
            "size_px": 48.0,
            "style": { "weight": 700, "fill": [1.0, 1.0, 1.0, 1.0] }
        });
        let layer: TextLayer = serde_json::from_value(payload).unwrap();
        assert_eq!(layer.style.stretch, Stretch::Normal, "default width");
        assert_eq!(layer.style.weight, Weight::BOLD);

        let payload = serde_json::json!({
            "text": "Headline",
            "font_family": "DejaVu Sans",
            "size_px": 48.0,
            "style": { "stretch": "SemiCondensed" }
        });
        let layer: TextLayer = serde_json::from_value(payload).unwrap();
        assert_eq!(layer.style.stretch, Stretch::SemiCondensed);
        let text = serde_json::to_string(&layer).unwrap();
        let back: TextLayer = serde_json::from_str(&text).unwrap();
        assert_eq!(back, layer, "stretch round-trips");
    }

    #[test]
    fn the_legacy_constructor_and_default_preserve_black_regular_text() {
        let built = TextLayer::legacy("Hi", "DejaVu Sans", 24.0);
        assert_eq!(built.style, BaseStyle::default());
        assert_eq!(built.paragraph, Paragraph::default());
        assert_eq!(built.frame, Frame::Point);
        assert!(built.is_legacy());
        // An empty layer is the same thing with nothing in it.
        assert_eq!(TextLayer::default(), TextLayer::legacy("", "", 16.0));
    }

    #[test]
    fn family_and_size_have_one_authority() {
        // The base style structurally cannot carry a family or a size: they
        // live on the layer alone, so aliases cannot disagree. A span may
        // still override either for its range, like any other property.
        let layer = TextLayer {
            text: String::new(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 48.0,
            style: BaseStyle::default(),
            spans: vec![StyleSpan {
                start: 0,
                end: 3,
                style: StyleOverride {
                    family: Some("DejaVu Sans Condensed".to_string()),
                    size_px: Some(64.0),
                    ..StyleOverride::default()
                },
            }],
            paragraph: Paragraph::default(),
            frame: Frame::Point,
            kerning: Vec::new(),
        };
        assert_eq!(layer.style.weight, Weight::NORMAL);
        assert_eq!(
            layer.spans[0].style.family.as_deref(),
            Some("DejaVu Sans Condensed"),
            "the span overrides the family for its range"
        );
        assert_eq!(layer.spans[0].style.size_px, Some(64.0));
    }

    #[test]
    fn colours_are_documented_as_linear_straight_rgba() {
        // The acceptance scene's white headline and green secondary, in the
        // schema's own colour space.
        let white = [1.0, 1.0, 1.0, 1.0];
        let green = [0.2, 0.8, 0.4, 1.0];
        let layer = TextLayer {
            style: BaseStyle {
                fill: white,
                ..BaseStyle::default()
            },
            spans: vec![StyleSpan {
                start: 0,
                end: 1,
                style: StyleOverride {
                    fill: Some(green),
                    ..StyleOverride::default()
                },
            }],
            ..TextLayer::default()
        };
        assert_eq!(layer.style.fill, white);
        assert_eq!(layer.spans[0].style.fill, Some(green));
    }
}

// ---------------------------------------------------------------------------
// Validation (plan card 017)
// ---------------------------------------------------------------------------

/// Why a text payload was refused. Every variant is a fact a caller can show;
/// nothing here panics, allocates unboundedly, or depends on the payload's
/// text being well-formed UTF-8-adjacent anything.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TextError {
    #[error("text size must be finite and between 1 and {max} px, got {value}")]
    SizeOutOfRange { value: f32, max: f32 },
    #[error("a colour component is not finite")]
    NonFiniteColour,
    #[error("tracking must be finite, got {value}")]
    NonFiniteTracking { value: f32 },
    #[error("glyph scale must be finite and between {min} and {max}, got {value}")]
    ScaleOutOfRange { value: f32, min: f32, max: f32 },
    #[error("baseline shift must be finite, got {value}")]
    NonFiniteBaselineShift { value: f32 },
    #[error("paragraph spacing and indents must be finite, got {value}")]
    NonFiniteParagraphSpacing { value: f32 },
    #[error("leading must be finite, got {value}")]
    NonFiniteLeading { value: f32 },
    #[error("frame dimension must be finite and non-negative, got {value}")]
    BadFrameDimension { value: f32 },
    #[error("kerning amount must be finite, got {value}")]
    NonFiniteKern { value: f32 },
    #[error("a style range [{start}, {end}) is out of bounds for the text")]
    SpanOutOfBounds { start: usize, end: usize },
    #[error("a style range cuts a UTF-8 code point at byte {byte}")]
    SpanSplitsCodePoint { byte: usize },
    #[error("the text is {len} bytes, over the {max}-byte import cap")]
    TextTooLong { len: usize, max: usize },
    #[error("{count} style spans is over the {max} cap")]
    TooManySpans { count: usize, max: usize },
}

/// Longest text a persisted layer may carry: a 4 MiB ceiling keeps an imported
/// or corrupt payload from allocating without bound, and is far above any
/// typeset run.
pub const MAX_TEXT_BYTES: usize = 4 << 20;
/// Most styled ranges one layer may carry.
pub const MAX_SPANS: usize = 4096;
/// Smallest glyph scale the schema accepts (1 %): a zero scale has no ink
/// and could not be scaled back.
pub const MIN_GLYPH_SCALE: f32 = 0.01;
/// Largest glyph scale the schema accepts (1000 %, Photoshop's ceiling).
pub const MAX_GLYPH_SCALE: f32 = 10.0;
/// Largest type size the schema accepts — far above every panel maximum, but
/// a finite bound so a corrupt payload cannot ask for an unbounded raster.
pub const MAX_SIZE_PX: f32 = 4096.0;

impl TextLayer {
    /// Reject everything the schema cannot mean: non-finite numbers, negative
    /// or absurd dimensions, spans outside the text or cutting a code point,
    /// and unbounded payloads. Valid text — multilingual, styled, long —
    /// passes untouched.
    pub fn validate(&self) -> Result<(), TextError> {
        if self.text.len() > MAX_TEXT_BYTES {
            return Err(TextError::TextTooLong {
                len: self.text.len(),
                max: MAX_TEXT_BYTES,
            });
        }
        if !self.size_px.is_finite() || self.size_px <= 0.0 || self.size_px > MAX_SIZE_PX {
            return Err(TextError::SizeOutOfRange {
                value: self.size_px,
                max: MAX_SIZE_PX,
            });
        }
        self.style.validate()?;
        for value in [
            self.paragraph.first_line_indent,
            self.paragraph.space_before,
            self.paragraph.space_after,
            self.paragraph.left_indent,
            self.paragraph.right_indent,
        ] {
            if !value.is_finite() {
                return Err(TextError::NonFiniteParagraphSpacing { value });
            }
        }
        if !self.paragraph.leading_is_finite() {
            return match self.paragraph.leading {
                Leading::Multiple(v) | Leading::Absolute(v) => {
                    Err(TextError::NonFiniteLeading { value: v })
                }
            };
        }
        if let Frame::Box { width, height } = self.frame {
            for v in [Some(width), height] {
                match v {
                    Some(v) if !v.is_finite() || v < 0.0 => {
                        return Err(TextError::BadFrameDimension { value: v })
                    }
                    _ => {}
                }
            }
        }
        for k in &self.kerning {
            if !k.amount.is_finite() {
                return Err(TextError::NonFiniteKern { value: k.amount });
            }
        }
        if self.spans.len() > MAX_SPANS {
            return Err(TextError::TooManySpans {
                count: self.spans.len(),
                max: MAX_SPANS,
            });
        }
        for span in &self.spans {
            if span.start > span.end || span.end > self.text.len() {
                return Err(TextError::SpanOutOfBounds {
                    start: span.start,
                    end: span.end,
                });
            }
            for byte in [span.start, span.end] {
                if !self.text.is_char_boundary(byte) {
                    return Err(TextError::SpanSplitsCodePoint { byte });
                }
            }
            span.style.validate()?;
        }
        Ok(())
    }

    /// The spans, deterministically ordered and deduplicated: sorted by
    /// `(start, end)` with the original order kept for equal ranges (stable),
    /// and exact duplicates dropped. Later spans win at render time, so this
    /// preserves meaning while making overlapping input canonical.
    pub fn normalized_spans(&self) -> Vec<StyleSpan> {
        let mut spans = self.spans.clone();
        spans.sort_by_key(|s| (s.start, s.end));
        spans.dedup();
        spans
    }
}

impl BaseStyle {
    /// The base style's own finiteness checks.
    fn validate(&self) -> Result<(), TextError> {
        if self.fill.iter().any(|c| !c.is_finite()) {
            return Err(TextError::NonFiniteColour);
        }
        if !self.tracking.is_finite() {
            return Err(TextError::NonFiniteTracking {
                value: self.tracking,
            });
        }
        for value in [self.horizontal_scale, self.vertical_scale] {
            if !value.is_finite() || !(MIN_GLYPH_SCALE..=MAX_GLYPH_SCALE).contains(&value) {
                return Err(TextError::ScaleOutOfRange {
                    value,
                    min: MIN_GLYPH_SCALE,
                    max: MAX_GLYPH_SCALE,
                });
            }
        }
        if !self.baseline_shift.is_finite() {
            return Err(TextError::NonFiniteBaselineShift {
                value: self.baseline_shift,
            });
        }
        Ok(())
    }
}

impl StyleOverride {
    /// A span patch's finiteness checks (colour/tracking).
    fn validate(&self) -> Result<(), TextError> {
        if let Some(fill) = self.fill {
            if fill.iter().any(|c| !c.is_finite()) {
                return Err(TextError::NonFiniteColour);
            }
        }
        if let Some(t) = self.tracking {
            if !t.is_finite() {
                return Err(TextError::NonFiniteTracking { value: t });
            }
        }
        Ok(())
    }
}

impl Paragraph {
    /// `true` when the leading value is finite.
    fn leading_is_finite(&self) -> bool {
        match self.leading {
            Leading::Multiple(v) | Leading::Absolute(v) => v.is_finite(),
        }
    }
}

impl StyleOverride {
    /// The sparse patch's default: no overrides at all.
    pub fn none() -> Self {
        Self::default()
    }
}

// -- card 017: payload and range validation ----------------------------

#[test]
fn validation_rejects_non_finite_numbers_and_bad_dimensions() {
    let mut layer = TextLayer::legacy("Hi", "DejaVu Sans", 24.0);
    assert!(layer.validate().is_ok(), "a legacy layer is valid");

    layer.size_px = f32::NAN;
    assert!(matches!(
        layer.validate(),
        Err(TextError::SizeOutOfRange { .. })
    ));
    layer.size_px = 0.0;
    assert!(layer.validate().is_err());
    layer.size_px = MAX_SIZE_PX * 2.0;
    assert!(layer.validate().is_err());
    layer.size_px = 24.0;

    layer.style.fill = [0.5, f32::NAN, 0.5, 1.0];
    assert!(matches!(layer.validate(), Err(TextError::NonFiniteColour)));
    layer.style.fill = [0.0, 0.0, 0.0, 1.0];
    layer.style.tracking = f32::INFINITY;
    assert!(matches!(
        layer.validate(),
        Err(TextError::NonFiniteTracking { .. })
    ));
    layer.style.tracking = 0.0;

    layer.frame = Frame::Box {
        width: -1.0,
        height: None,
    };
    assert!(matches!(
        layer.validate(),
        Err(TextError::BadFrameDimension { .. })
    ));
    layer.frame = Frame::Point;
    layer.kerning = vec![Kern {
        index: 1,
        amount: f32::NAN,
    }];
    assert!(matches!(
        layer.validate(),
        Err(TextError::NonFiniteKern { .. })
    ));
    layer.kerning.clear();
    assert!(layer.validate().is_ok(), "back to valid");
}

#[test]
fn validation_rejects_out_of_bounds_and_code_point_splitting_spans() {
    // "héy" — written as bytes: the ui crate's glyph gate scans library
    // source for non-ASCII string literals, so multilingual fixtures spell
    // their text out in UTF-8 bytes.
    let text = String::from_utf8(vec![b'h', 0xC3, 0xA9, b'y']).unwrap();
    let mut layer = TextLayer::legacy(text.clone(), "DejaVu Sans", 24.0);
    layer.spans = vec![StyleSpan {
        start: 2,
        end: 4,
        style: StyleOverride::none(),
    }];
    assert!(
        matches!(layer.validate(), Err(TextError::SpanSplitsCodePoint { .. })),
        "byte 2 cuts the two-byte e-acute"
    );

    layer.spans = vec![StyleSpan {
        start: 0,
        end: 100,
        style: StyleOverride::none(),
    }];
    assert!(matches!(
        layer.validate(),
        Err(TextError::SpanOutOfBounds { .. })
    ));

    // Multilingual text with boundaries on code points is valid.
    // "héllo 日本語 🌍".
    let multilingual = String::from_utf8(vec![
        b'h', 0xC3, 0xA9, b'l', b'l', b'o', b' ', 0xE6, 0x97, 0xA5, 0xE6, 0x9C, 0xAC, 0xE8, 0xAA,
        0x9E, b' ', 0xF0, 0x9F, 0x8C, 0x8D,
    ])
    .unwrap();
    layer.text = multilingual.clone();
    layer.spans = vec![StyleSpan {
        start: 0,
        end: multilingual.len(),
        style: StyleOverride::none(),
    }];
    assert!(layer.validate().is_ok(), "a whole-text span is valid");
}

#[test]
fn validation_bounds_imported_payloads() {
    let mut layer = TextLayer::legacy("x".repeat(MAX_TEXT_BYTES + 1), "F", 12.0);
    assert!(matches!(
        layer.validate(),
        Err(TextError::TextTooLong { .. })
    ));

    layer.text = "x".repeat(MAX_TEXT_BYTES);
    layer.spans = vec![
        StyleSpan {
            start: 0,
            end: 1,
            style: StyleOverride::none(),
        };
        MAX_SPANS + 1
    ];
    assert!(matches!(
        layer.validate(),
        Err(TextError::TooManySpans { .. })
    ));
}

#[test]
fn overlapping_spans_normalize_deterministically() {
    let mut layer = TextLayer::legacy("abcdef", "F", 12.0);
    let a = StyleSpan {
        start: 0,
        end: 2,
        style: StyleOverride::none(),
    };
    let b = StyleSpan {
        start: 2,
        end: 4,
        style: StyleOverride::none(),
    };
    // Out of order + an exact duplicate: sorted, duplicates dropped,
    // original order kept for equal ranges.
    layer.spans = vec![b.clone(), a.clone(), b.clone()];
    let normalized = layer.normalized_spans();
    assert_eq!(normalized, vec![a, b]);
}

// -- W3-J: scale, baseline shift, caps, anti-alias, indents ------------

#[test]
fn a_payload_written_before_the_w3j_fields_loads_with_neutral_defaults() {
    let payload = serde_json::json!({
        "text": "Headline",
        "font_family": "DejaVu Sans",
        "size_px": 48.0,
        "style": { "weight": 700 },
        "paragraph": {
            "alignment": "Center",
            "leading": { "Multiple": 1.0 },
            "first_line_indent": 4.0,
            "space_before": 0.0,
            "space_after": 0.0
        }
    });
    let layer: TextLayer = serde_json::from_value(payload).unwrap();
    assert_eq!(layer.style.horizontal_scale, 1.0);
    assert_eq!(layer.style.vertical_scale, 1.0);
    assert_eq!(layer.style.baseline_shift, 0.0);
    assert_eq!(layer.style.caps, Caps::Normal);
    assert_eq!(layer.style.anti_alias, AntiAlias::Smooth);
    assert_eq!(layer.paragraph.first_line_indent, 4.0);
    assert_eq!(layer.paragraph.left_indent, 0.0);
    assert_eq!(layer.paragraph.right_indent, 0.0);
    assert!(layer.validate().is_ok());
}

#[test]
fn the_w3j_fields_round_trip_and_validate() {
    let mut layer = TextLayer::legacy("Hi", "DejaVu Sans", 24.0);
    layer.style.horizontal_scale = 2.0;
    layer.style.vertical_scale = 0.5;
    layer.style.baseline_shift = 6.0;
    layer.style.caps = Caps::SmallCaps;
    layer.style.anti_alias = AntiAlias::None;
    layer.paragraph.left_indent = 10.0;
    layer.paragraph.right_indent = 12.0;
    layer.paragraph.alignment = Alignment::JustifyAll;
    assert!(layer.validate().is_ok());
    assert!(!layer.is_legacy());
    let back: TextLayer = serde_json::from_str(&serde_json::to_string(&layer).unwrap()).unwrap();
    assert_eq!(back, layer);

    layer.style.horizontal_scale = 0.0;
    assert!(matches!(
        layer.validate(),
        Err(TextError::ScaleOutOfRange { .. })
    ));
    layer.style.horizontal_scale = 1.0;
    layer.style.vertical_scale = f32::NAN;
    assert!(layer.validate().is_err());
    layer.style.vertical_scale = 1.0;
    layer.style.baseline_shift = f32::INFINITY;
    assert!(matches!(
        layer.validate(),
        Err(TextError::NonFiniteBaselineShift { .. })
    ));
    layer.style.baseline_shift = 0.0;
    layer.paragraph.left_indent = f32::NAN;
    assert!(matches!(
        layer.validate(),
        Err(TextError::NonFiniteParagraphSpacing { .. })
    ));
}

#[cfg(test)]
mod vertical_tests {
    use super::*;

    /// W7-F: the vertical flag is append-only — absent from a horizontal
    /// paragraph's JSON, absent-means-horizontal on load, and round-trips.
    #[test]
    fn the_vertical_flag_is_omitted_when_off_and_old_payloads_load_horizontal() {
        let json = serde_json::to_string(&Paragraph::default()).unwrap();
        assert!(!json.contains("vertical"), "{json}");
        let old: Paragraph = serde_json::from_str(r#"{"alignment":"Left"}"#).unwrap();
        assert!(!old.vertical);
        let on = Paragraph {
            vertical: true,
            ..Paragraph::default()
        };
        let back: Paragraph = serde_json::from_str(&serde_json::to_string(&on).unwrap()).unwrap();
        assert!(back.vertical);
    }
}
