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

/// The appearance: the app's own Light and Dark plus five of Photopea's
/// themes (More > Theme).
///
/// [`Theme::Light`] and [`Theme::Dark`] are the two the app first shipped.
/// They are modelled on Photopea's White and Dark Grey but are NOT those
/// palettes: their panel, pasteboard, button, text and accent numbers differ
/// from Photopea's (listed on [`crate::tokens::palette::LIGHT_ROLES`] and
/// [`crate::tokens::palette::DARK_ROLES`]). W13X-4 appended Photopea's Light
/// Grey, Blue, Dark Blue, Purple and Black after them — new variants go at the END, because a theme is persisted by
/// its [`Theme::key`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub enum Theme {
    Light,
    /// The default: an image editor should not glow at the user.
    #[default]
    Dark,
    /// W13X-4: Photopea's Light Grey.
    LightGrey,
    /// W13X-4: Photopea's Blue.
    Blue,
    /// W13X-4: Photopea's Dark Blue.
    DarkBlue,
    /// W13X-4: Photopea's Purple.
    Purple,
    /// W13X-4: Photopea's Black.
    Black,
}

impl Theme {
    /// Every appearance, in menu order: the first two, then the five
    /// Photopea themes in Photopea's own order.
    pub const ALL: &'static [Theme] = &[
        Self::Light,
        Self::Dark,
        Self::LightGrey,
        Self::Blue,
        Self::DarkBlue,
        Self::Purple,
        Self::Black,
    ];

    /// `true` for every theme whose chrome is dark (all but [`Theme::Light`]
    /// and [`Theme::LightGrey`]).
    pub const fn is_dark(self) -> bool {
        !matches!(self, Self::Light | Self::LightGrey)
    }

    /// The other appearance: a dark theme toggles to [`Theme::Light`], a
    /// light one to [`Theme::Dark`].
    pub const fn toggled(self) -> Self {
        if self.is_dark() {
            Self::Light
        } else {
            Self::Dark
        }
    }

    /// Human-readable name, for menus.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Light => "Light",
            Self::Dark => "Dark",
            Self::LightGrey => "Light Grey",
            Self::Blue => "Blue",
            Self::DarkBlue => "Dark Blue",
            Self::Purple => "Purple",
            Self::Black => "Black",
        }
    }

    /// W13X-4: the stable lowercase word a settings file stores.
    pub const fn key(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Dark => "dark",
            Self::LightGrey => "light-grey",
            Self::Blue => "blue",
            Self::DarkBlue => "dark-blue",
            Self::Purple => "purple",
            Self::Black => "black",
        }
    }

    /// W13X-4: the theme a settings-file [`Theme::key`] names.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|t| t.key() == key)
    }

    /// The role table this appearance is built from.
    fn roles(self) -> &'static [(crate::tokens::ColorRole, crate::tokens::Srgba)] {
        use crate::tokens::{palette, photopea_themes as pp};
        match self {
            Self::Light => palette::LIGHT_ROLES,
            Self::Dark => palette::DARK_ROLES,
            Self::LightGrey => pp::LIGHT_GREY_ROLES,
            Self::Blue => pp::BLUE_ROLES,
            Self::DarkBlue => pp::DARK_BLUE_ROLES,
            Self::Purple => pp::PURPLE_ROLES,
            Self::Black => pp::BLACK_ROLES,
        }
    }

    /// The resolved token bundle, built once per process.
    pub fn tokens(self) -> &'static Tokens {
        static CELLS: [OnceLock<Tokens>; 7] = [const { OnceLock::new() }; 7];
        let cell = &CELLS[self as usize];
        cell.get_or_init(|| Tokens {
            palette: Palette::from_pairs(self.is_dark(), self.roles()),
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
        for t in [Theme::Light, Theme::Dark] {
            assert_eq!(t.toggled().toggled(), t);
            assert_ne!(t.toggled(), t);
        }
        // W13X-4: every other theme toggles to the opposite appearance.
        for t in Theme::ALL {
            assert_ne!(t.toggled().is_dark(), t.is_dark(), "{t:?}");
        }
    }

    /// W13X-4: every theme has its own cell, its own palette and a key that
    /// names it back.
    #[test]
    fn every_photopea_theme_is_its_own_palette_and_round_trips_by_key() {
        let mut seen: Vec<&Palette> = Vec::new();
        for t in Theme::ALL {
            let p = t.palette();
            assert!(std::ptr::eq(t.tokens(), t.tokens()), "{t:?} rebuilt");
            assert_eq!(p.is_dark(), t.is_dark(), "{t:?}");
            assert!(p.missing_roles().is_empty(), "{t:?}");
            assert!(!seen.contains(&p), "{t:?} repeats another theme's palette");
            seen.push(p);
            assert_eq!(Theme::from_key(t.key()), Some(*t));
        }
        assert_eq!(Theme::ALL.len(), 7, "Photopea ships seven themes");
        assert_eq!(Theme::from_key("sepia"), None);
    }

    #[test]
    fn default_is_dark() {
        assert_eq!(Theme::default(), Theme::Dark);
        assert!(Theme::default().is_dark());
    }
}
