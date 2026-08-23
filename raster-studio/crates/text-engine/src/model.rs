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

use crate::style::{CharStyle, StyleRun};

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
    fn from(layer: &layer_model::TextLayer) -> Self {
        Self {
            text: layer.text.clone(),
            style: CharStyle {
                family: layer.font_family.clone(),
                size_px: layer.size_px,
                ..CharStyle::default()
            },
            ..Self::default()
        }
    }
}

impl From<layer_model::TextLayer> for TextRun {
    fn from(layer: layer_model::TextLayer) -> Self {
        Self::from(&layer)
    }
}

impl From<&TextRun> for layer_model::TextLayer {
    fn from(run: &TextRun) -> Self {
        Self {
            text: run.text.clone(),
            font_family: run.style.family.clone(),
            size_px: run.style.size_px,
        }
    }
}

impl From<TextRun> for layer_model::TextLayer {
    fn from(run: TextRun) -> Self {
        Self {
            text: run.text,
            font_family: run.style.family,
            size_px: run.style.size_px,
        }
    }
}
