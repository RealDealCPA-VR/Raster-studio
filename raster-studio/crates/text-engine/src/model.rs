//! The editable text-layer model.
//!
//! [`TextRun`] is the serialised, round-trippable description of a text layer:
//! the string, the base character style, the per-range style overrides, the
//! paragraph settings, the frame (point text or a wrapping box), manual
//! kerning, and where the whole thing sits in layer space.
//!
//! It is the richer companion of [`layer_model::TextLayer`], which stays the
//! minimal three-field shape stored in the document. [`From`] conversions go
//! both ways; see the module tests for the round-trip guarantee.

use serde::{Deserialize, Serialize};

use crate::style::{
    CharStyle, FontSlant, FontStretch, FontWeight, ScriptPosition, StyleOverride, StyleRun,
};

/// Horizontal alignment of the lines inside a paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum Alignment {
    /// Flush against the start edge.
    #[default]
    Left,
    /// Centred.
    Center,
    /// Flush against the end edge.
    Right,
    /// Both edges flush; word spaces absorb the slack. The last line of a
    /// paragraph is never justified.
    Justify,
    /// Justified; the last line of each paragraph is centred (W3-J).
    JustifyLastCenter,
    /// Justified; the last line of each paragraph is flush right (W3-J).
    JustifyLastRight,
    /// Justified, the last line included (W3-J).
    JustifyAll,
}

impl Alignment {
    /// Whether the lines of a boxed paragraph are stretched to both edges.
    #[must_use]
    pub const fn is_justified(self) -> bool {
        matches!(
            self,
            Self::Justify | Self::JustifyLastCenter | Self::JustifyLastRight | Self::JustifyAll
        )
    }
}

/// Distance from one baseline to the next.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum LineHeight {
    /// A multiple of the base font size ("auto leading").
    Multiple(f32),
    /// An absolute distance in layer pixels.
    Absolute(f32),
}

impl Default for LineHeight {
    fn default() -> Self {
        Self::Multiple(1.2)
    }
}

impl LineHeight {
    /// Resolve to pixels for a given base font size.
    #[must_use]
    pub fn resolve(self, base_size_px: f32) -> f32 {
        match self {
            Self::Multiple(m) => base_size_px * m,
            Self::Absolute(px) => px,
        }
    }
}

/// Paragraph-level settings. One set applies to the whole layer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ParagraphStyle {
    /// Horizontal alignment.
    pub alignment: Alignment,
    /// Leading.
    pub line_height: LineHeight,
    /// Extra indent applied to the first visual line of every paragraph, in
    /// layer pixels, along the paragraph's start direction.
    pub first_line_indent: f32,
    /// Extra vertical space inserted before every paragraph but the first.
    pub space_before: f32,
    /// Extra vertical space inserted after every paragraph but the last.
    pub space_after: f32,
    /// W3-J: indent of every line from the start edge, in layer pixels.
    pub left_indent: f32,
    /// W3-J: indent of every line from the end edge, in layer pixels. A
    /// boxed frame wraps inside both indents.
    pub right_indent: f32,
    /// W7-F: vertical type — each paragraph is a column read top to bottom,
    /// and the columns advance right to left. See [`crate::shape`]. Omitted
    /// from the JSON while `false`, so the serialised shape is unchanged.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub vertical: bool,
}

/// How the text is placed: a single anchor, or a box that text wraps inside.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub enum TextFrame {
    /// Point text: no wrapping, the block is as wide as its widest line.
    #[default]
    Point,
    /// Paragraph text: lines wrap at `width`. `height` is advisory — layout
    /// never clips, but [`crate::ShapedText::overflows`] reports overset.
    Box {
        /// Wrap width in layer pixels.
        width: f32,
        /// Optional box height in layer pixels.
        height: Option<f32>,
    },
}

/// A manual kerning adjustment: extra space inserted *before* the character
/// starting at `index`, measured in 1/1000 em of the base size.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct KernAdjustment {
    /// Byte index of the character the space is inserted before.
    pub index: usize,
    /// Amount in 1/1000 em. Negative tightens.
    pub amount: f32,
}

impl KernAdjustment {
    /// Build an adjustment.
    #[must_use]
    pub const fn new(index: usize, amount: f32) -> Self {
        Self { index, amount }
    }
}

/// A complete editable text layer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct TextRun {
    /// The text. `\n`, `\r\n`, `\r` and `\n\r` all start a new paragraph.
    pub text: String,
    /// Base character style; every byte inherits from it.
    pub style: CharStyle,
    /// Sparse per-range overrides, applied in order.
    pub runs: Vec<StyleRun>,
    /// Paragraph settings.
    pub paragraph: ParagraphStyle,
    /// Point text or a wrapping box.
    pub frame: TextFrame,
    /// Manual kerning adjustments.
    pub kerning: Vec<KernAdjustment>,
    /// Top-left of the laid-out block in layer space.
    pub origin: [f32; 2],
}

impl TextRun {
    /// Point text with the given family and size.
    #[must_use]
    pub fn point(text: impl Into<String>, family: impl Into<String>, size_px: f32) -> Self {
        Self {
            text: text.into(),
            style: CharStyle {
                family: family.into(),
                size_px,
                ..CharStyle::default()
            },
            ..Self::default()
        }
    }

    /// Paragraph text wrapped to `width`.
    #[must_use]
    pub fn paragraph(
        text: impl Into<String>,
        family: impl Into<String>,
        size_px: f32,
        width: f32,
    ) -> Self {
        let mut run = Self::point(text, family, size_px);
        run.frame = TextFrame::Box {
            width,
            height: None,
        };
        run
    }

    /// Builder: replace the style runs.
    #[must_use]
    pub fn with_runs(mut self, runs: Vec<StyleRun>) -> Self {
        self.runs = runs;
        self
    }

    /// Builder: replace the paragraph style.
    #[must_use]
    pub fn with_paragraph(mut self, paragraph: ParagraphStyle) -> Self {
        self.paragraph = paragraph;
        self
    }

    /// Builder: set the origin.
    #[must_use]
    pub const fn with_origin(mut self, origin: [f32; 2]) -> Self {
        self.origin = origin;
        self
    }

    /// Builder: replace the manual kerning table.
    #[must_use]
    pub fn with_kerning(mut self, kerning: Vec<KernAdjustment>) -> Self {
        self.kerning = kerning;
        self
    }

    /// The wrap width, if this is paragraph text.
    #[must_use]
    pub const fn wrap_width(&self) -> Option<f32> {
        match self.frame {
            TextFrame::Point => None,
            TextFrame::Box { width, .. } => Some(width),
        }
    }
}

impl From<&layer_model::TextLayer> for TextRun {
    /// The lossless direction (card 016): every persisted field reaches the
    /// run - base style, spans, paragraph settings, frame and kerning - so an
    /// edit that round-trips through the document cannot lose style.
    fn from(layer: &layer_model::TextLayer) -> Self {
        Self {
            text: layer.text.clone(),
            style: CharStyle {
                family: layer.font_family.clone(),
                size_px: layer.size_px,
                weight: FontWeight(layer.style.weight.0),
                slant: match layer.style.slant {
                    layer_model::text::Slant::Normal => FontSlant::Normal,
                    layer_model::text::Slant::Italic => FontSlant::Italic,
                },
                stretch: match layer.style.stretch {
                    layer_model::text::Stretch::UltraCondensed => FontStretch::UltraCondensed,
                    layer_model::text::Stretch::ExtraCondensed => FontStretch::ExtraCondensed,
                    layer_model::text::Stretch::Condensed => FontStretch::Condensed,
                    layer_model::text::Stretch::SemiCondensed => FontStretch::SemiCondensed,
                    layer_model::text::Stretch::Normal => FontStretch::Normal,
                    layer_model::text::Stretch::SemiExpanded => FontStretch::SemiExpanded,
                    layer_model::text::Stretch::Expanded => FontStretch::Expanded,
                    layer_model::text::Stretch::ExtraExpanded => FontStretch::ExtraExpanded,
                    layer_model::text::Stretch::UltraExpanded => FontStretch::UltraExpanded,
                },
                color: layer.style.fill,
                underline: layer.style.underline,
                strikethrough: layer.style.strikethrough,
                script: match layer.style.script {
                    layer_model::text::Script::Normal => ScriptPosition::Normal,
                    layer_model::text::Script::Superscript => ScriptPosition::Superscript,
                    layer_model::text::Script::Subscript => ScriptPosition::Subscript,
                },
                tracking: layer.style.tracking,
                ligatures: layer.style.ligatures,
                kerning: layer.style.kerning,
                allow_synthetic_bold: layer.style.synthetic_bold,
                allow_synthetic_italic: layer.style.synthetic_italic,
                horizontal_scale: layer.style.horizontal_scale,
                vertical_scale: layer.style.vertical_scale,
                baseline_shift: layer.style.baseline_shift,
                caps: layer.style.caps,
                anti_alias: layer.style.anti_alias,
            },
            runs: layer
                .spans
                .iter()
                .map(|span| StyleRun {
                    start: span.start,
                    end: span.end,
                    style: style_override_from_persisted(&span.style),
                })
                .collect(),
            paragraph: paragraph_from_persisted(&layer.paragraph),
            frame: match layer.frame {
                layer_model::text::Frame::Point => TextFrame::Point,
                layer_model::text::Frame::Box { width, height } => TextFrame::Box { width, height },
            },
            kerning: layer
                .kerning
                .iter()
                .map(|k| KernAdjustment {
                    index: k.index,
                    amount: k.amount,
                })
                .collect(),
            origin: [0.0, 0.0],
        }
    }
}

/// Map a persisted sparse patch onto the shaping vocabulary.
fn style_override_from_persisted(over: &layer_model::text::StyleOverride) -> StyleOverride {
    StyleOverride {
        family: over.family.clone(),
        size_px: over.size_px,
        weight: over.weight.map(|w| FontWeight(w.0)),
        slant: over.slant.map(|s| match s {
            layer_model::text::Slant::Normal => FontSlant::Normal,
            layer_model::text::Slant::Italic => FontSlant::Italic,
        }),
        stretch: over.stretch.map(|s| match s {
            layer_model::text::Stretch::UltraCondensed => FontStretch::UltraCondensed,
            layer_model::text::Stretch::ExtraCondensed => FontStretch::ExtraCondensed,
            layer_model::text::Stretch::Condensed => FontStretch::Condensed,
            layer_model::text::Stretch::SemiCondensed => FontStretch::SemiCondensed,
            layer_model::text::Stretch::Normal => FontStretch::Normal,
            layer_model::text::Stretch::SemiExpanded => FontStretch::SemiExpanded,
            layer_model::text::Stretch::Expanded => FontStretch::Expanded,
            layer_model::text::Stretch::ExtraExpanded => FontStretch::ExtraExpanded,
            layer_model::text::Stretch::UltraExpanded => FontStretch::UltraExpanded,
        }),
        color: over.fill,
        underline: over.underline,
        strikethrough: over.strikethrough,
        script: over.script.map(|s| match s {
            layer_model::text::Script::Normal => ScriptPosition::Normal,
            layer_model::text::Script::Superscript => ScriptPosition::Superscript,
            layer_model::text::Script::Subscript => ScriptPosition::Subscript,
        }),
        tracking: over.tracking,
    }
}

/// Map persisted paragraph settings onto the shaping vocabulary.
fn paragraph_from_persisted(p: &layer_model::text::Paragraph) -> ParagraphStyle {
    ParagraphStyle {
        alignment: match p.alignment {
            layer_model::text::Alignment::Left => Alignment::Left,
            layer_model::text::Alignment::Center => Alignment::Center,
            layer_model::text::Alignment::Right => Alignment::Right,
            layer_model::text::Alignment::Justified => Alignment::Justify,
            layer_model::text::Alignment::JustifyLastCenter => Alignment::JustifyLastCenter,
            layer_model::text::Alignment::JustifyLastRight => Alignment::JustifyLastRight,
            layer_model::text::Alignment::JustifyAll => Alignment::JustifyAll,
        },
        line_height: match p.leading {
            layer_model::text::Leading::Multiple(v) => LineHeight::Multiple(v),
            layer_model::text::Leading::Absolute(v) => LineHeight::Absolute(v),
        },
        first_line_indent: p.first_line_indent,
        space_before: p.space_before,
        space_after: p.space_after,
        left_indent: p.left_indent,
        right_indent: p.right_indent,
        vertical: p.vertical,
    }
}

impl From<layer_model::TextLayer> for TextRun {
    fn from(layer: layer_model::TextLayer) -> Self {
        Self::from(&layer)
    }
}

impl From<&TextRun> for layer_model::TextLayer {
    /// The other lossless direction: the run becomes the persisted schema with
    /// every field intact, so a round-trip through the document cannot drop a
    /// style (the E01 defect's root).
    fn from(run: &TextRun) -> Self {
        Self {
            text: run.text.clone(),
            font_family: run.style.family.clone(),
            size_px: run.style.size_px,
            style: layer_model::text::BaseStyle {
                weight: layer_model::text::Weight(run.style.weight.0),
                slant: match run.style.slant {
                    FontSlant::Normal | FontSlant::Oblique => layer_model::text::Slant::Normal,
                    FontSlant::Italic => layer_model::text::Slant::Italic,
                },
                stretch: match run.style.stretch {
                    FontStretch::UltraCondensed => layer_model::text::Stretch::UltraCondensed,
                    FontStretch::ExtraCondensed => layer_model::text::Stretch::ExtraCondensed,
                    FontStretch::Condensed => layer_model::text::Stretch::Condensed,
                    FontStretch::SemiCondensed => layer_model::text::Stretch::SemiCondensed,
                    FontStretch::Normal => layer_model::text::Stretch::Normal,
                    FontStretch::SemiExpanded => layer_model::text::Stretch::SemiExpanded,
                    FontStretch::Expanded => layer_model::text::Stretch::Expanded,
                    FontStretch::ExtraExpanded => layer_model::text::Stretch::ExtraExpanded,
                    FontStretch::UltraExpanded => layer_model::text::Stretch::UltraExpanded,
                },
                fill: run.style.color,
                underline: run.style.underline,
                strikethrough: run.style.strikethrough,
                script: match run.style.script {
                    ScriptPosition::Normal => layer_model::text::Script::Normal,
                    ScriptPosition::Superscript => layer_model::text::Script::Superscript,
                    ScriptPosition::Subscript => layer_model::text::Script::Subscript,
                },
                tracking: run.style.tracking,
                ligatures: run.style.ligatures,
                kerning: run.style.kerning,
                synthetic_bold: run.style.allow_synthetic_bold,
                synthetic_italic: run.style.allow_synthetic_italic,
                horizontal_scale: run.style.horizontal_scale,
                vertical_scale: run.style.vertical_scale,
                baseline_shift: run.style.baseline_shift,
                caps: run.style.caps,
                anti_alias: run.style.anti_alias,
            },
            spans: run
                .runs
                .iter()
                .map(|r| layer_model::text::StyleSpan {
                    start: r.start,
                    end: r.end,
                    style: layer_model::text::StyleOverride {
                        family: r.style.family.clone(),
                        size_px: r.style.size_px,
                        weight: r.style.weight.map(|w| layer_model::text::Weight(w.0)),
                        slant: r.style.slant.map(|s| match s {
                            FontSlant::Normal | FontSlant::Oblique => {
                                layer_model::text::Slant::Normal
                            }
                            FontSlant::Italic => layer_model::text::Slant::Italic,
                        }),
                        stretch: r.style.stretch.map(|s| match s {
                            FontStretch::UltraCondensed => {
                                layer_model::text::Stretch::UltraCondensed
                            }
                            FontStretch::ExtraCondensed => {
                                layer_model::text::Stretch::ExtraCondensed
                            }
                            FontStretch::Condensed => layer_model::text::Stretch::Condensed,
                            FontStretch::SemiCondensed => layer_model::text::Stretch::SemiCondensed,
                            FontStretch::Normal => layer_model::text::Stretch::Normal,
                            FontStretch::SemiExpanded => layer_model::text::Stretch::SemiExpanded,
                            FontStretch::Expanded => layer_model::text::Stretch::Expanded,
                            FontStretch::ExtraExpanded => layer_model::text::Stretch::ExtraExpanded,
                            FontStretch::UltraExpanded => layer_model::text::Stretch::UltraExpanded,
                        }),
                        fill: r.style.color,
                        underline: r.style.underline,
                        strikethrough: r.style.strikethrough,
                        script: r.style.script.map(|s| match s {
                            ScriptPosition::Normal => layer_model::text::Script::Normal,
                            ScriptPosition::Superscript => layer_model::text::Script::Superscript,
                            ScriptPosition::Subscript => layer_model::text::Script::Subscript,
                        }),
                        tracking: r.style.tracking,
                    },
                })
                .collect(),
            paragraph: layer_model::text::Paragraph {
                alignment: match run.paragraph.alignment {
                    Alignment::Left => layer_model::text::Alignment::Left,
                    Alignment::Center => layer_model::text::Alignment::Center,
                    Alignment::Right => layer_model::text::Alignment::Right,
                    Alignment::Justify => layer_model::text::Alignment::Justified,
                    Alignment::JustifyLastCenter => layer_model::text::Alignment::JustifyLastCenter,
                    Alignment::JustifyLastRight => layer_model::text::Alignment::JustifyLastRight,
                    Alignment::JustifyAll => layer_model::text::Alignment::JustifyAll,
                },
                leading: match run.paragraph.line_height {
                    LineHeight::Multiple(v) => layer_model::text::Leading::Multiple(v),
                    LineHeight::Absolute(v) => layer_model::text::Leading::Absolute(v),
                },
                first_line_indent: run.paragraph.first_line_indent,
                space_before: run.paragraph.space_before,
                space_after: run.paragraph.space_after,
                left_indent: run.paragraph.left_indent,
                right_indent: run.paragraph.right_indent,
                vertical: run.paragraph.vertical,
            },
            frame: match run.frame {
                TextFrame::Point => layer_model::text::Frame::Point,
                TextFrame::Box { width, height } => layer_model::text::Frame::Box { width, height },
            },
            kerning: run
                .kerning
                .iter()
                .map(|k| layer_model::text::Kern {
                    index: k.index,
                    amount: k.amount,
                })
                .collect(),
        }
    }
}

impl From<TextRun> for layer_model::TextLayer {
    fn from(run: TextRun) -> Self {
        Self::from(&run)
    }
}
