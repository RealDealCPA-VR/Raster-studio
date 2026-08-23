//! Character-level styling.
//!
//! A text layer carries one *base* [`CharStyle`] plus a list of [`StyleRun`]s.
//! Each run names a **byte range** of the layer's text and a sparse
//! [`StyleOverride`]; the effective style at a byte index is the base style
//! with every overlapping override applied in order. That is what makes
//! "bold just these three words" expressible without splitting the layer.

use serde::{Deserialize, Serialize};

/// Multiplier applied to the font size of a sub/superscript run.
///
/// Matches the conventional 58.3% used by desktop layout apps.
pub const SCRIPT_SIZE_FACTOR: f32 = 0.583;
/// Fraction of the *base* font size a superscript is raised by.
pub const SUPERSCRIPT_RISE: f32 = 0.333;
/// Fraction of the *base* font size a subscript is lowered by.
pub const SUBSCRIPT_DROP: f32 = 0.166;

/// CSS-style numeric font weight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FontWeight(pub u16);

impl FontWeight {
    /// 100.
    pub const THIN: Self = Self(100);
    /// 200.
    pub const EXTRA_LIGHT: Self = Self(200);
    /// 300.
    pub const LIGHT: Self = Self(300);
    /// 400 — the default.
    pub const NORMAL: Self = Self(400);
    /// 500.
    pub const MEDIUM: Self = Self(500);
    /// 600.
    pub const SEMI_BOLD: Self = Self(600);
    /// 700.
    pub const BOLD: Self = Self(700);
    /// 800.
    pub const EXTRA_BOLD: Self = Self(800);
    /// 900.
    pub const BLACK: Self = Self(900);

    /// A face this much lighter than the request is too light to pass off as
    /// the requested weight, so the renderer synthesises the difference.
    const SYNTHESIS_TOLERANCE: u16 = 150;

    /// Whether a face of weight `actual` has to be emboldened to stand in for
    /// a request of `self`.
    #[must_use]
    pub const fn needs_synthesis(self, actual: Self) -> bool {
        actual.0 + Self::SYNTHESIS_TOLERANCE < self.0
    }
}

impl Default for FontWeight {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Upright, italic, or oblique.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum FontSlant {
    /// Upright.
    #[default]
    Normal,
    /// A separately drawn italic design.
    Italic,
    /// A slanted version of the upright design.
    Oblique,
}

impl FontSlant {
    /// Whether this slant needs a non-upright face.
    #[must_use]
    pub const fn is_slanted(self) -> bool {
        !matches!(self, Self::Normal)
    }
}

/// Vertical position of a run relative to the baseline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ScriptPosition {
    /// On the baseline.
    #[default]
    Normal,
    /// Raised and shrunk.
    Superscript,
    /// Lowered and shrunk.
    Subscript,
}

impl ScriptPosition {
    /// Size multiplier for this position.
    #[must_use]
    pub const fn size_factor(self) -> f32 {
        match self {
            Self::Normal => 1.0,
            Self::Superscript | Self::Subscript => SCRIPT_SIZE_FACTOR,
        }
    }

    /// Baseline shift in layer space (y grows downwards) for a base size.
    #[must_use]
    pub fn baseline_shift(self, base_size_px: f32) -> f32 {
        match self {
            Self::Normal => 0.0,
            Self::Superscript => -SUPERSCRIPT_RISE * base_size_px,
            Self::Subscript => SUBSCRIPT_DROP * base_size_px,
        }
    }
}

/// Everything that can vary character by character.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharStyle {
    /// Font family name. Empty means "the generic sans-serif family".
    pub family: String,
    /// Em size in layer pixels.
    pub size_px: f32,
    /// Requested weight.
    pub weight: FontWeight,
    /// Requested slant.
    pub slant: FontSlant,
    /// Fill colour as **linear**, straight (non-premultiplied) RGBA.
    pub color: [f32; 4],
    /// Draw an underline beneath this run.
    pub underline: bool,
    /// Draw a strikethrough across this run.
    pub strikethrough: bool,
    /// Sub/superscript placement.
    pub script: ScriptPosition,
    /// Tracking in 1/1000 em, applied to every glyph's advance.
    pub tracking: f32,
    /// Enable the `liga`/`clig` `OpenType` features.
    pub ligatures: bool,
    /// Enable the `kern` `OpenType` feature.
    pub kerning: bool,
    /// Allow faux bold when the family has no face heavy enough.
    pub allow_synthetic_bold: bool,
    /// Allow faux italic when the family has no slanted face.
    pub allow_synthetic_italic: bool,
}

impl Default for CharStyle {
    fn default() -> Self {
        Self {
            family: String::new(),
            size_px: 16.0,
            weight: FontWeight::NORMAL,
            slant: FontSlant::Normal,
            color: [0.0, 0.0, 0.0, 1.0],
            underline: false,
            strikethrough: false,
            script: ScriptPosition::Normal,
            tracking: 0.0,
            ligatures: true,
            kerning: true,
            allow_synthetic_bold: true,
            allow_synthetic_italic: true,
        }
    }
}

impl CharStyle {
    /// The size actually used for shaping, accounting for sub/superscript.
    #[must_use]
    pub fn effective_size_px(&self) -> f32 {
        (self.size_px * self.script.size_factor()).max(crate::MIN_FONT_SIZE_PX)
    }
}

/// A sparse patch over a [`CharStyle`]. `None` means "inherit".
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleOverride {
    /// Override the family.
    pub family: Option<String>,
    /// Override the size.
    pub size_px: Option<f32>,
    /// Override the weight.
    pub weight: Option<FontWeight>,
    /// Override the slant.
    pub slant: Option<FontSlant>,
    /// Override the colour.
    pub color: Option<[f32; 4]>,
    /// Override the underline flag.
    pub underline: Option<bool>,
    /// Override the strikethrough flag.
    pub strikethrough: Option<bool>,
    /// Override the sub/superscript position.
    pub script: Option<ScriptPosition>,
    /// Override tracking.
    pub tracking: Option<f32>,
}

impl StyleOverride {
    /// Apply this patch on top of `base`.
    #[must_use]
    pub fn apply_to(&self, base: &CharStyle) -> CharStyle {
        let mut out = base.clone();
        if let Some(v) = &self.family {
            out.family.clone_from(v);
        }
        if let Some(v) = self.size_px {
            out.size_px = v;
        }
        if let Some(v) = self.weight {
            out.weight = v;
        }
        if let Some(v) = self.slant {
            out.slant = v;
        }
        if let Some(v) = self.color {
            out.color = v;
        }
        if let Some(v) = self.underline {
            out.underline = v;
        }
        if let Some(v) = self.strikethrough {
            out.strikethrough = v;
        }
        if let Some(v) = self.script {
            out.script = v;
        }
        if let Some(v) = self.tracking {
            out.tracking = v;
        }
        out
    }

    /// Builder: set the weight.
    #[must_use]
    pub const fn with_weight(mut self, weight: FontWeight) -> Self {
        self.weight = Some(weight);
        self
    }

    /// Builder: set the slant.
    #[must_use]
    pub const fn with_slant(mut self, slant: FontSlant) -> Self {
        self.slant = Some(slant);
        self
    }

    /// Builder: set the colour.
    #[must_use]
    pub const fn with_color(mut self, color: [f32; 4]) -> Self {
        self.color = Some(color);
        self
    }

    /// Builder: set the size.
    #[must_use]
    pub const fn with_size_px(mut self, size_px: f32) -> Self {
        self.size_px = Some(size_px);
        self
    }

    /// Builder: set the sub/superscript position.
    #[must_use]
    pub const fn with_script(mut self, script: ScriptPosition) -> Self {
        self.script = Some(script);
        self
    }

    /// Builder: turn the underline on or off.
    #[must_use]
    pub const fn with_underline(mut self, on: bool) -> Self {
        self.underline = Some(on);
        self
    }

    /// Builder: turn the strikethrough on or off.
    #[must_use]
    pub const fn with_strikethrough(mut self, on: bool) -> Self {
        self.strikethrough = Some(on);
        self
    }

    /// Builder: set the family.
    #[must_use]
    pub fn with_family(mut self, family: impl Into<String>) -> Self {
        self.family = Some(family.into());
        self
    }
}

/// A [`StyleOverride`] bound to a half-open byte range of the layer's text.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StyleRun {
    /// First byte covered.
    pub start: usize,
    /// One past the last byte covered.
    pub end: usize,
    /// The patch to apply.
    pub style: StyleOverride,
}

impl StyleRun {
    /// Build a run covering `start..end`.
    #[must_use]
    pub const fn new(start: usize, end: usize, style: StyleOverride) -> Self {
        Self { start, end, style }
    }

    /// Whether `index` falls inside this run.
    #[must_use]
    pub const fn contains(&self, index: usize) -> bool {
        index >= self.start && index < self.end
    }
}

/// Resolve the effective style at `index`: `base`, then every run that covers
/// the index, applied in list order.
#[must_use]
pub fn resolve_style(base: &CharStyle, runs: &[StyleRun], index: usize) -> CharStyle {
    let mut out = base.clone();
    for run in runs {
        if run.contains(index) {
            out = run.style.apply_to(&out);
        }
    }
    out
}
