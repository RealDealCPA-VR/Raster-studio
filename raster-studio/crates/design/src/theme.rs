//! The theme: one appearance, one bundle of resolved tokens.

use std::sync::OnceLock;

use crate::tokens::{BorderWidths, Metrics, Palette, Radii, TypeScale};

/// Every token needed to draw the app in one appearance.
///
/// Obtained through [`Theme::tokens`], which hands out a `&'static` reference —
/// widgets read tokens every frame, so building the palette per call would put
/// a `BTreeMap` allocation in the paint path.
#[derive(Clone, PartialEq, Debug)]
pub struct Tokens {
    pub palette: Palette,
    pub type_scale: TypeScale,
    pub metrics: Metrics,
    pub radii: Radii,
    pub borders: BorderWidths,
}

/// Light or dark appearance.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Theme {
    Light,
    /// The default: an image editor should not glow at the user.
    #[default]
    Dark,
}

impl Theme {
    /// Both appearances.
    pub const ALL: &'static [Theme] = &[Self::Light, Self::Dark];

    /// `true` for [`Theme::Dark`].
    pub const fn is_dark(self) -> bool {
        matches!(self, Self::Dark)
    }

    /// The other appearance.
    pub const fn toggled(self) -> Self {
        match self {
            Self::Light => Self::Dark,
            Self::Dark => Self::Light,
        }
    }

    /// Human-readable name, for menus and settings files.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    /// The resolved token bundle, built once per process.
    pub fn tokens(self) -> &'static Tokens {
        static LIGHT: OnceLock<Tokens> = OnceLock::new();
        static DARK: OnceLock<Tokens> = OnceLock::new();
        let cell = match self {
            Self::Light => &LIGHT,
            Self::Dark => &DARK,
        };
        cell.get_or_init(|| Tokens {
            palette: if self.is_dark() {
                Palette::dark()
            } else {
                Palette::light()
            },
            type_scale: TypeScale::default(),
            metrics: Metrics::default(),
            radii: Radii::default(),
            borders: BorderWidths::default(),
        })
    }

    /// Shorthand for `self.tokens().palette`.
    pub fn palette(self) -> &'static Palette {
        &self.tokens().palette
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_cached_not_rebuilt() {
        let a = Theme::Dark.tokens();
        let b = Theme::Dark.tokens();
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn each_theme_gets_its_own_palette() {
        assert!(Theme::Dark.palette().is_dark());
        assert!(!Theme::Light.palette().is_dark());
        assert_ne!(Theme::Light.tokens().palette, Theme::Dark.tokens().palette);
    }

    #[test]
    fn geometry_tokens_are_shared_across_appearances() {
        // Only color changes between appearances; layout must not shift when
        // the user flips the theme.
        assert_eq!(Theme::Light.tokens().metrics, Theme::Dark.tokens().metrics);
        assert_eq!(Theme::Light.tokens().radii, Theme::Dark.tokens().radii);
        assert_eq!(
            Theme::Light.tokens().type_scale,
            Theme::Dark.tokens().type_scale
        );
    }

    #[test]
    fn toggling_twice_is_the_identity() {
        for t in Theme::ALL {
            assert_eq!(t.toggled().toggled(), *t);
            assert_ne!(t.toggled(), *t);
        }
    }

    #[test]
    fn default_is_dark() {
        assert_eq!(Theme::default(), Theme::Dark);
        assert!(Theme::default().is_dark());
    }
}
