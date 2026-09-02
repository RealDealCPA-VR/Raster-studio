//! Type scale: a modular scale rounded to whole points.
//!
//! Sizes are in **points**, the same unit egui calls "points" — logical pixels
//! before the display scale factor. Rounding to whole points keeps stem widths
//! consistent after rasterization; a 12.6pt body would shimmer between
//! rendering passes at fractional scale factors.

/// Nominal font weight, expressed on the CSS 100..900 axis.
///
/// egui selects faces by *family*, not by a weight axis, so this token only
/// takes effect once a face is registered for the weight — see
/// [`FontWeight::family_suffix`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FontWeight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl FontWeight {
    /// Weight on the CSS numeric axis.
    pub const fn numeric(self) -> u16 {
        match self {
            Self::Regular => 400,
            Self::Medium => 500,
            Self::Semibold => 600,
            Self::Bold => 700,
        }
    }

    /// Suffix an application appends to its font family name when registering
    /// a face for this weight, e.g. `"UI-Semibold"`.
    pub const fn family_suffix(self) -> &'static str {
        match self {
            Self::Regular => "Regular",
            Self::Medium => "Medium",
            Self::Semibold => "Semibold",
            Self::Bold => "Bold",
        }
    }
}

use super::color::TextSize;

/// One rung of the type scale.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeStyle {
    /// Font size in points.
    pub size_pt: f32,
    pub weight: FontWeight,
    /// Baseline-to-baseline distance in points.
    pub line_height_pt: f32,
    /// Letter spacing in points; negative tightens.
    pub tracking_pt: f32,
}

impl TypeStyle {
    /// The WCAG 2.1 size class this rung is judged at.
    ///
    /// SC 1.4.3 defines "large scale" as >= 18pt, or >= 14pt when bold. Bold is
    /// taken as >= 700 on the weight axis; a 600 semibold does **not** qualify,
    /// which is the conservative reading and the one this crate enforces.
    pub fn wcag_size(&self) -> TextSize {
        if self.size_pt >= 18.0 || (self.size_pt >= 14.0 && self.weight.numeric() >= 700) {
            TextSize::Large
        } else {
            TextSize::Normal
        }
    }
}

/// The five rungs, smallest to largest. The declaration order **is** the
/// ordering asserted by the monotonicity test.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum TypeRole {
    /// Badges, axis ticks, dense numeric readouts.
    Caption,
    /// Secondary labels under a control.
    Footnote,
    /// Default UI text.
    Body,
    /// Section titles inside a panel.
    Headline,
    /// Window and document titles.
    Title,
}

impl TypeRole {
    /// Every rung, in ascending size order.
    pub const ALL: &'static [TypeRole] = &[
        Self::Caption,
        Self::Footnote,
        Self::Body,
        Self::Headline,
        Self::Title,
    ];

    /// Exponent applied to the scale ratio, relative to [`TypeRole::Body`].
    ///
    /// Title skips a step so headings separate from body text at a glance.
    const fn step(self) -> i32 {
        match self {
            Self::Caption => -2,
            Self::Footnote => -1,
            Self::Body => 0,
            Self::Headline => 1,
            Self::Title => 3,
        }
    }

    /// Weight the rung is set in.
    pub const fn weight(self) -> FontWeight {
        match self {
            Self::Caption | Self::Footnote | Self::Body => FontWeight::Regular,
            Self::Headline => FontWeight::Semibold,
            Self::Title => FontWeight::Bold,
        }
    }
}

/// A complete type scale generated from a base size and a ratio.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct TypeScale {
    base_pt: f32,
    ratio: f32,
    line_height_factor: f32,
}

impl TypeScale {
    /// Build a modular scale.
    ///
    /// `base_pt` must be > 0 and `ratio` > 1.0, otherwise the scale would not
    /// be monotonic; both are clamped into range rather than panicking so a
    /// bad theme file degrades instead of taking the app down.
    pub fn new(base_pt: f32, ratio: f32) -> Self {
        Self {
            base_pt: base_pt.max(1.0),
            ratio: ratio.max(1.01),
            line_height_factor: 1.35,
        }
    }

    /// Override the baseline-to-baseline multiplier (default 1.35).
    pub fn with_line_height_factor(mut self, factor: f32) -> Self {
        self.line_height_factor = factor.max(1.0);
        self
    }

    /// Base size in points — the size of [`TypeRole::Body`] before rounding.
    pub const fn base_pt(&self) -> f32 {
        self.base_pt
    }

    /// The scale ratio between adjacent steps.
    pub const fn ratio(&self) -> f32 {
        self.ratio
    }

    /// Rounded point size for a rung.
    pub fn size_pt(&self, role: TypeRole) -> f32 {
        (self.base_pt * self.ratio.powi(role.step()))
            .round()
            .max(1.0)
    }

    /// Full style for a rung.
    pub fn style(&self, role: TypeRole) -> TypeStyle {
        let size_pt = self.size_pt(role);
        TypeStyle {
            size_pt,
            weight: role.weight(),
            line_height_pt: (size_pt * self.line_height_factor * 2.0).round() / 2.0,
            // Larger type needs tighter tracking to keep an even color; the
            // relationship is linear in the distance from the base size.
            tracking_pt: (self.base_pt - size_pt) * 0.03,
        }
    }
}

impl Default for TypeScale {
    /// 12pt base at a 1.125 ratio, which rounds to 9 / 11 / 12 / 14 / 17 pt
    /// (Title skips a step) — Photopea's compact sizes, with body at 12pt.
    fn default() -> Self {
        Self::new(12.0, 1.125)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scale_hits_the_intended_point_sizes() {
        // Photopea's compact scale: 12pt base at 1.125 rounds to
        // 9 / 11 / 12 / 14 / 15 pt.
        let s = TypeScale::default();
        assert_eq!(s.size_pt(TypeRole::Caption), 9.0);
        assert_eq!(s.size_pt(TypeRole::Footnote), 11.0);
        assert_eq!(s.size_pt(TypeRole::Body), 12.0);
        assert_eq!(s.size_pt(TypeRole::Headline), 14.0);
        assert_eq!(s.size_pt(TypeRole::Title), 17.0);
    }

    #[test]
    fn sizes_are_whole_points() {
        let s = TypeScale::default();
        for role in TypeRole::ALL {
            let size = s.size_pt(*role);
            assert_eq!(size, size.round(), "{role:?} is fractional: {size}");
        }
    }

    #[test]
    fn line_height_always_exceeds_the_glyph_size() {
        let s = TypeScale::default();
        for role in TypeRole::ALL {
            let st = s.style(*role);
            assert!(
                st.line_height_pt > st.size_pt,
                "{role:?}: leading {} <= size {}",
                st.line_height_pt,
                st.size_pt
            );
        }
    }

    #[test]
    fn large_type_tracks_tighter_than_body() {
        let s = TypeScale::default();
        assert!(s.style(TypeRole::Title).tracking_pt < 0.0);
        assert!(s.style(TypeRole::Caption).tracking_pt > 0.0);
        assert_eq!(s.style(TypeRole::Body).tracking_pt, 0.0);
    }

    #[test]
    fn weights_climb_with_the_scale() {
        assert_eq!(TypeRole::Body.weight(), FontWeight::Regular);
        assert!(TypeRole::Headline.weight().numeric() > TypeRole::Body.weight().numeric());
        assert!(TypeRole::Title.weight().numeric() > TypeRole::Headline.weight().numeric());
    }

    #[test]
    fn only_the_title_rung_earns_the_wcag_large_text_discount() {
        let s = TypeScale::default();
        // 20pt Bold clears the >= 18pt rule.
        assert_eq!(s.style(TypeRole::Title).wcag_size(), TextSize::Large);
        // 15pt Semibold is >= 14pt but 600 is not bold, so no discount.
        assert_eq!(s.style(TypeRole::Headline).wcag_size(), TextSize::Normal);
        for role in [TypeRole::Caption, TypeRole::Footnote, TypeRole::Body] {
            assert_eq!(s.style(role).wcag_size(), TextSize::Normal, "{role:?}");
        }
    }

    #[test]
    fn fourteen_point_bold_is_large_but_fourteen_point_regular_is_not() {
        let bold = TypeStyle {
            size_pt: 14.0,
            weight: FontWeight::Bold,
            line_height_pt: 19.0,
            tracking_pt: 0.0,
        };
        assert_eq!(bold.wcag_size(), TextSize::Large);
        let regular = TypeStyle {
            weight: FontWeight::Regular,
            ..bold
        };
        assert_eq!(regular.wcag_size(), TextSize::Normal);
    }

    #[test]
    fn degenerate_inputs_are_clamped_into_a_usable_scale() {
        let s = TypeScale::new(0.0, 0.5);
        assert!(s.base_pt() >= 1.0);
        assert!(s.ratio() > 1.0);
        for pair in TypeRole::ALL.windows(2) {
            assert!(s.size_pt(pair[1]) >= s.size_pt(pair[0]));
        }
    }
}
