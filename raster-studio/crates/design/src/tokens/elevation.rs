//! Elevation levels and the shadow each one casts.
//!
//! Shadows are soft and low-contrast on purpose: depth is signalled by blur
//! radius, not by darkness. A hard shadow reads as a sticker; a wide, faint one
//! reads as a sheet of glass above the page.

/// A drop shadow, independent of any color.
///
/// The concrete color comes from the palette's shadow role scaled by
/// [`ShadowSpec::opacity`], so light and dark themes share one geometry.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ShadowSpec {
    /// Downward offset in points. Always >= 0: light comes from above.
    pub y_offset_pt: f32,
    /// Penumbra width in points.
    pub blur_pt: f32,
    /// Growth (or, when negative, shrink) of the shadow before blurring.
    pub spread_pt: f32,
    /// Multiplier applied to the palette's shadow color alpha, 0..=1.
    pub opacity: f32,
}

impl ShadowSpec {
    /// No shadow at all.
    pub const NONE: Self = Self {
        y_offset_pt: 0.0,
        blur_pt: 0.0,
        spread_pt: 0.0,
        opacity: 0.0,
    };

    /// `true` when the spec paints nothing.
    pub fn is_none(&self) -> bool {
        self.opacity <= 0.0 || (self.blur_pt <= 0.0 && self.y_offset_pt <= 0.0)
    }
}

/// How far a surface sits above the one behind it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Elevation {
    /// In-plane. Panels and the canvas.
    Flat,
    /// Just off the page: cards, the selected segment of a segmented control.
    Raised,
    /// Popovers, menus, floating tool bars.
    Overlay,
    /// Sheets and dialogs that own the window.
    Modal,
}

impl Elevation {
    /// Every level, ascending.
    pub const ALL: &'static [Elevation] =
        &[Self::Flat, Self::Raised, Self::Overlay, Self::Modal];

    /// The shadow cast at this level.
    pub const fn shadow(self) -> ShadowSpec {
        match self {
            Self::Flat => ShadowSpec::NONE,
            Self::Raised => ShadowSpec {
                y_offset_pt: 1.0,
                blur_pt: 3.0,
                spread_pt: 0.0,
                opacity: 0.40,
            },
            Self::Overlay => ShadowSpec {
                y_offset_pt: 6.0,
                blur_pt: 18.0,
                spread_pt: -2.0,
                opacity: 0.70,
            },
            Self::Modal => ShadowSpec {
                y_offset_pt: 18.0,
                blur_pt: 48.0,
                spread_pt: -6.0,
                opacity: 1.0,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blur_offset_and_opacity_all_climb_with_elevation() {
        for pair in Elevation::ALL.windows(2) {
            let lo = pair[0].shadow();
            let hi = pair[1].shadow();
            assert!(hi.blur_pt > lo.blur_pt, "{:?} -> {:?}", pair[0], pair[1]);
            assert!(hi.y_offset_pt > lo.y_offset_pt);
            assert!(hi.opacity > lo.opacity);
        }
    }

    #[test]
    fn light_always_comes_from_above_and_opacity_stays_in_range() {
        for e in Elevation::ALL {
            let s = e.shadow();
            assert!(s.y_offset_pt >= 0.0, "{e:?} casts upward");
            assert!((0.0..=1.0).contains(&s.opacity), "{e:?} opacity {}", s.opacity);
            // Spread never exceeds the blur, or the shadow reads as a border.
            assert!(s.spread_pt.abs() <= s.blur_pt);
        }
    }

    #[test]
    fn only_flat_paints_nothing() {
        assert!(Elevation::Flat.shadow().is_none());
        for e in [Elevation::Raised, Elevation::Overlay, Elevation::Modal] {
            assert!(!e.shadow().is_none(), "{e:?}");
        }
    }
}
